use crate::lib::mpeg::core::FfmpegParams;
use std::borrow::Cow;

pub const CPU_PSEUDOLOSSLESS_X264_PARAMS: &str =
    "me=umh:subme=8:merange=24:trellis=2:psy-rd=1:aq-strength=0.85:aq-mode=3";
pub const CPU_SANE_X264_PARAMS: &str = "aq-strength=0.8:aq-mode=3";


pub const CPU_DUMMY: [FfmpegParams; 16] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(25),
    FfmpegParams::Preset(Cow::Borrowed("veryfast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
pub const CPU_PSEUDOLOSSLESS: [FfmpegParams; 17] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::X264Params(Cow::Borrowed(CPU_PSEUDOLOSSLESS_X264_PARAMS)),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
pub const CPU_SANE_DEFAULTS: [FfmpegParams; 17] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::X264Params(Cow::Borrowed(CPU_SANE_X264_PARAMS)),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
// The two downscaling presets. Everything but the frame size is the standard CPU preset — same
// CRF, same x264 tuning — so a 720p release differs from a source-resolution one only in how many
// pixels it carries. `min(720,ih)` is what keeps the preset from upscaling a source that is
// already smaller, and `-2` keeps the width even for yuv420p; the scale runs before `ass` so
// libass renders the subtitles at the output size instead of having them resampled with the video.
pub const CPU_720P_FILTER: &str =
    "scale=-2:'min(720,ih)':flags=lanczos,ass=INPUTFILEASS,format=yuv420p";
pub const CPU_480P_FILTER: &str =
    "scale=-2:'min(480,ih)':flags=lanczos,ass=INPUTFILEASS,format=yuv420p";

pub const CPU_720P: [FfmpegParams; 17] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed(CPU_720P_FILTER)),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::X264Params(Cow::Borrowed(CPU_SANE_X264_PARAMS)),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
pub const CPU_480P: [FfmpegParams; 17] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed(CPU_480P_FILTER)),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::X264Params(Cow::Borrowed(CPU_SANE_X264_PARAMS)),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
// Slow quality-first CPU preset: x264 veryslow at CRF 18 with no -x264-params
// tuning at all, so AQ and motion search stay on libx264's own defaults.
pub const CPU_VERYSLOW: [FfmpegParams; 16] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(18),
    FfmpegParams::Preset(Cow::Borrowed("veryslow")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
pub const GPU_SANE_DEFAULTS: [FfmpegParams; 18] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("h264_amf")),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v:0")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::QpI(Cow::Borrowed("15")),
    FfmpegParams::QpP(Cow::Borrowed("15")),
    FfmpegParams::Rc(Cow::Borrowed("cqp")),
    FfmpegParams::R(Cow::Borrowed("23.976")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
];
pub const CONCAT: [FfmpegParams; 10] =
[
    FfmpegParams::Format(Cow::Borrowed("concat")),
    FfmpegParams::Safe(Cow::Borrowed("0")),
    FfmpegParams::Input(Cow::Borrowed("CONCATFILEV")),
    FfmpegParams::Cv(Cow::Borrowed("copy")),
    FfmpegParams::Ca(Cow::Borrowed("copy")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV"))
];

pub const CONCAT_LEGACY: [FfmpegParams; 17] =
[
    FfmpegParams::Input(Cow::Borrowed("CONCATFILEV")),
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::ComplexFilter(Cow::Borrowed("[0:v][0:a][1:v][1:a]concat=n=2:v=1:a=1[v][a]")),
    FfmpegParams::Map(Cow::Borrowed("[v]")),
    FfmpegParams::Map(Cow::Borrowed("[a]")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::R(Cow::Borrowed("FPSV")),        // ← added
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::NoStats,
    FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
    FfmpegParams::Overwrite,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV"))
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::mpeg::core::Decode;

    fn rendered(params: &[FfmpegParams]) -> Vec<Vec<String>> {
        params.iter().map(|param| param.decode()).collect()
    }

    // The downscaling presets are the standard preset plus a frame-height cap: if they ever drift
    // on CRF, x264 params or anything else, a 720p release stops being comparable to a source-
    // resolution one encoded from the same settings.
    #[test]
    fn scaled_presets_differ_from_standard_only_in_their_filter() {
        let standard = rendered(&CPU_SANE_DEFAULTS);
        for (scaled, filter) in [(&CPU_720P, CPU_720P_FILTER), (&CPU_480P, CPU_480P_FILTER)] {
            let scaled = rendered(scaled);
            assert_eq!(scaled.len(), standard.len());
            for (scaled, standard) in scaled.iter().zip(standard.iter()) {
                if scaled[0] == "-vf" {
                    assert_eq!(scaled[1], filter);
                    assert_ne!(scaled[1], standard[1]);
                } else {
                    assert_eq!(scaled, standard);
                }
            }
        }
    }

    // `min(...)` is what keeps a source that is already smaller from being upscaled, and the scale
    // has to come before `ass` for libass to render subtitles at the output size.
    #[test]
    fn scaled_filters_cap_rather_than_force_their_height() {
        for (filter, height) in [(CPU_720P_FILTER, 720), (CPU_480P_FILTER, 480)] {
            assert!(filter.starts_with(&format!("scale=-2:'min({height},ih)'")), "{filter}");
            assert!(filter.ends_with(",ass=INPUTFILEASS,format=yuv420p"), "{filter}");
        }
    }
}
