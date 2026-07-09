use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{PNCURL, PNP2P};
use crate::lib::image::Font;
use crate::lib::mpeg::preview::ffmpeg_screenshot;
use crate::lib::p2p::nyaaise::TorrentType;
use crate::lib::protocol::core::Protocol;
use crate::libkagami::core::{find_fonts_with_roots, SubstationAlpha};
use crate::pnworker::core::Stage;
use crate::pnworker::core::{CommData, WorkerMsg};
use crate::pnworker::messages::{
    CTORRENT_DONE, CTORRENT_FAIL, JOB_CANCELLED, MessagePayload, PREVIEW_DONE, PREVIEW_FAIL,
    PROBE_FAIL, PROBE_ROW,
};
use crate::pnworker::preview::compose_preview;
use crate::pnworker::tools::{PNCURL_TORRENT, PNP2P_PROBE};
use crate::pnworker::util::PathValue;
use crate::pnworker::util::{ToolResult, job_cancelled, run_tool, string_byte_to_mb};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};

pub type ProbeData = (PathBuf, TorrentType, u64);
pub type PreviewData = (PathBuf, Vec<(u64, String)>, Option<PathBuf>, u64, Option<u64>);

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
        if let Ok(msg) = rx.try_recv() {
            let WorkerMsg::Probe((directory, torrent, job_id)) = msg else {
                if let WorkerMsg::Preview((directory, shots, watermark_font, job_id, server_id)) = msg {
                    run_preview_job(
                        directory,
                        shots,
                        watermark_font,
                        job_id,
                        server_id,
                        &tx,
                        &pulse,
                    )
                    .await;
                }
                continue 'll;
            };
            if job_cancelled(&directory) {
                tx.send((
                    job_id,
                    MessagePayload::Static(JOB_CANCELLED),
                    Some(Stage::Cancelled),
                ))
                .await
                .unwrap();
                continue 'll;
            }
            // Phase 1: if Link, fetch .torrent file first (same as downloadworker)
            let arg_opcode: String;
            match torrent {
                TorrentType::GDrive(_) | TorrentType::Direct(_) => {
                    tx.send((
                        job_id,
                        MessagePayload::Static(PROBE_FAIL),
                        Some(Stage::Failed),
                    ))
                    .await
                    .unwrap();
                    continue 'll;
                }
                TorrentType::Link(ref link) => {
                    if job_cancelled(&directory) {
                        tx.send((
                            job_id,
                            MessagePayload::Static(JOB_CANCELLED),
                            Some(Stage::Cancelled),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }
                    let result = run_tool(
                        &pncurl_path,
                        PNCURL_TORRENT,
                        &HashMap::from([
                            ("LINK", PathValue::from(link.clone())),
                            (
                                "OPCODE",
                                PathValue::from(
                                    directory
                                        .join("contents")
                                        .join("fetch.torrent")
                                        .display()
                                        .to_string(),
                                ),
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
                            ("NEGKEY", PathValue::from("pn-probe-main".to_string())),
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
                    arg_opcode = directory
                        .join("contents")
                        .join("fetch.torrent")
                        .display()
                        .to_string();
                }
                TorrentType::Magnet(ref magnet) => {
                    arg_opcode = magnet.clone();
                }
            }

            let mut probe_rows: Vec<ProbeFile> = vec![];
            if job_cancelled(&directory) {
                tx.send((
                    job_id,
                    MessagePayload::Static(JOB_CANCELLED),
                    Some(Stage::Cancelled),
                ))
                .await
                .unwrap();
                continue 'll;
            }
            let result = run_tool(
                &pnp2p_path,
                PNP2P_PROBE,
                &HashMap::from([
                    ("OPCODE", PathValue::from(arg_opcode.clone())),
                    (
                        "TORRENTTYPE",
                        PathValue::from(format!("--{}", torrent.get_arg())),
                    ),
                    ("NEGKEY", PathValue::from("pn-probe-main".to_string())),
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
                            let idx = payload.get(0).and_then(|v| v.as_str()).unwrap_or("?");
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
                        5 => return Some(ToolResult::Fail),
                        _ => {}
                    }
                    None
                },
            )
            .await;

            match result {
                ToolResult::Success => {
                    if probe_rows.is_empty() {
                        tx.send((
                            job_id,
                            MessagePayload::Static(PROBE_FAIL),
                            Some(Stage::Failed),
                        ))
                        .await
                        .unwrap();
                        continue 'll;
                    }
                    let list = format_probe_rows(&probe_rows).join("\n");
                    tx.send((
                        job_id,
                        MessagePayload::Progress(PROBE_ROW, vec![list]),
                        Some(Stage::Probed),
                    ))
                    .await
                    .unwrap();
                }
                ToolResult::Fail => {
                    tx.send((
                        job_id,
                        MessagePayload::Static(PROBE_FAIL),
                        Some(Stage::Failed),
                    ))
                    .await
                    .unwrap();
                }
                _ => {
                    tx.send((
                        job_id,
                        MessagePayload::Static(PROBE_FAIL),
                        Some(Stage::Failed),
                    ))
                    .await
                    .unwrap();
                }
            }
            println!("[Pandora Probe] End of Session");
            continue 'll;
        }
        sleep(Duration::from_secs(5)).await;
        pulse.try_send(()).ok();
    }
}

async fn run_preview_job(
    directory: PathBuf,
    shots: Vec<(u64, String)>,
    watermark_font: Option<PathBuf>,
    job_id: u64,
    server_id: Option<u64>,
    tx: &Sender<CommData>,
    pulse: &Sender<()>,
) {
    if job_cancelled(&directory) {
        tx.send((
            job_id,
            MessagePayload::Static(JOB_CANCELLED),
            Some(Stage::Cancelled),
        ))
        .await
        .ok();
        return;
    }

    let subtitle = directory.join("contents").join("subtitle.ass");
    let input = directory
        .join("contents")
        .join("torrent")
        .join("input.mkv");
    let work_dir = directory.join("work");
    let fonts_dir = stage_preview_fonts(&directory, server_id).await;
    let watermark = load_preview_font(watermark_font.as_deref());
    let label_font = load_preview_font(watermark_font.as_deref());
    let mut rendered: Vec<(String, PathBuf)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (idx, (centiseconds, label)) in shots.into_iter().enumerate() {
        if job_cancelled(&directory) {
            tx.send((
                job_id,
                MessagePayload::Static(JOB_CANCELLED),
                Some(Stage::Cancelled),
            ))
            .await
            .ok();
            return;
        }
        pulse.try_send(()).ok();
        let raw = work_dir.join(format!("preview_raw_{}.png", idx + 1));
        let out = work_dir.join(format!("preview_{}.png", idx + 1));
        if let Err(e) =
            ffmpeg_screenshot(&input, &subtitle, &fonts_dir, centiseconds, &raw).await
        {
            errors.push(format!("{}: {}", label, e));
            continue;
        }

        match tokio::fs::read(&raw).await {
            Ok(bytes) => match compose_preview(&bytes, &label, &watermark, &label_font) {
                Ok(composed) => {
                    if let Err(e) = tokio::fs::write(&out, composed).await {
                        errors.push(format!("{}: {}", label, e));
                        continue;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[Pandora Preview] compose failed for {} on job {}: {}",
                        label, job_id, e
                    );
                    if let Err(e) = tokio::fs::copy(&raw, &out).await {
                        errors.push(format!("{}: {}", label, e));
                        continue;
                    }
                }
            },
            Err(e) => {
                errors.push(format!("{}: {}", label, e));
                continue;
            }
        }
        rendered.push((label, out));
    }

    if rendered.is_empty() {
        let reason = if errors.is_empty() {
            "no preview frames were rendered".to_string()
        } else {
            errors.join("; ")
        };
        tx.send((
            job_id,
            MessagePayload::Progress(PREVIEW_FAIL, vec![reason]),
            Some(Stage::Failed),
        ))
        .await
        .ok();
        return;
    }

    let mut args = vec![rendered.len().to_string()];
    for (label, path) in rendered {
        args.push(label);
        args.push(path.display().to_string());
    }
    tx.send((
        job_id,
        MessagePayload::Progress(PREVIEW_DONE, args),
        Some(Stage::Uploaded),
    ))
    .await
    .ok();
}

async fn stage_preview_fonts(directory: &Path, server_id: Option<u64>) -> PathBuf {
    let subtitle = directory.join("contents").join("subtitle.ass");
    let fonts_dir = directory.join("work").join("fonts");
    tokio::fs::create_dir_all(&fonts_dir).await.ok();
    let sub = SubstationAlpha::load(subtitle, true).await;
    let names = sub.font_names();
    if names.is_empty() {
        return fonts_dir;
    }
    let mut roots = Vec::new();
    if let Some(server_id) = server_id {
        roots.push(PathBuf::from("DB").join("fontconfig").join(server_id.to_string()));
    }
    roots.push(PathBuf::from("DB").join("fontconfig").join("global"));
    let font_files = tokio::task::spawn_blocking(move || find_fonts_with_roots(&names, &roots))
        .await
        .unwrap_or_default();
    for path in font_files {
        let Some(name) = path.file_name() else {
            continue;
        };
        let _ = tokio::fs::copy(&path, fonts_dir.join(name)).await;
    }
    fonts_dir
}

fn load_preview_font(path: Option<&Path>) -> Font {
    path.and_then(|path| Font::from_path(path).ok())
        .unwrap_or_else(Font::fallback)
}

fn format_probe_rows(rows: &[ProbeFile]) -> Vec<String> {
    let basenames: Vec<String> = rows.iter().map(|r| basename(&r.name)).collect();
    let direct_tokens: Vec<Option<String>> = basenames.iter().map(|n| episode_token(n)).collect();
    let tokens = if direct_tokens.iter().filter(|t| t.is_some()).count() >= 2 {
        direct_tokens
    } else {
        sequence_tokens(&basenames)
    };
    let detected = tokens.iter().filter(|t| t.is_some()).count() >= 2;
    rows.iter()
        .zip(basenames.iter())
        .zip(tokens.iter())
        .map(|((row, name), token)| {
            if detected {
                if let Some(t) = token {
                    return format!("`{}` — E{}", row.idx, t);
                }
            }
            format!(
                "`{}` — {} ({}MB)",
                row.idx,
                name,
                string_byte_to_mb(&row.size)
            )
        })
        .collect()
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
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            r"(?i)\s-\s*(\d{1,4}(?:v\d+)?)\b",
            r"(?i)(?:^|[\s._\-\[])[Ss]\d{1,2}[Ee](\d{1,4}(?:v\d+)?)\b",
            r"(?i)(?:^|[\s._\-\[])[Ee][Pp]?\s*(\d{1,4}(?:v\d+)?)\b",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    for re in res {
        if let Some(caps) = re.captures(name) {
            if let Some(m) = caps.get(1) {
                return Some(m.as_str().to_string());
            }
        }
    }
    None
}

fn sequence_tokens(names: &[String]) -> Vec<Option<String>> {
    let candidates: Vec<Vec<String>> = names.iter().map(|n| numeric_candidates(n)).collect();
    let max_cols = candidates.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut best_col = None;
    let mut best_score = 0usize;
    for col in 0..max_cols {
        let nums: Vec<Option<u64>> = candidates
            .iter()
            .map(|c| c.get(col).and_then(|t| token_number(t)))
            .collect();
        let present: Vec<u64> = nums.iter().filter_map(|n| *n).collect();
        if present.len() < 2 {
            continue;
        }
        let mut score = 0usize;
        for pair in nums.windows(2) {
            if let [Some(a), Some(b)] = pair {
                if *b == *a + 1 {
                    score += 1;
                }
            }
        }
        if score > best_score {
            best_score = score;
            best_col = Some(col);
        }
    }
    if best_score == 0 {
        return vec![None; names.len()];
    }
    let col = best_col.unwrap();
    candidates
        .into_iter()
        .map(|c| c.get(col).cloned())
        .collect()
}

fn numeric_candidates(name: &str) -> Vec<String> {
    static BRACKET_RE: OnceLock<Regex> = OnceLock::new();
    static DELIM_RE: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    let bracket_re = BRACKET_RE.get_or_init(|| Regex::new(r"(?i)\[(\d{1,4}(?:v\d+)?)\]").unwrap());
    for caps in bracket_re.captures_iter(name) {
        if let Some(m) = caps.get(1) {
            out.push(m.as_str().to_string());
        }
    }
    let delim_re = DELIM_RE
        .get_or_init(|| Regex::new(r"(?i)(?:^|[\s._\-])([0-9]{1,4}(?:v\d+)?)(?:$|[\s._\-\(\[])").unwrap());
    for caps in delim_re.captures_iter(name) {
        if let Some(m) = caps.get(1) {
            let token = m.as_str().to_string();
            if !out.iter().any(|v| v == &token) {
                out.push(token);
            }
        }
    }
    out
}

fn token_number(token: &str) -> Option<u64> {
    let digits: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok()
}
