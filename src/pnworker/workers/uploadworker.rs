use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;
use crate::libpnenv::core::get_env;
use crate::libpnenv::standard::PNCURL;
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::messages::{JOB_CANCELLED, UPLOAD_DONE, UPLOAD_FAIL, UPLOAD_PROG, };
use crate::pnworker::util::{ToolResult, run_tool};
use crate::pnworker::tools::PNCURL_UPLOAD;
use std::path::PathBuf;
use std::collections::HashMap;
use std::time::Duration;
use crate::pnworker::core::{Stage, WorkerMsg};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::core::CommData;

pub type UploadData = (PathBuf, String, bool, u64);

pub async fn pn_uloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let pncurl_path = get_env("env.pandora")[PNCURL].clone();
    'll: loop {
        if let Ok(WorkerMsg::Upload((directory, out_name, _release, job_id))) = rx.try_recv() {
            let output_path = directory.join("work").join("output.mp4").display().to_string();
            let mut completed = 0u8;
            let mut gd_link = "Google Bekleniyor".to_string(); let mut gd_done = false;
            let mut dood_link = "Doodstream Bekleniyor".to_string(); let mut dood_done = false;
            let mut uq_link = "Uqload Bekleniyor".to_string(); let mut uq_done = false;
            let mut lulu_link = "Lulustream Bekleniyor".to_string(); let mut lulu_done = false;
            let mut voesx_link = "Voe Bekleniyor".to_string(); let mut voesx_done = false;
            let mut abyss_link = "Abyss Bekleniyor".to_string(); let mut abyss_done = false;

            let emit_status = |gd: &str, dood: &str, uq: &str, lulu: &str, voe: &str, abyss: &str| {
                format!("{}\n{}\n{}\n{}\n{}\n{}",
                    gd, dood, uq, lulu, voe, abyss)
            };

            let result = run_tool(
                &pncurl_path,
                PNCURL_UPLOAD,
                &HashMap::from([
                    ("LINK",   PathValue::from(output_path.clone())),
                    ("OPCODE", PathValue::from(out_name.clone())),
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
                            let uq_payload = data.get(3).and_then(|v| v.as_multi())?;
                            let lulu_payload = data.get(4).and_then(|v| v.as_multi())?;
                            let voesx_payload = data.get(5).and_then(|v| v.as_multi())?;
                            let abyss_payload = data.get(6).and_then(|v| v.as_multi())?;
                            let gd_sent = gd_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let gd_totl = gd_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let dood_sent = dood_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let dood_totl = dood_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let uq_sent = uq_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let uq_totl = uq_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let lulu_sent = lulu_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let lulu_totl = lulu_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let voesx_sent = voesx_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let voesx_totl = voesx_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let abyss_sent = abyss_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let abyss_totl = abyss_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            if !gd_done { gd_link = format!("Google {}/{} MB", string_byte_to_mb(gd_sent), string_byte_to_mb(gd_totl)); }
                            if !dood_done { dood_link = format!("Doodstream {}/{} MB", string_byte_to_mb(dood_sent), string_byte_to_mb(dood_totl)); }
                            if !uq_done { uq_link = format!("Uqload {}/{} MB", string_byte_to_mb(uq_sent), string_byte_to_mb(uq_totl)); }
                            if !lulu_done { lulu_link = format!("Lulustream {}/{} MB", string_byte_to_mb(lulu_sent), string_byte_to_mb(lulu_totl)); }
                            if !voesx_done { voesx_link = format!("Voe {}/{} MB", string_byte_to_mb(voesx_sent), string_byte_to_mb(voesx_totl)); }
                            if !abyss_done { abyss_link = format!("Abyss {}/{} MB", string_byte_to_mb(abyss_sent), string_byte_to_mb(abyss_totl)); }
                            tx.try_send((job_id, format!("{}\n{}", UPLOAD_PROG,
                                emit_status(&gd_link, &dood_link, &uq_link, &lulu_link, &voesx_link, &abyss_link)
                            ), None)).ok();
                        }
                        1 => {
                            completed += 1;
                            let host_id = data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let url = data.get(2).and_then(|v| v.as_str()).unwrap_or("Başarısız").to_string();
                            match host_id {
                                "1" => { gd_link = url; gd_done = true; }
                                "2" => { dood_link = url; dood_done = true; }
                                "3" => { uq_link = url; uq_done = true; }
                                "4" => { lulu_link = url; lulu_done = true; }
                                "5" => { voesx_link = url; voesx_done = true; }
                                "6" => { abyss_link = url; abyss_done = true; }
                                _ => {}
                            }
                            let stage = if completed >= 6 { Some(Stage::Uploaded) } else { None };
                            tx.try_send((job_id, format!("{} \n{} \n{} \n{} \n{} \n{} \n{}",
                                UPLOAD_PROG, gd_link, dood_link, uq_link, lulu_link, voesx_link, abyss_link
                            ), stage)).ok();
                            if completed >= 6 {
                                return Some(ToolResult::Success);
                            }
                        }
                        2 => {
                            completed += 1;
                            let host_id = data.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            match host_id {
                                "1" => { gd_link = "Google Başarısız".to_string(); gd_done = true; }
                                "2" => { dood_link = "Doodstream Başarısız".to_string(); dood_done = true; }
                                "3" => { uq_link = "Uqload Başarısız".to_string(); uq_done = true; }
                                "4" => { lulu_link = "Lulustream Başarısız".to_string(); lulu_done = true; }
                                "5" => { voesx_link = "Voe Başarısız".to_string(); voesx_done = true; }
                                "6" => { abyss_link = "Abyss Başarısız".to_string(); abyss_done = true; }
                                _ => {}
                            }
                            let stage = if completed >= 6 { Some(Stage::Uploaded) } else { None };
                            tx.try_send((job_id, format!("{} \n{} \n{} \n{} \n{} \n{} \n{}",
                                UPLOAD_PROG, gd_link, dood_link, uq_link, lulu_link, voesx_link, abyss_link
                            ), stage)).ok();
                            if completed >= 6 {
                                return Some(ToolResult::Success);
                            }
                        }
                        _ => {}
                    }
                    None
                },
            ).await;

            let any_done = gd_done || dood_done || uq_done || lulu_done || voesx_done || abyss_done;
            match result {
                ToolResult::Success | ToolResult::Fail => {
                    if any_done {
                        tx.send((job_id, format!("{} \n{}\n{}\n{}\n{}\n{}\n{}",
                            UPLOAD_DONE, gd_link, dood_link, uq_link, lulu_link, voesx_link, abyss_link
                        ), Some(Stage::Uploaded))).await.unwrap();
                    } else {
                        tx.send((job_id, UPLOAD_FAIL.to_string(), Some(Stage::Failed))).await.unwrap();
                    }
                }
                ToolResult::Cancel => {
                    if any_done {
                        tx.send((job_id, format!("{} \n{}\n{}\n{}\n{}\n{}\n{}",
                            JOB_CANCELLED, gd_link, dood_link, uq_link, lulu_link, voesx_link, abyss_link
                        ), Some(Stage::Cancelled))).await.unwrap();
                    } else {
                        tx.send((job_id, JOB_CANCELLED.to_string(), Some(Stage::Cancelled))).await.unwrap();
                    }
                }
            }
            println!("[Pandora Uploader] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}
