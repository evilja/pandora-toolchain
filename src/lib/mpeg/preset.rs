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
fn gpu_av1() -> &'static [FfmpegParams] {
    static PARAMS: std::sync::OnceLock<Vec<FfmpegParams>> = std::sync::OnceLock::new();
    PARAMS.get_or_init(|| vec![
        FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
        FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
        FfmpegParams::Cv(Cow::Borrowed("av1_nvenc")),
        FfmpegParams::Map(Cow::Borrowed("0:v:0")),
        FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
        FfmpegParams::Preset(Cow::Borrowed("p6")),
        FfmpegParams::Rc(Cow::Borrowed("vbr")),
        FfmpegParams::Tune(Cow::Borrowed("hq")),
        FfmpegParams::Ca(Cow::Borrowed("aac")),
        FfmpegParams::Ba(Cow::Borrowed("192k")),
        FfmpegParams::Passthrough(vec![
            "-cq".to_string(), "28".to_string(), "-b:v".to_string(), "0".to_string(),
            "-rc-lookahead".to_string(), "32".to_string(),
            "-bf".to_string(), "3".to_string(),
            "-b_ref_mode".to_string(), "middle".to_string(),
            "-spatial-aq".to_string(), "1".to_string(),
            "-aq-strength".to_string(), "8".to_string(),
            "-temporal-aq".to_string(), "1".to_string(),
            "-multipass".to_string(), "fullres".to_string(),
        ]),
        FfmpegParams::Movflags,
        FfmpegParams::NoStats,
        FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
        FfmpegParams::Overwrite,
        FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
    ])
}
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

// ---------------------------------------------------------------------------------------------
// Presets as files
//
// Everything above is the built-in table, and it stays: it is the fallback, the seed for a file an
// operator wants to edit, and the thing the tests below pin the file format against. What changed
// is that it is no longer the only source. A preset may also be a TOML file under `PRESETS_DIR`,
// which is what makes a new quality tier a config change on one machine rather than a release.
//
// Two things follow from that being a *file* rather than a compiled table:
//
// - It carries `hardware`. A cluster has machines with an encoder ASIC and machines without, and
//   the scheduler cannot know which preset needs which unless the preset says so. See LINK.md.
// - The parsing is one function with one output type, so the day presets become scriptable there
//   is a single place that learns to run a script and every caller keeps working.
//
// The field set is deliberately the union of what the built-in presets use rather than everything
// ffmpeg accepts; `extra_args` is the escape hatch for the rest, and it is appended verbatim.

use crate::lib::env::standard::PRESETS_DIR;
use serde::{Deserialize, Serialize};

// What a preset needs to run on. This is the whole of the CPU/GPU distinction as far as scheduling
// is concerned: a node advertises what it is, a preset declares what it needs, and a job only
// crosses the link when the two agree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetHardware {
    #[default]
    Cpu,
    Gpu,
}

impl PresetHardware {
    pub fn label(self) -> &'static str {
        match self {
            PresetHardware::Cpu => "cpu",
            PresetHardware::Gpu => "gpu",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(PresetHardware::Cpu),
            "gpu" => Some(PresetHardware::Gpu),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetVideo {
    pub codec: Option<String>,
    // The `-vf` chain. `INPUTFILEASS` is substituted by pnmpeg exactly as it is in the built-ins,
    // so a filter written here keeps the same two placeholders and the same ordering rules — the
    // scale has to precede `ass` or libass renders subtitles at the wrong size.
    pub filter: Option<String>,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub x264_params: Option<String>,
    pub crf: Option<u8>,
    pub preset: Option<String>,
    pub qp: Option<String>,
    pub qp_i: Option<String>,
    pub qp_p: Option<String>,
    pub rc: Option<String>,
    pub framerate: Option<String>,
    pub tune: Option<String>,
    pub quality: Option<String>,
    pub bufsize: Option<String>,
    pub maxrate: Option<String>,
    pub keyframe: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetAudio {
    pub codec: Option<String>,
    pub bitrate: Option<String>,
    pub rate: Option<String>,
    pub channels: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetFile {
    #[serde(default)]
    pub hardware: PresetHardware,
    // Whether to start this encode while the source is still downloading. Left out, it is derived
    // the way it always was: the CPU presets that do not scale. Declared, it wins — the mechanism
    // is one ffmpeg process reading a growing file, so it has nothing to do with which encoder the
    // preset names, and a GPU node is exactly where the overlap is cheapest to have.
    #[serde(default)]
    pub aot: Option<bool>,
    // Whether to split the episode across parallel encoders. Left out, it is derived from how slow
    // the x264 preset is, which is the rule the built-in tables have always followed. Declared, it
    // wins — a preset tuned by hand can be slow enough to be worth chunking at an x264 preset no
    // list would have named, and the operator who wrote the settings is the one who knows.
    #[serde(default)]
    pub chunked: Option<bool>,
    // Whether this preset's encodes are background work: they run only while no other encode does,
    // and stop the moment one is ordered. Off unless declared, because it is a scheduling promise
    // rather than a quality setting — a preset that quietly took hours longer than it asked for
    // would be a worse default than one that simply encodes.
    #[serde(default)]
    pub idle: Option<bool>,
    #[serde(default)]
    pub video: PresetVideo,
    #[serde(default)]
    pub audio: PresetAudio,
    // Appended after the audio options and before the muxer flags, which is the only position that
    // cannot collide with something the fields above already wrote.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

// A preset, however it was defined. `params` is ready to hand to the same substitution pnmpeg runs
// over the built-in tables, so nothing downstream can tell the two apart.
#[derive(Clone, Debug)]
pub struct ResolvedPreset {
    pub name: String,
    pub hardware: PresetHardware,
    pub params: Vec<FfmpegParams>,
    // The preset's own answers on encoding ahead and on chunking, or None to derive them. A
    // built-in never declares either.
    pub aot: Option<bool>,
    pub chunked: Option<bool>,
    // Whether the encode runs as background work. A built-in never declares it.
    pub idle: Option<bool>,
    // True when this came off disk, so a log can say which of the two an encode used. An operator
    // who edits a preset and sees no change needs to know the file was never read.
    pub from_file: bool,
}

// What a preset asks libx264 for, read back out of its rendered parameters.
//
// The ahead-of-time encoder is not ffmpeg: it drives libx264 directly through `pnx264`, so it needs
// the same settings expressed as a struct rather than as command-line arguments. Deriving them from
// the preset's own parameters is what keeps the two halves of an encode identical — the speculative
// prefix and the foreground run must agree exactly, or a single output file carries two different
// quality settings across the handoff, and nothing downstream would ever notice.
//
// A second hardcoded copy of the same numbers is the obvious alternative and is what this replaces.
// It agreed with the built-in tables and could not possibly agree with a preset file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct X264Settings {
    pub codec: String,
    pub crf: Option<u8>,
    pub preset: Option<String>,
    pub tune: Option<String>,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub x264_params: Option<String>,
}

// The x264 presets slow enough that splitting an episode into chunks wins back more time than the
// coordination costs. Measured for `veryslow`; `placebo` is slower still and cannot lose.
const CHUNKED_X264_PRESETS: [&str; 2] = ["veryslow", "placebo"];

impl ResolvedPreset {
    pub fn video_codec(&self) -> Option<String> {
        self.params.iter().find_map(|param| match param {
            FfmpegParams::Cv(value) => Some(value.to_string()),
            _ => None,
        })
    }

    pub fn output_video_codec(&self) -> Option<String> {
        let encoder = self.video_codec()?;
        Some(if encoder == "libx264" || encoder.starts_with("h264_") {
            "h264".to_string()
        } else if encoder == "libx265" || encoder.starts_with("hevc_") {
            "hevc".to_string()
        } else if encoder == "libsvtav1"
            || encoder == "libaom-av1"
            || encoder.starts_with("av1_")
        {
            "av1".to_string()
        } else {
            encoder
        })
    }

    pub fn x264_settings(&self) -> X264Settings {
        let mut settings = X264Settings::default();
        for param in &self.params {
            match param {
                FfmpegParams::Cv(value) => settings.codec = value.to_string(),
                FfmpegParams::Crf(value) => settings.crf = Some(*value),
                FfmpegParams::Preset(value) => settings.preset = Some(value.to_string()),
                FfmpegParams::Tune(value) => settings.tune = Some(value.to_string()),
                FfmpegParams::Profile(value) => settings.profile = Some(value.to_string()),
                FfmpegParams::Level(value) => settings.level = Some(value.to_string()),
                FfmpegParams::X264Params(value) => settings.x264_params = Some(value.to_string()),
                _ => {}
            }
        }
        settings
    }

    // The frame height the preset caps its output at, read out of its own filter chain rather than
    // configured beside it — a preset that scaled to one height and declared another would name its
    // release wrong, and the filter is the half that actually decides the pixels.
    pub fn scale_height(&self) -> Option<u32> {
        let filter = self.params.iter().find_map(|param| match param {
            FfmpegParams::BasicFilter(value) => Some(value.to_string()),
            _ => None,
        })?;
        let captures = regex::Regex::new(r"min\((\d+),\s*ih\)")
            .unwrap()
            .captures(&filter)?;
        captures.get(1)?.as_str().parse::<u32>().ok()
    }

    pub fn encodes_with_x264(&self) -> bool {
        self.x264_settings().codec == "libx264"
    }

    // The `-vf` chain this preset applies, still carrying its `INPUTFILEASS` placeholder. Both
    // halves of an ahead-of-time encode take the filter from here, so a preset that scales or
    // denoises does it once and does it the same way whichever half ran.
    pub fn video_filter(&self) -> Option<String> {
        self.params.iter().find_map(|param| match param {
            FfmpegParams::BasicFilter(value) => Some(value.to_string()),
            _ => None,
        })
    }

    // Everything this preset tells ffmpeg about encoding video, rendered as arguments: the codec,
    // its rate control, and any passthrough the file added. The audio options, the maps, the muxer
    // flags and the filter are all left out — the ahead-of-time encode writes a video-only file and
    // supplies those itself.
    //
    // This is what makes the mechanism codec-agnostic. It used to reassemble a fixed list of x264
    // options from a struct, which is why it could only ever encode with libx264; handing over
    // whatever the preset itself says means NVENC, AMF and anything else ffmpeg can spawn work
    // without this code knowing their names.
    pub fn video_encoder_args(&self) -> Vec<String> {
        use crate::lib::mpeg::core::Decode;
        let mut args = Vec::new();
        for param in &self.params {
            let keep = matches!(
                param,
                FfmpegParams::Cv(_)
                    | FfmpegParams::X264Params(_)
                    | FfmpegParams::Profile(_)
                    | FfmpegParams::Level(_)
                    | FfmpegParams::Crf(_)
                    | FfmpegParams::Preset(_)
                    | FfmpegParams::Tune(_)
                    | FfmpegParams::Qp(_)
                    | FfmpegParams::QpI(_)
                    | FfmpegParams::QpP(_)
                    | FfmpegParams::Rc(_)
                    | FfmpegParams::R(_)
                    | FfmpegParams::Quality(_)
                    | FfmpegParams::Bufsize(_)
                    | FfmpegParams::Maxrate(_)
                    | FfmpegParams::Keyframe(_)
                    | FfmpegParams::Passthrough(_)
            );
            if keep {
                args.extend(param.decode());
            }
        }
        args
    }

    // The string the speculative encode records and the foreground encode compares against before
    // adopting anything. It is the filter and the encoder arguments themselves rather than a
    // summary of them, because every field left out of it is a way for the two halves of one output
    // file to be encoded differently with nothing downstream able to tell.
    pub fn aot_compatibility(&self) -> String {
        let mut parts = vec![self.video_filter().unwrap_or_default()];
        parts.extend(self.video_encoder_args());
        parts.join("\u{1f}")
    }

    // Whether this preset's encode starts while the source is still downloading.
    //
    // Nothing about the mechanism is codec-specific: it is one ffmpeg process reading a growing
    // file, running this preset's own filter and encoder arguments, and producing the whole video
    // track that the foreground run then muxes audio into. So this is a preference, and a file may
    // declare it.
    //
    // The default is the set that has always done it — the CPU presets that do not scale. A GPU
    // preset stays off unless it asks, because its foreground encode is minutes rather than hours
    // and the overlap it buys back is worth less than an encoder the whole cluster shares; a
    // scaling preset stays off because the two are usually run as a second output beside a
    // source-resolution one, and speculating on both doubles the load to save the smaller of them.
    pub fn wants_linear_aot(&self) -> bool {
        self.aot.unwrap_or_else(|| self.aot_by_default())
    }

    fn aot_by_default(&self) -> bool {
        self.encodes_with_x264() && self.scale_height().is_none()
    }

    // Whether this preset's encodes are background work: dispatched to their own lane, run only
    // while `enc-main` is idle, and paused for as long as it is not.
    //
    // Unlike encoding ahead, there is no sensible default to derive this from. Nothing about a
    // preset's settings says whether the operator is willing to wait days for the release — a
    // 9000k 10-bit HEVC archive encode and a same-settings encode someone is watching the progress
    // bar of are the same arguments to ffmpeg — so it is off until a file says otherwise.
    pub fn wants_idle_encode(&self) -> bool {
        self.idle.unwrap_or(false)
    }

    // Whether the episode can be split across parallel encoders at all. Unlike encoding ahead, this
    // one is not a preference: the chunk scheduler drives libx264 through `pnx264` directly, and it
    // applies its own filter chain, so a preset that is not x264 or that scales cannot be chunked
    // however it is configured.
    pub fn can_chunk(&self) -> bool {
        self.encodes_with_x264() && self.scale_height().is_none()
    }

    // Whether a chunkable preset splits the episode across parallel encoders or keeps one
    // continuous linear encoder alive. Chunking only pays for the slow presets; for the rest a
    // single instance keeps its real rate-control state, which is worth more than the parallelism.
    //
    // A file may say so outright, because the derived answer can only recognise the x264 presets it
    // was written knowing about: a preset built on `slower` with a heavy `-x264-params` can be
    // slower than a bare `veryslow` and still be read as fast.
    // An idle encode is never chunked, whatever it declares. The chunk scheduler's whole purpose is
    // to finish one episode sooner by occupying every core; a preset that asked to stay out of the
    // way and then took the whole machine would be two settings that cannot both be honoured, and
    // the one the operator wrote down last is the one about staying out of the way.
    pub fn wants_chunked_encode(&self) -> bool {
        !self.wants_idle_encode()
            && self.can_chunk()
            && self.chunked.unwrap_or_else(|| self.chunks_by_default())
    }

    fn chunks_by_default(&self) -> bool {
        self.x264_settings()
            .preset
            .is_some_and(|preset| CHUNKED_X264_PRESETS.contains(&preset.as_str()))
    }
}

// The built-in table, by the same names `server_effects::preset_from_name` accepts. `x264` is here
// because that is what the encode worker has always passed on pnmpeg's command line.
pub fn builtin(name: &str) -> Option<(&'static [FfmpegParams], PresetHardware)> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "standard" | "x264" => (&CPU_SANE_DEFAULTS, PresetHardware::Cpu),
        "veryslow" | "very_slow" => (&CPU_VERYSLOW, PresetHardware::Cpu),
        "pseudolossless" | "pseudo_lossless" => (&CPU_PSEUDOLOSSLESS, PresetHardware::Cpu),
        "dummy" => (&CPU_DUMMY, PresetHardware::Cpu),
        "gpu" => (&GPU_SANE_DEFAULTS, PresetHardware::Gpu),
        "av1" => (gpu_av1(), PresetHardware::Gpu),
        "720p" => (&CPU_720P, PresetHardware::Cpu),
        "480p" => (&CPU_480P, PresetHardware::Cpu),
        _ => return None,
    })
}

// Every name the built-in table answers to under its canonical spelling, which is what a node
// advertises and what `PRESETS_DIR` is scanned against.
pub const BUILTIN_PRESET_NAMES: [&str; 8] = [
    "standard",
    "veryslow",
    "pseudolossless",
    "dummy",
    "gpu",
    "av1",
    "720p",
    "480p",
];

fn preset_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(PRESETS_DIR).join(format!("{}.toml", name.trim().to_ascii_lowercase()))
}

// The hardware a preset needs, answered without building its parameters — which is what the
// coordinator wants, since it schedules presets it never runs itself.
pub fn hardware_for(name: &str) -> PresetHardware {
    if let Some(file) = read_preset_file(name) {
        return file.hardware;
    }
    builtin(name).map(|(_, hardware)| hardware).unwrap_or_default()
}

pub fn video_codec_for(name: &str) -> Option<String> {
    resolve(name)?.video_codec()
}

// Whether a preset's encodes are background work, answered from the name alone — which is what the
// coordinator wants, since it decides which lane a job is dispatched to and never runs the encode
// itself. An unknown name is not background work: it is about to be refused anyway, and the answer
// that keeps it on the ordinary lane is the one that fails visibly.
pub fn idle_encode_for(name: &str) -> bool {
    resolve(name).is_some_and(|preset| preset.wants_idle_encode())
}

// Hardware backends worth proving at node registration. The probe is a real encode, not
// `ffmpeg -encoders`: a compiled-in backend says nothing about the driver or GPU in this machine.
pub const HARDWARE_ENCODER_CANDIDATES: [&str; 12] = [
    "h264_amf",
    "h264_nvenc",
    "h264_qsv",
    "h264_vaapi",
    "hevc_amf",
    "hevc_nvenc",
    "hevc_qsv",
    "hevc_vaapi",
    "av1_amf",
    "av1_nvenc",
    "av1_qsv",
    "av1_vaapi",
];

fn read_preset_file(name: &str) -> Option<PresetFile> {
    // A preset name reaches this from a job spec and becomes a path component, so anything that
    // could leave the directory is refused before it is joined.
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let path = preset_path(name);
    let contents = std::fs::read_to_string(&path).ok()?;
    match toml::from_str::<PresetFile>(&contents) {
        Ok(file) => Some(file),
        Err(error) => {
            // Falling back silently would encode at the built-in settings while an operator
            // believes their file is in force, and the difference only shows up in the release.
            eprintln!("[Pandora] preset {} is invalid and was ignored: {}", path.display(), error);
            None
        }
    }
}

// A preset by name: its file if it has one, the built-in table otherwise, and nothing at all if
// neither knows the name — which is a caller's error and is reported as one rather than quietly
// encoding at some default.
pub fn resolve(name: &str) -> Option<ResolvedPreset> {
    if let Some(file) = read_preset_file(name) {
        return Some(resolved_from_file(name, &file));
    }
    let (params, hardware) = builtin(name)?;
    let canonical = name.trim().to_ascii_lowercase();
    Some(ResolvedPreset {
        name: name.to_string(),
        hardware,
        params: params.to_vec(),
        aot: (canonical == "av1").then_some(true),
        chunked: None,
        idle: None,
        from_file: false,
    })
}

fn resolved_from_file(name: &str, file: &PresetFile) -> ResolvedPreset {
    let preset = ResolvedPreset {
        name: name.to_string(),
        hardware: file.hardware,
        params: params_from_file(file),
        aot: file.aot,
        chunked: file.chunked,
        idle: file.idle,
        from_file: true,
    };
    // A preset that asks for chunking it cannot have is a mistake worth naming. It runs — as one
    // linear encode, which is correct output — but silence would leave an operator waiting for a
    // speedup that was never going to arrive, and looking at the encoder rather than at the file.
    if file.chunked == Some(true) && preset.wants_idle_encode() {
        eprintln!(
            "[Pandora] preset {} asks for both chunked and idle encoding; idle wins and it will encode linearly",
            name
        );
    } else if file.chunked == Some(true) && !preset.can_chunk() {
        eprintln!(
            "[Pandora] preset {} asks for chunked encoding but {}; it will encode linearly",
            name,
            if preset.encodes_with_x264() {
                "the chunk scheduler applies its own filter chain and cannot scale"
            } else {
                "chunked encoding is libx264 only"
            }
        );
    }
    preset
}

// The fixed skeleton every preset shares. The order is the built-in tables' order and is not
// configurable: ffmpeg cares where the input, the maps and the output sit, and a file that could
// reorder them would let a typo produce an encode that runs and is wrong.
fn params_from_file(file: &PresetFile) -> Vec<FfmpegParams> {
    let mut params: Vec<FfmpegParams> = Vec::new();
    let owned = |value: &Option<String>| value.clone().map(Cow::Owned);

    params.push(FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")));
    params.push(FfmpegParams::BasicFilter(
        owned(&file.video.filter)
            .unwrap_or(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    ));
    params.push(FfmpegParams::Cv(
        owned(&file.video.codec).unwrap_or(Cow::Borrowed("libx264")),
    ));
    if let Some(value) = owned(&file.video.x264_params) {
        params.push(FfmpegParams::X264Params(value));
    }
    if let Some(value) = owned(&file.video.profile) {
        params.push(FfmpegParams::Profile(value));
    }
    if let Some(value) = owned(&file.video.level) {
        params.push(FfmpegParams::Level(value));
    }
    params.push(FfmpegParams::Map(Cow::Borrowed("0:v:0")));
    params.push(FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")));
    if let Some(value) = file.video.crf {
        params.push(FfmpegParams::Crf(value));
    }
    if let Some(value) = owned(&file.video.preset) {
        params.push(FfmpegParams::Preset(value));
    }
    if let Some(value) = owned(&file.video.qp) {
        params.push(FfmpegParams::Qp(value));
    }
    if let Some(value) = owned(&file.video.qp_i) {
        params.push(FfmpegParams::QpI(value));
    }
    if let Some(value) = owned(&file.video.qp_p) {
        params.push(FfmpegParams::QpP(value));
    }
    if let Some(value) = owned(&file.video.rc) {
        params.push(FfmpegParams::Rc(value));
    }
    if let Some(value) = owned(&file.video.framerate) {
        params.push(FfmpegParams::R(value));
    }
    if let Some(value) = owned(&file.video.tune) {
        params.push(FfmpegParams::Tune(value));
    }
    if let Some(value) = owned(&file.video.quality) {
        params.push(FfmpegParams::Quality(value));
    }
    if let Some(value) = owned(&file.video.bufsize) {
        params.push(FfmpegParams::Bufsize(value));
    }
    if let Some(value) = owned(&file.video.maxrate) {
        params.push(FfmpegParams::Maxrate(value));
    }
    if let Some(value) = owned(&file.video.keyframe) {
        params.push(FfmpegParams::Keyframe(value));
    }
    params.push(FfmpegParams::Ca(
        owned(&file.audio.codec).unwrap_or(Cow::Borrowed("aac")),
    ));
    if let Some(value) = owned(&file.audio.rate) {
        params.push(FfmpegParams::Ar(value));
    }
    if let Some(value) = owned(&file.audio.channels) {
        params.push(FfmpegParams::Ac(value));
    }
    params.push(FfmpegParams::Ba(
        owned(&file.audio.bitrate).unwrap_or(Cow::Borrowed("192k")),
    ));
    if !file.extra_args.is_empty() {
        params.push(FfmpegParams::Passthrough(file.extra_args.clone()));
    }
    params.push(FfmpegParams::Movflags);
    params.push(FfmpegParams::NoStats);
    params.push(FfmpegParams::Progress(Cow::Borrowed("pipe:2")));
    params.push(FfmpegParams::Overwrite);
    params.push(FfmpegParams::Output(Cow::Borrowed("OUTFILEV")));
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::mpeg::core::Decode;

    fn rendered(params: &[FfmpegParams]) -> Vec<Vec<String>> {
        params.iter().map(|param| param.decode()).collect()
    }

    // The reference files in `presets/` are what an operator copies into
    // `DB/config/global/presets/` to take a preset out of the binary. If one of them stopped
    // rendering the encode it claims to mirror, copying it would silently change quality — which
    // is the one failure a preset file must not be able to cause.
    #[test]
    fn every_reference_file_renders_its_builtin_exactly() {
        for name in BUILTIN_PRESET_NAMES {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("presets")
                .join(format!("{name}.toml"));
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
            let file: PresetFile = toml::from_str(&contents)
                .unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
            let (builtin_params, hardware) = builtin(name).unwrap();
            assert_eq!(file.hardware, hardware, "{name} disagrees on hardware");
            assert_eq!(
                rendered(&params_from_file(&file)),
                rendered(builtin_params),
                "{name} does not render its built-in table"
            );
            // Rendering the same arguments is not the whole of encoding the same way: how the run
            // is scheduled is decided separately, and a reference file that dropped into a
            // different strategy would take a different amount of time to produce an identical
            // file, which is exactly the kind of change nobody goes looking for.
            let from_file = resolved_from_file(name, &file);
            let from_builtin = resolve(name).unwrap();
            assert_eq!(
                (from_file.wants_linear_aot(), from_file.wants_chunked_encode()),
                (from_builtin.wants_linear_aot(), from_builtin.wants_chunked_encode()),
                "{name} encodes on a different schedule than its built-in"
            );
        }
    }

    // `presets/` also carries files that mirror no built-in — an NVENC stand-in for the AMF `gpu`
    // preset, for one. Nothing else looks at those, so without this a typo in one would only be
    // found by the operator who copied it across and got the built-in back with a line on stderr.
    #[test]
    fn every_reference_file_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            let file: PresetFile = toml::from_str(&contents)
                .unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
            // A preset that renders no encoder is a file that parsed and still cannot encode.
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let resolved = resolved_from_file(&name, &file);
            assert!(
                resolved.video_encoder_args().iter().any(|arg| arg == "-c:v"),
                "{} names no video codec",
                path.display()
            );
            seen += 1;
        }
        assert!(seen >= BUILTIN_PRESET_NAMES.len(), "only found {seen} reference presets");
    }

    // Background work is a promise about scheduling, not about settings, so nothing derives it: a
    // preset says so or it does not.
    #[test]
    fn only_a_preset_that_says_so_encodes_as_background_work() {
        let declare = |toml: &str| {
            resolved_from_file("test", &toml::from_str::<PresetFile>(toml).expect(toml))
        };

        for name in BUILTIN_PRESET_NAMES {
            let preset = resolve(name).expect(name);
            assert!(
                !preset.wants_idle_encode(),
                "{name} became background work without asking"
            );
        }
        assert!(!declare("[video]\ncodec = \"libx264\"\n").wants_idle_encode());
        assert!(declare("idle = true\n[video]\ncodec = \"libx264\"\n").wants_idle_encode());
        assert!(!declare("idle = false\n[video]\ncodec = \"libx264\"\n").wants_idle_encode());
        // Read off the name too, which is the form the coordinator asks in: it picks the lane and
        // never resolves an encode of its own.
        assert!(!idle_encode_for("standard"));
        assert!(!idle_encode_for("no-such-preset"));
    }

    // Chunking occupies every core to finish one episode sooner, which is the opposite of what an
    // idle preset asked for. A file that declares both is not refused — it encodes, linearly.
    #[test]
    fn an_idle_preset_never_chunks_however_it_asks() {
        let preset = resolved_from_file(
            "test",
            &toml::from_str::<PresetFile>(
                "idle = true\nchunked = true\n[video]\ncodec = \"libx264\"\npreset = \"veryslow\"\n",
            )
            .unwrap(),
        );
        assert!(preset.can_chunk(), "the settings themselves are chunkable");
        assert!(
            !preset.wants_chunked_encode(),
            "an idle preset must encode linearly whatever it declares"
        );
    }

    // Encoding ahead of the download is one ffmpeg process running the preset's own filter and
    // encoder arguments, so nothing about it is specific to libx264 — a GPU preset can do it, and
    // the only reason the built-ins do not is that it is not worth the shared encoder by default.
    #[test]
    fn a_preset_file_may_declare_whether_it_encodes_ahead() {
        let declare = |toml: &str| {
            resolved_from_file("test", &toml::from_str::<PresetFile>(toml).expect(toml))
        };

        // The hardware encoders, off unless asked and on when they ask.
        for codec in ["h264_nvenc", "hevc_nvenc", "h264_amf", "h264_qsv"] {
            let bare = declare(&format!("[video]\ncodec = \"{codec}\"\n"));
            assert!(!bare.wants_linear_aot(), "{codec} encoded ahead uninvited");
            let asked = declare(&format!("aot = true\n[video]\ncodec = \"{codec}\"\n"));
            assert!(asked.wants_linear_aot(), "{codec} was refused an AOT it asked for");
            // Chunking is the one that really is libx264-only, and stays refused.
            assert!(!declare(&format!("chunked = true\n[video]\ncodec = \"{codec}\"\n"))
                .wants_chunked_encode());
        }

        // A scaling preset may encode ahead now that the speculative run applies the preset's own
        // filter rather than a hardcoded one; it is still off by default.
        let scaling = "filter = \"scale=-2:'min(720,ih)',ass=INPUTFILEASS\"\n";
        assert!(!declare(&format!("[video]\n{scaling}")).wants_linear_aot());
        assert!(declare(&format!("aot = true\n[video]\n{scaling}")).wants_linear_aot());

        // And the CPU default is unchanged in both directions.
        assert!(declare("[video]\ncodec = \"libx264\"\ncrf = 17\n").wants_linear_aot());
        assert!(!declare("aot = false\n[video]\ncodec = \"libx264\"\ncrf = 17\n").wants_linear_aot());
    }

    // What the speculative encode is handed, and what it records so the foreground can tell whether
    // adopting its output is safe. Everything the encode's picture depends on has to be in that
    // string: a field left out is two halves of one file encoded differently, with nothing
    // downstream able to notice.
    #[test]
    fn the_arguments_an_aot_encode_runs_with_come_from_the_preset() {
        let standard = resolve("standard").unwrap();
        assert_eq!(
            standard.video_encoder_args(),
            vec![
                "-c:v", "libx264",
                "-x264-params", CPU_SANE_X264_PARAMS,
                "-profile:v", "high",
                "-level:v", "4.1",
                "-crf", "17",
                "-preset", "fast",
            ]
        );
        // No audio, no maps, no muxer flags, no filter: the AOT run writes a video-only file and
        // supplies those itself.
        for excluded in ["-c:a", "-b:a", "-map", "-vf", "-movflags", "-i"] {
            assert!(!standard.video_encoder_args().contains(&excluded.to_string()), "{excluded}");
        }

        // A hardware preset renders its own rate control rather than being forced through -crf.
        let gpu = resolve("gpu").unwrap();
        assert_eq!(
            gpu.video_encoder_args(),
            vec![
                "-c:v", "h264_amf",
                "-profile:v", "high",
                "-level:v", "4.1",
                "-qp_i", "15",
                "-qp_p", "15",
                "-rc", "cqp",
                "-r", "23.976",
            ]
        );
        assert!(!gpu.video_encoder_args().contains(&"-crf".to_string()));

        // The filter travels with them, still carrying its placeholder for the subtitle path.
        assert!(standard.video_filter().unwrap().contains("INPUTFILEASS"));
        assert_eq!(resolve("720p").unwrap().video_filter().as_deref(), Some(CPU_720P_FILTER));

        // Two presets that differ anywhere the picture can see must not look compatible. Codec is
        // the one that used to be missing: the old string was x264 fields only, so an NVENC prefix
        // and an x264 foreground encode compared equal.
        let same_but_nvenc = resolved_from_file(
            "standard",
            &toml::from_str("[video]\ncodec = \"h264_nvenc\"\ncrf = 17\npreset = \"fast\"\n").unwrap(),
        );
        assert_ne!(standard.aot_compatibility(), same_but_nvenc.aot_compatibility());
        assert_ne!(
            resolve("720p").unwrap().aot_compatibility(),
            resolve("standard").unwrap().aot_compatibility(),
            "a scaling preset must not adopt an unscaled prefix"
        );
        assert_eq!(
            standard.aot_compatibility(),
            resolve("standard").unwrap().aot_compatibility()
        );
    }

    // Chunking is derived from how slow the x264 preset is, and the derivation can only recognise
    // the presets it was written knowing about. A file gets the last word — but only over the
    // choice between two schedules that are both available to it.
    #[test]
    fn a_preset_file_may_declare_whether_it_chunks() {
        let declare = |toml: &str| {
            resolved_from_file("test", &toml::from_str::<PresetFile>(toml).expect(toml))
        };

        // Hand-tuned and slower than its x264 preset name suggests: the derivation says linear,
        // the file says otherwise and is obeyed.
        let slower = declare(
            "chunked = true\n[video]\npreset = \"slower\"\nx264_params = \"me=tesa:subme=11\"\n",
        );
        assert!(slower.wants_linear_aot());
        assert!(slower.wants_chunked_encode(), "a declared chunk was ignored");

        // And the other way: veryslow's own settings with chunking turned off keeps one encoder.
        let linear = declare("chunked = false\n[video]\npreset = \"veryslow\"\ncrf = 18\n");
        assert!(linear.wants_linear_aot());
        assert!(!linear.wants_chunked_encode(), "a declared linear encode chunked anyway");

        // Undeclared still derives, so every preset written before the field behaves as it did.
        let derived = declare("[video]\npreset = \"veryslow\"\ncrf = 18\n");
        assert!(derived.wants_chunked_encode());
        assert!(!declare("[video]\npreset = \"fast\"\ncrf = 17\n").wants_chunked_encode());

        // Declaring it cannot conjure a schedule the encode is not eligible for at all: chunking
        // runs through libx264, and a scaling filter has to be applied exactly once.
        let gpu = declare("chunked = true\n[video]\ncodec = \"h264_nvenc\"\n");
        assert!(!gpu.wants_chunked_encode(), "a non-x264 preset was chunked");
        let scaled = declare(
            "chunked = true\n[video]\npreset = \"veryslow\"\nfilter = \"scale=-2:'min(720,ih)',ass=INPUTFILEASS\"\n",
        );
        assert!(!scaled.wants_chunked_encode(), "a scaling preset was chunked");
    }

    // The ahead-of-time encoder drives libx264 directly and has to be handed exactly what the
    // ffmpeg run would use. If these ever disagree, the speculative prefix and the rest of the
    // episode are encoded at different settings and land in one output file with nothing
    // downstream able to tell — which is why this asserts the numbers and not just the shape.
    #[test]
    fn x264_settings_come_back_out_of_the_preset_that_produced_them() {
        let standard = resolve("standard").unwrap().x264_settings();
        assert_eq!(standard.codec, "libx264");
        assert_eq!(standard.crf, Some(17));
        assert_eq!(standard.preset.as_deref(), Some("fast"));
        assert_eq!(standard.profile.as_deref(), Some("high"));
        assert_eq!(standard.level.as_deref(), Some("4.1"));
        assert_eq!(standard.x264_params.as_deref(), Some(CPU_SANE_X264_PARAMS));

        let veryslow = resolve("veryslow").unwrap().x264_settings();
        assert_eq!(veryslow.crf, Some(18));
        assert_eq!(veryslow.preset.as_deref(), Some("veryslow"));
        // VerySlow deliberately carries no -x264-params, so AQ and motion search stay on libx264's
        // own defaults. A settings reader that invented one would change what it encodes.
        assert_eq!(veryslow.x264_params, None);

        let dummy = resolve("dummy").unwrap().x264_settings();
        assert_eq!(dummy.crf, Some(25));
        assert_eq!(dummy.preset.as_deref(), Some("veryfast"));

        let gpu = resolve("gpu").unwrap().x264_settings();
        assert_eq!(gpu.codec, "h264_amf");
        assert_eq!(gpu.crf, None);

        assert_eq!(resolve("standard").unwrap().output_video_codec().as_deref(), Some("h264"));
        assert_eq!(resolve("gpu").unwrap().output_video_codec().as_deref(), Some("h264"));
        assert_eq!(resolve("av1").unwrap().output_video_codec().as_deref(), Some("av1"));
    }

    // Which presets may start encoding before the download finishes, and which of those chunk.
    // This is the table that used to be a list of boolean flags in pnmpeg; it is pinned here
    // because getting it wrong costs either a silent latency regression or a double resample, and
    // neither shows up as a failure.
    #[test]
    fn aot_eligibility_matches_what_each_preset_can_actually_do() {
        for name in ["standard", "pseudolossless", "dummy"] {
            let preset = resolve(name).unwrap();
            assert!(preset.wants_linear_aot(), "{name} should encode ahead");
            assert!(!preset.wants_chunked_encode(), "{name} should stay linear");
        }

        // Slow enough that chunking wins back more than the coordination costs.
        let veryslow = resolve("veryslow").unwrap();
        assert!(veryslow.wants_linear_aot());
        assert!(veryslow.wants_chunked_encode());

        // Not libx264, so pnx264 cannot drive it at all.
        assert!(!resolve("gpu").unwrap().wants_linear_aot());

        // Scaling presets must run their filter exactly once, in the encode that writes the
        // output — encoding ahead would resample the prefix a second time.
        for name in ["720p", "480p"] {
            let preset = resolve(name).unwrap();
            assert!(!preset.wants_linear_aot(), "{name} must not encode ahead");
        }
    }

    // The release is named after the height it was actually encoded at, and that height is only
    // knowable from the filter the preset applied.
    #[test]
    fn a_scaling_preset_reports_the_height_its_filter_caps_at() {
        assert_eq!(resolve("720p").unwrap().scale_height(), Some(720));
        assert_eq!(resolve("480p").unwrap().scale_height(), Some(480));
        assert_eq!(resolve("standard").unwrap().scale_height(), None);
        assert_eq!(resolve("gpu").unwrap().scale_height(), None);
    }

    // The name becomes a path component and arrives from a job spec, so a preset that tries to
    // leave the directory must not be looked up at all.
    #[test]
    fn a_preset_name_that_is_not_a_plain_word_reads_no_file() {
        for name in ["../../env", "a/b", "", "with space", "sneak\0y"] {
            assert!(read_preset_file(name).is_none(), "{name} was looked up");
        }
    }

    // Only `gpu` needs a GPU, and an unknown name answers CPU rather than refusing: the scheduler
    // asks this about every preset it sees, including ones a newer node named.
    #[test]
    fn hardware_defaults_to_cpu_for_everything_but_gpu() {
        assert_eq!(builtin("gpu").unwrap().1, PresetHardware::Gpu);
        for name in ["standard", "x264", "veryslow", "720p", "dummy"] {
            assert_eq!(builtin(name).unwrap().1, PresetHardware::Cpu, "{name}");
        }
        assert_eq!(PresetHardware::parse("GPU"), Some(PresetHardware::Gpu));
        assert_eq!(PresetHardware::parse("neither"), None);
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
