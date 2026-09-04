use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::lib::db::core::{job_type_label, stage_label, stage_to_int};
use crate::pnworker::core::{Job, WorkerMsg};
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};

// The worker's live state never reaches the database: the queue is a `Vec<Job>` owned by
// `pn_worker` and the shrine's heartbeats are in-memory only, so a stall is invisible to anything
// reading the jobs table — which is exactly the state you need when the bot looks frozen. The loop
// publishes a snapshot of both here and `GET /api/v1/workers` reads it, one writer and any number
// of readers, with no channel to block the loop if nobody is listening.
static SNAPSHOT: OnceLock<RwLock<Option<Value>>> = OnceLock::new();
const EVENT_CAPACITY: usize = 100;

#[derive(Default)]
struct EventState {
    initialized: bool,
    events: VecDeque<Value>,
    workers: HashMap<String, bool>,
    worker_reboots: HashMap<String, u64>,
    nodes: HashMap<String, bool>,
    migration_errors: HashMap<String, Option<String>>,
}

static EVENT_STATE: OnceLock<Mutex<EventState>> = OnceLock::new();

fn cell() -> &'static RwLock<Option<Value>> {
    SNAPSHOT.get_or_init(|| RwLock::new(None))
}

pub fn publish(shrine: &TypedShrine<WorkerMsg>, queue: &[Job], gitquery_pending: bool) {
    let now = unix_secs();
    let hearts = shrine
        .hearts()
        .into_iter()
        .map(|status| {
            json!({
                "worker": format!("{:?}", status.worker),
                "alive": status.alive,
                "last_beat_secs": status.last_beat_secs,
                "reboot_count": status.reboot_count,
            })
        })
        .collect::<Vec<_>>();
    let jobs = queue.iter().map(|job| job_view(job, now)).collect::<Vec<_>>();
    let nodes = crate::pnworker::link::board::nodes_view();
    record_snapshot_events(&hearts, &nodes, now);
    let snapshot = json!({
        "updated_at": now,
        "queue_len": queue.len(),
        "gitquery_pending": gitquery_pending,
        "encode_reboot_count": shrine.reboot_epoch(&Worker::Encode),
        "hearts": hearts,
        // The cluster is nowhere in the jobs table either, for the same reason the queue is not:
        // a node's liveness is in-memory state that a restart forgets.
        "nodes": nodes,
        // Machines that have a boot profile bound to them, registered or not. They are a separate
        // array rather than fields on `nodes` because the interesting one has never registered:
        // a node that was rented and never arrived exists in no roster.
        "boot": crate::pnworker::boot::manager::boot_view(),
        "queue": jobs,
    });
    if let Ok(mut guard) = cell().write() {
        *guard = Some(snapshot);
    }
}

// None until the worker loop publishes for the first time, which also distinguishes "the worker
// never started" from "the worker started and is idle".
pub fn current() -> Option<Value> {
    cell().read().ok().and_then(|guard| guard.clone())
}

// The public capacity view deliberately contains only aggregate counts. The full snapshot keeps
// node names, builds, encoder identities and queue rows behind the PNwitch gate.
pub fn summary() -> Option<Value> {
    let snapshot = current()?;
    let hearts = snapshot.get("hearts")?.as_array()?;
    let nodes = snapshot.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let workers_online = hearts.iter()
        .filter(|heart| heart.get("alive").and_then(Value::as_bool).unwrap_or(false))
        .count();
    let nodes_online = nodes.iter()
        .filter(|node| node.get("last_seen_secs").and_then(Value::as_u64).unwrap_or(u64::MAX) < 60)
        .count();
    let mut capacity: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for node in &nodes {
        let purpose = node.get("purpose").and_then(Value::as_str).unwrap_or("cpu").to_string();
        let entry = capacity.entry(purpose).or_default();
        entry.1 += 1;
        if node.get("last_seen_secs").and_then(Value::as_u64).unwrap_or(u64::MAX) < 60 {
            entry.0 += 1;
        }
    }
    Some(json!({
        "workers_online": workers_online,
        "workers_total": hearts.len(),
        "nodes_online": nodes_online,
        "nodes_total": nodes.len(),
        "queue_len": snapshot.get("queue_len").and_then(Value::as_u64).unwrap_or(0),
        "capacity": capacity.into_iter().map(|(purpose, (online, total))| json!({
            "purpose": purpose, "online": online, "total": total,
        })).collect::<Vec<_>>(),
    }))
}

pub fn recent_events(limit: usize) -> Vec<Value> {
    let limit = limit.clamp(1, EVENT_CAPACITY);
    event_state().lock().map(|state| {
        state.events.iter().rev().take(limit).cloned().collect()
    }).unwrap_or_default()
}

pub fn record_job_event(job_id: u64, stage: crate::pnworker::core::Stage) {
    use crate::pnworker::core::Stage;
    let (kind, severity, text) = match stage {
        Stage::Uploaded => ("job_completed", "info", "Job finished"),
        Stage::Probed => ("job_completed", "info", "Probe finished"),
        Stage::Cancelled => ("job_cancelled", "warn", "Job cancelled"),
        Stage::Declined => ("job_declined", "warn", "Job declined"),
        Stage::Failed => ("job_failed", "error", "Job failed"),
        _ => return,
    };
    push_event(json!({
        "at": unix_secs(), "kind": kind, "severity": severity,
        "job_id": job_id.to_string(), "text": text,
    }));
}

fn event_state() -> &'static Mutex<EventState> {
    EVENT_STATE.get_or_init(|| Mutex::new(EventState::default()))
}

fn push_event(event: Value) {
    if let Ok(mut state) = event_state().lock() {
        push_event_locked(&mut state, event);
    }
}

fn push_event_locked(state: &mut EventState, event: Value) {
    if state.events.len() == EVENT_CAPACITY {
        state.events.pop_front();
    }
    state.events.push_back(event);
}

fn record_snapshot_events(hearts: &[Value], nodes: &Value, now: u64) {
    let Ok(mut state) = event_state().lock() else { return };
    if !state.initialized {
        push_event_locked(&mut state, json!({
            "at": now, "kind": "worker_started", "severity": "info",
            "text": "Worker loop started",
        }));
    }
    for heart in hearts {
        let Some(worker) = heart.get("worker").and_then(Value::as_str) else { continue };
        let alive = heart.get("alive").and_then(Value::as_bool).unwrap_or(false);
        if state.initialized && state.workers.get(worker).copied() != Some(alive) {
            push_event_locked(&mut state, json!({
                "at": now,
                "kind": if alive { "worker_online" } else { "worker_offline" },
                "severity": if alive { "info" } else { "warn" },
                "text": format!("{} worker {}", worker, if alive { "came online" } else { "went offline" }),
            }));
        }
        state.workers.insert(worker.to_string(), alive);
        let reboots = heart.get("reboot_count").and_then(Value::as_u64).unwrap_or(0);
        if state.initialized && state.worker_reboots.get(worker).copied().unwrap_or(0) < reboots {
            push_event_locked(&mut state, json!({
                "at": now, "kind": "worker_restarted", "severity": "warn",
                "text": format!("{} worker restarted", worker),
            }));
        }
        state.worker_reboots.insert(worker.to_string(), reboots);
    }
    for node in nodes.as_array().into_iter().flatten() {
        let Some(name) = node.get("node").and_then(Value::as_str) else { continue };
        let online = node.get("last_seen_secs").and_then(Value::as_u64).unwrap_or(u64::MAX) < 60;
        if state.initialized && state.nodes.get(name).copied() != Some(online) {
            push_event_locked(&mut state, json!({
                "at": now,
                "kind": if online { "node_online" } else { "node_stale" },
                "severity": if online { "info" } else { "warn" },
                "text": if online { format!("{} came online", name) } else { format!("{} stopped checking in", name) },
            }));
        }
        state.nodes.insert(name.to_string(), online);
        let error = node.get("migration_error").and_then(Value::as_str).map(str::to_string);
        if error.is_some() && state.migration_errors.get(name) != Some(&error) {
            push_event_locked(&mut state, json!({
                "at": now, "kind": "node_migration_error", "severity": "error",
                "text": format!("{} reported a migration error", name),
            }));
        }
        state.migration_errors.insert(name.to_string(), error);
    }
    state.initialized = true;
}

fn job_view(job: &Job, now: u64) -> Value {
    json!({
        "job_id": job.job_id.to_string(),
        "job_type": job_type_label(job.job_type as u16 as i64),
        "stage": stage_label(stage_to_int(job.ready)),
        "worker": job.worker,
        "server_id": job.server_id.map(|id| id.to_string()),
        "forward_parent": job.forward_parent.map(|id| id.to_string()),
        "link_node": job.link_node,
        "link_attempts": job.link_attempts,
        // A job an orchestrator is holding for a node. It is `Queued` with a worker nothing is
        // running, which on any other deployment would mean it is simply next in line — so the
        // console has to be able to tell the two apart, and to say what the cluster answered.
        "link_waiting": job.link_waiting,
        "link_wait_reason": job.link_wait_reason,
        "batch_parent": job.batch_parent.map(|id| id.to_string()),
        "waiting_on_cache": job.duplicate_source.is_some(),
        "encode_dispatched": job.encode_dispatched,
        "encode_dispatch_order": job.encode_dispatch_order,
        "encode_dispatch_epoch": job.encode_dispatch_epoch,
        "secs_since_dispatch": since(job.encode_dispatched_at, now),
        "secs_since_frame": since(job.encode_last_frame_at, now),
        "secs_since_request": since(Some(job.requested_at), now),
        "encode_frame": job.encode_frame,
        "encode_total": job.encode_total,
        "encode_fps": job.encode_fps,
    })
}

fn since(then: Option<Duration>, now: u64) -> Option<u64> {
    then.map(|then| now.saturating_sub(then.as_secs()))
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
