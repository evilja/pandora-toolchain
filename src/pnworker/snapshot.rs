use std::sync::{OnceLock, RwLock};
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
    let snapshot = json!({
        "updated_at": now,
        "queue_len": queue.len(),
        "gitquery_pending": gitquery_pending,
        "encode_reboot_count": shrine.reboot_epoch(&Worker::Encode),
        "hearts": hearts,
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

fn job_view(job: &Job, now: u64) -> Value {
    json!({
        "job_id": job.job_id.to_string(),
        "job_type": job_type_label(job.job_type as u16 as i64),
        "stage": stage_label(stage_to_int(job.ready)),
        "worker": job.worker,
        "server_id": job.server_id.map(|id| id.to_string()),
        "forward_parent": job.forward_parent.map(|id| id.to_string()),
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
