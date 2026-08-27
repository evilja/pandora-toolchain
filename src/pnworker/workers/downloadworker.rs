use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{PNASS, PNCURL, PNMPEG, PNP2P};
use crate::lib::p2p::nyaaise::TorrentType;
use crate::lib::protocol::core::Protocol;
use crate::pnworker::core::Stage;
use crate::pnworker::core::{CommData, WorkerMsg};
use crate::pnworker::messages::{
    CTORRENT_DONE, CTORRENT_FAIL, JOB_CANCELLED, MessagePayload, TORRENT_DONE,
    TORRENT_DUPLICATE_WAIT, TORRENT_FAIL, TORRENT_FILE_DONE, TORRENT_PROG, TORRENT_PROG_SELECT,
    WORKER_ASSIGN,
};
use crate::pnworker::tools::{
    PNCURL_DIRECT, PNCURL_GSCRAPE, PNCURL_TORRENT, PNP2P_SELECT, PNP2P_SELECTS, PNP2P_TORRENT,
};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{ToolResult, WorkerNamePool, job_cancelled, run_tool};
use crate::pnworker::worker_slots::download_worker_slots;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
#[cfg(test)]
use std::path::MAIN_SEPARATOR;
use tokio::fs::{create_dir_all, rename};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::{Duration, Instant, sleep};

// The index list is empty for a whole-torrent download, one entry for `/encode pan`, and one entry
// per episode for a batch — a batch runs as a single pnp2p process because the info-hash lock
// admits exactly one downloader per torrent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadAot {
    VerySlow,
    Standard,
    PseudoLossless,
    Dummy,
}

pub type DownloadData = (PathBuf, TorrentType, u64, Vec<u64>, bool, Option<DownloadAot>);

// The three pnp2p invocations differ only in how they select files, so the half they share is built
// once. `CliParam::Path` keys are matched by string at runtime, with nothing tying a spec in
// `tools.rs` to the map a call site passes: a key added to one and not the other only shows up as a
// failed job, which is how the selected-file downloads lost their `--logfile` and stopped running.
fn p2p_params(
    directory: &PathBuf,
    torrent_dir: &PathBuf,
    arg_opcode: &str,
    torrent_arg: &str,
    worker_key: &str,
    log_name: &str,
) -> HashMap<&'static str, PathValue> {
    HashMap::from([
        ("OPCODE", PathValue::from(arg_opcode.to_string())),
        (
            "TORRENTTYPE",
            PathValue::from(format!("--{}", torrent_arg)),
        ),
        ("NEGKEY", PathValue::from(worker_key.to_string())),
        ("SAVE", PathValue::from(torrent_dir.display().to_string())),
        (
            "CANCELFILE",
            PathValue::from(directory.join("CANCEL").display().to_string()),
        ),
        (
            "LOGFILE",
            PathValue::from(directory.join("log").join(log_name).display().to_string()),
        ),
        (
            "PREFIX_STATE",
            PathValue::from(directory.join("work").join("download.prefix").display().to_string()),
        ),
    ])
}

pub async fn pn_dloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let env = get_pandora_env();
    let pncurl_path = env.get(PNCURL).cloned().unwrap_or_default();
    let pnp2p_path = env.get(PNP2P).cloned().unwrap_or_default();
    let pnmpeg_path = env.get(PNMPEG).cloned().unwrap_or_default();
    let pnass_path = env.get(PNASS).cloned().unwrap_or_default();
    let mut pool = WorkerNamePool::new(download_worker_slots().await);
    let mut next_slot_refresh = Instant::now() + Duration::from_secs(1);
    let (done_tx, mut done_rx) = channel::<String>(32);
    let mut pending: VecDeque<DownloadData> = VecDeque::new();

    loop {
        if Instant::now() >= next_slot_refresh {
            pool.set_names(download_worker_slots().await);
            next_slot_refresh = Instant::now() + Duration::from_secs(1);
        }
        while let Ok(name) = done_rx.try_recv() {
            pool.release(&name);
        }
        while let Ok(msg) = rx.try_recv() {
            if let WorkerMsg::Download(data) = msg {
                pending.push_back(data);
            }
        }
        loop {
            let Some(name) = pool.acquire() else {
                break;
            };
            let Some(data) = pending.pop_front() else {
                pool.release(&name);
                break;
            };
            let tx2 = tx.clone();
            let done_tx2 = done_tx.clone();
            let pncurl_path2 = pncurl_path.clone();
            let pnp2p_path2 = pnp2p_path.clone();
            let pnmpeg_path2 = pnmpeg_path.clone();
            let pnass_path2 = pnass_path.clone();
            tokio::spawn(async move {
                run_download_job(
                    data,
                    pncurl_path2,
                    pnp2p_path2,
                    pnmpeg_path2,
                    pnass_path2,
                    tx2,
                    name.clone(),
                ).await;
                done_tx2.send(name).await.ok();
            });
        }
        sleep(Duration::from_millis(200)).await;
        pulse.try_send(()).ok();
    }
}

// Download-time encoding is always optional. VerySlow plans/chunks speculatively; the other CPU
// presets keep one linear x264 instance alive so its real rate-control state reaches handoff.
struct DownloadPlanner {
    child: Option<tokio::process::Child>,
    linear: bool,
    prefix_state: PathBuf,
}

impl Drop for DownloadPlanner {
    fn drop(&mut self) {
        let completed_linear = self.linear
            && crate::lib::download_prefix::read_download_prefix(&self.prefix_state)
                .is_ok_and(|state| state.complete);
        if completed_linear {
            // The foreground worker adopts the still-live process through linear-aot.state.
            self.child.take();
        } else if let Some(child) = self.child.as_mut() {
            child.start_kill().ok();
        }
    }
}

async fn start_download_planner(
    directory: &PathBuf,
    pnmpeg_path: &str,
    pnass_path: &str,
    job_id: u64,
    mode: DownloadAot,
) -> Option<DownloadPlanner> {
    if pnmpeg_path.trim().is_empty() {
        return None;
    }
    let watermark = tokio::fs::read(directory.join("contents").join("server_watermark.ass"))
        .await
        .ok();
    let effects = match crate::pnworker::server_effects::server_effects(
        directory,
        watermark.as_deref(),
        pnass_path,
        job_id,
    ).await {
        Ok(effects) => effects,
        Err(e) => {
            eprintln!("[Pandora Downloader] planner subtitle setup skipped: {e}");
            return None;
        }
    };
    let stderr_path = directory.join("log").join(format!("PNmpeg_Plan{}.stderr.log", job_id));
    let stderr = std::fs::File::create(stderr_path).ok()?;
    let worker_root = directory.parent().unwrap_or(directory);
    let foreground_busy = worker_root.join(".foreground-encode");
    let aot_lease = worker_root.join(".aot-owner");
    let mut command = tokio::process::Command::new(pnmpeg_path);
    let linear = mode != DownloadAot::VerySlow;
    let prefix_state = directory.join("work").join("download.prefix");
    if linear {
        let preset = match mode {
            DownloadAot::Standard => "--x264",
            DownloadAot::PseudoLossless => "--pseudolossless",
            DownloadAot::Dummy => "--dummy",
            DownloadAot::VerySlow => unreachable!(),
        };
        command.args([
            "--linear-prefix",
            preset,
            "--aot-job-id",
            &job_id.to_string(),
            "--output",
            &directory.join("work").join("linear-aot-video.mp4").display().to_string(),
        ]);
    } else {
        command.args([
            "--plan-prefix",
            "--veryslow",
            "--output",
            &directory.join("work").join("parallel.plan").display().to_string(),
        ]);
    }
    command.args([
        "--input",
        &directory.join("work").join("download.prefix").display().to_string(),
        "--ass",
        &effects.subtitle.display().to_string(),
        "--cancelfile",
        &directory.join("CANCEL").display().to_string(),
        "--aot-busyfile",
        &foreground_busy.display().to_string(),
        "--aot-lockfile",
        &aot_lease.display().to_string(),
        "--logfile",
        &directory.join("log").join(format!("PNmpeg_Plan{}.log", job_id)).display().to_string(),
    ]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr))
        .kill_on_drop(false);
    match command.spawn() {
        Ok(child) => Some(DownloadPlanner { child: Some(child), linear, prefix_state }),
        Err(e) => {
            eprintln!("[Pandora Downloader] prefix planner could not start: {e}");
            None
        }
    }
}

async fn finish_download_planner(planner: &mut Option<DownloadPlanner>, success: bool) {
    let Some(mut planner) = planner.take() else {
        return;
    };
    let Some(mut child) = planner.child.take() else {
        return;
    };
    if planner.linear && success {
        // The foreground worker will adopt this live encoder through linear-aot.state.
        drop(child);
        return;
    }
    child.start_kill().ok();
    tokio::time::timeout(Duration::from_secs(2), child.wait()).await.ok();
}

async fn run_download_job(
    data: DownloadData,
    pncurl_path: String,
    pnp2p_path: String,
    pnmpeg_path: String,
    pnass_path: String,
    tx: Sender<CommData>,
    worker_name: String,
) {
    let (directory, torrent, job_id, file_indices, preserve_all, aot_mode) = data;
    let mut proto = Protocol::new(vec![1]);
    let worker_key = format!("pn-download-{}", worker_name);
    tx.try_send((
        job_id,
        MessagePayload::Progress(WORKER_ASSIGN, vec![format!("dwl-{}", worker_name)]),
        None,
    ))
    .ok();
    if job_cancelled(&directory) {
        tx.send((
            job_id,
            MessagePayload::Static(JOB_CANCELLED),
            Some(Stage::Cancelled),
        ))
        .await
        .unwrap();
        return;
    }

    let mut planner = if let Some(mode) = aot_mode {
        start_download_planner(&directory, &pnmpeg_path, &pnass_path, job_id, mode).await
    } else {
        None
    };

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
                return;
            }
            let target_path = torrent_dir.join("input.mkv");

            if job_cancelled(&directory) {
                tx.send((
                    job_id,
                    MessagePayload::Static(JOB_CANCELLED),
                    Some(Stage::Cancelled),
                ))
                .await
                .unwrap();
                return;
            }
            let result = run_tool(
                &pncurl_path,
                PNCURL_GSCRAPE,
                &HashMap::from([
                    ("LINK", PathValue::from(link.clone())),
                    ("OPCODE", PathValue::from(target_path.display().to_string())),
                    ("NEGKEY", PathValue::from(worker_key.clone())),
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
                    (
                        "PREFIX_STATE",
                        PathValue::from(directory.join("work").join("download.prefix").display().to_string()),
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
                            let percent = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let progmb = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlmb = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
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
            return;
        }
        TorrentType::Direct(ref link) => {
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
                return;
            }
            let target_path = torrent_dir.join("input.mkv");

            if job_cancelled(&directory) {
                tx.send((
                    job_id,
                    MessagePayload::Static(JOB_CANCELLED),
                    Some(Stage::Cancelled),
                ))
                .await
                .unwrap();
                return;
            }
            let result = run_tool(
                &pncurl_path,
                PNCURL_DIRECT,
                &HashMap::from([
                    ("LINK", PathValue::from(link.clone())),
                    ("OPCODE", PathValue::from(target_path.display().to_string())),
                    ("NEGKEY", PathValue::from(worker_key.clone())),
                    (
                        "LOGFILE",
                        PathValue::from(
                            directory
                                .join("log")
                                .join(format!("PNcurlD{}.log", job_id))
                                .display()
                                .to_string(),
                        ),
                    ),
                    (
                        "CANCELFILE",
                        PathValue::from(directory.join("CANCEL").display().to_string()),
                    ),
                    (
                        "PREFIX_STATE",
                        PathValue::from(directory.join("work").join("download.prefix").display().to_string()),
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
                            let percent = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let progmb = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlmb = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
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
            return;
        }
        TorrentType::Link(ref link) => {
            let fetch_torrent = directory.join("contents").join("fetch.torrent");
            if !link.is_empty() || !fetch_torrent.exists() {
                if job_cancelled(&directory) {
                    tx.send((
                        job_id,
                        MessagePayload::Static(JOB_CANCELLED),
                        Some(Stage::Cancelled),
                    ))
                    .await
                    .unwrap();
                    return;
                }
                let result = run_tool(
                    &pncurl_path,
                    PNCURL_TORRENT,
                    &HashMap::from([
                        ("LINK", PathValue::from(link.clone())),
                        (
                            "OPCODE",
                            PathValue::from(fetch_torrent.display().to_string()),
                        ),
                        ("NEGKEY", PathValue::from(worker_key.clone())),
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
                                tx.try_send((job_id, MessagePayload::Static(CTORRENT_DONE), None))
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
                        return;
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
    let mut duplicate_save_path: Option<String> = None;

    if job_cancelled(&directory) {
        tx.send((
            job_id,
            MessagePayload::Static(JOB_CANCELLED),
            Some(Stage::Cancelled),
        ))
        .await
        .unwrap();
        return;
    }
    let result = match file_indices.split_first() {
        None => {
            run_tool(
                &pnp2p_path,
                PNP2P_TORRENT,
                &p2p_params(
                    &directory,
                    &torrent_dir,
                    &arg_opcode,
                    &torrent.get_arg(),
                    &worker_key,
                    &format!("PNp2p{}.log", job_id),
                ),
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
                            let progmb = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlmb = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
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
                        5 => {
                            duplicate_save_path =
                                data.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
                            return Some(ToolResult::Fail);
                        }
                        _ => {}
                    }
                    None
                },
            )
            .await
        }
        Some((idx, rest)) if !rest.is_empty() => {
            let list = std::iter::once(*idx)
                .chain(rest.iter().copied())
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(",");
            run_tool(
                &pnp2p_path,
                PNP2P_SELECTS,
                &{
                    let mut params = p2p_params(
                        &directory,
                        &torrent_dir,
                        &arg_opcode,
                        &torrent.get_arg(),
                        &worker_key,
                        &format!("PNp2pSelects{}.log", job_id),
                    );
                    params.insert("INDEXES", PathValue::from(list));
                    params
                },
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
                            let progmb = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlmb = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
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
                        // A finished file cannot wait for the rest of the torrent: it is sent
                        // upstream immediately so its encode can be queued.
                        6 => {
                            let payload = data.get(1).and_then(|v| v.as_multi())?;
                            let index = payload.get(0).and_then(|v| v.as_str()).unwrap_or("");
                            let name = payload.get(1).and_then(|v| v.as_str()).unwrap_or("");
                            if !index.is_empty() && !name.is_empty() {
                                tx.try_send((
                                    job_id,
                                    MessagePayload::Progress(
                                        TORRENT_FILE_DONE,
                                        vec![index.to_string(), name.to_string()],
                                    ),
                                    None,
                                ))
                                .ok();
                            }
                        }
                        5 => {
                            duplicate_save_path =
                                data.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
                            return Some(ToolResult::Fail);
                        }
                        _ => {}
                    }
                    None
                },
            )
            .await
        }
        Some((idx, _)) => {
            run_tool(
                &pnp2p_path,
                PNP2P_SELECT,
                &{
                    let mut params = p2p_params(
                        &directory,
                        &torrent_dir,
                        &arg_opcode,
                        &torrent.get_arg(),
                        &worker_key,
                        &format!("PNp2pSelect{}.log", job_id),
                    );
                    params.insert("INDEX", PathValue::from(idx.to_string()));
                    params
                },
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
                            let progmb = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
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
                        5 => {
                            duplicate_save_path =
                                data.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
                            return Some(ToolResult::Fail);
                        }
                        _ => {}
                    }
                    None
                },
            )
            .await
        }
    };

    finish_download_planner(
        &mut planner,
        matches!(result, ToolResult::Success) && duplicate_save_path.is_none(),
    ).await;

    if let Some(path) = duplicate_save_path {
        tx.send((
            job_id,
            MessagePayload::Progress(TORRENT_DUPLICATE_WAIT, vec![path]),
            None,
        ))
        .await
        .unwrap();
        println!("[Pandora Downloader] Duplicate torrent cached, waiting for owner encode");
        return;
    }

    match result {
        ToolResult::Success => {
            let mkv_files = find_mkv_files(&torrent_dir).await;

            if mkv_files.is_empty() {
                eprintln!("No video file found in downloaded torrent");
                tx.send((
                    job_id,
                    MessagePayload::Static(TORRENT_FAIL),
                    Some(Stage::Failed),
                ))
                .await
                .unwrap();
                return;
            }
            if preserve_all {
                tx.send((
                    job_id,
                    MessagePayload::Static(TORRENT_DONE),
                    Some(Stage::Downloaded),
                ))
                .await
                .unwrap();
                return;
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

            let final_source = source_path.unwrap_or(largest_path);
            let source_parent = final_source.parent().map(PathBuf::from);

            println!(
                "[Pandora Downloader] Selected file: {}",
                final_source.display()
            );
            rename(&final_source, &target).await.unwrap();

            if let Some(parent) = source_parent {
                if parent != torrent_dir {
                    let mut parent_entries = tokio::fs::read_dir(&parent).await.unwrap();
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
    return;
}

fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts"
    )
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
                .map(is_video_ext)
                .unwrap_or(false)
            {
                result.push(path);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnworker::util::tool_args;

    // Nothing at compile time connects a spec's `CliParam::Path` keys to the map its caller builds,
    // so every pnp2p spec is walked against the parameters this worker actually passes. Dropping a
    // key does not fail to build — it fails the download, silently, for whichever selection mode
    // lost it.
    #[test]
    fn every_p2p_spec_gets_the_parameters_it_declares() {
        let directory = PathBuf::from("DB/work/7");
        let torrent_dir = directory.join("contents").join("torrent");
        let base = |log: &str| {
            p2p_params(
                &directory,
                &torrent_dir,
                "DB/work/7/contents/fetch.torrent",
                "nomagnet",
                "key",
                log,
            )
        };

        let whole = base("PNp2p7.log");
        let args = tool_args(PNP2P_TORRENT, &whole, 7).unwrap();
        assert!(args.contains(&"DB/work/7/log/PNp2p7.log".replace('/', &MAIN_SEPARATOR.to_string())));

        let mut one = base("PNp2pSelect7.log");
        one.insert("INDEX", PathValue::from("12".to_string()));
        let args = tool_args(PNP2P_SELECT, &one, 7).unwrap();
        assert!(args.contains(&"--select".to_string()));
        assert!(args.contains(&"12".to_string()));

        let mut many = base("PNp2pSelects7.log");
        many.insert("INDEXES", PathValue::from("1,2".to_string()));
        tool_args(PNP2P_SELECTS, &many, 7).unwrap();
    }

    // The map a call site forgets a key in is the one that fails, and it names the key.
    #[test]
    fn a_missing_parameter_names_itself() {
        let mut params = p2p_params(
            &PathBuf::from("DB/work/7"),
            &PathBuf::from("DB/work/7/contents/torrent"),
            "opcode",
            "nomagnet",
            "key",
            "PNp2pSelect7.log",
        );
        params.remove("LOGFILE");
        params.insert("INDEX", PathValue::from("0".to_string()));
        assert_eq!(
            tool_args(PNP2P_SELECT, &params, 7).unwrap_err(),
            "Missing or wrong type for path key: LOGFILE"
        );
    }
}
