use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use crate::libpnenv::core::get_env;
use crate::libpnenv::standard::{PNCURL, PNP2P};
use crate::libpnp2p::nyaaise::TorrentType;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{CTORRENT_DONE, CTORRENT_FAIL, JOB_CANCELLED, TORRENT_DONE, TORRENT_FAIL, TORRENT_PROG};
use crate::pnworker::util::{ToolResult, run_tool};
use crate::pnworker::tools::{PNCURL_TORRENT, PNP2P_TORRENT, PNP2P_SELECT};
use tokio::fs::{rename};
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::Stage;
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::core::{CommData, WorkerMsg};

pub type DownloadData = (PathBuf, TorrentType, u64, Option<u64>);

pub async fn pn_dloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_env("env.pandora");
    let pncurl_path = env[PNCURL].clone();
    let pnp2p_path = env[PNP2P].clone();
    'll: loop {
        if let Ok(WorkerMsg::Download((directory, torrent, job_id, file_index))) = rx.try_recv() {
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

            let mut targeted_file: Option<String> = None;
            
            let result = match file_index {
                None => {
                    run_tool(
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
                    ).await
                }
                Some(idx) => {
                    run_tool(
                        &pnp2p_path,
                        PNP2P_SELECT,
                        &HashMap::from([
                            ("OPCODE",      PathValue::from(arg_opcode.clone())),
                            ("TORRENTTYPE", PathValue::from(format!("--{}", torrent.get_arg()))),
                            ("SAVE",        PathValue::from(torrent_dir.display().to_string())),
                            ("INDEX",       PathValue::from(idx.to_string())),
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
                                    tx.try_send((job_id, format!("{} {}% {}MB", TORRENT_PROG, percent,
                                        string_byte_to_mb(progmb)), None)).ok();
                                }
                                1 => return Some(ToolResult::Success),
                                2 => return Some(ToolResult::Fail),
                                3 => return Some(ToolResult::Cancel),
                                4 => {
                                    if let Some(name) = data.get(1).and_then(|v| v.as_str()) {
                                        targeted_file = Some(name.to_string());
                                    }
                                }
                                _ => {}
                            }
                            None
                        },
                    ).await
                }
            };

            match result {
                ToolResult::Success => {
                    // Find all .mkv files recursively (no closure inside this block)
                    let mkv_files = find_mkv_files(&torrent_dir).await;
                    
                    if mkv_files.is_empty() {
                        eprintln!("No .mkv file found in downloaded torrent");
                        tx.send((job_id, TORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                        continue 'll;
                    }
                    
                    // Pick the largest file (simple heuristic)
                    let mut largest_path = mkv_files[0].clone();
                    let mut largest_size = tokio::fs::metadata(&largest_path).await.map(|m| m.len()).unwrap_or(0);
                    for path in &mkv_files[1..] {
                        let size = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
                        if size > largest_size {
                            largest_size = size;
                            largest_path = path.clone();
                        }
                    }
                    
                    let target = torrent_dir.join("input.mkv");
        
                    // Use the captured filename if we have it, otherwise fallback to largest file scan
                    let source_path = if let Some(ref rel_path) = targeted_file {
                        let full_path = torrent_dir.join(rel_path);
                        if full_path.exists() {
                            Some(full_path)
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let final_source = match source_path {
                        Some(p) => p,
                        None => {
                            // Fallback to your existing "Largest MKV" heuristic
                            let mkv_files = find_mkv_files(&torrent_dir).await;
                            if mkv_files.is_empty() {
                                tx.send((job_id, TORRENT_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                                continue 'll;
                            }
                            let mut largest = mkv_files[0].clone();
                            let mut max_sz = tokio::fs::metadata(&largest).await.map(|m| m.len()).unwrap_or(0);
                            for path in mkv_files {
                                let sz = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
                                if sz > max_sz { max_sz = sz; largest = path; }
                            }
                            largest
                        }
                    };

                    println!("[Pandora Downloader] Selected file: {}", &final_source.to_string_lossy().to_string());
                    rename(&final_source, &target).await.unwrap();
                    
                    // Optionally clean up empty source directory
                    if let Some(parent) = largest_path.parent() {
                        if parent != torrent_dir {
                            let mut parent_entries = tokio::fs::read_dir(parent).await.unwrap();
                            if parent_entries.next_entry().await.unwrap().is_none() {
                                tokio::fs::remove_dir_all(parent).await.ok();
                            }
                        }
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
            println!("[Pandora Downloader] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}

async fn find_mkv_files(root: &PathBuf) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let mut read = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = read.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("mkv") {
                result.push(path);
            }
        }
    }
    result
}
