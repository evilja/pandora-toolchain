use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::{PNCURL, PNP2P};
use crate::libpnp2p::nyaaise::TorrentType;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::core::Stage;
use crate::pnworker::core::{CommData, WorkerMsg};
use crate::pnworker::messages::{
    CTORRENT_DONE, CTORRENT_FAIL, JOB_CANCELLED, MessagePayload, TORRENT_DONE, TORRENT_FAIL,
    TORRENT_PROG, TORRENT_PROG_SELECT,
};
use crate::pnworker::tools::{PNCURL_GSCRAPE, PNCURL_TORRENT, PNP2P_SELECT, PNP2P_TORRENT};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{ToolResult, run_tool};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs::{create_dir_all, rename};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};

pub type DownloadData = (PathBuf, TorrentType, u64, Option<u64>, bool);

pub async fn pn_dloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_pandora_env();
    let pncurl_path = env.get(PNCURL).cloned().unwrap_or_default();
    let pnp2p_path = env.get(PNP2P).cloned().unwrap_or_default();
    'll: loop {
        if let Ok(WorkerMsg::Download((directory, torrent, job_id, file_index, preserve_all))) =
            rx.try_recv()
        {
            let arg_opcode: String;
            match torrent {
                TorrentType::GDrive(ref link) => {
                    let torrent_dir = directory.join("contents").join("torrent");
                    if let Err(e) = create_dir_all(&torrent_dir).await {
                        eprintln!("[Pandora Downloader] Failed to create torrent dir: {e}");
                        tx.send((
                            job_id,
                            MessagePayload::Static(TORRENT_FAIL),
                            Some(Stage::Failed),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }
                    let target_path = torrent_dir.join("input.mkv");

                    let result = run_tool(
                        &pncurl_path,
                        PNCURL_GSCRAPE,
                        &HashMap::from([
                            ("LINK", PathValue::from(link.clone())),
                            ("OPCODE", PathValue::from(target_path.display().to_string())),
                            (
                                "LOGFILE",
                                PathValue::from(
                                    directory
                                        .join("log")
                                        .join(format!("PNcurlGS{}.log", job_id))
                                        .display()
                                        .to_string(),
                                ),
                            ),
                            (
                                "CANCELFILE",
                                PathValue::from(directory.join("CANCEL").display().to_string()),
                            ),
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
                                    let percent =
                                        payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let progmb =
                                        payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    let totlmb =
                                        payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                                    tx.try_send((
                                        job_id,
                                        MessagePayload::Progress(
                                            TORRENT_PROG,
                                            vec![
                                                percent.to_string(),
                                                string_byte_to_mb(progmb).to_string(),
                                                string_byte_to_mb(totlmb).to_string(),
                                            ],
                                        ),
                                        None,
                                    ))
                                    .ok();
                                }
                                1 => return Some(ToolResult::Success),
                                2 => return Some(ToolResult::Fail),
                                3 => return Some(ToolResult::Cancel),
                                _ => {}
                            }
                            None
                        },
                    )
                    .await;

                    match result {
                        ToolResult::Success => {
                            tx.send((
                                job_id,
                                MessagePayload::Static(TORRENT_DONE),
                                Some(Stage::Downloaded),
                            ))
                            .await
                            .unwrap();
                        }
                        ToolResult::Fail => {
                            tx.send((
                                job_id,
                                MessagePayload::Static(TORRENT_FAIL),
                                Some(Stage::Failed),
                            ))
                            .await
                            .unwrap();
                        }
                        ToolResult::Cancel => {
                            tx.send((
                                job_id,
                                MessagePayload::Static(JOB_CANCELLED),
                                Some(Stage::Cancelled),
                            ))
                            .await
                            .unwrap();
                        }
                    }
                    println!("[Pandora Downloader] End of Session");
                    continue 'll;
                }
                TorrentType::Link(ref link) => {
                    let fetch_torrent = directory.join("contents").join("fetch.torrent");
                    if !link.is_empty() || !fetch_torrent.exists() {
                        let result = run_tool(
                            &pncurl_path,
                            PNCURL_TORRENT,
                            &HashMap::from([
                                ("LINK", PathValue::from(link.clone())),
                                (
                                    "OPCODE",
                                    PathValue::from(fetch_torrent.display().to_string()),
                                ),
                                (
                                    "LOGFILE",
                                    PathValue::from(
                                        directory
                                            .join("log")
                                            .join(format!("PNcurl{}.log", job_id))
                                            .display()
                                            .to_string(),
                                    ),
                                ),
                            ]),
                            job_id,
                            &mut proto,
                            |data| {
                                let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                                    Some(v) => v,
                                    None => return None,
                                };
                                match out {
                                    1 => {
                                        tx.try_send((
                                            job_id,
                                            MessagePayload::Static(CTORRENT_DONE),
                                            None,
                                        ))
                                        .ok();
                                    }
                                    2 => return Some(ToolResult::Fail),
                                    _ => {}
                                }
                                None
                            },
                        )
                        .await;

                        match result {
                            ToolResult::Fail => {
                                tx.send((
                                    job_id,
                                    MessagePayload::Static(CTORRENT_FAIL),
                                    Some(Stage::Failed),
                                ))
                                .await
                                .unwrap();
                                continue 'll;
                            }
                            _ => {}
                        }
                    }
                    arg_opcode = fetch_torrent.display().to_string();
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
                            ("OPCODE", PathValue::from(arg_opcode.clone())),
                            (
                                "TORRENTTYPE",
                                PathValue::from(format!("--{}", torrent.get_arg())),
                            ),
                            ("SAVE", PathValue::from(torrent_dir.display().to_string())),
                            (
                                "CANCELFILE",
                                PathValue::from(directory.join("CANCEL").display().to_string()),
                            ),
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
                                    let percent =
                                        payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let progmb =
                                        payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    let totlmb =
                                        payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                                    tx.try_send((
                                        job_id,
                                        MessagePayload::Progress(
                                            TORRENT_PROG,
                                            vec![
                                                percent.to_string(),
                                                string_byte_to_mb(progmb).to_string(),
                                                string_byte_to_mb(totlmb).to_string(),
                                            ],
                                        ),
                                        None,
                                    ))
                                    .ok();
                                }
                                1 => return Some(ToolResult::Success),
                                2 => return Some(ToolResult::Fail),
                                3 => return Some(ToolResult::Cancel),
                                _ => {}
                            }
                            None
                        },
                    )
                    .await
                }
                Some(idx) => {
                    run_tool(
                        &pnp2p_path,
                        PNP2P_SELECT,
                        &HashMap::from([
                            ("OPCODE", PathValue::from(arg_opcode.clone())),
                            (
                                "TORRENTTYPE",
                                PathValue::from(format!("--{}", torrent.get_arg())),
                            ),
                            ("SAVE", PathValue::from(torrent_dir.display().to_string())),
                            ("INDEX", PathValue::from(idx.to_string())),
                            (
                                "CANCELFILE",
                                PathValue::from(directory.join("CANCEL").display().to_string()),
                            ),
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
                                    let percent =
                                        payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let progmb =
                                        payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    tx.try_send((
                                        job_id,
                                        MessagePayload::Progress(
                                            TORRENT_PROG_SELECT,
                                            vec![
                                                percent.to_string(),
                                                string_byte_to_mb(progmb).to_string(),
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
                                    if let Some(name) = data.get(1).and_then(|v| v.as_str()) {
                                        targeted_file = Some(name.to_string());
                                    }
                                }
                                _ => {}
                            }
                            None
                        },
                    )
                    .await
                }
            };

            match result {
                ToolResult::Success => {
                    let mkv_files = find_mkv_files(&torrent_dir).await;

                    if mkv_files.is_empty() {
                        eprintln!("No .mkv file found in downloaded torrent");
                        tx.send((
                            job_id,
                            MessagePayload::Static(TORRENT_FAIL),
                            Some(Stage::Failed),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }
                    if preserve_all {
                        tx.send((
                            job_id,
                            MessagePayload::Static(TORRENT_DONE),
                            Some(Stage::Downloaded),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }

                    let mut largest_path = mkv_files[0].clone();
                    let mut largest_size = tokio::fs::metadata(&largest_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    for path in &mkv_files[1..] {
                        let size = tokio::fs::metadata(path)
                            .await
                            .map(|m| m.len())
                            .unwrap_or(0);
                        if size > largest_size {
                            largest_size = size;
                            largest_path = path.clone();
                        }
                    }

                    let target = torrent_dir.join("input.mkv");

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
                            let mkv_files = find_mkv_files(&torrent_dir).await;
                            if mkv_files.is_empty() {
                                tx.send((
                                    job_id,
                                    MessagePayload::Static(TORRENT_FAIL),
                                    Some(Stage::Failed),
                                ))
                                .await
                                .unwrap();
                                continue 'll;
                            }
                            let mut largest = mkv_files[0].clone();
                            let mut max_sz = tokio::fs::metadata(&largest)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0);
                            for path in mkv_files {
                                let sz = tokio::fs::metadata(&path)
                                    .await
                                    .map(|m| m.len())
                                    .unwrap_or(0);
                                if sz > max_sz {
                                    max_sz = sz;
                                    largest = path;
                                }
                            }
                            largest
                        }
                    };

                    println!(
                        "[Pandora Downloader] Selected file: {}",
                        &final_source.to_string_lossy().to_string()
                    );
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

                    tx.send((
                        job_id,
                        MessagePayload::Static(TORRENT_DONE),
                        Some(Stage::Downloaded),
                    ))
                    .await
                    .unwrap();
                }
                ToolResult::Fail => {
                    tx.send((
                        job_id,
                        MessagePayload::Static(TORRENT_FAIL),
                        Some(Stage::Failed),
                    ))
                    .await
                    .unwrap();
                }
                ToolResult::Cancel => {
                    tx.send((
                        job_id,
                        MessagePayload::Static(JOB_CANCELLED),
                        Some(Stage::Cancelled),
                    ))
                    .await
                    .unwrap();
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
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mkv"))
                .unwrap_or(false)
            {
                result.push(path);
            }
        }
    }
    result
}
