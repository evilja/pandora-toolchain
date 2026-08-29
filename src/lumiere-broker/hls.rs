use axum::body::Body;
use axum::extract::Path;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio_util::io::ReaderStream;

use crate::lib::bin::resolve_runtime_binary;
use crate::lib::mpeg::hls::{HlsNames, is_chunk_path, is_playlist_filename};

use super::client::Config;
use super::observe::{info, token_tag, warn};

const HLS_ROOT: &str = "DB/lumiere/hls";
const HLS_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const EXPIRES_FILE: &str = ".expires";

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

// Height names the output, width only decorates the master's STREAM-INF, so a source ffprobe cannot
// measure is a missing label rather than a failed publication.
async fn probe_dimensions(source: &FsPath) -> Option<(u32, u32)> {
    let mut command = Command::new(resolve_runtime_binary("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(source);
    command.kill_on_drop(true);
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let (width, height) = text.trim().split_once(',')?;
    Some((
        width.trim().parse::<u32>().ok()?,
        height.trim().parse::<u32>().ok()?,
    ))
}

// `source` is the finished MP4 to remux. `prepared` is a directory an encoder already wrote the
// layout into — a job whose server publishes HLS only muxes straight to it rather than producing an
// MP4 for this to take apart again — and is adopted whole when it holds a playlist this recognises.
pub async fn publish_hls(
    source: &FsPath,
    prepared: Option<&FsPath>,
    config: &Config,
    cancel_file: Option<&FsPath>,
) -> Result<HlsPublication, String> {
    cleanup_expired_hls().await;
    let adoptable = match prepared {
        Some(prepared) => prepared_media_playlist(prepared).await,
        None => None,
    };
    // Only the remux needs the MP4, and a job that muxed its own HLS has no MP4 to offer.
    let source = if adoptable.is_none() {
        let source = tokio::fs::canonicalize(source)
            .await
            .map_err(|e| format!("HLS source is unavailable: {e}"))?;
        let metadata = tokio::fs::metadata(&source)
            .await
            .map_err(|e| format!("HLS source metadata is unavailable: {e}"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err("HLS source is not a non-empty file".to_string());
        }
        Some(source)
    } else {
        None
    };

    let root = PathBuf::from(HLS_ROOT);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("HLS storage could not be created: {e}"))?;
    let token = crate::lib::secret::random_hex_token()
        .map_err(|_| "secure HLS capability generation failed".to_string())?;
    let final_directory = root.join(&token);
    let temporary = root.join(format!(".{token}.tmp"));
    tokio::fs::remove_dir_all(&temporary).await.ok();

    // Adoption is one rename of the encoder's directory into the staging name, so the layout is
    // never copied and never half-published; the ordinary path stages an empty directory and muxes
    // into it. Either way what follows sees the same thing.
    let names = match (adoptable, prepared) {
        (Some(names), Some(prepared)) => {
            tokio::fs::rename(prepared, &temporary).await.map_err(|e| {
                format!("HLS output prepared by the encoder could not be published: {e}")
            })?;
            names
        }
        _ => {
            tokio::fs::create_dir(&temporary)
                .await
                .map_err(|e| format!("HLS staging directory could not be created: {e}"))?;
            let source = source.as_deref().unwrap_or(FsPath::new(""));
            let names = HlsNames::new(
                probe_dimensions(source).await.map(|(_, height)| height),
                &crate::lib::secret::random_uuid_v4()
                    .map_err(|_| "secure HLS name generation failed".to_string())?,
            );
            if let Err(error) = build_hls(source, &temporary, &names, cancel_file).await {
                tokio::fs::remove_dir_all(&temporary).await.ok();
                return Err(error);
            }
            names
        }
    };

    if let Err(error) = finish_hls(&temporary, &names).await {
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
        segments.push(&names.master);
    }
    info(
        &format!("hls {}", token_tag(&token)),
        match source {
            Some(source) => format!("published {} as HLS for 12h", source.display()),
            None => format!("published the encoder's own HLS output ({}) for 12h", names.media),
        },
    );
    Ok(HlsPublication {
        url: url.to_string(),
        directory: final_directory,
    })
}

async fn build_hls(
    source: &FsPath,
    directory: &FsPath,
    names: &HlsNames,
    cancel_file: Option<&FsPath>,
) -> Result<(), String> {
    // ffmpeg writes chunks where it is told to and nowhere else — it will not create the directory
    // the segment pattern points into, and a missing one fails the mux rather than the open.
    tokio::fs::create_dir_all(directory.join(&names.chunk_directory))
        .await
        .map_err(|e| format!("HLS chunk directory could not be created: {e}"))?;
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
        ])
        .args(names.muxer_args());
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

    Ok(())
}

// What a directory an encoder wrote for us is called, if it holds one playlist this scheme
// recognises. A `None` here is not an error: it means the ordinary remux still has to run.
async fn prepared_media_playlist(directory: &FsPath) -> Option<HlsNames> {
    let mut entries = tokio::fs::read_dir(directory).await.ok()?;
    let mut found = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(names) = HlsNames::from_media_filename(&name) {
            // Two layouts in one directory is a leftover from an earlier attempt, and there is no
            // way to tell which one the chunks belong to. Remux instead of guessing.
            if found.is_some() {
                return None;
            }
            found = Some(names);
        }
    }
    found
}

// The master playlist, written last for both paths: the media playlist is read back and every chunk
// it names is measured, so a playlist pointing at something that is not there fails here rather
// than in a player. The bandwidth is the average the chunks actually add up to, with the 10% of
// headroom a player expects of the figure.
async fn finish_hls(directory: &FsPath, names: &HlsNames) -> Result<(), String> {
    let playlist_path = directory.join(&names.media);
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
    // Read off a chunk rather than the source: it is the one thing both paths are certain to have,
    // and it is what the player is actually going to be handed.
    let resolution = match probe_dimensions(&directory.join(&segment_names[0])).await {
        Some((width, height)) => format!(",RESOLUTION={width}x{height}"),
        None => String::new(),
    };
    let media = &names.media;
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}{resolution}\n{media}\n"
    );
    tokio::fs::write(directory.join(&names.master), master)
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
            if !is_chunk_path(line) {
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
    Path((token, resource)): Path<(String, String)>,
    method: Method,
) -> Response {
    if !valid_token(&token) || !is_public_hls_resource(&resource) {
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
    let path = directory.join(&resource);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        _ => return hls_not_found(),
    };
    let content_type = if resource.ends_with(".m3u8") {
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

fn is_public_hls_resource(resource: &str) -> bool {
    is_playlist_filename(resource) || is_chunk_path(resource)
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

    const ID: &str = "f28305c9-74d0-449e-920b-938155931dd6";

    #[test]
    fn public_hls_names_are_a_small_fixed_surface() {
        assert!(is_public_hls_resource(&format!("1080p_{ID}.m3u8")));
        assert!(is_public_hls_resource(&format!("chunk-1080p/p12-{ID}.ts")));
        assert!(!is_public_hls_resource(".expires"));
        assert!(!is_public_hls_resource("master.m3u8"));
        assert!(!is_public_hls_resource(&format!("../720p_{ID}.m3u8")));
        assert!(!is_public_hls_resource(&format!(
            "chunk-720p/../p0-{ID}.ts"
        )));
        assert!(!is_public_hls_resource(&format!("chunk-720p/p0-{ID}.ts.ts")));
        assert!(!is_public_hls_resource(&format!("720p_{ID}")));
        assert!(!is_public_hls_resource(&format!("720_{ID}.m3u8")));
        assert!(!is_public_hls_resource("720p_not-a-uuid.m3u8"));
        assert!(!is_public_hls_resource(&format!(
            "720p_{}.m3u8",
            ID.to_uppercase()
        )));
        assert!(!is_public_hls_resource(&format!("chunk-720p/px-{ID}.ts")));
        assert!(!is_public_hls_resource(&format!(
            "chunk-720p/sub/p0-{ID}.ts"
        )));
    }

    // The encoder's directory is adopted whole, so what is in it decides what gets published: one
    // recognisable playlist is a layout, and anything else has to be remuxed instead of guessed at.
    #[tokio::test]
    async fn a_prepared_directory_is_adopted_only_when_it_names_one_layout() {
        let root = std::env::temp_dir().join(format!("pandora-hls-{}", std::process::id()));
        tokio::fs::remove_dir_all(&root).await.ok();
        tokio::fs::create_dir_all(&root).await.unwrap();
        assert_eq!(prepared_media_playlist(&root).await, None);
        assert_eq!(prepared_media_playlist(&root.join("absent")).await, None);

        let names = HlsNames::new(Some(720), ID);
        tokio::fs::write(root.join(&names.media), "").await.unwrap();
        tokio::fs::write(root.join(&names.master), "").await.unwrap();
        assert_eq!(prepared_media_playlist(&root).await, Some(names));

        let other = HlsNames::new(Some(480), ID);
        tokio::fs::write(root.join(&other.media), "").await.unwrap();
        assert_eq!(prepared_media_playlist(&root).await, None);
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[test]
    fn media_playlist_validation_rejects_external_or_nested_paths() {
        let valid = format!(
            "#EXTM3U\n#EXTINF:6.0,\nchunk-720p/p0-{ID}.ts\n#EXTINF:2.5,\nchunk-720p/p1-{ID}.ts\n"
        );
        let (duration, chunks) = validate_media_playlist(&valid).unwrap();
        assert_eq!(duration, 8.5);
        assert_eq!(
            chunks,
            [
                format!("chunk-720p/p0-{ID}.ts"),
                format!("chunk-720p/p1-{ID}.ts")
            ]
        );
        assert!(
            validate_media_playlist("#EXTM3U\n#EXTINF:6,\nhttps://other.example/a.ts\n").is_err()
        );
        assert!(
            validate_media_playlist(&format!(
                "#EXTM3U\n#EXTINF:6,\nsub/chunk-720p/p0-{ID}.ts\n"
            ))
            .is_err()
        );
    }
}
