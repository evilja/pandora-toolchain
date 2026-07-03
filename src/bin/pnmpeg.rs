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
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
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

    #[arg(long)]
    joinconcat: bool,

    #[arg(long)]
    joinass: bool,

    /// Input file
    #[arg(short, long)]
    input: String,

    /// Output file
    #[arg(short, long)]
    output: String,

    /// ASS subtitle file
    #[arg(short, long)]
    ass: Option<String>,

    #[arg(long, alias = "fontdir")]
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

    let encoder = FFmpeg::new();
    let concfilepath = PathBuf::from_str(&args.input).unwrap()
        .parent().unwrap()
        .canonicalize().unwrap()
        .join("PNmpeg_Concat.txt");

    let selected_subinput = select_subinput(&args.input, &args.candidate, &args.subinput);

    if args.joinconcat || args.joinass {
        let mut join_inputs = vec![args.input.clone()];
        join_inputs.extend(args.candidate.iter().cloned());
        let mut totalframe: u64 = 0;
        let parent = PathBuf::from_str(&args.input).unwrap()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let joinfile = parent
            .canonicalize()
            .unwrap_or(parent)
            .join("PNmpeg_Keycode_Concat.txt");
        let mut file = File::create(&joinfile).await.unwrap();
        for input in &join_inputs {
            totalframe += ffprobe_frame(input).unwrap_or(0);
            let canon = PathBuf::from_str(input).unwrap()
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(input))
                .display()
                .to_string();
            file.write(format!("file '{}'\n", canon.replace('\'', "'\\''")).as_bytes()).await.unwrap();
        }
        drop(file);

        let mut params = if args.joinconcat {
            Vec::from(CONCAT)
        } else {
            let mut p = Vec::from(CPU_SANE_DEFAULTS);
            p.insert(0, FfmpegParams::Safe(Cow::Borrowed("0")));
            p.insert(0, FfmpegParams::Format(Cow::Borrowed("concat")));
            p
        };
        for i in params.iter_mut() {
            match i {
                FfmpegParams::Input(a) => {
                    let c = a
                        .replace("CONCATFILEV", &joinfile.display().to_string())
                        .replace("INPUTFILEV", &joinfile.display().to_string());
                    *i = FfmpegParams::Input(Cow::Owned(c));
                }
                FfmpegParams::BasicFilter(a) => {
                    if let Some(ref b) = args.ass {
                        let ass = quote_filter_value(b);
                        *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", &ass)));
                    }
                }
                FfmpegParams::Map(a) => {
                    *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", "a:0")));
                }
                FfmpegParams::Output(a) => {
                    *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
                }
                _ => {}
            }
        }
        run_with_progress(&mut proto, &neg, encoder, params, totalframe, args.cancelfile, args.logfile).await;
        return;
    }

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

    let audio_index = if !args.concat || args.legacyconcat {
        args.lang.as_deref()
            .and_then(|lang| ffprobe_lang(&args.input, lang).map(|idx| idx.to_string()))
            .unwrap_or_else(|| wrap("a:0"))
    } else {
        wrap("1")
    };

    let mut totalframe: u64 = 0;
    for i in params.iter_mut() {
        match i {
            FfmpegParams::Map(a) => {
                *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", &format!("{}", audio_index))));
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
                    let ass = quote_filter_value(b);
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

    run_with_progress(&mut proto, &neg, encoder, params, totalframe, args.cancelfile, args.logfile).await;
}

async fn run_with_progress(
    proto: &mut Protocol,
    neg: &str,
    mut encoder: FFmpeg,
    params: Vec<FfmpegParams>,
    totalframe: u64,
    cancelfile: Option<String>,
    logfile: Option<String>,
) {
    let (tx, mut rx): (UnboundedSender<RpbData>, UnboundedReceiver<RpbData>) = mpsc::unbounded_channel();
    let _thr = tokio::spawn(async move {
        do_comm_encode_ffmpeg(
            &mut encoder,
            params,
            tx,
            Some(totalframe),
            cancelfile,
            logfile,
        ).await;
    });

    let mut last: Option<Instant> = None;
    while let Some(val) = rx.recv().await {
        match val {
            RpbData::Progress(fps, frame, total, bitrate) => {
                if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) {
                    continue;
                }
                last = Some(Instant::now());
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, [leaf, leaf, leaf, leaf]],
                        data   = ["0", [fps, frame, total, bitrate]]
                    ).unwrap()
                )
            }
            RpbData::Warning(warning) => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["4", warning]
                    ).unwrap()
                )
            }
            RpbData::Done(a) => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["1", a]
                    ).unwrap()
                )
            }
            RpbData::Fail => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["2", "0"]
                    ).unwrap()
                )
            }
            RpbData::CancelFile => {
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
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

fn quote_filter_value(value: &str) -> String {
    format!("'{}'", escape_filter_value(value))
}

fn escape_filter_value(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' | '\'' | ':' | ',' | '[' | ']' | ';' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::quote_filter_value;

    #[test]
    fn quote_filter_value_escapes_filter_specials() {
        assert_eq!(
            quote_filter_value("C:\\work,subs\\a'b.ass"),
            "'C\\:\\\\work\\,subs\\\\a\\'b.ass'"
        );
    }
}
