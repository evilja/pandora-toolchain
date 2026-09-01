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
    // The shared worker name this node reports under, set by `/teenode`. It is a display name and
    // nothing else: a group of interchangeable machines reads as one worker in the job embed and on
    // the console, while the roster, the leases, the purposes and the scheduler all keep addressing
    // each machine by its own name. Persisted for the same reason `drain` is — an operator who
    // grouped a farm did not mean "until the next deploy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
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
}

// `env.pandora` is re-read from disk on every call, so the loop that runs twenty times a second
// cannot ask it directly. Five seconds is short enough that an operator editing `link_only_node`
// sees it take effect while they are still watching, and long enough to cost nothing.
pub fn settings() -> LinkSettings {
    static CACHE: OnceLock<Mutex<Option<(Instant, LinkSettings)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((read_at, cached)) = guard.as_ref() {
            if read_at.elapsed() < Duration::from_secs(5) {
                return cached.clone();
            }
        }
    }
    let env = get_pandora_env();
    let flag = |key: &str| {
        matches!(
            env.get(key).map(|v| v.trim().to_ascii_lowercase()).as_deref(),
            Some("true") | Some("1") | Some("yes") | Some("on")
        )
    };
    let fresh = LinkSettings {
        enabled: flag(LINK_ENABLED),
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
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
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
        eprintln!("[link] node roster at {LINK_NODES_PATH} is unreadable; starting empty");
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
    let mut state = board().lock().unwrap();
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
            assets_revision: crate::pnworker::link::assets::manifest().revision,
            purpose,
            // A refused node is still told where the cluster is. Its refusal may well be that it
            // is running the wrong build, and the answer to that is on this line.
            release,
        };
    }
    let existing = state.nodes.get(&request.node);
    let drain = existing.map(|node| node.drain).unwrap_or(false);
    let group = existing.and_then(|node| node.group.clone());
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
        max_jobs: request.max_jobs.max(1),
        encoders: request.encoders,
        purpose,
        build: request.build,
        migration_error: request.migration_error,
        registered_at,
        last_seen: now(),
        drain,
        group,
    };
    state.nodes.insert(request.node, node);
    save_roster(&state);
    NodeRegistered {
        accepted: true,
        reason: None,
        renew_secs: DEFAULT_RENEW_SECS,
        lease_timeout_secs: settings.lease_timeout_secs,
        assets_revision: crate::pnworker::link::assets::manifest().revision,
        purpose,
        release,
    }
}

// What this machine is running, as a node is told it. The commit is read from the checkout rather
// than from the build record so that a coordinator whose repository moved underneath it — someone
// pulling by hand on the box — advertises where it actually is, not where it last thought it was.
pub fn local_release() -> ReleaseInfo {
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
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    if let Some(entry) = state.nodes.get_mut(node) {
        entry.last_seen = now();
    }
}

// The node a job should be offered to, or None to keep it local. Called from `pn_worker`, which is
// why it never blocks: a job that finds no free node runs here rather than waiting for one.
pub fn pick_node(preset: &str, pin: Option<&str>) -> Option<String> {
    let settings = settings();
    if !settings.enabled {
        return None;
    }
    let hardware = crate::lib::mpeg::preset::hardware_for(preset);
    let required_encoder = (hardware == crate::lib::mpeg::preset::PresetHardware::Gpu)
        .then(|| crate::lib::mpeg::preset::video_codec_for(preset))
        .flatten();
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    let stale_before = now().saturating_sub(settings.lease_timeout_secs);
    let mut busy: HashMap<&str, u32> = HashMap::new();
    for lease in state.leases.values() {
        *busy.entry(lease.node.as_str()).or_insert(0) += 1;
    }
    let mut candidates = state
        .nodes
        .values()
        .filter(|node| !node.drain)
        .filter(|node| node.last_seen >= stale_before)
        .filter(|node| match pin {
            Some(pinned) => node.name == pinned,
            None => settings
                .only_node
                .as_deref()
                .is_none_or(|only| node.name == only),
        })
        .filter(|node| advertises_encoder(&node.encoders, required_encoder.as_deref()))
        // A GPU preset on a CPU box does not fail cleanly: ffmpeg either refuses the encoder or
        // silently falls back to a software one, and the second outcome ships a release at a
        // quality tier nobody chose. The purpose comes off the node's token, so this is the
        // coordinator's own answer rather than the node's claim about itself.
        .filter(|node| node.purpose.accepts(hardware))
        .filter(|node| busy.get(node.name.as_str()).copied().unwrap_or(0) < node.max_jobs)
        .collect::<Vec<_>>();
    // Most idle first, then the most threads, so a cluster of unequal machines fills its biggest
    // free box before its smallest.
    candidates.sort_by(|a, b| {
        let a_busy = busy.get(a.name.as_str()).copied().unwrap_or(0);
        let b_busy = busy.get(b.name.as_str()).copied().unwrap_or(0);
        a_busy
            .cmp(&b_busy)
            .then(b.threads.cmp(&a.threads))
            .then(a.name.cmp(&b.name))
    });
    candidates.first().map(|node| node.name.clone())
}

fn advertises_encoder(encoders: &[String], required: Option<&str>) -> bool {
    required.is_none_or(|required| encoders.iter().any(|encoder| encoder == required))
}

pub fn offer(node: &str, spec: LinkJobSpec) {
    let job_id = spec.job_id.parse::<u64>().unwrap_or(0);
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
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
}

// A node collecting the job it was offered. Leases are targeted at a node before it polls, so this
// never has to choose anything: it hands over whatever is waiting under that node's name.
pub fn claim(node: &str) -> Option<LinkJobSpec> {
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    let job_id = state
        .leases
        .iter()
        .find(|(_, lease)| lease.node == node && lease.phase == LeasePhase::Offered)
        .map(|(job_id, _)| *job_id)?;
    let lease = state.leases.get_mut(&job_id)?;
    lease.phase = LeasePhase::Leased;
    lease.last_seen = now();
    Some(lease.spec.clone())
}

pub fn renew(job_id: u64, request: LeaseRenew) -> LeaseControl {
    // Read before the board is locked: building a manifest touches the filesystem, and the loop
    // publishing into this board must never wait behind a directory scan.
    let assets_revision = crate::pnworker::link::assets::manifest().revision;
    let mut state = board().lock().unwrap();
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
    let mut state = board().lock().unwrap();
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
    let mut state = board().lock().unwrap();
    match state.leases.get_mut(&job_id) {
        Some(lease) => {
            lease.cancel = true;
            true
        }
        None => false,
    }
}

pub fn release(job_id: u64) {
    let mut state = board().lock().unwrap();
    state.leases.remove(&job_id);
}

pub fn drain_events() -> Vec<LinkEvent> {
    let mut state = board().lock().unwrap();
    std::mem::take(&mut state.events)
}

// Leases whose node has gone quiet. An offered lease is given a shorter rope than a running one:
// nothing has started, so re-offering it costs nothing but the round trip.
pub fn expire_leases(timeout_secs: u64) -> Vec<(u64, String)> {
    let now = now();
    let mut state = board().lock().unwrap();
    let lost = state
        .leases
        .iter()
        .filter(|(_, lease)| match lease.phase {
            LeasePhase::Offered => now.saturating_sub(lease.offered_at) > OFFER_PICKUP_SECS,
            LeasePhase::Leased => now.saturating_sub(lease.last_seen) > timeout_secs,
        })
        .map(|(job_id, lease)| (*job_id, lease.node.clone()))
        .collect::<Vec<_>>();
    for (job_id, _) in &lost {
        state.leases.remove(job_id);
    }
    lost
}

pub fn set_drain(node: &str, drain: bool) -> bool {
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    let Some(entry) = state.nodes.get_mut(node) else {
        return false;
    };
    entry.drain = drain;
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
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    let Some(entry) = state.nodes.get_mut(node) else {
        return Err(format!("No node `{}` has registered.", node));
    };
    entry.group = group.clone();
    save_roster(&state);
    Ok(group)
}

// The name a node's work reports under: its group when it has one, itself otherwise. Every worker
// label goes through here, so grouping is one lookup rather than a rule each call site remembers.
pub fn display_name(node: &str) -> String {
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    state
        .nodes
        .get(node)
        .and_then(|entry| entry.group.clone())
        .unwrap_or_else(|| node.to_string())
}

pub fn remove_node(node: &str) -> bool {
    let mut state = board().lock().unwrap();
    ensure_loaded(&mut state);
    let removed = state.nodes.remove(node).is_some();
    if removed {
        save_roster(&state);
    }
    removed
}

pub fn node_for_job(job_id: u64) -> Option<String> {
    let state = board().lock().unwrap();
    state.leases.get(&job_id).map(|lease| lease.node.clone())
}

// The registered nodes and the jobs each currently holds, name-ordered. Shared by `/lsnode` and the
// worker snapshot so the two can never disagree about who is in the cluster.
pub fn roster() -> Vec<(NodeState, Vec<u64>)> {
    let mut state = board().lock().unwrap();
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
            expires_at: 0,
            renew_secs: DEFAULT_RENEW_SECS,
        }
    }

    // The board is one process-wide structure, exactly as it is in production, so each test uses
    // node names of its own rather than racing its neighbours for the same roster entry.

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
        offer("claim-a", spec(9001));
        assert!(claim("claim-b").is_none());
        let claimed = claim("claim-a").expect("the offered node could not claim its own job");
        assert_eq!(claimed.job_id, "9001");
        assert!(claim("claim-a").is_none());
        release(9001);
    }

    // A node that comes back after the coordinator gave its job away has to be told to stop, or it
    // finishes work a second machine is already doing.
    #[test]
    fn renewing_a_reclaimed_lease_is_told_to_abandon() {
        release(9002);
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
    }

    // A renew from the wrong node must not keep somebody else's lease alive.
    #[test]
    fn a_renew_from_another_node_does_not_hold_the_lease() {
        release(9003);
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

    #[test]
    fn an_offer_nobody_collects_expires_and_is_reported_lost() {
        release(9004);
        offer("expiry-a", spec(9004));
        {
            let mut state = board().lock().unwrap();
            if let Some(lease) = state.leases.get_mut(&9004) {
                lease.offered_at = now().saturating_sub(OFFER_PICKUP_SECS + 1);
            }
        }
        let lost = expire_leases(DEFAULT_LEASE_TIMEOUT_SECS);
        assert!(lost.iter().any(|(job_id, node)| *job_id == 9004 && node == "expiry-a"));
        assert!(claim("expiry-a").is_none());
    }
}
