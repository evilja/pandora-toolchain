use crate::lib::mpeg::probe::ffprobe_duration_centiseconds;
use crate::lib::protocol::core::Protocol;
use crate::pnworker::core::Preset;
use crate::pnworker::tools::PNASS_INJECT;
use crate::pnworker::util::{PathValue, ToolResult, run_tool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ServerSettings {
    pub preset: Preset,
    pub watermark: Option<Vec<u8>>,
}

pub struct AppliedServerEffects {
    pub subtitle: PathBuf,
    pub warnings: Vec<String>,
}

pub fn load_server_settings(server_id: Option<u64>) -> ServerSettings {
    let Some(server_id) = server_id else {
        return ServerSettings {
            preset: Preset::Standard(None),
            watermark: None,
        };
    };

    let meta_path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    let contents = std::fs::read_to_string(meta_path).unwrap_or_default();
    let lines = contents.lines().map(str::trim).collect::<Vec<_>>();
    let preset_name = lines.get(11).copied().unwrap_or("standard");
    let concat_group = lines
        .get(12)
        .copied()
        .filter(|value| !value.is_empty() && *value != "-");
    let candidates =
        concat_group.and_then(|group| crate::pnworker::util::IntrosConfig::load().resolve(group));
    let preset = match preset_name.to_ascii_lowercase().as_str() {
        "gpu" => Preset::Gpu(candidates),
        "pseudolossless" | "pseudo_lossless" => Preset::PseudoLossless(candidates),
        "dummy" => Preset::Dummy(candidates),
        _ => Preset::Standard(candidates),
    };
    let watermark_path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("watermark.ass");
    let watermark = std::fs::read(watermark_path)
        .ok()
        .filter(|bytes| !bytes.is_empty());

    ServerSettings { preset, watermark }
}

pub async fn server_effects(
    directory: &Path,
    watermark: Option<&[u8]>,
    pnass_path: &str,
    job_id: u64,
) -> Result<AppliedServerEffects, String> {
    let subtitle = directory.join("contents").join("subtitle.ass");
    let Some(watermark) = watermark else {
        return Ok(AppliedServerEffects {
            subtitle,
            warnings: Vec::new(),
        });
    };
    if watermark.is_empty() {
        return Ok(AppliedServerEffects {
            subtitle,
            warnings: Vec::new(),
        });
    }

    if directory.join("CANCEL").try_exists().unwrap_or(false) {
        return Err("cancelled".to_string());
    }
    let input = directory.join("contents").join("torrent").join("input.mkv");
    let output = directory.join("work").join("subtitle_server_effects.ass");
    let watermark_path = directory.join("contents").join("server_watermark.ass");
    let duration = ffprobe_duration_centiseconds(&input.to_string_lossy())
        .ok_or_else(|| "could not determine downloaded video duration".to_string())?;
    if directory.join("CANCEL").try_exists().unwrap_or(false) {
        return Err("cancelled".to_string());
    }
    tokio::fs::write(&watermark_path, watermark)
        .await
        .map_err(|e| format!("could not write watermark: {}", e))?;

    if pnass_path.trim().is_empty() {
        return Err("PNASS binary path is not configured".to_string());
    }
    let mut warnings = Vec::new();
    let mut proto = Protocol::new(vec![1]);
    let result = run_tool(
        pnass_path,
        PNASS_INJECT,
        &HashMap::from([
            ("INPUT", PathValue::from(subtitle.display().to_string())),
            (
                "INJECT",
                PathValue::from(watermark_path.display().to_string()),
            ),
            ("OUTPUT", PathValue::from(output.display().to_string())),
            ("DURATION", PathValue::from(duration.to_string())),
        ]),
        job_id,
        &mut proto,
        |data| {
            if data.get(0).and_then(|v| v.as_str()) == Some("4") {
                if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                    warnings.push(warning.to_string());
                }
            }
            None
        },
    )
    .await;
    if directory.join("CANCEL").try_exists().unwrap_or(false) {
        return Err("cancelled".to_string());
    }
    if !matches!(result, ToolResult::Success) {
        return Err("server subtitle effects failed".to_string());
    }
    Ok(AppliedServerEffects {
        subtitle: output,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_server_settings_use_standard_without_effects() {
        let settings = load_server_settings(None);
        assert!(matches!(settings.preset, Preset::Standard(None)));
        assert!(settings.watermark.is_none());
    }
}
