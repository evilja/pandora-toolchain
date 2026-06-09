use pandora_toolchain::libpnmpeg::{
    core::{
        FFmpeg, FfmpegParams, do_comm_encode_ffmpeg}, preset::{
        CONCAT, CONCAT_LEGACY, CPU_DUMMY, CPU_PSEUDOLOSSLESS, CPU_SANE_DEFAULTS, GPU_SANE_DEFAULTS
    }, probe::{
        ffprobe_frame, ffprobe_framerate, ffprobe_lang, ffprobe_samplerate
    }
};
use tokio::{fs::File, io::AsyncWriteExt, time::{Duration, Instant}};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use pandora_toolchain::libpnmpeg::core::RpbData;
use pandora_toolchain::libpnprotocol::core::{Protocol, Schema, ToolInfo};
use std::str::FromStr;
use clap::Parser;
use std::path::PathBuf;
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

    #[arg(long)]
    dummy: bool,

    #[arg(long)]
    legacyconcat: bool,

    /// Input file
    #[arg(short, long)]
    input: String,

    /// Output file
    #[arg(short, long)]
    output: String,

    /// ASS subtitle file
    #[arg(short, long)]
    ass: Option<String>,

    #[arg(long)]
    fontconfig: Option<String>,

    /// Language to search in input file
    #[arg(short, long)]
    lang: Option<String>,

    #[arg(short, long)]
    subinput: Option<String>,

    /// Intro candidate - if any of these videos' properties are the same as main, it'll get selected.
    /// Otherwise, highest framerate video will get reencoded into main's properties.
    #[arg(short, long, num_args = 0..)]
    candidate: Vec<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,

    #[arg(long)]
    cancelfile: Option<String>,

    #[arg(long)]
    logfile: Option<String>,
}

#[inline]
fn wrap(a: &str) -> String { return String::from(a) }

#[tokio::main]
async fn main() {
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
    let concfilepath = PathBuf::from_str(&args.input).unwrap()
        .parent().unwrap()
        .canonicalize().unwrap()
        .join("PNmpeg_Concat.txt");

    let selected_subinput = select_subinput(&args.input, &args.candidate, &args.subinput);

    let use_legacy = args.concat && !args.candidate.is_empty() && selected_subinput.as_ref().map(|p| {
        ffprobe_framerate(p) != ffprobe_framerate(&args.input) ||
        ffprobe_samplerate(p) != ffprobe_samplerate(&args.input)
    }).unwrap_or(false);

    let mut concfile = match args.concat && !use_legacy {
        true => Some(File::create(&concfilepath).await.unwrap()),
        false => None,
    };

    let mut params: Vec<FfmpegParams>;
    let a = if args.gpu { 1 } else { 0 } +
            if args.x264 { 1 } else { 0 } +
            if args.pseudolossless { 1 } else { 0 } +
            if args.dummy { 1 } else { 0 };

    if a > 1 {
        panic!("You must use one preset at a time.");
    } else if args.gpu {
        params = Vec::from(GPU_SANE_DEFAULTS);
    } else if args.x264 {
        params = Vec::from(CPU_SANE_DEFAULTS);
    } else if args.pseudolossless {
        params = Vec::from(CPU_PSEUDOLOSSLESS);
    } else if args.concat {
        if use_legacy {
            params = Vec::from(CONCAT_LEGACY);
        } else {
            params = Vec::from(CONCAT);
        }
    } else if args.legacyconcat {
        params = Vec::from(CONCAT_LEGACY);
    } else if args.dummy {
        params = Vec::from(CPU_DUMMY);
    } else {
        params = Vec::from(CPU_SANE_DEFAULTS);
    }

    let jpn_index = if !args.concat || args.legacyconcat {
        match ffprobe_lang(&args.input, &match args.lang {
            Some(l) => format!("{}", l),
            None => wrap("jpn"),
        }) {
            Some(v) => format!("{}", v),
            None => wrap("a")
        }
    } else {
        wrap("1")
    };

    let mut totalframe: u64 = 0;
    for i in params.iter_mut() {
        match i {
            FfmpegParams::Map(a) => {
                *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", &format!("{}", jpn_index))));
            },
            FfmpegParams::Input(a) => {
                let mut c = a.to_string();
                if let Some(ref mut file) = concfile {
                    if let Some(ref b) = selected_subinput {
                        let canon_input = PathBuf::from_str(&args.input).unwrap().canonicalize().unwrap().display().to_string();
                        let canon_snput = PathBuf::from_str(b).unwrap().canonicalize().unwrap().display().to_string();
                        file.write(format!("file '{}'\nfile '{}'\n", canon_snput, canon_input).as_bytes()).await.unwrap();
                    }
                    c = c.replace("CONCATFILEV", &concfilepath.display().to_string());
                } else {
                    c = c.replace("INPUTFILEV", &args.input);
                    if let Some(ref b) = selected_subinput {
                        c = c.replace("CONCATFILEV", b);
                    }
                }
                totalframe += ffprobe_frame(&c).unwrap_or(0);
                *i = FfmpegParams::Input(Cow::Owned(c));
            },
            FfmpegParams::BasicFilter(a) => {
                if let Some(ref b) = args.ass {
                    let ass = match args.fontconfig {
                        Some(ref fontconfig) if !fontconfig.is_empty() => {
                            format!("{}:fontsdir={}", b, fontconfig)
                        }
                        _ => b.to_string(),
                    };
                    *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", &ass)));
                }
            }
            FfmpegParams::Output(a) => {
                *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
            }
            FfmpegParams::R(a) => {
                if a.contains("FPSV") {
                    let fps = ffprobe_framerate(&args.input)
                        .map(|(n, d)| format!("{}/{}", n, d))
                        .unwrap_or_else(|| "24".to_string());
                    *i = FfmpegParams::R(Cow::Owned(a.replace("FPSV", &fps)));
                }
            },
            _ => ()
        }
    }

    let (tx, rx): (Sender<RpbData>, Receiver<RpbData>) = mpsc::channel();
    let _thr = tokio::spawn(async move {
        do_comm_encode_ffmpeg(
            &mut encoder,
            params,
            tx,
            Some(totalframe),
            args.cancelfile,
            args.logfile,
        ).await;
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
            RpbData::CancelFile => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data   = ["3", "CANCELFILE"]
                    ).unwrap()
                )
            }
        }
    }
}

fn select_subinput(input: &String, candidates: &Vec<String>, subinput: &Option<String>) -> Option<String> {
    if !candidates.is_empty() {
        let main_fps = ffprobe_framerate(input);
        let main_sr = ffprobe_samplerate(input);
        let mut best_match: Option<(usize, &String)> = None;
        let mut highest_fps: Option<(&String, (u32, u32))> = None;
        for candidate in candidates {
            let cand_fps = ffprobe_framerate(candidate);
            let cand_sr = ffprobe_samplerate(candidate);
            if let Some(fps) = cand_fps {
                match highest_fps {
                    None => highest_fps = Some((candidate, fps)),
                    Some((_, hfps)) => {
                        if fps.0 * hfps.1 > hfps.0 * fps.1 {
                            highest_fps = Some((candidate, fps));
                        }
                    }
                }
            }

            let mut score = 0usize;
            if main_fps.is_some() && cand_fps == main_fps { score += 1; }
            if main_sr.is_some() && cand_sr == main_sr { score += 1; }
            if score > best_match.map(|(s, _)| s).unwrap_or(0) {
                best_match = Some((score, candidate));
            }
        }

        if let Some((score, path)) = best_match {
            if score >= 2 {
                Some(path.clone())
            } else {
                highest_fps.map(|(p, _)| p.clone())
            }
        } else {
            None
        }
    } else {
        subinput.clone()
    }
}
