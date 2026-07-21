use super::core::{base64_decode_bytes, require_local, submit, ApiAuth, AppState};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::lib::image::timeline::{TimelineSpec, TimelineTrack, render_timeline};
use crate::lib::mpeg::studio::StudioTrackMode;
use crate::lib::p2p::nyaaise::TorrentType;
use crate::pnworker::core::{Job, JobType, StudioJobRequest};
use crate::pnworker::studio::{
    StudioMeta, StudioStore, studio_job_display, studio_render_presets,
};

fn identity(auth: &ApiAuth, state: &AppState) -> Result<(u64, u64), Response> {
    let guild_id = require_local(auth)?;
    Ok((guild_id, state.api_author))
}

fn error_response(error: String) -> Response {
    let status = if error.contains("was not found") {
        StatusCode::NOT_FOUND
    } else if error.contains("not a collaborator") {
        StatusCode::FORBIDDEN
    } else if error.contains("expired") || error.contains("do not have a current Studio") {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, error).into_response()
}

fn studio_json(meta: &StudioMeta, current: bool) -> Value {
    json!({
        "studio_id": meta.studio_id,
        "current": current,
        "source_kind": meta.source_kind.label(),
        "sources": meta.sources.iter().map(|source| json!({
            "keyword": source.keyword,
            "kind": source.kind.label(),
            "duration_ms": source.duration_ms,
            "fps_num": source.fps_num,
            "fps_den": source.fps_den,
            "width": source.width,
            "height": source.height,
            "has_audio": source.has_audio,
        })).collect::<Vec<_>>(),
        "tracks": meta.tracks.iter().map(track_json).collect::<Vec<_>>(),
        "collaborators": meta.collaborators.iter().map(u64::to_string).collect::<Vec<_>>(),
        "next_track_id": meta.next_track_id,
        "total_duration_ms": meta.total_duration_ms,
        "fps_num": meta.fps_num,
        "fps_den": meta.fps_den,
        "created_at": meta.created_at,
        "last_command_at": meta.last_command_at,
        "expires_at": meta.expires_at,
        "disowned_at": meta.disowned_at,
    })
}

fn mode_name(mode: StudioTrackMode) -> &'static str {
    match mode {
        StudioTrackMode::Insert => "insert",
        StudioTrackMode::Override => "override",
        StudioTrackMode::Duck => "duck",
    }
}

fn track_json(track: &crate::pnworker::studio::StudioTrack) -> Value {
    json!({
        "id": track.id,
        "mode": mode_name(track.mode),
        "offset_ms": track.offset_ms,
        "duration_ms": track.duration_ms,
        "display_name": track.display_name,
        "volume_percent": track.volume_percent,
        "duck_volume_percent": track.duck_volume_percent,
        "fade_ms": track.fade_ms,
        "trim_start_ms": track.trim_start_ms,
        "trim_end_ms": track.trim_end_ms,
    })
}

fn parse_mode(raw: &str) -> Result<StudioTrackMode, Response> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "insert" => Ok(StudioTrackMode::Insert),
        "override" => Ok(StudioTrackMode::Override),
        "duck" => Ok(StudioTrackMode::Duck),
        _ => Err((StatusCode::BAD_REQUEST, "mode must be insert, override, or duck").into_response()),
    }
}

fn keywords_string(keywords: Vec<String>) -> Result<String, Response> {
    let keywords = keywords.into_iter()
        .map(|keyword| keyword.trim().to_string())
        .filter(|keyword| !keyword.is_empty())
        .collect::<Vec<_>>();
    if keywords.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "keywords must not be empty").into_response());
    }
    Ok(keywords.join(","))
}

#[derive(Deserialize)]
pub(super) struct KeywordsReq {
    keywords: Vec<String>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().list_owned(guild_id, user_id).await {
        Ok(studios) => Json(Value::Array(studios.iter()
            .map(|(meta, current)| studio_json(meta, *current))
            .collect())).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<KeywordsReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let keywords = match keywords_string(req.keywords) {
        Ok(keywords) => keywords,
        Err(response) => return response,
    };
    match StudioStore::new().create_from_keywords(guild_id, user_id, &keywords).await {
        Ok(meta) => (StatusCode::CREATED, Json(studio_json(&meta, true))).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn current(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().details(guild_id, user_id, None).await {
        Ok(meta) => Json(studio_json(&meta, true)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn details(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(studio_id): Path<String>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let store = StudioStore::new();
    let current_id = store.inspect_current(guild_id, user_id).await.ok().map(|meta| meta.studio_id);
    match store.details(guild_id, user_id, Some(&studio_id)).await {
        Ok(meta) => Json(studio_json(&meta, current_id.as_deref() == Some(meta.studio_id.as_str()))).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn switch(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(studio_id): Path<String>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().switch(guild_id, user_id, &studio_id).await {
        Ok(meta) => Json(studio_json(&meta, true)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn reown(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(studio_id): Path<String>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().reown(guild_id, user_id, Some(&studio_id)).await {
        Ok(meta) => Json(studio_json(&meta, true)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn disown(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().disown(guild_id, user_id).await {
        Ok(meta) => Json(studio_json(&meta, false)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn replace_keywords(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<KeywordsReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let keywords = match keywords_string(req.keywords) {
        Ok(keywords) => keywords,
        Err(response) => return response,
    };
    match StudioStore::new().replace_keywords(guild_id, user_id, &keywords).await {
        Ok((meta, removed_tracks)) => Json(json!({
            "studio": studio_json(&meta, true),
            "removed_tracks": removed_tracks,
        })).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
pub(super) struct AddTrackReq {
    audio_b64: String,
    filename: String,
    mode: String,
    #[serde(default)]
    duck_volume_percent: Option<u8>,
    #[serde(default)]
    fade_seconds: Option<f64>,
}

pub(super) async fn add_track(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<AddTrackReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let mode = match parse_mode(&req.mode) {
        Ok(mode) => mode,
        Err(response) => return response,
    };
    let duck_volume = req.duck_volume_percent.unwrap_or(100);
    if duck_volume > 100 {
        return (StatusCode::BAD_REQUEST, "duck_volume_percent must be from 0 to 100").into_response();
    }
    let fade_seconds = req.fade_seconds.unwrap_or(0.0);
    if !fade_seconds.is_finite() || !(0.0..=3600.0).contains(&fade_seconds) {
        return (StatusCode::BAD_REQUEST, "fade_seconds must be from 0 to 3600").into_response();
    }
    if mode != StudioTrackMode::Duck
        && (req.duck_volume_percent.is_some() || req.fade_seconds.is_some())
    {
        return (StatusCode::BAD_REQUEST, "duck settings require mode=duck").into_response();
    }
    let bytes = match base64_decode_bytes(&req.audio_b64) {
        Ok(bytes) => bytes,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("audio_b64: {}", error)).into_response(),
    };
    let ext = safe_extension(&req.filename);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temp = std::env::temp_dir().join(format!("pandora-studio-api-{}-{}.{}", user_id, nonce, ext));
    if let Err(error) = tokio::fs::write(&temp, bytes).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to stage audio: {}", error)).into_response();
    }
    let result = StudioStore::new().add_track_from_path(
        guild_id,
        user_id,
        &temp,
        mode,
        Some(&req.filename),
        duck_volume,
        (fade_seconds * 1000.0).round() as u64,
    ).await;
    tokio::fs::remove_file(&temp).await.ok();
    match result {
        Ok(track) => Json(track_json(&track)).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
pub(super) struct EditTrackReq {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    volume_percent: Option<u16>,
    #[serde(default)]
    duck_volume_percent: Option<u8>,
    #[serde(default)]
    fade_seconds: Option<f64>,
}

pub(super) async fn edit_track(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(track_id): Path<u64>,
    Json(req): Json<EditTrackReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let mode = match req.mode.as_deref().map(parse_mode).transpose() {
        Ok(mode) => mode,
        Err(response) => return response,
    };
    if req.volume_percent.is_some_and(|volume| volume > 200) {
        return (StatusCode::BAD_REQUEST, "volume_percent must be from 0 to 200").into_response();
    }
    if req.duck_volume_percent.is_some_and(|volume| volume > 100) {
        return (StatusCode::BAD_REQUEST, "duck_volume_percent must be from 0 to 100").into_response();
    }
    let fade_ms = match req.fade_seconds {
        Some(seconds) if seconds.is_finite() && (0.0..=3600.0).contains(&seconds) => {
            Some((seconds * 1000.0).round() as u64)
        }
        Some(_) => return (StatusCode::BAD_REQUEST, "fade_seconds must be from 0 to 3600").into_response(),
        None => None,
    };
    match StudioStore::new().edit_track(
        guild_id,
        user_id,
        track_id,
        mode,
        req.volume_percent,
        req.duck_volume_percent,
        fade_ms,
    ).await {
        Ok(track) => Json(track_json(&track)).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
pub(super) struct MoveTrackReq {
    offset: String,
}

pub(super) async fn move_track(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(track_id): Path<u64>,
    Json(req): Json<MoveTrackReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().move_track(guild_id, user_id, track_id, &req.offset).await {
        Ok(track) => Json(json!({ "id": track.id, "offset_ms": track.offset_ms })).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
pub(super) struct CutTrackReq {
    side: String,
    seconds: f64,
}

pub(super) async fn cut_track(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(track_id): Path<u64>,
    Json(req): Json<CutTrackReq>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let sides = match req.side.trim().to_ascii_lowercase().as_str() {
        "start" => (true, false),
        "end" => (false, true),
        "both" => (true, true),
        _ => return (StatusCode::BAD_REQUEST, "side must be start, end, or both").into_response(),
    };
    if !req.seconds.is_finite() || !(0.001..=86_400.0).contains(&req.seconds) {
        return (StatusCode::BAD_REQUEST, "seconds must be from 0.001 to 86400").into_response();
    }
    match StudioStore::new().cut_track(
        guild_id,
        user_id,
        track_id,
        (req.seconds * 1000.0).round() as u64,
        sides.0,
        sides.1,
    ).await {
        Ok(track) => Json(json!({
            "id": track.id,
            "duration_ms": track.duration_ms,
            "trim_start_ms": track.trim_start_ms,
            "trim_end_ms": track.trim_end_ms,
        })).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn remove_track(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(track_id): Path<u64>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    match StudioStore::new().remove_track(guild_id, user_id, track_id).await {
        Ok(meta) => Json(json!({ "removed_track": track_id, "studio": studio_json(&meta, true) })).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn source_media(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(source_index): Path<usize>,
    headers: HeaderMap,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let meta = match StudioStore::new().inspect_current(guild_id, user_id).await {
        Ok(meta) => meta,
        Err(error) => return error_response(error),
    };
    let Some(source) = meta.sources.get(source_index) else {
        return (StatusCode::NOT_FOUND, "Studio source was not found").into_response();
    };
    stream_media(source.path.clone(), &headers).await
}

pub(super) async fn track_media(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(track_id): Path<u64>,
    headers: HeaderMap,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let meta = match StudioStore::new().inspect_current(guild_id, user_id).await {
        Ok(meta) => meta,
        Err(error) => return error_response(error),
    };
    let Some(track) = meta.tracks.iter().find(|track| track.id == track_id) else {
        return (StatusCode::NOT_FOUND, "Studio track was not found").into_response();
    };
    stream_media(track.path.clone(), &headers).await
}

async fn stream_media(path: PathBuf, headers: &HeaderMap) -> Response {
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return (StatusCode::NOT_FOUND, "Studio media was not found").into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, "Studio media was not found").into_response();
        }
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    };
    let file_size = metadata.len();
    if file_size == 0 {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "Studio media is empty").into_response();
    }
    let range = match headers.get(header::RANGE).and_then(|value| value.to_str().ok()) {
        Some(raw) => match parse_byte_range(raw, file_size) {
            Some(range) => Some(range),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{}", file_size))
                    .body(Body::empty())
                    .unwrap();
            }
        },
        None => None,
    };
    let (start, end, status) = range
        .map(|(start, end)| (start, end, StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, file_size - 1, StatusCode::OK));
    let content_length = end - start + 1;
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    };
    if start > 0 {
        if let Err(error) = file.seek(std::io::SeekFrom::Start(start)).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    }
    let stream = ReaderStream::new(file.take(content_length));
    let mut response = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::CONTENT_TYPE, media_content_type(&path))
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .body(Body::from_stream(stream))
        .unwrap();
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", start, end, file_size).parse().unwrap(),
        );
    }
    response
}

fn parse_byte_range(raw: &str, file_size: u64) -> Option<(u64, u64)> {
    let value = raw.trim().strip_prefix("bytes=")?;
    if value.contains(',') || file_size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(file_size);
        if suffix == 0 {
            return None;
        }
        return Some((file_size - suffix, file_size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= file_size {
        return None;
    }
    let end = if end.is_empty() {
        file_size - 1
    } else {
        end.parse::<u64>().ok()?.min(file_size - 1)
    };
    (end >= start).then_some((start, end))
}

fn media_content_type(path: &FsPath) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4") | Some("m4v") | Some("mov") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("m4a") | Some("aac") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("ogg") | Some("oga") | Some("opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        _ => "application/octet-stream",
    }
}

pub(super) async fn timeline(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let meta = match StudioStore::new().snapshot(guild_id, user_id).await {
        Ok(meta) => meta,
        Err(error) => return error_response(error),
    };
    let spec = TimelineSpec {
        duration_ms: meta.total_duration_ms,
        tracks: meta.tracks.iter().map(|track| TimelineTrack {
            id: track.id,
            name: track.display_name.clone(),
            mode: track.mode,
            volume_percent: track.volume_percent,
            offset_ms: track.offset_ms,
            duration_ms: track.duration_ms,
        }).collect(),
    };
    match tokio::task::spawn_blocking(move || render_timeline(&spec)).await {
        Ok(Ok(png)) => ([(header::CONTENT_TYPE, "image/png")], png).into_response(),
        Ok(Err(error)) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[derive(Default, Deserialize)]
pub(super) struct RenderReq {
    #[serde(default)]
    track_id: Option<u64>,
    #[serde(default)]
    channel_id: Option<String>,
}

pub(super) async fn preview(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<RenderReq>,
) -> Response {
    let Some(track_id) = req.track_id else {
        return (StatusCode::BAD_REQUEST, "track_id is required").into_response();
    };
    queue_render(state, auth, req.channel_id, Some(track_id)).await
}

pub(super) async fn render(
    State(state): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<RenderReq>,
) -> Response {
    queue_render(state, auth, req.channel_id, None).await
}

async fn queue_render(
    state: AppState,
    auth: ApiAuth,
    channel_id: Option<String>,
    preview_track: Option<u64>,
) -> Response {
    let (guild_id, user_id) = match identity(&auth, &state) {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let channel_id = match channel_id {
        Some(id) => match id.trim().parse::<u64>() {
            Ok(id) => id,
            Err(_) => return (StatusCode::BAD_REQUEST, "channel_id must be a numeric string").into_response(),
        },
        None => 0,
    };
    let store = StudioStore::new();
    let current = match store.inspect_current(guild_id, user_id).await {
        Ok(meta) => meta,
        Err(error) => return error_response(error),
    };
    let preview = preview_track.is_some();
    let (job_preset, video_preset) = studio_render_presets(current.source_kind, guild_id, preview);
    let job_type = if preview { JobType::StudioPreview } else { JobType::Studio };
    let mut job = Job::new_api(
        user_id,
        channel_id,
        job_type,
        TorrentType::Link(format!("studio:{}", current.studio_id)),
        Vec::new(),
        "EN".to_string(),
        Some(guild_id),
    );
    let (manifest, meta) = match store.stage_render_snapshot(
        guild_id,
        user_id,
        &job.directory,
        preview_track,
        video_preset,
    ).await {
        Ok(snapshot) => snapshot,
        Err(error) => return error_response(error),
    };
    job.display_link = Some(studio_job_display(&meta, preview_track));
    job.preset = job_preset;
    job.studio = Some(StudioJobRequest { manifest });
    submit(&state, job).await
}

#[cfg(test)]
mod tests {
    use super::parse_byte_range;

    #[test]
    fn byte_ranges_support_open_bounded_and_suffix_forms() {
        assert_eq!(parse_byte_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_byte_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_byte_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_byte_range("bytes=90-200", 100), Some((90, 99)));
    }

    #[test]
    fn byte_ranges_reject_invalid_or_multiple_ranges() {
        assert_eq!(parse_byte_range("items=0-1", 100), None);
        assert_eq!(parse_byte_range("bytes=100-", 100), None);
        assert_eq!(parse_byte_range("bytes=20-10", 100), None);
        assert_eq!(parse_byte_range("bytes=0-1,4-5", 100), None);
    }
}

fn safe_extension(filename: &str) -> String {
    FsPath::new(filename).extension().and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| extension.len() <= 8 && extension.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "audio".to_string())
}
