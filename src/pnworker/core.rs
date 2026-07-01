use crate::libpndb::core::JobDb;
use crate::libpnp2p::core::cleanup_pandora_qbit;
use crate::libpnp2p::nyaaise::TorrentType;
use crate::pnworker::cache::{
    cache_encode_input, cleanup_expired_input_cache, cleanup_input_cache_startup,
    duplicate_input_path, duplicate_path_to_container, duplicate_source_ready, input_cache_key,
    past_downloaded, use_cache_or_wait, use_cached_input,
};
use crate::pnworker::frontend::Frontend;
use crate::pnworker::forwarding::{
    encode_forward_keys, forwarded_worker_for, is_forwardable_encode, mark_forwarded,
    persist_forwarded_wait, queued_encode_parent, sync_forwarded_jobs, sync_forwarded_state,
};
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};
use crate::pnworker::lifecycle::{cleanup_job, render};
use crate::pnworker::messages::{
    ENCODE_WARNING, MessagePayload, PROBE_TIMEOUT, QUEUE_TOO_LONG, QUEUED, TORRENT_DUPLICATE_WAIT,
    WORKER_ASSIGN,
};
use crate::pnworker::presence::{Presence, presence_from_queue};
use crate::pnworker::progress::{drive_link_from_payload, persist_side_effects};
use crate::pnworker::pull::git_pull;
use crate::pnworker::workers::downloadworker::*;
use crate::pnworker::workers::encodeworker::*;
use crate::pnworker::workers::probeworker::*;
use crate::pnworker::workers::uploadworker::*;
use serenity::all::{Context, Message};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{File, create_dir_all, remove_dir_all, write};
use tokio::sync::mpsc::Receiver;
use tokio::time::Duration;
use tokio::time::sleep;

pub type CommData = (u64, MessagePayload, Option<Stage>);

#[derive(Clone)]
pub enum WorkerMsg {
    Download(DownloadData),
    Probe(ProbeData),
    Encode(EncodeData),
    Upload(UploadData),
    UploadAll(UploadAllData),
}

pub const STRUCT: [&str; 3] = ["contents", "work", "log"];

pub async fn pn_worker(mut rx: Receiver<JobClass>) {
    let db = JobDb::new().await.unwrap();
    db.init_schema().await.unwrap();
    db.migrate().await.unwrap();
    db.fail_stale_active().await.unwrap();
    cleanup_pandora_qbit().await;
    cleanup_input_cache_startup().await;

    let mut queue: Vec<Job> = vec![];
    let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
    shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
    shrine.layer(Worker::Encode, pn_encdeworker, 5, 50);
    shrine.layer(Worker::Upload, pn_uloadworker, 5, 50);
    shrine.layer(Worker::Probe, pn_probeworker, 5, 50);

    loop {
        sleep(Duration::from_millis(200)).await;

        shrine.drain_heartbeats().await;
        cleanup_expired_input_cache().await;

        if do_queue_things(&mut rx, &db, &mut queue, &mut shrine).await {
            continue;
        }
        do_probe_timeout_things(&db, &mut queue).await;
        if do_worker_message_things(&db, &mut queue, &mut shrine).await {
            continue;
        }
        do_duplicate_waiting_things(&db, &mut queue).await;
        do_job_progression_things(&db, &mut queue, &mut shrine).await;
    }
}

async fn do_queue_things(
    rx: &mut Receiver<JobClass>,
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) -> bool {
    let Ok(jobclass) = rx.try_recv() else {
        return false;
    };
    match jobclass {
        JobClass::Job(mut job) => {
            if queue.len() > 4 {
                job.ready = Stage::Declined;
                render(&mut job, MessagePayload::Static(QUEUE_TOO_LONG)).await;
                return true;
            }
            if queue_new_job(db, queue, shrine, &mut job).await {
                return true;
            }
            db.insert_job(&job).await.unwrap();
            queue.push(job);
        }
        JobClass::HalfJob(halfjob) => {
            handle_half_job(db, queue, shrine, halfjob).await;
        }
    }
    false
}

async fn queue_new_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    match job.job_type {
        JobType::Encode => queue_encode_job(db, queue, shrine, job).await,
        JobType::Probe => queue_probe_job(db, queue, shrine, job).await,
        JobType::Pancode => queue_pancode_job(db, queue, shrine, job).await,
        JobType::Backup => queue_backup_job(db, queue, shrine, job).await,
        JobType::BackupAll => queue_backup_all_job(db, queue, shrine, job).await,
        _ => false,
    }
}

async fn prepare_queued_job(job: &mut Job, worker: &str, write_subtitle: bool) {
    job.worker = worker.to_string();
    render(job, MessagePayload::Static(QUEUED)).await;
    for i in STRUCT {
        create_dir_all(job.directory.join(i)).await.unwrap();
    }
    if write_subtitle {
        write(
            job.directory.join("contents").join("subtitle.ass"),
            &job.attachment,
        )
        .await
        .unwrap();
    }
}

async fn queue_encode_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    prepare_queued_job(job, "dwl-pending", true).await;
    if let Some((parent_id, parent_stage, forwarded_worker)) = queued_encode_parent(job, queue) {
        mark_forwarded(job, parent_id, parent_stage, &forwarded_worker);
        render(
            job,
            MessagePayload::Progress(TORRENT_DUPLICATE_WAIT, vec![parent_id.to_string()]),
        )
        .await;
        db.insert_job(job).await.unwrap();
        persist_forwarded_wait(db, job).await;
        queue.push(job.clone());
        return true;
    }
    queue_download_job(db, queue, shrine, job, None, false).await
}

async fn queue_probe_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    prepare_queued_job(job, "prb-main", false).await;
    if !dispatch_or_kill(
        shrine,
        &Worker::Probe,
        WorkerMsg::Probe((job.directory.clone(), job.torrent.clone(), job.job_id)),
        job,
        db,
        true,
    )
    .await
    {
        return true;
    }
    job.ready = Stage::Probing;
    job.frontend
        .set_presence(Presence::Probing {
            idx: queue.len(),
            total: queue.len() + 1,
        })
        .await;
    false
}

async fn queue_pancode_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let probe_dir = env::current_dir()
        .unwrap()
        .join("DB")
        .join("work")
        .join(job.probe_job_id.unwrap().to_string());
    prepare_queued_job(job, "dwl-pending", true).await;

    let torrent_src = probe_dir.join("contents").join("fetch.torrent");
    let torrent_dst = job.directory.join("contents").join("fetch.torrent");
    if tokio::fs::copy(&torrent_src, &torrent_dst).await.is_err() {
        return true;
    }

    queue_download_job(db, queue, shrine, job, job.probe_file_index, false).await
}

async fn queue_backup_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let probe_dir = job.probe_job_id.map(|id| {
        env::current_dir()
            .unwrap()
            .join("DB")
            .join("work")
            .join(id.to_string())
    });
    prepare_queued_job(job, "dwl-pending", false).await;
    if let Some(probe_dir) = probe_dir {
        let torrent_src = probe_dir.join("contents").join("fetch.torrent");
        let torrent_dst = job.directory.join("contents").join("fetch.torrent");
        if tokio::fs::copy(&torrent_src, &torrent_dst).await.is_err() {
            return true;
        }
    }
    queue_download_job(db, queue, shrine, job, job.probe_file_index, false).await
}

async fn queue_backup_all_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    prepare_queued_job(job, "dwl-pending", false).await;
    if !dispatch_or_kill(
        shrine,
        &Worker::Download,
        WorkerMsg::Download((job.directory.clone(), job.torrent.clone(), job.job_id, None, true)),
        job,
        db,
        true,
    )
    .await
    {
        return true;
    }
    if job.ready == Stage::Queued {
        job.ready = Stage::Downloading;
        job.frontend
            .set_presence(Presence::Downloading {
                idx: queue.len(),
                total: queue.len() + 1,
            })
            .await;
    }
    false
}

async fn queue_download_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
    file_index: Option<u64>,
    preserve_all: bool,
) -> bool {
    if use_cache_or_wait(db, job, queue).await {
        job.frontend
            .set_presence(Presence::Downloading {
                idx: queue.len(),
                total: queue.len() + 1,
            })
            .await;
    } else if !dispatch_or_kill(
        shrine,
        &Worker::Download,
        WorkerMsg::Download((
            job.directory.clone(),
            job.torrent.clone(),
            job.job_id,
            file_index,
            preserve_all,
        )),
        job,
        db,
        true,
    )
    .await
    {
        return true;
    }
    if job.ready == Stage::Queued {
        job.ready = Stage::Downloading;
        job.frontend
            .set_presence(Presence::Downloading {
                idx: queue.len(),
                total: queue.len() + 1,
            })
            .await;
    }
    false
}

async fn handle_half_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    halfjob: HalfJob,
) {
    match halfjob.job_type {
        JobType::Cancel => {
            if let Some(pos) = queue
                .iter()
                .position(|i| halfjob.job_id == i.job_id && halfjob.author == i.author)
            {
                if queue[pos].forward_parent.is_some() {
                    queue[pos].ready = Stage::Cancelled;
                    db.update_stage(queue[pos].job_id, Stage::Cancelled).await.ok();
                    render(
                        &mut queue[pos],
                        MessagePayload::Static(crate::pnworker::messages::JOB_CANCELLED),
                    )
                    .await;
                    let directory = queue[pos].directory.clone();
                    db.archive_job(queue[pos].job_id).await.ok();
                    cleanup_job(
                        &directory,
                        &PathBuf::from("DB")
                            .join("saved_data")
                            .join(queue[pos].job_id.to_string()),
                    )
                    .await;
                    queue.remove(pos);
                } else {
                    File::create(queue[pos].directory.join("CANCEL")).await.unwrap();
                }
            }
        }
        JobType::Hearts => {
            let mut frontend = halfjob.frontend;
            let statuses = shrine.hearts();
            let mut embed_text = String::new();
            for status in statuses {
                let beat = if status.alive {
                    format!("✅ Last beat {}s ago", status.last_beat_secs)
                } else {
                    format!("❌ Dead")
                };
                embed_text.push_str(&format!(
                    "**{:?}** — {} | Reboots: {}\n",
                    status.worker, beat, status.reboot_count
                ));
            }
            frontend.set_text(&embed_text).await;
        }
        JobType::GitSync => {
            let mut frontend = halfjob.frontend;
            frontend.notify_recompiling();
            shrine.kill().await;
            let repo_path = env::var("PANDORA_GITSYNC_REPO").unwrap_or_else(|_| {
                std::env::current_dir()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            });
            println!("{}", repo_path);
            let mut rebuild_requested = false;
            match git_pull(&repo_path) {
                Ok(_) => {
                    if let Ok(request_path) = env::var("PANDORA_GITSYNC_REQUEST") {
                        let request_path = PathBuf::from(request_path);
                        if let Some(parent) = request_path.parent() {
                            let _ = create_dir_all(parent).await;
                        }
                        rebuild_requested = write(request_path, b"rebuild\n").await.is_ok();
                    }
                    frontend.set_text("Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.").await
                }
                Err(e) => {
                    println!("{}", e);
                    frontend.set_text("Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.").await
                }
            }
            let _ = remove_dir_all(PathBuf::from("DB").join("work")).await;
            if rebuild_requested {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            } else {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            std::process::exit(0);
        }
        _ => {}
    }
}

async fn do_probe_timeout_things(db: &JobDb, queue: &mut Vec<Job>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    let timed_out: Vec<u64> = queue
        .iter()
        .filter(|j| j.ready == Stage::Probed)
        .filter(|j| now.saturating_sub(j.requested_at) > Duration::from_secs(180))
        .map(|j| j.job_id)
        .collect();
    for id in timed_out {
        if let Some(pos) = queue.iter().position(|j| j.job_id == id) {
            let directory = queue[pos].directory.clone();
            let mut frontend = queue[pos].frontend.clone();

            frontend
                .update(&queue[pos], &MessagePayload::Static(PROBE_TIMEOUT))
                .await;

            cleanup_job(
                &directory,
                &PathBuf::from("DB").join("saved_data").join(id.to_string()),
            )
            .await;
            db.archive_job(id).await.unwrap();
            queue.remove(pos);
            frontend.set_presence(presence_from_queue(queue)).await;
        }
    }
}

async fn do_worker_message_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) -> bool {
    let Some((_, commdata)) = shrine.receive(500).await else {
        return false;
    };
    let mut finished_fe: Option<Frontend> = None;
    if let Some(pos) = queue.iter().position(|j| j.job_id == commdata.0) {
        if let MessagePayload::Progress(id, args) = &commdata.1 {
            if *id == WORKER_ASSIGN {
                if let Some(worker) = args.get(0) {
                    let job_id = queue[pos].job_id;
                    let worker = worker.clone();
                    queue[pos].worker = worker.clone();
                    db.update_worker(job_id, &worker).await.ok();
                    let forwarded_worker = forwarded_worker_for(&worker);
                    sync_forwarded_state(db, queue, job_id, None, Some(&forwarded_worker)).await;
                }
                return true;
            }
            if *id == ENCODE_WARNING {
                if let Some(warning) = args.get(0) {
                    let warning = warning.clone();
                    if !queue[pos].encode_warnings.iter().any(|w| w == &warning) {
                        queue[pos].encode_warnings.push(warning.clone());
                    }
                    let parent_id = queue[pos].job_id;
                    for child in queue.iter_mut().filter(|j| j.forward_parent == Some(parent_id)) {
                        if !child.encode_warnings.iter().any(|w| w == &warning) {
                            child.encode_warnings.push(warning.clone());
                        }
                    }
                }
                return true;
            }
            if *id == TORRENT_DUPLICATE_WAIT {
                if let Some(path) = args.get(0) {
                    queue[pos].duplicate_source = Some(duplicate_path_to_container(path));
                }
                let v = serde_json::json!({ "type": "download", "waiting": "cache" });
                db.update_progress(queue[pos].job_id, &v.to_string()).await.ok();
                let payload = commdata.1;
                render(&mut queue[pos], payload).await;
                return true;
            }
        }

        let payload = commdata.1.clone();
        let stage = commdata.2;
        let mut finished_job: Option<(u64, Option<u64>, PathBuf)> = None;

        {
            let i = &mut queue[pos];
            if let Some(a) = stage {
                let previous_ready = i.ready;
                i.ready = a;
                db.update_stage(i.job_id, i.ready).await.unwrap();
                if a == Stage::Encoded || (a == Stage::Cancelled && past_downloaded(previous_ready))
                {
                    cache_encode_input(i).await;
                }
            }
            if stage == Some(Stage::Uploaded) {
                if let Some(acix) = i.acix.clone() {
                    if let Some(drive) = drive_link_from_payload(&payload) {
                        if drive.starts_with("http") {
                            let pending = crate::pnworker::acix::AcixPending {
                                status: "pending".to_string(),
                                acix,
                                drive,
                            };
                            if let Ok(j) = serde_json::to_string(&pending) {
                                db.set_acix_pending(i.job_id, &j).await.ok();
                            }
                        }
                    }
                }
            }
            persist_side_effects(db, i.job_id, &payload, stage, &i.encode_warnings).await;
            render(i, payload.clone()).await;

            let finished = matches!(i.ready, Stage::Uploaded | Stage::Failed | Stage::Cancelled);
            if finished {
                finished_fe = Some(i.frontend.clone());
                finished_job = Some((i.job_id, i.probe_job_id, i.directory.clone()));
            }
        }

        let parent_worker = queue
            .iter()
            .find(|job| job.job_id == commdata.0)
            .map(|job| job.worker.clone())
            .unwrap_or_else(|| "enc-forward".to_string());
        sync_forwarded_jobs(db, queue, commdata.0, stage, &payload, &parent_worker).await;

        if let Some((job_id, probe_job_id, directory)) = finished_job {
            if let Some(probe_id) = probe_job_id {
                if let Some(probe_pos) = queue.iter().position(|j| j.job_id == probe_id) {
                    let probe = &queue[probe_pos];
                    cleanup_job(
                        &probe.directory.clone(),
                        &PathBuf::from("DB")
                            .join("saved_data")
                            .join(probe_id.to_string()),
                    )
                    .await;
                    db.archive_job(probe_id).await.unwrap();
                    queue.remove(probe_pos);
                }
            }
            db.archive_job(job_id).await.unwrap();
            cleanup_job(
                &directory,
                &PathBuf::from("DB")
                    .join("saved_data")
                    .join(job_id.to_string()),
            )
            .await;
            queue.retain(|j| j.job_id != job_id);
        }
    }
    if let Some(fe) = finished_fe {
        fe.set_presence(presence_from_queue(queue)).await;
    }
    false
}

async fn do_duplicate_waiting_things(db: &JobDb, queue: &mut Vec<Job>) {
    let duplicate_waiting: Vec<u64> = queue
        .iter()
        .filter(|j| {
            j.forward_parent.is_none()
                && j.ready == Stage::Downloading
                && j.duplicate_source.is_some()
        })
        .map(|j| j.job_id)
        .collect();
    for id in duplicate_waiting {
        if let Some(pos) = queue.iter().position(|j| j.job_id == id) {
            if use_cached_input(&mut queue[pos]).await {
                db.update_stage(queue[pos].job_id, Stage::Downloaded)
                    .await
                    .unwrap();
                render(
                    &mut queue[pos],
                    MessagePayload::Static(crate::pnworker::messages::TORRENT_DONE),
                )
                .await;
                continue;
            }
            let Some(source_dir) = queue[pos].duplicate_source.clone() else {
                continue;
            };
            if !duplicate_source_ready(queue, &source_dir) {
                continue;
            }
            let source = duplicate_input_path(&source_dir);
            let target_dir = queue[pos].directory.join("contents").join("torrent");
            let target = target_dir.join("input.mkv");
            create_dir_all(&target_dir).await.unwrap();
            match tokio::fs::copy(&source, &target).await {
                Ok(_) => {
                    queue[pos].duplicate_source = None;
                    queue[pos].ready = Stage::Downloaded;
                    db.update_stage(queue[pos].job_id, Stage::Downloaded)
                        .await
                        .unwrap();
                    let v = serde_json::json!({ "type": "download", "percent": 100, "cached": true });
                    db.update_progress(queue[pos].job_id, &v.to_string()).await.ok();
                    render(
                        &mut queue[pos],
                        MessagePayload::Static(crate::pnworker::messages::TORRENT_DONE),
                    )
                    .await;
                }
                Err(e) => {
                    eprintln!("[Pandora] duplicate cache copy failed for {}: {}", id, e);
                }
            }
        }
    }
}

async fn do_job_progression_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) {
    let qlen = queue.len();
    let mut dead: Vec<u64> = vec![];
    let mut forwarded_state_updates: Vec<(u64, Stage, String)> = vec![];
    let mut active_encode_sources: HashMap<String, PathBuf> = queue
        .iter()
        .filter(|j| j.forward_parent.is_none() && j.ready == Stage::Encoding)
        .map(|j| {
            (
                input_cache_key(j),
                j.directory.join("contents").join("torrent"),
            )
        })
        .collect();
    let mut active_encode_parents: HashMap<String, (u64, Stage, String)> = HashMap::new();
    for j in queue.iter().filter(|j| {
        j.forward_parent.is_none()
            && is_forwardable_encode(j)
            && matches!(
                j.ready,
                Stage::Queued
                    | Stage::Downloading
                    | Stage::Encoding
                    | Stage::Encoded
                    | Stage::Uploading
            )
    }) {
        for key in encode_forward_keys(j) {
            active_encode_parents
                .entry(key)
                .or_insert((j.job_id, j.ready, forwarded_worker_for(&j.worker)));
        }
    }
    for (idx, job) in queue.iter_mut().enumerate() {
        if job.forward_parent.is_some() {
            continue;
        }
        if job.ready == Stage::Probed {
            continue;
        }

        if job.ready == Stage::Downloaded {
            if job.job_type == JobType::Backup {
                let src = job
                    .directory
                    .join("contents")
                    .join("torrent")
                    .join("input.mkv");
                let dst = job.directory.join("work").join("output.mp4");
                let _ = tokio::fs::rename(&src, &dst).await;
                job.worker = "upl-pending".to_string();
                db.update_worker(job.job_id, &job.worker).await.ok();
                if !dispatch_or_kill(
                    shrine,
                    &Worker::Upload,
                    WorkerMsg::Upload((
                        job.directory.clone(),
                        format!(
                            "{}.mkv",
                            job.directory.file_name().unwrap_or_default().display()
                        ),
                        false,
                        job.job_id,
                        job.server_id,
                        None,
                        None,
                    )),
                    job,
                    db,
                    false,
                )
                .await
                {
                    dead.push(job.job_id);
                    continue;
                }
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                job.frontend
                    .set_presence(Presence::Uploading { idx, total: qlen })
                    .await;
            } else if job.job_type == JobType::BackupAll {
                job.worker = "upl-pending".to_string();
                db.update_worker(job.job_id, &job.worker).await.ok();
                if !dispatch_or_kill(
                    shrine,
                    &Worker::Upload,
                    WorkerMsg::UploadAll((job.directory.clone(), job.job_id, job.server_id)),
                    job,
                    db,
                    false,
                )
                .await
                {
                    dead.push(job.job_id);
                    continue;
                }
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                job.frontend
                    .set_presence(Presence::Uploading { idx, total: qlen })
                    .await;
            } else {
                if let Some((parent_id, parent_stage, forwarded_worker)) = encode_forward_keys(job)
                    .iter()
                    .find_map(|key| active_encode_parents.get(key).cloned())
                {
                    if parent_id != job.job_id {
                        mark_forwarded(job, parent_id, parent_stage, &forwarded_worker);
                        persist_forwarded_wait(db, job).await;
                        render(
                            job,
                            MessagePayload::Progress(
                                TORRENT_DUPLICATE_WAIT,
                                vec![parent_id.to_string()],
                            ),
                        )
                        .await;
                        continue;
                    }
                }
                let key = input_cache_key(job);
                if let Some(source) = active_encode_sources.get(&key).cloned() {
                    job.duplicate_source = Some(source.clone());
                    job.ready = Stage::Downloading;
                    job.worker = "dwl-cache".to_string();
                    let v = serde_json::json!({ "type": "download", "waiting": "cache" });
                    db.update_stage(job.job_id, Stage::Downloading).await.unwrap();
                    db.update_progress(job.job_id, &v.to_string()).await.ok();
                    db.update_worker(job.job_id, &job.worker).await.ok();
                    render(
                        job,
                        MessagePayload::Progress(
                            TORRENT_DUPLICATE_WAIT,
                            vec![source.display().to_string()],
                        ),
                    )
                    .await;
                    continue;
                }
                job.worker = "enc-main".to_string();
                db.update_worker(job.job_id, &job.worker).await.ok();
                if !dispatch_or_kill(
                    shrine,
                    &Worker::Encode,
                    WorkerMsg::Encode((
                        job.directory.clone(),
                        job.preset.clone(),
                        job.job_id,
                        job.server_id,
                    )),
                    job,
                    db,
                    false,
                )
                .await
                {
                    dead.push(job.job_id);
                    continue;
                }
                job.ready = Stage::Encoding;
                active_encode_sources.insert(key, job.directory.join("contents").join("torrent"));
                for key in encode_forward_keys(job) {
                    active_encode_parents
                        .entry(key)
                        .or_insert((job.job_id, Stage::Encoding, forwarded_worker_for(&job.worker)));
                }
                db.update_stage(job.job_id, Stage::Encoding).await.unwrap();
                forwarded_state_updates.push((
                    job.job_id,
                    Stage::Encoding,
                    forwarded_worker_for(&job.worker),
                ));
                job.frontend
                    .set_presence(Presence::Encoding { idx, total: qlen })
                    .await;
            }
        } else if job.ready == Stage::Encoded {
            job.worker = "upl-pending".to_string();
            db.update_worker(job.job_id, &job.worker).await.ok();
            if !dispatch_or_kill(
                shrine,
                &Worker::Upload,
                WorkerMsg::Upload((
                    job.directory.clone(),
                    format!(
                        "{}.mp4",
                        job.directory.file_name().unwrap_or_default().display()
                    ),
                    match job.preset {
                        Preset::Dummy(_) => false,
                        _ => true,
                    },
                    job.job_id,
                    job.server_id,
                    job.gdrive_folder_global.clone(),
                    job.gdrive_folder_local.clone(),
                )),
                job,
                db,
                false,
            )
            .await
            {
                dead.push(job.job_id);
                continue;
            }
            job.ready = Stage::Uploading;
            db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
            forwarded_state_updates.push((
                job.job_id,
                Stage::Uploading,
                forwarded_worker_for(&job.worker),
            ));
            job.frontend
                .set_presence(Presence::Uploading { idx, total: qlen })
                .await;
        }
    }
    for (parent_id, stage, worker) in forwarded_state_updates {
        sync_forwarded_state(db, queue, parent_id, Some(stage), Some(&worker)).await;
    }
    queue.retain(|j| !dead.contains(&j.job_id));
}

async fn dispatch_or_kill(
    shrine: &mut TypedShrine<WorkerMsg>,
    worker: &Worker,
    msg: WorkerMsg,
    job: &mut Job,
    db: &JobDb,
    needs_insert: bool,
) -> bool {
    if let Err(e) = shrine.send(worker, msg).await {
        eprintln!("[Pandora] job {} dispatch failed: {}", job.job_id, e);
        job.frontend.mark_failed().await;
        if needs_insert {
            let _ = db.insert_job(job).await;
        }
        let _ = db.update_stage(job.job_id, Stage::Failed).await;
        let _ = db.archive_job(job.job_id).await;
        cleanup_job(
            &job.directory,
            &PathBuf::from("DB")
                .join("saved_data")
                .join(job.job_id.to_string()),
        )
        .await;
        false
    } else {
        true
    }
}

#[derive(Clone, Debug)]
pub enum Preset {
    PseudoLossless(Option<Vec<String>>),
    Dummy(Option<Vec<String>>),
    Standard(Option<Vec<String>>),
    Gpu(Option<Vec<String>>),
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u16)]
pub enum JobType {
    Encode = 001,
    Cancel = 002,
    Hearts = 003,
    GitSync = 004,
    Probe = 005,
    Pancode = 006,
    Scrape = 007,
    Backup = 008,
    BackupAll = 009,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Stage {
    Queued,
    Probing,
    Probed,
    Downloading,
    Downloaded,
    Encoding,
    Encoded,
    Uploading,
    Uploaded,
    Failed,
    Declined,
    Cancelled,
}

pub enum JobClass {
    Job(Job),
    HalfJob(HalfJob),
}

#[derive(Clone)]
pub struct HalfJob {
    pub author: u64,
    pub channel_id: u64,
    pub requested_at: Duration,
    pub job_id: u64,
    pub job_type: JobType,
    pub frontend: Frontend,
}

impl HalfJob {
    pub fn new_cancel(author: u64, channel_id: u64, job_id: u64) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Cancel,
            frontend: Frontend::None,
        }
    }
    pub fn new_hearts(
        author: u64,
        channel_id: u64,
        job_id: u64,
        context: Context,
        msg: Message,
    ) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Hearts,
            frontend: Frontend::discord(context, msg),
        }
    }
    pub fn new_gitsync(
        author: u64,
        channel_id: u64,
        job_id: u64,
        context: Context,
        msg: Message,
    ) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0)),
            job_type: JobType::GitSync,
            frontend: Frontend::discord(context, msg),
        }
    }
    pub fn new_gitsync_api(author: u64, channel_id: u64) -> Self {
        Self {
            author,
            channel_id,
            job_id: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            requested_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0)),
            job_type: JobType::GitSync,
            frontend: Frontend::Web,
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AcixPublish {
    pub name: String,
    pub mal_id: i64,
    pub season_num: Option<i64>,
    pub episode_num: Option<i64>,
    pub template: i64,
    pub extra: String,
}

#[derive(Clone)]
pub struct Job {
    pub author: u64,
    pub channel_id: u64,
    pub response_id: u64,
    pub requested_at: Duration,
    pub job_type: JobType,
    pub job_id: u64,
    pub preset: Preset,
    pub torrent: TorrentType,
    pub attachment: Vec<u8>,
    pub frontend: Frontend,
    pub directory: PathBuf,
    pub ready: Stage,
    pub probe_files: Option<Vec<(u64, String, u64)>>, // (index, name, size)
    pub probe_torrent_path: Option<String>,           // saved .torrent path for later
    pub probe_job_id: Option<u64>,
    pub probe_file_index: Option<u64>,
    pub lang: String,
    pub server_id: Option<u64>,
    pub acix: Option<AcixPublish>,
    pub gdrive_folder_global: Option<String>,
    pub gdrive_folder_local: Option<String>,
    pub worker: String,
    pub duplicate_source: Option<PathBuf>,
    pub forward_parent: Option<u64>,
    pub encode_warnings: Vec<String>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.job_id == other.job_id
    }
}

impl Job {
    pub fn new(
        author: u64,
        channel_id: u64,
        response_id: u64,
        job_type: JobType,
        job_id: u64,
        preset: Preset,
        torrent: TorrentType,
        attachment: Vec<u8>,
        context: Context,
        msg: Message,
        lang: String,
        server_id: Option<u64>,
    ) -> Self {
        let requested_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0));
        Self {
            author,
            channel_id,
            response_id,
            job_type,
            job_id,
            preset,
            torrent,
            attachment,
            frontend: Frontend::discord(context, msg),
            directory: env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("DB")
                .join("work")
                .join(format!("{}", job_id)),
            requested_at,
            ready: Stage::Queued,
            probe_files: None,
            probe_torrent_path: None,
            probe_job_id: None,
            probe_file_index: None,
            lang,
            server_id,
            acix: None,
            gdrive_folder_global: None,
            gdrive_folder_local: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
        }
    }

    pub fn new_api(
        author: u64,
        channel_id: u64,
        job_type: JobType,
        preset: Preset,
        torrent: TorrentType,
        attachment: Vec<u8>,
        lang: String,
        server_id: Option<u64>,
    ) -> Self {
        let requested_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0));
        let job_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            author,
            channel_id,
            response_id: 0,
            job_type,
            job_id,
            preset,
            torrent,
            attachment,
            frontend: Frontend::Web,
            directory: env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("DB")
                .join("work")
                .join(format!("{}", job_id)),
            requested_at,
            ready: Stage::Queued,
            probe_files: None,
            probe_torrent_path: None,
            probe_job_id: None,
            probe_file_index: None,
            lang,
            server_id,
            acix: None,
            gdrive_folder_global: None,
            gdrive_folder_local: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
        }
    }
}

/*
let candidates = intros.resolve(&group_name);
let preset = Preset::Standard(candidates);

HashMap::from([
    ("INPUT",      PathValue::from(path_to_ffmpeg(...))),
    ("CANDIDATES", PathValue::from(candidates.clone())),
    ...
])
*/
