use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::lib::bin::resolve_runtime_binary;

// One subtitle track of a container. `ordinal` is the position among subtitle
// streams, which is what `-map 0:s:N` selects — not the global stream index,
// which counts video and audio too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleStream {
    pub ordinal: usize,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub forced: bool,
}

#[derive(Debug, Deserialize)]
struct ProbeStreams {
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    tags: Option<ProbeTags>,
    #[serde(default)]
    disposition: Option<ProbeDisposition>,
}

#[derive(Debug, Deserialize)]
struct ProbeTags {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeDisposition {
    #[serde(default)]
    forced: u8,
}

pub fn ffprobe_subtitle_streams(path: &Path) -> Vec<SubtitleStream> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v",
            "error",
            "-select_streams",
            "s",
            "-show_entries",
            "stream=codec_name:stream_tags=language,title:stream_disposition=forced",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok();
    let Some(output) = output.filter(|output| output.status.success()) else {
        return Vec::new();
    };
    parse_subtitle_streams(&output.stdout)
}

pub fn parse_subtitle_streams(stdout: &[u8]) -> Vec<SubtitleStream> {
    let Ok(parsed) = serde_json::from_slice::<ProbeStreams>(stdout) else {
        return Vec::new();
    };
    parsed
        .streams
        .into_iter()
        .enumerate()
        .map(|(ordinal, stream)| SubtitleStream {
            ordinal,
            codec: stream.codec_name.unwrap_or_default(),
            language: stream
                .tags
                .as_ref()
                .and_then(|tags| tags.language.clone())
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty() && value != "und"),
            title: stream
                .tags
                .and_then(|tags| tags.title)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            forced: stream
                .disposition
                .map(|disposition| disposition.forced == 1)
                .unwrap_or(false),
        })
        .collect()
}

// Image-based tracks carry bitmaps, not text: they can be demuxed but nothing
// downstream here can read them without OCR, so they are reported as skipped
// instead of producing a file nobody can edit.
pub fn subtitle_extension(codec: &str) -> Option<&'static str> {
    match codec.trim().to_ascii_lowercase().as_str() {
        "ass" | "ssa" => Some("ass"),
        "subrip" | "srt" | "text" => Some("srt"),
        "webvtt" => Some("vtt"),
        "mov_text" => Some("srt"),
        "microdvd" => Some("sub"),
        _ => None,
    }
}

// mov_text is MP4's own text format, which no standalone container accepts as a
// stream copy, so it is the one codec transcoded on the way out.
fn extraction_codec(codec: &str) -> &'static str {
    match codec.trim().to_ascii_lowercase().as_str() {
        "mov_text" => "srt",
        _ => "copy",
    }
}

// Track filenames have to survive being unzipped next to each other, so the
// ordinal leads and everything taken from the file's own metadata is reduced to
// a conservative slug.
pub fn subtitle_filename(stream: &SubtitleStream, extension: &str) -> String {
    let mut name = format!("{}", stream.ordinal);
    if let Some(language) = stream.language.as_deref().map(slug).filter(|s| !s.is_empty()) {
        name.push('.');
        name.push_str(&language);
    }
    if let Some(title) = stream.title.as_deref().map(slug).filter(|s| !s.is_empty()) {
        name.push('.');
        name.push_str(&title);
    }
    if stream.forced {
        name.push_str(".forced");
    }
    format!("{name}.{extension}")
}

fn slug(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for character in raw.chars().take(48) {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub struct ExtractedSubtitle {
    pub stream: SubtitleStream,
    pub path: PathBuf,
}

pub enum ExtractOutcome {
    Extracted(ExtractedSubtitle),
    Skipped { stream: SubtitleStream, reason: String },
}

pub fn extract_subtitle(
    input: &Path,
    output_dir: &Path,
    stream: &SubtitleStream,
) -> ExtractOutcome {
    let Some(extension) = subtitle_extension(&stream.codec) else {
        return ExtractOutcome::Skipped {
            stream: stream.clone(),
            reason: format!("{} is image-based", stream.codec),
        };
    };
    let path = output_dir.join(subtitle_filename(stream, extension));
    let run = Command::new(resolve_runtime_binary("ffmpeg"))
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-map", &format!("0:s:{}", stream.ordinal)])
        .args(["-c:s", extraction_codec(&stream.codec)])
        .arg(&path)
        .output();
    match run {
        Ok(output) if output.status.success() => {
            // ffmpeg exits 0 on an empty track, and an empty file is worse than
            // an honest skip because it looks like a usable subtitle.
            if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0 {
                let _ = std::fs::remove_file(&path);
                return ExtractOutcome::Skipped {
                    stream: stream.clone(),
                    reason: "track is empty".to_string(),
                };
            }
            ExtractOutcome::Extracted(ExtractedSubtitle {
                stream: stream.clone(),
                path,
            })
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .last()
                .unwrap_or("no ffmpeg output")
                .trim()
                .to_string();
            let _ = std::fs::remove_file(&path);
            ExtractOutcome::Skipped {
                stream: stream.clone(),
                reason: detail,
            }
        }
        Err(error) => ExtractOutcome::Skipped {
            stream: stream.clone(),
            reason: format!("ffmpeg failed to start: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(ordinal: usize, codec: &str, language: Option<&str>, title: Option<&str>) -> SubtitleStream {
        SubtitleStream {
            ordinal,
            codec: codec.to_string(),
            language: language.map(str::to_string),
            title: title.map(str::to_string),
            forced: false,
        }
    }

    #[test]
    fn ordinals_are_positions_among_subtitle_streams_not_global_indexes() {
        let json = br#"{"streams":[
            {"codec_name":"ass","tags":{"language":"eng","title":"Full Subtitles"},"disposition":{"forced":0}},
            {"codec_name":"ass","tags":{"language":"eng","title":"Signs & Songs"},"disposition":{"forced":1}},
            {"codec_name":"hdmv_pgs_subtitle","tags":{"language":"jpn"},"disposition":{"forced":0}}
        ]}"#;
        let streams = parse_subtitle_streams(json);
        assert_eq!(streams.len(), 3);
        assert_eq!(streams[0].ordinal, 0);
        assert_eq!(streams[1].ordinal, 1);
        assert_eq!(streams[2].ordinal, 2);
        assert!(streams[1].forced);
        assert_eq!(streams[1].title.as_deref(), Some("Signs & Songs"));
    }

    #[test]
    fn undefined_and_blank_metadata_is_dropped_rather_than_named() {
        let json = br#"{"streams":[{"codec_name":"subrip","tags":{"language":"und","title":"  "}}]}"#;
        let streams = parse_subtitle_streams(json);
        assert_eq!(streams[0].language, None);
        assert_eq!(streams[0].title, None);
    }

    #[test]
    fn malformed_probe_output_lists_no_tracks_instead_of_failing() {
        assert!(parse_subtitle_streams(b"not json").is_empty());
        assert!(parse_subtitle_streams(b"{}").is_empty());
    }

    #[test]
    fn text_codecs_map_to_their_own_container_and_image_codecs_do_not() {
        assert_eq!(subtitle_extension("ass"), Some("ass"));
        assert_eq!(subtitle_extension("SSA"), Some("ass"));
        assert_eq!(subtitle_extension("subrip"), Some("srt"));
        assert_eq!(subtitle_extension("webvtt"), Some("vtt"));
        assert_eq!(subtitle_extension("hdmv_pgs_subtitle"), None);
        assert_eq!(subtitle_extension("dvd_subtitle"), None);
    }

    // Only MP4's own text format cannot be stream-copied into a sidecar file.
    #[test]
    fn only_mov_text_is_transcoded_on_the_way_out() {
        assert_eq!(extraction_codec("ass"), "copy");
        assert_eq!(extraction_codec("subrip"), "copy");
        assert_eq!(extraction_codec("mov_text"), "srt");
    }

    #[test]
    fn filenames_lead_with_the_ordinal_and_slug_everything_from_metadata() {
        assert_eq!(
            subtitle_filename(&stream(0, "ass", Some("eng"), Some("Full Subtitles")), "ass"),
            "0.eng.full-subtitles.ass"
        );
        assert_eq!(
            subtitle_filename(&stream(2, "subrip", None, None), "srt"),
            "2.srt"
        );
        let mut forced = stream(1, "ass", Some("eng"), Some("Signs & Songs"));
        forced.forced = true;
        assert_eq!(subtitle_filename(&forced, "ass"), "1.eng.signs-songs.forced.ass");
    }

    // A title is attacker-controlled metadata inside someone else's file, so it
    // must not be able to escape the directory it is written into.
    #[test]
    fn hostile_track_titles_cannot_escape_the_output_directory() {
        let name = subtitle_filename(&stream(0, "ass", Some("../../etc"), Some("../../../passwd")), "ass");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
        assert!(name.starts_with("0."));
    }
}
