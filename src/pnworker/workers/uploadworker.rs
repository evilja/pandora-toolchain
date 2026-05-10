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
                            let gd_sent = gd_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let gd_totl = gd_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let dood_sent = dood_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let dood_totl = dood_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let uq_sent = uq_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let uq_totl = uq_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let lulu_sent = lulu_payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let lulu_totl = lulu_payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, format!("{} \nGoogle Drive: {}/{} \nDoodstream: {}/{} \nUqload: {}/{} \nLulustream: {}/{}",
                                UPLOAD_PROG,
                                string_byte_to_mb(gd_sent), string_byte_to_mb(gd_totl),
                                string_byte_to_mb(dood_sent), string_byte_to_mb(dood_totl),
                                string_byte_to_mb(uq_sent), string_byte_to_mb(uq_totl),
                                string_byte_to_mb(lulu_sent), string_byte_to_mb(lulu_totl),
                            ), None)).ok();
                        }
                        1 => {
                            let gd_link = data.get(1).and_then(|v| v.as_str()).unwrap_or("Başarısız").to_string();
                            let dood_link = data.get(2).and_then(|v| v.as_str()).unwrap_or("Başarısız").to_string();
                            let uq_link = data.get(3).and_then(|v| v.as_str()).unwrap_or("Başarısız").to_string();
                            let lulu_link = data.get(4).and_then(|v| v.as_str()).unwrap_or("Başarısız").to_string();
                            tx.try_send((job_id, format!("{} \nGoogle Drive: {} \nDoodstream: {} \nUqload: {} \nLulustream: {}", UPLOAD_DONE, gd_link, dood_link, uq_link, lulu_link), Some(Stage::Uploaded))).ok();
                            return Some(ToolResult::Success);
                        }
                        2 => return Some(ToolResult::Fail),
                        _ => {}
                    }
                    None
                },
            ).await;

            match result {
                ToolResult::Fail | ToolResult::Success => {
                    if matches!(result, ToolResult::Fail) {
                        tx.send((job_id, format!("{}", UPLOAD_FAIL), Some(Stage::Failed))).await.unwrap();
                    }
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
