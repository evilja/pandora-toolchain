use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use crate::libpnenv::core::get_env;
use crate::libpnenv::standard::{PNCURL, PNP2P};
use crate::libpnp2p::nyaaise::TorrentType;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{CTORRENT_DONE, CTORRENT_FAIL, JOB_CANCELLED, TORRENT_DONE, TORRENT_FAIL, TORRENT_PROG};
use crate::pnworker::util::{ToolResult, run_tool};
use crate::pnworker::tools::{PNCURL_TORRENT, PNP2P_TORRENT};
use tokio::fs::{read_dir, rename};
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::Stage;
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::core::{CommData, WorkerMsg};

pub type DownloadData = (PathBuf, TorrentType, u64);

pub async fn pn_dloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_env("env.pandora");
    let pncurl_path = env[PNCURL].clone();
    let pnp2p_path = env[PNP2P].clone();
    'll: loop {
        if let Ok(WorkerMsg::Download((directory, torrent, job_id))) = rx.try_recv() {
            let arg_opcode: String;
            match torrent {
                TorrentType::Link(ref link) => {
                    let result = run_tool(
                        &pncurl_path,
                        PNCURL_TORRENT,
                        &HashMap::from([
                            ("LINK",    PathValue::from(link.clone())),
                            ("OPCODE",  PathValue::from(directory.join("contents").join("fetch.torrent").display().to_string())),
                            ("LOGFILE", PathValue::from(directory.join("log").join(format!("PNcurl{}.log", job_id)).display().to_string())),
                        ]),
                        job_id,
                        &mut proto,
                        |data| {
                            let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                                Some(v) => v,
                                None => return None,
                            };
                            match out {
                                1 => { tx.try_send((job_id, CTORRENT_DONE.to_string(), None)).ok(); }
                                2 => return Some(ToolResult::Fail),
                                _ => {}
                            }
                            None
                        },
                    ).await;

                    match result {
                        ToolResult::Fail => {
                            tx.send((job_id, CTORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                            continue 'll;
                        }
                        _ => {}
                    }
                    arg_opcode = directory.join("contents").join("fetch.torrent").display().to_string();
                }
                TorrentType::Magnet(ref magnet) => {
                    arg_opcode = magnet.clone();
                }
            }

            let torrent_dir = directory.join("contents").join("torrent");
            let result = run_tool(
                &pnp2p_path,
                PNP2P_TORRENT,
                &HashMap::from([
                    ("OPCODE",      PathValue::from(arg_opcode.clone())),
                    ("TORRENTTYPE", PathValue::from(format!("--{}", torrent.get_arg()))),
                    ("SAVE",        PathValue::from(torrent_dir.display().to_string())),
                    ("CANCELFILE",  PathValue::from(directory.join("CANCEL").display().to_string())),
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
                            let percent = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let progmb  = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlmb  = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, format!("{} {}% {}MB/{}MB", TORRENT_PROG, percent,
                                string_byte_to_mb(progmb), string_byte_to_mb(totlmb)), None)).ok();
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
                    let mut entries = read_dir(&torrent_dir).await.unwrap();
                    if let Some(entry) = entries.next_entry().await.unwrap() {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        rename(entry.path(), torrent_dir.join("input.mkv")).await.unwrap();
                    }
                    tx.send((job_id, TORRENT_DONE.to_string(), Some(Stage::Downloaded))).await.unwrap();
                }
                ToolResult::Fail => {
                    tx.send((job_id, TORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                }
                ToolResult::Cancel => {
                    tx.send((job_id, JOB_CANCELLED.to_string(), Some(Stage::Cancelled))).await.unwrap();
                }
            }
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}
