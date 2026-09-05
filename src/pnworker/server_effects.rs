use crate::lib::protocol::core::Protocol;
use crate::lib::mpeg::logo::{
    LOGO_EXTENSIONS, LogoConfig, ServerLogo, logo_extension_from_filename,
};
use crate::pnworker::core::{Concat, Preset};
use crate::pnworker::tools::PNASS_INJECT;
use crate::pnworker::util::{ConcatConfig, ConcatKind, PathValue, ToolResult, run_tool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ServerSettings {
    pub preset: Preset,
    pub watermark: Option<Vec<u8>>,
    // The image watermark, beside the ASS one. Both are optional and independent: an ASS watermark
    // is text libass draws into the subtitle stream, a logo is a picture the encoder composites over
    // every frame, and a server may configure either, both, or neither.
    pub logo: Option<ServerLogo>,
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
            logo: None,
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

    ServerSettings {
        preset,
        watermark,
        logo: load_server_logo(server_id),
    }
}

// The logo config written into a job's own `contents/`, beside the picture it names. Both the
// download worker's speculative prefix and the encode that adopts it read this, so the two cannot
// disagree about which picture goes where; reading the server's live config instead would let a
// logo changed mid-encode apply to half an episode.
pub const JOB_LOGO_CONFIG_FILE: &str = "server_logo.toml";

pub async fn write_job_logo(directory: &Path, logo: &ServerLogo) -> Result<(), String> {
    let contents = directory.join("contents");
    let placement = logo.placement.sanitized();
    let config = LogoConfig {
        file: logo.file_name(),
        placement,
    };
    let body = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // Picture first: a config naming a file that has not landed is what reads back as a broken
    // logo, and the reader treats a missing picture as no logo at all.
    tokio::fs::write(contents.join(logo.file_name()), &logo.bytes)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(contents.join(JOB_LOGO_CONFIG_FILE), body)
        .await
        .map_err(|e| e.to_string())
}

// The logo a job was set up with, or None when it has none. Never an error: a job with an unusable
// logo still has a release to produce, and failing here would strand it over a watermark.
pub fn load_job_logo(directory: &Path) -> Option<ServerLogo> {
    let contents = directory.join("contents");
    let config: LogoConfig =
        toml::from_str(&std::fs::read_to_string(contents.join(JOB_LOGO_CONFIG_FILE)).ok()?).ok()?;
    let extension = logo_extension_from_filename(&config.file)?;
    if config.file.contains('/') || config.file.contains('\\') || config.file.starts_with('.') {
        return None;
    }
    let bytes = std::fs::read(contents.join(&config.file))
        .ok()
        .filter(|bytes| !bytes.is_empty())?;
    Some(ServerLogo {
        bytes,
        extension: extension.to_string(),
        placement: config.placement.sanitized(),
    })
}

// The path a job's logo picture sits at, for handing to pnmpeg. Separate from `load_job_logo`
// because the encoder needs the file, not its bytes.
pub fn job_logo_path(directory: &Path, logo: &ServerLogo) -> PathBuf {
    directory.join("contents").join(logo.file_name())
}

pub fn server_config_dir(server_id: u64) -> PathBuf {
    PathBuf::from("DB").join("config").join(server_id.to_string())
}

pub const LOGO_CONFIG_FILE: &str = "logo.toml";

// The image watermark this server configured, or None when it has none — which is also the answer
// when the config names a file that is not there. A logo that cannot be read must not fail the job:
// the encode still has a release to produce, and the alternative is every job on that server
// declining until an operator notices.
pub fn load_server_logo(server_id: u64) -> Option<ServerLogo> {
    let directory = server_config_dir(server_id);
    let contents = std::fs::read_to_string(directory.join(LOGO_CONFIG_FILE)).ok()?;
    let config: LogoConfig = match toml::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Warning: could not parse {}: {}", directory.join(LOGO_CONFIG_FILE).display(), e);
            return None;
        }
    };
    // The stored name addresses a file beside the config and nothing else. It is written by the bot,
    // but it is also a file on disk an operator may edit, and a path that climbed out of the
    // server's own directory would be read and then shipped inside a release.
    let extension = logo_extension_from_filename(&config.file)?;
    if config.file.contains('/') || config.file.contains('\\') || config.file.starts_with('.') {
        eprintln!("Warning: logo file `{}` is not a plain name", config.file);
        return None;
    }
    let bytes = std::fs::read(directory.join(&config.file))
        .ok()
        .filter(|bytes| !bytes.is_empty())?;
    Some(ServerLogo {
        bytes,
        extension: extension.to_string(),
        placement: config.placement.sanitized(),
    })
}

// Writes the image and the config that points at it, replacing whatever was there. The image is
// written first: a config naming a file that has not landed yet is the one ordering that reads back
// as a broken logo, and the reader treats a missing file as no logo at all.
pub fn save_server_logo(server_id: u64, logo: &ServerLogo) -> Result<(), String> {
    let directory = server_config_dir(server_id);
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let file = logo.file_name();
    std::fs::write(directory.join(&file), &logo.bytes).map_err(|e| e.to_string())?;
    let config = LogoConfig {
        file: file.clone(),
        placement: logo.placement.sanitized(),
    };
    let body = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(directory.join(LOGO_CONFIG_FILE), body).map_err(|e| e.to_string())?;
    // A server that switches from a PNG to a WebP would otherwise leave the old picture on disk,
    // where nothing reads it and every backup carries it forever.
    for extension in LOGO_EXTENSIONS {
        let stale = directory.join(format!("server_logo.{extension}"));
        if stale.file_name().map(|name| name.to_string_lossy() != file).unwrap_or(false) {
            std::fs::remove_file(stale).ok();
        }
    }
    Ok(())
}

// Removes the config first, so a failure part-way through leaves a server with no logo rather than
// with a config pointing at a file that has been deleted.
pub fn clear_server_logo(server_id: u64) -> Result<bool, String> {
    let directory = server_config_dir(server_id);
    let config = directory.join(LOGO_CONFIG_FILE);
    let had_logo = config.exists();
    match std::fs::remove_file(&config) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    for extension in LOGO_EXTENSIONS {
        std::fs::remove_file(directory.join(format!("server_logo.{extension}"))).ok();
    }
    Ok(had_logo)
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

    // The download worker's speculative prefix and the encode that adopts it both read the job's own
    // copy, so what one wrote the other has to read back exactly — a placement that did not survive
    // the round trip would put the logo in different corners in one release.
    #[tokio::test]
    async fn a_jobs_logo_reads_back_as_it_was_written() {
        let directory = std::env::temp_dir().join(format!("pnlogo-{}", std::process::id()));
        std::fs::create_dir_all(directory.join("contents")).unwrap();
        let logo = ServerLogo {
            bytes: b"not really a png".to_vec(),
            extension: "png".to_string(),
            placement: crate::lib::mpeg::logo::LogoPlacement {
                position: crate::lib::mpeg::logo::LogoPosition::BottomCenter,
                margin: 12,
                opacity: 65,
                width_percent: Some(9),
            },
        };
        write_job_logo(&directory, &logo).await.unwrap();
        assert_eq!(load_job_logo(&directory), Some(logo.clone()));

        // The picture is what makes it a logo: a config left behind without one is no logo at all,
        // not a job that fails.
        std::fs::remove_file(directory.join("contents").join(logo.file_name())).unwrap();
        assert_eq!(load_job_logo(&directory), None);

        // And a job that never had one reads back as none rather than as an error.
        std::fs::remove_file(directory.join("contents").join(JOB_LOGO_CONFIG_FILE)).unwrap();
        assert_eq!(load_job_logo(&directory), None);
        std::fs::remove_dir_all(&directory).ok();
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
