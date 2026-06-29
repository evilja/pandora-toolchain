use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::{
    CLIENT_ID, CLIENT_SECRET, ENV_PATH, ENV_SEP, PARENTID, PNCURL, REFRESH_TOKEN,
};
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::core::CommData;
use crate::pnworker::core::{Stage, WorkerMsg};
use crate::pnworker::messages::{
    BACKUPALL_PROG, JOB_CANCELLED, MessagePayload, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_FAIL,
    UPLOAD_PROG, WORKER_ASSIGN,
};
use crate::pnworker::tools::{PNCURL_BACKUP, PNCURL_BACKUP_FOLDER, PNCURL_UPLOAD, PNCURL_UPLOAD_FOLDER};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{ToolResult, WorkerNamePool, run_tool};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::sleep;

pub type UploadData = (PathBuf, String, bool, u64, Option<u64>, Option<String>, Option<String>);
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
        WorkerMsg::Upload((_, _, _, job_id, _, _, _)) => Some(*job_id),
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
        WorkerMsg::Upload((directory, out_name, release, job_id, server_id, gdrive_folder_global, gdrive_folder_local)) => {
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

            let (drive_env, local_drive) = drive_env_path(&directory, server_id).await;
            let drive_folder = if local_drive {
                gdrive_folder_local.unwrap_or_default()
            } else {
                gdrive_folder_global.unwrap_or_default()
            };

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
                    ("ENV", PathValue::from(drive_env)),
                    ("DRIVEFOLDER", PathValue::from(drive_folder)),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
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
                                    dood_link = url;
                                    dood_done = true;
                                }
                                "4" => {
                                    lulu_link = url;
                                    lulu_done = true;
                                }
                                "5" => {
                                    voesx_link = url;
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

            let (drive_env, _) = drive_env_path(&directory, server_id).await;
            for (idx, file) in files.iter().enumerate() {
                let label = format!("episode {:02}", idx + 1);
                let out_name = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("backup.mkv")
                    .to_string();
                let mut uploaded = false;

                let upload_job_id = job_id.saturating_mul(1000).saturating_add(idx as u64);
                let result = run_tool(
                    &pncurl_path,
                    PNCURL_BACKUP,
                    &HashMap::from([
                        ("LINK", PathValue::from(file.display().to_string())),
                        ("OPCODE", PathValue::from(out_name)),
                        ("ENV", PathValue::from(drive_env.clone())),
                        ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
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

async fn drive_env_path(directory: &PathBuf, server_id: Option<u64>) -> (String, bool) {
    let Some(server_id) = server_id else {
        return (ENV_PATH.to_string(), false);
    };
    let meta_path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    let meta = match tokio::fs::read_to_string(meta_path).await {
        Ok(s) => s,
        Err(_) => return (ENV_PATH.to_string(), false),
    };
    let mut lines = meta.lines();
    for _ in 0..4 {
        lines.next();
    }
    let client_id = lines.next().unwrap_or("").trim().to_string();
    let client_secret = lines.next().unwrap_or("").trim().to_string();
    let refresh_token = lines.next().unwrap_or("").trim().to_string();
    let parent_id = lines.next().unwrap_or("").trim().to_string();
    if client_id.is_empty()
        || client_secret.is_empty()
        || refresh_token.is_empty()
        || parent_id.is_empty()
    {
        return (ENV_PATH.to_string(), false);
    }

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
        (path.display().to_string(), true)
    } else {
        (ENV_PATH.to_string(), false)
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
