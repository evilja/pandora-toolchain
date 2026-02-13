

use pandora_toolchain::libpnmpeg::{
    core::{
        FFmpeg, FfmpegParams, do_comm_encode_ffmpeg}, preset::{
        CONCAT, CPU_PSEUDOLOSSLESS, CPU_SANE_DEFAULTS, GPU_SANE_DEFAULTS
    }, probe::{
        ffprobe_lang,
        ffprobe_frame
    }
};
use tokio::time::{Duration, Instant};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use pandora_toolchain::libpnmpeg::core::RpbData;
use pandora_toolchain::libpnprotocol::core::{Protocol, Schema, ToolInfo};
use clap::Parser;
use std::thread::{self};
use std::sync::mpsc::{self, Receiver, Sender};
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

    #[arg(long)]
    pseudolossless: bool,

    #[arg(long)]
    concat: bool,

    /// Input file
    #[arg(short, long)]
    input: String,

    /// Output file
    #[arg(short, long)]
    output: String,

    /// ASS subtitle file
    #[arg(short, long)]
    ass: Option<String>,

    /// Language to search in input file
    #[arg(short, long)]
    lang: Option<String>,

    #[arg(short, long)]
    subinput: Option<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,
}
#[inline]
fn wrap(a: &str) -> String { return String::from(a) }

fn main() {
    let args = Args::parse();
    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(ToolInfo { tool: match args.negotiator {
                        Some(ref negotiator) => negotiator,
                        None => "PNmpeg",
                    }, build: match args.negver {
                        Some(ref negver) => negver,
                        None => "0.1.1",
                    }, proto: 1 },
                  ToolInfo { tool: "PNmpeg", build: "0.1.1", proto: 1 },
                  match args.negkey {
                      Some(key) => key,
                      None => "PNmpegCLI".to_string(),
                  });

    let mut encoder = FFmpeg::new();
    let mut params: Vec<FfmpegParams>;
    let a = if args.gpu == true { 1 } else { 0 } +
            if args.x264 == true { 1 } else { 0 } +
            if args.pseudolossless == true { 1 } else { 0 }
            ;
    if a > 1 {
        panic!("You must use one preset at a time.");
    } else if args.gpu {
        params = Vec::from(GPU_SANE_DEFAULTS);
    } else if args.x264 {
        params = Vec::from(CPU_SANE_DEFAULTS);
    } else if args.pseudolossless {
        params = Vec::from(CPU_PSEUDOLOSSLESS);
    } else if args.concat {
        params = Vec::from(CONCAT);
    } else {
        params = Vec::from(CPU_SANE_DEFAULTS);
    }
    let jpn_index = if !args.concat { match ffprobe_lang(&args.input, &match args.lang {
        Some(l) => l,
        None => wrap("jpn"),
    }) {
        Some(v) => v,
        None => panic!("selected lang (default jpn) not found in your input")
    } } else {
        0
    };
    let mut totalframe: u64 = 0;
    for i in params.iter_mut() {
        match i {
            FfmpegParams::Map(a) => {
                *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", &format!("{}", jpn_index))));
            },
            FfmpegParams::Input(a) => {
                let mut c = a.to_string();
                c = c.replace("INPUTFILEV", &args.input);
                if let Some(ref b) = args.subinput {
                    c = c.replace("CONCATFILEV", b);
                }
                totalframe += ffprobe_frame(&c).unwrap_or(0);
                *i = FfmpegParams::Input(Cow::Owned(c));
            },
            FfmpegParams::BasicFilter(a) => {
                if let Some(ref b) = args.ass {
                    *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", b)));
                }
            }
            FfmpegParams::Output(a) => {
                *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
            }
            _=> ()
        }
    }
    let (tx, rx): (Sender<RpbData>, Receiver<RpbData>) = mpsc::channel();
    let _thr = thread::spawn(move || {
        do_comm_encode_ffmpeg(
            &mut encoder,
            params,
            tx,
            Some(totalframe)
        );
    });
    let mut last = Instant::now();
    while let Ok(val) = rx.recv() {
        match val {
            RpbData::Progress(fps, frame, total, bitrate) => {
                if last.elapsed() < Duration::from_secs(5) {
                    continue;
                }
                last = Instant::now();
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, [leaf, leaf, leaf, leaf]],
                        data   = ["0", [fps, frame, total, bitrate]]
                    ).unwrap()
                )
            }
            RpbData::Done(a) => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data   = ["1", a]
                    ).unwrap()
                )
            }
            RpbData::Fail => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data   = ["2", "0"]
                    ).unwrap()
                )
            }
        }
    }
}
