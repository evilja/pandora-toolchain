use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    CLIENT_ID, CLIENT_SECRET, ENV_PATH, ENV_SEP, PARENTID, PNCURL, REFRESH_TOKEN,
};
use crate::lib::mpeg::probe::ffprobe_video_height;
use crate::lib::protocol::core::Protocol;
use crate::pnworker::core::{CommData, SmartcodeDriveName};
use crate::pnworker::core::{Stage, WorkerMsg};
use crate::pnworker::messages::{
    BACKUPALL_PROG, JOB_CANCELLED, MessagePayload, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_FAIL,
    UPLOAD_PROG, WORKER_ASSIGN,
};
use crate::pnworker::tools::{PNCURL_BACKUP, PNCURL_BACKUP_FOLDER, PNCURL_UPLOAD, PNCURL_UPLOAD_FOLDER};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{ToolResult, WorkerNamePool, job_cancelled, run_tool};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::sleep;

pub type UploadData = (
    PathBuf,
    String,
    bool,
    u64,
    Option<u64>,
    Option<String>,
    Option<String>,
    Option<SmartcodeDriveName>,
);
pub type UploadAllData = (PathBuf, u64, Option<u64>);

pub async fn pn_uloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let pncurl_path = get_pandora_env().get(PNCURL).cloned().unwrap_or_default();
    let mut pool = WorkerNamePool::new(&["tsuki", "sora", "tenki", "suisei"]);
    let (done_tx, mut done_rx) = channel::<String>(4);
    let mut pending: VecDeque<WorkerMsg> = VecDeque::new();

    loop {
        while let Ok(name) = done_rx.try_recv() {
            pool.release(&name);
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Upload(_) | WorkerMsg::UploadAll(_) => pending.push_back(msg),
                _ => {}
            }
        }
        loop {
            let Some(name) = pool.acquire() else {
                break;
            };
            let Some(msg) = pending.pop_front() else {
                pool.release(&name);
                break;
            };
            let tx2 = tx.clone();
            let done_tx2 = done_tx.clone();
            let pncurl_path2 = pncurl_path.clone();
            tokio::spawn(async move {
                run_upload_job(msg, pncurl_path2, tx2, name.clone()).await;
                done_tx2.send(name).await.ok();
            });
        }
        sleep(Duration::from_millis(200)).await;
        pulse.try_send(()).ok();
    }
}

async fn run_upload_job(
    msg: WorkerMsg,
    pncurl_path: String,
    tx: Sender<CommData>,
    worker_name: String,
) {
    let mut proto = Protocol::new(vec![1]);
    let assign_job_id = match &msg {
        WorkerMsg::Upload((_, _, _, job_id, _, _, _, _)) => Some(*job_id),
        WorkerMsg::UploadAll((_, job_id, _)) => Some(*job_id),
        _ => None,
    };
    if let Some(job_id) = assign_job_id {
        tx.try_send((
            job_id,
            MessagePayload::Progress(WORKER_ASSIGN, vec![format!("upl-{}", worker_name)]),
            None,
        ))
        .ok();
    }
    match msg {
        WorkerMsg::Upload((
            directory,
            out_name,
            release,
            job_id,
            server_id,
            gdrive_folder_global,
            gdrive_folder_local,
            smartcode_drive_name,
        )) => {
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
            let output_path = directory
                .join("work")
                .join("output.mp4")
                .display()
                .to_string();
            let mut completed = 0u8;
            let mut gd_link = "Google Bekleniyor".to_string();
            let mut gd_done = false;
            let mut dood_link = "Doodstream Bekleniyor".to_string();
            let mut dood_done = false;
            let mut lulu_link = "Lulustream Bekleniyor".to_string();
            let mut lulu_done = false;
            let mut voesx_link = "Voe Bekleniyor".to_string();
            let mut voesx_done = false;
            let mut abyss_link = "Abyss Bekleniyor".to_string();
            let mut abyss_done = false;
            let expected_hosts = if release { 5 } else { 1 };

            let is_smartcode = gdrive_folder_local.is_some();
            let drive_env = drive_env_path(&directory, server_id, is_smartcode).await;
            let drive_folder = drive_folder_path(
                drive_env.local_drive,
                is_smartcode,
                server_id,
                if drive_env.local_drive {
                    gdrive_folder_local
                } else {
                    gdrive_folder_global
                },
            );
            let drive_out_name = smartcode_drive_name
                .filter(|_| drive_env.local_drive && drive_env.smartcode_root_set)
                .map(|name| name.filename(&resolution_label(&output_path)))
                .unwrap_or_else(|| out_name.clone());
            let logfile = directory
                .join("log")
                .join(format!("PNcurl_Upload{}.log", job_id))
                .display()
                .to_string();
            println!(
                "[uploadworker] job={} drive_env={} local_drive={} drive_folder={} logfile={}",
                job_id,
                drive_env.path,
                drive_env.local_drive,
                if drive_folder.is_empty() { "(none)" } else { drive_folder.as_str() },
                logfile,
            );

            let spec = if release && !drive_folder.is_empty() {
                PNCURL_UPLOAD_FOLDER
            } else if release {
                PNCURL_UPLOAD
            } else if !drive_folder.is_empty() {
                PNCURL_BACKUP_FOLDER
            } else {
                PNCURL_BACKUP
            };

            let result = run_tool(
                &pncurl_path,
                spec,
                &HashMap::from([
                    ("LINK", PathValue::from(output_path.clone())),
                    ("OPCODE", PathValue::from(out_name.clone())),
                    ("DRIVEOPCODE", PathValue::from(drive_out_name)),
                    ("ENV", PathValue::from(drive_env.path)),
                    ("DRIVEFOLDER", PathValue::from(drive_folder)),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                    ("LOGFILE", PathValue::from(logfile)),
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
                            let total = data.get(1).and_then(|v| v.as_str());
                            let compact = total.is_some();
                            let offset = if compact { 2 } else { 1 };
                            let total = total.unwrap_or("0");
                            let gd_payload = data.get(offset).and_then(|v| v.as_multi())?;
                            let dood_payload = data.get(offset + 1).and_then(|v| v.as_multi())?;
                            let lulu_payload = data.get(offset + 2).and_then(|v| v.as_multi())?;
                            let voesx_payload = data.get(offset + 3).and_then(|v| v.as_multi())?;
                            let abyss_payload = data.get(offset + 4).and_then(|v| v.as_multi())?;
                            let total_index = if compact { 0 } else { 1 };
                            let ext_index = if compact { 1 } else { 2 };
                            let gd_sent = gd_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let gd_totl = if compact {
                                total
                            } else {
                                gd_payload
                                    .get(total_index)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0")
                            };
                            let gd_ext = gd_payload
                                .get(ext_index)
                                .and_then(|v| v.as_str())
                                .unwrap_or("0");
                            let dood_sent =
                                dood_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let dood_totl = if compact {
                                total
                            } else {
                                dood_payload
                                    .get(total_index)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0")
                            };
                            let dood_ext = dood_payload
                                .get(ext_index)
                                .and_then(|v| v.as_str())
                                .unwrap_or("0");
                            let lulu_sent =
                                lulu_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let lulu_totl = if compact {
                                total
                            } else {
                                lulu_payload
                                    .get(total_index)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0")
                            };
                            let lulu_ext = lulu_payload
                                .get(ext_index)
                                .and_then(|v| v.as_str())
                                .unwrap_or("0");
                            let voesx_sent =
                                voesx_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let voesx_totl = if compact {
                                total
                            } else {
                                voesx_payload
                                    .get(total_index)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0")
                            };
                            let voesx_ext = voesx_payload
                                .get(ext_index)
                                .and_then(|v| v.as_str())
                                .unwrap_or("0");
                            let abyss_sent =
                                abyss_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let abyss_totl = if compact {
                                total
                            } else {
                                abyss_payload
                                    .get(total_index)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0")
                            };
                            let abyss_ext = abyss_payload
                                .get(ext_index)
                                .and_then(|v| v.as_str())
                                .unwrap_or("0");
                            if !gd_done {
                                gd_link = upload_progress_text("Google", gd_sent, gd_totl, gd_ext);
                            }
                            if release && !dood_done && dood_totl != "0" {
                                dood_link = upload_progress_text(
                                    "Doodstream",
                                    dood_sent,
                                    dood_totl,
                                    dood_ext,
                                );
                            }
                            if release && !lulu_done && lulu_totl != "0" {
                                lulu_link = upload_progress_text(
                                    "Lulustream",
                                    lulu_sent,
                                    lulu_totl,
                                    lulu_ext,
                                );
                            }
                            if release && !voesx_done && voesx_totl != "0" {
                                voesx_link =
                                    upload_progress_text("Voe", voesx_sent, voesx_totl, voesx_ext);
                            }
                            if release && !abyss_done && abyss_totl != "0" {
                                abyss_link = upload_progress_text(
                                    "Abyss", abyss_sent, abyss_totl, abyss_ext,
                                );
                            }
                            tx.try_send(upload_payload(
                                job_id,
                                release,
                                UPLOAD_PROG,
                                &gd_link,
                                &dood_link,
                                &lulu_link,
                                &voesx_link,
                                &abyss_link,
                                None,
                            ))
                            .ok();
                        }
                        1 => {
                            completed += 1;
                            let host_id = data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let url = data
                                .get(2)
                                .and_then(|v| v.as_str())
                                .unwrap_or("Başarısız")
                                .to_string();
                            match host_id {
                                "1" => {
                                    gd_link = url;
                                    gd_done = true;
                                }
                                "2" => {
                                    dood_link = normalize_doodstream_link(&url);
                                    dood_done = true;
                                }
                                "4" => {
                                    lulu_link = normalize_lulu_link(&url);
                                    lulu_done = true;
                                }
                                "5" => {
                                    voesx_link = normalize_voe_link(&url);
                                    voesx_done = true;
                                }
                                "6" => {
                                    abyss_link = url;
                                    abyss_done = true;
                                }
                                _ => {}
                            }
                            let stage = if completed >= expected_hosts {
                                Some(Stage::Uploaded)
                            } else {
                                None
                            };
                            tx.try_send(upload_payload(
                                job_id,
                                release,
                                UPLOAD_PROG,
                                &gd_link,
                                &dood_link,
                                &lulu_link,
                                &voesx_link,
                                &abyss_link,
                                stage,
                            ))
                            .ok();
                            if completed >= expected_hosts {
                                return Some(ToolResult::Success);
                            }
                        }
                        2 => {
                            completed += 1;
                            let host_id = data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            match host_id {
                                "1" => {
                                    gd_link = "Google Başarısız".to_string();
                                    gd_done = true;
                                }
                                "2" => {
                                    dood_link = "Doodstream Başarısız".to_string();
                                    dood_done = true;
                                }
                                "4" => {
                                    lulu_link = "Lulustream Başarısız".to_string();
                                    lulu_done = true;
                                }
                                "5" => {
                                    voesx_link = "Voe Başarısız".to_string();
                                    voesx_done = true;
                                }
                                "6" => {
                                    abyss_link = "Abyss Başarısız".to_string();
                                    abyss_done = true;
                                }
                                _ => {}
                            }
                            let stage = if completed >= expected_hosts {
                                Some(Stage::Uploaded)
                            } else {
                                None
                            };
                            tx.try_send(upload_payload(
                                job_id,
                                release,
                                UPLOAD_PROG,
                                &gd_link,
                                &dood_link,
                                &lulu_link,
                                &voesx_link,
                                &abyss_link,
                                stage,
                            ))
                            .ok();
                            if completed >= expected_hosts {
                                return Some(ToolResult::Success);
                            }
                        }
                        3 => return Some(ToolResult::Cancel),
                        _ => {}
                    }
                    None
                },
            )
            .await;

            let any_done = gd_done || dood_done || lulu_done || voesx_done || abyss_done;
            match result {
                ToolResult::Success | ToolResult::Fail => {
                    if any_done {
                        tx.send(upload_payload(
                            job_id,
                            release,
                            UPLOAD_DONE,
                            &gd_link,
                            &dood_link,
                            &lulu_link,
                            &voesx_link,
                            &abyss_link,
                            Some(Stage::Uploaded),
                        ))
                        .await
                        .unwrap();
                    } else {
                        tx.send((
                            job_id,
                            MessagePayload::Static(UPLOAD_FAIL),
                            Some(Stage::Failed),
                        ))
                        .await
                        .unwrap();
                    }
                }
                ToolResult::Cancel => {
                    if any_done {
                        tx.send(upload_payload(
                            job_id,
                            release,
                            JOB_CANCELLED,
                            &gd_link,
                            &dood_link,
                            &lulu_link,
                            &voesx_link,
                            &abyss_link,
                            Some(Stage::Cancelled),
                        ))
                        .await
                        .unwrap();
                    } else {
                        tx.send((
                            job_id,
                            MessagePayload::Static(JOB_CANCELLED),
                            Some(Stage::Cancelled),
                        ))
                        .await
                        .unwrap();
                    }
                }
            }
            println!("[Pandora Uploader] End of Session");
            return;
        }
        WorkerMsg::UploadAll((directory, job_id, server_id)) => {
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
            let mut files = find_mkv_files(&directory.join("contents").join("torrent")).await;
            files.sort_by(|a, b| a.display().to_string().cmp(&b.display().to_string()));
            if files.is_empty() {
                tx.send((
                    job_id,
                    MessagePayload::Static(UPLOAD_FAIL),
                    Some(Stage::Failed),
                ))
                .await
                .unwrap();
                return;
            }

            let mut rows: Vec<String> = (0..files.len())
                .map(|i| format!("episode {:02}: Bekleniyor", i + 1))
                .collect();
            let mut any_uploaded = false;
            tx.try_send((
                job_id,
                MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
                None,
            ))
            .ok();

            let drive_env = drive_env_path(&directory, server_id, false).await;
            let drive_folder = drive_folder_path(drive_env.local_drive, false, server_id, None);
            println!(
                "[uploadworker] upload_all job={} drive_env={} local_drive={} drive_folder={}",
                job_id,
                drive_env.path,
                drive_env.local_drive,
                if drive_folder.is_empty() { "(none)" } else { drive_folder.as_str() },
            );
            let spec = if drive_folder.is_empty() {
                PNCURL_BACKUP
            } else {
                PNCURL_BACKUP_FOLDER
            };
            for (idx, file) in files.iter().enumerate() {
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
                let label = format!("episode {:02}", idx + 1);
                let out_name = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("backup.mkv")
                    .to_string();
                let mut uploaded = false;

                let upload_job_id = job_id.saturating_mul(1000).saturating_add(idx as u64);
                let logfile = directory
                    .join("log")
                    .join(format!("PNcurl_UploadAll{}.log", upload_job_id))
                    .display()
                    .to_string();
                println!(
                    "[uploadworker] upload_all job={} file={} logfile={}",
                    upload_job_id,
                    file.display(),
                    logfile,
                );
                let result = run_tool(
                    &pncurl_path,
                    spec,
                    &HashMap::from([
                        ("LINK", PathValue::from(file.display().to_string())),
                        ("OPCODE", PathValue::from(out_name.clone())),
                        ("DRIVEOPCODE", PathValue::from(out_name)),
                        ("ENV", PathValue::from(drive_env.path.clone())),
                        ("DRIVEFOLDER", PathValue::from(drive_folder.clone())),
                        ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                        ("LOGFILE", PathValue::from(logfile)),
                    ]),
                    upload_job_id,
                    &mut proto,
                    |data| {
                        let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                            Some(v) => v,
                            None => return None,
                        };
                        match out {
                            0 => {
                                let total = data.get(1).and_then(|v| v.as_str());
                                let compact = total.is_some();
                                let gd_payload = data
                                    .get(if compact { 2 } else { 1 })
                                    .and_then(|v| v.as_multi())?;
                                let gd_sent =
                                    gd_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                let gd_totl = if compact {
                                    total.unwrap_or("0")
                                } else {
                                    gd_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0")
                                };
                                let gd_ext = gd_payload
                                    .get(if compact { 1 } else { 2 })
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0");
                                rows[idx] = format!(
                                    "{}: {}",
                                    label,
                                    upload_progress_text("", gd_sent, gd_totl, gd_ext)
                                );
                                tx.try_send((
                                    job_id,
                                    MessagePayload::Progress(
                                        BACKUPALL_PROG,
                                        vec![format_backupall_rows(&rows)],
                                    ),
                                    None,
                                ))
                                .ok();
                            }
                            1 => {
                                let host_id = data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                let url = data
                                    .get(2)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Başarısız")
                                    .to_string();
                                if host_id == "1" {
                                    uploaded = true;
                                    rows[idx] = format!("{}: {}", label, url);
                                    tx.try_send((
                                        job_id,
                                        MessagePayload::Progress(
                                            BACKUPALL_PROG,
                                            vec![format_backupall_rows(&rows)],
                                        ),
                                        None,
                                    ))
                                    .ok();
                                    return Some(ToolResult::Success);
                                }
                            }
                            2 => {
                                rows[idx] = format!("{}: Başarısız", label);
                                tx.try_send((
                                    job_id,
                                    MessagePayload::Progress(
                                        BACKUPALL_PROG,
                                        vec![format_backupall_rows(&rows)],
                                    ),
                                    None,
                                ))
                                .ok();
                                return Some(ToolResult::Fail);
                            }
                            3 => return Some(ToolResult::Cancel),
                            _ => {}
                        }
                        None
                    },
                )
                .await;

                match result {
                    ToolResult::Success => {
                        if !uploaded {
                            rows[idx] = format!("{}: Başarısız", label);
                            tx.try_send((
                                job_id,
                                MessagePayload::Progress(
                                    BACKUPALL_PROG,
                                    vec![format_backupall_rows(&rows)],
                                ),
                                None,
                            ))
                            .ok();
                        } else {
                            any_uploaded = true;
                        }
                    }
                    ToolResult::Fail => {
                        rows[idx] = format!("{}: Başarısız", label);
                        tx.try_send((
                            job_id,
                            MessagePayload::Progress(
                                BACKUPALL_PROG,
                                vec![format_backupall_rows(&rows)],
                            ),
                            None,
                        ))
                        .ok();
                    }
                    ToolResult::Cancel => {
                        rows[idx] = format!("{}: İptal Edildi", label);
                        tx.send((
                            job_id,
                            MessagePayload::Progress(
                                BACKUPALL_PROG,
                                vec![format_backupall_rows(&rows)],
                            ),
                            Some(Stage::Cancelled),
                        ))
                        .await
                        .unwrap();
                        return;
                    }
                }
            }

            let stage = if any_uploaded {
                Stage::Uploaded
            } else {
                Stage::Failed
            };
            tx.send((
                job_id,
                MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
                Some(stage),
            ))
            .await
            .unwrap();
            println!("[Pandora Uploader] End of BackupAll Session");
            return;
        }
        _ => {}
    }
}

struct DriveEnv {
    path: String,
    local_drive: bool,
    smartcode_root_set: bool,
}

async fn drive_env_path(directory: &PathBuf, server_id: Option<u64>, is_smartcode: bool) -> DriveEnv {
    let Some(server_id) = server_id else {
        return global_drive_env();
    };
    let meta_path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    let meta = match tokio::fs::read_to_string(meta_path).await {
        Ok(s) => s,
        Err(_) => return global_drive_env(),
    };
    let smartcode_root_set = smartcode_root_configured(&meta);
    let Some((client_id, client_secret, refresh_token, parent_id)) = parse_server_drive_meta(&meta, is_smartcode) else {
        return global_drive_env();
    };

    let mut env = get_pandora_env();
    env.insert(CLIENT_ID.to_string(), client_id);
    env.insert(CLIENT_SECRET.to_string(), client_secret);
    env.insert(REFRESH_TOKEN.to_string(), refresh_token);
    env.insert(PARENTID.to_string(), parent_id);

    let path = directory.join("work").join("gdrive_env.pandora");
    let mut out = String::new();
    for (key, value) in env {
        out.push_str(&format!("{}{}{}\n", key, ENV_SEP, value));
    }
    if tokio::fs::write(&path, out).await.is_ok() {
        DriveEnv {
            path: path.display().to_string(),
            local_drive: true,
            smartcode_root_set,
        }
    } else {
        global_drive_env()
    }
}

fn global_drive_env() -> DriveEnv {
    DriveEnv {
        path: ENV_PATH.to_string(),
        local_drive: false,
        smartcode_root_set: false,
    }
}

fn smartcode_root_configured(meta: &str) -> bool {
    meta.lines()
        .nth(7)
        .map(str::trim)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

fn resolution_label(path: &str) -> String {
    ffprobe_video_height(path)
        .map(|height| format!("{}p", height))
        .unwrap_or_else(|| "1080p".to_string())
}

fn resolve_local_drive_root(lines: &[&str], is_smartcode: bool) -> Option<String> {
    let smartcode_root = lines.get(7).copied().unwrap_or("").trim();
    let anonymous_root = lines.get(10).copied().unwrap_or("").trim();
    let selected = if is_smartcode {
        if smartcode_root.is_empty() {
            anonymous_root
        } else {
            smartcode_root
        }
    } else if anonymous_root.is_empty() {
        smartcode_root
    } else {
        anonymous_root
    };
    if selected.is_empty() {
        None
    } else {
        Some(selected.to_string())
    }
}

fn parse_server_drive_meta(meta: &str, is_smartcode: bool) -> Option<(String, String, String, String)> {
    let lines: Vec<&str> = meta.lines().collect();
    let client_id = lines.get(4).copied().unwrap_or("").trim();
    let client_secret = lines.get(5).copied().unwrap_or("").trim();
    let refresh_token = lines.get(6).copied().unwrap_or("").trim();
    let parent_id = resolve_local_drive_root(&lines, is_smartcode)?;
    let local_drive = lines.get(9).copied().unwrap_or("true").trim();
    if matches!(local_drive, "false" | "0" | "disabled" | "off") {
        return None;
    }
    if client_id.is_empty()
        || client_secret.is_empty()
        || refresh_token.is_empty()
    {
        return None;
    }
    Some((
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
        parent_id,
    ))
}

fn drive_folder_path(
    local_drive: bool,
    is_smartcode: bool,
    server_id: Option<u64>,
    folder: Option<String>,
) -> String {
    let folder = folder
        .unwrap_or_default()
        .trim()
        .trim_matches('/')
        .to_string();
    if !local_drive {
        return match server_id {
            Some(id) if folder.is_empty() => id.to_string(),
            Some(id) => format!("{}/{}", id, folder),
            None => folder,
        };
    }
    if is_smartcode {
        return folder;
    }
    if folder.is_empty() {
        return "pntools".to_string();
    }
    if folder == "pntools" || folder.starts_with("pntools/") {
        folder
    } else {
        format!("pntools/{}", folder)
    }
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

fn format_backupall_rows(rows: &[String]) -> String {
    let mut out = String::new();
    let mut hidden = 0usize;
    for row in rows {
        let next = if out.is_empty() {
            row.clone()
        } else {
            format!("\n{}", row)
        };
        if out.len() + next.len() > 1000 {
            hidden += 1;
        } else {
            out.push_str(&next);
        }
    }
    if hidden > 0 {
        out.push_str(&format!("\n...and {} more", hidden));
    }
    out
}

fn upload_progress_text(host: &str, sent: &str, total: &str, extensions: &str) -> String {
    let suffix = if extensions == "0" {
        String::new()
    } else {
        format!("+{}", extensions)
    };
    let progress = format!(
        "{}/{} MB{}",
        string_byte_to_mb(sent),
        string_byte_to_mb(total),
        suffix
    );
    if host.is_empty() {
        progress
    } else {
        format!("{} {}", host, progress)
    }
}

fn normalize_lulu_link(link: &str) -> String {
    let trimmed = link.trim();
    for prefix in ["https://lulustream.com/", "http://lulustream.com/", "https://luluvdo.com/", "http://luluvdo.com/"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let code = rest.strip_prefix("e/").unwrap_or(rest).trim_matches('/');
            if !code.is_empty() && !code.contains('/') {
                return format!("https://luluvdo.com/e/{}", code);
            }
        }
    }
    trimmed.to_string()
}

fn normalize_doodstream_link(link: &str) -> String {
    let trimmed = link.trim();
    for prefix in ["https://doodstream.com/", "http://doodstream.com/"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let code = rest
                .strip_prefix("e/")
                .or_else(|| rest.strip_prefix("d/"))
                .unwrap_or(rest)
                .trim_matches('/');
            if !code.is_empty() && !code.contains('/') {
                return format!("https://doodstream.com/e/{}", code);
            }
        }
    }
    trimmed.to_string()
}

fn normalize_voe_link(link: &str) -> String {
    let trimmed = link.trim();
    for prefix in ["https://voe.sx/", "http://voe.sx/"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let code = rest.strip_prefix("e/").unwrap_or(rest).trim_matches('/');
            if !code.is_empty() && !code.contains('/') {
                return format!("https://voe.sx/e/{}", code);
            }
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(wrap_style: &str, local_gdrive: &str) -> String {
        format!(
            "EN\nhttps://forgejo.example/org\n123\napi\nclient\nsecret\nrefresh\nparent\n{}\n{}\n",
            wrap_style, local_gdrive
        )
    }

    fn meta_with_roots(smartcode_root: &str, anonymous_root: &str) -> String {
        format!(
            "EN\nhttps://forgejo.example/org\n123\napi\nclient\nsecret\nrefresh\n{}\n\ntrue\n{}\n",
            smartcode_root, anonymous_root
        )
    }

    #[test]
    fn parse_server_drive_meta_uses_local_gdrive_line_not_wrapstyle() {
        let parsed = parse_server_drive_meta(&meta("0", "true"), true);
        assert_eq!(
            parsed,
            Some((
                "client".to_string(),
                "secret".to_string(),
                "refresh".to_string(),
                "parent".to_string(),
            ))
        );
    }

    #[test]
    fn parse_server_drive_meta_respects_disabled_local_gdrive() {
        assert_eq!(parse_server_drive_meta(&meta("1", "false"), true), None);
    }

    #[test]
    fn parse_server_drive_meta_splits_smartcode_and_anonymous_roots() {
        let smart = parse_server_drive_meta(&meta_with_roots("smart-root", "anon-root"), true);
        let anon = parse_server_drive_meta(&meta_with_roots("smart-root", "anon-root"), false);
        assert_eq!(smart.unwrap().3, "smart-root");
        assert_eq!(anon.unwrap().3, "anon-root");
    }

    #[test]
    fn parse_server_drive_meta_falls_back_between_roots() {
        let smart = parse_server_drive_meta(&meta_with_roots("", "anon-root"), true);
        let anon = parse_server_drive_meta(&meta_with_roots("smart-root", ""), false);
        assert_eq!(smart.unwrap().3, "anon-root");
        assert_eq!(anon.unwrap().3, "smart-root");
    }

    #[test]
    fn parse_server_drive_meta_rejects_missing_roots() {
        assert_eq!(parse_server_drive_meta(&meta_with_roots("", ""), true), None);
        assert_eq!(parse_server_drive_meta(&meta_with_roots("", ""), false), None);
    }

    #[test]
    fn smartcode_root_configured_requires_line_seven() {
        assert!(smartcode_root_configured(&meta_with_roots("smart-root", "anon-root")));
        assert!(!smartcode_root_configured(&meta_with_roots("", "anon-root")));
    }

    #[test]
    fn smartcode_drive_name_formats_release_filename() {
        let name = SmartcodeDriveName::new("AkiraSubs/frieren", "Sousou no Frieren", 1);
        assert_eq!(
            name.filename("1080p"),
            "[AkiraSubs] Sousou no Frieren - Bölüm 01 [1080p].mp4",
        );
    }

    #[test]
    fn parse_server_drive_meta_old_ten_line_meta_uses_smartcode_root_for_anonymous() {
        let parsed = parse_server_drive_meta(&meta("0", "true"), false);
        assert_eq!(parsed.unwrap().3, "parent");
    }

    #[test]
    fn normalize_lulu_link_converts_to_embed_url() {
        assert_eq!(
            normalize_lulu_link("https://lulustream.com/yzip3nvuot20"),
            "https://luluvdo.com/e/yzip3nvuot20",
        );
        assert_eq!(
            normalize_lulu_link("https://luluvdo.com/e/yzip3nvuot20"),
            "https://luluvdo.com/e/yzip3nvuot20",
        );
    }

    #[test]
    fn normalize_doodstream_link_converts_to_embed_url() {
        assert_eq!(
            normalize_doodstream_link("https://doodstream.com/d/abc123"),
            "https://doodstream.com/e/abc123",
        );
        assert_eq!(
            normalize_doodstream_link("https://doodstream.com/e/abc123"),
            "https://doodstream.com/e/abc123",
        );
    }

    #[test]
    fn normalize_voe_link_converts_to_embed_url() {
        assert_eq!(
            normalize_voe_link("https://voe.sx/abc123"),
            "https://voe.sx/e/abc123",
        );
        assert_eq!(
            normalize_voe_link("https://voe.sx/e/abc123"),
            "https://voe.sx/e/abc123",
        );
    }

    #[test]
    fn drive_folder_path_puts_smartcode_local_folder_at_root() {
        assert_eq!(
            drive_folder_path(true, true, Some(123), Some("Anime Name".to_string())),
            "Anime Name",
        );
    }

    #[test]
    fn drive_folder_path_keeps_pntools_for_non_smartcode_local_jobs() {
        assert_eq!(
            drive_folder_path(true, false, Some(123), Some("backup".to_string())),
            "pntools/backup",
        );
        assert_eq!(drive_folder_path(true, false, Some(123), None), "pntools");
    }

    #[test]
    fn drive_folder_path_keeps_global_server_prefix() {
        assert_eq!(
            drive_folder_path(false, true, Some(123), Some("Anime Name".to_string())),
            "123/Anime Name",
        );
    }
}

fn upload_payload(
    job_id: u64,
    release: bool,
    message_id: &'static str,
    gd_link: &str,
    dood_link: &str,
    lulu_link: &str,
    voesx_link: &str,
    abyss_link: &str,
    stage: Option<Stage>,
) -> CommData {
    if release {
        (
            job_id,
            MessagePayload::Progress(
                message_id,
                vec![
                    gd_link.to_string(),
                    dood_link.to_string(),
                    lulu_link.to_string(),
                    voesx_link.to_string(),
                    abyss_link.to_string(),
                ],
            ),
            stage,
        )
    } else {
        (
            job_id,
            MessagePayload::Progress(UPLOAD_BACKUP_PROG, vec![gd_link.to_string()]),
            stage,
        )
    }
}
