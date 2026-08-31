use crate::lib::db::core::JobDb;
use crate::lib::p2p::core::cleanup_torrent_runtime;
use crate::lib::p2p::nyaaise::TorrentType;
use crate::lib::subs::ensure_ass_bytes;
use crate::lumiere_broker::cleanup_expired_hls;
use crate::pnworker::cache::{
    cache_encode_input, cleanup_expired_input_cache, cleanup_input_cache_startup,
    duplicate_input_path, duplicate_path_to_container, duplicate_source_orphaned,
    duplicate_source_owner,
    duplicate_source_ready, input_cache_keys, jobs_share_input, jobs_share_source, past_downloaded,
    use_cache_or_wait, use_cached_input,
};
use crate::pnworker::forwarding::{
    encode_forward_keys, forwarded_worker_for, is_forwardable_encode, mark_forwarded,
    persist_forwarded_wait, queued_encode_parent, sync_forwarded_jobs, sync_forwarded_state,
};
use crate::pnworker::batch::{
    BatchRequest, batch_child_may_dispatch, batch_page_available, build_batch_child,
    persist_batch_progress, store_batch_token,
};
use crate::pnworker::estimate::QueueEstimator;
use crate::pnworker::frontend::Frontend;
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};
use crate::pnworker::keep::{
    KeywordResolve, ResolvedKeywords, cleanup_expired_keeps, cleanup_keep_startup,
    mark_output_failed, prepare_keep, reserve_output, resolve_keywords_for_keycode, scope,
    store_output,
};
use crate::pnworker::lifecycle::{cleanup_job, render};
use crate::pnworker::studio::{cleanup_expired_studios, cleanup_studios_startup};
use crate::pnworker::messages::{
    ENCODE_CONCAT_PROG, ENCODE_PROG, ENCODE_STALLED, ENCODE_WARNING, GITQUERY_BLOCKED, JOB_SETUP_FAIL,
    MessagePayload, QUEUE_TOO_LONG, QUEUED, TORRENT_DUPLICATE_WAIT, TORRENT_FILE_DONE, UPLOAD_DONE,
    UPLOAD_PROG, WORKER_ASSIGN,
};
use crate::pnworker::presence::{Presence, presence_from_queue};
use crate::pnworker::progress::{drive_link_from_payload, persist_side_effects};
use crate::pnworker::pull::{git_pull, git_reset, head_commit, head_oid, SyncReport};
use crate::pnworker::server_effects::load_server_settings;
use crate::pnworker::drive_cleanup::{
    delete_job_drive_upload, drive_deletable_job_type, persist_job_drive_upload,
    DriveDeleteOutcome,
};
use crate::pnworker::smartcode_drive::{replace_smartcode_upload, SmartcodeDriveUpload};
use crate::pnworker::workers::downloadworker::*;
use crate::pnworker::workers::encodeworker::*;
use crate::pnworker::workers::probeworker::*;
use crate::pnworker::workers::uploadworker::*;
use crate::pnworker::workers_view::{
    build_workers_model, render_detail_lines, render_workers_columns, worker_waiting, WorkerJobView,
};
use crate::pnworker::worker_slots::{download_worker_slots, probe_worker_slots, upload_worker_slots};
use crate::pnworker::util::job_cancelled;
use serenity::all::{Context, CreateEmbed, Message};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{File, create_dir_all, remove_dir_all, rename, write};
use tokio::sync::mpsc::Receiver;
use tokio::time::Duration;
use tokio::time::sleep;

pub type CommData = (u64, MessagePayload, Option<Stage>);

#[derive(Clone)]
pub enum WorkerMsg {
    Download(DownloadData),
    Probe(ProbeData),
    Preview(PreviewData),
    StudioPreview(StudioData),
    Encode(EncodeData),
    Studio(StudioData),
    Keycode(KeycodeData),
    Upload(UploadData),
    UploadAll(UploadAllData),
    Subs(SubsData),
}

pub const STRUCT: [&str; 3] = ["contents", "work", "log"];

pub async fn pn_worker(mut rx: Receiver<JobClass>) {
    let db = JobDb::new().await.unwrap();
    db.init_schema().await.unwrap();
    db.migrate().await.unwrap();
    db.fail_stale_active().await.unwrap();
    cleanup_torrent_runtime().await;
    cleanup_input_cache_startup().await;
    cleanup_keep_startup().await;
    cleanup_studios_startup().await;
    cleanup_expired_hls().await;

    let mut queue: Vec<Job> = vec![];
    let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
    shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
    shrine.layer(Worker::Encode, pn_encdeworker, 5, 50);
    shrine.layer(Worker::Upload, pn_uloadworker, 5, 50);
    shrine.layer(Worker::Probe, pn_probeworker, 5, 50);
    let mut queue_estimator = QueueEstimator::new();
    let mut next_encode_dispatch_order = 1u64;
    let mut encodes_since_batch = 0u64;
    let mut encode_reboot_epoch = shrine.reboot_epoch(&Worker::Encode);
    let mut gitquery: Option<HalfJob> = None;
    let mut next_cache_cleanup = tokio::time::Instant::now() + Duration::from_secs(1);
    let mut next_studio_cleanup = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut next_snapshot = tokio::time::Instant::now();

    loop {
        sleep(Duration::from_millis(50)).await;

        // Once a second is plenty for something a human is reading, and keeps the loop's own cost flat.
        if tokio::time::Instant::now() >= next_snapshot {
            crate::pnworker::snapshot::publish(&shrine, &queue, gitquery.is_some());
            next_snapshot = tokio::time::Instant::now() + Duration::from_secs(1);
        }

        shrine.drain_heartbeats().await;
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        if tokio::time::Instant::now() >= next_cache_cleanup {
            cleanup_expired_input_cache().await;
            cleanup_expired_keeps().await;
            next_cache_cleanup = tokio::time::Instant::now() + Duration::from_secs(1);
        }
        if tokio::time::Instant::now() >= next_studio_cleanup {
            cleanup_expired_studios().await;
            cleanup_expired_hls().await;
            next_studio_cleanup = tokio::time::Instant::now() + Duration::from_secs(60);
        }

        if do_queue_things(&mut rx, &db, &mut queue, &mut shrine, &mut gitquery).await {
            check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
            continue;
        }
        do_link_things(&db, &mut queue, &mut shrine).await;
        do_probe_timeout_things(&db, &mut queue).await;
        do_encode_stall_things(&db, &mut queue, &mut shrine).await;
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        if do_worker_message_things(&db, &mut queue, &mut shrine).await {
            check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
            continue;
        }
        do_duplicate_waiting_things(&db, &mut queue).await;
        do_queued_download_waiting_things(&db, &mut queue, &mut shrine).await;
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        do_job_progression_things(
            &db,
            &mut queue,
            &mut shrine,
            &mut next_encode_dispatch_order,
            &mut encodes_since_batch,
        )
        .await;
        do_batch_parent_things(&db, &mut queue).await;
        if let Some(halfjob) = gitquery.take() {
            if encode_jobs_active(&queue) {
                gitquery = Some(halfjob);
            } else {
                run_gitsync(halfjob.frontend, &mut shrine, false).await;
            }
        }
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        queue_estimator.tick(&db, &mut queue).await;
    }
}

async fn do_queue_things(
    rx: &mut Receiver<JobClass>,
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    gitquery: &mut Option<HalfJob>,
) -> bool {
    let Ok(jobclass) = rx.try_recv() else {
        return false;
    };
    match jobclass {
        JobClass::Job(mut job) => {
            if gitquery.is_some() && is_encode_job_type(job.job_type) {
                decline_gitquery_blocked_encode(&mut job).await;
                return true;
            }
            // Batch children are spawned by the worker itself; counting them here would let one
            // batch decline every other submission with "queue too long".
            if queue.iter().filter(|job| job.batch_parent.is_none()).count() > 4 {
                job.ready = Stage::Declined;
                render(&mut job, MessagePayload::Static(QUEUE_TOO_LONG)).await;
                if matches!(job.job_type, JobType::Studio | JobType::StudioPreview) {
                    remove_dir_all(&job.directory).await.ok();
                }
                return true;
            }
            match try_link_offload(db, &mut job).await {
                LinkOffload::Offered => {
                    if let Err(e) = db.insert_job(&job).await {
                        eprintln!("[Pandora] job {} insert failed: {}", job.job_id, e);
                        decline_job_setup(&mut job, "internal error").await;
                        crate::pnworker::link::board::release(job.job_id);
                        return true;
                    }
                    queue.push(job);
                    return true;
                }
                LinkOffload::Declined => return true,
                LinkOffload::Local => {}
            }
            if queue_new_job(db, queue, shrine, &mut job).await {
                return true;
            }
            if let Err(e) = db.insert_job(&job).await {
                eprintln!("[Pandora] job {} insert failed: {}", job.job_id, e);
                decline_job_setup(&mut job, "internal error").await;
                return true;
            }
            persist_keep_reserved(db, &job).await;
            persist_batch_progress(db, &job).await;
            queue.push(job);
        }
        JobClass::HalfJob(halfjob) => {
            handle_half_job(db, queue, shrine, halfjob, gitquery).await;
        }
        JobClass::DriveDelete(request) => {
            let db = db.clone();
            tokio::spawn(async move {
                handle_drive_delete(&db, request).await;
            });
        }
    }
    false
}

async fn handle_drive_delete(db: &JobDb, request: DriveDeleteRequest) {
    let row = match db.get_job(request.job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(error) => {
            eprintln!(
                "[drive-delete] could not read reacted job {}: {}",
                request.job_id, error
            );
            return;
        }
    };
    if row.channel_id as u64 != request.channel_id
        || !request.may_delete(row.author as u64)
        || row.stage != 6
        || ![
            JobType::Encode as u16 as i64,
            JobType::Pancode as u16 as i64,
            JobType::Keycode as u16 as i64,
            JobType::Studio as u16 as i64,
        ]
        .contains(&row.job_type)
    {
        return;
    }
    match delete_job_drive_upload(db, request.job_id).await {
        Ok(DriveDeleteOutcome::Deleted { affected_jobs }) => println!(
            "[drive-delete] user {} removed Google Drive upload for job {} (shared job records: {:?})",
            request.author, request.job_id, affected_jobs
        ),
        Ok(DriveDeleteOutcome::NoCapability) => println!(
            "[drive-delete] job {} has no retained Google Drive deletion capability",
            request.job_id
        ),
        Err(error) => eprintln!(
            "[drive-delete] Google Drive removal failed for job {}: {}",
            request.job_id, error
        ),
    }
}

fn is_encode_job_type(job_type: JobType) -> bool {
    matches!(
        job_type,
        JobType::Encode | JobType::Pancode | JobType::Keycode | JobType::Studio | JobType::Batch
    )
}

fn encode_jobs_active(queue: &[Job]) -> bool {
    queue.iter().any(|job| is_encode_job_type(job.job_type))
}

async fn decline_gitquery_blocked_encode(job: &mut Job) {
    job.ready = Stage::Declined;
    job.worker = "gitquery".to_string();
    render(
        job,
        MessagePayload::Static(GITQUERY_BLOCKED),
    )
    .await;
}

async fn decline_job_setup(job: &mut Job, reason: &str) {
    job.ready = Stage::Declined;
    if let Some(keep) = &job.keep {
        mark_output_failed(&scope(job.server_id), keep).await.ok();
    }
    render(
        job,
        MessagePayload::Progress(JOB_SETUP_FAIL, vec![reason.to_string()]),
    )
    .await;
    let _ = remove_dir_all(&job.directory).await;
}

// A rebooted layer starts with an empty inbox, so whatever was queued for the old one is gone and
// has to be dispatched again. Only jobs handed to a *previous* layer qualify: `shrine.send` reboots
// an expired layer before sending, so without the epoch check the job dispatched microseconds ago
// would be cleared and sent a second time, putting two encoders on one work directory.
fn reset_encode_dispatches_after_reboot(queue: &mut [Job], epoch: u32) {
    for job in queue {
        if job.encode_dispatch_epoch >= epoch {
            continue;
        }
        if job.ready == Stage::Downloaded
            || (job.job_type == JobType::Keycode && job.ready == Stage::Queued)
        {
            job.encode_dispatched = false;
            job.encode_dispatch_order = None;
            job.encode_dispatched_at = None;
        }
    }
}

fn check_encode_reboot_epoch(
    shrine: &TypedShrine<WorkerMsg>,
    encode_reboot_epoch: &mut u32,
    queue: &mut [Job],
) {
    let current_encode_reboot_epoch = shrine.reboot_epoch(&Worker::Encode);
    if current_encode_reboot_epoch != *encode_reboot_epoch {
        reset_encode_dispatches_after_reboot(queue, current_encode_reboot_epoch);
        *encode_reboot_epoch = current_encode_reboot_epoch;
    }
}

enum LinkOffload {
    // Handed to a node. The mirror row is inserted and queued, but nothing is dispatched here.
    Offered,
    // Keep it on this machine, exactly as before the link existed.
    Local,
    // Setup refused the job outright (an unusable subtitle); it is already finished.
    Declined,
}

// The one place a job leaves this machine. It runs before `queue_new_job`, because everything that
// function does — preparing a work directory, dispatching a download — is what the node will do
// instead. A job that finds no free node simply falls through and runs here, so the cluster being
// full, drained, or absent is never a reason for work to wait.
async fn try_link_offload(db: &JobDb, job: &mut Job) -> LinkOffload {
    let Some(node) = crate::pnworker::link::coordinator::choose_node(job) else {
        return LinkOffload::Local;
    };
    // An HLS-only release is served for twelve hours from the machine that published it, and a
    // node has no public hostname to serve it from. Such a job is still encoded remotely; the node
    // holds its finished MP4 and hands it back, and this machine does the publishing, so every
    // playback URL stays on the one hostname that is already public.
    let return_output = crate::pnworker::server_config::server_hls_enabled(job.server_id).await;
    // A node has no `meta.pandora` for the originating guild, so the upload policy travels with
    // the job rather than being looked up on the far side.
    let drive_only = crate::pnworker::server_config::server_drive_only(job.server_id).await;
    let label = crate::pnworker::link::coordinator::worker_label(&node);
    // The same preparation a local job gets, and for the same reason: this is where a subtitle is
    // normalised to ASS and where an unusable one is refused with its own message, so a node can
    // never be the thing that discovers the attachment was a PGS stream.
    if let Err(reason) = prepare_queued_job(job, &label, true).await {
        decline_job_setup(job, &reason).await;
        return LinkOffload::Declined;
    }
    let settings = crate::pnworker::link::board::settings();
    let spec = crate::pnworker::link::coordinator::build_spec(
        job,
        unix_now().as_secs() + settings.lease_timeout_secs,
        crate::pnworker::link::board::DEFAULT_RENEW_SECS,
        return_output,
        drive_only,
    );
    crate::pnworker::link::board::offer(&node, spec);
    job.link_node = Some(node.clone());
    job.link_return_output = return_output;
    job.worker = label;
    job.ready = Stage::Queued;
    db.update_worker(job.job_id, &job.worker).await.ok();
    println!(
        "[link] job {} offered to {}{}",
        job.job_id,
        node,
        if return_output { " (output returns here for HLS publication)" } else { "" }
    );
    LinkOffload::Offered
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
        JobType::Subs => queue_subs_job(db, queue, shrine, job).await,
        JobType::Pancode => queue_pancode_job(db, queue, shrine, job).await,
        JobType::Batch => queue_batch_job(db, queue, shrine, job).await,
        JobType::Backup => queue_backup_job(db, queue, shrine, job).await,
        JobType::BackupAll => queue_backup_all_job(db, queue, shrine, job).await,
        JobType::Keycode => queue_keycode_job(db, queue, shrine, job).await,
        JobType::Preview => queue_preview_job(db, queue, shrine, job).await,
        JobType::Studio => queue_studio_job(db, queue, shrine, job).await,
        JobType::StudioPreview => queue_studio_preview_job(db, queue, shrine, job).await,
        _ => false,
    }
}

// Sets up the work directory for a queued job. The error string is the reason the
// caller hands to decline_job_setup, so subtitle problems reach the user as themselves
// instead of a generic setup failure.
async fn prepare_queued_job(job: &mut Job, worker: &str, write_subtitle: bool) -> Result<(), String> {
    job.worker = worker.to_string();
    if let Some((parent, keyword)) = keep_keywords(job) {
        render(
            job,
            MessagePayload::Progress(crate::pnworker::messages::KEEP_READY, vec![parent, keyword]),
        )
        .await;
    } else {
        render(job, MessagePayload::Static(QUEUED)).await;
    }
    for i in STRUCT {
        if let Err(e) = create_dir_all(job.directory.join(i)).await {
            eprintln!(
                "[Pandora] job {} work directory setup failed: {}",
                job.job_id, e
            );
            return Err("could not prepare the work directory".to_string());
        }
    }
    if write_subtitle {
        // Anything ffmpeg can read as text is accepted and normalised here, since libass
        // only takes ASS/SSA and the attachment arrives as raw bytes.
        if !job.attachment.is_empty() {
            match ensure_ass_bytes(&job.attachment).await {
                Ok(converted) => {
                    if converted.warning.is_some() {
                        println!("[Pandora] job {} subtitle converted to ASS", job.job_id);
                    }
                    job.attachment = converted.bytes;
                }
                Err(e) => {
                    eprintln!("[Pandora] job {} subtitle conversion failed: {}", job.job_id, e);
                    return Err(e);
                }
            }
        }
        if let Err(e) = write(
            job.directory.join("contents").join("subtitle.ass"),
            &job.attachment,
        )
        .await
        {
            eprintln!(
                "[Pandora] job {} subtitle setup failed: {}",
                job.job_id, e
            );
            return Err("could not prepare the work directory".to_string());
        }
        if let Some(watermark) = &job.server_watermark {
            if let Err(e) = write(
                job.directory.join("contents").join("server_watermark.ass"),
                watermark,
            )
            .await
            {
                eprintln!(
                    "[Pandora] job {} watermark setup failed: {}",
                    job.job_id, e
                );
                return Err("could not prepare the work directory".to_string());
            }
        }
    }
    Ok(())
}

async fn queue_encode_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if !prepare_keep_job(job, KeepKind::Encode).await {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEEP_FAIL,
                vec!["invalid or unavailable keyword".to_string()],
            ),
        )
        .await;
        return true;
    }
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    if let Some((parent_id, parent_stage, forwarded_worker)) = queued_encode_parent(job, queue) {
        mark_forwarded(job, parent_id, parent_stage, &forwarded_worker);
        render(
            job,
            MessagePayload::Progress(TORRENT_DUPLICATE_WAIT, vec![parent_id.to_string()]),
        )
        .await;
        if let Err(e) = db.insert_job(job).await {
            eprintln!(
                "[Pandora] forwarded job {} insert failed: {}",
                job.job_id, e
            );
        }
        persist_forwarded_wait(db, job).await;
        queue.push(job.clone());
        return true;
    }
    queue_download_job(db, queue, shrine, job, Vec::new(), false).await
}

async fn queue_probe_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if let Err(reason) = prepare_queued_job(job, "prw-pending", false).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
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

// A job built from a probe adopts the .torrent the probe already fetched instead of downloading it
// again: the file index the caller picked is an index into that exact metainfo, so refetching risks
// selecting against a different one. Returns whether the copy landed — what a miss means is the
// caller's to decide, since most can still fall back to refetching from the link and a backup,
// whose link is not necessarily still a torrent, cannot.
async fn adopt_probe_torrent(job: &Job, probe_id: u64) -> bool {
    let probe_dir = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("DB")
        .join("work")
        .join(probe_id.to_string());
    tokio::fs::copy(
        probe_dir.join("contents").join("fetch.torrent"),
        job.directory.join("contents").join("fetch.torrent"),
    )
    .await
    .is_ok()
}

async fn queue_pancode_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let Some(probe_id) = job.probe_job_id else {
        decline_job_setup(job, "probe job id missing").await;
        return true;
    };
    if !prepare_keep_job(job, KeepKind::Encode).await {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEEP_FAIL,
                vec!["invalid or unavailable keyword".to_string()],
            ),
        )
        .await;
        return true;
    }
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, &reason).await;
        return true;
    }

    if !adopt_probe_torrent(job, probe_id).await && job.torrent.get().trim().is_empty() {
        decline_job_setup(job, "probe torrent data is no longer available").await;
        return true;
    }

    queue_download_job(db, queue, shrine, job, job.probe_file_index.into_iter().collect(), false).await
}

// Extraction needs the video and nothing else — no subtitle attachment, no
// preset — so it queues straight to the downloader and picks a probed file index
// when one was supplied.
async fn queue_subs_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    if let Some(probe_id) = job.probe_job_id {
        if !adopt_probe_torrent(job, probe_id).await && job.torrent.get().trim().is_empty() {
            decline_job_setup(job, "probe torrent data is no longer available").await;
            return true;
        }
    }
    queue_download_job(
        db,
        queue,
        shrine,
        job,
        job.probe_file_index.into_iter().collect(),
        false,
    )
    .await
}

// A batch owns one download of many files. Its own work directory only ever holds the torrent —
// the per-episode subtitle goes to the child job that the finished file is handed to, so nothing
// here writes `contents/subtitle.ass`.
async fn queue_batch_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let Some(batch) = job.batch.clone() else {
        decline_job_setup(job, "batch selection missing").await;
        return true;
    };
    if batch.entries.is_empty() {
        decline_job_setup(job, "batch selection is empty").await;
        return true;
    }
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, &reason).await;
        return true;
    }

    if !adopt_probe_torrent(job, batch.probe_job_id).await && job.torrent.get().trim().is_empty() {
        decline_job_setup(job, "probe torrent data is no longer available").await;
        return true;
    }

    store_batch_token(&batch.token, job.job_id).await;
    queue_download_job(db, queue, shrine, job, batch.file_indices(), true).await
}

async fn queue_backup_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if !prepare_keep_job(job, KeepKind::Backup).await {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEEP_FAIL,
                vec!["invalid or unavailable keyword".to_string()],
            ),
        )
        .await;
        return true;
    }
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    if let Some(probe_id) = job.probe_job_id {
        if !adopt_probe_torrent(job, probe_id).await {
            decline_job_setup(job, "probe torrent data is no longer available").await;
            return true;
        }
    }
    queue_download_job(db, queue, shrine, job, job.probe_file_index.into_iter().collect(), false).await
}

async fn queue_keycode_job(
    db: &JobDb,
    queue: &[Job],
    _shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if let Err(reason) = prepare_queued_job(job, "enc-main", !job.attachment.is_empty()).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    let Some(request) = job.keycode.clone() else {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEYCODE_FAIL,
                vec!["missing keycode request".to_string()],
            ),
        )
        .await;
        return true;
    };
    if request.keywords.is_empty() {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEYCODE_FAIL,
                vec!["at least one keyword is required".to_string()],
            ),
        )
        .await;
        return true;
    }
    persist_keycode_waiting(db, job, &request.keywords).await;
    job.frontend
        .set_presence(Presence::QueueTotal(queue.len() + 1))
        .await;
    false
}

enum KeycodeDispatch {
    Waiting,
    Dispatched,
    Failed,
}

async fn try_dispatch_keycode(
    db: &JobDb,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
    next_encode_dispatch_order: &mut u64,
) -> KeycodeDispatch {
    let Some(request) = job.keycode.clone() else {
        return fail_keycode(db, job, "missing keycode request").await;
    };
    let resolved =
        match resolve_keywords_for_keycode(&scope(job.server_id), &request.keywords).await {
            Ok(KeywordResolve::Ready(resolved)) => resolved,
            Ok(KeywordResolve::Waiting(waiting)) => {
                persist_keycode_waiting(db, job, &waiting).await;
                return KeycodeDispatch::Waiting;
            }
            Err(e) => return fail_keycode(db, job, &e).await,
        };
    dispatch_keycode_ready(
        db,
        shrine,
        job,
        request,
        resolved,
        next_encode_dispatch_order,
    )
    .await
}

async fn dispatch_keycode_ready(
    db: &JobDb,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
    _request: KeycodeRequest,
    resolved: ResolvedKeywords,
    next_encode_dispatch_order: &mut u64,
) -> KeycodeDispatch {
    if resolved.kind == KeepKind::Backup && job.attachment.is_empty() {
        return fail_keycode(db, job, "backup keywords require a subtitle").await;
    }
    let inputs = resolved.paths;
    let intro_dir = match &job.preset {
        Preset::PseudoLossless(intro_dir)
        | Preset::Dummy(intro_dir)
        | Preset::Standard(intro_dir)
        | Preset::VerySlow(intro_dir)
        | Preset::Hd720(intro_dir)
        | Preset::Sd480(intro_dir)
        | Preset::Gpu(intro_dir) => intro_dir.clone(),
        Preset::Copy => None,
    };
    if inputs.is_empty() {
        return fail_keycode(db, job, "no usable keyword outputs").await;
    }
    job.worker = "enc-main".to_string();
    db.update_worker(job.job_id, &job.worker).await.ok();
    if !dispatch_or_kill(
        shrine,
        &Worker::Encode,
        WorkerMsg::Keycode((
            job.directory.clone(),
            inputs,
            intro_dir,
            resolved.kind,
            job.job_id,
            job.server_id,
        )),
        job,
        db,
        false,
    )
    .await
    {
        return KeycodeDispatch::Failed;
    }
    // Read the epoch after the send: `shrine.send` may have rebooted an expired layer on its way in.
    mark_encode_dispatched(
        job,
        next_encode_dispatch_order,
        shrine.reboot_epoch(&Worker::Encode),
    );
    KeycodeDispatch::Dispatched
}

async fn fail_keycode(db: &JobDb, job: &mut Job, reason: &str) -> KeycodeDispatch {
    job.ready = Stage::Failed;
    job.worker = "key-fail".to_string();
    db.update_stage(job.job_id, Stage::Failed).await.ok();
    db.update_worker(job.job_id, &job.worker).await.ok();
    render(
        job,
        MessagePayload::Progress(
            crate::pnworker::messages::KEYCODE_FAIL,
            vec![reason.to_string()],
        ),
    )
    .await;
    db.archive_job(job.job_id).await.ok();
    cleanup_job(
        &job.directory,
        &PathBuf::from("DB")
            .join("saved_data")
            .join(job.job_id.to_string()),
    )
    .await;
    KeycodeDispatch::Failed
}

async fn persist_keycode_waiting(db: &JobDb, job: &mut Job, keywords: &[String]) {
    let first_wait = job.worker != "key-wait";
    job.worker = "key-wait".to_string();
    db.update_worker(job.job_id, &job.worker).await.ok();
    let v = serde_json::json!({
        "type": "keycode",
        "waiting": keywords,
    });
    db.update_progress(job.job_id, &v.to_string()).await.ok();
    if first_wait {
        render(
            job,
            MessagePayload::Progress(
                crate::pnworker::messages::KEYCODE_WAIT,
                vec![keywords.join(", ")],
            ),
        )
        .await;
    }
}

async fn prepare_keep_job(job: &mut Job, kind: KeepKind) -> bool {
    let Some(mut keep) = job.keep.clone() else {
        return true;
    };
    let prepared = match prepare_keep(&scope(job.server_id), kind, &keep).await {
        Ok(prepared) => prepared,
        Err(e) => {
            eprintln!("[Pandora] keep prepare failed for {}: {}", job.job_id, e);
            return false;
        }
    };
    keep.parent_keyword = Some(prepared.parent_keyword);
    keep.output_keyword = Some(prepared.output_keyword);
    if kind == KeepKind::Encode && keep.keyword.is_some() {
        job.preset = preset_without_intro(&job.preset);
    }
    job.keep = Some(keep);
    if let Some(keep) = &job.keep {
        if let Err(e) = reserve_output(
            &scope(job.server_id),
            kind,
            keep,
            if kind == KeepKind::Encode {
                Some(&job.preset)
            } else {
                None
            },
            job.job_id,
        )
        .await
        {
            eprintln!(
                "[Pandora] keep reservation failed for {}: {}",
                job.job_id, e
            );
            return false;
        }
    }
    true
}

async fn persist_keep_reserved(db: &JobDb, job: &Job) {
    let Some((parent, keyword)) = keep_keywords(job) else {
        return;
    };
    let v = serde_json::json!({
        "type": "keep",
        "keyword": keyword,
        "parent_keyword": parent,
        "ready": false,
    });
    db.update_progress(job.job_id, &v.to_string()).await.ok();
}

fn keep_keywords(job: &Job) -> Option<(String, String)> {
    let keep = job.keep.as_ref()?;
    Some((keep.parent_keyword.clone()?, keep.output_keyword.clone()?))
}

fn preset_without_intro(preset: &Preset) -> Preset {
    match preset {
        Preset::PseudoLossless(_) => Preset::PseudoLossless(None),
        Preset::Dummy(_) => Preset::Dummy(None),
        Preset::Standard(_) => Preset::Standard(None),
        Preset::VerySlow(_) => Preset::VerySlow(None),
        Preset::Gpu(_) => Preset::Gpu(None),
        Preset::Hd720(_) => Preset::Hd720(None),
        Preset::Sd480(_) => Preset::Sd480(None),
        Preset::Copy => Preset::Copy,
    }
}

async fn queue_backup_all_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    if !dispatch_or_kill(
        shrine,
        &Worker::Download,
        WorkerMsg::Download((
            job.directory.clone(),
            job.torrent.clone(),
            job.job_id,
            Vec::new(),
            true,
            None,
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

async fn queue_preview_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if let Err(reason) = prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, &reason).await;
        return true;
    }
    queue_download_job(db, queue, shrine, job, Vec::new(), false).await
}

async fn queue_studio_job(
    _db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let Some(request) = job.studio.clone() else {
        decline_job_setup(job, "missing Studio render manifest").await;
        return true;
    };
    if !request.manifest.exists() {
        decline_job_setup(job, "Studio render manifest is missing").await;
        return true;
    }
    if prepare_queued_job(job, "enc-main", false).await.is_err() {
        decline_job_setup(job, "could not prepare the Studio work directory").await;
        return true;
    }
    if !dispatch_or_kill(
        shrine,
        &Worker::Encode,
        WorkerMsg::Studio((job.directory.clone(), request.manifest, job.job_id)),
        job,
        _db,
        false,
    ).await {
        return true;
    }
    job.ready = Stage::Encoding;
    job.encode_dispatched = true;
    job.frontend.set_presence(Presence::Encoding { idx: queue.len(), total: queue.len() + 1 }).await;
    false
}

async fn queue_studio_preview_job(
    _db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    let Some(request) = job.studio.clone() else {
        decline_job_setup(job, "missing Studio preview manifest").await;
        return true;
    };
    if !request.manifest.exists() {
        decline_job_setup(job, "Studio preview manifest is missing").await;
        return true;
    }
    if prepare_queued_job(job, "prw-pending", false).await.is_err() {
        decline_job_setup(job, "could not prepare the Studio preview directory").await;
        return true;
    }
    if !dispatch_or_kill(
        shrine,
        &Worker::Probe,
        WorkerMsg::StudioPreview((job.directory.clone(), request.manifest, job.job_id)),
        job,
        _db,
        false,
    ).await {
        return true;
    }
    job.ready = Stage::Encoding;
    job.frontend.set_presence(Presence::Encoding { idx: queue.len(), total: queue.len() + 1 }).await;
    false
}

async fn queue_download_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
    file_indices: Vec<u64>,
    preserve_all: bool,
) -> bool {
    // Both shortcuts below hand back a single `input.mkv`. A batch needs its own multi-file
    // download or no per-file completion ever fires and it produces nothing, so it always runs its
    // own transfer; a torrent genuinely locked elsewhere fails it visibly instead.
    let batch = job.batch.is_some();
    if !batch && use_cache_or_wait(db, job, queue).await {
        job.frontend
            .set_presence(Presence::Downloading {
                idx: queue.len(),
                total: queue.len() + 1,
            })
            .await;
    } else if !batch && wait_for_active_torrent_download(db, job, queue).await {
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
            file_indices,
            preserve_all,
            if !preserve_all && matches!(job.job_type, JobType::Encode | JobType::Pancode) {
                download_aot_for(&job.preset)
            } else {
                None
            },
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

fn active_torrent_download_source(job: &Job, queue: &[Job]) -> Option<PathBuf> {
    queue
        .iter()
        .find(|other| {
            let active_download = other.ready == Stage::Downloading && other.duplicate_source.is_none();
            let earlier_queued_wait = other.ready == Stage::Queued
                && other.worker == "dwl-pending"
                && (other.requested_at < job.requested_at
                    || (other.requested_at == job.requested_at && other.job_id < job.job_id));
            other.forward_parent.is_none()
                && other.batch.is_none()
                && other.job_id != job.job_id
                && (active_download || earlier_queued_wait)
                && jobs_share_source(job, other)
        })
        .map(|other| other.directory.join("contents").join("torrent"))
}

async fn wait_for_active_torrent_download(db: &JobDb, job: &mut Job, queue: &[Job]) -> bool {
    let Some(source) = active_torrent_download_source(job, queue) else {
        return false;
    };
    job.ready = Stage::Queued;
    job.worker = "dwl-pending".to_string();
    let v = serde_json::json!({ "type": "download", "waiting": "cache" });
    db.update_progress(job.job_id, &v.to_string()).await.ok();
    db.update_worker(job.job_id, &job.worker).await.ok();
    db.update_stage(job.job_id, job.ready).await.ok();
    render(
        job,
        MessagePayload::Progress(TORRENT_DUPLICATE_WAIT, vec![source.display().to_string()]),
    )
    .await;
    true
}

async fn handle_half_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    halfjob: HalfJob,
    gitquery: &mut Option<HalfJob>,
) {
    match halfjob.job_type {
        JobType::Cancel => {
            if let Some(pos) = queue
                .iter()
                .position(|i| halfjob.job_id == i.job_id && halfjob.may_cancel(i.author))
            {
                // Cancelling the batch has to reach the episodes it already handed to the encoder;
                // they are ordinary jobs by then and nothing else would stop them.
                if queue[pos].batch.is_some() {
                    let parent_id = queue[pos].job_id;
                    let children: Vec<PathBuf> = queue
                        .iter()
                        .filter(|job| job.batch_parent == Some(parent_id))
                        .map(|job| job.directory.clone())
                        .collect();
                    for directory in children {
                        File::create(directory.join("CANCEL")).await.ok();
                    }
                }
                // A leased job is cancelled where it is running. The request rides the node's
                // next renew; the node then takes its own local cancel path and reports back, so
                // the job ends through exactly the same events as any other remote transition.
                if let Some(node) = queue[pos].link_node.clone() {
                    if crate::pnworker::link::board::request_cancel(queue[pos].job_id) {
                        println!(
                            "[link] {node} | cancel requested for job {}",
                            queue[pos].job_id
                        );
                        return;
                    }
                    // The lease is already gone; fall through and end it here.
                }
                let disposition = cancel_disposition(&queue[pos]);
                if let Err(e) = File::create(queue[pos].directory.join("CANCEL")).await {
                    if disposition == CancelDisposition::CancelFile {
                        eprintln!(
                            "[Pandora] cancel marker could not be written; job {} will finish its current stage: {}",
                            queue[pos].job_id, e
                        );
                    } else {
                        eprintln!(
                            "[Pandora] cancel marker could not be written for job {}: {}",
                            queue[pos].job_id, e
                        );
                    }
                }
                if disposition == CancelDisposition::Immediate {
                    finalize_cancelled_job(db, queue, pos).await;
                }
            }
        }
        JobType::Hearts => {
            let mut frontend = halfjob.frontend;
            let statuses = shrine.hearts();
            let all_alive = statuses.iter().all(|status| status.alive);
            let mut details = String::new();
            for status in statuses {
                let beat = if status.alive {
                    format!("✅ Last beat `{}s` ago", status.last_beat_secs)
                } else {
                    "❌ No heartbeat".to_string()
                };
                details.push_str(&format!(
                    "**{:?}** — {} • reboots `{}`\n",
                    status.worker, beat, status.reboot_count
                ));
            }
            let embed = CreateEmbed::new()
                .title("💗 Pandora shrine health")
                .description(details)
                .colour(if all_alive { serenity::all::Colour::DARK_GREEN } else { serenity::all::Colour::RED })
                .timestamp(serenity::model::Timestamp::now());
            frontend.set_embed(embed).await;
        }
        JobType::Workers => {
            let mut frontend = halfjob.frontend;
            frontend.set_embed(create_workers_embed(queue).await).await;
        }
        JobType::GitSync => {
            run_gitsync(halfjob.frontend, shrine, false).await;
        }
        JobType::GitForce => {
            run_gitsync(halfjob.frontend, shrine, true).await;
        }
        JobType::GitQuery => {
            let mut frontend = halfjob.frontend.clone();
            if gitquery.is_some() {
                frontend
                    .set_text("A git query is already waiting for encode jobs to finish.")
                    .await;
            } else if encode_jobs_active(queue) {
                frontend
                    .set_text("Git query armed. New encode jobs are disabled; git sync will run after current encode jobs finish.")
                    .await;
                *gitquery = Some(halfjob);
            } else {
                run_gitsync(halfjob.frontend, shrine, false).await;
            }
        }
        _ => {}
    }
}

// `force` is `/gitforce`: reset onto origin's tip rather than fast-forwarding towards it, and bump
// the build whether or not anything moved. Both halves exist for the same reason — a fast-forward
// cannot rescue a checkout that has diverged, and a build that only moves with HEAD cannot make a
// cluster restart onto a rebuild. Neither belongs in `/gitsync`, which is run constantly and must
// not discard local state or restart every node for nothing.
async fn run_gitsync(mut frontend: Frontend, shrine: &mut TypedShrine<WorkerMsg>, force: bool) {
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
    // Captured before the checkout moves, so "did this sync actually change anything" is answered
    // by comparing commits rather than by trusting the pull to have said so.
    let previous = head_oid(&repo_path);
    let outcome = if force {
        // An empty target means origin's branch tip, which is the only thing `/gitforce` can mean
        // when it is invoked from Discord with nothing to name.
        git_reset(&repo_path, "")
    } else {
        git_pull(&repo_path)
    };
    // A failed pull still reports HEAD, so the reply always names the revision the bot restarts on.
    let (synced, status, report) = match outcome {
        Ok(report) => (
            true,
            "Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.",
            Some(report),
        ),
        Err(e) => {
            println!("{}", e);
            (
                false,
                "Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.",
                head_commit(&repo_path).map(SyncReport::at_head),
            )
        }
    };
    let mut lines = vec![status.to_string()];
    if let Some(report) = report {
        lines.extend(report.lines());
    }
    if synced {
        lines.extend(advance_release(&repo_path, previous.as_deref(), force).await);
    }
    frontend.set_text(&lines.join("\n")).await;
    preserve_work_logs().await;
    let _ = remove_dir_all(PathBuf::from("DB").join("work")).await;
    if synced {
        crate::lib::release::restart_into_new_build().await;
    }
    // Nothing was pulled, so there is nothing new to build: exit into the restart loop without
    // asking a Docker host to rebuild an image whose source did not change.
    tokio::time::sleep(Duration::from_secs(1)).await;
    std::process::exit(0);
}

// The build number, and the migrations that go with it.
//
// The number moves only when the checkout did, because it is what every node in the cluster resets
// and restarts on: bumping it for a sync that pulled nothing would drain and restart every machine
// to arrive back where they started. `/gitforce` is the deliberate exception, and it is a separate
// command precisely so that cost is asked for rather than paid by accident.
//
// Migrations run here — after the pull, before the exit — which is the only moment they can. The
// scripts are the newly pulled ones and the binary is still the old one, so a migration prepares
// the state that the build about to be compiled expects to find.
async fn advance_release(repo_path: &str, previous: Option<&str>, force: bool) -> Vec<String> {
    let current = head_oid(repo_path).unwrap_or_default();
    let moved = previous != Some(current.as_str());
    if !moved && !force {
        return Vec::new();
    }
    let record = crate::lib::release::bump(&current);
    if force {
        // Nodes reset rather than fast-forward onto this one. Recorded against the build so the
        // next ordinary sync stops matching it and the reset does not repeat forever.
        crate::pnworker::link::board::mark_forced_reset(record.build);
    }
    let mut lines = vec![format!(
        "Build `{}`{}.",
        record.build,
        if force { " — nodes will reset onto it" } else { "" }
    )];
    let run = crate::lib::migration::run_pending(std::path::Path::new(repo_path)).await;
    if let Some(summary) = run.summary() {
        lines.push(summary);
    }
    lines
}

// `/gitsync` clears DB/work so the restart does not inherit half-finished scratch directories, but a
// job's logs only reach DB/saved_data when it *archives*. Anything still running at sync time —
// which includes every job that was stuck, the exact case worth reading afterwards — lost its logs
// to that wipe. Move them out first; the scratch data still goes.
async fn preserve_work_logs() {
    let saved = preserve_logs_from(
        &PathBuf::from("DB").join("work"),
        &PathBuf::from("DB").join("saved_data"),
    )
    .await;
    if !saved.is_empty() {
        // These are the jobs the wipe on the next line is about to break. Each one dies moments
        // later with its scratch gone — "No such file or directory" from a tool that was healthy a
        // second earlier — and nothing else on either side names the connection. Print the ids so a
        // failed encode can be traced back to the deploy that caused it.
        println!(
            "[Pandora] gitsync is clearing DB/work under {} unfinished job(s): {}",
            saved.len(),
            saved.iter().map(u64::to_string).collect::<Vec<String>>().join(", ")
        );
    }
}

// Returns the ids whose logs it kept — which is also the list of jobs the caller's wipe is about
// to pull the ground out from under.
async fn preserve_logs_from(work: &std::path::Path, saved_data: &std::path::Path) -> Vec<u64> {
    let Ok(mut entries) = tokio::fs::read_dir(work).await else {
        return Vec::new();
    };
    let mut saved = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let source = entry.path().join("log");
        if !source.is_dir() {
            continue;
        }
        // Job directories are named by job id; batch-pending and other scratch dirs are not, and
        // have no logs to keep anyway.
        let Some(job_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        let dest = saved_data.join(job_id.to_string());
        if create_dir_all(&dest).await.is_err() {
            continue;
        }
        let dest = dest.join("log");
        // An archived job already moved its logs there; never overwrite the finished copy.
        if dest.exists() {
            continue;
        }
        if rename(&source, &dest).await.is_ok() {
            saved.push(job_id);
        }
    }
    saved.sort_unstable();
    saved
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum CancelDisposition {
    Immediate,
    CancelFile,
}

async fn create_workers_embed(queue: &[Job]) -> CreateEmbed {
    let download = download_worker_slots().await;
    let probe = probe_worker_slots().await;
    let upload = upload_worker_slots().await;
    let views = queue
        .iter()
        .map(|job| WorkerJobView {
            worker: job.worker.clone(),
            active: worker_active_stage(job.ready),
            waiting: worker_waiting(&job.worker),
            job_id: job.job_id,
            organisation: job_organisation(job),
            type_label: job_type_label(job.job_type),
            stage_label: stage_label(job.ready),
        })
        .collect::<Vec<_>>();
    let model = build_workers_model(&views, download, probe, upload);
    let (download_column, core_column, upload_column) = render_workers_columns(&model);
    let mut embed = CreateEmbed::new()
        .title("Pandora workers")
        .description(format!("{} active queue item(s)", model.queue_len))
        .field("download", download_column, true)
        .field("core", core_column, true)
        .field("upload", upload_column, true);
    if !model.active.is_empty() {
        embed = embed.field("active", render_detail_lines(&model.active), false);
    }
    if !model.waiting.is_empty() {
        embed = embed.field("waiting", render_detail_lines(&model.waiting), false);
    }
    embed
}

fn job_organisation(job: &Job) -> String {
    if let Some(name) = job
        .smartcode_drive_name
        .as_ref()
        .map(|name| name.organisation.trim())
        .filter(|name| !name.is_empty())
    {
        return name.to_string();
    }
    if let Some(org) = job
        .server_id
        .and_then(|server_id| organisation_from_channel_meta(server_id, job.channel_id))
    {
        return org;
    }
    "anonymous".to_string()
}

fn organisation_from_channel_meta(server_id: u64, channel_id: u64) -> Option<String> {
    let path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join("meta.toml");
    let raw = std::fs::read_to_string(path).ok()?;
    let val = toml::from_str::<toml::Value>(&raw).ok()?;
    let repo_url = val.get("repo_url")?.as_str()?.trim();
    organisation_from_repo_url(repo_url)
}

fn organisation_from_repo_url(repo_url: &str) -> Option<String> {
    let repo_url = repo_url.trim();
    if repo_url.is_empty() {
        return None;
    }
    if let Ok(url) = reqwest::Url::parse(repo_url) {
        return url
            .path_segments()
            .and_then(|mut segments| segments.next())
            .map(str::trim)
            .filter(|org| !org.is_empty())
            .map(|org| org.to_string());
    }
    repo_url
        .trim_end_matches('/')
        .rsplit_once('/')
        .and_then(|(left, _)| left.rsplit('/').next())
        .map(str::trim)
        .filter(|org| !org.is_empty())
        .map(|org| org.to_string())
}

fn worker_active_stage(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Probing | Stage::Downloading | Stage::Encoding | Stage::Uploading
    )
}

fn job_type_label(job_type: JobType) -> &'static str {
    match job_type {
        JobType::Encode => "encode",
        JobType::Cancel => "cancel",
        JobType::Hearts => "hearts",
        JobType::Workers => "workers",
        JobType::GitSync => "gitsync",
        JobType::Probe => "probe",
        JobType::Pancode => "pancode",
        JobType::Scrape => "scrape",
        JobType::Backup => "backup",
        JobType::BackupAll => "backupall",
        JobType::Keycode => "keycode",
        JobType::GitQuery => "gitquery",
        JobType::Preview => "preview",
        JobType::Studio => "studio",
        JobType::StudioPreview => "studio-preview",
        JobType::Batch => "batch",
        JobType::Subs => "subs",
        JobType::GitForce => "gitforce",
    }
}

fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Queued => "queued",
        Stage::Probing => "probing",
        Stage::Probed => "probed",
        Stage::Downloading => "downloading",
        Stage::Downloaded => "downloaded",
        Stage::Encoding => "encoding",
        Stage::Encoded => "encoded",
        Stage::Uploading => "uploading",
        Stage::Uploaded => "uploaded",
        Stage::Failed => "failed",
        Stage::Declined => "declined",
        Stage::Cancelled => "cancelled",
    }
}

async fn persist_smartcode_drive_upload(job: &Job, payload: &MessagePayload, stage: Option<Stage>) {
    if stage != Some(Stage::Uploaded) || job.smartcode_drive_name.is_none() {
        return;
    }
    let Some(server_id) = job.server_id else {
        return;
    };
    let MessagePayload::Progress(id, args) = payload else {
        return;
    };
    if *id != UPLOAD_PROG && *id != UPLOAD_DONE && *id != crate::pnworker::messages::UPLOAD_BACKUP_PROG {
        return;
    }
    let Some(name) = job.smartcode_drive_name.as_ref() else {
        return;
    };
    let Some(file_id) = args.get(5).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(folder_id) = args.get(6).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return;
    };
    let profile = args.get(7).cloned().unwrap_or_default();
    let delete_token = args.get(8).cloned().unwrap_or_default();
    let root = args.get(9).map(|value| value.trim()).unwrap_or_default();
    if !profile.starts_with("guild:") || root != "smartcode" {
        return;
    }
    let url = args.first().cloned().unwrap_or_default();
    let upload = SmartcodeDriveUpload {
        job_id: job.job_id,
        file_id: file_id.to_string(),
        folder_id: folder_id.to_string(),
        profile,
        delete_token,
        url,
    };
    if let Err(e) = replace_smartcode_upload(server_id, job.channel_id, name.episode, upload).await {
        eprintln!(
            "[smartcode-drive] failed to replace Drive upload for server={} channel={} episode={}: {}",
            server_id, job.channel_id, name.episode, e
        );
    }
}

fn cancel_disposition(job: &Job) -> CancelDisposition {
    if job.forward_parent.is_some() {
        return CancelDisposition::Immediate;
    }
    if job.encode_dispatched {
        return CancelDisposition::CancelFile;
    }
    if job.duplicate_source.is_some() || matches!(job.worker.as_str(), "dwl-pending" | "key-wait") {
        return CancelDisposition::Immediate;
    }
    match job.ready {
        Stage::Queued | Stage::Downloaded | Stage::Probed | Stage::Encoded => {
            CancelDisposition::Immediate
        }
        Stage::Probing | Stage::Downloading | Stage::Encoding | Stage::Uploading => {
            CancelDisposition::CancelFile
        }
        Stage::Uploaded | Stage::Failed | Stage::Declined | Stage::Cancelled => {
            CancelDisposition::Immediate
        }
    }
}

async fn finalize_cancelled_job(db: &JobDb, queue: &mut Vec<Job>, pos: usize) {
    let job_id = queue[pos].job_id;
    let previous_ready = queue[pos].ready;
    let payload = MessagePayload::Static(crate::pnworker::messages::JOB_CANCELLED);
    queue[pos].ready = Stage::Cancelled;
    db.update_stage(job_id, Stage::Cancelled).await.ok();
    if past_downloaded(previous_ready) {
        cache_encode_input(&queue[pos]).await;
    }
    if let Some(keep) = &queue[pos].keep {
        mark_output_failed(&scope(queue[pos].server_id), keep).await.ok();
    }
    render(&mut queue[pos], payload.clone()).await;
    let parent_worker = queue[pos].worker.clone();
    sync_forwarded_jobs(db, queue, job_id, Some(Stage::Cancelled), &payload, &parent_worker).await;
    let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
        return;
    };
    let directory = queue[pos].directory.clone();
    let frontend = queue[pos].frontend.clone();
    db.archive_job(job_id).await.ok();
    cleanup_job(
        &directory,
        &PathBuf::from("DB").join("saved_data").join(job_id.to_string()),
    )
    .await;
    queue.remove(pos);
    frontend.set_presence(presence_from_queue(queue)).await;
}

// Everything a Pandora Mini node has said since the last pass, plus the leases it has stopped
// saying anything about. Remote jobs progress only through here: they are skipped by every local
// dispatch, so this function is the whole of their lifecycle on this machine.
async fn do_link_things(db: &JobDb, queue: &mut Vec<Job>, shrine: &mut TypedShrine<WorkerMsg>) {
    use crate::pnworker::link::board::{self, LinkEvent};
    use crate::pnworker::link::spec::LinkOutcome;

    let settings = board::settings();
    let mut events = board::drain_events();
    // A node that has gone quiet takes the same path out as one that reported failure, so that
    // losing a machine and a machine losing a job are one code path and not two.
    for (job_id, node) in board::expire_leases(settings.lease_timeout_secs) {
        eprintln!("[link] {node} | job {job_id} lease expired; taking it back");
        events.push(LinkEvent::Lost { job_id, node });
    }

    for event in events {
        match event {
            LinkEvent::Reports {
                job_id,
                node,
                worker,
                reports,
            } => {
                apply_link_reports(db, queue, job_id, &node, &worker, reports).await;
            }
            LinkEvent::Finished {
                job_id,
                node,
                outcome,
                reason,
                reports,
                warnings,
            } => {
                if let Some(pos) = link_job_position(queue, job_id, &node) {
                    queue[pos].encode_warnings.extend(warnings);
                }
                apply_link_reports(db, queue, job_id, &node, "", reports).await;
                match outcome {
                    // A node that cannot run this job at all — a preset it does not have, an asset
                    // it cannot resolve. Nothing was attempted, so it costs no retry.
                    LinkOutcome::Declined => {
                        eprintln!(
                            "[link] {node} | job {job_id} declined ({}); running it here",
                            reason.unwrap_or_else(|| "no reason given".to_string())
                        );
                        requeue_link_job(db, queue, shrine, job_id, false).await;
                    }
                    // The node encoded and handed its output back. This is not a terminal state:
                    // the job resumes here at `Encoded` and takes the ordinary local upload path,
                    // which is what puts an HLS release on the one hostname that is public.
                    LinkOutcome::Returned => {
                        resume_returned_job(db, queue, job_id, &node).await;
                    }
                    // The probe answered. The job stays in the queue at `Probed` exactly as a
                    // local probe would, waiting for a file to be selected and archived by the
                    // probe timeout; only the lease is over.
                    LinkOutcome::Probed => {
                        release_probed_job(db, queue, job_id, &node).await;
                    }
                    _ => {
                        // Uploaded, Failed and Cancelled all arrive as reports carrying their own
                        // terminal stage, which `apply_link_reports` has already settled. Anything
                        // still in the queue reported no terminal payload, and is finished here so
                        // a job cannot outlive its lease.
                        settle_link_terminal(db, queue, job_id, &node, outcome, reason).await;
                    }
                }
            }
            LinkEvent::Lost { job_id, node } => {
                requeue_link_job(db, queue, shrine, job_id, true).await;
                let _ = node;
            }
        }
    }
}

fn link_job_position(queue: &[Job], job_id: u64, node: &str) -> Option<usize> {
    queue
        .iter()
        .position(|job| job.job_id == job_id && job.link_node.as_deref() == Some(node))
}

// Replays a node's worker output through the same chokepoint a local job uses. Forwarding the
// payload itself rather than a summary is what makes this possible: `persist_side_effects` writes
// the progress JSON the web console reads, the Drive helpers keep their deletion capability, and
// `render` localises the message against the job's own language rather than the node's.
async fn apply_link_reports(
    db: &JobDb,
    queue: &mut Vec<Job>,
    job_id: u64,
    node: &str,
    worker: &str,
    reports: Vec<crate::pnworker::link::spec::LinkReport>,
) {
    use crate::pnworker::link::spec::stage_from_name;

    for report in reports {
        let Some(pos) = link_job_position(queue, job_id, node) else {
            return;
        };
        let Some(payload) = report.payload.to_payload() else {
            eprintln!(
                "[link] {node} | job {job_id} sent message id {} this build cannot render",
                report.payload.id
            );
            continue;
        };
        let stage = report.stage.as_deref().and_then(stage_from_name);
        {
            let job = &mut queue[pos];
            if !worker.is_empty() {
                let label = crate::pnworker::link::coordinator::worker_label(node);
                if job.worker != label {
                    job.worker = label.clone();
                    db.update_worker(job_id, &label).await.ok();
                }
            }
            if let Some(stage) = stage {
                job.ready = stage;
                db.update_stage(job_id, stage).await.ok();
            }
            if stage == Some(Stage::Uploaded) {
                if let Some(acix) = job.acix.clone() {
                    if let Some(drive) = drive_link_from_payload(&payload) {
                        if drive.starts_with("http") {
                            let pending = crate::pnworker::acix::AcixPending::new(acix, drive);
                            if let Ok(encoded) = serde_json::to_string(&pending) {
                                db.set_acix_pending(job_id, &encoded).await.ok();
                            }
                        }
                    }
                }
            }
            persist_side_effects(db, job_id, &payload, stage, &job.encode_warnings).await;
            if let Err(error) = persist_job_drive_upload(job, &payload, stage).await {
                eprintln!(
                    "[drive-delete] failed to retain deletion capability for job {}: {}",
                    job_id, error
                );
            }
            persist_smartcode_drive_upload(job, &payload, stage).await;
            render(job, payload).await;
        }
        let terminal = queue
            .get(pos)
            .map(|job| matches!(job.ready, Stage::Uploaded | Stage::Failed | Stage::Cancelled))
            .unwrap_or(false);
        if terminal {
            finish_link_job(db, queue, job_id).await;
            return;
        }
    }
}

// A node reported a terminal outcome without a payload that carried it — it died mid-upload, or a
// build skew meant its final message could not be rendered here. The job still has to end.
async fn settle_link_terminal(
    db: &JobDb,
    queue: &mut Vec<Job>,
    job_id: u64,
    node: &str,
    outcome: crate::pnworker::link::spec::LinkOutcome,
    reason: Option<String>,
) {
    use crate::pnworker::link::spec::LinkOutcome;

    let Some(pos) = link_job_position(queue, job_id, node) else {
        return;
    };
    let (stage, payload) = match outcome {
        LinkOutcome::Cancelled => (
            Stage::Cancelled,
            MessagePayload::Static(crate::pnworker::messages::JOB_CANCELLED),
        ),
        _ => (
            Stage::Failed,
            MessagePayload::Progress(
                crate::pnworker::messages::ENCODE_FAIL,
                vec![reason.unwrap_or_else(|| format!("node {node} reported no outcome"))],
            ),
        ),
    };
    queue[pos].ready = stage;
    db.update_stage(job_id, stage).await.ok();
    render(&mut queue[pos], payload).await;
    finish_link_job(db, queue, job_id).await;
}

// A node has streamed its finished encode into this job's work directory. Clearing `link_node`
// hands the job back to the local pipeline, which picks it up at `Encoded` and dispatches the
// upload exactly as it would for something encoded here.
async fn resume_returned_job(db: &JobDb, queue: &mut Vec<Job>, job_id: u64, node: &str) {
    crate::pnworker::link::board::release(job_id);
    let Some(pos) = link_job_position(queue, job_id, node) else {
        return;
    };
    let output = queue[pos].directory.join("work").join("output.mp4");
    let delivered = tokio::fs::metadata(&output)
        .await
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false);
    if !delivered {
        // The node said it sent the output and there is nothing there. Requeueing would re-run the
        // whole encode on a machine that has just proved it cannot deliver, so this ends here.
        eprintln!("[link] {node} | job {job_id} reported a returned output that is not on disk");
        queue[pos].ready = Stage::Failed;
        db.update_stage(job_id, Stage::Failed).await.ok();
        let payload = MessagePayload::Progress(
            crate::pnworker::messages::ENCODE_FAIL,
            vec![format!("node {node} returned no output")],
        );
        render(&mut queue[pos], payload).await;
        finish_link_job(db, queue, job_id).await;
        return;
    }
    let job = &mut queue[pos];
    job.link_node = None;
    job.link_return_output = false;
    job.ready = Stage::Encoded;
    job.worker = "upl-pending".to_string();
    db.update_stage(job_id, Stage::Encoded).await.ok();
    db.update_worker(job_id, &job.worker).await.ok();
    println!("[link] {node} | job {job_id} output received; publishing here");
}

// Hands a finished probe back to the local queue without ending it. Clearing `link_node` is what
// lets `do_probe_timeout_things` own the rest of its life, and releasing the lease is what stops
// the watchdog from reclaiming a job that was never lost.
async fn release_probed_job(db: &JobDb, queue: &mut Vec<Job>, job_id: u64, node: &str) {
    crate::pnworker::link::board::release(job_id);
    let Some(pos) = link_job_position(queue, job_id, node) else {
        return;
    };
    queue[pos].link_node = None;
    queue[pos].ready = Stage::Probed;
    db.update_stage(job_id, Stage::Probed).await.ok();
    println!("[link] {node} | job {job_id} probed; waiting on a file selection here");
}

async fn finish_link_job(db: &JobDb, queue: &mut Vec<Job>, job_id: u64) {
    crate::pnworker::link::board::release(job_id);
    let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
        return;
    };
    let directory = queue[pos].directory.clone();
    let frontend = queue[pos].frontend.clone();
    let probe_job_id = queue[pos].probe_job_id;
    if let Some(probe_id) = probe_job_id {
        if let Some(probe_pos) = queue.iter().position(|job| job.job_id == probe_id) {
            let probe_directory = queue[probe_pos].directory.clone();
            cleanup_job(
                &probe_directory,
                &PathBuf::from("DB").join("saved_data").join(probe_id.to_string()),
            )
            .await;
            db.archive_job(probe_id).await.ok();
            queue.remove(probe_pos);
        }
    }
    db.archive_job(job_id).await.ok();
    cleanup_job(
        &directory,
        &PathBuf::from("DB").join("saved_data").join(job_id.to_string()),
    )
    .await;
    queue.retain(|job| job.job_id != job_id);
    frontend.set_presence(presence_from_queue(queue)).await;
}

// A job whose node was lost, or which the node refused. It goes back through the ordinary local
// queue path — the source is a link and the subtitle is bytes, so nothing about a remote job is
// unreproducible, and requeueing is always cheaper than trying to recover it.
async fn requeue_link_job(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    job_id: u64,
    spend_attempt: bool,
) {
    crate::pnworker::link::board::release(job_id);
    let Some(pos) = queue.iter().position(|job| job.job_id == job_id && job.link_node.is_some())
    else {
        return;
    };
    let mut job = queue.remove(pos);
    job.link_node = None;
    job.link_pin = None;
    if spend_attempt {
        job.link_attempts = job.link_attempts.saturating_add(1);
    }
    job.ready = Stage::Queued;
    job.encode_dispatched = false;
    job.encode_dispatch_order = None;
    job.encode_dispatched_at = None;
    job.encode_last_frame_at = None;
    db.update_stage(job_id, Stage::Queued).await.ok();
    println!("[link] job {job_id} requeued locally (attempt {})", job.link_attempts);
    if !queue_new_job(db, queue, shrine, &mut job).await {
        queue.push(job);
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
            let frontend = queue[pos].frontend.clone();

            frontend.delete().await;

            cleanup_job(
                &directory,
                &PathBuf::from("DB").join("saved_data").join(id.to_string()),
            )
            .await;
            db.archive_job(id).await.ok();
            queue.remove(pos);
            frontend.set_presence(presence_from_queue(queue)).await;
        }
    }
}

// A dispatched encode that has gone this long without a progress frame is not slow, it is wedged:
// the layer took the message and stopped talking. The heartbeat watchdog reboots such a layer and
// `reset_encode_dispatches_after_reboot` hands the job straight back to the new one, so on its own
// that pair retries a poisoned job forever — silently, with no terminal state and no message. This
// is the thing that turns one stuck job into a bot that looks frozen.
const ENCODE_STALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn encode_stalled(job: &Job, now: Duration) -> bool {
    if job.forward_parent.is_some()
        || !is_encode_job_type(job.job_type)
        || !job.encode_dispatched
        || !matches!(job.ready, Stage::Downloaded | Stage::Encoding)
    {
        return false;
    }
    // Before the first frame the dispatch itself is the clock; after it, every frame resets it.
    let last = job
        .encode_last_frame_at
        .or(job.encode_dispatched_at)
        .unwrap_or(job.requested_at);
    now.saturating_sub(last) > ENCODE_STALL_TIMEOUT
}

async fn do_encode_stall_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) {
    let now = unix_now();
    let stalled: Vec<u64> = queue
        .iter()
        .filter(|job| encode_stalled(job, now))
        .map(|job| job.job_id)
        .collect();
    if stalled.is_empty() {
        return;
    }
    // The worker is what is stuck, not only this job, so restart it before anything else is
    // dispatched into the same silent task. `kill_on_drop` in `run_tool` takes its tool down with it.
    eprintln!("[Pandora] encode stall detected — rebooting the Encode layer");
    shrine.force_reboot(&Worker::Encode).await;
    let minutes = (ENCODE_STALL_TIMEOUT.as_secs() / 60).to_string();
    for job_id in stalled {
        let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
            continue;
        };
        eprintln!(
            "[Pandora] job {} made no encode progress for {} minutes — failing it",
            job_id, minutes
        );
        let payload = MessagePayload::Progress(ENCODE_STALLED, vec![minutes.clone()]);
        queue[pos].ready = Stage::Failed;
        queue[pos].encode_dispatched = false;
        queue[pos].encode_dispatch_order = None;
        db.update_stage(job_id, Stage::Failed).await.ok();
        if let Some(keep) = &queue[pos].keep {
            mark_output_failed(&scope(queue[pos].server_id), keep).await.ok();
        }
        persist_side_effects(
            db,
            job_id,
            &payload,
            Some(Stage::Failed),
            &queue[pos].encode_warnings,
        )
        .await;
        render(&mut queue[pos], payload.clone()).await;
        let parent_worker = forwarded_worker_for(&queue[pos].worker);
        sync_forwarded_jobs(db, queue, job_id, Some(Stage::Failed), &payload, &parent_worker).await;
        let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
            continue;
        };
        let directory = queue[pos].directory.clone();
        let frontend = queue[pos].frontend.clone();
        if let Some(parent_id) = queue[pos].batch_parent {
            update_batch_parent(db, queue, parent_id, job_id, Stage::Failed).await;
        }
        db.archive_job(job_id).await.ok();
        cleanup_job(
            &directory,
            &PathBuf::from("DB")
                .join("saved_data")
                .join(job_id.to_string()),
        )
        .await;
        queue.retain(|job| job.job_id != job_id);
        frontend.set_presence(presence_from_queue(queue)).await;
    }
}

async fn do_worker_message_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) -> bool {
    let Some((_, commdata)) = shrine.receive(100).await else {
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
                    for child in queue
                        .iter_mut()
                        .filter(|j| j.forward_parent == Some(parent_id))
                    {
                        if !child.encode_warnings.iter().any(|w| w == &warning) {
                            child.encode_warnings.push(warning.clone());
                        }
                    }
                }
                return true;
            }
            if *id == ENCODE_PROG {
                queue[pos].encode_frame = args.get(1).and_then(|s| s.parse().ok());
                queue[pos].encode_total = args.get(2).and_then(|s| s.parse().ok());
                queue[pos].encode_fps = args.get(3).and_then(|s| s.parse().ok());
                queue[pos].encode_last_frame_at = Some(unix_now());
            }
            if *id == ENCODE_CONCAT_PROG {
                queue[pos].encode_frame = args.get(0).and_then(|s| s.parse().ok());
                queue[pos].encode_total = args.get(1).and_then(|s| s.parse().ok());
                queue[pos].encode_fps = args.get(2).and_then(|s| s.parse().ok());
                queue[pos].encode_last_frame_at = Some(unix_now());
            }
            if *id == TORRENT_FILE_DONE {
                let index = args.get(0).and_then(|value| value.parse::<u64>().ok());
                let relative = args.get(1).cloned().unwrap_or_default();
                if let Some(index) = index {
                    spawn_batch_child(db, queue, pos, index, &relative).await;
                }
                return true;
            }
            if *id == TORRENT_DUPLICATE_WAIT {
                if let Some(path) = args.get(0) {
                    queue[pos].duplicate_source = Some(duplicate_path_to_container(path));
                }
                let v = serde_json::json!({ "type": "download", "waiting": "cache" });
                db.update_progress(queue[pos].job_id, &v.to_string())
                    .await
                    .ok();
                let payload = commdata.1;
                render(&mut queue[pos], payload).await;
                return true;
            }
        }

        let payload = commdata.1.clone();
        let stage = commdata.2;
        let mut finished_job: Option<(u64, Option<u64>, PathBuf)> = None;
        let queue_total = queue.len();

        {
            let i = &mut queue[pos];
            if let Some(a) = stage {
                let previous_ready = i.ready;
                i.ready = a;
                db.update_stage(i.job_id, i.ready).await.ok();
                if a == Stage::Encoding {
                    i.frontend
                        .set_presence(Presence::Encoding {
                            idx: pos,
                            total: queue_total,
                        })
                        .await;
                } else {
                    i.encode_dispatched = false;
                    i.encode_dispatch_order = None;
                }
                if a == Stage::Uploaded && previous_ready != Stage::Uploaded {
                    i.frontend.ghost_ping(i.author).await;
                }
                if a == Stage::Encoded
                    || (a == Stage::Downloaded && i.job_type == JobType::Preview)
                    || (a == Stage::Cancelled && past_downloaded(previous_ready))
                {
                    cache_encode_input(i).await;
                }
                if matches!(a, Stage::Failed | Stage::Cancelled) {
                    if let Some(keep) = &i.keep {
                        mark_output_failed(&scope(i.server_id), keep).await.ok();
                    }
                }
            }
            if stage == Some(Stage::Uploaded) {
                if let Some(acix) = i.acix.clone() {
                    if let Some(drive) = drive_link_from_payload(&payload) {
                        if drive.starts_with("http") {
                            let pending = crate::pnworker::acix::AcixPending::new(acix, drive);
                            if let Ok(j) = serde_json::to_string(&pending) {
                                db.set_acix_pending(i.job_id, &j).await.ok();
                            }
                        }
                    }
                }
            }
            persist_side_effects(db, i.job_id, &payload, stage, &i.encode_warnings).await;
            if let Err(error) = persist_job_drive_upload(i, &payload, stage).await {
                eprintln!(
                    "[drive-delete] failed to retain deletion capability for job {}: {}",
                    i.job_id, error
                );
            }
            persist_smartcode_drive_upload(i, &payload, stage).await;
            // PREVIEW_DONE attaches files from the work dir here; cleanup below must remain after render.
            render(i, payload.clone()).await;

            let finished = matches!(i.ready, Stage::Uploaded | Stage::Failed | Stage::Cancelled);
            if finished {
                finished_fe = Some(i.frontend.clone());
                finished_job = Some((i.job_id, i.probe_job_id, i.directory.clone()));
            }
        }

        if let (Some(parent_id), Some(stage)) = (queue[pos].batch_parent, stage) {
            let child_id = queue[pos].job_id;
            update_batch_parent(db, queue, parent_id, child_id, stage).await;
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
                    db.archive_job(probe_id).await.ok();
                    queue.remove(probe_pos);
                }
            }
            db.archive_job(job_id).await.ok();
            // PREVIEW_DONE attachments are uploaded during render above before this removes the work dir.
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

// One file of a batch has landed. It becomes an encode job right away rather than waiting for the
// rest of the torrent, which is the whole point of downloading the selection in one process.
async fn spawn_batch_child(
    db: &JobDb,
    queue: &mut Vec<Job>,
    pos: usize,
    file_index: u64,
    relative: &str,
) {
    let parent = queue[pos].clone();
    let Some(batch) = parent.batch.clone() else {
        return;
    };
    let Some(entry) = batch.entry_for(file_index).cloned() else {
        return;
    };
    if entry.job_id.is_some() {
        return;
    }
    let source = parent
        .directory
        .join("contents")
        .join("torrent")
        .join(relative);
    let Some(mut child) = build_batch_child(&parent, &entry, &source).await else {
        if let Some(batch) = queue[pos].batch.as_mut() {
            batch.failed += 1;
        }
        persist_batch_progress(db, &queue[pos]).await;
        render_batch_parent(&mut queue[pos]).await;
        return;
    };
    // With an output page the batch speaks through one message; without one, every episode needs
    // somewhere of its own to report.
    if !batch_page_available() {
        child.frontend = parent.frontend.spawn_child_message("...").await;
    }
    if let Err(e) = db.insert_job(&child).await {
        eprintln!("[Pandora Batch] child {} insert failed: {}", child.job_id, e);
        remove_dir_all(&child.directory).await.ok();
        if let Some(batch) = queue[pos].batch.as_mut() {
            batch.failed += 1;
        }
        persist_batch_progress(db, &queue[pos]).await;
        render_batch_parent(&mut queue[pos]).await;
        return;
    }
    let child_id = child.job_id;
    render(
        &mut child,
        MessagePayload::Progress(
            crate::pnworker::messages::BATCH_EPISODE,
            vec![parent.job_id.to_string(), entry.file_label.clone()],
        ),
    )
    .await;
    if let Some(batch) = queue[pos].batch.as_mut() {
        if let Some(entry) = batch
            .entries
            .iter_mut()
            .find(|entry| entry.file_index == file_index)
        {
            entry.job_id = Some(child_id);
        }
        batch.current = entry.file_label.clone();
    }
    persist_batch_progress(db, &queue[pos]).await;
    render_batch_parent(&mut queue[pos]).await;
    queue.push(child);
}

async fn render_batch_parent(job: &mut Job) {
    let Some(batch) = job.batch.clone() else {
        return;
    };
    let current = if batch.current.is_empty() {
        "-".to_string()
    } else {
        batch.current.clone()
    };
    render(
        job,
        MessagePayload::Progress(
            crate::pnworker::messages::BATCH_PROG,
            vec![
                (batch.finished + batch.failed).to_string(),
                batch.total().to_string(),
                current,
            ],
        ),
    )
    .await;
}

// A child's stage change is the parent's only progress signal, so the parent is re-rendered on the
// transitions worth a message edit rather than on every encode progress tick.
async fn update_batch_parent(
    db: &JobDb,
    queue: &mut Vec<Job>,
    parent_id: u64,
    child_id: u64,
    stage: Stage,
) {
    let Some(pos) = queue.iter().position(|job| job.job_id == parent_id) else {
        return;
    };
    let Some(batch) = queue[pos].batch.as_mut() else {
        return;
    };
    let label = batch.label_for_job(child_id);
    match stage {
        Stage::Uploaded => batch.finished += 1,
        Stage::Failed | Stage::Cancelled | Stage::Declined => batch.failed += 1,
        _ => {}
    }
    if !label.is_empty() && matches!(stage, Stage::Encoding | Stage::Uploading) {
        batch.current = label;
    }
    persist_batch_progress(db, &queue[pos]).await;
    render_batch_parent(&mut queue[pos]).await;
}

// The parent outlives its download: it stays in the queue reporting the children until the last
// episode is terminal, then reports the batch as a whole.
async fn do_batch_parent_things(db: &JobDb, queue: &mut Vec<Job>) {
    let settled: Vec<u64> = queue
        .iter()
        .filter(|job| {
            job.ready == Stage::Downloaded
                && job
                    .batch
                    .as_ref()
                    .is_some_and(|batch| !batch.download_settled)
        })
        .map(|job| job.job_id)
        .collect();
    for job_id in settled {
        let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
            continue;
        };
        if queue[pos]
            .batch
            .as_mut()
            .is_some_and(|batch| batch.settle_download())
        {
            persist_batch_progress(db, &queue[pos]).await;
        }
    }
    let complete: Vec<u64> = queue
        .iter()
        .filter(|job| {
            job.batch
                .as_ref()
                .is_some_and(|batch| batch.complete() && !batch.entries.is_empty())
                && matches!(job.ready, Stage::Downloaded | Stage::Downloading)
        })
        .map(|job| job.job_id)
        .collect();
    for job_id in complete {
        let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
            continue;
        };
        let Some(batch) = queue[pos].batch.clone() else {
            continue;
        };
        queue[pos].ready = Stage::Uploaded;
        queue[pos].worker = "que-main".to_string();
        db.update_stage(job_id, Stage::Uploaded).await.ok();
        db.update_worker(job_id, "que-main").await.ok();
        persist_batch_progress(db, &queue[pos]).await;
        render(
            &mut queue[pos],
            MessagePayload::Progress(
                crate::pnworker::messages::BATCH_DONE,
                vec![batch.finished.to_string(), batch.total().to_string()],
            ),
        )
        .await;
        let directory = queue[pos].directory.clone();
        let frontend = queue[pos].frontend.clone();
        db.archive_job(job_id).await.ok();
        cleanup_job(
            &directory,
            &PathBuf::from("DB").join("saved_data").join(job_id.to_string()),
        )
        .await;
        queue.retain(|job| job.job_id != job_id);
        frontend.set_presence(presence_from_queue(queue)).await;
    }
}

async fn requeue_duplicate_waiter(db: &JobDb, job: &mut Job) {
    job.duplicate_source = None;
    job.ready = Stage::Queued;
    job.worker = "dwl-pending".to_string();
    db.update_stage(job.job_id, Stage::Queued).await.ok();
    db.update_worker(job.job_id, &job.worker).await.ok();
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
                    .ok();
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
            if let Some(owner) = duplicate_source_owner(queue, &source_dir) {
                if owner.job_id != queue[pos].job_id && !jobs_share_input(&queue[pos], owner) {
                    if owner.ready == Stage::Downloading {
                        continue;
                    }
                    requeue_duplicate_waiter(db, &mut queue[pos]).await;
                    continue;
                }
            }
            if !duplicate_source_ready(queue, &source_dir) {
                if duplicate_source_orphaned(queue, &source_dir) {
                    eprintln!(
                        "[Pandora] duplicate source for {} vanished; requeueing for download",
                        id
                    );
                    // Duplicate waiters are only created for encode/pancode/backup downloads.
                    requeue_duplicate_waiter(db, &mut queue[pos]).await;
                }
                continue;
            }
            let source = duplicate_input_path(&source_dir);
            let target_dir = queue[pos].directory.join("contents").join("torrent");
            let target = target_dir.join("input.mkv");
            if let Err(e) = create_dir_all(&target_dir).await {
                eprintln!(
                    "[Pandora] duplicate cache target setup failed for {}: {}",
                    id, e
                );
                continue;
            }
            match tokio::fs::copy(&source, &target).await {
                Ok(_) => {
                    queue[pos].duplicate_source = None;
                    queue[pos].ready = Stage::Downloaded;
                    db.update_stage(queue[pos].job_id, Stage::Downloaded)
                        .await
                        .ok();
                    let v =
                        serde_json::json!({ "type": "download", "percent": 100, "cached": true });
                    db.update_progress(queue[pos].job_id, &v.to_string())
                        .await
                        .ok();
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

async fn do_queued_download_waiting_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) {
    let waiting: Vec<u64> = queue
        .iter()
        .filter(|j| {
            j.forward_parent.is_none()
                && j.ready == Stage::Queued
                && j.worker == "dwl-pending"
                && matches!(
                    j.job_type,
                    JobType::Encode | JobType::Pancode | JobType::Backup | JobType::Preview
                )
        })
        .map(|j| j.job_id)
        .collect();
    let mut dead = Vec::new();
    for id in waiting {
        let Some(pos) = queue.iter().position(|j| j.job_id == id) else {
            continue;
        };
        if active_torrent_download_source(&queue[pos], queue).is_some() {
            continue;
        }
        let snapshot = queue.clone();
        let file_index = queue[pos].probe_file_index;
        if queue_download_job(db, &snapshot, shrine, &mut queue[pos], file_index.into_iter().collect(), false).await {
            dead.push(id);
        }
    }
    queue.retain(|j| !dead.contains(&j.job_id));
}

async fn do_job_progression_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
    next_encode_dispatch_order: &mut u64,
    encodes_since_batch: &mut u64,
) {
    let qlen = queue.len();
    // A batch child holds the encoder alone, and only takes it once two ordinary encodes that were
    // ready to run have gone ahead of it.
    let mut batch_in_flight = queue.iter().any(|job| {
        job.batch_parent.is_some()
            && job.encode_dispatched
            && matches!(job.ready, Stage::Downloaded | Stage::Encoding)
    });
    let others_waiting = queue.iter().any(|job| {
        job.batch_parent.is_none()
            && job.batch.is_none()
            && job.forward_parent.is_none()
            && is_encode_job_type(job.job_type)
            && !job.encode_dispatched
            && (job.ready == Stage::Downloaded
                || (job.job_type == JobType::Keycode && job.ready == Stage::Queued))
    });
    let mut dead: Vec<u64> = vec![];
    let mut forwarded_state_updates: Vec<(u64, Stage, String)> = vec![];
    let mut active_encode_sources: HashMap<String, PathBuf> = HashMap::new();
    for j in queue
        .iter()
        .filter(|j| {
            j.forward_parent.is_none()
                // A leased job's input was downloaded on the node, so this machine's copy of its
                // work directory is empty. Offering it as a duplicate source would hand another
                // job a path with no video behind it.
                && j.link_node.is_none()
                && j.job_type != JobType::Preview
                && (j.ready == Stage::Encoding
                    || (j.ready == Stage::Downloaded && j.encode_dispatched))
        })
    {
        for key in input_cache_keys(j) {
            active_encode_sources
                .entry(key)
                .or_insert_with(|| j.directory.join("contents").join("torrent"));
        }
    }
    let mut active_encode_parents: HashMap<String, (u64, Stage, String)> = HashMap::new();
    for j in queue.iter().filter(|j| {
        j.forward_parent.is_none()
            && is_forwardable_encode(j)
            && (matches!(
                    j.ready,
                    Stage::Queued
                        | Stage::Downloading
                        | Stage::Encoding
                        | Stage::Encoded
                        | Stage::Uploading
                )
                || (j.ready == Stage::Downloaded && j.encode_dispatched)
            )
    }) {
        for key in encode_forward_keys(j) {
            active_encode_parents.entry(key).or_insert((
                j.job_id,
                j.ready,
                forwarded_worker_for(&j.worker),
            ));
        }
    }
    for (idx, job) in queue.iter_mut().enumerate() {
        if job.forward_parent.is_some() {
            continue;
        }
        // A leased job is executing on another machine. Every stage it reaches arrives through
        // `do_link_things`; dispatching anything for it here would put two encoders on one job.
        if job.link_node.is_some() {
            continue;
        }
        // A batch parent never encodes anything itself; its download feeds the child jobs and
        // do_batch_parent_things owns the rest of its life.
        if job.batch.is_some() {
            continue;
        }
        if job.ready == Stage::Probed {
            continue;
        }
        if job.job_type == JobType::Keycode && job.ready == Stage::Queued && !job.encode_dispatched {
            match try_dispatch_keycode(db, shrine, job, next_encode_dispatch_order).await {
                KeycodeDispatch::Waiting => continue,
                KeycodeDispatch::Dispatched => {
                    continue;
                }
                KeycodeDispatch::Failed => {
                    dead.push(job.job_id);
                    continue;
                }
            }
        }

        if job.ready == Stage::Downloaded {
            if job.job_type == JobType::Subs {
                job.worker = "prw-pending".to_string();
                db.update_worker(job.job_id, &job.worker).await.ok();
                if !dispatch_or_kill(
                    shrine,
                    &Worker::Probe,
                    WorkerMsg::Subs((job.directory.clone(), job.job_id)),
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
                db.update_stage(job.job_id, Stage::Encoding).await.ok();
                job.frontend
                    .set_presence(Presence::Encoding { idx, total: qlen })
                    .await;
            } else if job.job_type == JobType::Preview {
                let Some(preview) = job.preview.clone() else {
                    job.ready = Stage::Failed;
                    db.update_stage(job.job_id, Stage::Failed).await.ok();
                    render(
                        job,
                        MessagePayload::Progress(
                            crate::pnworker::messages::PREVIEW_FAIL,
                            vec!["missing preview request".to_string()],
                        ),
                    )
                    .await;
                    dead.push(job.job_id);
                    continue;
                };
                job.worker = "prw-pending".to_string();
                db.update_worker(job.job_id, &job.worker).await.ok();
                if !dispatch_or_kill(
                    shrine,
                    &Worker::Probe,
                    WorkerMsg::Preview((
                        job.directory.clone(),
                        preview.shots,
                        preview.watermark_font,
                        preview.ranking_log,
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
                db.update_stage(job.job_id, Stage::Encoding).await.ok();
                job.frontend
                    .set_presence(Presence::Encoding { idx, total: qlen })
                    .await;
            } else if job.job_type == JobType::Backup {
                if job.keep.is_some() {
                    if finish_keep_job(db, job, KeepKind::Backup).await {
                        dead.push(job.job_id);
                    }
                    continue;
                }
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
                        job.channel_id,
                        job.server_id,
                        None,
                        None,
                        None,
                        false,
                        job.link_drive_only,
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
                db.update_stage(job.job_id, Stage::Uploading).await.ok();
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
                db.update_stage(job.job_id, Stage::Uploading).await.ok();
                job.frontend
                    .set_presence(Presence::Uploading { idx, total: qlen })
                    .await;
            } else {
                if job.encode_dispatched {
                    continue;
                }
                if job_cancelled(&job.directory) {
                    continue;
                }
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
                let cache_keys = input_cache_keys(job);
                if let Some(source) = cache_keys
                    .iter()
                    .find_map(|key| active_encode_sources.get(key).cloned())
                {
                    job.duplicate_source = Some(source.clone());
                    job.ready = Stage::Downloading;
                    job.worker = "dwl-cache".to_string();
                    let v = serde_json::json!({ "type": "download", "waiting": "cache" });
                    db.update_stage(job.job_id, Stage::Downloading)
                        .await
                        .ok();
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
                if job.batch_parent.is_some()
                    && !batch_child_may_dispatch(
                        batch_in_flight,
                        others_waiting,
                        *encodes_since_batch,
                    )
                {
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
                        job.server_watermark.clone(),
                        job.smartcode_drive_name.is_some(),
                        // The same three conditions the upload worker publishes HLS under, asked
                        // before the encode instead of after it: a kept job needs its MP4 on disk,
                        // and a Dummy encode is never released.
                        job.keep.is_none()
                            && !matches!(job.preset, Preset::Dummy(_))
                            && crate::pnworker::server_config::server_hls_enabled(job.server_id)
                                .await,
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
                mark_encode_dispatched(
                    job,
                    next_encode_dispatch_order,
                    shrine.reboot_epoch(&Worker::Encode),
                );
                if job.batch_parent.is_some() {
                    batch_in_flight = true;
                    *encodes_since_batch = 0;
                } else {
                    *encodes_since_batch = encodes_since_batch.saturating_add(1);
                }
                for key in cache_keys {
                    active_encode_sources
                        .entry(key)
                        .or_insert_with(|| job.directory.join("contents").join("torrent"));
                }
                for key in encode_forward_keys(job) {
                    active_encode_parents.entry(key).or_insert((
                        job.job_id,
                        Stage::Downloaded,
                        forwarded_worker_for(&job.worker),
                    ));
                }
            }
        } else if job.ready == Stage::Encoded {
            if job.keep.is_some() {
                if finish_keep_job(db, job, KeepKind::Encode).await {
                    dead.push(job.job_id);
                }
                continue;
            }
            // A node encoding for an HLS-only server stops here: the output is the coordinator's
            // to publish, not this machine's to upload. The job waits until the link client has
            // handed the file over, and only then is its work directory allowed to go.
            if job.link_return_output {
                if crate::pnworker::link::client::output_returned(job.job_id) {
                    db.update_stage(job.job_id, Stage::Uploaded).await.ok();
                    db.archive_job(job.job_id).await.ok();
                    cleanup_job(
                        &job.directory,
                        &PathBuf::from("DB")
                            .join("saved_data")
                            .join(job.job_id.to_string()),
                    )
                    .await;
                    dead.push(job.job_id);
                }
                continue;
            }
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
                    if matches!(job.job_type, JobType::Keycode | JobType::Studio) {
                        true
                    } else {
                        match job.preset {
                            Preset::Dummy(_) => false,
                            _ => true,
                        }
                    },
                    job.job_id,
                    job.channel_id,
                    job.server_id,
                    job.gdrive_folder_global.clone(),
                    job.gdrive_folder_local.clone(),
                    job.smartcode_drive_name.clone(),
                    drive_deletable_job_type(job.job_type),
                    job.link_drive_only,
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
            db.update_stage(job.job_id, Stage::Uploading).await.ok();
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

fn mark_encode_dispatched(job: &mut Job, next_encode_dispatch_order: &mut u64, epoch: u32) {
    job.encode_dispatched = true;
    job.encode_dispatch_order = Some(*next_encode_dispatch_order);
    job.encode_dispatched_at = Some(unix_now());
    job.encode_last_frame_at = None;
    job.encode_dispatch_epoch = epoch;
    *next_encode_dispatch_order = next_encode_dispatch_order.saturating_add(1);
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
}

async fn finish_keep_job(db: &JobDb, job: &mut Job, kind: KeepKind) -> bool {
    let Some(keep) = job.keep.clone() else {
        return false;
    };
    let source = match kind {
        KeepKind::Encode => job.directory.join("work").join("output.mp4"),
        KeepKind::Backup => job
            .directory
            .join("contents")
            .join("torrent")
            .join("input.mkv"),
    };
    let meta = match store_output(
        &scope(job.server_id),
        kind,
        &keep,
        source,
        if kind == KeepKind::Encode {
            Some(&job.preset)
        } else {
            None
        },
        job.job_id,
    )
    .await
    {
        Ok(meta) => meta,
        Err(e) => {
            eprintln!("[Pandora] keep store failed for {}: {}", job.job_id, e);
            job.ready = Stage::Failed;
            db.update_stage(job.job_id, Stage::Failed).await.ok();
            render(
                job,
                MessagePayload::Progress(crate::pnworker::messages::KEEP_FAIL, vec![e]),
            )
            .await;
            return true;
        }
    };
    let progress = serde_json::json!({
        "type": "keep",
        "keyword": meta.keyword,
        "parent_keyword": meta.parent_keyword,
        "kind": meta.kind.label(),
        "expires_at": meta.expires_at,
        "ready": true,
    });
    db.update_progress(job.job_id, &progress.to_string())
        .await
        .ok();
    job.ready = Stage::Uploaded;
    job.worker = "keep-done".to_string();
    db.update_worker(job.job_id, &job.worker).await.ok();
    db.update_stage(job.job_id, Stage::Uploaded).await.ok();
    render(
        job,
        MessagePayload::Progress(
            crate::pnworker::messages::KEEP_DONE,
            vec![
                meta.keyword.clone(),
                meta.parent_keyword.clone(),
                meta.kind.label().to_string(),
            ],
        ),
    )
    .await;
    db.archive_job(job.job_id).await.ok();
    cleanup_job(
        &job.directory,
        &PathBuf::from("DB")
            .join("saved_data")
            .join(job.job_id.to_string()),
    )
    .await;
    true
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
    PseudoLossless(Option<String>),
    Dummy(Option<String>),
    Standard(Option<String>),
    VerySlow(Option<String>),
    Gpu(Option<String>),
    // Standard x264 settings at a capped frame height. API-only: no /edit choice offers them.
    Hd720(Option<String>),
    Sd480(Option<String>),
    Copy,
}

impl Preset {
    // The canonical preset name — the same one `preset_from_name` accepts, the file name under
    // `DB/config/global/presets/`, and what `pnmpeg --preset` is given. `Copy` has none: it is a
    // stream copy, not an encode, and there is no preset to look up for it.
    pub fn name(&self) -> Option<&'static str> {
        Some(match self {
            Preset::PseudoLossless(_) => "pseudolossless",
            Preset::Dummy(_) => "dummy",
            Preset::Standard(_) => "standard",
            Preset::VerySlow(_) => "veryslow",
            Preset::Gpu(_) => "gpu",
            Preset::Hd720(_) => "720p",
            Preset::Sd480(_) => "480p",
            Preset::Copy => return None,
        })
    }
}

// What, if anything, the download worker should start encoding before the download finishes.
//
// This asks the resolved preset rather than matching on the variant, which is what lets a preset
// file decide it. The variant list this replaced was a third copy of the same table — pnmpeg had
// two of its own — and the copies could only ever agree about the built-ins: a preset file that
// turned encoding-ahead on got a coordinator that never started it, and one that turned it off got
// a speculative encode nothing would adopt.
pub fn download_aot_for(preset: &Preset) -> Option<DownloadAot> {
    let resolved = crate::lib::mpeg::preset::resolve(preset.name()?)?;
    if resolved.wants_chunked_encode() {
        return Some(DownloadAot { preset: resolved.name, chunked: true });
    }
    resolved
        .wants_linear_aot()
        .then(|| DownloadAot { preset: resolved.name, chunked: false })
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeepKind {
    Encode,
    Backup,
}

impl KeepKind {
    pub fn label(self) -> &'static str {
        match self {
            KeepKind::Encode => "encode",
            KeepKind::Backup => "backup",
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeepRequest {
    pub keyword: Option<String>,
    pub parent_keyword: Option<String>,
    pub output_keyword: Option<String>,
}

impl KeepRequest {
    pub fn new(keyword: Option<String>) -> Self {
        Self {
            keyword,
            parent_keyword: None,
            output_keyword: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeycodeRequest {
    pub keywords: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub shots: Vec<(u64, String)>,
    pub watermark_font: Option<PathBuf>,
    pub ranking_log: String,
}

#[derive(Clone, Debug)]
pub struct StudioJobRequest {
    pub manifest: PathBuf,
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u16)]
pub enum JobType {
    Encode = 001,
    Cancel = 002,
    Hearts = 003,
    Workers = 012,
    GitSync = 004,
    Probe = 005,
    Pancode = 006,
    Scrape = 007,
    Backup = 008,
    BackupAll = 009,
    Keycode = 010,
    GitQuery = 011,
    Preview = 013,
    Studio = 014,
    StudioPreview = 015,
    Batch = 016,
    Subs = 017,
    GitForce = 018,
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
    DriveDelete(DriveDeleteRequest),
}

#[derive(Clone, Debug)]
pub struct DriveDeleteRequest {
    pub author: u64,
    pub channel_id: u64,
    pub job_id: u64,
    pub is_witch: bool,
}

impl DriveDeleteRequest {
    pub fn new(author: u64, channel_id: u64, job_id: u64, is_witch: bool) -> Self {
        Self { author, channel_id, job_id, is_witch }
    }

    fn may_delete(&self, job_author: u64) -> bool {
        self.is_witch || self.author == job_author
    }
}

#[derive(Clone)]
pub struct HalfJob {
    pub author: u64,
    pub channel_id: u64,
    pub requested_at: Duration,
    pub job_id: u64,
    pub job_type: JobType,
    pub frontend: Frontend,
    // A cancel normally has to come from the job's own author, which is what keeps one user's ❌
    // off another user's encode. A Witch's 🔪 is the deliberate exception: it retracts the bot's
    // message whoever asked for it, so it has to be able to stop the job behind it too. Read only
    // by JobType::Cancel.
    pub any_author: bool,
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
            any_author: false,
        }
    }
    // The Witch knife's cancel: same message id, without the author match that makes ❌ personal.
    pub fn new_cancel_any(author: u64, channel_id: u64, job_id: u64) -> Self {
        Self {
            any_author: true,
            ..Self::new_cancel(author, channel_id, job_id)
        }
    }
    // ❌ only stops the job its own author asked for, so one user's reaction cannot end another's
    // encode. A Witch's 🔪 is the one request allowed past that.
    pub fn may_cancel(&self, job_author: u64) -> bool {
        self.any_author || self.author == job_author
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
            any_author: false,
        }
    }
    pub fn new_workers(
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
            job_type: JobType::Workers,
            frontend: Frontend::discord(context, msg),
            any_author: false,
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
            any_author: false,
        }
    }
    // `/gitforce`. A separate job type rather than a flag on the gitsync one, because the two
    // differ in what they are allowed to destroy: this one resets a checkout and restarts a whole
    // cluster, and nothing should be able to reach it by passing the wrong boolean.
    pub fn new_gitforce(
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
            job_type: JobType::GitForce,
            frontend: Frontend::discord(context, msg),
            any_author: false,
        }
    }
    pub fn new_gitquery(
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
            job_type: JobType::GitQuery,
            frontend: Frontend::discord(context, msg),
            any_author: false,
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
            any_author: false,
        }
    }
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AcixCredits {
    pub tl: Option<String>,
    pub tlc: Option<String>,
    pub ts: Option<String>,
    pub qc: Option<String>,
}

impl AcixCredits {
    pub fn extra(&self) -> String {
        [&self.tl, &self.tlc, &self.ts, &self.qc]
            .into_iter()
            .filter_map(|credit| credit.as_deref())
            .map(str::trim)
            .filter(|credit| !credit.is_empty() && *credit != "---")
            .collect::<Vec<_>>()
            .join(" & ")
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
    #[serde(default)]
    pub credits: Option<AcixCredits>,
    // The AnimeciX title id, when it was already resolved by something the MyAnimeList id cannot
    // reach — `/publish` resolving a TMDB import because the two catalogs file the anime under
    // different MAL ids. Confirm uses it instead of searching, which would fail the same way again.
    // Absent on every record queued before this existed and on the ordinary MAL-resolvable path.
    #[serde(default)]
    pub acix_id: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SmartcodeDriveName {
    pub organisation: String,
    pub mal_name: String,
    pub episode: u32,
}

impl SmartcodeDriveName {
    pub fn new(owner_repo: &str, mal_name: &str, episode: u32) -> Self {
        let organisation = owner_repo
            .split('/')
            .next()
            .unwrap_or("")
            .trim();
        Self {
            organisation: upload_name_component(organisation),
            mal_name: upload_name_component(mal_name),
            episode,
        }
    }

    pub fn filename(&self, resolution: &str) -> String {
        format!(
            "[{}] {} - Bölüm {:02} [{}].mp4",
            fallback_component(&self.organisation, "Pandora"),
            fallback_component(&self.mal_name, "Anime"),
            self.episode,
            resolution,
        )
    }
}

fn upload_name_component(raw: &str) -> String {
    raw.replace(['/', '\\'], "-").trim().to_string()
}

fn fallback_component<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
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
    pub display_link: Option<String>,
    pub attachment: Vec<u8>,
    pub server_watermark: Option<Vec<u8>>,
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
    pub smartcode_drive_name: Option<SmartcodeDriveName>,
    pub worker: String,
    pub duplicate_source: Option<PathBuf>,
    pub forward_parent: Option<u64>,
    pub encode_warnings: Vec<String>,
    pub encode_dispatched: bool,
    pub encode_dispatch_order: Option<u64>,
    // Unix time of the dispatch and of the last encoder progress frame, plus the Encode layer's
    // reboot count at dispatch. The first two are what the stall watchdog measures against; the
    // third is how a reboot tells apart the jobs that were lost with the old layer from the one
    // that was just handed to the new one.
    pub encode_dispatched_at: Option<Duration>,
    pub encode_last_frame_at: Option<Duration>,
    pub encode_dispatch_epoch: u32,
    pub encode_frame: Option<u64>,
    pub encode_total: Option<u64>,
    pub encode_fps: Option<f64>,
    pub keep: Option<KeepRequest>,
    pub keycode: Option<KeycodeRequest>,
    pub preview: Option<PreviewRequest>,
    pub studio: Option<StudioJobRequest>,
    // Set on the parent that owns the multi-file download; `batch_parent` is set on the per-episode
    // encodes it spawns. Exactly one of the two is ever populated.
    pub batch: Option<BatchRequest>,
    pub batch_parent: Option<u64>,
    // Set while a Pandora Mini node is executing this job. The row, the message and the DB stay
    // here; only the work is elsewhere, so a job with this set is skipped by every local dispatch
    // and progresses solely through `do_link_things`.
    pub link_node: Option<String>,
    // Nodes this job has already been lost on. A job that reliably kills whatever runs it must not
    // become an endless tour of the cluster, so past `LINK_MAX_ATTEMPTS` it stays local.
    pub link_attempts: u32,
    // A node named on submit. The job waits for that node instead of running locally.
    pub link_pin: Option<String>,
    // Set on a leased job whose output belongs to the coordinator rather than to the node that
    // produced it: an HLS-only release is served for twelve hours from the machine that publishes
    // it, and a node has no public hostname to serve it from. The node encodes, holds at
    // `Encoded`, hands the file back, and the coordinator resumes the job from there.
    pub link_return_output: bool,
    // The originating server's Drive-only upload policy, resolved by the coordinator. A node holds
    // no `meta.pandora` for that guild, so without this it would publish to streaming hosts the
    // server had deliberately switched off. `None` on every local job, which reads the file.
    pub link_drive_only: Option<bool>,
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
        let settings = load_server_settings(server_id);
        let preset = match job_type {
            JobType::Encode | JobType::Pancode | JobType::Batch => settings.preset.clone(),
            JobType::Keycode => match settings.preset {
                Preset::PseudoLossless(candidates)
                | Preset::Dummy(candidates)
                | Preset::Standard(candidates)
                | Preset::VerySlow(candidates)
                | Preset::Hd720(candidates)
                | Preset::Sd480(candidates)
                | Preset::Gpu(candidates) => Preset::Standard(candidates),
                Preset::Copy => Preset::Standard(None),
            },
            JobType::Studio => Preset::Copy,
            JobType::StudioPreview => Preset::Dummy(None),
            _ => Preset::Dummy(None),
        };
        Self {
            author,
            channel_id,
            response_id,
            job_type,
            job_id,
            preset,
            torrent,
            display_link: None,
            attachment,
            server_watermark: if matches!(
                job_type,
                JobType::Encode | JobType::Pancode | JobType::Batch
            ) {
                settings.watermark
            } else {
                None
            },
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
            smartcode_drive_name: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
            encode_dispatched: false,
            encode_dispatch_order: None,
            encode_dispatched_at: None,
            encode_last_frame_at: None,
            encode_dispatch_epoch: 0,
            encode_frame: None,
            encode_total: None,
            encode_fps: None,
            keep: None,
            keycode: None,
            preview: None,
            studio: None,
            batch: None,
            batch_parent: None,
            link_node: None,
            link_attempts: 0,
            link_pin: None,
            link_return_output: false,
            link_drive_only: None,
        }
    }

    pub fn new_api(
        author: u64,
        channel_id: u64,
        job_type: JobType,
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
        let settings = load_server_settings(server_id);
        let preset = match job_type {
            JobType::Encode | JobType::Pancode | JobType::Batch => settings.preset.clone(),
            JobType::Keycode => match settings.preset {
                Preset::PseudoLossless(candidates)
                | Preset::Dummy(candidates)
                | Preset::Standard(candidates)
                | Preset::VerySlow(candidates)
                | Preset::Hd720(candidates)
                | Preset::Sd480(candidates)
                | Preset::Gpu(candidates) => Preset::Standard(candidates),
                Preset::Copy => Preset::Standard(None),
            },
            JobType::Studio => Preset::Copy,
            JobType::StudioPreview => Preset::Dummy(None),
            _ => Preset::Dummy(None),
        };
        Self {
            author,
            channel_id,
            response_id: 0,
            job_type,
            job_id,
            preset,
            torrent,
            display_link: None,
            attachment,
            server_watermark: if matches!(
                job_type,
                JobType::Encode | JobType::Pancode | JobType::Batch
            ) {
                settings.watermark
            } else {
                None
            },
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
            smartcode_drive_name: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
            encode_dispatched: false,
            encode_dispatch_order: None,
            encode_dispatched_at: None,
            encode_last_frame_at: None,
            encode_dispatch_epoch: 0,
            encode_frame: None,
            encode_total: None,
            encode_fps: None,
            keep: None,
            keycode: None,
            preview: None,
            studio: None,
            batch: None,
            batch_parent: None,
            link_node: None,
            link_attempts: 0,
            link_pin: None,
            link_return_output: false,
            link_drive_only: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::p2p::nyaaise::TorrentType;

    // The download worker's speculation used to be chosen by matching on the `Preset` variant,
    // which meant a preset file could not reach it: an operator who turned encoding-ahead on for
    // their GPU preset got a coordinator that never started one. Every answer here comes from the
    // resolved preset instead, so it stays in step with what pnmpeg will actually do.
    #[test]
    fn the_download_worker_speculates_on_what_the_preset_asks_for() {
        let linear = |preset: Preset| {
            download_aot_for(&preset).map(|aot| (aot.preset, aot.chunked))
        };
        for (preset, name) in [
            (Preset::Standard(None), "standard"),
            (Preset::PseudoLossless(None), "pseudolossless"),
            (Preset::Dummy(None), "dummy"),
        ] {
            assert_eq!(linear(preset), Some((name.to_string(), false)), "{name}");
        }
        // VerySlow plans chunk boundaries instead of keeping one encoder alive.
        assert_eq!(
            linear(Preset::VerySlow(None)),
            Some(("veryslow".to_string(), true))
        );
        // Off by default, and a stream copy has no preset to speculate with at all.
        for preset in [Preset::Gpu(None), Preset::Hd720(None), Preset::Sd480(None), Preset::Copy] {
            assert_eq!(linear(preset.clone()), None, "{:?}", preset);
        }
        // Whatever it decides, it names a preset pnmpeg can look up — including its file.
        for preset in [Preset::Standard(None), Preset::VerySlow(None), Preset::Gpu(None)] {
            let name = preset.name().unwrap();
            assert!(
                crate::lib::mpeg::preset::resolve(name).is_some(),
                "{name} does not resolve"
            );
        }
    }

    #[test]
    fn drive_deletion_is_limited_to_the_job_author_or_a_witch() {
        let author = DriveDeleteRequest::new(7, 1, 99, false);
        assert!(author.may_delete(7));
        assert!(!author.may_delete(8));
        let witch = DriveDeleteRequest::new(7, 1, 99, true);
        assert!(witch.may_delete(8));
    }

    #[test]
    fn a_knife_cancel_reaches_a_job_its_sender_did_not_start() {
        let own = HalfJob::new_cancel(7, 1, 99);
        assert!(own.may_cancel(7));
        assert!(!own.may_cancel(8));
        let witch = HalfJob::new_cancel_any(7, 1, 99);
        assert!(witch.may_cancel(8));
        assert_eq!(witch.job_id, 99);
        assert!(matches!(witch.job_type, JobType::Cancel));
    }

    fn dispatched_encode(now: Duration) -> Job {
        let mut job = Job::new_api(
            0,
            0,
            JobType::Encode,
            TorrentType::Link("https://example.invalid/input.torrent".to_string()),
            Vec::new(),
            "EN".to_string(),
            None,
        );
        job.ready = Stage::Downloaded;
        job.encode_dispatched = true;
        job.encode_dispatched_at = Some(now);
        job.encode_dispatch_epoch = 1;
        job
    }

    #[test]
    fn a_dispatched_encode_stalls_only_after_the_timeout() {
        let dispatched_at = Duration::from_secs(10_000);
        let job = dispatched_encode(dispatched_at);
        assert!(!encode_stalled(&job, dispatched_at + ENCODE_STALL_TIMEOUT));
        assert!(encode_stalled(
            &job,
            dispatched_at + ENCODE_STALL_TIMEOUT + Duration::from_secs(1)
        ));
    }

    #[test]
    fn each_progress_frame_restarts_the_stall_clock() {
        let dispatched_at = Duration::from_secs(10_000);
        let mut job = dispatched_encode(dispatched_at);
        job.ready = Stage::Encoding;
        let long_after = dispatched_at + ENCODE_STALL_TIMEOUT * 4;
        job.encode_last_frame_at = Some(long_after);
        assert!(!encode_stalled(&job, long_after + Duration::from_secs(60)));
        assert!(encode_stalled(
            &job,
            long_after + ENCODE_STALL_TIMEOUT + Duration::from_secs(1)
        ));
    }

    #[test]
    fn an_undispatched_or_forwarded_job_never_counts_as_stalled() {
        let dispatched_at = Duration::from_secs(10_000);
        let now = dispatched_at + ENCODE_STALL_TIMEOUT * 2;

        let mut waiting = dispatched_encode(dispatched_at);
        waiting.encode_dispatched = false;
        assert!(!encode_stalled(&waiting, now));

        let mut forwarded = dispatched_encode(dispatched_at);
        forwarded.forward_parent = Some(7);
        assert!(!encode_stalled(&forwarded, now));

        // A finished job keeps its dispatch timestamp; only the live stages are watched.
        let mut uploading = dispatched_encode(dispatched_at);
        uploading.ready = Stage::Uploading;
        assert!(!encode_stalled(&uploading, now));
    }

    #[tokio::test]
    async fn gitsync_keeps_unfinished_job_logs_and_never_clobbers_archived_ones() {
        let root = std::env::temp_dir().join(format!("pandora-gitsync-logs-{}", std::process::id()));
        tokio::fs::remove_dir_all(&root).await.ok();
        let work = root.join("work");
        let saved_data = root.join("saved_data");

        // A running job with logs, an archived job that already moved its own, and scratch that is
        // not a job directory at all.
        tokio::fs::create_dir_all(work.join("111").join("log")).await.unwrap();
        tokio::fs::write(work.join("111").join("log").join("PNmpeg_Encode111.log"), b"live")
            .await
            .unwrap();
        tokio::fs::create_dir_all(work.join("222").join("log")).await.unwrap();
        tokio::fs::write(work.join("222").join("log").join("stale.log"), b"stale")
            .await
            .unwrap();
        tokio::fs::create_dir_all(saved_data.join("222").join("log")).await.unwrap();
        tokio::fs::write(saved_data.join("222").join("log").join("real.log"), b"archived")
            .await
            .unwrap();
        tokio::fs::create_dir_all(work.join("batch-pending").join("9").join("subs"))
            .await
            .unwrap();

        // The ids come back so the caller can name the jobs its wipe is about to break.
        assert_eq!(preserve_logs_from(&work, &saved_data).await, vec![111]);

        assert_eq!(
            tokio::fs::read_to_string(saved_data.join("111").join("log").join("PNmpeg_Encode111.log"))
                .await
                .unwrap(),
            "live"
        );
        assert_eq!(
            tokio::fs::read_to_string(saved_data.join("222").join("log").join("real.log"))
                .await
                .unwrap(),
            "archived"
        );
        assert!(!saved_data.join("222").join("log").join("stale.log").exists());
        assert!(!saved_data.join("batch-pending").exists());

        tokio::fs::remove_dir_all(root).await.ok();
    }

    #[test]
    fn a_reboot_resets_only_jobs_dispatched_into_the_previous_layer() {
        let now = Duration::from_secs(10_000);
        let mut lost = dispatched_encode(now);
        lost.job_id = 1;
        lost.encode_dispatch_epoch = 1;
        // Dispatched *during* the reboot that `shrine.send` performed on its way in.
        let mut fresh = dispatched_encode(now);
        fresh.job_id = 2;
        fresh.encode_dispatch_epoch = 2;
        let mut queue = vec![lost, fresh];

        reset_encode_dispatches_after_reboot(&mut queue, 2);

        assert!(!queue[0].encode_dispatched, "job from the dead layer must be re-sent");
        assert!(queue[1].encode_dispatched, "job just handed to the new layer must not be re-sent");
    }
}
