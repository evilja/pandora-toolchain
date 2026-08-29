use axum::body::Body;
use axum::extract::Path;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio_util::io::ReaderStream;

use crate::lib::bin::resolve_runtime_binary;

use super::client::Config;
use super::observe::{info, token_tag, warn};

const HLS_ROOT: &str = "DB/lumiere/hls";
const HLS_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const EXPIRES_FILE: &str = ".expires";
const MASTER_FILE: &str = "master.m3u8";
const MEDIA_FILE: &str = "media.m3u8";

pub struct HlsPublication {
    url: String,
    directory: PathBuf,
}

impl HlsPublication {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn revoke(self) {
        tokio::fs::remove_dir_all(self.directory).await.ok();
    }
}

pub async fn publish_hls(
    source: &FsPath,
    config: &Config,
    cancel_file: Option<&FsPath>,
) -> Result<HlsPublication, String> {
    cleanup_expired_hls().await;
    let source = tokio::fs::canonicalize(source)
        .await
        .map_err(|e| format!("HLS source is unavailable: {e}"))?;
    let metadata = tokio::fs::metadata(&source)
        .await
        .map_err(|e| format!("HLS source metadata is unavailable: {e}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("HLS source is not a non-empty file".to_string());
    }

    let root = PathBuf::from(HLS_ROOT);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("HLS storage could not be created: {e}"))?;
    let token = crate::lib::secret::random_hex_token()
        .map_err(|_| "secure HLS capability generation failed".to_string())?;
    let final_directory = root.join(&token);
    let temporary = root.join(format!(".{token}.tmp"));
    tokio::fs::remove_dir_all(&temporary).await.ok();
    tokio::fs::create_dir(&temporary)
        .await
        .map_err(|e| format!("HLS staging directory could not be created: {e}"))?;

    let result = build_hls(&source, &temporary, cancel_file).await;
    if let Err(error) = result {
        tokio::fs::remove_dir_all(&temporary).await.ok();
        return Err(error);
    }

    let expires_at = unix_now().saturating_add(HLS_TTL.as_secs());
    if let Err(error) =
        tokio::fs::write(temporary.join(EXPIRES_FILE), format!("{expires_at}\n")).await
    {
        tokio::fs::remove_dir_all(&temporary).await.ok();
        return Err(format!("HLS expiry metadata could not be written: {error}"));
    }
    if let Err(error) = tokio::fs::rename(&temporary, &final_directory).await {
        tokio::fs::remove_dir_all(&temporary).await.ok();
        return Err(format!("HLS output could not be published: {error}"));
    }

    let mut url = config.public_url().clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Lumiere public URL cannot be used for HLS paths".to_string())?;
        segments.pop_if_empty();
        segments.push("lumiere");
        segments.push("v1");
        segments.push("hls");
        segments.push(&token);
        segments.push(MASTER_FILE);
    }
    info(
        &format!("hls {}", token_tag(&token)),
        format!("published {} as HLS for 12h", source.display()),
    );
    Ok(HlsPublication {
        url: url.to_string(),
        directory: final_directory,
    })
}

async fn build_hls(
    source: &FsPath,
    directory: &FsPath,
    cancel_file: Option<&FsPath>,
) -> Result<(), String> {
    let mut command = Command::new(resolve_runtime_binary("ffmpeg"));
    command
        .current_dir(directory)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c",
            "copy",
            "-start_number",
            "0",
            "-hls_time",
            "6",
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "vod",
            "-hls_flags",
            "independent_segments",
            "-hls_segment_filename",
            "chunk-%05d.ts",
            MEDIA_FILE,
        ]);
    command.kill_on_drop(true);
    let output = {
        let output = command.output();
        tokio::pin!(output);
        loop {
            tokio::select! {
                result = &mut output => {
                    break result.map_err(|e| format!("ffmpeg could not start the HLS mux: {e}"))?;
                }
                _ = tokio::time::sleep(Duration::from_millis(250)), if cancel_file.is_some() => {
                    if tokio::fs::metadata(cancel_file.unwrap()).await.is_ok() {
                        return Err("HLS mux was cancelled".to_string());
                    }
                }
            }
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            "ffmpeg failed to create HLS output".to_string()
        } else {
            format!("ffmpeg failed to create HLS output: {detail}")
        });
    }

    let playlist_path = directory.join(MEDIA_FILE);
    let playlist = tokio::fs::read_to_string(&playlist_path)
        .await
        .map_err(|e| format!("HLS media playlist is missing: {e}"))?;
    let (duration, segment_names) = validate_media_playlist(&playlist)?;
    let mut segment_bytes = 0u64;
    for name in &segment_names {
        segment_bytes = segment_bytes.saturating_add(
            tokio::fs::metadata(directory.join(name))
                .await
                .map_err(|e| format!("HLS chunk `{name}` is missing: {e}"))?
                .len(),
        );
    }
    let bandwidth = ((segment_bytes as f64 * 8.0 / duration) * 1.10)
        .ceil()
        .clamp(1.0, u64::MAX as f64) as u64;
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}\n{MEDIA_FILE}\n"
    );
    tokio::fs::write(directory.join(MASTER_FILE), master)
        .await
        .map_err(|e| format!("HLS master playlist could not be written: {e}"))?;
    Ok(())
}

fn validate_media_playlist(playlist: &str) -> Result<(f64, Vec<String>), String> {
    let mut duration = 0.0f64;
    let mut segments = Vec::new();
    for line in playlist
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value
                .trim_end_matches(',')
                .parse::<f64>()
                .map_err(|_| "HLS media playlist has an invalid segment duration".to_string())?;
            if seconds.is_finite() && seconds > 0.0 {
                duration += seconds;
            }
        } else if !line.starts_with('#') {
            if !is_chunk_filename(line) {
                return Err("HLS media playlist contains an unsafe chunk path".to_string());
            }
            segments.push(line.to_string());
        }
    }
    if duration <= 0.0 || segments.is_empty() {
        return Err("HLS media playlist contains no playable chunks".to_string());
    }
    Ok((duration, segments))
}

pub async fn serve_hls(
    Path((token, filename)): Path<(String, String)>,
    method: Method,
) -> Response {
    if !valid_token(&token) || !is_public_hls_filename(&filename) {
        return hls_not_found();
    }
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }
    let directory = PathBuf::from(HLS_ROOT).join(&token);
    let expires_at = match read_expiry(&directory).await {
        Some(value) => value,
        None => return hls_not_found(),
    };
    if expires_at <= unix_now() {
        tokio::fs::remove_dir_all(&directory).await.ok();
        return hls_not_found();
    }
    let path = directory.join(&filename);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        _ => return hls_not_found(),
    };
    let content_type = if filename.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else {
        "video/mp2t"
    };
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::CACHE_CONTROL, "private, no-store, max-age=0")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Robots-Tag", "noindex, nofollow, noarchive");
    if method == Method::HEAD {
        return builder.body(Body::empty()).unwrap();
    }
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return hls_not_found(),
    };
    builder
        .body(Body::from_stream(ReaderStream::with_capacity(
            file,
            256 * 1024,
        )))
        .unwrap()
}

pub async fn cleanup_expired_hls() {
    let root = PathBuf::from(HLS_ROOT);
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let expired = if name.starts_with('.') && name.ends_with(".tmp") {
            staging_is_stale(&path).await
        } else {
            read_expiry(&path)
                .await
                .map(|expires_at| expires_at <= unix_now())
                .unwrap_or(true)
        };
        if expired {
            if let Err(error) = tokio::fs::remove_dir_all(&path).await {
                let log_token = name
                    .strip_prefix('.')
                    .and_then(|value| value.strip_suffix(".tmp"))
                    .unwrap_or(&name);
                warn(
                    &format!("hls {}", token_tag(log_token)),
                    format!("could not remove expired output: {error}"),
                );
            }
        }
    }
}

async fn staging_is_stale(directory: &FsPath) -> bool {
    let modified = tokio::fs::metadata(directory)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    modified
        .and_then(|value| SystemTime::now().duration_since(value).ok())
        .map(|age| age >= STALE_STAGING_AGE)
        .unwrap_or(false)
}

async fn read_expiry(directory: &FsPath) -> Option<u64> {
    tokio::fs::read_to_string(directory.join(EXPIRES_FILE))
        .await
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_public_hls_filename(filename: &str) -> bool {
    matches!(filename, MASTER_FILE | MEDIA_FILE) || is_chunk_filename(filename)
}

fn is_chunk_filename(filename: &str) -> bool {
    filename
        .strip_prefix("chunk-")
        .and_then(|value| value.strip_suffix(".ts"))
        .is_some_and(|digits| digits.len() == 5 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn hls_not_found() -> Response {
    (StatusCode::NOT_FOUND, "HLS output not found").into_response()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_hls_names_are_a_small_fixed_surface() {
        assert!(is_public_hls_filename("master.m3u8"));
        assert!(is_public_hls_filename("media.m3u8"));
        assert!(is_public_hls_filename("chunk-00042.ts"));
        assert!(!is_public_hls_filename(".expires"));
        assert!(!is_public_hls_filename("../chunk-00042.ts"));
        assert!(!is_public_hls_filename("chunk-42.ts"));
    }

    #[test]
    fn media_playlist_validation_rejects_external_or_nested_paths() {
        let valid = "#EXTM3U\n#EXTINF:6.0,\nchunk-00000.ts\n#EXTINF:2.5,\nchunk-00001.ts\n";
        let (duration, chunks) = validate_media_playlist(valid).unwrap();
        assert_eq!(duration, 8.5);
        assert_eq!(chunks, ["chunk-00000.ts", "chunk-00001.ts"]);
        assert!(
            validate_media_playlist("#EXTM3U\n#EXTINF:6,\nhttps://other.example/a.ts\n").is_err()
        );
        assert!(validate_media_playlist("#EXTM3U\n#EXTINF:6,\nsub/chunk-00000.ts\n").is_err());
    }
}
