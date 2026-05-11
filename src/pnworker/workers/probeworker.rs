use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use crate::libpnenv::core::get_env;
use crate::libpnenv::standard::{PNCURL, PNP2P};
use crate::libpnp2p::nyaaise::TorrentType;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{CTORRENT_DONE, CTORRENT_FAIL, PROBE_FAIL, PROBE_ROW};
use crate::pnworker::util::{ToolResult, run_tool, string_byte_to_mb};
use crate::pnworker::tools::{PNCURL_TORRENT, PNP2P_PROBE};
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::Stage;
use crate::pnworker::util::PathValue;
use crate::pnworker::core::{CommData, WorkerMsg};

pub type ProbeData = (PathBuf, TorrentType, u64);

pub async fn pn_probeworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_env("env.pandora");
    let pncurl_path = env[PNCURL].clone();
    let pnp2p_path = env[PNP2P].clone();
    'll: loop {
        if let Ok(WorkerMsg::Probe((directory, torrent, job_id))) = rx.try_recv() {
            // Phase 1: if Link, fetch .torrent file first (same as downloadworker)
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

            // Phase 2: run pnp2p --probe, collect file rows, emit them all as one message
            let mut probe_rows: Vec<String> = vec![];
            let result = run_tool(
                &pnp2p_path,
                PNP2P_PROBE,
                &HashMap::from([
                    ("OPCODE",      PathValue::from(arg_opcode.clone())),
                    ("TORRENTTYPE", PathValue::from(format!("--{}", torrent.get_arg()))),
                ]),
                job_id,
                &mut proto,
                |data| {
                    let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                        Some(v) => v,
                        None => return None,
                    };
                    match out {
                        // opcode 4 = probe file row: [index, name, size]
                        4 => {
                            let payload = data.get(1).and_then(|v| v.as_multi())?;
                            let idx  = payload.get(0).and_then(|v| v.as_str()).unwrap_or("?");
                            let name = payload.get(1).and_then(|v| v.as_str()).unwrap_or("?");
                            let size = payload.get(2).and_then(|v| v.as_str()).unwrap_or("?");
                            // Remove bracketed tags like [1080p], [HEVC], [SubGroup] etc
                            let short_name = name
                                .split('-').last()          // everything before first bracket
                                .unwrap_or(&name)
                                .trim()
                                .to_string();
                            let l_i = short_name.char_indices().rev().nth(15).map(|(i, _)| i);
                            match l_i {
                                Some(index) => {
                                    println!("[Pandora Prober] {}", &short_name[index..]);
                                    probe_rows.push(format!("`{}` — {} ({}MB)", idx, &short_name[index..], string_byte_to_mb(size)));
                                },
                                None => {
                                    println!("[Pandora Prober] {}", short_name);
                                    probe_rows.push(format!("`{}` — {} ({}MB)", idx, short_name, string_byte_to_mb(size)));
                                },
                            }
                        }
                        1 => return Some(ToolResult::Success),
                        2 => return Some(ToolResult::Fail),
                        _ => {}
                    }
                    None
                },
            ).await;

            match result {
                ToolResult::Success => {
                    if probe_rows.is_empty() {
                        tx.send((job_id, PROBE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                        continue 'll;
                    }
                    // Emit the whole file list as a single CommData message, stage → Probed
                    // The core loop will format this into the Discord embed
                    let list = probe_rows.join("\n");
                    let msg = format!("{}\n{}", PROBE_ROW, list);
                    tx.send((job_id, msg, Some(Stage::Probed))).await.unwrap();
                }
                ToolResult::Fail => {
                    tx.send((job_id, PROBE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                }
                // Probe has no cancel support (it's fast, no cancelfile needed)
                _ => {
                    tx.send((job_id, PROBE_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                }
            }
            println!("[Pandora Probe] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}
