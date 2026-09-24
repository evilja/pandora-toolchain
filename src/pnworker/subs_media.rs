use crate::lib::bin::resolve_runtime_binary;
use crate::lib::mpeg::probe::{MediaProbe, probe_media};
use crate::lib::secret::random_hex_token;
use crate::pnworker::core::{CommData, Stage};
use crate::pnworker::messages::{JOB_CANCELLED, MessagePayload, SUBSMEDIA_DONE, SUBSMEDIA_FAIL, SUBSMEDIA_PROG};
use crate::pnworker::util::{ToolResult, job_cancelled};
use crate::pnworker::workers::probeworker::extract_subtitle_tracks;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, Instant, sleep};

// Where `/subs` finds a video that came from a link. Each prepared source is one directory named
// by a 256-bit capability: the editor page holds the token and fetches its files through the
// public `/subs/media/:token/:file` route, which is how a `<video>` element — which cannot send a
// bearer header — gets to play it.
pub const SUBS_MEDIA_ROOT: &str = "DB/cache/subsmedia";
const EXPIRES_FILE: &str = ".expires";
// Long enough for an evening of timing, short enough that a server full of proxies clears itself.
pub const SUBS_MEDIA_TTL_SECS: u64 = 12 * 60 * 60;
// Height of the proxy. A phone shows the video in a strip a few hundred pixels tall, and the
// editor only needs to see where a sign is and when a mouth moves.
const PROXY_HEIGHT: u32 = 540;
// Waveform resolution: one peak per 10 ms, matching the editor's own `PEAK_RATE`, taken from mono
// 8 kHz PCM so each peak covers 80 samples.
const PEAK_RATE: u32 = 100;
const PEAK_SAMPLE_RATE: u32 = 8000;
const STAGING_STALE: Duration = Duration::from_secs(6 * 60 * 60);

pub fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// The files a prepared source exposes, and nothing else — `.expires` and any staging leftovers are
// never served. Track names are generated here, so anything outside `track-<n>.<ext>` is refused
// rather than resolved.
pub fn public_file_type(name: &str) -> Option<&'static str> {
    match name {
        "manifest.json" => return Some("application/json"),
        "video.mp4" => return Some("video/mp4"),
        "peaks.bin" => return Some("application/octet-stream"),
        _ => {}
    }
    let rest = name.strip_prefix("track-")?;
    let (number, ext) = rest.split_once('.')?;
    if number.is_empty() || number.len() > 3 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match ext {
        "ass" | "ssa" => Some("text/plain; charset=utf-8"),
        "srt" => Some("text/plain; charset=utf-8"),
        "vtt" => Some("text/vtt; charset=utf-8"),
        _ => None,
    }
}

pub fn media_dir(token: &str) -> PathBuf {
    PathBuf::from(SUBS_MEDIA_ROOT).join(token)
}

// Whether a prepared source is still inside its lifetime. A directory without a readable expiry
// is treated as expired, which is also how a half-written one is kept from being served.
pub async fn is_live(directory: &Path) -> bool {
    read_expiry(directory).await.map(|at| at > unix_now()).unwrap_or(false)
}

pub async fn cleanup_expired_subs_media() {
    let Ok(mut entries) = tokio::fs::read_dir(SUBS_MEDIA_ROOT).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let expired = if name.starts_with('.') && name.ends_with(".tmp") {
            staging_is_stale(&path).await
        } else {
            !is_live(&path).await
        };
        if expired {
            tokio::fs::remove_dir_all(&path).await.ok();
        }
    }
}

async fn staging_is_stale(directory: &Path) -> bool {
    tokio::fs::metadata(directory)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age >= STAGING_STALE)
        .unwrap_or(false)
}

async fn read_expiry(directory: &Path) -> Option<u64> {
    tokio::fs::read_to_string(directory.join(EXPIRES_FILE))
        .await
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Turns a downloaded source into what the browser editor can use: a small H.264/AAC MP4 every
// phone decodes, a waveform the page would otherwise have to decode the whole file to draw, and
// the file's own subtitle tracks as text. The input is whatever the torrent held — usually a
// 10-bit HEVC MKV no mobile browser will play — so the proxy is always made, even for a source
// that happens to be playable, which also keeps what a phone downloads small.
pub async fn run_subs_media_job(
    directory: PathBuf,
    job_id: u64,
    pnmpeg_path: &str,
    tx: &Sender<CommData>,
) {
    if job_cancelled(&directory) {
        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled)))
            .await
            .ok();
        return;
    }
    let fail = |reason: String| async move {
        tx.send((
            job_id,
            MessagePayload::Progress(SUBSMEDIA_FAIL, vec![reason]),
            Some(Stage::Failed),
        ))
        .await
        .ok();
    };
    let input = directory.join("contents").join("torrent").join("input.mkv");
    let probe = match probe_media(input.clone()).await {
        Ok(probe) if probe.has_video => probe,
        Ok(_) => return fail("the file has no video stream".to_string()).await,
        Err(error) => return fail(error).await,
    };
    let token = match random_hex_token() {
        Ok(token) => token,
        Err(error) => return fail(format!("no randomness for a token: {error}")).await,
    };
    let root = PathBuf::from(SUBS_MEDIA_ROOT);
    let staging = root.join(format!(".{token}.tmp"));
    if let Err(error) = tokio::fs::create_dir_all(&staging).await {
        return fail(format!("could not create the output folder: {error}")).await;
    }

    tx.send((job_id, MessagePayload::Progress(SUBSMEDIA_PROG, vec!["0".to_string()]), None))
        .await
        .ok();

    // Tracks first: they take seconds, and a script to start from is worth having even if the
    // proxy then fails. Failing to extract is not failing the job — plenty of releases have no
    // text subtitles at all.
    let tracks_dir = directory.join("work").join("subs");
    let (result, extracted) = extract_subtitle_tracks(&input, &tracks_dir, job_id, pnmpeg_path).await;
    if matches!(result, ToolResult::Cancel) || job_cancelled(&directory) {
        tokio::fs::remove_dir_all(&staging).await.ok();
        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled)))
            .await
            .ok();
        return;
    }
    let mut tracks = Vec::new();
    for track in extracted.iter() {
        let Some(path) = &track.path else { continue };
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        let file = format!("track-{}.{}", tracks.len(), ext);
        if public_file_type(&file).is_none() {
            continue;
        }
        if tokio::fs::copy(path, staging.join(&file)).await.is_err() {
            continue;
        }
        tracks.push(serde_json::json!({
            "file": file,
            "language": track.language,
            "title": track.title,
            "codec": track.codec,
        }));
    }

    let peaks = match transcode_proxy(&input, &staging, &probe, &directory, job_id, tx).await {
        Ok(peaks) => peaks,
        Err(ProxyError::Cancelled) => {
            tokio::fs::remove_dir_all(&staging).await.ok();
            tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled)))
                .await
                .ok();
            return;
        }
        Err(ProxyError::Failed(reason)) => {
            tokio::fs::remove_dir_all(&staging).await.ok();
            return fail(reason).await;
        }
    };
    if !peaks.is_empty() {
        tokio::fs::write(staging.join("peaks.bin"), &peaks).await.ok();
    }

    let expires_at = unix_now() + SUBS_MEDIA_TTL_SECS;
    let manifest = serde_json::json!({
        "duration_ms": probe.duration_ms,
        "width": probe.width,
        "height": probe.height,
        "fps_num": probe.fps_num,
        "fps_den": probe.fps_den,
        "has_audio": probe.has_audio,
        "peaks": if peaks.is_empty() { serde_json::Value::Null } else { serde_json::json!({ "file": "peaks.bin", "rate": PEAK_RATE }) },
        "tracks": tracks,
        "expires_at": expires_at,
    });
    let written = async {
        tokio::fs::write(staging.join("manifest.json"), manifest.to_string()).await?;
        tokio::fs::write(staging.join(EXPIRES_FILE), format!("{expires_at}\n")).await?;
        tokio::fs::rename(&staging, root.join(&token)).await
    }
    .await;
    if let Err(error) = written {
        tokio::fs::remove_dir_all(&staging).await.ok();
        return fail(format!("could not store the result: {error}")).await;
    }
    // The token rides along as an argument the message text never prints: it is a capability, and
    // only the job's progress row — which only the submitter can read — should carry it.
    tx.send((
        job_id,
        MessagePayload::Progress(SUBSMEDIA_DONE, vec![token]),
        Some(Stage::Uploaded),
    ))
    .await
    .ok();
}

enum ProxyError {
    Cancelled,
    Failed(String),
}

// One ffmpeg pass writes the proxy and, on a second output, streams mono 8 kHz PCM to stdout,
// which is folded into peaks as it arrives so the audio never lands on disk. Progress comes from
// `-progress` on stderr, measured against the probed duration.
async fn transcode_proxy(
    input: &Path,
    staging: &Path,
    probe: &MediaProbe,
    directory: &Path,
    job_id: u64,
    tx: &Sender<CommData>,
) -> Result<Vec<u8>, ProxyError> {
    let mut args: Vec<String> = vec![
        "-hide_banner", "-nostdin", "-loglevel", "error", "-nostats",
        "-progress", "pipe:2", "-y",
        "-i",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    args.push(input.display().to_string());
    // `V` rather than `v`: a cover image in an MKV is a video stream too, and it is never the one
    // anybody is timing against.
    args.extend(
        [
            "-map", "0:V:0", "-map", "0:a:0?",
            "-map_metadata", "-1", "-map_chapters", "-1",
            "-vf",
        ]
        .into_iter()
        .map(String::from),
    );
    args.push(format!("scale=-2:'min({PROXY_HEIGHT},trunc(ih/2)*2)'"));
    args.extend(
        [
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "27",
            "-maxrate", "1500k", "-bufsize", "3000k",
            "-pix_fmt", "yuv420p", "-profile:v", "high", "-level", "4.0",
            // Frequent keyframes so seeking on a phone lands quickly; passthrough so frame times
            // stay the source's, which is what the script is timed against.
            "-g", "48", "-keyint_min", "12", "-fps_mode", "passthrough",
            "-c:a", "aac", "-b:a", "112k", "-ac", "2",
            "-movflags", "+faststart",
        ]
        .into_iter()
        .map(String::from),
    );
    args.push(staging.join("video.mp4").display().to_string());
    if probe.has_audio {
        args.extend(
            [
                "-map", "0:a:0", "-vn", "-ac", "1", "-ar",
            ]
            .into_iter()
            .map(String::from),
        );
        args.push(PEAK_SAMPLE_RATE.to_string());
        args.extend(["-f", "s16le", "pipe:1"].into_iter().map(String::from));
    }

    let mut child = Command::new(resolve_runtime_binary("ffmpeg"))
        .args(&args)
        .stdin(Stdio::null())
        .stdout(if probe.has_audio { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ProxyError::Failed(format!("ffmpeg could not start: {error}")))?;

    let peaks_task = child.stdout.take().map(|stdout| tokio::spawn(fold_peaks(stdout)));
    let stderr = child.stderr.take().unwrap();
    let duration_us = probe.duration_ms.max(1) * 1000;
    let tx_progress = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail: Vec<String> = Vec::new();
        let mut last_percent = 0u64;
        let mut last_sent = Instant::now();
        while let Ok(Some(line)) = lines.next_line().await {
            // Both keys are microseconds; `out_time_ms` is a historical misnomer kept for
            // compatibility, and older builds only print that one.
            let value = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="));
            if let Some(value) = value {
                if let Ok(us) = value.trim().parse::<u64>() {
                    let percent = (us.saturating_mul(100) / duration_us).min(99);
                    if percent > last_percent && last_sent.elapsed() >= Duration::from_secs(2) {
                        last_percent = percent;
                        last_sent = Instant::now();
                        tx_progress
                            .send((job_id, MessagePayload::Progress(SUBSMEDIA_PROG, vec![percent.to_string()]), None))
                            .await
                            .ok();
                    }
                }
                continue;
            }
            if line.contains('=') && !line.contains(' ') {
                continue;
            }
            tail.push(line);
            if tail.len() > 4 {
                tail.remove(0);
            }
        }
        tail.join("\n")
    });

    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = sleep(Duration::from_secs(1)) => {
                if job_cancelled(directory) {
                    child.kill().await.ok();
                    if let Some(task) = peaks_task { task.abort(); }
                    stderr_task.abort();
                    return Err(ProxyError::Cancelled);
                }
            }
        }
    };
    let tail = stderr_task.await.unwrap_or_default();
    let peaks = match peaks_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };
    match status {
        Ok(status) if status.success() => Ok(peaks),
        Ok(_) => Err(ProxyError::Failed(if tail.trim().is_empty() {
            "ffmpeg failed".to_string()
        } else {
            tail
        })),
        Err(error) => Err(ProxyError::Failed(format!("ffmpeg did not finish: {error}"))),
    }
}

// Reads s16le mono PCM and keeps the loudest sample of every window, scaled so the loudest window
// in the file is 255. The whole file's peaks are 100 bytes a second — a 24-minute episode is
// 144 KB — so they are held in memory and normalised at the end.
async fn fold_peaks(mut stdout: tokio::process::ChildStdout) -> Vec<u8> {
    let window = (PEAK_SAMPLE_RATE / PEAK_RATE) as usize;
    let mut raw: Vec<u16> = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    let mut current: u16 = 0;
    let mut filled = 0usize;
    loop {
        let read = match stdout.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let mut index = 0;
        while index < read {
            let (low, high) = match carry.take() {
                Some(low) => (low, buffer[index]),
                None if index + 1 < read => {
                    let pair = (buffer[index], buffer[index + 1]);
                    index += 1;
                    pair
                }
                None => {
                    carry = Some(buffer[index]);
                    index += 1;
                    continue;
                }
            };
            index += 1;
            let sample = i16::from_le_bytes([low, high]).unsigned_abs();
            if sample > current {
                current = sample;
            }
            filled += 1;
            if filled == window {
                raw.push(current);
                current = 0;
                filled = 0;
            }
        }
    }
    if filled > 0 {
        raw.push(current);
    }
    normalise_peaks(&raw)
}

fn normalise_peaks(raw: &[u16]) -> Vec<u8> {
    let max = raw.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return vec![0; raw.len()];
    }
    raw.iter()
        .map(|&value| ((value as u32 * 255 + max as u32 / 2) / max as u32) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_generated_names_are_public() {
        for name in ["manifest.json", "video.mp4", "peaks.bin", "track-0.ass", "track-12.srt", "track-3.vtt"] {
            assert!(public_file_type(name).is_some(), "{name}");
        }
        for name in [
            ".expires", "track-.ass", "track-1.mkv", "track-a.ass", "track-1234.ass",
            "../manifest.json", "track-1.ass/..", "input.mkv", "",
        ] {
            assert!(public_file_type(name).is_none(), "{name}");
        }
    }

    #[test]
    fn tokens_are_64_hex_characters() {
        assert!(valid_token(&"a".repeat(64)));
        assert!(!valid_token(&"a".repeat(63)));
        assert!(!valid_token(&format!("{}/", "a".repeat(63))));
        assert!(!valid_token(&"g".repeat(64)));
    }

    #[test]
    fn peaks_scale_to_the_loudest_window() {
        assert_eq!(normalise_peaks(&[0, 100, 200]), vec![0, 128, 255]);
        assert_eq!(normalise_peaks(&[0, 0]), vec![0, 0]);
        assert!(normalise_peaks(&[]).is_empty());
    }
}
