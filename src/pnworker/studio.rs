use crate::lib::mpeg::probe::{probe_media, MediaProbe};
use crate::lib::mpeg::studio::{PreviewWindow, StudioInput, StudioRenderManifest, StudioRenderTrack, StudioSourceKind, StudioTrackMode, StudioVideoPreset};
use crate::pnworker::core::KeepKind;
use crate::pnworker::keep::{now_secs, resolve_studio_keywords, sanitize_keyword};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::fs;
use tokio::sync::Mutex;

pub const STUDIO_ACTIVE_TTL_SECS: u64 = 24 * 60 * 60;
pub const STUDIO_DISOWNED_TTL_SECS: u64 = 30 * 60;
pub const STUDIO_MAX_TRACKS: usize = 64;

fn default_duck_volume_percent() -> u8 {
    100
}

fn studio_lock() -> &'static Arc<Mutex<()>> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(())))
}

fn studios_root() -> PathBuf {
    PathBuf::from("DB").join("cache").join("studios")
}

fn guild_root(guild_id: u64) -> PathBuf {
    studios_root().join(guild_id.to_string())
}

fn studio_dir(guild_id: u64, studio_id: &str) -> PathBuf {
    guild_root(guild_id).join(studio_id)
}

fn meta_path(guild_id: u64, studio_id: &str) -> PathBuf {
    studio_dir(guild_id, studio_id).join("meta.json")
}

fn user_pointer_path(guild_id: u64, user_id: u64) -> PathBuf {
    studios_root().join("users").join(guild_id.to_string()).join(format!("{}.json", user_id))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StudioSource {
    pub keyword: String,
    pub path: PathBuf,
    pub kind: KeepKind,
    pub duration_ms: u64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StudioTrack {
    pub id: u64,
    pub path: PathBuf,
    pub mode: StudioTrackMode,
    pub offset_ms: u64,
    pub duration_ms: u64,
    pub display_name: String,
    #[serde(default = "default_duck_volume_percent")]
    pub duck_volume_percent: u8,
    #[serde(default)]
    pub fade_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StudioMeta {
    pub guild_id: u64,
    pub studio_id: String,
    pub source_kind: KeepKind,
    pub sources: Vec<StudioSource>,
    pub collaborators: Vec<u64>,
    pub tracks: Vec<StudioTrack>,
    pub next_track_id: u64,
    pub total_duration_ms: u64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub created_at: u64,
    pub last_command_at: u64,
    pub expires_at: u64,
    pub disowned_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UserPointers {
    current: Option<String>,
    last: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StudioStore;

impl StudioStore {
    pub fn new() -> Self {
        Self
    }

    pub async fn create_from_keywords(
        &self,
        guild_id: u64,
        user_id: u64,
        raw_keywords: &str,
    ) -> Result<StudioMeta, String> {
        let keywords = parse_keywords(raw_keywords)?;
        let resolved = resolve_studio_keywords(guild_id, &keywords).await?;
        let mut sources = Vec::with_capacity(resolved.paths.len());
        let mut probes = Vec::with_capacity(resolved.paths.len());
        for path in &resolved.paths {
            let probe = probe_media(path.clone()).await?;
            validate_video_probe(path, &probe)?;
            probes.push(probe);
        }
        validate_source_compatibility(&probes)?;

        let _guard = studio_lock().lock().await;
        let studio_id = allocate_studio_id(guild_id).await?;
        let final_dir = studio_dir(guild_id, &studio_id);
        let stage_dir = studios_root().join(format!(".stage-{}", random_hex(12)?));
        let stage_sources = stage_dir.join("sources");
        let stage_tracks = stage_dir.join("tracks");
        if let Err(e) = fs::create_dir_all(&stage_sources).await {
            return Err(format!("failed to create Studio staging directory: {}", e));
        }
        fs::create_dir_all(&stage_tracks).await.map_err(|e| e.to_string())?;
        for (idx, (source, probe)) in resolved.paths.iter().zip(probes.iter()).enumerate() {
            let ext = safe_extension(source).unwrap_or_else(|| "mkv".to_string());
            let target = stage_sources.join(format!("{:03}.{}", idx + 1, ext));
            if let Err(e) = fs::copy(source, &target).await {
                fs::remove_dir_all(&stage_dir).await.ok();
                return Err(format!("failed to copy source {}: {}", idx + 1, e));
            }
            let final_path = final_dir.join("sources").join(format!("{:03}.{}", idx + 1, ext));
            sources.push(StudioSource {
                keyword: keywords[idx].clone(),
                path: final_path,
                kind: resolved.kind,
                duration_ms: probe.duration_ms,
                fps_num: probe.fps_num,
                fps_den: probe.fps_den,
                width: probe.width,
                height: probe.height,
                has_audio: probe.has_audio,
            });
        }

        let now = now_secs();
        let meta = StudioMeta {
            guild_id,
            studio_id: studio_id.clone(),
            source_kind: resolved.kind,
            total_duration_ms: probes.iter().map(|p| p.duration_ms).sum(),
            fps_num: probes[0].fps_num,
            fps_den: probes[0].fps_den,
            sources,
            collaborators: vec![user_id],
            tracks: Vec::new(),
            next_track_id: 1,
            created_at: now,
            last_command_at: now,
            expires_at: now + STUDIO_ACTIVE_TTL_SECS,
            disowned_at: None,
        };
        if let Err(e) = fs::create_dir_all(&guild_root(guild_id)).await {
            fs::remove_dir_all(&stage_dir).await.ok();
            return Err(e.to_string());
        }
        if let Err(e) = fs::rename(&stage_dir, &final_dir).await {
            fs::remove_dir_all(&stage_dir).await.ok();
            return Err(format!("failed to commit Studio files: {}", e));
        }
        if let Err(e) = write_json_atomic(&meta_path(guild_id, &studio_id), &meta).await {
            fs::remove_dir_all(&final_dir).await.ok();
            return Err(e);
        }

        let mut pointers = read_pointers(guild_id, user_id).await?;
        if let Some(previous) = pointers.current.clone() {
            if previous != studio_id {
                detach_user_locked(guild_id, user_id, &previous).await?;
            }
        }
        pointers.current = Some(studio_id.clone());
        pointers.last = Some(studio_id.clone());
        write_pointers(guild_id, user_id, &pointers).await?;
        Ok(meta)
    }

    pub async fn get_current(&self, guild_id: u64, user_id: u64) -> Result<StudioMeta, String> {
        let _guard = studio_lock().lock().await;
        let pointers = read_pointers(guild_id, user_id).await?;
        let id = pointers.current.ok_or_else(|| "you do not have a current Studio".to_string())?;
        let mut meta = self.authorized_meta_locked(guild_id, user_id, &id).await?;
        refresh_meta(&mut meta)?;
        write_json_atomic(&meta_path(guild_id, &id), &meta).await?;
        Ok(meta)
    }

    pub async fn reown(
        &self,
        guild_id: u64,
        user_id: u64,
        requested_id: Option<&str>,
    ) -> Result<StudioMeta, String> {
        let _guard = studio_lock().lock().await;
        let mut pointers = read_pointers(guild_id, user_id).await?;
        let id = requested_id.map(str::trim).filter(|id| !id.is_empty())
            .map(str::to_string).or(pointers.last.clone())
            .ok_or_else(|| "no Studio ID was supplied and no previous Studio exists".to_string())?;
        validate_studio_id(&id)?;
        let mut meta = self.read_meta_checked(guild_id, &id).await?;
        let now = now_secs();
        if meta.expires_at <= now {
            fs::remove_dir_all(studio_dir(guild_id, &id)).await.ok();
            if pointers.current.as_deref() == Some(&id) { pointers.current = None; }
            if pointers.last.as_deref() == Some(&id) { pointers.last = None; }
            write_pointers(guild_id, user_id, &pointers).await.ok();
            return Err(format!("Studio `{}` has expired", id));
        }
        if let Some(previous) = pointers.current.clone() {
            if previous != id {
                detach_user_locked(guild_id, user_id, &previous).await?;
            }
        }
        if !meta.collaborators.contains(&user_id) {
            meta.collaborators.push(user_id);
        }
        meta.last_command_at = now;
        meta.expires_at = now + STUDIO_ACTIVE_TTL_SECS;
        meta.disowned_at = None;
        write_json_atomic(&meta_path(guild_id, &id), &meta).await?;
        pointers.current = Some(id.clone());
        pointers.last = Some(id);
        write_pointers(guild_id, user_id, &pointers).await?;
        Ok(meta)
    }

    pub async fn disown(&self, guild_id: u64, user_id: u64) -> Result<StudioMeta, String> {
        let _guard = studio_lock().lock().await;
        let mut pointers = read_pointers(guild_id, user_id).await?;
        let id = pointers.current.clone().ok_or_else(|| "you do not have a current Studio".to_string())?;
        let mut meta = self.authorized_meta_locked(guild_id, user_id, &id).await?;
        meta.collaborators.retain(|id| *id != user_id);
        let now = now_secs();
        meta.last_command_at = now;
        if meta.collaborators.is_empty() {
            meta.expires_at = now + STUDIO_DISOWNED_TTL_SECS;
            meta.disowned_at = Some(now);
        } else {
            meta.expires_at = now + STUDIO_ACTIVE_TTL_SECS;
        }
        write_json_atomic(&meta_path(guild_id, &id), &meta).await?;
        pointers.current = None;
        pointers.last = Some(id);
        write_pointers(guild_id, user_id, &pointers).await?;
        Ok(meta)
    }

    pub async fn add_track_from_path(
        &self,
        guild_id: u64,
        user_id: u64,
        source: &Path,
        mode: StudioTrackMode,
        display_name: Option<&str>,
        duck_volume_percent: u8,
        fade_ms: u64,
    ) -> Result<StudioTrack, String> {
        if duck_volume_percent > 100 {
            return Err("duck volume must be a percentage from 0 to 100".to_string());
        }
        let (duck_volume_percent, fade_ms) = if mode == StudioTrackMode::Duck {
            (duck_volume_percent, fade_ms)
        } else {
            (100, 0)
        };
        let probe = probe_media(source.to_path_buf()).await?;
        if !probe.has_audio || probe.duration_ms == 0 {
            return Err("attachment must contain a non-empty decodable audio stream".to_string());
        }
        let _guard = studio_lock().lock().await;
        let mut meta = self.get_authorized_without_refresh_locked(guild_id, user_id).await?;
        if meta.tracks.len() >= STUDIO_MAX_TRACKS {
            return Err(format!("a Studio cannot contain more than {} tracks", STUDIO_MAX_TRACKS));
        }
        let id = meta.next_track_id;
        meta.next_track_id = meta.next_track_id.saturating_add(1);
        let ext = safe_extension(source).unwrap_or_else(|| "audio".to_string());
        let target = studio_dir(guild_id, &meta.studio_id).join("tracks").join(format!("{}.{}", id, ext));
        fs::create_dir_all(target.parent().unwrap()).await.map_err(|e| e.to_string())?;
        fs::copy(source, &target).await.map_err(|e| format!("failed to copy audio attachment: {}", e))?;
        let track = StudioTrack {
            id,
            path: target,
            mode,
            offset_ms: 0,
            duration_ms: probe.duration_ms,
            display_name: display_name.unwrap_or("audio").to_string(),
            duck_volume_percent,
            fade_ms,
        };
        meta.tracks.push(track.clone());
        refresh_meta(&mut meta)?;
        write_json_atomic(&meta_path(guild_id, &meta.studio_id), &meta).await?;
        Ok(track)
    }

    pub async fn move_track(&self, guild_id: u64, user_id: u64, track_id: u64, raw_offset: &str) -> Result<StudioTrack, String> {
        let _guard = studio_lock().lock().await;
        let mut meta = self.get_authorized_without_refresh_locked(guild_id, user_id).await?;
        let offset = parse_offset(raw_offset, meta.fps_num, meta.fps_den, meta.total_duration_ms)?;
        let track = meta.tracks.iter_mut().find(|track| track.id == track_id)
            .ok_or_else(|| format!("track `{}` does not exist", track_id))?;
        track.offset_ms = offset;
        let result = track.clone();
        refresh_meta(&mut meta)?;
        write_json_atomic(&meta_path(guild_id, &meta.studio_id), &meta).await?;
        Ok(result)
    }

    pub async fn remove_track(&self, guild_id: u64, user_id: u64, track_id: u64) -> Result<StudioMeta, String> {
        let _guard = studio_lock().lock().await;
        let mut meta = self.get_authorized_without_refresh_locked(guild_id, user_id).await?;
        let pos = meta.tracks.iter().position(|track| track.id == track_id)
            .ok_or_else(|| format!("track `{}` does not exist", track_id))?;
        let track = meta.tracks.remove(pos);
        refresh_meta(&mut meta)?;
        write_json_atomic(&meta_path(guild_id, &meta.studio_id), &meta).await?;
        fs::remove_file(track.path).await.ok();
        Ok(meta)
    }

    pub async fn snapshot(&self, guild_id: u64, user_id: u64) -> Result<StudioMeta, String> {
        self.get_current(guild_id, user_id).await
    }

    pub async fn inspect_current(&self, guild_id: u64, user_id: u64) -> Result<StudioMeta, String> {
        let _guard = studio_lock().lock().await;
        self.get_authorized_without_refresh_locked(guild_id, user_id).await
    }

    pub async fn stage_render_snapshot(
        &self,
        guild_id: u64,
        user_id: u64,
        job_dir: &Path,
        preview_track: Option<u64>,
        video_preset: StudioVideoPreset,
    ) -> Result<(PathBuf, StudioMeta), String> {
        let _guard = studio_lock().lock().await;
        let mut meta = self.get_authorized_without_refresh_locked(guild_id, user_id).await?;
        let preview_window = match preview_track {
            Some(id) => {
                let track = meta.tracks.iter().find(|track| track.id == id)
                    .ok_or_else(|| format!("track `{}` does not exist", id))?;
                let center = track.offset_ms.saturating_add(track.duration_ms / 2);
                Some(PreviewWindow::centered(center, meta.total_duration_ms))
            }
            None => None,
        };
        refresh_meta(&mut meta)?;

        let snapshot_dir = job_dir.join("contents").join("studio");
        let source_dir = snapshot_dir.join("sources");
        let track_dir = snapshot_dir.join("tracks");
        for dir in [&source_dir, &track_dir, &job_dir.join("work"), &job_dir.join("log")] {
            if let Err(e) = fs::create_dir_all(dir).await {
                fs::remove_dir_all(job_dir).await.ok();
                return Err(format!("failed to prepare Studio job snapshot: {}", e));
            }
        }

        let mut manifest = to_manifest(&meta, preview_window, video_preset);
        for (idx, input) in manifest.sources.iter_mut().enumerate() {
            let ext = safe_extension(&input.path).unwrap_or_else(|| "mkv".to_string());
            let target = source_dir.join(format!("{:03}.{}", idx + 1, ext));
            if let Err(e) = link_or_copy(&input.path, &target).await {
                fs::remove_dir_all(job_dir).await.ok();
                return Err(format!("failed to snapshot Studio source {}: {}", idx + 1, e));
            }
            input.path = target;
        }
        for track in &mut manifest.tracks {
            let ext = safe_extension(&track.path).unwrap_or_else(|| "audio".to_string());
            let target = track_dir.join(format!("{}.{}", track.id, ext));
            if let Err(e) = link_or_copy(&track.path, &target).await {
                fs::remove_dir_all(job_dir).await.ok();
                return Err(format!("failed to snapshot Studio track {}: {}", track.id, e));
            }
            track.path = target;
        }

        let manifest_path = snapshot_dir.join("manifest.json");
        let raw = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        if let Err(e) = fs::write(&manifest_path, raw).await {
            fs::remove_dir_all(job_dir).await.ok();
            return Err(format!("failed to write Studio render manifest: {}", e));
        }
        write_json_atomic(&meta_path(guild_id, &meta.studio_id), &meta).await?;
        Ok((manifest_path, meta))
    }

    pub async fn cleanup_expired(&self) -> Result<usize, String> {
        let _guard = studio_lock().lock().await;
        cleanup_expired_locked().await
    }

    async fn read_meta_checked(&self, guild_id: u64, studio_id: &str) -> Result<StudioMeta, String> {
        let meta = read_json::<StudioMeta>(&meta_path(guild_id, studio_id)).await?
            .ok_or_else(|| format!("Studio `{}` was not found in this guild", studio_id))?;
        if meta.guild_id != guild_id || meta.studio_id != studio_id {
            return Err("Studio metadata does not belong to this guild".to_string());
        }
        Ok(meta)
    }

    async fn authorized_meta_locked(&self, guild_id: u64, user_id: u64, studio_id: &str) -> Result<StudioMeta, String> {
        let meta = self.read_meta_checked(guild_id, studio_id).await?;
        if meta.expires_at <= now_secs() {
            return Err(format!("Studio `{}` has expired", studio_id));
        }
        if !meta.collaborators.contains(&user_id) {
            return Err("you are not a collaborator on this Studio".to_string());
        }
        Ok(meta)
    }

    async fn get_authorized_without_refresh_locked(&self, guild_id: u64, user_id: u64) -> Result<StudioMeta, String> {
        let pointers = read_pointers(guild_id, user_id).await?;
        let id = pointers.current.ok_or_else(|| "you do not have a current Studio".to_string())?;
        self.authorized_meta_locked(guild_id, user_id, &id).await
    }
}

pub async fn cleanup_studios_startup() {
    let store = StudioStore::new();
    store.cleanup_expired().await.ok();
}

pub async fn cleanup_expired_studios() {
    cleanup_studios_startup().await;
}

pub fn parse_keywords(raw: &str) -> Result<Vec<String>, String> {
    let keywords = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect::<Vec<_>>();
    if keywords.is_empty() {
        return Err("at least one comma-separated keyword is required".to_string());
    }
    for keyword in &keywords {
        if sanitize_keyword(keyword).is_none() {
            return Err(format!("invalid keyword `{}`", keyword));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for keyword in &keywords {
        let normalized = sanitize_keyword(keyword).unwrap();
        if !seen.insert(normalized.clone()) {
            return Err(format!("duplicate keyword `{}`", normalized));
        }
    }
    Ok(keywords.into_iter().map(|s| sanitize_keyword(&s).unwrap()).collect())
}

pub fn parse_offset(raw: &str, fps_num: u32, fps_den: u32, video_duration_ms: u64) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("offset is empty".to_string());
    }
    let value_ms = if let Some(frame_text) = raw.strip_suffix('f').or_else(|| raw.strip_prefix("frame:").map(|v| v)) {
        let frames = frame_text.trim().parse::<u64>().map_err(|_| "invalid frame offset".to_string())?;
        if fps_num == 0 || fps_den == 0 {
            return Err("Studio source FPS is unavailable for a frame offset".to_string());
        }
        let value = frames as f64 * fps_den as f64 * 1000.0 / fps_num as f64;
        if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 {
            return Err("frame offset is not finite".to_string());
        }
        value.round() as u64
    } else if raw.contains(':') {
        parse_colon_time(raw)?
    } else {
        let seconds = raw.strip_suffix('s').unwrap_or(raw).trim().parse::<f64>()
            .map_err(|_| "invalid seconds offset".to_string())?;
        if !seconds.is_finite() || seconds < 0.0 || seconds > u64::MAX as f64 / 1000.0 {
            return Err("offset must be a finite non-negative time".to_string());
        }
        (seconds * 1000.0).round() as u64
    };
    if value_ms >= video_duration_ms {
        return Err("offset must be before the end of the video".to_string());
    }
    Ok(value_ms)
}

fn parse_colon_time(raw: &str) -> Result<u64, String> {
    let parts = raw.split(':').collect::<Vec<_>>();
    if parts.len() != 2 && parts.len() != 3 {
        return Err("time offset must be MM:SS.mmm or HH:MM:SS.mmm".to_string());
    }
    let seconds_part = parts.last().unwrap().parse::<f64>().map_err(|_| "invalid time offset".to_string())?;
    if !seconds_part.is_finite() || seconds_part < 0.0 || seconds_part >= 60.0 {
        return Err("seconds in a colon offset must be in the range 0..60".to_string());
    }
    let minute_index = parts.len() - 2;
    let minutes = parts[minute_index].parse::<u64>().map_err(|_| "invalid minutes in offset".to_string())?;
    let hours = if parts.len() == 3 {
        parts[0].parse::<u64>().map_err(|_| "invalid hours in offset".to_string())?
    } else { 0 };
    let total = hours.checked_mul(3_600_000)
        .and_then(|v| v.checked_add(minutes.checked_mul(60_000)?))
        .and_then(|v| v.checked_add((seconds_part * 1000.0).round() as u64))
        .ok_or_else(|| "offset is too large".to_string())?;
    Ok(total)
}

fn validate_video_probe(path: &Path, probe: &MediaProbe) -> Result<(), String> {
    if !probe.has_video || probe.width == 0 || probe.height == 0 || probe.duration_ms == 0 {
        return Err(format!("source `{}` has no usable video stream", path.display()));
    }
    if probe.fps_num == 0 || probe.fps_den == 0 {
        return Err(format!("source `{}` has no usable frame rate", path.display()));
    }
    Ok(())
}

fn validate_source_compatibility(probes: &[MediaProbe]) -> Result<(), String> {
    let Some(first) = probes.first() else { return Err("at least one source is required".to_string()); };
    for (idx, probe) in probes.iter().enumerate().skip(1) {
        if probe.width != first.width || probe.height != first.height {
            return Err(format!("source {} has incompatible dimensions", idx + 1));
        }
        if probe.fps_num as u64 * first.fps_den as u64 != first.fps_num as u64 * probe.fps_den as u64 {
            return Err(format!("source {} has an incompatible FPS", idx + 1));
        }
        if probe.has_audio != first.has_audio {
            return Err(format!("source {} has incompatible audio stream availability", idx + 1));
        }
    }
    Ok(())
}

fn safe_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext.len() <= 8 && ext.chars().all(|ch| ch.is_ascii_alphanumeric()) { Some(ext) } else { None }
}

fn validate_studio_id(id: &str) -> Result<(), String> {
    if id.len() < 16 || id.len() > 128 || !id.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("invalid Studio ID".to_string());
    }
    Ok(())
}

fn random_hex(bytes_len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; bytes_len];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{:02x}", b)).collect())
}

async fn allocate_studio_id(guild_id: u64) -> Result<String, String> {
    for _ in 0..32 {
        let id = random_hex(16)?;
        if fs::metadata(studio_dir(guild_id, &id)).await.is_err() { return Ok(id); }
    }
    Err("could not allocate a unique Studio ID".to_string())
}

fn refresh_meta(meta: &mut StudioMeta) -> Result<(), String> {
    let now = now_secs();
    if meta.expires_at <= now && !meta.collaborators.is_empty() {
        return Err(format!("Studio `{}` has expired", meta.studio_id));
    }
    meta.last_command_at = now;
    meta.expires_at = now + STUDIO_ACTIVE_TTL_SECS;
    meta.disowned_at = None;
    Ok(())
}

async fn detach_user_locked(guild_id: u64, user_id: u64, studio_id: &str) -> Result<(), String> {
    let path = meta_path(guild_id, studio_id);
    let Some(mut meta) = read_json::<StudioMeta>(&path).await? else { return Ok(()); };
    meta.collaborators.retain(|id| *id != user_id);
    if meta.collaborators.is_empty() {
        let now = now_secs();
        meta.expires_at = now + STUDIO_DISOWNED_TTL_SECS;
        meta.disowned_at = Some(now);
        meta.last_command_at = now;
    } else {
        meta.expires_at = now_secs() + STUDIO_ACTIVE_TTL_SECS;
    }
    write_json_atomic(&path, &meta).await
}

async fn read_pointers(guild_id: u64, user_id: u64) -> Result<UserPointers, String> {
    Ok(read_json(&user_pointer_path(guild_id, user_id)).await?.unwrap_or_default())
}

async fn write_pointers(guild_id: u64, user_id: u64, pointers: &UserPointers) -> Result<(), String> {
    write_json_atomic(&user_pointer_path(guild_id, user_id), pointers).await
}

async fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match fs::read_to_string(path).await {
        Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| format!("invalid JSON at {}: {}", path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

async fn link_or_copy(source: &Path, target: &Path) -> Result<(), String> {
    match fs::hard_link(source, target).await {
        Ok(()) => Ok(()),
        Err(_) => fs::copy(source, target).await.map(|_| ()).map_err(|e| e.to_string()),
    }
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "JSON path has no parent".to_string())?;
    fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    let suffix = random_hex(6)?;
    let temp = path.with_file_name(format!(".{}.tmp-{}", path.file_name().unwrap().to_string_lossy(), suffix));
    let raw = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fs::write(&temp, raw).await.map_err(|e| e.to_string())?;
    fs::rename(&temp, path).await.map_err(|e| e.to_string())
}

async fn cleanup_expired_locked() -> Result<usize, String> {
    let mut removed = 0usize;
    let mut guilds = match fs::read_dir(studios_root()).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    while let Some(guild) = guilds.next_entry().await.map_err(|e| e.to_string())? {
        if !guild.file_type().await.map_err(|e| e.to_string())?.is_dir() || guild.file_name() == "users" { continue; }
        if guild.file_name().to_string_lossy().starts_with(".stage-") {
            fs::remove_dir_all(guild.path()).await.ok();
            removed += 1;
            continue;
        }
        let mut entries = fs::read_dir(guild.path()).await.map_err(|e| e.to_string())?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            if !entry.file_type().await.map_err(|e| e.to_string())?.is_dir() { continue; }
            let expired = read_json::<StudioMeta>(&entry.path().join("meta.json")).await?.map(|m| m.expires_at <= now_secs()).unwrap_or(true);
            if expired {
                fs::remove_dir_all(entry.path()).await.ok();
                removed += 1;
            }
        }
    }
    cleanup_stale_pointers().await?;
    Ok(removed)
}

async fn cleanup_stale_pointers() -> Result<(), String> {
    let root = studios_root().join("users");
    let mut guilds = match fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    while let Some(guild) = guilds.next_entry().await.map_err(|e| e.to_string())? {
        let Ok(guild_id) = guild.file_name().to_string_lossy().parse::<u64>() else { continue; };
        let mut users = fs::read_dir(guild.path()).await.map_err(|e| e.to_string())?;
        while let Some(user) = users.next_entry().await.map_err(|e| e.to_string())? {
            let Some(mut pointer) = read_json::<UserPointers>(&user.path()).await? else { continue; };
            let current_valid = pointer.current.as_ref().map(|id| meta_path(guild_id, id).exists()).unwrap_or(false);
            let last_valid = pointer.last.as_ref().map(|id| meta_path(guild_id, id).exists()).unwrap_or(false);
            if !current_valid { pointer.current = None; }
            if !last_valid { pointer.last = None; }
            if pointer.current.is_none() && pointer.last.is_none() { fs::remove_file(user.path()).await.ok(); }
            else { write_json_atomic(&user.path(), &pointer).await?; }
        }
    }
    Ok(())
}

fn to_manifest(meta: &StudioMeta, preview: Option<PreviewWindow>, video_preset: StudioVideoPreset) -> StudioRenderManifest {
    StudioRenderManifest {
        sources: meta.sources.iter().map(|source| StudioInput { path: source.path.clone(), duration_ms: source.duration_ms, has_audio: source.has_audio }).collect(),
        tracks: meta.tracks.iter().map(|track| StudioRenderTrack {
            id: track.id, path: track.path.clone(), mode: track.mode, offset_ms: track.offset_ms,
            duration_ms: track.duration_ms, display_name: track.display_name.clone(),
            duck_volume_percent: track.duck_volume_percent, fade_ms: track.fade_ms,
        }).collect(),
        total_duration_ms: meta.total_duration_ms,
        fps_num: meta.fps_num,
        fps_den: meta.fps_den,
        source_kind: if meta.source_kind == KeepKind::Encode { StudioSourceKind::Encode } else { StudioSourceKind::Backup },
        video_preset,
        preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_documented_offset_forms() {
        assert_eq!(parse_offset("30s", 24, 1, 60_000), Ok(30_000));
        assert_eq!(parse_offset("1.25", 24, 1, 60_000), Ok(1_250));
        assert_eq!(parse_offset("01:02.500", 24, 1, 70_000), Ok(62_500));
        assert_eq!(parse_offset("01:02:03.500", 24, 1, 100_000_000), Ok(3_723_500));
        assert_eq!(parse_offset("24f", 24, 1, 60_000), Ok(1_000));
        assert_eq!(parse_offset("frame:48", 24, 1, 60_000), Ok(2_000));
    }

    #[test]
    fn rejects_bad_offsets_and_end() {
        assert!(parse_offset("-1s", 24, 1, 60_000).is_err());
        assert!(parse_offset("NaN", 24, 1, 60_000).is_err());
        assert!(parse_offset("60s", 24, 1, 60_000).is_err());
        assert!(parse_offset("1:60.0", 24, 1, 60_000).is_err());
    }

    #[test]
    fn frame_conversion_uses_source_fps() {
        assert_eq!(parse_offset("24000f", 24000, 1001, 2_000_000), Ok(1_001_000));
    }
}
