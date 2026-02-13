use std::alloc::System;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use serenity::all::UserId;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::sleep;
use tokio::time::Duration;
use crate::libpnprotocol::core::{Protocol, TypeC};
use crate::pnworker::messages::{self, CTORRENT_DONE, CTORRENT_FAIL, ENCODE_DONE, ENCODE_FAIL, ENCODE_PROG, TORRENT_DONE, TORRENT_FAIL, TORRENT_PROG};
use crate::libpndb::core::JobDb;
use tokio::fs::{write, create_dir_all, read_dir, rename};
use std::path::PathBuf;
use std::env;
use tokio::process::Command;
use serenity::{
    prelude::*,
    all::{Message, Context, EditMessage, Ready},
};
// all data types have job id as their last value.
// directory, link
type DownloadData = (PathBuf, String, u64);
// directory, out name, release or gdrive only
type UploadData = (PathBuf, String, bool, u64);
// directory, preset
type EncodeData = (PathBuf, Preset, u64);
// jobid, message, opt stage
type CommData = (u64, String, Option<Stage>);

/*
 *      let ffmpeg = encoder.as_mut();
 *      ffmpeg.out.stderr(Stdio::piped());
 *      ffmpeg.out.stdout(Stdio::null());
 *      let mut child = ffmpeg.out.spawn().expect("Failed to spawn ffmpeg");
 *      let stderr = child.stderr.take().expect("No stderr");
 */



pub async fn pn_dloadworker(mut rx: Receiver<DownloadData>, tx: Sender<CommData>) {
    let mut proto = Protocol::new(vec![1]);
    'll: loop {
        if let Some((directory, link, job_id)) = rx.recv().await {
            let mut negotiated: bool = false;
            let mut pncurl = Command::new("./pncurl");
            pncurl.args(
                ["--link", &link, "--opcode", &directory.join("contents").join("fetch.torrent").to_string_lossy().to_string(),
                 "--negkey", &format!("PNcurlT{}", job_id), "--negotiator", "PNdloadworker", "--negver", "1"]
            );
            pncurl.stderr(Stdio::null());
            pncurl.stdout(Stdio::piped());
            let mut child = pncurl.spawn().expect("Failed to spawn PNcurl");
            let stdout = child.stdout.take().expect("No stdout");
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("{}", line);
                if !negotiated {
                    if let Ok(_) = proto.negotiate(&line) {
                        negotiated = true;
                    }
                } else {
                    if let Ok(data) = proto.extract_data(&line) {
                        match data {
                            TypeC::Multi(indm) => {
                                let mut out = 0;
                                for (i, val) in indm.iter().enumerate() {
                                    if let TypeC::Single(sd) = val {
                                        if i == 0 {
                                            out = sd.value.parse::<u16>().unwrap();
                                        } else {
                                             if out == 1 { // jobid, message, opt stage
                                                 tx.send((job_id, CTORRENT_DONE.to_string(), None)).await.unwrap();
                                             } else if out == 2 {
                                                 tx.send((job_id, CTORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                                                 continue 'll;
                                             }
                                        }
                                    }
                                }
                            }
                            TypeC::Single(_) => {
                                ()
                            }
                        }
                    }
                }
            }

            if !child.wait().await.expect("Failed to wait on child").success() {
                continue 'll;
            }

            let mut pnp2p = Command::new("./pnp2p");
            pnp2p.args(
                ["--opcode", &directory.join("contents").join("fetch.torrent").to_string_lossy().to_string(),
                 "--save", &directory.join("contents").join("torrent").to_string_lossy().to_string(),
                 "--negkey", &format!("PNp2pT{}", job_id), "--negotiator", "PNdloadworker", "--negver", "1"]
            );
            pnp2p.stderr(Stdio::null());
            pnp2p.stdout(Stdio::piped());
            let mut child = pnp2p.spawn().expect("Failed to spawn PNp2p");
            let stdout = child.stdout.take().expect("No stdout");
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            negotiated = false;
            while let Ok(Some(line)) = lines.next_line().await {
                println!("{}", line);
                if !negotiated {
                    if let Ok(_) = proto.negotiate(&line) {
                        negotiated = true;
                    }
                } else {
                    if let Ok(data) = proto.extract_data(&line) {
                        match data {
                            TypeC::Multi(indm) => {
                                let mut out = 0;
                                for (i, val) in indm.iter().enumerate() {
                                    if let TypeC::Single(sd) = val {
                                        if i == 0 {
                                            out = sd.value.parse::<u16>().unwrap();
                                        } else if out == 1 { // [percent, downloaded, total]
                                            println!("torrdone");
                                            // Read the directory
                                            let torrent_dir = directory.join("contents").join("torrent");
                                            let mut entries = read_dir(&torrent_dir).await.unwrap();

                                            // Find the first file (or iterate to find specific one)
                                            if let Some(entry) = entries.next_entry().await.unwrap() {
                                                let old_path = entry.path();
                                                let new_path = torrent_dir.join("input.mkv");

                                                // Rename/move the file
                                                rename(old_path, new_path).await.unwrap();
                                            };
                                            println!("torrdone");
                                            tx.send((job_id, TORRENT_DONE.to_string(), Some(Stage::Downloaded))).await.unwrap();
                                            continue 'll;
                                        } else if out == 2 {
                                            tx.send((job_id, TORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                                        }
                                    } else if let TypeC::Multi(sd) = val {
                                        if i == 1 {
                                            if out == 0 { // jobid, message, opt stage
                                                let mut percent: String = String::new();
                                                let mut progmb: String = String::new();
                                                let mut totlmb: String = String::new();
                                                for (j, jval) in sd.iter().enumerate() {
                                                    if j == 0 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             percent = jvalj.value.clone();
                                                         }
                                                    } else if j == 1 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             progmb = jvalj.value.clone();
                                                         }
                                                    } else if j == 2 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             totlmb = jvalj.value.clone();
                                                         }
                                                    }
                                                }
                                                tx.send((job_id, format!("{} {}% {} {}", TORRENT_PROG, percent, progmb, totlmb), None)).await.unwrap();
                                            }
                                        }
                                    }
                                }
                            }
                            TypeC::Single(_) => {
                                ()
                            }
                        };
                    }
                }
            }
        }
    }
}
pub async fn pn_encdeworker(mut rx: Receiver<EncodeData>, tx: Sender<CommData>) {
    let mut proto = Protocol::new(vec![1]);
    'll: loop {
        if let Some((directory, preset, job_id)) = rx.recv().await {
            let (concat_value, insert) = match preset {
                Preset::PseudoLossless(cc) => {
                    (cc, "pseudolossless")
                }
                Preset::Gpu(cc) => {
                    (cc, "gpu")
                }
                Preset::Standard(cc) => {
                    (cc, "x264")
                }
            };

            let mut negotiated: bool = false;
            let mut pnmpeg = Command::new("./pnmpeg");
            pnmpeg.args(
                ["--input", &directory.join("contents").join("torrent").join("input.mkv").to_string_lossy().to_string(),
                 "--output", &directory.join("work").join("output_noconcat.mp4").to_string_lossy().to_string(),
                 "--ass", &directory.join("contents").join("subtitle.ass").to_string_lossy().to_string(),
                 &format!("--{}", insert),
                 "--negkey", &format!("PNmpeg{}", job_id), "--negotiator", "PNencdeworker", "--negver", "1"]
            );
            pnmpeg.stderr(Stdio::null());
            pnmpeg.stdout(Stdio::piped());
            let mut child = pnmpeg.spawn().expect("Failed to spawn PNmpeg");
            let stdout = child.stdout.take().expect("No stdout");
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            'enc: while let Ok(Some(line)) = lines.next_line().await {
                println!("{}", line);
                if !negotiated {
                    if let Ok(_) = proto.negotiate(&line) {
                        negotiated = true;
                    }
                } else {
                    if let Ok(data) = proto.extract_data(&line) {
                        match data {
                            TypeC::Multi(indm) => {
                                let mut out = 0;
                                for (i, val) in indm.iter().enumerate() {
                                    if let TypeC::Single(sd) = val {
                                        if i == 0 {
                                            out = sd.value.parse::<u16>().unwrap();
                                            if out == 1 {
                                                break 'enc;
                                            } else if out == 2 {
                                                tx.send((job_id, ENCODE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                                                continue 'll;
                                            }
                                        }
                                    } else if let TypeC::Multi(msd) = val {
                                        if i == 1 {
                                            if out == 0 { // jobid, message, opt stage
                                                let mut fps: String = String::new();
                                                let mut frame: String = String::new();
                                                let mut totlframe: String = String::new();
                                                let mut bitrate: String = String::new();
                                                for (j, jval) in msd.iter().enumerate() {
                                                    if j == 0 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             fps = jvalj.value.clone();
                                                         }
                                                    } else if j == 1 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             frame = jvalj.value.clone();
                                                         }
                                                    } else if j == 2 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             totlframe = jvalj.value.clone();
                                                         }
                                                    } else if j == 3 {
                                                         if let TypeC::Single(jvalj) = jval {
                                                             bitrate = jvalj.value.clone();
                                                         }
                                                    }
                                                }
                                                tx.send((job_id, format!("{}\nPHASE: (1/2)\nFPS: {} FRAME: {}/{} \nBITRATE: {}", ENCODE_PROG, fps, frame, totlframe, bitrate), None)).await.unwrap();
                                            }
                                        }
                                    }
                                }
                            }
                            TypeC::Single(_) => {
                                ()
                            }
                        }
                    }
                }
            }
            if let Some(_) = concat_value {
                let mut negotiated: bool = false;
                let mut pnmpeg = Command::new("./pnmpeg");
                pnmpeg.args(
                    ["--input", &directory.join("work").join("output_noconcat.mp4").to_string_lossy().to_string(),
                     "--output", &directory.join("work").join("output.mp4").to_string_lossy().to_string(),
                     "--subinput", &directory.join("contents").join("concat.mp4").to_string_lossy().to_string(),
                     "--concat",
                     "--negkey", &format!("PNmpegC{}", job_id), "--negotiator", "PNdloadworker", "--negver", "1"]
                );
                pnmpeg.stderr(Stdio::null());
                pnmpeg.stdout(Stdio::piped());
                let mut child = pnmpeg.spawn().expect("Failed to spawn PNmpeg");
                let stdout = child.stdout.take().expect("No stdout");
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                'enc_concat: while let Ok(Some(line)) = lines.next_line().await {
                    println!("{}", line);
                    if !negotiated {
                        if let Ok(_) = proto.negotiate(&line) {
                            negotiated = true;
                        }
                    } else {
                        if let Ok(data) = proto.extract_data(&line) {
                            match data {
                                TypeC::Multi(indm) => {
                                    let mut out = 0;
                                    for (i, val) in indm.iter().enumerate() {
                                        if let TypeC::Single(sd) = val {
                                            if i == 0 {
                                                out = sd.value.parse::<u16>().unwrap();
                                                if out == 1 {
                                                    tx.send((job_id, ENCODE_DONE.to_string(), Some(Stage::Encoded))).await.unwrap();
                                                } else if out == 2 {
                                                    tx.send((job_id, ENCODE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                                                    continue 'll;
                                                }
                                            }
                                        } else if let TypeC::Multi(msd) = val {
                                            if i == 1 {
                                                if out == 0 { // jobid, message, opt stage
                                                    let mut fps: String = String::new();
                                                    let mut frame: String = String::new();
                                                    let mut totlframe: String = String::new();
                                                    let mut bitrate: String = String::new();
                                                    for (j, jval) in msd.iter().enumerate() {
                                                        if j == 0 {
                                                             if let TypeC::Single(jvalj) = jval {
                                                                 fps = jvalj.value.clone();
                                                             }
                                                        } else if j == 1 {
                                                             if let TypeC::Single(jvalj) = jval {
                                                                 frame = jvalj.value.clone();
                                                             }
                                                        } else if j == 2 {
                                                             if let TypeC::Single(jvalj) = jval {
                                                                 totlframe = jvalj.value.clone();
                                                             }
                                                        } else if j == 3 {
                                                             if let TypeC::Single(jvalj) = jval {
                                                                 bitrate = jvalj.value.clone();
                                                             }
                                                        }
                                                    }
                                                    tx.send((job_id, format!("{}\nPHASE: (2/2)\nFPS: {} FRAME: {}/{} \nBITRATE: {}", ENCODE_PROG, fps, frame, totlframe, bitrate), None)).await.unwrap();
                                                }
                                            }
                                        }
                                    }
                                }
                                TypeC::Single(_) => {
                                    ()
                                }
                            }
                        }
                    }
                }
            } else {
                rename(&directory.join("work").join("output_noconcat.mp4"), &directory.join("work").join("output.mp4")).await.unwrap();
                tx.send((job_id, ENCODE_DONE.to_string(), Some(Stage::Encoded))).await.unwrap();
                continue 'll;
            }
            tx.send((job_id, ENCODE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
            // TODO: Encode logic here
            // tx.send((job_id, "Encoded".to_string(), Some(Stage::Encoded))).await.unwrap();
        }
    }
}
pub async fn pn_uloadworker(mut rx: Receiver<UploadData>, tx: Sender<CommData>) {
    loop {
        if let Some((directory, out_name, release, job_id)) = rx.recv().await {
            // TODO: Upload logic here
            // tx.send((job_id, "Uploaded".to_string(), Some(Stage::Uploaded))).await.unwrap();
        }
    }
}

const STRUCT: [&str; 2] = [
    "contents",
    "work",
];

pub async fn pn_worker(mut rx: Receiver<Job>) {
    let db = JobDb::new("host=localhost port=5432 user=postgres password=secret dbname=pandora").await.unwrap();
    db.init_schema().await.unwrap();

    let mut queue: Vec<Job> = vec![];
    let (tx_d, mut rx_d): (Sender<DownloadData>, Receiver<DownloadData>) = channel(5);
    let (tx_u, mut rx_u): (Sender<UploadData>, Receiver<UploadData>) = channel(5);
    let (tx_e, mut rx_e): (Sender<EncodeData>, Receiver<EncodeData>) = channel(5);
    let (tx_c, mut rx_c): (Sender<CommData>, Receiver<CommData>) = channel(50);
    tokio::spawn(pn_dloadworker(rx_d, tx_c.clone()));
    tokio::spawn(pn_uloadworker(rx_u, tx_c.clone()));
    tokio::spawn(pn_encdeworker(rx_e, tx_c.clone()));
    loop {
        sleep(Duration::from_millis(200)).await;
        if let Ok(mut job) = rx.try_recv() {
            if queue.len() > 4 {
                match job.context.1.reply(&job.context.0, messages::QUEUE_TOO_LONG).await {
                    _ => ()
                }
                continue;
            }
            match job.job_type {
                JobType::Encode => { // type DownloadData = (PathBuf, String); directory, link
                    let attachments = job.context.1.attachments.get(0).unwrap().download().await;
                    match job.context.1.reply(&job.context.0, messages::QUEUED).await {
                        Ok(msg) => {
                            job.context.1 = msg;
                        }
                        _ => {continue;}
                    };

                    for i in STRUCT {
                        create_dir_all(job.directory.join(i)).await.unwrap();
                    }

                    write(job.directory.join("contents").join("subtitle.ass"),
                        attachments.unwrap()).await.unwrap();
                    tx_d.send((job.directory.clone(), job.link.clone(), job.job_id)).await.unwrap();
                    job.ready = Stage::Downloading;
                }
            };
            db.insert_job(&job).await.unwrap();
            queue.push(job);
        }
        if queue.len() > 0 {
            let job = &mut queue[0];
            if job.ready == Stage::Downloaded { // directory, preset type EncodeData = (PathBuf, Preset);
                tx_e.send((job.directory.clone(), job.preset, job.job_id)).await.unwrap();
                job.ready = Stage::Encoding;
                db.update_stage(job.job_id, Stage::Encoding).await.unwrap();
            } else if job.ready == Stage::Encoded { // directory, out name, release or gdrive only type UploadData = (PathBuf, String, bool);
                tx_u.send((job.directory.clone(), job.directory.file_name().and_then(|name| name.to_str()).unwrap_or("").to_string(), false, job.job_id)).await.unwrap();
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
            } else {
                if let Ok(commdata) = rx_c.try_recv() {
                    for i in queue.iter_mut() {
                        if i.job_id == commdata.0 {
                            i.context.1.edit(&i.context.0, EditMessage::new().content(commdata.1.clone())).await.unwrap();
                            if let Some(a) = commdata.2 {
                                i.ready = a;
                                db.update_stage(i.job_id, i.ready).await.unwrap();
                                if i.ready == Stage::Uploaded || i.ready == Stage::Failed {
                                    db.archive_job(i.job_id).await.unwrap();
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
}
#[derive(Copy, Clone, Debug)]
pub enum Preset {
    PseudoLossless(Option<i16>),
    Standard(Option<i16>),
    Gpu(Option<i16>),
}

#[derive(Copy, Clone, Debug)]
pub enum Concat {
    Default,
    SomeSubs, // two phaser INTRO concat - complex filter
}
#[derive(Copy, Clone, Debug)]
#[repr(u16)]
pub enum JobType {
    Encode = 001
}
#[derive(Copy, Clone, Debug)]
#[derive(PartialEq)]
pub enum Stage {
    Queued,
    Downloading,
    Downloaded,
    Encoding,
    Encoded,
    Uploading,
    Uploaded,
    Failed,
}

#[derive(Clone)]
pub struct Job {
    pub author: String,
    pub requested_at: Duration,
    pub job_type: JobType,
    pub job_id: u64,
    pub preset: Preset,
    pub link: String,
    pub context: (Context, Message),
    pub directory: PathBuf,
    pub ready: Stage,
}
impl Job {
    pub fn new(author: String, job_type: JobType, job_id: u64,
            preset: Preset, link: String, context: (Context, Message)) -> Self {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        let dir_name = format!("{}-{}-{}", author, job_id, requested_at.as_secs());
        let directory = env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("DB")
            .join(&dir_name);

        Self {
            author: author.clone(),
            job_type,
            job_id,
            preset,
            link,
            context,
            directory,
            requested_at,
            ready: Stage::Queued
        }
    }
}

pub struct Handler {
    pub tx: Sender<Job>
}


/*
 * pub struct Job {
     pub author: String,
     pub requested_at: Duration,
     pub job_type: JobType,
     pub job_id: u64,
     pub preset: Preset,
     pub link: String,
     pub context: (Context, Message),
     pub directory: PathBuf,
     pub ready: Stage,
 }
 */
#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, context: Context, msg: Message) {
        if msg.author.id.get() != 944246988575215627 {
            return;
        }
        if msg.content.starts_with("!enc") {
            self.tx.send(
                Job::new(msg.author.id.get().to_string(), JobType::Encode, msg.id.get(), Preset::PseudoLossless(None), "https://nyaa.si/download/2075988.torrent".to_string(), (context, msg))
            ).await.unwrap();
        }
    }
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("Bot ID: {}", ready.user.id);
        println!("Serving {} guilds", ready.guilds.len());
    }
}
