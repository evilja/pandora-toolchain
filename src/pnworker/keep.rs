use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::fs::{create_dir_all, read_to_string, remove_dir_all, write};

use crate::lib::mpeg::probe::{ffprobe_framerate, ffprobe_samplerate};
use crate::pnworker::core::{KeepKind, KeepRequest, Preset};

const KEEP_TTL_SECS: u64 = 5 * 60 * 60;
pub const KEYWORD_POOL_PATH: &str = "DB/config/global/environment/keyword_pool.pandora";
const DEFAULT_KEYWORDS: &[&str] = &[
    "akira", "aqua", "aster", "ciel", "ember", "fable", "glint", "hikari", "iris",
    "jade", "kumo", "lumen", "mika", "nagi", "onyx", "ruri", "sora", "toki",
    "umi", "yuki", "zero", "altair", "bell", "clover", "dawn", "echo", "frost",
    "halo", "ivory", "lyra", "mira", "noa", "opal", "pulse", "quartz", "rei",
];

pub fn normalize_pool_keyword(raw: &str) -> Option<String> {
    sanitize_keyword(raw)
}

pub fn configured_keyword_pool() -> Vec<String> {
    let mut out = match std::fs::read_to_string(KEYWORD_POOL_PATH) {
        Ok(raw) => {
            raw.lines()
                .filter(|line| {
                    let line = line.trim();
                    !line.is_empty() && !line.starts_with('#') && !line.starts_with(';')
                })
                .filter_map(sanitize_keyword)
                .collect::<Vec<_>>()
        }
        Err(_) => DEFAULT_KEYWORDS.iter().map(|s| (*s).to_string()).collect(),
    };
    out.sort();
    out.dedup();
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct KeepMeta {
    pub keyword: String,
    pub parent_keyword: String,
    pub kind: KeepKind,
    pub server_scope: String,
    pub output: String,
    pub expires_at: u64,
    pub preset: Option<String>,
    pub fps: Option<String>,
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub job_id: u64,
}

pub(crate) struct PreparedKeep {
    pub output_keyword: String,
    pub parent_keyword: String,
}

pub struct ResolvedKeywords {
    pub kind: KeepKind,
    pub paths: Vec<PathBuf>,
}

pub enum KeywordResolve {
    Ready(ResolvedKeywords),
    Waiting(Vec<String>),
}

pub(crate) fn scope(server_id: Option<u64>) -> String {
    server_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "global".to_string())
}

fn root() -> PathBuf {
    PathBuf::from("DB").join("cache").join("keeps")
}

fn scope_dir(server_scope: &str) -> PathBuf {
    root().join(sanitize_keyword(server_scope).unwrap_or_else(|| "global".to_string()))
}

fn keyword_dir(server_scope: &str, keyword: &str) -> PathBuf {
    scope_dir(server_scope).join(keyword)
}

fn meta_path(server_scope: &str, keyword: &str) -> PathBuf {
    keyword_dir(server_scope, keyword).join("meta.json")
}

pub(crate) fn output_path(meta: &KeepMeta) -> PathBuf {
    keyword_dir(&meta.server_scope, &meta.keyword).join(&meta.output)
}

pub(crate) async fn cleanup_keep_startup() {
    cleanup_expired_keeps().await;
}

pub(crate) async fn cleanup_expired_keeps() {
    let mut scopes = match tokio::fs::read_dir(root()).await {
        Ok(entries) => entries,
        Err(_) => return,
    };
    while let Ok(Some(scope_entry)) = scopes.next_entry().await {
        let mut entries = match tokio::fs::read_dir(scope_entry.path()).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let meta_file = entry.path().join("meta.json");
            let expired = match read_to_string(&meta_file).await {
                Ok(raw) => serde_json::from_str::<KeepMeta>(&raw)
                    .map(|m| m.expires_at <= now_secs())
                    .unwrap_or(true),
                Err(_) => true,
            };
            if expired {
                remove_dir_all(entry.path()).await.ok();
            }
        }
    }
}

pub(crate) async fn prepare_keep(
    server_scope: &str,
    kind: KeepKind,
    request: &KeepRequest,
) -> Result<PreparedKeep, String> {
    cleanup_expired_keeps().await;
    let parent_keyword = match request.keyword.as_deref().and_then(sanitize_keyword) {
        Some(keyword) => {
            let meta = read_meta(server_scope, &keyword)
                .await?
                .ok_or_else(|| format!("unknown keyword `{}`", keyword))?;
            if meta.kind != kind {
                return Err(format!("keyword `{}` is not a {} keyword", keyword, kind.label()));
            }
            meta.parent_keyword
        }
        None => String::new(),
    };

    let output_keyword = allocate_keyword(server_scope).await?;
    let parent_keyword = if parent_keyword.is_empty() {
        output_keyword.clone()
    } else {
        parent_keyword
    };
    Ok(PreparedKeep {
        output_keyword,
        parent_keyword,
    })
}

pub(crate) async fn store_output(
    server_scope: &str,
    kind: KeepKind,
    keep: &KeepRequest,
    source: PathBuf,
    preset: Option<&Preset>,
    job_id: u64,
) -> Result<KeepMeta, String> {
    let output_keyword = keep
        .output_keyword
        .as_deref()
        .and_then(sanitize_keyword)
        .ok_or_else(|| "keep output keyword was not prepared".to_string())?;
    let parent_keyword = keep
        .parent_keyword
        .as_deref()
        .and_then(sanitize_keyword)
        .unwrap_or_else(|| output_keyword.clone());
    let ext = source
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("mp4");
    let dir = keyword_dir(server_scope, &output_keyword);
    create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let output = format!("output.{}", ext);
    let target = dir.join(&output);
    tokio::fs::copy(&source, &target)
        .await
        .map_err(|e| format!("failed to keep output: {}", e))?;

    let target_s = target.to_string_lossy().to_string();
    let fps = ffprobe_framerate(&target_s).map(|(n, d)| format!("{}/{}", n, d));
    let sample_rate = ffprobe_samplerate(&target_s);
    let meta = KeepMeta {
        keyword: output_keyword,
        parent_keyword,
        kind,
        server_scope: server_scope.to_string(),
        output,
        expires_at: now_secs() + KEEP_TTL_SECS,
        preset: preset.map(preset_label),
        fps,
        sample_rate,
        ready: true,
        failed: false,
        job_id,
    };
    let raw = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    write(meta_path(server_scope, &meta.keyword), raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(meta)
}

pub(crate) async fn reserve_output(
    server_scope: &str,
    kind: KeepKind,
    keep: &KeepRequest,
    preset: Option<&Preset>,
    job_id: u64,
) -> Result<KeepMeta, String> {
    let output_keyword = keep
        .output_keyword
        .as_deref()
        .and_then(sanitize_keyword)
        .ok_or_else(|| "keep output keyword was not prepared".to_string())?;
    let parent_keyword = keep
        .parent_keyword
        .as_deref()
        .and_then(sanitize_keyword)
        .unwrap_or_else(|| output_keyword.clone());
    let dir = keyword_dir(server_scope, &output_keyword);
    create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let meta = KeepMeta {
        keyword: output_keyword,
        parent_keyword,
        kind,
        server_scope: server_scope.to_string(),
        output: "output.mp4".to_string(),
        expires_at: now_secs() + KEEP_TTL_SECS,
        preset: preset.map(preset_label),
        fps: None,
        sample_rate: None,
        ready: false,
        failed: false,
        job_id,
    };
    let raw = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    write(meta_path(server_scope, &meta.keyword), raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(meta)
}

pub(crate) async fn mark_output_failed(
    server_scope: &str,
    keep: &KeepRequest,
) -> Result<(), String> {
    let output_keyword = keep
        .output_keyword
        .as_deref()
        .and_then(sanitize_keyword)
        .ok_or_else(|| "keep output keyword was not prepared".to_string())?;
    let Some(mut meta) = read_meta(server_scope, &output_keyword).await? else {
        return Ok(());
    };
    meta.ready = false;
    meta.failed = true;
    let raw = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    write(meta_path(server_scope, &meta.keyword), raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) async fn resolve_keywords_for_keycode(
    server_scope: &str,
    keywords: &[String],
) -> Result<KeywordResolve, String> {
    cleanup_expired_keeps().await;
    let mut metas = Vec::new();
    let mut waiting = Vec::new();
    for raw in keywords {
        let keyword = sanitize_keyword(raw).ok_or_else(|| format!("invalid keyword `{}`", raw))?;
        let meta = read_meta(server_scope, &keyword)
            .await?
            .ok_or_else(|| format!("unknown keyword `{}`", keyword))?;
        if meta.expires_at <= now_secs() {
            remove_dir_all(keyword_dir(server_scope, &keyword)).await.ok();
            return Err(format!("keyword `{}` expired", keyword));
        }
        if meta.failed {
            return Err(format!("keyword `{}` failed", keyword));
        }
        if !meta.ready || !output_path(&meta).exists() {
            waiting.push(keyword);
        }
        metas.push(meta);
    }
    let kind = metas
        .first()
        .map(|m| m.kind)
        .ok_or_else(|| "at least one keyword is required".to_string())?;
    if metas.iter().any(|m| m.kind != kind) {
        return Err("mixed encode and backup keywords are not supported in one keycode".to_string());
    }
    if !waiting.is_empty() {
        return Ok(KeywordResolve::Waiting(waiting));
    }
    let paths = metas.iter().map(output_path).collect::<Vec<_>>();
    Ok(KeywordResolve::Ready(ResolvedKeywords { kind, paths }))
}

pub async fn resolve_studio_keywords(
    guild_id: u64,
    keywords: &[String],
) -> Result<ResolvedKeywords, String> {
    cleanup_expired_keeps().await;
    if keywords.is_empty() {
        return Err("at least one keyword is required".to_string());
    }
    let server_scope = scope(Some(guild_id));
    let mut metas = Vec::with_capacity(keywords.len());
    let mut seen = std::collections::HashSet::new();
    for raw in keywords {
        let keyword = sanitize_keyword(raw).ok_or_else(|| format!("invalid keyword `{}`", raw))?;
        if !seen.insert(keyword.clone()) {
            return Err(format!("duplicate keyword `{}`", keyword));
        }
        let meta = read_meta(&server_scope, &keyword)
            .await?
            .ok_or_else(|| format!("unknown keyword `{}`", keyword))?;
        if meta.expires_at <= now_secs() {
            remove_dir_all(keyword_dir(&server_scope, &keyword)).await.ok();
            return Err(format!("keyword `{}` expired", keyword));
        }
        if meta.failed {
            return Err(format!("keyword `{}` failed", keyword));
        }
        if !meta.ready || !output_path(&meta).exists() {
            return Err(format!("keyword `{}` is not ready", keyword));
        }
        metas.push(meta);
    }
    let kind = metas[0].kind;
    if metas.iter().any(|meta| meta.kind != kind) {
        return Err("mixed encode and backup keywords are not supported in one Studio".to_string());
    }
    Ok(ResolvedKeywords {
        kind,
        paths: metas.iter().map(output_path).collect(),
    })
}

async fn read_meta(server_scope: &str, keyword: &str) -> Result<Option<KeepMeta>, String> {
    match read_to_string(meta_path(server_scope, keyword)).await {
        Ok(raw) => {
            let meta: KeepMeta = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            Ok(Some(meta))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

async fn allocate_keyword(server_scope: &str) -> Result<String, String> {
    let pool = configured_keyword_pool();
    for base in &pool {
        if read_meta(server_scope, base).await?.is_none() {
            return Ok(base.clone());
        }
    }
    for idx in 1..=999 {
        for base in &pool {
            let candidate = format!("{}{}", base, idx);
            if read_meta(server_scope, &candidate).await?.is_none() {
                return Ok(candidate);
            }
        }
    }
    Err("keyword pool is exhausted".to_string())
}

pub fn sanitize_keyword(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if s.len() > 48 {
        return None;
    }
    if s.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        Some(s)
    } else {
        None
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn preset_label(preset: &Preset) -> String {
    match preset {
        Preset::PseudoLossless(_) => "pseudolossless",
        Preset::Dummy(_) => "dummy",
        Preset::Standard(_) => "standard",
        Preset::Gpu(_) => "gpu",
        Preset::Copy => "copy",
    }
    .to_string()
}
