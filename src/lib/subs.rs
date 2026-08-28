use std::path::PathBuf;

use regex::Regex;

use crate::lib::bin::resolve_runtime_binary;
use crate::libkagami::core::SubstationAlpha;

// Text subtitle formats ffmpeg can demux, mapped from the uploaded extension to the
// extension the temp file gets before ffmpeg probes it (several demuxers key off it).
// `.sub` is ambiguous — MicroDVD text or a binary VobSub payload — so it is resolved
// by sniffing the bytes, not by this table.
const CONVERTIBLE: &[(&str, &str)] = &[
    ("srt", "srt"),
    ("ssa", "ssa"),
    ("vtt", "vtt"),
    ("webvtt", "vtt"),
    ("smi", "smi"),
    ("sami", "smi"),
    ("mpl2", "mpl2"),
    ("lrc", "lrc"),
    ("jss", "jss"),
    ("stl", "stl"),
    ("pjs", "pjs"),
    ("rt", "rt"),
    ("aqt", "aqt"),
];

// Formats that carry rendered bitmaps instead of text. Turning these into ASS needs
// OCR, which Pandora does not do, so they are rejected with a specific message.
const IMAGE_BASED: &[(&str, &str)] = &[
    ("sup", "PGS"),
    ("pgs", "PGS"),
    ("idx", "VobSub"),
];

pub enum SubtitleInput {
    Ass,
    Convertible(&'static str),
    ImageBased(&'static str),
    Unsupported,
}

#[derive(Debug)]
pub struct ConvertedSubtitle {
    pub bytes: Vec<u8>,
    pub warning: Option<String>,
}

// Decides what an uploaded subtitle is from its extension, falling back to sniffing
// the leading bytes when the name carries no usable extension.
pub fn classify_subtitle(filename: &str, bytes: &[u8]) -> SubtitleInput {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default();

    if ext == "ass" {
        return SubtitleInput::Ass;
    }
    if let Some((_, label)) = IMAGE_BASED.iter().find(|(name, _)| *name == ext) {
        return SubtitleInput::ImageBased(label);
    }
    if ext == "sub" {
        // VobSub .sub files are an MPEG program stream; MicroDVD .sub files are text.
        return if bytes.starts_with(&[0x00, 0x00, 0x01, 0xBA]) {
            SubtitleInput::ImageBased("VobSub")
        } else {
            SubtitleInput::Convertible("sub")
        };
    }
    if let Some((_, demux_ext)) = CONVERTIBLE.iter().find(|(name, _)| *name == ext) {
        return SubtitleInput::Convertible(demux_ext);
    }
    sniff_subtitle(bytes)
}

// True for any file name the upload paths will try to turn into ASS. Image-based names
// are included on purpose so a zip holding one produces the specific OCR rejection
// instead of a generic "no subtitle in the zip" error.
pub fn is_subtitle_name(name: &str) -> bool {
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) => ext.to_lowercase(),
        None => return false,
    };
    ext == "ass"
        || ext == "sub"
        || CONVERTIBLE.iter().any(|(known, _)| *known == ext)
        || IMAGE_BASED.iter().any(|(known, _)| *known == ext)
}

// Classification from content alone, for uploads that arrive without a usable name —
// job attachments lose their filename before they reach the worker.
pub fn sniff_subtitle(bytes: &[u8]) -> SubtitleInput {
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0xBA]) {
        return SubtitleInput::ImageBased("VobSub");
    }
    if bytes.starts_with(b"PG") {
        return SubtitleInput::ImageBased("PGS");
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_lowercase();
    let first_line = head.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_string();
    if head.contains("[v4 styles]") {
        SubtitleInput::Convertible("ssa")
    } else if head.contains("[script info]") || head.contains("[v4+ styles]") || head.contains("[events]") {
        SubtitleInput::Ass
    } else if first_line.starts_with("webvtt") {
        SubtitleInput::Convertible("vtt")
    } else if head.contains("<sami") {
        SubtitleInput::Convertible("smi")
    } else if Regex::new(r"^\{\d+\}\{\d+\}").unwrap().is_match(&first_line) {
        SubtitleInput::Convertible("sub")
    } else if Regex::new(r"^\[\d{2}:\d{2}[.:]\d{2}\]").unwrap().is_match(&first_line) {
        SubtitleInput::Convertible("lrc")
    } else if head.contains("-->") {
        SubtitleInput::Convertible("srt")
    } else {
        SubtitleInput::Unsupported
    }
}

// Normalises any accepted subtitle upload into ASS bytes. ASS input is passed through
// untouched; everything else goes through ffmpeg, which produces an unstyled script,
// hence the warning the callers surface to the user.
pub async fn ensure_ass(filename: &str, bytes: &[u8]) -> Result<ConvertedSubtitle, String> {
    let kind = classify_subtitle(filename, bytes);
    convert_classified(kind, &format!("`{}`", filename), bytes).await
}

// Same as ensure_ass for uploads that reached us without their original name, which is
// the case for job attachments: only the bytes are carried through the queue.
pub async fn ensure_ass_bytes(bytes: &[u8]) -> Result<ConvertedSubtitle, String> {
    convert_classified(sniff_subtitle(bytes), "the attached subtitle", bytes).await
}

async fn convert_classified(kind: SubtitleInput, label: &str, bytes: &[u8]) -> Result<ConvertedSubtitle, String> {
    match kind {
        SubtitleInput::Ass => Ok(ConvertedSubtitle {
            bytes: bytes.to_vec(),
            warning: None,
        }),
        SubtitleInput::ImageBased(format) => Err(format!(
            "{} is {} — image-based subtitles cannot be converted to ASS (they would need OCR).",
            label, format
        )),
        SubtitleInput::Unsupported => Err(format!(
            "unsupported subtitle file type ({}). Use .ass, or a text subtitle ffmpeg can read (.srt, .ssa, .vtt, .sub, .smi, .lrc, .mpl2, .jss, .stl, .pjs, .rt, .aqt).",
            label
        )),
        SubtitleInput::Convertible(demux_ext) => {
            if std::str::from_utf8(bytes).is_err() {
                return Err(format!(
                    "{} is not valid UTF-8. Re-save it as UTF-8 and upload it again.",
                    label
                ));
            }
            let converted = convert_to_ass(bytes, demux_ext).await?;
            Ok(ConvertedSubtitle {
                bytes: converted,
                warning: Some(format!(
                    "{} was converted to ASS by ffmpeg. Converted scripts carry no styling — the result uses ffmpeg's Default Arial style, rescaled onto a 1920x1080 canvas.",
                    label
                )),
            })
        }
    }
}

// Runs the input through ffmpeg's ASS muxer in a scratch directory.
async fn convert_to_ass(bytes: &[u8], demux_ext: &str) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!(
        "pandora_subs_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let result = convert_in_dir(&dir, bytes, demux_ext).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn convert_in_dir(dir: &PathBuf, bytes: &[u8], demux_ext: &str) -> Result<Vec<u8>, String> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| e.to_string())?;
    let input = dir.join(format!("input.{}", demux_ext));
    let output = dir.join("output.ass");
    tokio::fs::write(&input, bytes).await.map_err(|e| e.to_string())?;

    let run = tokio::process::Command::new(resolve_runtime_binary("ffmpeg"))
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&input)
        .arg(&output)
        .output()
        .await
        .map_err(|e| format!("ffmpeg failed to start: {}", e))?;

    if !run.status.success() {
        let err = String::from_utf8_lossy(&run.stderr);
        let detail = err.lines().last().unwrap_or("no ffmpeg output").trim().to_string();
        return Err(format!("subtitle conversion failed: {}", detail));
    }

    let converted = tokio::fs::read(&output)
        .await
        .map_err(|e| format!("subtitle conversion produced no output: {}", e))?;
    if !converted
        .windows(9)
        .any(|window| window == b"Dialogue:")
    {
        return Err("subtitle conversion produced no dialogue lines.".to_string());
    }
    Ok(rescale_converted(&output, converted).await)
}

// The canvas every script here is authored against; the watermarks and TS scripts a converted
// subtitle later gets merged with are all written for it.
const CONVERTED_PLAYRES_X: u16 = 1920;
const CONVERTED_PLAYRES_Y: u16 = 1080;

// What ffmpeg synthesises whenever the source format carries no header of its own. A converted
// .ssa that arrived with a real header keeps it — canvas, positioning and all — and is left alone.
const FFMPEG_DEFAULT_PLAYRES_X: u16 = 384;
const FFMPEG_DEFAULT_PLAYRES_Y: u16 = 288;

// ffmpeg's ASS muxer always writes its own 384x288 canvas with an Arial 16 Default sized for it,
// and nothing downstream is prepared for that. A 1920x1080 watermark cannot be merged into a 4:3
// script — the PlayRes ratios do not match, so the merge is rejected and the job dies with it —
// and on the way to a 16:9 frame libass stretches the text horizontally to fill the mismatch.
// Rescaling onto the 1080p canvas settles both, and renders the same text at the same proportions.
// Only converted output passes through here: an uploaded ASS keeps whatever canvas it was authored
// on.
async fn rescale_converted(path: &PathBuf, converted: Vec<u8>) -> Vec<u8> {
    let mut sub = SubstationAlpha::load(path.clone(), false).await;
    let source_x = sub.script_info.playresx;
    let source_y = sub.script_info.playresy;
    if source_x != FFMPEG_DEFAULT_PLAYRES_X || source_y != FFMPEG_DEFAULT_PLAYRES_Y {
        return converted;
    }
    // Only the header and the style metrics move, so a script that positions anything itself would
    // be left with coordinates pointing at the old canvas. Nothing ffmpeg writes this header for
    // can carry those tags, but declining such a script costs nothing and cannot be wrong.
    if carries_positioning(&converted) {
        return converted;
    }

    let sx = CONVERTED_PLAYRES_X as f32 / source_x as f32;
    let sy = CONVERTED_PLAYRES_Y as f32 / source_y as f32;
    sub.script_info.playresx = CONVERTED_PLAYRES_X;
    sub.script_info.playresy = CONVERTED_PLAYRES_Y;
    if sub.script_info.layout_res_x != 0 {
        sub.script_info.layout_res_x = CONVERTED_PLAYRES_X;
    }
    if sub.script_info.layout_res_y != 0 {
        sub.script_info.layout_res_y = CONVERTED_PLAYRES_Y;
    }
    // Anything measured against the glyphs follows the vertical factor, the way libass sizes text
    // off PlayResY; only the side margins follow the width. The 384x288 Default lands on exactly
    // the canonical 1080p row that way: Arial 60, outline 3.75, margins 50/50/38.
    for style in &mut sub.v4p_styles {
        style.fontsize = scale_metric(style.fontsize, sy);
        style.outline *= sy;
        style.shadow *= sy;
        style.spacing *= sy;
        style.margin_v = scale_metric(style.margin_v, sy);
        style.margin_l = scale_metric(style.margin_l, sx);
        style.margin_r = scale_metric(style.margin_r, sx);
    }
    for event in &mut sub.events {
        event.margin_v = scale_metric(event.margin_v, sy);
        event.margin_l = scale_metric(event.margin_l, sx);
        event.margin_r = scale_metric(event.margin_r, sx);
    }
    sub.stringify().into_bytes()
}

fn scale_metric(value: u16, factor: f32) -> u16 {
    (value as f32 * factor).round().clamp(0.0, u16::MAX as f32) as u16
}

// Every tag whose meaning is tied to the canvas it was written against.
fn carries_positioning(script: &[u8]) -> bool {
    let text = String::from_utf8_lossy(script).to_lowercase();
    ["\\pos(", "\\move(", "\\org(", "\\clip(", "\\iclip(", "\\p1", "\\p2", "\\p3", "\\p4"]
        .iter()
        .any(|tag| text.contains(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_extension_passes_through() {
        assert!(matches!(
            classify_subtitle("TL - Show - E01.ass", b"[Script Info]\n"),
            SubtitleInput::Ass
        ));
    }

    #[test]
    fn known_text_formats_are_convertible() {
        for (name, expected) in [
            ("a.srt", "srt"),
            ("a.SRT", "srt"),
            ("a.vtt", "vtt"),
            ("a.webvtt", "vtt"),
            ("a.ssa", "ssa"),
            ("a.sami", "smi"),
        ] {
            match classify_subtitle(name, b"") {
                SubtitleInput::Convertible(ext) => assert_eq!(ext, expected, "{}", name),
                _ => panic!("{} should be convertible", name),
            }
        }
    }

    #[test]
    fn image_based_formats_are_rejected() {
        assert!(matches!(
            classify_subtitle("a.sup", b""),
            SubtitleInput::ImageBased("PGS")
        ));
        assert!(matches!(
            classify_subtitle("a.idx", b""),
            SubtitleInput::ImageBased("VobSub")
        ));
    }

    #[test]
    fn sub_extension_splits_on_content() {
        assert!(matches!(
            classify_subtitle("a.sub", &[0x00, 0x00, 0x01, 0xBA, 0x44]),
            SubtitleInput::ImageBased("VobSub")
        ));
        assert!(matches!(
            classify_subtitle("a.sub", b"{0}{25}hello\n"),
            SubtitleInput::Convertible("sub")
        ));
    }

    #[test]
    fn extensionless_uploads_are_sniffed() {
        assert!(matches!(
            classify_subtitle("subtitle", b"[Script Info]\nTitle: x\n"),
            SubtitleInput::Ass
        ));
        assert!(matches!(
            classify_subtitle("subtitle", b"WEBVTT\n\n00:01.000 --> 00:02.000\nhi\n"),
            SubtitleInput::Convertible("vtt")
        ));
        assert!(matches!(
            classify_subtitle("subtitle", b"1\n00:00:01,000 --> 00:00:02,000\nhi\n"),
            SubtitleInput::Convertible("srt")
        ));
        assert!(matches!(
            classify_subtitle("subtitle.bin", b"\x00\x01\x02"),
            SubtitleInput::Unsupported
        ));
        // An ASS script missing its [Script Info] header still must not be converted.
        assert!(matches!(
            classify_subtitle("subtitle", b"[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,hi\n"),
            SubtitleInput::Ass
        ));
    }

    #[tokio::test]
    async fn ass_input_is_not_rewritten() {
        let raw = b"[Script Info]\nScriptType: v4.00+\n";
        let out = ensure_ass("x.ass", raw).await.unwrap();
        assert_eq!(out.bytes, raw.to_vec());
        assert!(out.warning.is_none());
    }

    // Skipped when the machine running the tests has no ffmpeg; pndc always has one.
    #[tokio::test]
    async fn srt_becomes_playable_ass() {
        if tokio::process::Command::new(resolve_runtime_binary("ffmpeg"))
            .arg("-version")
            .output()
            .await
            .is_err()
        {
            return;
        }
        let srt = b"1\n00:00:01,000 --> 00:00:03,500\nHello <i>world</i>\n\n";
        let out = ensure_ass("x.srt", srt).await.unwrap();
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(text.contains("[Script Info]"), "{}", text);
        assert!(text.contains("0:00:01.00,0:00:03.50"), "{}", text);
        assert!(text.contains("Hello {\\i1}world{\\i0}"), "{}", text);
        assert!(out.warning.is_some());
        // ffmpeg hands back a 384x288 4:3 canvas, which no watermark can be merged into.
        assert!(text.contains("PlayResX: 1920"), "{}", text);
        assert!(text.contains("PlayResY: 1080"), "{}", text);
        assert!(text.contains("Default,Arial,60,"), "{}", text);
    }

    #[test]
    fn positioned_scripts_keep_the_canvas_their_coordinates_were_written_against() {
        assert!(carries_positioning(
            b"Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\pos(320,240)}sign\n"
        ));
        assert!(carries_positioning(
            b"Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}\n"
        ));
        assert!(!carries_positioning(
            b"Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello {\\i1}world{\\i0}\n"
        ));
    }

    #[tokio::test]
    async fn non_utf8_text_is_rejected_before_ffmpeg() {
        let err = ensure_ass("x.srt", &[0xff, 0xfe, 0x00]).await.unwrap_err();
        assert!(err.contains("not valid UTF-8"), "{}", err);
    }
}
