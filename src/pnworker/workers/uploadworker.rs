use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::{CLIENT_ID, CLIENT_SECRET, ENV_PATH, ENV_SEP, PARENTID, PNCURL, REFRESH_TOKEN};
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::core::CommData;
use crate::pnworker::core::{Stage, WorkerMsg};
use crate::pnworker::messages::{
    BACKUPALL_PROG, JOB_CANCELLED, MessagePayload, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_FAIL,
    UPLOAD_PROG,
};
use crate::pnworker::tools::{PNCURL_BACKUP, PNCURL_UPLOAD};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{ToolResult, run_tool};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

pub type UploadData = (PathBuf, String, bool, u64, Option<u64>);
pub type UploadAllData = (PathBuf, u64, Option<u64>);

pub async fn pn_uloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let pncurl_path = get_pandora_env().get(PNCURL).cloned().unwrap_or_default();
    'll: loop {
        if let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Upload((directory, out_name, release, job_id, server_id)) => {
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

                    let result = run_tool(
                        &pncurl_path,
                        if release {
                            PNCURL_UPLOAD
                        } else {
                            PNCURL_BACKUP
                        },
                        &HashMap::from([
                            ("LINK", PathValue::from(output_path.clone())),
                            ("OPCODE", PathValue::from(out_name.clone())),
                            ("ENV", PathValue::from(drive_env_path(&directory, server_id).await)),
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
                                    let gd_payload = data.get(1).and_then(|v| v.as_multi())?;
                                    let dood_payload = data.get(2).and_then(|v| v.as_multi())?;
                                    let lulu_payload = data.get(3).and_then(|v| v.as_multi())?;
                                    let voesx_payload = data.get(4).and_then(|v| v.as_multi())?;
                                    let abyss_payload = data.get(5).and_then(|v| v.as_multi())?;
                                    let gd_sent =
                                        gd_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let gd_totl =
                                        gd_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    let dood_sent =
                                        dood_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let dood_totl =
                                        dood_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    let lulu_sent =
                                        lulu_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                    let lulu_totl =
                                        lulu_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                    let voesx_sent = voesx_payload
                                        .get(0)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0");
                                    let voesx_totl = voesx_payload
                                        .get(1)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0");
                                    let abyss_sent = abyss_payload
                                        .get(0)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0");
                                    let abyss_totl = abyss_payload
                                        .get(1)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0");
                                    if !gd_done {
                                        gd_link = format!(
                                            "Google {}/{} MB",
                                            string_byte_to_mb(gd_sent),
                                            string_byte_to_mb(gd_totl)
                                        );
                                    }
                                    if release && !dood_done && dood_totl != "0" {
                                        dood_link = format!(
                                            "Doodstream {}/{} MB",
                                            string_byte_to_mb(dood_sent),
                                            string_byte_to_mb(dood_totl)
                                        );
                                    }
                                    if release && !lulu_done && lulu_totl != "0" {
                                        lulu_link = format!(
                                            "Lulustream {}/{} MB",
                                            string_byte_to_mb(lulu_sent),
                                            string_byte_to_mb(lulu_totl)
                                        );
                                    }
                                    if release && !voesx_done && voesx_totl != "0" {
                                        voesx_link = format!(
                                            "Voe {}/{} MB",
                                            string_byte_to_mb(voesx_sent),
                                            string_byte_to_mb(voesx_totl)
                                        );
                                    }
                                    if release && !abyss_done && abyss_totl != "0" {
                                        abyss_link = format!(
                                            "Abyss {}/{} MB",
                                            string_byte_to_mb(abyss_sent),
                                            string_byte_to_mb(abyss_totl)
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
                                    let host_id =
                                        data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
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
                                    let host_id =
                                        data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
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
                    continue 'll;
                }
                WorkerMsg::UploadAll((directory, job_id, server_id)) => {
                    let mut files =
                        find_mkv_files(&directory.join("contents").join("torrent")).await;
                    files.sort_by(|a, b| a.display().to_string().cmp(&b.display().to_string()));
                    if files.is_empty() {
                        tx.send((
                            job_id,
                            MessagePayload::Static(UPLOAD_FAIL),
                            Some(Stage::Failed),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }

                    let mut rows: Vec<String> = (0..files.len())
                        .map(|i| format!("episode {:02}: Bekleniyor", i + 1))
                        .collect();
                    let mut any_uploaded = false;
                    tx.try_send((
                        job_id,
                        MessagePayload::Progress(
                            BACKUPALL_PROG,
                            vec![format_backupall_rows(&rows)],
                        ),
                        None,
                    ))
                    .ok();

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
                                ("ENV", PathValue::from(drive_env_path(&directory, server_id).await)),
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
                                        let gd_payload = data.get(1).and_then(|v| v.as_multi())?;
                                        let gd_sent = gd_payload
                                            .get(0)
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("0");
                                        let gd_totl = gd_payload
                                            .get(1)
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("0");
                                        rows[idx] = format!(
                                            "{}: {}/{}MB",
                                            label,
                                            string_byte_to_mb(gd_sent),
                                            string_byte_to_mb(gd_totl)
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
                                        let host_id =
                                            data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
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
                                continue 'll;
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
                        MessagePayload::Progress(
                            BACKUPALL_PROG,
                            vec![format_backupall_rows(&rows)],
                        ),
                        Some(stage),
                    ))
                    .await
                    .unwrap();
                    println!("[Pandora Uploader] End of BackupAll Session");
                    continue 'll;
                }
                _ => {}
            }
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}

async fn drive_env_path(directory: &PathBuf, server_id: Option<u64>) -> String {
    let Some(server_id) = server_id else {
        return ENV_PATH.to_string();
    };
    let meta_path = PathBuf::from("DB").join("config").join(server_id.to_string()).join("meta.pandora");
    let meta = match tokio::fs::read_to_string(meta_path).await {
        Ok(s) => s,
        Err(_) => return ENV_PATH.to_string(),
    };
    let mut lines = meta.lines();
    for _ in 0..4 {
        lines.next();
    }
    let client_id = lines.next().unwrap_or("").trim().to_string();
    let client_secret = lines.next().unwrap_or("").trim().to_string();
    let refresh_token = lines.next().unwrap_or("").trim().to_string();
    let parent_id = lines.next().unwrap_or("").trim().to_string();
    if client_id.is_empty() && client_secret.is_empty() && refresh_token.is_empty() && parent_id.is_empty() {
        return ENV_PATH.to_string();
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
        path.display().to_string()
    } else {
        ENV_PATH.to_string()
    }
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts")
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
