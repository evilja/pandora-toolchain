use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::fs::{create_dir_all, read_to_string, remove_dir_all, write};

use crate::libpnmpeg::probe::{ffprobe_framerate, ffprobe_samplerate};
use crate::pnworker::core::{KeepKind, KeepRequest, Preset};

const KEEP_TTL_SECS: u64 = 2 * 60 * 60;
const KEYWORDS: &[&str] = &[
    "akira", "aqua", "aster", "ciel", "ember", "fable", "glint", "hikari", "iris",
    "jade", "kumo", "lumen", "mika", "nagi", "onyx", "ruri", "sora", "toki",
    "umi", "yuki", "zero", "altair", "bell", "clover", "dawn", "echo", "frost",
    "halo", "ivory", "lyra", "mira", "noa", "opal", "pulse", "quartz", "rei",
];

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
}

pub(crate) struct PreparedKeep {
    pub output_keyword: String,
    pub parent_keyword: String,
    pub parent_preset: Option<String>,
}

pub(crate) struct ResolvedKeywords {
    pub kind: KeepKind,
    pub paths: Vec<PathBuf>,
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
    let mut parent_preset = None;
    let parent_keyword = match request.keyword.as_deref().and_then(sanitize_keyword) {
        Some(keyword) => {
            let meta = read_meta(server_scope, &keyword)
                .await?
                .ok_or_else(|| format!("unknown keyword `{}`", keyword))?;
            if meta.kind != kind {
                return Err(format!("keyword `{}` is not a {} keyword", keyword, kind.label()));
            }
            parent_preset = meta.preset.clone();
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
        parent_preset,
    })
}

pub(crate) async fn store_output(
    server_scope: &str,
    kind: KeepKind,
    keep: &KeepRequest,
    source: PathBuf,
    preset: Option<&Preset>,
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
    };
    let raw = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    write(meta_path(server_scope, &meta.keyword), raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(meta)
}

pub(crate) async fn resolve_keywords(
    server_scope: &str,
    keywords: &[String],
) -> Result<ResolvedKeywords, String> {
    cleanup_expired_keeps().await;
    let mut metas = Vec::new();
    for raw in keywords {
        let keyword = sanitize_keyword(raw).ok_or_else(|| format!("invalid keyword `{}`", raw))?;
        let meta = read_meta(server_scope, &keyword)
            .await?
            .ok_or_else(|| format!("unknown keyword `{}`", keyword))?;
        if meta.expires_at <= now_secs() {
            remove_dir_all(keyword_dir(server_scope, &keyword)).await.ok();
            return Err(format!("keyword `{}` expired", keyword));
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
    let paths = metas.iter().map(output_path).collect::<Vec<_>>();
    Ok(ResolvedKeywords { kind, paths })
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
    for base in KEYWORDS {
        if read_meta(server_scope, base).await?.is_none() {
            return Ok((*base).to_string());
        }
    }
    for idx in 1..=999 {
        for base in KEYWORDS {
            let candidate = format!("{}{}", base, idx);
            if read_meta(server_scope, &candidate).await?.is_none() {
                return Ok(candidate);
            }
        }
    }
    Err("keyword pool is exhausted".to_string())
}

fn sanitize_keyword(raw: &str) -> Option<String> {
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

fn now_secs() -> u64 {
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
    }
    .to_string()
}
