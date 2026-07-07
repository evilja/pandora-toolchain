use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;
use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::PNMPEG;
use crate::lib::protocol::core::Protocol;
use crate::pnworker::messages::{ENCODE_CONCAT_PROG, ENCODE_DONE, ENCODE_FAIL, ENCODE_PROG, ENCODE_START, ENCODE_WARNING, JOB_CANCELLED, MessagePayload};
use crate::pnworker::util::{ToolResult, job_cancelled, run_tool};
use crate::pnworker::tools::{PNMPEG_CONCAT, PNMPEG_ENCODE, PNMPEG_JOIN, PNMPEG_JOIN_ASS};
use tokio::fs::rename;
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::{KeepKind, Preset, Stage, WorkerMsg};
use crate::pnworker::util::PathValue;
use crate::pnworker::core::CommData;
pub type EncodeData = (PathBuf, Preset, u64, Option<u64>);
pub type KeycodeData = (PathBuf, Vec<PathBuf>, KeepKind, u64, Option<u64>);


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
        if let Ok(msg) = rx.try_recv() {
            if let WorkerMsg::Keycode((directory, inputs, kind, job_id, _server_id)) = msg {
                let Some(first) = inputs.first() else {
                    tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                };
                let rest = inputs.iter().skip(1).map(|p| path_to_ffmpeg(p)).collect::<Vec<_>>();
                let (spec, mode) = match kind {
                    KeepKind::Encode => (PNMPEG_JOIN, "--joinconcat"),
                    KeepKind::Backup => (PNMPEG_JOIN_ASS, "--joinass"),
                };
                let mut params = HashMap::from([
                    ("INPUT", PathValue::from(path_to_ffmpeg(first))),
                    ("OUTPUT", PathValue::from(path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()))),
                    ("CANDIDATES", PathValue::from(rest)),
                    ("MODE", PathValue::from(mode.to_string())),
                    ("NEGKEY", PathValue::from("pn-encode-main".to_string())),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                    ("LOGFILE", PathValue::from(directory.join("log").join(format!("PNmpeg_Keycode{}.log", job_id)).display().to_string())),
                ]);
                if kind == KeepKind::Backup {
                    params.insert("ASS", PathValue::from(path_to_ffmpeg(directory.join("contents").join("subtitle.ass").as_path())));
                }
                if job_cancelled(&directory) {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                tx.try_send((job_id, MessagePayload::Static(ENCODE_START), Some(Stage::Encoding))).ok();
                let result = run_tool(
                    &pnmpeg_path,
                    spec,
                    &params,
                    job_id,
                    &mut proto,
                    |data| keycode_progress(data, job_id, &tx),
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
                continue 'll;
            }
            let WorkerMsg::Encode((directory, preset, job_id, server_id)) = msg else {
                continue 'll;
            };
            let (concat_value, insert) = match preset {
                Preset::PseudoLossless(cc) => (cc, "pseudolossless"),
                Preset::Gpu(cc)            => (cc, "gpu"),
                Preset::Standard(cc)       => (cc, "x264"),
                Preset::Dummy(cc)          => (cc, "dummy"),
            };
            let intro_q = if concat_value.is_some() { 2 } else { 1 };
            let fontconfig_dir = PathBuf::from("DB").join("fontconfig").join(
                server_id.map(|id| id.to_string()).unwrap_or_else(|| "global".to_string())
            );
            tokio::fs::create_dir_all(&fontconfig_dir).await.ok();

            if job_cancelled(&directory) {
                tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                continue 'll;
            }
            tx.try_send((job_id, MessagePayload::Static(ENCODE_START), Some(Stage::Encoding))).ok();
            let result = run_tool(
                &pnmpeg_path,
                PNMPEG_ENCODE,
                &HashMap::from([
                    ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("contents").join("torrent").join("input.mkv").as_path()))),
                    ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                    ("ASS",        PathValue::from(path_to_ffmpeg(directory.join("contents").join("subtitle.ass").as_path()))),
                    ("FONTCONFIG", PathValue::from(path_to_ffmpeg(fontconfig_dir.as_path()))),
                    ("PRESET",     PathValue::from(format!("--{}", insert))),
                    ("NEGKEY",     PathValue::from("pn-encode-main".to_string())),
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
                        4 => {
                            if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                                tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![
                                    warning.to_string(),
                                ]), None)).ok();
                            }
                        }
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
                if job_cancelled(&directory) {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                let result = run_tool(
                    &pnmpeg_path,
                    PNMPEG_CONCAT,
                    &HashMap::from([
                        ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                        ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()))),
                        ("CANDIDATES", PathValue::from(candidates.clone())),
                        ("NEGKEY",     PathValue::from("pn-encode-main".to_string())),
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
                            4 => {
                                if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                                    tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![
                                        warning.to_string(),
                                    ]), None)).ok();
                                }
                            }
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

fn keycode_progress(
    data: &crate::lib::protocol::core::TypeC,
    job_id: u64,
    tx: &Sender<CommData>,
) -> Option<ToolResult> {
    let out: u16 = match data.get(0).and_then(|v| v.parse()) {
        Some(v) => v,
        None => return None,
    };
    match out {
        0 => {
            let payload = data.get(1).and_then(|v| v.as_multi())?;
            let fps = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
            let frame = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
            let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
            let bitrate = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
            tx.try_send((
                job_id,
                MessagePayload::Progress(
                    ENCODE_CONCAT_PROG,
                    vec![
                        frame.to_string(),
                        totlframe.to_string(),
                        fps.to_string(),
                        bitrate.to_string(),
                    ],
                ),
                None,
            ))
            .ok();
        }
        1 => return Some(ToolResult::Success),
        2 => return Some(ToolResult::Fail),
        3 => return Some(ToolResult::Cancel),
        4 => {
            if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                tx.try_send((
                    job_id,
                    MessagePayload::Progress(ENCODE_WARNING, vec![warning.to_string()]),
                    None,
                ))
                .ok();
            }
        }
        _ => {}
    }
    None
}
