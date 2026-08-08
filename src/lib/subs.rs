use std::path::PathBuf;

use regex::Regex;

use crate::lib::bin::resolve_runtime_binary;

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
                    "{} was converted to ASS by ffmpeg. Converted scripts carry no styling — the result uses ffmpeg's Default Arial 16 style at 384x288.",
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
    Ok(converted)
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
    }

    #[tokio::test]
    async fn non_utf8_text_is_rejected_before_ffmpeg() {
        let err = ensure_ass("x.srt", &[0xff, 0xfe, 0x00]).await.unwrap_err();
        assert!(err.contains("not valid UTF-8"), "{}", err);
    }
}
