use std::string::String;
use std::borrow::Cow;
use std::process::{Command, Stdio};
use std::io::{BufRead, BufReader, stderr};
use regex::Regex;

use std::sync::mpsc::Sender;

pub enum RpbData {
    Progress(u64, u64, u64, u64),
    Done(String),
    Fail,
}

pub struct FFmpeg {
    out: Command
}

pub trait Decode {
    fn decode(&self) -> Vec<String>;
}

pub enum FfmpegParams {
    Input(Cow<'static, str>),
    X264Params(Cow<'static, str>),
    BasicFilter(Cow<'static, str>),
    ComplexFilter(Cow<'static, str>),
    Cv(Cow<'static, str>),
    Profile(Cow<'static, str>),
    Level(Cow<'static, str>),
    Map(Cow<'static, str>),
    R(Cow<'static, str>),
    Quality(Cow<'static, str>),
    Qp(Cow<'static, str>),
    QpI(Cow<'static, str>),
    QpP(Cow<'static, str>),
    Tune(Cow<'static, str>),
    Rc(Cow<'static, str>),
    Bufsize(Cow<'static, str>),
    Maxrate(Cow<'static, str>),
    Crf(u8),
    Preset(Cow<'static, str>),
    Ca(Cow<'static, str>),
    Ba(Cow<'static, str>),
    Movflags,
    Stats,
    NoStats,
    Overwrite,
    NoOverwrite,
    Progress(Cow<'static, str>),
    Output(Cow<'static, str>),
}

impl Decode for FfmpegParams {
    fn decode(&self) -> Vec<String> {
        match self {
            Self::Input(a) => vec!["-i".to_string(), a.to_string()],
            Self::X264Params(a) => vec!["-x264-params".to_string(), a.to_string()],
            Self::BasicFilter(a) => vec!["-vf".to_string(), a.to_string()],
            Self::ComplexFilter(a) => vec!["-filter_complex".to_string(), a.to_string()],
            Self::Cv(a) => vec!["-c:v".to_string(), a.to_string()],
            Self::Profile(a) => vec!["-profile:v".to_string(), a.to_string()],
            Self::Level(a) => vec!["-level:v".to_string(), a.to_string()],
            Self::Map(a) => vec!["-map".to_string(), a.to_string()],
            Self::R(a) => vec!["-r".to_string(), a.to_string()],
            Self::Quality(a) => vec!["-quality".to_string(), a.to_string()],
            Self::Qp(a) => vec!["-qp".to_string(), a.to_string()],
            Self::QpI(a) => vec!["-qp_i".to_string(), a.to_string()],
            Self::QpP(a) => vec!["-qp_p".to_string(), a.to_string()],
            Self::Tune(a) => vec!["-tune".to_string(), a.to_string()],
            Self::Rc(a) => vec!["-rc".to_string(), a.to_string()],
            Self::Bufsize(a) => vec!["-bufsize".to_string(), a.to_string()],
            Self::Maxrate(a) => vec!["-maxrate".to_string(), a.to_string()],
            Self::Crf(a) => vec!["-crf".to_string(), a.to_string()],
            Self::Preset(a) => vec!["-preset".to_string(), a.to_string()],
            Self::Ca(a) => vec!["-c:a".to_string(), a.to_string()],
            Self::Ba(a) => vec!["-b:a".to_string(), a.to_string()],
            Self::Movflags => vec!["-movflags".to_string(), "+faststart".to_string()],
            Self::Stats => vec!["-stats".to_string()],
            Self::NoStats => vec!["-nostats".to_string()],
            Self::Overwrite => vec!["-y".to_string()],
            Self::NoOverwrite => vec!["-n".to_string()],
            Self::Progress(a) => vec!["-progress".to_string(), a.to_string()],
            Self::Output(a) => vec![a.to_string()],
        }
    }
}

pub trait Encode<T> {
    fn insert_param(&mut self, param: T);
    fn run(&mut self) -> bool;
}

impl<T> Encode<T> for FFmpeg
where T: Decode
{
    fn insert_param(&mut self, param: T) {
        for i in param.decode() {
            self.out.arg(i);
        }
    }
    fn run(&mut self) -> bool {
        self.out.status().unwrap();
        true
    }
}

impl AsMut<FFmpeg> for FFmpeg {
    fn as_mut(&mut self) -> &mut FFmpeg {
        self
    }
}

impl FFmpeg {
    pub fn new() -> Self {
        Self {
            out: Command::new("ffmpeg")
        }
    }
}

pub fn do_encode<T, I>(encoder: &mut T, params: Vec::<I>)
where T: Encode<I>, I: Decode
{
    for i in params {
        encoder.insert_param(i);
    }

    encoder.run();

}

pub fn do_comm_encode_ffmpeg<T, I>(
    encoder: &mut T,
    params: Vec<I>,
    tx: Sender<RpbData>,
    totalframe: Option<u64>,
)
where
    T: Encode<I> + AsMut<FFmpeg>,
    I: Decode,
{
    for p in params {
        encoder.insert_param(p);
    }

    let ffmpeg = encoder.as_mut();
    ffmpeg.out.stderr(Stdio::piped());
    ffmpeg.out.stdout(Stdio::null());
    let mut child = ffmpeg.out.spawn().expect("Failed to spawn ffmpeg");
    let stderr = child.stderr.take().expect("No stderr");

    let reader = BufReader::new(stderr);

    // ---- Regex bundle ----
    let frame_re   = Regex::new(r"frame=\s*(\d+)").unwrap();
    let fps_re     = Regex::new(r"fps=\s*([\d\.]+)").unwrap();
    let bitrate_re = Regex::new(r"bitrate=\s*([\d\.]+)").unwrap();

    let tx_clone = tx.clone();

    std::thread::spawn(move || {
        let mut last_fps: u64 = 0;
        let mut last_frame: u64 = 0;
        let mut last_bitrate: u64 = 0;

        // Placeholder until you implement probe
        let total_frame: u64 = totalframe.unwrap_or(0);

        for line in reader.lines().flatten() {
            if let Some(cap) = frame_re.captures(&line) {
                last_frame = cap[1].parse::<u64>().unwrap_or(0);
            }

            if let Some(cap) = fps_re.captures(&line) {
                last_fps = cap[1]
                    .parse::<f64>()
                    .unwrap_or(0.0) as u64;
            }

            if let Some(cap) = bitrate_re.captures(&line) {
                last_bitrate = cap[1]
                    .parse::<f64>()
                    .unwrap_or(0.0) as u64;
            }

            // Only emit when we actually got frame info
            if line.contains("frame=") {
                let _ = tx_clone.send(
                    RpbData::Progress(
                        last_fps,
                        last_frame,
                        total_frame,
                        last_bitrate
                    )
                );
            }
        }
    });

    let status = child.wait().expect("Failed to wait ffmpeg");

    if status.success() {
        let _ = tx.send(RpbData::Done("DONE".into()));
    } else {
        println!("aaaaa");
        let _ = tx.send(RpbData::Fail);
    }
}
