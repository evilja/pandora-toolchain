use std::env;
use std::path::PathBuf;
use std::time::SystemTime;

use tokio::fs::{create_dir_all, remove_dir_all, write};

use crate::libpndb::core::JobDb;
use crate::pnworker::core::{Job, Stage};
use crate::pnworker::lifecycle::render;
use crate::pnworker::messages::{MessagePayload, TORRENT_DUPLICATE_WAIT};

const INPUT_CACHE_TTL_SECS: u64 = 30 * 60;

pub(crate) fn past_downloaded(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Downloaded | Stage::Encoding | Stage::Encoded | Stage::Uploading | Stage::Uploaded
    )
}

pub(crate) fn input_cache_key(job: &Job) -> String {
    format!(
        "{:x}",
        md5::compute(format!("{}|{:?}", job.torrent.get(), job.probe_file_index))
    )
}

fn input_cache_dir(key: &str) -> PathBuf {
    PathBuf::from("DB").join("cache").join("inputs").join(key)
}

fn input_cache_input(key: &str) -> PathBuf {
    input_cache_dir(key).join("input.mkv")
}

fn input_cache_touch(key: &str) -> PathBuf {
    input_cache_dir(key).join("touch")
}

pub(crate) async fn cleanup_input_cache_startup() {
    remove_dir_all(PathBuf::from("DB").join("cache").join("inputs"))
        .await
        .ok();
}

pub(crate) async fn cleanup_expired_input_cache() {
    let root = PathBuf::from("DB").join("cache").join("inputs");
    let mut entries = match tokio::fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let touch = entry.path().join("touch");
        let expired = tokio::fs::metadata(&touch)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d.as_secs() > INPUT_CACHE_TTL_SECS)
            .unwrap_or(true);
        if expired {
            remove_dir_all(entry.path()).await.ok();
        }
    }
}

async fn touch_input_cache(key: &str) {
    let dir = input_cache_dir(key);
    create_dir_all(&dir).await.ok();
    write(input_cache_touch(key), b"touch\n").await.ok();
}

pub(crate) async fn cache_encode_input(job: &Job) {
    let source = job
        .directory
        .join("contents")
        .join("torrent")
        .join("input.mkv");
    if !source.exists() {
        return;
    }
    let key = input_cache_key(job);
    let dir = input_cache_dir(&key);
    create_dir_all(&dir).await.ok();
    if tokio::fs::copy(&source, input_cache_input(&key))
        .await
        .is_ok()
    {
        touch_input_cache(&key).await;
    }
}

pub(crate) async fn use_cached_input(job: &mut Job) -> bool {
    let key = input_cache_key(job);
    let source = input_cache_input(&key);
    if !source.exists() {
        return false;
    }
    let target_dir = job.directory.join("contents").join("torrent");
    let target = target_dir.join("input.mkv");
    create_dir_all(&target_dir).await.unwrap();
    match tokio::fs::copy(&source, &target).await {
        Ok(_) => {
            touch_input_cache(&key).await;
            job.ready = Stage::Downloaded;
            job.worker = "dwl-cache".to_string();
            true
        }
        Err(e) => {
            eprintln!(
                "[Pandora] input cache copy failed for {}: {}",
                job.job_id, e
            );
            false
        }
    }
}

fn queued_duplicate_source(job: &Job, queue: &[Job]) -> Option<PathBuf> {
    let key = input_cache_key(job);
    queue
        .iter()
        .find(|other| {
            other.forward_parent.is_none()
                && other.job_id != job.job_id
                && input_cache_key(other) == key
        })
        .map(|other| {
            other
                .duplicate_source
                .clone()
                .unwrap_or_else(|| other.directory.join("contents").join("torrent"))
        })
}

pub(crate) async fn use_cache_or_wait(db: &JobDb, job: &mut Job, queue: &[Job]) -> bool {
    if use_cached_input(job).await {
        let v = serde_json::json!({ "type": "download", "percent": 100, "cached": true });
        db.update_progress(job.job_id, &v.to_string()).await.ok();
        db.update_worker(job.job_id, &job.worker).await.ok();
        render(
            job,
            MessagePayload::Static(crate::pnworker::messages::TORRENT_DONE),
        )
        .await;
        return true;
    }
    if let Some(source) = queued_duplicate_source(job, queue) {
        job.duplicate_source = Some(source.clone());
        job.ready = Stage::Downloading;
        job.worker = "dwl-cache".to_string();
        let v = serde_json::json!({ "type": "download", "waiting": "cache" });
        db.update_progress(job.job_id, &v.to_string()).await.ok();
        db.update_worker(job.job_id, &job.worker).await.ok();
        render(
            job,
            MessagePayload::Progress(TORRENT_DUPLICATE_WAIT, vec![source.display().to_string()]),
        )
        .await;
        return true;
    }
    false
}

pub(crate) fn duplicate_path_to_container(raw: &str) -> PathBuf {
    let mut path = raw.replace('\\', "/");
    if let Ok(host_prefix) = env::var("PNP2P_QBIT_SAVE_HOST") {
        let host_prefix = host_prefix
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string();
        if !host_prefix.is_empty() && path.starts_with(&host_prefix) {
            let container_prefix =
                env::var("PNP2P_QBIT_SAVE_CONTAINER").unwrap_or_else(|_| "/app/DB".to_string());
            path = format!(
                "{}{}",
                container_prefix.trim_end_matches('/'),
                &path[host_prefix.len()..]
            );
        }
    }
    PathBuf::from(path)
}

pub(crate) fn duplicate_input_path(source: &PathBuf) -> PathBuf {
    if source.file_name().and_then(|n| n.to_str()) == Some("input.mkv") {
        source.clone()
    } else {
        source.join("input.mkv")
    }
}

pub(crate) fn duplicate_source_ready(queue: &[Job], source: &PathBuf) -> bool {
    let input = duplicate_input_path(source);
    if !input.exists() {
        return false;
    }
    for owner in queue {
        let owner_torrent_dir = owner.directory.join("contents").join("torrent");
        if source.starts_with(&owner_torrent_dir) || input.starts_with(&owner_torrent_dir) {
            return matches!(
                owner.ready,
                Stage::Encoded
                    | Stage::Uploading
                    | Stage::Uploaded
                    | Stage::Failed
                    | Stage::Cancelled
            );
        }
    }
    true
}
