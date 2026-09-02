use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    LINK_ALLOW_BUILD_MISMATCH, LINK_ENABLED, LINK_LEASE_TIMEOUT_SECS, LINK_NODES_PATH,
    LINK_ONLY_NODE,
};
use crate::lib::sync::lock;
use crate::pnworker::link::spec::{
    LeaseControl, LeaseRenew, LeaseResult, LinkJobSpec, LinkOutcome, LinkReport, NodePurpose,
    NodeRegister, NodeRegistered, ReleaseInfo,
};

// The coordinator's view of its cluster. It sits between two tasks that cannot call each other:
// the axum link routes, which is where nodes speak, and `pn_worker`'s loop, which is where jobs
// live. `snapshot.rs` solves the same split for the worker view with one writer and many readers;
// here both sides write, so it is a plain mutex and every function below locks, finishes, and
// returns without ever holding the guard across an await.

pub const DEFAULT_RENEW_SECS: u64 = 10;
pub const DEFAULT_LEASE_TIMEOUT_SECS: u64 = 90;
// A node that has been handed a job but has not come back to collect it. It is a different failure
// from a node that took the work and went quiet, and it resolves much faster: the job has not
// started, so there is nothing to lose by offering it elsewhere.
const OFFER_PICKUP_SECS: u64 = 60;
// A node's own answer to how much it can hold, and it arrives off the wire. One that says a
// thousand would be handed a thousand leases, each of which is a job not running anywhere; the
// ceiling is far above what a real machine reports and exists only so a typo cannot drain the
// queue into one box.
const MAX_JOBS_CEILING: u32 = 64;
// Consecutive offers a node was handed and never came back for. A node in this state is not
// refusing work — it is not answering at all, while still registering often enough to look
// perfectly healthy — so each round costs a job the pickup window and, before this existed, one of
// its link attempts. Past the bound it is drained with a reason `/lsnode` can show, which is the
// difference between a cluster that quietly swallows jobs and one that says which box to go and
// look at.
const MAX_MISSED_PICKUPS: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeState {
    pub name: String,
    #[serde(default)]
    pub pandora_version: String,
    #[serde(default)]
    pub encoder_identity: String,
    #[serde(default)]
    pub ffmpeg_version: String,
    #[serde(default)]
    pub threads: u32,
    #[serde(default = "one")]
    pub max_jobs: u32,
    #[serde(default, alias = "presets")]
    pub encoders: Vec<String>,
    // What this node is for, taken from its token at every register rather than persisted. A
    // purpose is a property of the token, and re-minting one has to take effect on the next
    // register — a value carried across a restart would outlive the token that justified it.
    #[serde(skip, default)]
    pub purpose: NodePurpose,
    // The build the node last recorded itself level with, and a migration it could not run. Both
    // are reports, not state the coordinator acts on: they exist so `/lsnode` can show a node that
    // has quietly stopped keeping up instead of only one that has stopped answering.
    #[serde(default)]
    pub build: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_error: Option<String>,
    #[serde(default)]
    pub registered_at: u64,
    #[serde(default)]
    pub last_seen: u64,
    // Survives a coordinator restart on purpose: an operator who drained a node before a deploy
    // did not mean "until the next deploy".
    #[serde(default)]
    pub drain: bool,
    // Why the coordinator drained this node itself, when it did. It is persisted with the flag it
    // explains: a drain an operator did not ask for and cannot see the reason for is exactly the
    // kind of state that reads as "the cluster stopped working for no reason".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_reason: Option<String>,
    // Offers handed to this node in a row that it never came back to collect. Live state rather
    // than a record — a restarted coordinator has offered nothing yet — so it is not persisted.
    #[serde(skip, default)]
    pub missed_pickups: u32,
    // The shared worker name this node reports under, set by `/teenode`. It is a display name and
    // nothing else: a group of interchangeable machines reads as one worker in the job embed and on
    // the console, while the roster, the leases, the purposes and the scheduler all keep addressing
    // each machine by its own name. Persisted for the same reason `drain` is — an operator who
    // grouped a farm did not mean "until the next deploy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    // The one guild this node works for, set by `/limit`. A node with a reservation is offered
    // nothing from anywhere else — not the machine's preference but the cluster's rule about it,
    // which is why it lives here and not in the node's own config: a box someone else paid for
    // should not be reachable by editing a file on it. Persisted for the same reason `drain` and
    // `group` are; an operator who reserved a node did not mean "until the next deploy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_for: Option<u64>,
}

fn one() -> u32 {
    1
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeasePhase {
    Offered,
    Leased,
}

struct LeaseState {
    node: String,
    spec: LinkJobSpec,
    phase: LeasePhase,
    offered_at: u64,
    last_seen: u64,
    cancel: bool,
}

// What the routes hand back to `pn_worker`. Both variants carry the node's reports verbatim so the
// coordinator can replay them through the ordinary payload path.
pub enum LinkEvent {
    Reports {
        job_id: u64,
        node: String,
        worker: String,
        reports: Vec<LinkReport>,
    },
    Finished {
        job_id: u64,
        node: String,
        outcome: LinkOutcome,
        reason: Option<String>,
        reports: Vec<LinkReport>,
        warnings: Vec<String>,
    },
    // Produced by the coordinator's own watchdog rather than by a node, so that losing a node and
    // a node reporting failure take the same path out.
    Lost {
        job_id: u64,
        node: String,
        // False for an offer the node never collected. Nothing was attempted, so it costs the job
        // no attempt — the same answer a decline gets, and for the same reason.
        was_running: bool,
    },
}

#[derive(Default)]
struct LinkBoard {
    nodes: HashMap<String, NodeState>,
    leases: HashMap<u64, LeaseState>,
    events: Vec<LinkEvent>,
    loaded: bool,
}

fn board() -> &'static Mutex<LinkBoard> {
    static BOARD: OnceLock<Mutex<LinkBoard>> = OnceLock::new();
    BOARD.get_or_init(|| Mutex::new(LinkBoard::default()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct LinkSettings {
    pub enabled: bool,
    pub only_node: Option<String>,
    pub lease_timeout_secs: u64,
    pub allow_build_mismatch: bool,
    // This coordinator orchestrates and never encodes. Every job that can be leased waits for a
    // node instead of falling back to the local pipeline — see `pnworker::link::coordinator`.
    pub orchestrator: bool,
}

// `env.pandora` is re-read from disk on every call, so the loop that runs twenty times a second
// cannot ask it directly. Five seconds is short enough that an operator editing `link_only_node`
// sees it take effect while they are still watching, and long enough to cost nothing.
pub fn settings() -> LinkSettings {
    static CACHE: OnceLock<Mutex<Option<(Instant, LinkSettings)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((read_at, cached)) = lock(cache).as_ref() {
        if read_at.elapsed() < Duration::from_secs(5) {
            return cached.clone();
        }
    }
    let env = get_pandora_env();
    let flag = |key: &str| {
        matches!(
            env.get(key).map(|v| v.trim().to_ascii_lowercase()).as_deref(),
            Some("true") | Some("1") | Some("yes") | Some("on")
        )
    };
    // An orchestrator has no local pipeline to fall back to, so `link_enabled` being off would
    // not mean "run everything here" as it does anywhere else — it would mean nothing ever runs at
    // all, with no error and no node to look at. The mode implies the switch rather than requiring
    // an operator to set two keys consistently.
    let orchestrator = crate::pnworker::link::coordinator::is_orchestrator();
    let fresh = LinkSettings {
        enabled: orchestrator || flag(LINK_ENABLED),
        orchestrator,
        only_node: env
            .get(LINK_ONLY_NODE)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        lease_timeout_secs: env
            .get(LINK_LEASE_TIMEOUT_SECS)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_LEASE_TIMEOUT_SECS),
        allow_build_mismatch: flag(LINK_ALLOW_BUILD_MISMATCH),
    };
    *lock(cache) = Some((Instant::now(), fresh.clone()));
    fresh
}

// The roster is advisory — a node re-registers within seconds of coming up, and the coordinator
// learns everything else from that. It is persisted for the one field a restart must not forget:
// an operator's drain flag.
fn ensure_loaded(state: &mut LinkBoard) {
    if state.loaded {
        return;
    }
    state.loaded = true;
    let Ok(contents) = std::fs::read_to_string(LINK_NODES_PATH) else {
        return;
    };
    let Ok(nodes) = serde_json::from_str::<Vec<NodeState>>(&contents) else {
        // Starting empty is survivable — every node re-registers within thirty seconds — but the
        // first `save_roster` after it would overwrite the file, and with it every drain flag,
        // `/teenode` group and `/limit` reservation the operator set. The unreadable copy is moved
        // aside first, so what was lost can still be read back by hand.
        let kept = format!("{LINK_NODES_PATH}.unreadable");
        match std::fs::rename(LINK_NODES_PATH, &kept) {
            Ok(()) => eprintln!(
                "[link] node roster at {LINK_NODES_PATH} is unreadable; kept as {kept} and starting empty"
            ),
            Err(error) => eprintln!(
                "[link] node roster at {LINK_NODES_PATH} is unreadable and could not be set aside ({error}); starting empty"
            ),
        }
        return;
    };
    for node in nodes {
        state.nodes.insert(node.name.clone(), node);
    }
}

fn save_roster(state: &LinkBoard) {
    let mut nodes = state.nodes.values().cloned().collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let Ok(body) = serde_json::to_string_pretty(&nodes) else {
        return;
    };
    let path = std::path::Path::new(LINK_NODES_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, body).is_err() {
        return;
    }
    restrict(&temporary);
    if std::fs::rename(&temporary, path).is_err() {
        std::fs::remove_file(&temporary).ok();
    }
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

// `coordinator_identity` is the libx264 this build encodes with. Comparing that rather than a hash
// of the whole toolchain is what lets a node built on another distribution, or with another Rust
// compiler, join a cluster it is genuinely encoder-equivalent to — while still refusing one whose
// encoder would make different decisions.
//
// A node that reports no identity at all is refused too. Treating an absent value as "no opinion"
// would have made the check optional for anyone who simply omitted it, which is not a property
// worth having.
pub fn register(
    request: NodeRegister,
    coordinator_identity: &str,
    purpose: NodePurpose,
) -> NodeRegistered {
    let settings = settings();
    // Resolved before the lock: it opens the git repository and reads two files, and the board is
    // one mutex shared with every other node's request.
    let release = local_release();
    // The same reason, and a stronger one: building a manifest scans the whole font and intro
    // corpus and hashes anything whose mtime moved. Doing it inside the lock put a filesystem walk
    // in front of every other node's register, renew and — through `pn_worker` — every scheduling
    // decision the coordinator makes.
    let assets_revision = crate::pnworker::link::assets::manifest().revision;
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let refusal = if settings.allow_build_mismatch {
        None
    } else if request.encoder_identity.is_empty() {
        Some("this node reported no encoder identity (set link_allow_build_mismatch to permit)".to_string())
    } else if request.encoder_identity != coordinator_identity {
        Some(format!(
            "encoder mismatch: node has {}, coordinator has {} (set link_allow_build_mismatch to permit)",
            request.encoder_identity, coordinator_identity,
        ))
    } else {
        None
    };
    if let Some(reason) = refusal {
        return NodeRegistered {
            accepted: false,
            reason: Some(reason),
            renew_secs: DEFAULT_RENEW_SECS,
            lease_timeout_secs: settings.lease_timeout_secs,
            assets_revision,
            purpose,
            // A refused node is still told where the cluster is. Its refusal may well be that it
            // is running the wrong build, and the answer to that is on this line.
            release,
            // A refused node takes nothing anyway; it is told what the roster holds so the answer
            // means the same thing whether or not the registration was accepted.
            drain: state.nodes.get(&request.node).map(|node| node.drain).unwrap_or(false),
        };
    }
    let existing = state.nodes.get(&request.node);
    let drain = existing.map(|node| node.drain).unwrap_or(false);
    let drain_reason = existing.and_then(|node| node.drain_reason.clone()).filter(|_| drain);
    // A node that is answering registers is not necessarily a node that collects what it is
    // offered, so the count is carried rather than cleared here. `claim` is what proves it, and
    // `claim` is what resets it.
    let missed_pickups = existing.map(|node| node.missed_pickups).unwrap_or(0);
    let group = existing.and_then(|node| node.group.clone());
    // Survives the node re-registering, exactly as the drain flag does. A node that reconnects has
    // not been un-reserved; only `/limit` releases it.
    let reserved_for = existing.and_then(|node| node.reserved_for);
    let registered_at = existing
        .map(|node| node.registered_at)
        .filter(|value| *value > 0)
        .unwrap_or_else(now);
    let node = NodeState {
        name: request.node.clone(),
        pandora_version: request.pandora_version,
        encoder_identity: request.encoder_identity,
        ffmpeg_version: request.ffmpeg_version,
        threads: request.threads,
        // Clamped, not trusted: this is the node's own answer about itself and a mistyped one
        // would have the coordinator hand it every job in the queue and then wait for all of them.
        max_jobs: request.max_jobs.clamp(1, MAX_JOBS_CEILING),
        encoders: request.encoders,
        purpose,
        build: request.build,
        migration_error: request.migration_error,
        registered_at,
        last_seen: now(),
        drain,
        drain_reason,
        missed_pickups,
        group,
        reserved_for,
    };
    state.nodes.insert(request.node, node);
    save_roster(&state);
    NodeRegistered {
        accepted: true,
        reason: None,
        renew_secs: DEFAULT_RENEW_SECS,
        lease_timeout_secs: settings.lease_timeout_secs,
        assets_revision,
        purpose,
        release,
        drain,
    }
}

// What this machine is running, as a node is told it. The commit is read from the checkout rather
// than from the build record so that a coordinator whose repository moved underneath it — someone
// pulling by hand on the box — advertises where it actually is, not where it last thought it was.
pub fn local_release() -> ReleaseInfo {
    // Cached for a moment, for the same reason `settings` is. Every node asks on every loop pass
    // and again on every register, and each answer opens the git repository, reads HEAD and reads
    // two files under `DB/` — on the thread serving the whole API. Five seconds is far below any
    // interval that matters here and turns a cluster's worth of polling into one read.
    static CACHE: OnceLock<Mutex<Option<(Instant, ReleaseInfo)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((read_at, cached)) = lock(cache).as_ref() {
        if read_at.elapsed() < Duration::from_secs(5) {
            return cached.clone();
        }
    }
    let fresh = read_local_release();
    *lock(cache) = Some((Instant::now(), fresh.clone()));
    fresh
}

fn read_local_release() -> ReleaseInfo {
    let record = crate::lib::release::read();
    let commit = crate::pnworker::pull::head_oid(&crate::lib::release::repo_path())
        .unwrap_or(record.commit);
    ReleaseInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: record.build,
        commit,
        reset: forced_reset(),
    }
}

// `/gitforce` leaves this behind so the reset it performed here is performed on every node too.
// It is a file rather than a flag in memory because the command ends in `exit(0)`: the coordinator
// that has to advertise the reset is the process *after* the one that decided on it.
const FORCE_MARKER: &str = "DB/config/global/environment/gitforce.pandora";

pub fn mark_forced_reset(build: u64) {
    if let Some(parent) = std::path::Path::new(FORCE_MARKER).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Err(error) = std::fs::write(FORCE_MARKER, format!("{build}\n")) {
        eprintln!("[Pandora] could not record the forced reset: {error}");
    }
}

// True while the current build is the one `/gitforce` produced. Tying the marker to a build rather
// than clearing it on a timer is what stops a reset from applying to every deploy that follows it:
// the next ordinary gitsync bumps past it and the marker stops matching.
fn forced_reset() -> bool {
    let Ok(contents) = std::fs::read_to_string(FORCE_MARKER) else {
        return false;
    };
    contents.trim().parse::<u64>().ok() == Some(crate::lib::release::read().build)
}

pub fn touch(node: &str) {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    if let Some(entry) = state.nodes.get_mut(node) {
        entry.last_seen = now();
    }
}

// Why no node took a job, when none did. On a coordinator that falls back to its own encoders the
// answer never mattered — the job simply ran here — but an orchestrator has no fallback, and a job
// that sits still with nothing written anywhere is the exact shape of a failure nobody can debug.
// So the filter that emptied the candidate list is named, and the caller can put it in front of an
// operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoNode {
    Disabled,
    // The job itself cannot travel — the wrong type, no fetchable source, or out of attempts.
    // Never a cluster problem, so it is never rendered as one.
    NotLeasable,
    NoNodesRegistered,
    // Every registered node was excluded, and this is the last reason one was. Ordered by how
    // specific the answer is: "your GPU preset has no GPU node" beats "everything is busy".
    AllBusy,
    AllDraining,
    AllStale,
    OnlyNode(String),
    ReservedElsewhere,
    NoHardware(&'static str),
    NoEncoder(String),
    AllAlreadyTried,
}

impl NoNode {
    pub fn describe(&self) -> String {
        match self {
            NoNode::Disabled => "link offload is switched off".to_string(),
            NoNode::NotLeasable => "this job cannot be run on a node".to_string(),
            NoNode::NoNodesRegistered => "no Pandora Mini node has registered".to_string(),
            NoNode::AllBusy => "every node is at its job limit".to_string(),
            NoNode::AllDraining => "every node is draining".to_string(),
            NoNode::AllStale => "no node has been heard from recently".to_string(),
            NoNode::OnlyNode(name) => {
                format!("link_only_node pins offload to `{name}`, which cannot take this job")
            }
            NoNode::ReservedElsewhere => {
                "every free node is reserved for another server".to_string()
            }
            NoNode::NoHardware(hardware) => {
                format!("no node is marked for {hardware} work")
            }
            NoNode::NoEncoder(codec) => {
                format!("no node has proved the `{codec}` encoder this preset needs")
            }
            NoNode::AllAlreadyTried => {
                "every node that could run this job has already declined or lost it".to_string()
            }
        }
    }
}

// The node a job should be offered to, or why none can take it. Called from `pn_worker`, which is
// why it never blocks: on an ordinary coordinator a job that finds no free node runs here rather
// than waiting for one, and on an orchestrator it waits with the reason attached.
//
// `excluded` are the machines this job has already been turned away from: one that declined it
// outright, and one that took it and then lost it. On an ordinary coordinator the list stays empty
// — either answer sends the job to the local pipeline and there is no second round — but an
// orchestrator has to keep offering, and offering it straight back to the machine that just failed
// it is how one unhealthy box turns into a job that never runs anywhere.
pub fn pick_node(preset: &str, server: Option<u64>, excluded: &[String]) -> Result<String, NoNode> {
    let settings = settings();
    if !settings.enabled {
        return Err(NoNode::Disabled);
    }
    let hardware = crate::lib::mpeg::preset::hardware_for(preset);
    let required_encoder = (hardware == crate::lib::mpeg::preset::PresetHardware::Gpu)
        .then(|| crate::lib::mpeg::preset::video_codec_for(preset))
        .flatten();
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let mut busy: HashMap<String, u32> = HashMap::new();
    for lease in state.leases.values() {
        *busy.entry(lease.node.clone()).or_insert(0) += 1;
    }
    let nodes = state.nodes.values().cloned().collect::<Vec<_>>();
    select_node(
        &nodes,
        &busy,
        &settings,
        hardware,
        required_encoder.as_deref(),
        server,
        excluded,
        now(),
    )
}

// The choice itself, with nothing around it that has to be locked or read off disk. It is split
// out because the interesting half is which filter emptied the list — the answer an orchestrator
// puts in front of an operator — and that is only worth having if it can be tested against a
// roster rather than against whatever `env.pandora` happens to say on the machine running it.
#[allow(clippy::too_many_arguments)]
fn select_node(
    nodes: &[NodeState],
    busy: &HashMap<String, u32>,
    settings: &LinkSettings,
    hardware: crate::lib::mpeg::preset::PresetHardware,
    required_encoder: Option<&str>,
    server: Option<u64>,
    excluded: &[String],
    now: u64,
) -> Result<String, NoNode> {
    if nodes.is_empty() {
        return Err(NoNode::NoNodesRegistered);
    }
    let stale_before = now.saturating_sub(settings.lease_timeout_secs);
    // Every node that is turned away records why, so an empty candidate list still has an answer.
    // The passes run from the least specific reason to the most, and the last one recorded wins,
    // so what an operator is shown is the reason they can act on: "no node is marked for gpu work"
    // rather than "everything is busy".
    let mut why = NoNode::AllBusy;
    let mut candidates = Vec::new();
    for node in nodes {
        if node.drain {
            why = NoNode::AllDraining;
            continue;
        }
        if node.last_seen < stale_before {
            why = NoNode::AllStale;
            continue;
        }
        if let Some(only) = settings.only_node.as_deref() {
            if node.name != only {
                why = NoNode::OnlyNode(only.to_string());
                continue;
            }
        }
        // A reserved node belongs to one guild. A job from anywhere else does not see it, and a
        // job with no guild at all — nothing on this machine submits one, but the field is an
        // Option — sees no reserved node either, because "unknown" is not the guild it was kept
        // for. Note this is not the inverse rule: that guild still uses every other free node.
        if !serves_server(node.reserved_for, server) {
            why = NoNode::ReservedElsewhere;
            continue;
        }
        if !advertises_encoder(&node.encoders, required_encoder) {
            why = NoNode::NoEncoder(required_encoder.unwrap_or_default().to_string());
            continue;
        }
        // A GPU preset on a CPU box does not fail cleanly: ffmpeg either refuses the encoder or
        // silently falls back to a software one, and the second outcome ships a release at a
        // quality tier nobody chose. The purpose comes off the node's token, so this is the
        // coordinator's own answer rather than the node's claim about itself.
        if !node.purpose.accepts(hardware) {
            why = NoNode::NoHardware(
                if hardware == crate::lib::mpeg::preset::PresetHardware::Gpu {
                    "gpu"
                } else {
                    "cpu"
                },
            );
            continue;
        }
        if excluded.iter().any(|name| name == &node.name) {
            why = NoNode::AllAlreadyTried;
            continue;
        }
        if busy.get(&node.name).copied().unwrap_or(0) >= node.max_jobs {
            continue;
        }
        candidates.push(node);
    }
    // Most idle first, then the most threads, so a cluster of unequal machines fills its biggest
    // free box before its smallest.
    candidates.sort_by(|a, b| {
        let a_busy = busy.get(&a.name).copied().unwrap_or(0);
        let b_busy = busy.get(&b.name).copied().unwrap_or(0);
        a_busy
            .cmp(&b_busy)
            .then(b.threads.cmp(&a.threads))
            .then(a.name.cmp(&b.name))
    });
    match candidates.first() {
        Some(node) => Ok(node.name.clone()),
        None => Err(why),
    }
}

// Whether a node will take a job from this guild. A node with no reservation takes anything; a
// reserved one takes that guild and nothing else, including a job carrying no guild at all —
// "unknown" is not the server it was kept for.
//
// This is not the inverse rule: reserving a node says nothing about which nodes that guild uses,
// and it keeps every other one it could already reach.
fn serves_server(reserved_for: Option<u64>, server: Option<u64>) -> bool {
    match reserved_for {
        Some(reserved) => server == Some(reserved),
        None => true,
    }
}

fn advertises_encoder(encoders: &[String], required: Option<&str>) -> bool {
    required.is_none_or(|required| encoders.iter().any(|encoder| encoder == required))
}

// Puts a job under a node's name for it to collect. False when the spec carries a job id this
// cannot key a lease on — `unwrap_or(0)` used to bucket every such spec under the same id, where
// the second one silently evicted the first. The caller keeps the job local instead.
pub fn offer(node: &str, spec: LinkJobSpec) -> bool {
    let Ok(job_id) = spec.job_id.parse::<u64>() else {
        eprintln!("[link] refusing to offer a job whose id is not a number: {:?}", spec.job_id);
        return false;
    };
    if job_id == 0 {
        eprintln!("[link] refusing to offer a job with no id");
        return false;
    }
    let mut state = lock(board());
    ensure_loaded(&mut state);
    // `pick_node` answered several awaits ago — resolving the server's upload policy and preparing
    // the work directory both happen in between — and the board is written by every link route in
    // the meantime. A node that has since been drained, removed, or filled up would take the offer
    // nowhere: nobody collects it, it expires sixty seconds later, and the job's only symptom is a
    // minute of sitting still. Re-asking here costs one map lookup and turns that into an
    // immediate local run, or, on an orchestrator, an immediate offer to somebody else.
    let timeout = settings().lease_timeout_secs;
    let held = state
        .leases
        .values()
        .filter(|lease| lease.node == node)
        .count() as u32;
    let Some(entry) = state.nodes.get(node) else {
        eprintln!("[link] {node} is no longer registered; job {job_id} was not offered to it");
        return false;
    };
    if entry.drain {
        eprintln!("[link] {node} started draining; job {job_id} was not offered to it");
        return false;
    }
    if !is_alive(entry, timeout) {
        eprintln!("[link] {node} has gone quiet; job {job_id} was not offered to it");
        return false;
    }
    if held >= entry.max_jobs {
        eprintln!("[link] {node} filled up; job {job_id} was not offered to it");
        return false;
    }
    state.leases.insert(
        job_id,
        LeaseState {
            node: node.to_string(),
            spec,
            phase: LeasePhase::Offered,
            offered_at: now(),
            last_seen: now(),
            cancel: false,
        },
    );
    true
}

// A node collecting the job it was offered. Leases are targeted at a node before it polls, so this
// never has to choose anything: it hands over whatever is waiting under that node's name.
pub fn claim(node: &str) -> Option<LinkJobSpec> {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let job_id = state
        .leases
        .iter()
        .find(|(_, lease)| lease.node == node && lease.phase == LeasePhase::Offered)
        .map(|(job_id, _)| *job_id)?;
    let lease = state.leases.get_mut(&job_id)?;
    lease.phase = LeasePhase::Leased;
    lease.last_seen = now();
    let spec = lease.spec.clone();
    // Collecting an offer is the only proof that this node's poll loop is alive. It is what the
    // missed-pickup count is counting the absence of, so it is what clears it.
    if let Some(entry) = state.nodes.get_mut(node) {
        entry.missed_pickups = 0;
    }
    Some(spec)
}

pub fn renew(job_id: u64, request: LeaseRenew) -> LeaseControl {
    // Read before the board is locked: building a manifest touches the filesystem, and the loop
    // publishing into this board must never wait behind a directory scan.
    let assets_revision = crate::pnworker::link::assets::manifest().revision;
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let mut drain = false;
    if let Some(entry) = state.nodes.get_mut(&request.node) {
        entry.last_seen = now();
        drain = entry.drain;
    }
    let Some(lease) = state.leases.get_mut(&job_id) else {
        // The coordinator already reclaimed this job and gave it to somebody else. Telling the node
        // to drop it is the only thing that stops two machines finishing the same work.
        return LeaseControl {
            assets_revision,
            cancel: false,
            abandon: true,
            drain,
        };
    };
    if lease.node != request.node {
        return LeaseControl {
            assets_revision,
            cancel: false,
            abandon: true,
            drain,
        };
    }
    lease.last_seen = now();
    lease.phase = LeasePhase::Leased;
    let cancel = lease.cancel;
    if !request.reports.is_empty() {
        state.events.push(LinkEvent::Reports {
            job_id,
            node: request.node,
            worker: request.worker,
            reports: request.reports,
        });
    }
    LeaseControl {
        assets_revision,
        cancel,
        abandon: false,
        drain,
    }
}

pub fn finish(job_id: u64, result: LeaseResult) -> bool {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let Some(lease) = state.leases.get(&job_id) else {
        return false;
    };
    if lease.node != result.node {
        return false;
    }
    state.leases.remove(&job_id);
    if let Some(entry) = state.nodes.get_mut(&result.node) {
        entry.last_seen = now();
    }
    state.events.push(LinkEvent::Finished {
        job_id,
        node: result.node,
        outcome: result.outcome,
        reason: result.reason,
        reports: result.reports,
        warnings: result.warnings,
    });
    true
}

pub fn request_cancel(job_id: u64) -> bool {
    let mut state = lock(board());
    match state.leases.get_mut(&job_id) {
        Some(lease) => {
            lease.cancel = true;
            true
        }
        None => false,
    }
}

pub fn release(job_id: u64) {
    let mut state = lock(board());
    state.leases.remove(&job_id);
}

pub fn drain_events() -> Vec<LinkEvent> {
    let mut state = lock(board());
    std::mem::take(&mut state.events)
}

// A lease the coordinator has taken back, and which of the two ways it lost it.
//
// The distinction is not cosmetic. A node that took a job and went quiet may have encoded most of
// it; a node that never came back for an offer attempted nothing at all, exactly like one that
// declined. Charging both against the same `LINK_MAX_ATTEMPTS` budget meant two silent nodes could
// pin a job to the coordinator for good — and, on an orchestrator, fail it outright — without any
// machine ever having tried to run it.
pub struct ExpiredLease {
    pub job_id: u64,
    pub node: String,
    // False for an offer nobody collected. The caller spends one of the job's attempts only for a
    // lease that was actually taken up.
    pub was_running: bool,
}

// Leases whose node has gone quiet. An offered lease is given a shorter rope than a running one:
// nothing has started, so re-offering it costs nothing but the round trip.
pub fn expire_leases(timeout_secs: u64) -> Vec<ExpiredLease> {
    let now = now();
    let mut state = lock(board());
    let lost = state
        .leases
        .iter()
        .filter(|(_, lease)| match lease.phase {
            LeasePhase::Offered => now.saturating_sub(lease.offered_at) > OFFER_PICKUP_SECS,
            LeasePhase::Leased => now.saturating_sub(lease.last_seen) > timeout_secs,
        })
        .map(|(job_id, lease)| ExpiredLease {
            job_id: *job_id,
            node: lease.node.clone(),
            was_running: lease.phase == LeasePhase::Leased,
        })
        .collect::<Vec<_>>();
    for lease in &lost {
        state.leases.remove(&lease.job_id);
    }
    // A node that registers on time and never collects what it is handed is the worst shape of
    // failure this system has: it passes every liveness check, so the scheduler keeps choosing it,
    // and every job it is given loses a minute before going somewhere else. Draining it is the
    // only thing that takes it out of `pick_node`, and recording why is the only thing that tells
    // an operator which box to look at — a node that drained itself with no explanation would be
    // indistinguishable from one somebody drained on purpose and forgot.
    let mut drained = Vec::new();
    for lease in lost.iter().filter(|lease| !lease.was_running) {
        let Some(entry) = state.nodes.get_mut(&lease.node) else {
            continue;
        };
        entry.missed_pickups = entry.missed_pickups.saturating_add(1);
        if entry.missed_pickups >= MAX_MISSED_PICKUPS && !entry.drain {
            entry.drain = true;
            entry.drain_reason = Some(format!(
                "drained automatically after {} offers it never collected",
                entry.missed_pickups
            ));
            drained.push(lease.node.clone());
        }
    }
    if !drained.is_empty() {
        save_roster(&state);
        for node in drained {
            eprintln!(
                "[link] {node} | registered but never collects its offers; draining it (use /drainnode drain:false once it is fixed)"
            );
        }
    }
    lost
}

pub fn set_drain(node: &str, drain: bool) -> bool {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let Some(entry) = state.nodes.get_mut(node) else {
        return false;
    };
    entry.drain = drain;
    // An operator's decision replaces the coordinator's, in both directions: un-draining a node it
    // drained itself also clears the count that drained it, so one more missed offer does not put
    // it straight back.
    entry.drain_reason = None;
    if !drain {
        entry.missed_pickups = 0;
    }
    save_roster(&state);
    true
}

// `/teenode`. An empty or `-` group ungroups; anything else is the shared display name. The value
// is rejected rather than sanitised when it could not be a worker label, because a name with a
// newline or a `|` in it would be a worker column nothing can read back.
pub fn set_group(node: &str, group: Option<&str>) -> Result<Option<String>, String> {
    let group = match group.map(str::trim).filter(|value| !value.is_empty() && *value != "-") {
        Some(value) => {
            if value.chars().any(|c| c.is_whitespace() || c == '|' || c.is_control()) {
                return Err("a group name has no spaces, control characters, or `|`".to_string());
            }
            if value.chars().count() > 32 {
                return Err("a group name is at most 32 characters".to_string());
            }
            Some(value.to_string())
        }
        None => None,
    };
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let Some(entry) = state.nodes.get_mut(node) else {
        return Err(format!("No node `{}` has registered.", node));
    };
    entry.group = group.clone();
    save_roster(&state);
    Ok(group)
}

// Reserve a node for one guild, or release it. Returns what it is reserved for afterwards, so the
// caller reports the state that actually landed rather than the one it asked for.
pub fn set_reserved(node: &str, server: Option<u64>) -> Result<Option<u64>, String> {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let Some(entry) = state.nodes.get_mut(node) else {
        return Err(format!("No node `{}` has registered.", node));
    };
    entry.reserved_for = server;
    save_roster(&state);
    Ok(server)
}

// The name a node's work reports under: its group when it has one, itself otherwise. Every worker
// label goes through here, so grouping is one lookup rather than a rule each call site remembers.
pub fn display_name(node: &str) -> String {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    state
        .nodes
        .get(node)
        .and_then(|entry| entry.group.clone())
        .unwrap_or_else(|| node.to_string())
}

pub fn remove_node(node: &str) -> bool {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let removed = state.nodes.remove(node).is_some();
    if removed {
        save_roster(&state);
    }
    removed
}

pub fn node_for_job(job_id: u64) -> Option<String> {
    let state = lock(board());
    state.leases.get(&job_id).map(|lease| lease.node.clone())
}

// The registered nodes and the jobs each currently holds, name-ordered. Shared by `/lsnode` and the
// worker snapshot so the two can never disagree about who is in the cluster.
pub fn roster() -> Vec<(NodeState, Vec<u64>)> {
    let mut state = lock(board());
    ensure_loaded(&mut state);
    let mut busy: HashMap<String, Vec<u64>> = HashMap::new();
    for (job_id, lease) in &state.leases {
        busy.entry(lease.node.clone()).or_default().push(*job_id);
    }
    for jobs in busy.values_mut() {
        jobs.sort();
    }
    let mut nodes = state
        .nodes
        .values()
        .map(|node| {
            let jobs = busy.get(&node.name).cloned().unwrap_or_default();
            (node.clone(), jobs)
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    nodes
}

// Whether a node has been heard from recently enough to be offered work. The same staleness bound
// `pick_node` applies, so what `/lsnode` calls alive is what the scheduler calls available.
pub fn is_alive(node: &NodeState, timeout_secs: u64) -> bool {
    now().saturating_sub(node.last_seen) < timeout_secs
}

pub fn seconds_since_seen(node: &NodeState) -> u64 {
    now().saturating_sub(node.last_seen)
}

// Rendered into the worker snapshot, which is what `/workers` and `GET /api/v1/workers` read. The
// cluster is nowhere in the jobs table either, for the same reason the queue is not.
pub fn nodes_view() -> Value {
    json!(
        roster()
            .into_iter()
            .map(|(node, jobs)| json!({
                "node": node.name,
                // What its work reports as, when that is not its own name. `/teenode` sets it, and
                // it is the only field here that is a display choice rather than a fact about the
                // machine — hence both, so a snapshot can still name the box that is stalling.
                "group": node.group,
                "threads": node.threads,
                "max_jobs": node.max_jobs,
                "encoders": node.encoders,
                "drain": node.drain,
                // Null unless the coordinator drained it rather than an operator. A node that
                // took itself out of rotation with no reason attached reads exactly like one
                // somebody drained on purpose and forgot about.
                "drain_reason": node.drain_reason,
                // Null unless `/limit` reserved it. A reserved node produces no other visible
                // symptom — it simply stops being offered work and reads as idle — so the console
                // has to be able to say why.
                "reserved_for": node.reserved_for.map(|id| id.to_string()),
                "last_seen_secs": seconds_since_seen(&node),
                "jobs": jobs.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "pandora_version": node.pandora_version,
                "encoder_identity": node.encoder_identity,
                "purpose": node.purpose.label(),
                "build": node.build,
                "migration_error": node.migration_error,
            }))
            .collect::<Vec<_>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_capability_requires_the_exact_proved_encoder() {
        let encoders = vec!["h264_nvenc".to_string()];
        assert!(advertises_encoder(&encoders, Some("h264_nvenc")));
        assert!(!advertises_encoder(&encoders, Some("av1_nvenc")));
        assert!(!advertises_encoder(&[], Some("av1_nvenc")));
        assert!(advertises_encoder(&[], None));
    }

    // `/limit` is one-way: it narrows who may use a node, never which nodes a server may use.
    #[test]
    fn a_reserved_node_serves_its_own_server_and_nobody_else() {
        // Unreserved: every job, including one carrying no guild.
        assert!(serves_server(None, Some(1)));
        assert!(serves_server(None, None));

        // Reserved: that guild only.
        assert!(serves_server(Some(7), Some(7)));
        assert!(!serves_server(Some(7), Some(8)));
        // A job with no guild is not the guild it was kept for, so it is not offered the node —
        // it runs on the coordinator or on an unreserved one, exactly as it would have before.
        assert!(!serves_server(Some(7), None));
    }

    fn spec(job_id: u64) -> LinkJobSpec {
        LinkJobSpec {
            job_id: job_id.to_string(),
            job_type: "Encode".to_string(),
            source_kind: "magnet".to_string(),
            source: "magnet:?xt=urn:btih:abc".to_string(),
            display_link: None,
            file_index: None,
            probe_job_id: None,
            torrent_b64: None,
            subtitle_b64: String::new(),
            watermark_b64: None,
            preset: "standard".to_string(),
            lang: "en".to_string(),
            server_id: None,
            gdrive_folder_global: None,
            gdrive_folder_local: None,
            return_output: false,
            drive_only: false,
            intro_group: None,
            assets_revision: String::new(),
        }
    }

    // The board is one process-wide structure, exactly as it is in production, so each test uses
    // node names of its own rather than racing its neighbours for the same roster entry.

    // A node the scheduler will actually accept. `offer` re-checks the roster, so a test that
    // skips this is testing a machine the coordinator has never heard of.
    fn register_test_node(name: &str) {
        register(
            NodeRegister {
                node: name.to_string(),
                pandora_version: "test".to_string(),
                encoder_identity: "x264-board-test".to_string(),
                ffmpeg_version: String::new(),
                threads: 4,
                max_jobs: 2,
                encoders: Vec::new(),
                build: 0,
                migration_error: None,
            },
            "x264-board-test",
            NodePurpose::Cpu,
        );
    }

    // Treating an absent identity as "no opinion" would make the whole check optional for anyone
    // who simply omitted the field, which is not a property worth having.
    #[test]
    fn a_node_reporting_no_encoder_identity_is_refused() {
        let request = NodeRegister {
            node: "identity-none".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: String::new(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs: 1,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        let answer = register(request, "x264-165-0.165.x-pandora", NodePurpose::Cpu);
        assert!(!answer.accepted);
        assert!(
            answer.reason.unwrap_or_default().contains("no encoder identity"),
            "the refusal should say what was missing",
        );
    }

    // A node whose x264 would make different rate decisions is refused, and the message has to
    // name both sides or nobody can act on it.
    #[test]
    fn a_different_encoder_is_refused_and_both_sides_are_named() {
        let request = NodeRegister {
            node: "identity-other".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-164-0.164.3108-stock".to_string(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs: 1,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        let answer = register(request, "x264-165-0.165.x-pandora", NodePurpose::Cpu);
        assert!(!answer.accepted);
        let reason = answer.reason.unwrap_or_default();
        assert!(reason.contains("x264-164-0.164.3108-stock"), "{reason}");
        assert!(reason.contains("x264-165-0.165.x-pandora"), "{reason}");
    }

    // A lease is targeted before the node polls, so a second node polling must not be able to take
    // work meant for the first. Two machines on one job is the failure this whole lease phase
    // exists to prevent.
    #[test]
    fn a_claim_only_returns_that_nodes_own_offer() {
        release(9001);
        register_test_node("claim-a");
        register_test_node("claim-b");
        offer("claim-a", spec(9001));
        assert!(claim("claim-b").is_none());
        let claimed = claim("claim-a").expect("the offered node could not claim its own job");
        assert_eq!(claimed.job_id, "9001");
        assert!(claim("claim-a").is_none());
        release(9001);
        remove_node("claim-a");
        remove_node("claim-b");
    }

    // A node that comes back after the coordinator gave its job away has to be told to stop, or it
    // finishes work a second machine is already doing.
    #[test]
    fn renewing_a_reclaimed_lease_is_told_to_abandon() {
        release(9002);
        register_test_node("reclaim-a");
        offer("reclaim-a", spec(9002));
        claim("reclaim-a");
        release(9002);
        let control = renew(
            9002,
            LeaseRenew {
                node: "reclaim-a".to_string(),
                worker: "enc-main".to_string(),
                reports: Vec::new(),
                logs: Vec::new(),
            },
        );
        assert!(control.abandon);
        remove_node("reclaim-a");
    }

    // A renew from the wrong node must not keep somebody else's lease alive.
    #[test]
    fn a_renew_from_another_node_does_not_hold_the_lease() {
        release(9003);
        register_test_node("stranger-a");
        offer("stranger-a", spec(9003));
        claim("stranger-a");
        let control = renew(
            9003,
            LeaseRenew {
                node: "stranger-b".to_string(),
                worker: String::new(),
                reports: Vec::new(),
                logs: Vec::new(),
            },
        );
        assert!(control.abandon);
        release(9003);
        remove_node("stranger-a");
    }

    // Grouping renames the work and nothing else. If it merged the roster entries instead, draining
    // one machine would drain the farm and a stall would name a group rather than the box to go
    // and look at.
    #[test]
    fn a_group_renames_the_work_and_leaves_the_nodes_apart() {
        let request = |name: &str| NodeRegister {
            node: name.to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-group-test".to_string(),
            ffmpeg_version: String::new(),
            threads: 4,
            max_jobs: 1,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        register(request("tee-a"), "x264-group-test", NodePurpose::Cpu);
        register(request("tee-b"), "x264-group-test", NodePurpose::Cpu);

        assert_eq!(display_name("tee-a"), "tee-a");
        set_group("tee-a", Some("farm")).unwrap();
        set_group("tee-b", Some("farm")).unwrap();
        assert_eq!(display_name("tee-a"), "farm");
        assert_eq!(display_name("tee-b"), "farm");

        // Still two machines, each addressable on its own name.
        let names = roster()
            .into_iter()
            .filter(|(node, _)| node.group.as_deref() == Some("farm"))
            .map(|(node, _)| node.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["tee-a", "tee-b"]);
        assert!(set_drain("tee-a", true));
        assert!(roster().iter().any(|(node, _)| node.name == "tee-b" && !node.drain));

        // Ungrouping puts the node's own name back.
        set_group("tee-a", Some("-")).unwrap();
        assert_eq!(display_name("tee-a"), "tee-a");
        set_group("tee-b", None).unwrap();
        assert_eq!(display_name("tee-b"), "tee-b");

        remove_node("tee-a");
        remove_node("tee-b");
    }

    // A group name lands in the `worker` column of a job row and in a Discord embed. One with a
    // `|` or a newline in it would be a label nothing can read back.
    #[test]
    fn an_unusable_group_name_is_refused_rather_than_sanitised() {
        let request = NodeRegister {
            node: "tee-strict".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-group-test".to_string(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs: 1,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        register(request, "x264-group-test", NodePurpose::Cpu);
        assert!(set_group("tee-strict", Some("two words")).is_err());
        assert!(set_group("tee-strict", Some("a|b")).is_err());
        assert!(set_group("tee-strict", Some(&"x".repeat(33))).is_err());
        assert_eq!(display_name("tee-strict"), "tee-strict");
        // A node that never registered cannot be grouped either.
        assert!(set_group("tee-absent", Some("farm")).is_err());
        remove_node("tee-strict");
    }

    // The renew answer carries `drain` too, but only a node that is working sends renews. An idle
    // node's only channel is the register, so un-draining one has to reach it here or the node
    // never starts polling again and every job offered to it expires uncollected.
    #[test]
    fn the_register_answer_carries_the_drain_flag() {
        let request = || NodeRegister {
            node: "drain-answer".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-drain-test".to_string(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs: 1,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        assert!(!register(request(), "x264-drain-test", NodePurpose::Cpu).drain);
        assert!(set_drain("drain-answer", true));
        assert!(register(request(), "x264-drain-test", NodePurpose::Cpu).drain);
        assert!(set_drain("drain-answer", false));
        assert!(!register(request(), "x264-drain-test", NodePurpose::Cpu).drain);
        remove_node("drain-answer");
    }

    // The purpose is not persisted, so a coordinator restart reloads every node as `cpu` — which
    // is only ever correct because the node says otherwise again a moment later.
    #[test]
    fn a_re_register_reasserts_a_purpose_the_roster_did_not_keep() {
        let request = || NodeRegister {
            node: "purpose-again".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-purpose-test".to_string(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs: 1,
            encoders: vec!["h264_nvenc".to_string()],
            build: 0,
            migration_error: None,
        };
        register(request(), "x264-purpose-test", NodePurpose::Gpu);
        let gpu = roster()
            .into_iter()
            .find(|(node, _)| node.name == "purpose-again")
            .map(|(node, _)| node.purpose);
        assert_eq!(gpu, Some(NodePurpose::Gpu));

        // What a restart leaves behind: everything else survives, the purpose does not.
        let serialised = serde_json::to_string(&roster()
            .into_iter()
            .find(|(node, _)| node.name == "purpose-again")
            .map(|(node, _)| node)
            .unwrap())
            .unwrap();
        let reloaded: NodeState = serde_json::from_str(&serialised).unwrap();
        assert_eq!(reloaded.purpose, NodePurpose::Cpu);
        assert_eq!(reloaded.encoders, ["h264_nvenc"]);

        // And what the node saying hello again puts back.
        register(request(), "x264-purpose-test", NodePurpose::Gpu);
        assert_eq!(
            roster()
                .into_iter()
                .find(|(node, _)| node.name == "purpose-again")
                .map(|(node, _)| node.purpose),
            Some(NodePurpose::Gpu)
        );
        remove_node("purpose-again");
    }

    #[test]
    fn an_offer_nobody_collects_expires_and_is_reported_lost() {
        release(9004);
        register_test_node("expiry-a");
        offer("expiry-a", spec(9004));
        {
            let mut state = lock(board());
            if let Some(lease) = state.leases.get_mut(&9004) {
                lease.offered_at = now().saturating_sub(OFFER_PICKUP_SECS + 1);
            }
        }
        let lost = expire_leases(DEFAULT_LEASE_TIMEOUT_SECS);
        assert!(lost.iter().any(|lease| lease.job_id == 9004 && lease.node == "expiry-a"));
        // Nothing was attempted, so the caller must not charge the job one of its attempts.
        assert!(lost.iter().all(|lease| !lease.was_running));
        assert!(claim("expiry-a").is_none());
        remove_node("expiry-a");
    }

    // `pick_node` answers several awaits before `offer` runs — the server's upload policy and the
    // work directory are both resolved in between — and every link route writes the board in the
    // meantime. An offer to a node that stopped being a candidate is collected by nobody, expires
    // sixty seconds later, and looks from the outside like a job that simply did not start.
    #[test]
    fn an_offer_to_a_node_that_stopped_taking_work_is_refused_at_once() {
        register_test_node("offer-gone");
        assert!(offer("offer-gone", spec(9005)));
        release(9005);

        assert!(set_drain("offer-gone", true));
        assert!(!offer("offer-gone", spec(9005)), "a draining node takes nothing");
        assert!(set_drain("offer-gone", false));

        remove_node("offer-gone");
        assert!(!offer("offer-gone", spec(9005)), "an unregistered node takes nothing");
        assert!(claim("offer-gone").is_none());
    }

    // The worst shape of node failure: it registers on time, so every liveness check passes and the
    // scheduler keeps choosing it, and it never collects a thing. Each round costs a job a minute.
    #[test]
    fn a_node_that_never_collects_its_offers_is_drained_with_a_reason() {
        register_test_node("blackhole");
        for job_id in 9100..9100 + MAX_MISSED_PICKUPS as u64 {
            assert!(offer("blackhole", spec(job_id)), "job {job_id} should have been offered");
            {
                let mut state = lock(board());
                if let Some(lease) = state.leases.get_mut(&job_id) {
                    lease.offered_at = now().saturating_sub(OFFER_PICKUP_SECS + 1);
                }
            }
            expire_leases(DEFAULT_LEASE_TIMEOUT_SECS);
        }
        let drained = roster()
            .into_iter()
            .find(|(node, _)| node.name == "blackhole")
            .map(|(node, _)| node)
            .expect("the node should still be on the roster");
        assert!(drained.drain, "it should have taken itself out of rotation");
        assert!(
            drained.drain_reason.unwrap_or_default().contains("never collected"),
            "a drain nobody asked for has to say why, or it reads like one an operator forgot",
        );

        // An operator's decision replaces the coordinator's, count and all.
        assert!(set_drain("blackhole", false));
        let back = roster()
            .into_iter()
            .find(|(node, _)| node.name == "blackhole")
            .map(|(node, _)| node)
            .unwrap();
        assert!(!back.drain);
        assert!(back.drain_reason.is_none());
        assert_eq!(back.missed_pickups, 0);
        remove_node("blackhole");
    }

    // An orchestrator holds a job until a node can take it, so "no" has to come with a reason: a
    // job sitting still with nothing written anywhere is the one failure this mode could add.
    #[test]
    fn a_refusal_names_the_filter_that_emptied_the_cluster() {
        use crate::lib::mpeg::preset::PresetHardware;

        let settings = LinkSettings {
            enabled: true,
            orchestrator: true,
            only_node: None,
            lease_timeout_secs: DEFAULT_LEASE_TIMEOUT_SECS,
            allow_build_mismatch: false,
        };
        let now = 1_000_000u64;
        let node = |name: &str| NodeState {
            name: name.to_string(),
            pandora_version: String::new(),
            encoder_identity: String::new(),
            ffmpeg_version: String::new(),
            threads: 4,
            max_jobs: 1,
            encoders: Vec::new(),
            purpose: NodePurpose::Cpu,
            build: 0,
            migration_error: None,
            registered_at: now,
            last_seen: now,
            drain: false,
            drain_reason: None,
            missed_pickups: 0,
            group: None,
            reserved_for: None,
        };
        let busy = HashMap::new();
        let pick = |nodes: &[NodeState], hardware, required, server, excluded: &[String]| {
            select_node(nodes, &busy, &settings, hardware, required, server, excluded, now)
        };

        assert_eq!(
            pick(&[], PresetHardware::Cpu, None, None, &[]).unwrap_err(),
            NoNode::NoNodesRegistered,
        );
        assert_eq!(
            pick(&[node("a")], PresetHardware::Cpu, None, None, &[]).unwrap(),
            "a",
        );

        let mut drained = node("a");
        drained.drain = true;
        assert_eq!(
            pick(&[drained], PresetHardware::Cpu, None, None, &[]).unwrap_err(),
            NoNode::AllDraining,
        );

        let mut stale = node("a");
        stale.last_seen = now - settings.lease_timeout_secs - 1;
        assert_eq!(
            pick(&[stale], PresetHardware::Cpu, None, None, &[]).unwrap_err(),
            NoNode::AllStale,
        );

        let mut reserved = node("a");
        reserved.reserved_for = Some(7);
        assert_eq!(
            pick(&[reserved], PresetHardware::Cpu, None, Some(8), &[]).unwrap_err(),
            NoNode::ReservedElsewhere,
        );

        // A CPU box is not a fallback for GPU work — it was marked `cpu` to keep that off it — and
        // the refusal has to say so rather than reading as a cluster that is merely busy.
        assert_eq!(
            pick(&[node("a")], PresetHardware::Gpu, None, None, &[]).unwrap_err(),
            NoNode::NoHardware("gpu"),
        );
        assert_eq!(
            pick(&[node("a")], PresetHardware::Gpu, Some("h264_nvenc"), None, &[]).unwrap_err(),
            NoNode::NoEncoder("h264_nvenc".to_string()),
        );

        // Excluded because it already declined this job. Re-offering it is a loop, and on an
        // orchestrator it is a loop with nothing else to fall back to.
        assert_eq!(
            pick(&[node("a")], PresetHardware::Cpu, None, None, &["a".to_string()]).unwrap_err(),
            NoNode::AllAlreadyTried,
        );

        // At its limit is the plain case, and it stays the least specific answer: it is the one an
        // operator can do nothing about except wait.
        let mut full = HashMap::new();
        full.insert("a".to_string(), 1u32);
        assert_eq!(
            select_node(&[node("a")], &full, &settings, PresetHardware::Cpu, None, None, &[], now)
                .unwrap_err(),
            NoNode::AllBusy,
        );

        // Every reason has to render as something an operator can read.
        for reason in [
            NoNode::Disabled,
            NoNode::NotLeasable,
            NoNode::AllBusy,
            NoNode::AllDraining,
            NoNode::AllStale,
            NoNode::OnlyNode("n".to_string()),
            NoNode::ReservedElsewhere,
            NoNode::NoHardware("gpu"),
            NoNode::NoEncoder("av1_nvenc".to_string()),
            NoNode::AllAlreadyTried,
        ] {
            assert!(!reason.describe().is_empty(), "{reason:?} says nothing");
        }
    }

    // A node's own answer about itself, arriving off the wire. One mistyped digit would have the
    // coordinator hand it every job in the queue and then wait for all of them.
    #[test]
    fn a_nodes_job_limit_is_clamped_rather_than_believed() {
        let request = |max_jobs: u32| NodeRegister {
            node: "limits".to_string(),
            pandora_version: "test".to_string(),
            encoder_identity: "x264-board-test".to_string(),
            ffmpeg_version: String::new(),
            threads: 1,
            max_jobs,
            encoders: Vec::new(),
            build: 0,
            migration_error: None,
        };
        let held = |name: &str| {
            roster()
                .into_iter()
                .find(|(node, _)| node.name == name)
                .map(|(node, _)| node.max_jobs)
                .unwrap()
        };
        register(request(0), "x264-board-test", NodePurpose::Cpu);
        assert_eq!(held("limits"), 1, "zero would mean a node that takes nothing");
        register(request(100_000), "x264-board-test", NodePurpose::Cpu);
        assert_eq!(held("limits"), MAX_JOBS_CEILING);
        remove_node("limits");
    }
}
