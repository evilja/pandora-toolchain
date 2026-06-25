use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::{PNCURL, PNP2P};
use crate::libpnp2p::nyaaise::TorrentType;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{CTORRENT_DONE, CTORRENT_FAIL, MessagePayload, PROBE_FAIL, PROBE_ROW};
use crate::pnworker::util::{ToolResult, run_tool, string_byte_to_mb};
use crate::pnworker::tools::{PNCURL_TORRENT, PNP2P_PROBE};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use crate::pnworker::core::Stage;
use crate::pnworker::util::PathValue;
use crate::pnworker::core::{CommData, WorkerMsg};

pub type ProbeData = (PathBuf, TorrentType, u64);

struct ProbeFile {
    idx: String,
    name: String,
    size: String,
}

pub async fn pn_probeworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_pandora_env();
    let pncurl_path = env.get(PNCURL).cloned().unwrap_or_default();
    let pnp2p_path = env.get(PNP2P).cloned().unwrap_or_default();
    'll: loop {
        if let Ok(WorkerMsg::Probe((directory, torrent, job_id))) = rx.try_recv() {
            // Phase 1: if Link, fetch .torrent file first (same as downloadworker)
            let arg_opcode: String;
            match torrent {
                TorrentType::GDrive(_) => {
                    tx.send((job_id, MessagePayload::Static(PROBE_FAIL), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                }
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
                                1 => { tx.try_send((job_id, MessagePayload::Static(CTORRENT_DONE), None)).ok(); }
                                2 => return Some(ToolResult::Fail),
                                _ => {}
                            }
                            None
                        },
                    ).await;

                    match result {
                        ToolResult::Fail => {
                            tx.send((job_id, MessagePayload::Static(CTORRENT_FAIL), Some(Stage::Failed))).await.unwrap();
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

            let mut probe_rows: Vec<ProbeFile> = vec![];
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
                        4 => {
                            let payload = data.get(1).and_then(|v| v.as_multi())?;
                            let idx  = payload.get(0).and_then(|v| v.as_str()).unwrap_or("?");
                            let name = payload.get(1).and_then(|v| v.as_str()).unwrap_or("?");
                            let size = payload.get(2).and_then(|v| v.as_str()).unwrap_or("?");
                            println!("[Pandora Prober] {}", name);
                            probe_rows.push(ProbeFile {
                                idx: idx.to_string(),
                                name: name.to_string(),
                                size: size.to_string(),
                            });
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
                        tx.send((job_id, MessagePayload::Static(PROBE_FAIL), Some(Stage::Failed))).await.unwrap();
                        continue 'll;
                    }
                    let list = format_probe_rows(&probe_rows).join("\n");
                    tx.send((job_id, MessagePayload::Progress(PROBE_ROW, vec![list]), Some(Stage::Probed))).await.unwrap();
                }
                ToolResult::Fail => {
                    tx.send((job_id, MessagePayload::Static(PROBE_FAIL), Some(Stage::Failed))).await.unwrap();
                }
                _ => {
                    tx.send((job_id, MessagePayload::Static(PROBE_FAIL), Some(Stage::Failed))).await.unwrap();
                }
            }
            println!("[Pandora Probe] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}

fn format_probe_rows(rows: &[ProbeFile]) -> Vec<String> {
    let basenames: Vec<String> = rows.iter().map(|r| basename(&r.name)).collect();
    let tokens: Vec<Option<String>> = basenames.iter().map(|n| episode_token(n)).collect();
    let detected = tokens.iter().filter(|t| t.is_some()).count() >= 2;
    rows.iter().zip(basenames.iter()).zip(tokens.iter()).map(|((row, name), token)| {
        if detected {
            if let Some(t) = token {
                return format!("`{}` — E{}", row.idx, t);
            }
        }
        format!("`{}` — {} ({}MB)", row.idx, name, string_byte_to_mb(&row.size))
    }).collect()
}

fn basename(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    Path::new(&normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
        .to_string()
}

fn episode_token(name: &str) -> Option<String> {
    for pattern in [
        r"(?i)\s-\s*(\d{1,4}(?:v\d+)?)\b",
        r"(?i)(?:^|[\s._\-\[])[Ss]\d{1,2}[Ee](\d{1,4}(?:v\d+)?)\b",
        r"(?i)(?:^|[\s._\-\[])[Ee][Pp]?\s*(\d{1,4}(?:v\d+)?)\b",
    ] {
        let re = Regex::new(pattern).unwrap();
        if let Some(caps) = re.captures(name) {
            if let Some(m) = caps.get(1) {
                return Some(m.as_str().to_string());
            }
        }
    }
    None
}
