use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{Receiver};
use tokio::time::sleep;
use tokio::time::Duration;
use crate::libpnp2p::nyaaise::TorrentType;
use crate::pnworker::heartbeat::core::{TypedShrine, Worker};
use crate::pnworker::messages::{QUEUE_TOO_LONG, QUEUED, create_job_embed};
use crate::libpndb::core::JobDb;
use crate::pnworker::presence::{change_presence_job};
use crate::pnworker::pull::git_pull;
use tokio::fs::{File, create_dir_all, remove_dir_all, rename, write};
use std::path::PathBuf;
use std::env;
use serenity::all::{ActivityData, Context, EditMessage, Message, OnlineStatus};
use crate::pnworker::workers::downloadworker::*;
use crate::pnworker::workers::encodeworker::*;
use crate::pnworker::workers::uploadworker::*;
use crate::pnworker::workers::probeworker::*;

pub type CommData = (u64, String, Option<Stage>);

#[derive(Clone)]
pub enum WorkerMsg {
    Download(DownloadData),
    Probe(ProbeData),
    Encode(EncodeData),
    Upload(UploadData),
}

pub const STRUCT: [&str; 3] = ["contents", "work", "log"];

pub async fn pn_worker(mut rx: Receiver<JobClass>) {
    let db = JobDb::new().await.unwrap();
    db.init_schema().await.unwrap();
    db.migrate().await.unwrap();

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
                        job.context.1.edit(&job.context.0, EditMessage::new().content("").embed(create_job_embed(&job, QUEUE_TOO_LONG))).await.unwrap();
                        continue;
                    }
                    match job.job_type {
                        JobType::Encode => {
                            job.context.1.edit(&job.context.0, EditMessage::new().content("").embed(create_job_embed(&job, QUEUED))).await.unwrap();
                            for i in STRUCT {
                                create_dir_all(job.directory.join(i)).await.unwrap();
                            }
                            write(job.directory.join("contents").join("subtitle.ass"), &job.attachment).await.unwrap();
                            shrine.send(&Worker::Download, WorkerMsg::Download((job.directory.clone(), job.torrent.clone(), job.job_id, None))).await.unwrap();
                            job.ready = Stage::Downloading;
                        }
                        JobType::Probe => {
                            job.context.1.edit(&job.context.0, EditMessage::new().content("").embed(create_job_embed(&job, QUEUED))).await.unwrap();
                            for i in STRUCT { create_dir_all(job.directory.join(i)).await.unwrap(); }
                            // no subtitle to write
                            shrine.send(&Worker::Probe, WorkerMsg::Probe((job.directory.clone(), job.torrent.clone(), job.job_id))).await.unwrap();
                            job.ready = Stage::Probing;
                        }
                        JobType::Pancode => {
                            let probe_dir = env::current_dir().unwrap()
                                .join("DB").join("work")
                                .join(job.probe_job_id.unwrap().to_string());

                            job.context.1.edit(&job.context.0, EditMessage::new().content("").embed(create_job_embed(&job, QUEUED))).await.unwrap();
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

                            shrine.send(&Worker::Download, WorkerMsg::Download((
                                job.directory.clone(),
                                job.torrent.clone(),
                                job.job_id,
                                job.probe_file_index,
                            ))).await.unwrap();
                            job.ready = Stage::Downloading;
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
                            if let Some(mut ctx) = halfjob.context {
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
                                ctx.1.edit(&ctx.0, EditMessage::new().content(embed_text)).await.unwrap();
                            }
                        }
                        JobType::GitSync => {
                            if let Some(mut ctx) = halfjob.context {
                                ctx.0.set_presence(Some(ActivityData::custom("Recompiling Pandora.")), OnlineStatus::Idle);
                                shrine.kill().await;
                                let repo_path = std::env::current_dir().unwrap().to_str().unwrap().to_owned();
                                println!("{}", repo_path);
                                match git_pull(&repo_path) {
                                    Ok(_) => ctx.1.edit(&ctx.0, EditMessage::new().content("Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.")).await.unwrap(),
                                    Err(e) => {
                                        println!("{}", e);
                                        ctx.1.edit(&ctx.0, EditMessage::new().content("Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.")).await.unwrap()
                                    },
                                }
                                let _ = remove_dir_all(PathBuf::from("DB").join("work")).await;
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                std::process::exit(0);
                            }
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
                let context_0 = queue[pos].context.0.clone();
                let mut context_1 = queue[pos].context.1.clone();

                context_1.edit(&*context_0, EditMessage::new().content("").embed(
                    create_job_embed(&queue[pos], "Probe timed out. use /pancode within 3 minutes next time.")
                )).await.unwrap();

                cleanup_job(&directory, &PathBuf::from("DB").join("saved_data").join(id.to_string())).await;
                db.archive_job(id).await.unwrap();
                queue.remove(pos);
            }
        }
        if let Some((_, commdata)) = shrine.receive(500).await {
            let qlen = queue.len();
            if let Some(i) = queue.iter_mut().find(|j| j.job_id == commdata.0) {
                if let Some(a) = commdata.2 {
                    i.ready = a;
                    db.update_stage(i.job_id, i.ready).await.unwrap();
                }
                let _ = i.context.1.edit(&*i.context.0, EditMessage::new().content("").embed(create_job_embed(&i, &commdata.1))).await;

                let finished = matches!(i.ready, Stage::Uploaded | Stage::Failed | Stage::Cancelled);
                let probe_job_id = i.probe_job_id;
                let job_id = i.job_id;
                let directory = i.directory.clone();

                if finished {
                    // If this was a pancode job, remove and clean up its probe parent
                    if let Some(probe_id) = probe_job_id {
                        if let Some(probe_pos) = queue.iter().position(|j| j.job_id == probe_id) {
                            let probe = &queue[probe_pos];
                            cleanup_job(&probe.directory.clone(), &PathBuf::from("DB").join("saved_data").join(probe_id.to_string())).await;
                            db.archive_job(probe_id).await.unwrap();
                            queue.remove(probe_pos);
                        }
                    } else {
                        change_presence_job(&i.context.0, (None, qlen.saturating_sub(1))).await;
                    }
                    db.archive_job(job_id).await.unwrap();
                    cleanup_job(&directory, &PathBuf::from("DB").join("saved_data").join(job_id.to_string())).await;
                    // Find and remove by job_id since indices may have shifted after probe removal
                    queue.retain(|j| j.job_id != job_id);
                }
            }
        }
        let qlen = queue.len();
        for (idx, job) in queue.iter_mut().enumerate() {
            // Find the first job that's actively progressing (not parked at Probed)
            //
            if job.ready == Stage::Probed {
                continue;
            }

            if job.ready == Stage::Downloaded {
                shrine.send(&Worker::Encode, WorkerMsg::Encode((job.directory.clone(), job.preset.clone(), job.job_id))).await.unwrap();
                job.ready = Stage::Encoding;
                db.update_stage(job.job_id, Stage::Encoding).await.unwrap();
                change_presence_job(&job.context.0, (Some(idx), qlen)).await;
            } else if job.ready == Stage::Encoded {
                shrine.send(&Worker::Upload, WorkerMsg::Upload((job.directory.clone(), format!("{}.mp4", job.directory.file_name().unwrap_or_default().display()), false, job.job_id))).await.unwrap();
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
                change_presence_job(&job.context.0, (None, qlen)).await;
            }
        }
    }
}

async fn cleanup_job(source: &PathBuf, dest: &PathBuf) {
    create_dir_all(dest).await.unwrap();
    let _ = rename(source.join("contents").join("subtitle.ass"), dest.join("subtitle.ass")).await;
    let _ = rename(source.join("contents").join("fetch.torrent"), dest.join("fetch.torrent")).await;
    let _ = rename(source.join("log"), dest.join("log")).await;
    remove_dir_all(source).await.ok();
}

#[derive(Clone, Debug)]
pub enum Preset {
    PseudoLossless(Option<Vec<String>>),
    Dummy(Option<Vec<String>>),
    Standard(Option<Vec<String>>),
    Gpu(Option<Vec<String>>),

}

#[derive(Copy, Clone, Debug)]
#[repr(u16)]
pub enum JobType {
    Encode = 001,
    Cancel = 002,
    Hearts = 003,
    GitSync = 004,
    Probe   = 005,   // ← new
    Pancode = 006,   // ← new (probe-seeded encode)
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Stage {
    Queued,
    Probing,      // ← new
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
    pub context: Option<(Arc<Context>, Message)>,
}

impl HalfJob {
    pub fn new_cancel(author: u64, channel_id: u64, job_id: u64) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Cancel,
            context: None,
        }
    }
    pub fn new_hearts(author: u64, channel_id: u64, job_id: u64, context: Context, msg: Message) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::Hearts,
            context: Some((Arc::new(context), msg)),
        }
    }
    pub fn new_gitsync(author: u64, channel_id: u64, job_id: u64, context: Context, msg: Message) -> Self {
        Self {
            author,
            channel_id,
            job_id,
            requested_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)),
            job_type: JobType::GitSync,
            context: Some((Arc::new(context), msg)),
        }
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
    pub attachment: Vec<u8>,
    pub context: (Arc<Context>, Message),
    pub directory: PathBuf,
    pub ready: Stage,
    pub probe_files: Option<Vec<(u64, String, u64)>>,  // (index, name, size)
    pub probe_torrent_path: Option<String>,             // saved .torrent path for later
    pub probe_job_id: Option<u64>,
    pub probe_file_index: Option<u64>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.job_id == other.job_id
    }
}

impl Job {
    pub fn new(author: u64, channel_id: u64, response_id: u64, job_type: JobType, job_id: u64,
            preset: Preset, torrent: TorrentType, attachment: Vec<u8>, context: Context, msg: Message) -> Self {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        Self {
            author, channel_id, response_id, job_type, job_id, preset, torrent, attachment,
            context: (Arc::new(context), msg),
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
