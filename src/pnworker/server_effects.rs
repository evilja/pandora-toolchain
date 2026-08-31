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

// `[all]` only needs to outlive the video: rendering an event through ASS's maximum timestamp is
// frame-equivalent to ending it at the probed duration, but unlike a duration probe it can be done
// before a streaming download finishes. The overlapped x264 planner and the eventual encoder can
// therefore consume the exact same generated subtitle file.
const MAX_ASS_CENTISECONDS: u64 = 255 * 360_000 + 59 * 6_000 + 59 * 100 + 99;

// Every name a preset can be written as, whether it arrives on line 11 of `meta.pandora` or in an
// API payload's `preset`. `720p` and `480p` are the standard preset with a frame-height cap;
// nothing else here changes the frame size. An unrecognised name is the caller's to reject or
// default — the config reader defaults it, the API rejects it.
pub fn preset_from_name(name: &str, candidates: Option<String>) -> Option<Preset> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "standard" => Preset::Standard(candidates),
        "gpu" => Preset::Gpu(candidates),
        "av1" => Preset::Av1(candidates),
        "pseudolossless" | "pseudo_lossless" => Preset::PseudoLossless(candidates),
        "dummy" => Preset::Dummy(candidates),
        "veryslow" | "very_slow" => Preset::VerySlow(candidates),
        "720p" => Preset::Hd720(candidates),
        "480p" => Preset::Sd480(candidates),
        _ => return None,
    })
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
    let preset = preset_from_name(preset_name, candidates.clone())
        .unwrap_or_else(|| Preset::Standard(candidates));
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
    let output = directory.join("work").join("subtitle_server_effects.ass");
    let watermark_path = directory.join("contents").join("server_watermark.ass");
    tokio::fs::write(&watermark_path, watermark)
        .await
        .map_err(|e| format!("could not write watermark: {}", e))?;

    if pnass_path.trim().is_empty() {
        return Err("PNASS binary path is not configured".to_string());
    }
    let mut warnings = Vec::new();
    let mut failure: Option<String> = None;
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
            (
                "DURATION",
                PathValue::from(MAX_ASS_CENTISECONDS.to_string()),
            ),
            (
                "LOGFILE",
                PathValue::from(
                    directory
                        .join("log")
                        .join(format!("PNass_Inject{}.log", job_id))
                        .display()
                        .to_string(),
                ),
            ),
        ]),
        job_id,
        &mut proto,
        |data| {
            match data.get(0).and_then(|v| v.as_str()) {
                Some("4") => {
                    if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                        warnings.push(warning.to_string());
                    }
                }
                Some("2") => failure = data.get(1).and_then(|v| v.as_str()).map(str::to_string),
                _ => {}
            }
            None
        },
    )
    .await;
    if directory.join("CANCEL").try_exists().unwrap_or(false) {
        return Err("cancelled".to_string());
    }
    if !matches!(result, ToolResult::Success) {
        // pnass names what it choked on before it exits; reporting the generic failure instead left
        // the job saying only that effects failed, which is what the operator already knew.
        return Err(failure.unwrap_or_else(|| {
            format!(
                "pnass exited without applying server effects (see log/PNass_Inject{}.log)",
                job_id
            )
        }));
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
    fn preset_names_map_to_their_presets() {
        assert!(matches!(
            preset_from_name("720p", None),
            Some(Preset::Hd720(None))
        ));
        assert!(matches!(
            preset_from_name(" 480P ", Some("intro".to_string())),
            Some(Preset::Sd480(Some(_)))
        ));
        assert!(matches!(
            preset_from_name("very_slow", None),
            Some(Preset::VerySlow(None))
        ));
        assert!(matches!(preset_from_name("AV1", None), Some(Preset::Av1(None))));
        assert!(preset_from_name("1440p", None).is_none());
        assert!(preset_from_name("", None).is_none());
    }

    #[test]
    fn missing_server_settings_use_standard_without_effects() {
        let settings = load_server_settings(None);
        assert!(matches!(settings.preset, Preset::Standard(None)));
        assert!(settings.watermark.is_none());
    }

    #[test]
    fn all_watermarks_can_span_any_ass_representable_video() {
        assert_eq!(MAX_ASS_CENTISECONDS, 92_159_999);
    }
}
