use crate::lib::protocol::core::Protocol;
use crate::pnworker::core::{Concat, Preset};
use crate::pnworker::tools::PNASS_INJECT;
use crate::pnworker::util::{ConcatConfig, ConcatKind, PathValue, ToolResult, run_tool};
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
pub fn preset_from_name(name: &str, concat: Concat) -> Option<Preset> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "standard" => Preset::Standard(concat),
        "gpu" => Preset::Gpu(concat),
        "av1" => Preset::Av1(concat),
        "pseudolossless" | "pseudo_lossless" => Preset::PseudoLossless(concat),
        "dummy" => Preset::Dummy(concat),
        "veryslow" | "very_slow" => Preset::VerySlow(concat),
        "720p" => Preset::Hd720(concat),
        "480p" => Preset::Sd480(concat),
        // Not a name this binary knows, but perhaps one an operator wrote. A preset file has always
        // been able to *replace* a built-in; this is what lets one exist on its own, selected by
        // the name of the file it lives in.
        other => {
            if crate::lib::mpeg::preset::file_preset_exists(other) {
                Preset::Named(other.to_string(), concat)
            } else {
                return None;
            }
        }
    })
}

// A meta line naming a concat group, resolved to the folder that group currently points at. `-` is
// how every other text field in `meta.pandora` spells "cleared", and a group that has since been
// removed from the registry resolves to nothing rather than to a folder that is not there.
fn resolve_concat_line(lines: &[&str], index: usize, kind: ConcatKind) -> Option<String> {
    let group = lines
        .get(index)
        .copied()
        .filter(|value| !value.is_empty() && *value != "-")?;
    ConcatConfig::load_kind(kind).resolve(group)
}

pub fn load_server_settings(server_id: Option<u64>) -> ServerSettings {
    let Some(server_id) = server_id else {
        return ServerSettings {
            preset: Preset::Standard(Concat::NONE),
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
    // Line 12 is the intro group and line 19 the outro group. 19 is past the end of every file
    // written before outros existed, which `get` reports as no group rather than as an error.
    let concat = Concat::new(
        resolve_concat_line(&lines, 12, ConcatKind::Intro),
        resolve_concat_line(&lines, 19, ConcatKind::Outro),
    );
    let preset = preset_from_name(preset_name, concat.clone())
        .unwrap_or_else(|| Preset::Standard(concat));
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
            preset_from_name("720p", Concat::NONE),
            Some(Preset::Hd720(_))
        ));
        assert!(matches!(
            preset_from_name("very_slow", Concat::NONE),
            Some(Preset::VerySlow(_))
        ));
        assert!(matches!(preset_from_name("AV1", Concat::NONE), Some(Preset::Av1(_))));
        assert!(preset_from_name("1440p", Concat::NONE).is_none());
        assert!(preset_from_name("", Concat::NONE).is_none());
    }

    // Both folders have to survive the name lookup. An outro that was dropped on the way through
    // is a release that encodes correctly and ships without its ending.
    #[test]
    fn a_named_preset_keeps_both_concat_folders() {
        let concat = Concat::new(
            Some("DB/concat/1/op".to_string()),
            Some("DB/concat-outro/1/ed".to_string()),
        );
        let preset = preset_from_name(" 480P ", concat.clone()).unwrap();
        assert!(matches!(preset, Preset::Sd480(_)));
        assert_eq!(preset.concat(), &concat);
    }

    // A preset that exists only as a file is selectable by the name of that file. Nothing is
    // compiled in for it, so the name table has to reach the disk to answer at all — and has to
    // keep saying no for a name with no file behind it, or a typo becomes a job that resolves to
    // nothing much later on.
    #[test]
    fn a_preset_file_is_selectable_under_its_own_name() {
        let root = std::path::Path::new(crate::lib::env::standard::PRESETS_DIR);
        let name = format!("pntest-{}", std::process::id());
        let path = root.join(format!("{name}.toml"));
        assert!(
            preset_from_name(&name, Concat::NONE).is_none(),
            "a name with no file must not resolve"
        );

        std::fs::create_dir_all(root).unwrap();
        std::fs::write(&path, "[video]\ncodec = \"libx265\"\n").unwrap();
        let resolved = preset_from_name(&name, Concat::intro_only(Some("intro".to_string())));
        std::fs::remove_file(&path).ok();

        match resolved {
            Some(Preset::Named(resolved_name, concat)) => {
                assert_eq!(resolved_name, name);
                // The intro group belongs to the server, not to the preset, and travels either way.
                assert_eq!(concat.intro.as_deref(), Some("intro"));
            }
            other => panic!("a preset file did not resolve to a named preset: {other:?}"),
        }
    }

    // `meta.pandora` is read by index, and line 19 is past the end of every file written before
    // outros existed. Reading it has to mean "no outro" rather than an error, and the cleared
    // spellings every other text field uses have to mean the same here.
    #[test]
    fn a_meta_line_without_a_group_resolves_to_no_folder() {
        let short = ["EN", "", "", ""];
        assert_eq!(resolve_concat_line(&short, 19, ConcatKind::Outro), None);

        let mut lines = vec![""; 20];
        lines[19] = "-";
        assert_eq!(resolve_concat_line(&lines, 19, ConcatKind::Outro), None);
        lines[19] = "";
        assert_eq!(resolve_concat_line(&lines, 19, ConcatKind::Outro), None);
        // A group name that is not in the registry is a group that was removed, not a folder.
        lines[19] = "pntest-no-such-outro-group";
        assert_eq!(resolve_concat_line(&lines, 19, ConcatKind::Outro), None);
    }

    #[test]
    fn missing_server_settings_use_standard_without_effects() {
        let settings = load_server_settings(None);
        assert!(matches!(settings.preset, Preset::Standard(_)));
        assert!(settings.preset.concat().is_empty());
        assert!(settings.watermark.is_none());
    }

    #[test]
    fn all_watermarks_can_span_any_ass_representable_video() {
        assert_eq!(MAX_ASS_CENTISECONDS, 92_159_999);
    }
}
