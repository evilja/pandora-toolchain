use std::path::PathBuf;

use crate::lib::db::core::JobDb;
use crate::lib::p2p::core::{magnet_info_hash, torrent_info_hash};
use crate::lib::p2p::nyaaise::TorrentType;
use crate::pnworker::core::{Job, JobType, Preset, Stage};
use crate::pnworker::lifecycle::{cleanup_job, render};
use crate::pnworker::messages::{
    CTORRENT_DONE, MessagePayload, TORRENT_DONE, TORRENT_PROG, TORRENT_PROG_SELECT,
};
use crate::pnworker::progress::persist_side_effects;

pub(crate) fn is_forwardable_encode(job: &Job) -> bool {
    job.job_type == JobType::Encode && job.keep.is_none()
}

pub(crate) fn queued_encode_parent(job: &Job, queue: &[Job]) -> Option<(u64, Stage, String)> {
    if !is_forwardable_encode(job) {
        return None;
    }
    let keys = encode_forward_keys(job);
    queue
        .iter()
        .filter(|other| other.job_id != job.job_id)
        .filter(|other| other.forward_parent.is_none())
        .filter(|other| is_forwardable_encode(other))
        .filter(|other| !is_terminal_stage(other.ready))
        .find(|other| {
            encode_forward_keys(other)
                .iter()
                .any(|key| keys.iter().any(|candidate| candidate == key))
        })
        .map(|other| (other.job_id, other.ready, forwarded_worker_for(&other.worker)))
}

pub(crate) fn mark_forwarded(job: &mut Job, parent_id: u64, parent_stage: Stage, worker: &str) {
    job.forward_parent = Some(parent_id);
    job.duplicate_source = None;
    job.ready = parent_stage;
    job.worker = worker.to_string();
}

pub(crate) fn forwarded_worker_for(parent_worker: &str) -> String {
    if parent_worker.starts_with("dwl-") {
        "dwl-forward".to_string()
    } else if parent_worker.starts_with("upl-") {
        "upl-forward".to_string()
    } else {
        "enc-forward".to_string()
    }
}

pub(crate) async fn persist_forwarded_wait(db: &JobDb, job: &Job) {
    db.update_stage(job.job_id, job.ready).await.ok();
    db.update_worker(job.job_id, &job.worker).await.ok();
    let v = serde_json::json!({
        "type": "forward",
        "parent_job_id": job.forward_parent.map(|id| id.to_string()),
    });
    db.update_progress(job.job_id, &v.to_string()).await.ok();
}

pub(crate) async fn sync_forwarded_jobs(
    db: &JobDb,
    queue: &mut Vec<Job>,
    parent_id: u64,
    stage: Option<Stage>,
    payload: &MessagePayload,
    parent_worker: &str,
) {
    let ids: Vec<u64> = queue
        .iter()
        .filter(|job| job.forward_parent == Some(parent_id))
        .map(|job| job.job_id)
        .collect();

    for id in ids {
        if let Some(pos) = queue.iter().position(|job| job.job_id == id) {
            let worker = forwarded_worker_for(parent_worker);
            if let Some(stage) = stage {
                let previous_ready = queue[pos].ready;
                queue[pos].ready = stage;
                db.update_stage(queue[pos].job_id, stage).await.ok();
                if stage == Stage::Uploaded && previous_ready != Stage::Uploaded {
                    queue[pos].frontend.ghost_ping(queue[pos].author).await;
                }
            }
            queue[pos].worker = worker.clone();
            db.update_worker(queue[pos].job_id, &worker).await.ok();
            if !is_forwarded_download_payload(payload) {
                persist_side_effects(db, queue[pos].job_id, payload, stage, &queue[pos].encode_warnings).await;
                render(&mut queue[pos], payload.clone()).await;
            }
            if stage.map(is_terminal_stage).unwrap_or(false) {
                let directory = queue[pos].directory.clone();
                db.archive_job(id).await.ok();
                cleanup_job(
                    &directory,
                    &PathBuf::from("DB")
                        .join("saved_data")
                        .join(id.to_string()),
                )
                .await;
                queue.remove(pos);
            }
        }
    }
}

fn is_forwarded_download_payload(payload: &MessagePayload) -> bool {
    match payload {
        MessagePayload::Progress(id, _) => *id == TORRENT_PROG || *id == TORRENT_PROG_SELECT,
        MessagePayload::Static(id) => *id == CTORRENT_DONE || *id == TORRENT_DONE,
    }
}

pub(crate) async fn sync_forwarded_state(
    db: &JobDb,
    queue: &mut Vec<Job>,
    parent_id: u64,
    stage: Option<Stage>,
    worker: Option<&str>,
) {
    let ids: Vec<u64> = queue
        .iter()
        .filter(|job| job.forward_parent == Some(parent_id))
        .map(|job| job.job_id)
        .collect();

    for id in ids {
        if let Some(pos) = queue.iter().position(|job| job.job_id == id) {
            if let Some(stage) = stage {
                queue[pos].ready = stage;
                db.update_stage(queue[pos].job_id, stage).await.ok();
            }
            if let Some(worker) = worker {
                queue[pos].worker = worker.to_string();
                db.update_worker(queue[pos].job_id, worker).await.ok();
            }
        }
    }
}

fn is_terminal_stage(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Uploaded | Stage::Failed | Stage::Declined | Stage::Cancelled
    )
}

pub(crate) fn encode_forward_keys(job: &Job) -> Vec<String> {
    encode_source_keys(job)
        .into_iter()
        .map(|source_key| encode_forward_key(job, source_key))
        .collect()
}

fn encode_forward_key(job: &Job, source_key: String) -> String {
    let payload = serde_json::json!([
        "v1",
        source_key,
        job.probe_file_index,
        preset_forward_key(&job.preset),
        format!("{:x}", md5::compute(&job.attachment)),
        job.server_id,
        job.gdrive_folder_global.as_deref(),
        job.gdrive_folder_local.as_deref(),
    ]);
    format!("{:x}", md5::compute(payload.to_string()))
}

fn preset_forward_key(preset: &Preset) -> serde_json::Value {
    match preset {
        Preset::PseudoLossless(candidates) => serde_json::json!(["pseudolossless", candidates]),
        Preset::Dummy(candidates) => serde_json::json!(["dummy", candidates]),
        Preset::Standard(candidates) => serde_json::json!(["standard", candidates]),
        Preset::Gpu(candidates) => serde_json::json!(["gpu", candidates]),
    }
}

fn encode_source_keys(job: &Job) -> Vec<String> {
    match &job.torrent {
        TorrentType::GDrive(link) => vec![format!("gdrive:{}", link)],
        TorrentType::Magnet(magnet) => magnet_info_hash(magnet)
            .map(|hash| vec![format!("torrent:{}", hash)])
            .unwrap_or_else(|| vec![format!("magnet:{:x}", md5::compute(magnet.as_bytes()))]),
        TorrentType::Link(link) => {
            let mut keys = vec![format!("link:{}", link)];
            let fetch = job.directory.join("contents").join("fetch.torrent");
            if let Ok(data) = std::fs::read(fetch) {
                if let Some(hash) = torrent_info_hash(&data) {
                    keys.push(format!("torrent:{}", hash));
                }
            }
            keys
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnworker::frontend::Frontend;
    use std::path::PathBuf;
    use std::time::Duration;

    fn encode_job(local_folder: Option<&str>) -> Job {
        Job {
            author: 1,
            channel_id: 1,
            response_id: 1,
            requested_at: Duration::from_secs(1),
            job_type: JobType::Encode,
            job_id: 1,
            preset: Preset::Standard(None),
            torrent: TorrentType::Magnet("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_string()),
            display_link: None,
            attachment: b"ass".to_vec(),
            frontend: Frontend::None,
            directory: PathBuf::from("DB/work/1"),
            ready: Stage::Queued,
            probe_files: None,
            probe_torrent_path: None,
            probe_job_id: None,
            probe_file_index: None,
            lang: "EN".to_string(),
            server_id: Some(1),
            acix: None,
            gdrive_folder_global: None,
            gdrive_folder_local: local_folder.map(|s| s.to_string()),
            smartcode_drive_name: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
            encode_dispatched: false,
            encode_frame: None,
            encode_total: None,
            encode_fps: None,
            keep: None,
            keycode: None,
        }
    }

    #[test]
    fn smartcode_and_anonymous_jobs_do_not_share_forward_key() {
        let smartcode = encode_forward_keys(&encode_job(Some("pntools/anime")));
        let anonymous = encode_forward_keys(&encode_job(None));
        assert_ne!(smartcode, anonymous);
    }
}
