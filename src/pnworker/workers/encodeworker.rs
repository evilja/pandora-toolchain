use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;
use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::PNMPEG;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{ENCODE_CONCAT_PROG, ENCODE_DONE, ENCODE_FAIL, ENCODE_PROG, JOB_CANCELLED, MessagePayload};
use crate::pnworker::util::{ToolResult, run_tool};
use crate::pnworker::tools::{PNMPEG_CONCAT, PNMPEG_ENCODE};
use tokio::fs::rename;
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::{Preset, Stage, WorkerMsg};
use crate::pnworker::util::PathValue;
use crate::pnworker::core::CommData;
pub type EncodeData = (PathBuf, Preset, u64);


#[cfg(target_os = "windows")]
use std::env;
#[cfg(target_os = "windows")]
fn path_to_ffmpeg(path: &Path) -> String {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let relative = path.strip_prefix(&current_dir).unwrap_or(path);
    relative.display().to_string().replace('\\', "/")
}
#[cfg(not(target_os = "windows"))]
fn path_to_ffmpeg(path: &Path) -> String {
    path.display().to_string()
}

pub async fn pn_encdeworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let pnmpeg_path = get_pandora_env().get(PNMPEG).cloned().unwrap_or_default();
    'll: loop {
        if let Ok(WorkerMsg::Encode((directory, preset, job_id))) = rx.try_recv() {
            let (concat_value, insert) = match preset {
                Preset::PseudoLossless(cc) => (cc, "pseudolossless"),
                Preset::Gpu(cc)            => (cc, "gpu"),
                Preset::Standard(cc)       => (cc, "x264"),
                Preset::Dummy(cc)          => (cc, "dummy"),
            };
            let intro_q = if concat_value.is_some() { 2 } else { 1 };

            let result = run_tool(
                &pnmpeg_path,
                PNMPEG_ENCODE,
                &HashMap::from([
                    ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("contents").join("torrent").join("input.mkv").as_path()))),
                    ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                    ("ASS",        PathValue::from(path_to_ffmpeg(directory.join("contents").join("subtitle.ass").as_path()))),
                    ("PRESET",     PathValue::from(format!("--{}", insert))),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                    ("LOGFILE",    PathValue::from(directory.join("log").join(format!("PNmpeg_Encode{}.log", job_id)).display().to_string())),
                ]),
                job_id,
                &mut proto,
                |data| {
                    let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                        Some(v) => v,
                        None => return None,
                    };
                    match out {
                        0 => {
                            let payload = data.get(1).and_then(|v| v.as_multi())?;
                            let fps       = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let frame     = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                            let bitrate   = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, MessagePayload::Progress(ENCODE_PROG, vec![
                                intro_q.to_string(),
                                frame.to_string(),
                                totlframe.to_string(),
                                fps.to_string(),
                                bitrate.to_string(),
                            ]), None)).ok();
                        }
                        1 => return Some(ToolResult::Success),
                        2 => return Some(ToolResult::Fail),
                        3 => return Some(ToolResult::Cancel),
                        _ => {}
                    }
                    None
                },
            ).await;

            match result {
                ToolResult::Fail => {
                    tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                }
                ToolResult::Cancel => {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                ToolResult::Success => {}
            }

            if let Some(ref candidates) = concat_value {
                let result = run_tool(
                    &pnmpeg_path,
                    PNMPEG_CONCAT,
                    &HashMap::from([
                        ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                        ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()))),
                        ("CANDIDATES", PathValue::from(candidates.clone())),
                        ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                        ("LOGFILE",    PathValue::from(directory.join("log").join(format!("PNmpeg_Concat{}.log", job_id)).display().to_string())),
                    ]),
                    job_id,
                    &mut proto,
                    |data| {
                        let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                            Some(v) => v,
                            None => return None,
                        };
                        match out {
                            0 => {
                                let payload = data.get(1).and_then(|v| v.as_multi())?;
                                let fps       = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                let frame     = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                                let bitrate   = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, MessagePayload::Progress(ENCODE_CONCAT_PROG, vec![
                                frame.to_string(),
                                totlframe.to_string(),
                                fps.to_string(),
                                bitrate.to_string(),
                            ]), None)).ok();
                            }
                            1 => return Some(ToolResult::Success),
                            2 => return Some(ToolResult::Fail),
                            3 => return Some(ToolResult::Cancel),
                            _ => {}
                        }
                        None
                    },
                ).await;

                match result {
                    ToolResult::Success => {
                        tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.unwrap();
                    }
                    ToolResult::Fail => {
                        tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    }
                    ToolResult::Cancel => {
                        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    }
                }
            } else {
                rename(
                    directory.join("work").join("output_noconcat.mp4"),
                    directory.join("work").join("output.mp4"),
                ).await.unwrap();
                tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.unwrap();
            }
            println!("[Pandora Encoder] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}
