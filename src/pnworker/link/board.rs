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
    LeaseControl, LeaseRenew, LeaseResult, LinkJobSpec, LinkOutcome, LinkReport, NodeRegister,
    NodeRegistered,
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
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub registered_at: u64,
    #[serde(default)]
    pub last_seen: u64,
    // Survives a coordinator restart on purpose: an operator who drained a node before a deploy
    // did not mean "until the next deploy".
    #[serde(default)]
    pub drain: bool,
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
pub fn register(request: NodeRegister, coordinator_identity: &str) -> NodeRegistered {
    let settings = settings();
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
        };
    }
    let existing = state.nodes.get(&request.node);
    let drain = existing.map(|node| node.drain).unwrap_or(false);
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
        presets: request.presets,
        registered_at,
        last_seen: now(),
        drain,
    };
    state.nodes.insert(request.node, node);
    save_roster(&state);
    NodeRegistered {
        accepted: true,
        reason: None,
        renew_secs: DEFAULT_RENEW_SECS,
        lease_timeout_secs: settings.lease_timeout_secs,
        assets_revision: crate::pnworker::link::assets::manifest().revision,
    }
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
        .filter(|node| node.presets.is_empty() || node.presets.iter().any(|value| value == preset))
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
                "threads": node.threads,
                "max_jobs": node.max_jobs,
                "presets": node.presets,
                "drain": node.drain,
                "last_seen_secs": seconds_since_seen(&node),
                "jobs": jobs.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "pandora_version": node.pandora_version,
                "encoder_identity": node.encoder_identity,
            }))
            .collect::<Vec<_>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(job_id: u64) -> LinkJobSpec {
        LinkJobSpec {
            job_id: job_id.to_string(),
            job_type: "Encode".to_string(),
            source_kind: "magnet".to_string(),
            source: "magnet:?xt=urn:btih:abc".to_string(),
            display_link: None,
            file_index: None,
            probe_job_id: None,
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
            presets: Vec::new(),
        };
        let answer = register(request, "x264-165-0.165.x-pandora");
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
            presets: Vec::new(),
        };
        let answer = register(request, "x264-165-0.165.x-pandora");
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
