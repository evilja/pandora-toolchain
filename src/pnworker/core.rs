use std::process::Stdio;
use std::sync::Arc;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::sleep;
use tokio::time::Duration;
use crate::libpnenv::core::get_env;
use crate::libpnenv::standard::{PNCURL, PNMPEG, PNP2P};
use crate::libpnprotocol::core::{Protocol, TypeC};
use crate::pnworker::messages::{CTORRENT_DONE, CTORRENT_FAIL, ENCODE_DONE, ENCODE_FAIL, ENCODE_PROG, QUEUE_TOO_LONG,
                    QUEUED, TORRENT_DONE, TORRENT_FAIL, TORRENT_PROG, UPLOAD_DONE, UPLOAD_FAIL, UPLOAD_PROG, headerize, create_job_embed};
use crate::libpndb::core::JobDb;
use tokio::fs::{copy, create_dir_all, read_dir, rename, write};
use std::path::PathBuf;
use std::{env};
use tokio::process::Command;
use serenity::{
    all::{Message, Context, EditMessage, CreateMessage},
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
    let pncurl_path = get_env("env.pandora".to_string())[PNCURL].clone();
    let pnp2p_path = get_env("env.pandora".to_string())[PNP2P].clone();
    'll: loop {
        if let Some((directory, link, job_id)) = rx.recv().await {
            let mut negotiated: bool = false;
            let mut pncurl = Command::new(&pncurl_path);
            pncurl.args(
                ["--link", &link, "--opcode", &directory.join("contents").join("fetch.torrent").display().to_string(),
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
                tx.send((job_id, CTORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                continue 'll;
            }

            let mut pnp2p = Command::new(&pnp2p_path);
            pnp2p.args(
                ["--opcode", &directory.join("contents").join("fetch.torrent").display().to_string(),
                 "--save", &directory.join("contents").join("torrent").display().to_string(),
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
                                            // Read the directory
                                            let torrent_dir = directory.join("contents").join("torrent");
                                            let mut entries = read_dir(&torrent_dir).await.unwrap();

                                            // Find the first file (or iterate to find specific one)
                                            if let Some(entry) = entries.next_entry().await.unwrap() {
                                                let old_path = entry.path();
                                                let new_path = torrent_dir.join("input.mkv");

                                                // Rename/move the file
                                                tokio::time::sleep(Duration::from_secs(1)).await;
                                                rename(old_path, new_path).await.unwrap();
                                            };
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
                                                tx.send((job_id, format!("{} {}% {}MB/{}MB", TORRENT_PROG, percent,
                                                    string_byte_to_mb(&progmb),
                                                    string_byte_to_mb(&totlmb)),
                                                    None
                                                )).await.unwrap();
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
            if !child.wait().await.expect("Failed to wait on child").success() {
                tx.send((job_id, TORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                continue 'll;
            }
        }
    }
}
#[cfg(target_os = "windows")]
fn path_to_ffmpeg(path: &Path) -> String {
    let relative = absolute_to_relative(path);
    relative.display().to_string().replace('\\', "/")
}
fn absolute_to_relative(path: &Path) -> PathBuf {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Try to strip the current directory prefix to get relative path
    if let Ok(relative) = path.strip_prefix(&current_dir) {
        relative.to_path_buf()
    } else {
        // If it's already relative or can't be stripped, return as-is
        path.to_path_buf()
    }
}
#[cfg(not(target_os = "windows"))]
fn path_to_ffmpeg(path: &Path) -> String {
    path.display().to_string()
}

pub async fn pn_encdeworker(mut rx: Receiver<EncodeData>, tx: Sender<CommData>) {
    let mut proto = Protocol::new(vec![1]);
    let pnmpeg_path = get_env("env.pandora".to_string())[PNMPEG].clone();
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
            let mut pnmpeg = Command::new(&pnmpeg_path);
            pnmpeg.args(
                ["--input", &path_to_ffmpeg(directory.join("contents").join("torrent").join("input.mkv").as_path()),
                 "--output", &path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()),
                 "--ass", &path_to_ffmpeg(directory.join("contents").join("subtitle.ass").as_path()),
                 &format!("--{}", insert),
                 "--negkey", &format!("PNmpeg{}", job_id), "--negotiator", "PNencdeworker", "--negver", "1"]
            );
            pnmpeg.stderr(Stdio::null());
            pnmpeg.stdout(Stdio::piped());
            let mut child = pnmpeg.spawn().expect("Failed to spawn PNmpeg");
            let stdout = child.stdout.take().expect("No stdout");
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let intro_q = match concat_value {
                None => 1,
                Some(_) => 2,
            };
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
                                                tx.send((job_id, format!("{}\nAşama: 1/{}\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {} \nSaniye başına ortalama veri: {}kbit/s", ENCODE_PROG, intro_q, frame, totlframe, fps, bitrate), None)).await.unwrap();
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
                tx.send((job_id, ENCODE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                continue 'll;
            }
            if let Some(_) = concat_value {
                let mut negotiated: bool = false;
                let mut pnmpeg = Command::new(&pnmpeg_path);
                pnmpeg.args(
                    ["--input", &path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()),
                     "--output", &path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()),
                     "--subinput", &path_to_ffmpeg(directory.join("contents").join("concat.mp4").as_path()),
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
                                                    tx.send((job_id, format!("{}\nAşama: 2/2\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {} \nSaniye başına ortalama veri: {}kbit/s", ENCODE_PROG, frame, totlframe, fps, bitrate), None)).await.unwrap();
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

#[inline]
fn string_byte_to_mb(str: &String) -> u16 {
    let bytes = str.parse::<u64>().unwrap_or(1);
    return (bytes / 1024 / 1024) as u16;
}

#[inline]
fn normalize(str: &String) -> String {
    str.replace("?PNslash?", "/")
        .replace("?PNcolon?", ":")
        .replace("?PNpercent?", "%")
}

pub async fn pn_uloadworker(mut rx: Receiver<UploadData>, tx: Sender<CommData>) {
    let mut proto = Protocol::new(vec![1]);
    let pncurl_path = get_env("env.pandora".to_string())[PNCURL].clone();
    'll: loop {
        if let Some((directory, out_name, release, job_id)) = rx.recv().await {
            let mut negotiated: bool = false;
            let mut pncurl = Command::new(&pncurl_path);
            // &directory.join("work").join("output.mp4")
            pncurl.args(
                ["--link", &directory.join("work").join("output.mp4").display().to_string(),
                 "--opcode", &out_name, "--drive", "--env", "env.pandora",
                 "--negkey", &format!("PNcurlG{}", job_id), "--negotiator", "PNuloadworker", "--negver", "1"]
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
                                        } else { // schema = [leaf, [leaf, leaf]]
                                            if out == 1 { // jobid, message, opt stage
                                                tx.send((job_id, format!("{} {}", UPLOAD_DONE, normalize(&sd.value)), Some(Stage::Uploaded))).await.unwrap();
                                            } else if out == 2 {
                                                tx.send((job_id, format!("{} {}", UPLOAD_FAIL, &directory.join("work").join("output.mp4").display().to_string()),
                                                    Some(Stage::Failed))).await.unwrap();
                                                continue 'll;
                                            }
                                        }
                                    } else if let TypeC::Multi(msd) = val {
                                        if out == 0 {
                                            let mut sent_mb: u16 = 0;
                                            let mut totl_mb: u16 = 0;
                                            for (j, jval) in msd.iter().enumerate() {
                                                if j == 0 {
                                                     if let TypeC::Single(jvalj) = jval {
                                                         sent_mb = string_byte_to_mb(&jvalj.value);
                                                     }
                                                } else if j == 1 {
                                                     if let TypeC::Single(jvalj) = jval {
                                                         totl_mb = string_byte_to_mb(&jvalj.value);
                                                     }
                                                }
                                            }
                                            tx.send((job_id, format!("{} {}/{}", UPLOAD_PROG, sent_mb, totl_mb), None)).await.unwrap();
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
                tx.send((job_id, format!("{} {}", UPLOAD_FAIL, &directory.join("work").join("output.mp4").display().to_string()),
                    Some(Stage::Failed))).await.unwrap();
                continue 'll;
            }
        }
    }
}

const STRUCT: [&str; 2] = [
    "contents",
    "work",
];

pub async fn pn_worker(mut rx: Receiver<Job>) {
    let db = JobDb::new().await.unwrap(); // pwd/DB/DATA.db
    db.init_schema().await.unwrap();

    let mut queue: Vec<Job> = vec![];
    let (tx_d, rx_d): (Sender<DownloadData>, Receiver<DownloadData>) = channel(5);
    let (tx_u, rx_u): (Sender<UploadData>, Receiver<UploadData>) = channel(5);
    let (tx_e, rx_e): (Sender<EncodeData>, Receiver<EncodeData>) = channel(5);
    let (tx_c, mut rx_c): (Sender<CommData>, Receiver<CommData>) = channel(50);
    tokio::spawn(pn_dloadworker(rx_d, tx_c.clone()));
    tokio::spawn(pn_uloadworker(rx_u, tx_c.clone()));
    tokio::spawn(pn_encdeworker(rx_e, tx_c.clone()));
    loop {
        sleep(Duration::from_millis(200)).await;
        if let Ok(mut job) = rx.try_recv() {
            if queue.len() > 4 {
                job.ready = Stage::Declined;
                job.context.1.channel_id.send_message(
                    &*job.context.0,
                    CreateMessage::new()
                        .embed(create_job_embed(&job, QUEUE_TOO_LONG))
                ).await.unwrap();
                continue;
            }
            match job.job_type {
                JobType::Encode => { // type DownloadData = (PathBuf, String); directory, link
                    let attachments = job.context.1.attachments.get(0).unwrap().download().await;
                    job.context.1 = job.context.1.channel_id.send_message(
                        &*job.context.0,
                        CreateMessage::new()
                            .embed(create_job_embed(&job, QUEUED))
                    ).await.unwrap();

                    for i in STRUCT {
                        create_dir_all(job.directory.join(i)).await.unwrap();
                    }

                    write(job.directory.join("contents").join("subtitle.ass"),
                        attachments.unwrap()).await.unwrap();
                    match job.preset {
                        Preset::PseudoLossless(a) | Preset::Gpu(a) | Preset::Standard(a) => {
                            match a {
                                None => {}
                                Some(a) => {
                                    match preset_i16_f(a) {
                                        None => (),
                                        Some(string) => {
                                            copy(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("DB").join("concat").join(string).as_os_str(), job.directory.join("contents").join("concat.mp4")).await.unwrap();
                                        }
                                    };
                                }
                            };
                        }
                    };
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
                tx_u.send((job.directory.clone(), format!("{}.mp4", job.directory.file_name().unwrap_or_default().display().to_string()), false, job.job_id)).await.unwrap();
                job.ready = Stage::Uploading;
                db.update_stage(job.job_id, Stage::Uploading).await.unwrap();
            } else {
                if let Ok(commdata) = rx_c.try_recv() {
                    for i in queue.iter_mut() {
                        if i.job_id == commdata.0 {
                            i.context.1.edit(
                                &*i.context.0,
                                EditMessage::new()
                                    .content("")
                                    .embed(create_job_embed(&i, &commdata.1))
                            ).await.unwrap();
                            if let Some(a) = commdata.2 {
                                i.ready = a;
                                db.update_stage(i.job_id, i.ready).await.unwrap();
                                if i.ready == Stage::Uploaded || i.ready == Stage::Failed {
                                    db.archive_job(i.job_id).await.unwrap();
                                    queue.remove(0);
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

fn preset_i16_f(preset: i16) -> Option<String> {
    match preset {
        1 => {Some("somesubs.mp4".to_string())}
        _ => {None}
    }
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
    Declined,
}

#[derive(Clone)]
pub struct Job {
    pub author: u64,
    pub channel_id: u64,
    pub requested_at: Duration,
    pub job_type: JobType,
    pub job_id: u64,
    pub preset: Preset,
    pub link: String,
    pub context: (Arc<Context>, Message),
    pub directory: PathBuf,
    pub ready: Stage,
}
impl Job {
    pub fn new(author: u64, channel_id: u64, job_type: JobType, job_id: u64,
            preset: Preset, link: String, context: Context, msg: Message) -> Self {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
        let dir_name = format!("{}-{}-{}", author, job_id, requested_at.as_secs());
        let directory = env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("DB")
            .join(&dir_name);

        Self {
            author,
            channel_id,
            job_type,
            job_id,
            preset,
            link,
            context: (Arc::new(context), msg),
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
