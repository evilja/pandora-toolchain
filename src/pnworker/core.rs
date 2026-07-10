use crate::lib::db::core::JobDb;
use crate::lib::p2p::core::cleanup_pandora_qbit;
use crate::lib::p2p::nyaaise::TorrentType;
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
use crate::pnworker::estimate::QueueEstimator;
use crate::pnworker::frontend::Frontend;
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};
use crate::pnworker::keep::{
    KeywordResolve, ResolvedKeywords, cleanup_expired_keeps, cleanup_keep_startup,
    mark_output_failed, prepare_keep, reserve_output, resolve_keywords_for_keycode, scope,
    store_output,
};
use crate::pnworker::lifecycle::{cleanup_job, render};
use crate::pnworker::messages::{
    ENCODE_CONCAT_PROG, ENCODE_PROG, ENCODE_WARNING, GITQUERY_BLOCKED, JOB_SETUP_FAIL,
    MessagePayload, QUEUE_TOO_LONG, QUEUED, TORRENT_DUPLICATE_WAIT, UPLOAD_DONE, UPLOAD_PROG,
    WORKER_ASSIGN,
};
use crate::pnworker::presence::{Presence, presence_from_queue};
use crate::pnworker::progress::{drive_link_from_payload, persist_side_effects};
use crate::pnworker::pull::git_pull;
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
use tokio::fs::{File, create_dir_all, remove_dir_all, write};
use tokio::sync::mpsc::Receiver;
use tokio::time::Duration;
use tokio::time::sleep;

pub type CommData = (u64, MessagePayload, Option<Stage>);

#[derive(Clone)]
pub enum WorkerMsg {
    Download(DownloadData),
    Probe(ProbeData),
    Preview(PreviewData),
    Encode(EncodeData),
    Keycode(KeycodeData),
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
    cleanup_keep_startup().await;

    let mut queue: Vec<Job> = vec![];
    let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
    shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
    shrine.layer(Worker::Encode, pn_encdeworker, 5, 50);
    shrine.layer(Worker::Upload, pn_uloadworker, 5, 50);
    shrine.layer(Worker::Probe, pn_probeworker, 5, 50);
    let mut queue_estimator = QueueEstimator::new();
    let mut encode_reboot_epoch = shrine.reboot_epoch(&Worker::Encode);
    let mut gitquery: Option<HalfJob> = None;

    loop {
        sleep(Duration::from_millis(200)).await;

        shrine.drain_heartbeats().await;
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        cleanup_expired_input_cache().await;
        cleanup_expired_keeps().await;

        if do_queue_things(&mut rx, &db, &mut queue, &mut shrine, &mut gitquery).await {
            check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
            continue;
        }
        do_probe_timeout_things(&db, &mut queue).await;
        if do_worker_message_things(&db, &mut queue, &mut shrine).await {
            check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
            continue;
        }
        do_duplicate_waiting_things(&db, &mut queue).await;
        do_queued_download_waiting_things(&db, &mut queue, &mut shrine).await;
        check_encode_reboot_epoch(&shrine, &mut encode_reboot_epoch, &mut queue);
        do_job_progression_things(&db, &mut queue, &mut shrine).await;
        if let Some(halfjob) = gitquery.take() {
            if encode_jobs_active(&queue) {
                gitquery = Some(halfjob);
            } else {
                run_gitsync(halfjob.frontend, &mut shrine).await;
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
            if queue.len() > 4 {
                job.ready = Stage::Declined;
                render(&mut job, MessagePayload::Static(QUEUE_TOO_LONG)).await;
                return true;
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
            queue.push(job);
        }
        JobClass::HalfJob(halfjob) => {
            handle_half_job(db, queue, shrine, halfjob, gitquery).await;
        }
    }
    false
}

fn is_encode_job_type(job_type: JobType) -> bool {
    matches!(job_type, JobType::Encode | JobType::Pancode | JobType::Keycode)
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

fn reset_encode_dispatches_after_reboot(queue: &mut [Job]) {
    for job in queue {
        if job.ready == Stage::Downloaded
            || (job.job_type == JobType::Keycode && job.ready == Stage::Queued)
        {
            job.encode_dispatched = false;
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
        reset_encode_dispatches_after_reboot(queue);
        *encode_reboot_epoch = current_encode_reboot_epoch;
    }
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
        JobType::Keycode => queue_keycode_job(db, queue, shrine, job).await,
        JobType::Preview => queue_preview_job(db, queue, shrine, job).await,
        _ => false,
    }
}

async fn prepare_queued_job(job: &mut Job, worker: &str, write_subtitle: bool) -> bool {
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
            return false;
        }
    }
    if write_subtitle {
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
            return false;
        }
    }
    true
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
    if !prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, "could not prepare the work directory").await;
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
    queue_download_job(db, queue, shrine, job, None, false).await
}

async fn queue_probe_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if !prepare_queued_job(job, "prb-pending", false).await {
        decline_job_setup(job, "could not prepare the work directory").await;
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
    let probe_dir = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("DB")
        .join("work")
        .join(probe_id.to_string());
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
    if !prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, "could not prepare the work directory").await;
        return true;
    }

    let torrent_src = probe_dir.join("contents").join("fetch.torrent");
    let torrent_dst = job.directory.join("contents").join("fetch.torrent");
    if tokio::fs::copy(&torrent_src, &torrent_dst).await.is_err()
        && job.torrent.get().trim().is_empty()
    {
        decline_job_setup(job, "probe torrent data is no longer available").await;
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
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("DB")
            .join("work")
            .join(id.to_string())
    });
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
    if !prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, "could not prepare the work directory").await;
        return true;
    }
    if let Some(probe_dir) = probe_dir {
        let torrent_src = probe_dir.join("contents").join("fetch.torrent");
        let torrent_dst = job.directory.join("contents").join("fetch.torrent");
        if tokio::fs::copy(&torrent_src, &torrent_dst).await.is_err() {
            decline_job_setup(job, "probe torrent data is no longer available").await;
            return true;
        }
    }
    queue_download_job(db, queue, shrine, job, job.probe_file_index, false).await
}

async fn queue_keycode_job(
    db: &JobDb,
    queue: &[Job],
    _shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if !prepare_queued_job(job, "enc-main", !job.attachment.is_empty()).await {
        decline_job_setup(job, "could not prepare the work directory").await;
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
    dispatch_keycode_ready(db, shrine, job, request, resolved).await
}

async fn dispatch_keycode_ready(
    db: &JobDb,
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
    request: KeycodeRequest,
    resolved: ResolvedKeywords,
) -> KeycodeDispatch {
    if resolved.kind == KeepKind::Backup && job.attachment.is_empty() {
        return fail_keycode(db, job, "backup keywords require a subtitle").await;
    }
    let mut inputs = resolved.paths;
    if let Some(intro) = request
        .concat_candidates
        .as_ref()
        .and_then(|c| select_keycode_intro(inputs.first(), c))
    {
        inputs.insert(0, intro);
    }
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
    job.encode_dispatched = true;
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
        Preset::Gpu(_) => Preset::Gpu(None),
    }
}

fn select_keycode_intro(first_input: Option<&PathBuf>, candidates: &[String]) -> Option<PathBuf> {
    let first = first_input?.to_string_lossy().to_string();
    let main_fps = crate::lib::mpeg::probe::ffprobe_framerate(&first);
    let main_sr = crate::lib::mpeg::probe::ffprobe_samplerate(&first);
    let mut best_match: Option<(usize, &String)> = None;
    let mut highest_fps: Option<(&String, (u32, u32))> = None;
    for candidate in candidates {
        let cand_fps = crate::lib::mpeg::probe::ffprobe_framerate(candidate);
        let cand_sr = crate::lib::mpeg::probe::ffprobe_samplerate(candidate);
        if let Some(fps) = cand_fps {
            match highest_fps {
                None => highest_fps = Some((candidate, fps)),
                Some((_, hfps)) => {
                    if fps.0 * hfps.1 > hfps.0 * fps.1 {
                        highest_fps = Some((candidate, fps));
                    }
                }
            }
        }
        let mut score = 0usize;
        if main_fps.is_some() && cand_fps == main_fps {
            score += 1;
        }
        if main_sr.is_some() && cand_sr == main_sr {
            score += 1;
        }
        if score > best_match.map(|(s, _)| s).unwrap_or(0) {
            best_match = Some((score, candidate));
        }
    }
    best_match
        .filter(|(score, _)| *score >= 2)
        .map(|(_, path)| PathBuf::from(path))
        .or_else(|| highest_fps.map(|(path, _)| PathBuf::from(path)))
}

async fn queue_backup_all_job(
    db: &JobDb,
    queue: &[Job],
    shrine: &mut TypedShrine<WorkerMsg>,
    job: &mut Job,
) -> bool {
    if !prepare_queued_job(job, "dwl-pending", false).await {
        decline_job_setup(job, "could not prepare the work directory").await;
        return true;
    }
    if !dispatch_or_kill(
        shrine,
        &Worker::Download,
        WorkerMsg::Download((
            job.directory.clone(),
            job.torrent.clone(),
            job.job_id,
            None,
            true,
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
    if !prepare_queued_job(job, "dwl-pending", true).await {
        decline_job_setup(job, "could not prepare the work directory").await;
        return true;
    }
    queue_download_job(db, queue, shrine, job, None, false).await
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
    } else if wait_for_active_torrent_download(db, job, queue).await {
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
                .position(|i| halfjob.job_id == i.job_id && halfjob.author == i.author)
            {
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
        JobType::Workers => {
            let mut frontend = halfjob.frontend;
            frontend.set_embed(create_workers_embed(queue).await).await;
        }
        JobType::GitSync => {
            run_gitsync(halfjob.frontend, shrine).await;
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
                run_gitsync(halfjob.frontend, shrine).await;
            }
        }
        _ => {}
    }
}

async fn run_gitsync(mut frontend: Frontend, shrine: &mut TypedShrine<WorkerMsg>) {
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
            frontend
                .set_text("Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.")
                .await
        }
        Err(e) => {
            println!("{}", e);
            frontend
                .set_text(
                    "Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.",
                )
                .await
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
    if *id != UPLOAD_PROG && *id != UPLOAD_DONE {
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
    let url = args.get(0).cloned().unwrap_or_default();
    let upload = SmartcodeDriveUpload {
        job_id: job.job_id,
        file_id: file_id.to_string(),
        folder_id: folder_id.to_string(),
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
            }
            if *id == ENCODE_CONCAT_PROG {
                queue[pos].encode_frame = args.get(0).and_then(|s| s.parse().ok());
                queue[pos].encode_total = args.get(1).and_then(|s| s.parse().ok());
                queue[pos].encode_fps = args.get(2).and_then(|s| s.parse().ok());
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
            persist_smartcode_drive_upload(i, &payload, stage).await;
            // PREVIEW_DONE attaches files from the work dir here; cleanup below must remain after render.
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
        if queue_download_job(db, &snapshot, shrine, &mut queue[pos], file_index, false).await {
            dead.push(id);
        }
    }
    queue.retain(|j| !dead.contains(&j.job_id));
}

async fn do_job_progression_things(
    db: &JobDb,
    queue: &mut Vec<Job>,
    shrine: &mut TypedShrine<WorkerMsg>,
) {
    let qlen = queue.len();
    let mut dead: Vec<u64> = vec![];
    let mut forwarded_state_updates: Vec<(u64, Stage, String)> = vec![];
    let mut active_encode_sources: HashMap<String, PathBuf> = HashMap::new();
    for j in queue
        .iter()
        .filter(|j| {
            j.forward_parent.is_none()
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
        if job.ready == Stage::Probed {
            continue;
        }
        if job.job_type == JobType::Keycode && job.ready == Stage::Queued && !job.encode_dispatched {
            match try_dispatch_keycode(db, shrine, job).await {
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
            if job.job_type == JobType::Preview {
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
                job.worker = "prb-pending".to_string();
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
                job.encode_dispatched = true;
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
                    if job.job_type == JobType::Keycode {
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
    PseudoLossless(Option<Vec<String>>),
    Dummy(Option<Vec<String>>),
    Standard(Option<Vec<String>>),
    Gpu(Option<Vec<String>>),
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    pub concat_candidates: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub shots: Vec<(u64, String)>,
    pub watermark_font: Option<PathBuf>,
    pub ranking_log: String,
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
    pub encode_frame: Option<u64>,
    pub encode_total: Option<u64>,
    pub encode_fps: Option<f64>,
    pub keep: Option<KeepRequest>,
    pub keycode: Option<KeycodeRequest>,
    pub preview: Option<PreviewRequest>,
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
            display_link: None,
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
            preview: None,
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
            display_link: None,
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
            preview: None,
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
