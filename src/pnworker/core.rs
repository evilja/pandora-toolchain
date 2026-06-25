use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{Receiver};
use tokio::time::sleep;
use tokio::time::Duration;
use crate::libpnp2p::nyaaise::TorrentType;
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};
use crate::pnworker::messages::{MessagePayload, PROBE_TIMEOUT, QUEUE_TOO_LONG, QUEUED,
    ENCODE_PROG, ENCODE_CONCAT_PROG, PROBE_ROW, UPLOAD_DONE, UPLOAD_BACKUP_PROG, BACKUPALL_PROG,
    TORRENT_PROG, TORRENT_PROG_SELECT, UPLOAD_PROG};
use crate::libpndb::core::JobDb;
use crate::pnworker::presence::{presence_from_queue, Presence};
use crate::pnworker::frontend::Frontend;
use crate::pnworker::pull::git_pull;
use tokio::fs::{File, create_dir_all, remove_dir_all, rename, write};
use std::path::PathBuf;
use std::env;
use serenity::all::{Context, Message};
use crate::pnworker::workers::downloadworker::*;
use crate::pnworker::workers::encodeworker::*;
use crate::pnworker::workers::uploadworker::*;
use crate::pnworker::workers::probeworker::*;

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

    let mut queue: Vec<Job> = vec![];
    let mut shrine: TypedShrine<WorkerMsg> = TypedShrine::new();
    shrine.layer(Worker::Download, pn_dloadworker, 5, 50);
    shrine.layer(Worker::Encode,   pn_encdeworker, 5, 50);
    shrine.layer(Worker::Upload,   pn_uloadworker, 5, 50);
    shrine.layer(Worker::Probe,    pn_probeworker, 5, 50);

    loop {
        sleep(Duration::from_millis(200)).await;

        shrine.drain_heartbeats().await;

        if let Ok(jobclass) = rx.try_recv() {
            match jobclass {
                JobClass::Job(mut job) => {
                    if queue.len() > 4 {
                        job.ready = Stage::Declined;
                        render(&mut job, MessagePayload::Static(QUEUE_TOO_LONG)).await;
                        continue;
                    }
                    match job.job_type {
                        JobType::Encode => {
                            render(&mut job, MessagePayload::Static(QUEUED)).await;
                            for i in STRUCT {
                                create_dir_all(job.directory.join(i)).await.unwrap();
                            }
                            write(job.directory.join("contents").join("subtitle.ass"), &job.attachment).await.unwrap();
                            if !dispatch_or_kill(&mut shrine, &Worker::Download, WorkerMsg::Download((job.directory.clone(), job.torrent.clone(), job.job_id, None, false)), &mut job, &db, true).await {
                                continue;
                            }
                            job.ready = Stage::Downloading;
                            job.frontend.set_presence(Presence::Downloading { idx: queue.len(), total: queue.len() + 1 }).await;
                        }
                        JobType::Probe => {
                            render(&mut job, MessagePayload::Static(QUEUED)).await;
                            for i in STRUCT { create_dir_all(job.directory.join(i)).await.unwrap(); }
                            // no subtitle to write
                            if !dispatch_or_kill(&mut shrine, &Worker::Probe, WorkerMsg::Probe((job.directory.clone(), job.torrent.clone(), job.job_id)), &mut job, &db, true).await {
                                continue;
                            }
                            job.ready = Stage::Probing;
                            job.frontend.set_presence(Presence::Probing { idx: queue.len(), total: queue.len() + 1 }).await;
                        }
                        JobType::Pancode => {
                            let probe_dir = env::current_dir().unwrap()
                                .join("DB").join("work")
                                .join(job.probe_job_id.unwrap().to_string());

                            render(&mut job, MessagePayload::Static(QUEUED)).await;
                            for i in STRUCT { create_dir_all(job.directory.join(i)).await.unwrap(); }
                            write(job.directory.join("contents").join("subtitle.ass"), &job.attachment).await.unwrap();

                            let torrent_src = probe_dir.join("contents").join("fetch.torrent");
                            let torrent_dst = job.directory.join("contents").join("fetch.torrent");
                            match tokio::fs::copy(&torrent_src, &torrent_dst).await {
                                Ok(_) => {
                                    ();
                                }
                                Err(_) => {
                                    continue;
                                }
                            };

                            if !dispatch_or_kill(&mut shrine, &Worker::Download, WorkerMsg::Download((
                                job.directory.clone(),
                                job.torrent.clone(),
                                job.job_id,
                                job.probe_file_index,
                                false,
                            )), &mut job, &db, true).await {
                                continue;
                            }
                            job.ready = Stage::Downloading;
                            job.frontend.set_presence(Presence::Downloading { idx: queue.len(), total: queue.len() + 1 }).await;
                        }
                        JobType::Backup => {
                            let probe_dir = job.probe_job_id.map(|id| {
                                env::current_dir().unwrap()
                                    .join("DB").join("work")
                                    .join(id.to_string())
                            });

                            render(&mut job, MessagePayload::Static(QUEUED)).await;
                            for i in STRUCT {
                                create_dir_all(job.directory.join(i)).await.unwrap();
                            }
                            if let Some(probe_dir) = probe_dir {
                                let torrent_src = probe_dir.join("contents").join("fetch.torrent");
                                let torrent_dst = job.directory.join("contents").join("fetch.torrent");
                                if tokio::fs::copy(&torrent_src, &torrent_dst).await.is_err() {
                                    continue;
                                }
                            }
                            if !dispatch_or_kill(&mut shrine, &Worker::Download, WorkerMsg::Download((job.directory.clone(), job.torrent.clone(), job.job_id, job.probe_file_index, false)), &mut job, &db, true).await {
                                continue;
                            }
                            job.ready = Stage::Downloading;
                            job.frontend.set_presence(Presence::Downloading { idx: queue.len(), total: queue.len() + 1 }).await;
                        }
                        JobType::BackupAll => {
                            render(&mut job, MessagePayload::Static(QUEUED)).await;
                            for i in STRUCT {
                                create_dir_all(job.directory.join(i)).await.unwrap();
                            }
                            if !dispatch_or_kill(&mut shrine, &Worker::Download, WorkerMsg::Download((job.directory.clone(), job.torrent.clone(), job.job_id, None, true)), &mut job, &db, true).await {
                                continue;
                            }
                            job.ready = Stage::Downloading;
                            job.frontend.set_presence(Presence::Downloading { idx: queue.len(), total: queue.len() + 1 }).await;
                        }
                        _ => {}
                    }
                    db.insert_job(&job).await.unwrap();
                    queue.push(job);
                }
                JobClass::HalfJob(halfjob) => {
                    match halfjob.job_type {
                        JobType::Cancel => {
                            for i in &queue {
                                if halfjob.job_id == i.job_id && halfjob.author == i.author {
                                    File::create(i.directory.join("CANCEL")).await.unwrap();
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
                            let repo_path = env::var("PANDORA_GITSYNC_REPO")
                                .unwrap_or_else(|_| std::env::current_dir().unwrap().to_str().unwrap().to_owned());
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
                                },
                                Err(e) => {
                                    println!("{}", e);
                                    frontend.set_text("Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.").await
                                },
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
            }
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        let timed_out: Vec<u64> = queue.iter()
            .filter(|j| j.ready == Stage::Probed)
            .filter(|j| now.saturating_sub(j.requested_at) > Duration::from_secs(180))
            .map(|j| j.job_id)
            .collect();
        for id in timed_out {
            if let Some(pos) = queue.iter().position(|j| j.job_id == id) {
                let directory = queue[pos].directory.clone();
                let mut frontend = queue[pos].frontend.clone();

                frontend.update(&queue[pos], &MessagePayload::Static(PROBE_TIMEOUT)).await;

                cleanup_job(&directory, &PathBuf::from("DB").join("saved_data").join(id.to_string())).await;
                db.archive_job(id).await.unwrap();
                queue.remove(pos);
                frontend.set_presence(presence_from_queue(&queue)).await;
            }
        }
        if let Some((_, commdata)) = shrine.receive(500).await {
            let mut finished_fe: Option<Frontend> = None;
            if let Some(i) = queue.iter_mut().find(|j| j.job_id == commdata.0) {
                if let Some(a) = commdata.2 {
                    i.ready = a;
                    db.update_stage(i.job_id, i.ready).await.unwrap();
                }
                if commdata.2 == Some(Stage::Uploaded) {
                    if let Some(acix) = i.acix.clone() {
                        if let Some(drive) = drive_link_from_payload(&commdata.1) {
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
                persist_side_effects(&db, i.job_id, &commdata.1, commdata.2).await;
                render(i, commdata.1).await;

                let finished = matches!(i.ready, Stage::Uploaded | Stage::Failed | Stage::Cancelled);
                let probe_job_id = i.probe_job_id;
                let job_id = i.job_id;
                let directory = i.directory.clone();
                if finished {
                    finished_fe = Some(i.frontend.clone());
                }

                if finished {
                    // If this was a pancode job, remove and clean up its probe parent
                    if let Some(probe_id) = probe_job_id {
                        if let Some(probe_pos) = queue.iter().position(|j| j.job_id == probe_id) {
                            let probe = &queue[probe_pos];
                            cleanup_job(&probe.directory.clone(), &PathBuf::from("DB").join("saved_data").join(probe_id.to_string())).await;
                            db.archive_job(probe_id).await.unwrap();
                            queue.remove(probe_pos);
                        }
                    }
                    db.archive_job(job_id).await.unwrap();
                    cleanup_job(&directory, &PathBuf::from("DB").join("saved_data").join(job_id.to_string())).await;
                    // Find and remove by job_id since indices may have shifted after probe removal
                    queue.retain(|j| j.job_id != job_id);
                }
            }
            if let Some(fe) = finished_fe {
                fe.set_presence(presence_from_queue(&queue)).await;
            }
        }
        let qlen = queue.len();
        let mut dead: Vec<u64> = vec![];
        for (idx, job) in queue.iter_mut().enumerate() {
            // Find the first job that's actively progressing (not parked at Probed)
            //
            if job.ready == Stage::Probed {
                continue;
            }

            if job.ready == Stage::Downloaded {
                if job.job_type == JobType::Backup {
                    let src = job.directory.join("contents").join("torrent").join("input.mkv");
                    let dst = job.directory.join("work").join("output.mp4");
                    let _ = tokio::fs::rename(&src, &dst).await;
                    if !dispatch_or_kill(&mut shrine, &Worker::Upload, WorkerMsg::Upload((
                        job.directory.clone(),
                        format!("{}.mkv", job.directory.file_name().unwrap_or_default().display()),
                        false,
                        job.job_id,
                        job.server_id
                    )), job, &db, false).await {
                        dead.push(job.job_id);
                        continue;
                    }
                    job.ready = Stage::Uploading;
                    db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                    job.frontend.set_presence(Presence::Uploading { idx, total: qlen }).await;
                } else if job.job_type == JobType::BackupAll {
                    if !dispatch_or_kill(&mut shrine, &Worker::Upload, WorkerMsg::UploadAll((job.directory.clone(), job.job_id, job.server_id)), job, &db, false).await {
                        dead.push(job.job_id);
                        continue;
                    }
                    job.ready = Stage::Uploading;
                    db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                    job.frontend.set_presence(Presence::Uploading { idx, total: qlen }).await;
                } else {
                    if !dispatch_or_kill(&mut shrine, &Worker::Encode, WorkerMsg::Encode((job.directory.clone(), job.preset.clone(), job.job_id, job.server_id)), job, &db, false).await {
                        dead.push(job.job_id);
                        continue;
                    }
                    job.ready = Stage::Encoding;
                    db.update_stage(job.job_id, Stage::Encoding).await.unwrap();
                    job.frontend.set_presence(Presence::Encoding { idx, total: qlen }).await;
                }
            } else if job.ready == Stage::Encoded {
                if !dispatch_or_kill(&mut shrine, &Worker::Upload, WorkerMsg::Upload((job.directory.clone(), format!("{}.mp4", job.directory.file_name().unwrap_or_default().display()),
                    match job.preset {
                        Preset::Dummy(_) => false,
                        _ => true,
                    },
                 job.job_id,
                 job.server_id)), job, &db, false).await {
                    dead.push(job.job_id);
                    continue;
                }
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                job.frontend.set_presence(Presence::Uploading { idx, total: qlen }).await;
            }
        }
        queue.retain(|j| !dead.contains(&j.job_id));
    }
}

async fn render(job: &mut Job, payload: MessagePayload) {
    let mut fe = std::mem::replace(&mut job.frontend, Frontend::None);
    fe.update(job, &payload).await;
    job.frontend = fe;
}

async fn persist_side_effects(db: &JobDb, job_id: u64, payload: &MessagePayload, stage: Option<Stage>) {
    let MessagePayload::Progress(id, args) = payload else { return; };
    if *id == ENCODE_PROG {
        let frame = args.get(1).cloned().unwrap_or_default();
        let total = args.get(2).cloned().unwrap_or_default();
        let v = serde_json::json!({
            "type": "encode", "frame": frame, "total": total,
            "fps": args.get(3), "kbps": args.get(4),
            "percent": encode_percent(&frame, &total),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == ENCODE_CONCAT_PROG {
        let frame = args.get(0).cloned().unwrap_or_default();
        let total = args.get(1).cloned().unwrap_or_default();
        let v = serde_json::json!({
            "type": "encode", "frame": frame, "total": total,
            "fps": args.get(2), "percent": encode_percent(&frame, &total),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == TORRENT_PROG {
        let v = serde_json::json!({
            "type": "download",
            "percent": args.get(0).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
            "done": args.get(1), "total": args.get(2),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == TORRENT_PROG_SELECT {
        let v = serde_json::json!({
            "type": "download",
            "percent": args.get(0).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
            "done": args.get(1),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == UPLOAD_PROG {
        let v = serde_json::json!({
            "type": "upload",
            "percent": upload_percent(args),
            "hosts": args,
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == PROBE_ROW {
        let files = args.get(0).cloned().unwrap_or_default();
        let v = serde_json::json!({ "type": "probe", "files": files, "file_options": parse_probe_options(&files) });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == UPLOAD_DONE {
        let v = serde_json::json!({
            "drive": args.get(0), "doodstream": args.get(1), "lulustream": args.get(2),
            "voe": args.get(3), "abyss": args.get(4),
        });
        db.update_links(job_id, &v.to_string()).await.ok();
    } else if *id == UPLOAD_BACKUP_PROG && stage == Some(Stage::Uploaded) {
        let v = serde_json::json!({ "drive": args.get(0) });
        db.update_links(job_id, &v.to_string()).await.ok();
    } else if *id == BACKUPALL_PROG && stage == Some(Stage::Uploaded) {
        let v = serde_json::json!({ "episodes": args.get(0) });
        db.update_links(job_id, &v.to_string()).await.ok();
    }
}

fn parse_probe_options(files: &str) -> Vec<serde_json::Value> {
    files
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix('`')?;
            let end = rest.find('`')?;
            let index = &rest[..end];
            let label = line.replace('`', "");
            Some(serde_json::json!({ "index": index, "label": label }))
        })
        .collect()
}

fn encode_percent(frame: &str, total: &str) -> u64 {
    let f = frame.parse::<f64>().unwrap_or(0.0);
    let t = total.parse::<f64>().unwrap_or(0.0);
    if t <= 0.0 { return 0; }
    ((f / t) * 100.0).clamp(0.0, 100.0) as u64
}

fn upload_percent(hosts: &[String]) -> u64 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for h in hosts {
        let h = h.trim();
        if h.is_empty() {
            continue;
        }
        if h.starts_with("http") {
            sum += 100.0;
            n += 1.0;
            continue;
        }
        for tok in h.split_whitespace() {
            if let Some((a, b)) = tok.split_once('/') {
                if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
                    if b > 0.0 {
                        sum += (a / b * 100.0).min(100.0);
                        n += 1.0;
                    }
                    break;
                }
            }
        }
    }
    if n > 0.0 { (sum / n).round() as u64 } else { 0 }
}

fn drive_link_from_payload(payload: &MessagePayload) -> Option<String> {
    let MessagePayload::Progress(id, args) = payload else { return None; };
    if *id == UPLOAD_DONE || *id == UPLOAD_BACKUP_PROG {
        return args.get(0).cloned();
    }
    None
}

async fn cleanup_job(source: &PathBuf, dest: &PathBuf) {
    create_dir_all(dest).await.unwrap();
    let _ = rename(source.join("contents").join("subtitle.ass"), dest.join("subtitle.ass")).await;
    let _ = rename(source.join("contents").join("fetch.torrent"), dest.join("fetch.torrent")).await;
    let _ = rename(source.join("log"), dest.join("log")).await;
    remove_dir_all(source).await.ok();
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
        cleanup_job(&job.directory, &PathBuf::from("DB").join("saved_data").join(job.job_id.to_string())).await;
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
    Probe   = 005,
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
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Cancel,
            frontend: Frontend::None,
        }
    }
    pub fn new_hearts(author: u64, channel_id: u64, job_id: u64, context: Context, msg: Message) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Hearts,
            frontend: Frontend::discord(context, msg),
        }
    }
    pub fn new_gitsync(author: u64, channel_id: u64, job_id: u64, context: Context, msg: Message) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::GitSync,
            frontend: Frontend::discord(context, msg),
        }
    }
    pub fn new_gitsync_api(author: u64, channel_id: u64) -> Self {
        Self {
            author,
            channel_id,
            job_id: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
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
    pub probe_files: Option<Vec<(u64, String, u64)>>,  // (index, name, size)
    pub probe_torrent_path: Option<String>,             // saved .torrent path for later
    pub probe_job_id: Option<u64>,
    pub probe_file_index: Option<u64>,
    pub lang: String,
    pub server_id: Option<u64>,
    pub acix: Option<AcixPublish>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.job_id == other.job_id
    }
}

impl Job {
    pub fn new(author: u64, channel_id: u64, response_id: u64, job_type: JobType, job_id: u64,
            preset: Preset, torrent: TorrentType, attachment: Vec<u8>, context: Context, msg: Message,
            lang: String, server_id: Option<u64>) -> Self {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        Self {
            author, channel_id, response_id, job_type, job_id, preset, torrent, attachment,
            frontend: Frontend::discord(context, msg),
            directory: env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
        }
    }

    pub fn new_api(author: u64, channel_id: u64, job_type: JobType, preset: Preset,
            torrent: TorrentType, attachment: Vec<u8>, lang: String, server_id: Option<u64>) -> Self {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        let job_id = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        Self {
            author, channel_id, response_id: 0, job_type, job_id, preset, torrent, attachment,
            frontend: Frontend::Web,
            directory: env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
