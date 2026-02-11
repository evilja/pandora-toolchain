
use pandora_toolchain::libpnmpeg::{
    core::{
        FFmpeg, 
        FfmpegParams, 
        do_encode}, 
    probe::ffprobe
};
use clap::Parser;
use std::borrow::Cow;

#[derive(Parser, Debug)]
#[command(
    name = "pnmpeg",
    version = "0.1.1",
    about = "Pandora Toolchain FFmpeg wrapper",
    long_about = None
)]
struct Args {
    /// Use GPU encoder (nvenc / vaapi / mesa)
    #[arg(long)]
    gpu: bool,

    /// Use x264 software encoder
    #[arg(long)]
    x264: bool,

    /// Input file
    #[arg(short, long)]
    input: String,

    /// Output file
    #[arg(short, long)]
    output: String,

    /// ASS subtitle file
    #[arg(short, long)]
    subs: Option<String>,

    /// Language to search in input file
    #[arg(short, long)]
    lang: Option<String>,

    #[arg(short, long)]
    concat: Option<String>,
}
#[inline]
fn wrap(a: &str) -> String { return String::from(a) }

const CPU_SANE_DEFAULTS: [FfmpegParams; 14] =
[
    FfmpegParams::Input(Cow::Borrowed("INPUTFILEV")),
    FfmpegParams::X264Params(Cow::Borrowed("me=umh:subme=8:merange=24:trellis=2:psy-rd=1:aq-strength=1.1:aq-mode=3:deblock=0,0")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass=INPUTFILEASS,format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("libx264")),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::Crf(17),
    FfmpegParams::Preset(Cow::Borrowed("fast")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::Output(Cow::Borrowed("'OUTFILEV'")),
]; 
const GPU_SANE_DEFAULTS: [FfmpegParams; 15] =
[
    FfmpegParams::Input(Cow::Borrowed("'INPUTFILEV'")),
    FfmpegParams::BasicFilter(Cow::Borrowed("ass='INPUTFILEASS',format=yuv420p")),
    FfmpegParams::Cv(Cow::Borrowed("h264_amf")),
    FfmpegParams::Profile(Cow::Borrowed("high")),
    FfmpegParams::Level(Cow::Borrowed("4.1")),
    FfmpegParams::Map(Cow::Borrowed("0:v")),
    FfmpegParams::Map(Cow::Borrowed("0:JPN_INDEX")),
    FfmpegParams::QpI(Cow::Borrowed("15")),
    FfmpegParams::QpP(Cow::Borrowed("15")),
    FfmpegParams::Rc(Cow::Borrowed("cqp")),
    FfmpegParams::R(Cow::Borrowed("23.976")),
    FfmpegParams::Ca(Cow::Borrowed("aac")),
    FfmpegParams::Ba(Cow::Borrowed("192k")),
    FfmpegParams::Movflags,
    FfmpegParams::Output(Cow::Borrowed("OUTFILEV")),
]; 

fn main() {
    let args = Args::parse();

    let mut encoder = FFmpeg::new();
    let mut params: Vec<FfmpegParams>;

    if args.gpu && args.x264 {
        panic!("You must use one preset at a time.");
    } else if args.gpu {
        params = Vec::from(GPU_SANE_DEFAULTS);
    } else if args.x264 {
        params = Vec::from(CPU_SANE_DEFAULTS);
    } else {
        params = Vec::from(CPU_SANE_DEFAULTS);
    }

    let jpn_index =  match ffprobe(&args.input, &match args.lang {
        Some(l) => l,
        None => wrap("jpn"),
    }) {
        Some(v) => v,
        None => panic!("selected lang (default jpn) not found in your input")
    };

    for i in params.iter_mut() {
        match i {
            FfmpegParams::Map(a) => {
                *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", &format!("{}", jpn_index))));
            },
            FfmpegParams::Input(a) => {
                *i = FfmpegParams::Input(Cow::Owned(a.replace("INPUTFILEV", &args.input)));
            },
            FfmpegParams::BasicFilter(a) => {
                if let Some(ref b) = args.subs {
                    *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", &b)));
                }
            }
            FfmpegParams::Output(a) => {
                *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
            }
            _=> ()
        }
    }
    do_encode(
        &mut encoder,
        params 
    );
}
