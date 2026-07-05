use crate::libpndb::core::JobDb;
use crate::pnworker::core::Stage;
use crate::pnworker::messages::{
    BACKUPALL_PROG, ENCODE_CONCAT_PROG, ENCODE_PROG, MessagePayload, PROBE_ROW, TORRENT_PROG,
    TORRENT_PROG_SELECT, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_PROG,
};

pub(crate) async fn persist_side_effects(
    db: &JobDb,
    job_id: u64,
    payload: &MessagePayload,
    stage: Option<Stage>,
    encode_warnings: &[String],
) {
    let MessagePayload::Progress(id, args) = payload else {
        return;
    };
    if *id == ENCODE_PROG {
        let frame = args.get(1).cloned().unwrap_or_default();
        let total = args.get(2).cloned().unwrap_or_default();
        let v = serde_json::json!({
            "type": "encode", "frame": frame, "total": total,
            "fps": args.get(3), "kbps": args.get(4),
            "percent": encode_percent(&frame, &total),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == ENCODE_CONCAT_PROG {
        let frame = args.get(0).cloned().unwrap_or_default();
        let total = args.get(1).cloned().unwrap_or_default();
        let v = serde_json::json!({
            "type": "encode", "frame": frame, "total": total,
            "fps": args.get(2), "percent": encode_percent(&frame, &total),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == TORRENT_PROG {
        let v = serde_json::json!({
            "type": "download",
            "percent": args.get(0).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
            "done": args.get(1), "total": args.get(2),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == TORRENT_PROG_SELECT {
        let v = serde_json::json!({
            "type": "download",
            "percent": args.get(0).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
            "done": args.get(1),
        });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == UPLOAD_PROG {
        if stage == Some(Stage::Uploaded) {
            let v = upload_links_json(args, encode_warnings);
            db.update_links(job_id, &v.to_string()).await.ok();
            let p = serde_json::json!({ "type": "upload", "percent": 100, "hosts": args });
            db.update_progress(job_id, &p.to_string()).await.ok();
        } else {
            let v = serde_json::json!({
                "type": "upload",
                "percent": upload_percent(args),
                "hosts": args,
            });
            db.update_progress(job_id, &v.to_string()).await.ok();
        }
    } else if *id == PROBE_ROW {
        let files = args.get(0).cloned().unwrap_or_default();
        let v = serde_json::json!({ "type": "probe", "files": files, "file_options": parse_probe_options(&files) });
        db.update_progress(job_id, &v.to_string()).await.ok();
    } else if *id == UPLOAD_DONE {
        let v = upload_links_json(args, encode_warnings);
        db.update_links(job_id, &v.to_string()).await.ok();
        let p = serde_json::json!({ "type": "upload", "percent": 100, "hosts": args });
        db.update_progress(job_id, &p.to_string()).await.ok();
    } else if *id == UPLOAD_BACKUP_PROG {
        if stage == Some(Stage::Uploaded) {
            let mut v = serde_json::json!({ "drive": args.get(0) });
            add_warnings(&mut v, encode_warnings);
            db.update_links(job_id, &v.to_string()).await.ok();
            let p = serde_json::json!({ "type": "upload", "percent": 100, "hosts": args });
            db.update_progress(job_id, &p.to_string()).await.ok();
        } else {
            let v = serde_json::json!({
                "type": "upload",
                "percent": upload_percent(args),
                "hosts": args,
            });
            db.update_progress(job_id, &v.to_string()).await.ok();
        }
    } else if *id == BACKUPALL_PROG {
        let rows = args.get(0).cloned().unwrap_or_default();
        if stage == Some(Stage::Uploaded) {
            let v = serde_json::json!({ "episodes": rows });
            db.update_links(job_id, &v.to_string()).await.ok();
            let p = serde_json::json!({ "type": "upload_all", "percent": 100, "rows": rows });
            db.update_progress(job_id, &p.to_string()).await.ok();
        } else {
            let v = serde_json::json!({
                "type": "upload_all",
                "percent": backupall_percent(&rows),
                "rows": rows,
            });
            db.update_progress(job_id, &v.to_string()).await.ok();
        }
    }
}

fn upload_links_json(args: &[String], encode_warnings: &[String]) -> serde_json::Value {
    let mut v = serde_json::json!({
        "drive": args.get(0), "doodstream": args.get(1), "lulustream": args.get(2).map(|s| normalize_lulu_link(s)),
        "voe": args.get(3), "abyss": args.get(4),
    });
    add_warnings(&mut v, encode_warnings);
    v
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

fn add_warnings(v: &mut serde_json::Value, encode_warnings: &[String]) {
    if encode_warnings.is_empty() {
        return;
    }
    if let Some(obj) = v.as_object_mut() {
        obj.insert("warnings".to_string(), serde_json::json!(encode_warnings));
    }
}

pub(crate) fn drive_link_from_payload(payload: &MessagePayload) -> Option<String> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    if *id == UPLOAD_DONE || *id == UPLOAD_PROG || *id == UPLOAD_BACKUP_PROG {
        return args.get(0).cloned();
    }
    None
}

fn parse_probe_options(files: &str) -> Vec<serde_json::Value> {
    files
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix('`')?;
            let end = rest.find('`')?;
            let index = &rest[..end];
            let label = line.replace('`', "");
            Some(serde_json::json!({ "index": index, "label": label }))
        })
        .collect()
}

fn encode_percent(frame: &str, total: &str) -> u64 {
    let f = frame.parse::<f64>().unwrap_or(0.0);
    let t = total.parse::<f64>().unwrap_or(0.0);
    if t <= 0.0 {
        return 0;
    }
    ((f / t) * 100.0).clamp(0.0, 100.0) as u64
}

fn upload_percent(hosts: &[String]) -> u64 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for h in hosts {
        let h = h.trim();
        if h.is_empty() {
            continue;
        }
        if h.starts_with("http") {
            sum += 100.0;
            n += 1.0;
            continue;
        }
        for tok in h.split_whitespace() {
            if let Some((a, b)) = tok.split_once('/') {
                if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
                    if b > 0.0 {
                        sum += (a / b * 100.0).min(100.0);
                        n += 1.0;
                    }
                    break;
                }
            }
        }
    }
    if n > 0.0 { (sum / n).round() as u64 } else { 0 }
}

fn backupall_percent(rows: &str) -> u64 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for row in rows.lines() {
        let row = row.trim();
        if row.is_empty() {
            continue;
        }
        n += 1.0;
        if row.contains("http") || row.contains("Başarısız") || row.contains("İptal") {
            sum += 100.0;
            continue;
        }
        for tok in row.split_whitespace() {
            if let Some((a, b)) = tok.trim_end_matches("MB").split_once('/') {
                if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
                    if b > 0.0 {
                        sum += (a / b * 100.0).min(100.0);
                    }
                    break;
                }
            }
        }
    }
    if n > 0.0 { (sum / n).round() as u64 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lulu_link_converts_file_codes_to_embed_urls() {
        assert_eq!(
            normalize_lulu_link("https://lulustream.com/yzip3nvuot20"),
            "https://luluvdo.com/e/yzip3nvuot20",
        );
        assert_eq!(
            normalize_lulu_link("https://luluvdo.com/e/yzip3nvuot20"),
            "https://luluvdo.com/e/yzip3nvuot20",
        );
    }
}
