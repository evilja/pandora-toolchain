use std::env;
use std::path::PathBuf;
use std::time::SystemTime;

use tokio::fs::{create_dir_all, remove_dir_all, write};

use crate::libpndb::core::JobDb;
use crate::libpnp2p::core::{magnet_info_hash, torrent_info_hash};
use crate::libpnp2p::nyaaise::TorrentType;
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

pub(crate) fn input_source_keys(job: &Job) -> Vec<String> {
    let mut keys = Vec::new();
    match &job.torrent {
        TorrentType::GDrive(link) => keys.push(format!("gdrive:{}", link)),
        TorrentType::Magnet(magnet) => {
            if let Some(hash) = magnet_info_hash(magnet) {
                keys.push(format!("torrent:{}", hash));
            }
            keys.push(format!("magnet:{:x}", md5::compute(magnet.as_bytes())));
        }
        TorrentType::Link(link) => {
            let fetch = job.directory.join("contents").join("fetch.torrent");
            if let Ok(data) = std::fs::read(fetch) {
                if let Some(hash) = torrent_info_hash(&data) {
                    keys.push(format!("torrent:{}", hash));
                }
            }
            if !link.trim().is_empty() {
                keys.push(format!("link:{}", link));
            }
        }
    }
    if keys.is_empty() {
        keys.push(format!("job:{}", job.job_id));
    }
    keys.dedup();
    keys
}

pub(crate) fn input_cache_keys(job: &Job) -> Vec<String> {
    input_source_keys(job)
        .into_iter()
        .map(|source| input_cache_key_for(&source, job.probe_file_index))
        .collect()
}

fn input_cache_key_for(source: &str, file_index: Option<u64>) -> String {
    format!("{:x}", md5::compute(format!("{}|{:?}", source, file_index)))
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
    for key in input_cache_keys(job) {
        let dir = input_cache_dir(&key);
        create_dir_all(&dir).await.ok();
        if tokio::fs::copy(&source, input_cache_input(&key))
            .await
            .is_ok()
        {
            touch_input_cache(&key).await;
        }
    }
}

pub(crate) async fn use_cached_input(job: &mut Job) -> bool {
    let Some((key, source)) = input_cache_keys(job)
        .into_iter()
        .map(|key| {
            let source = input_cache_input(&key);
            (key, source)
        })
        .find(|(_, source)| source.exists())
    else {
        return false;
    };
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
    let keys = input_cache_keys(job);
    queue
        .iter()
        .find(|other| {
            other.forward_parent.is_none()
                && other.job_id != job.job_id
                && input_cache_keys(other)
                    .iter()
                    .any(|key| keys.iter().any(|candidate| candidate == key))
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
    if let Some(owner) = duplicate_source_owner(queue, source) {
        return matches!(
            owner.ready,
            Stage::Encoded | Stage::Uploading | Stage::Uploaded | Stage::Failed | Stage::Cancelled
        );
    }
    true
}

pub(crate) fn duplicate_source_owner<'a>(queue: &'a [Job], source: &PathBuf) -> Option<&'a Job> {
    let input = duplicate_input_path(source);
    queue.iter().find(|owner| {
        let owner_torrent_dir = owner.directory.join("contents").join("torrent");
        source.starts_with(&owner_torrent_dir) || input.starts_with(&owner_torrent_dir)
    })
}

pub(crate) fn jobs_share_input(job: &Job, other: &Job) -> bool {
    let keys = input_cache_keys(job);
    input_cache_keys(other)
        .iter()
        .any(|key| keys.iter().any(|candidate| candidate == key))
}

pub(crate) fn jobs_share_source(job: &Job, other: &Job) -> bool {
    let keys = input_source_keys(job);
    input_source_keys(other)
        .iter()
        .any(|key| keys.iter().any(|candidate| candidate == key))
}
