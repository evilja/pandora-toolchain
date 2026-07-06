use crate::lib::mpeg::core::FfmpegParams;
use std::borrow::Cow;


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
    FfmpegParams::X264Params(Cow::Borrowed("me=umh:subme=8:merange=24:trellis=2:psy-rd=1:aq-strength=1.1:aq-mode=3")),
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
    FfmpegParams::X264Params(Cow::Borrowed("aq-strength=1.0:aq-mode=3")),
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
