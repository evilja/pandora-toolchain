use std::str::FromStr;
use std::string::String;
use std::borrow::Cow;
use crate::lib::bin::resolve_runtime_binary;
use crate::lib::logging::core::LoggingHandle;
use crate::log;
use std::process::{Command, Stdio};
use std::path::PathBuf;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;
use std::collections::HashSet;

pub enum RpbData {
    Progress(u64, u64, u64, u64),
    Warning(String),
    Done(String),
    Fail,
    CancelFile,
}

pub struct FFmpeg {
    out: Command
}

pub trait Decode {
    fn decode(&self) -> Vec<String>;
}

pub enum FfmpegParams {
    Input(Cow<'static, str>),
    Seek(Cow<'static, str>),
    Duration(Cow<'static, str>),
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
    Ar(Cow<'static, str>),
    Ac(Cow<'static, str>),
    Ba(Cow<'static, str>),
    Format(Cow<'static, str>),
    Safe(Cow<'static, str>),
    Keyframe(Cow<'static, str>),
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
            Self::Seek(a) => vec!["-ss".to_string(), a.to_string()],
            Self::Duration(a) => vec!["-t".to_string(), a.to_string()],
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
            Self::Ar(a) => vec!["-ar".to_string(), a.to_string()],
            Self::Ac(a) => vec!["-ac".to_string(), a.to_string()],
            Self::Ba(a) => vec!["-b:a".to_string(), a.to_string()],
            Self::Format(a) => vec!["-f".to_string(), a.to_string()],
            Self::Safe(a) => vec!["-safe".to_string(), a.to_string()],
            Self::Keyframe(a) => vec!["-g".to_string(), a.to_string()],
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
            out: Command::new(resolve_runtime_binary("ffmpeg"))
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

pub fn run_ffmpeg_params(params: Vec<FfmpegParams>) -> bool {
    let mut encoder = FFmpeg::new();
    for param in params {
        encoder.insert_param(param);
    }
    encoder.out.stderr(Stdio::null());
    encoder.out.stdout(Stdio::null());
    encoder.out.status().map(|s| s.success()).unwrap_or(false)
}

pub async fn do_comm_encode_ffmpeg<T, I>(
    encoder: &mut T,
    params: Vec<I>,
    tx: UnboundedSender<RpbData>,
    totalframe: Option<u64>,
    cancelfile: Option<String>,
    logfile: Option<String>
)
where
    T: Encode<I> + AsMut<FFmpeg>,
    I: Decode,
{
    let cfile: Option<PathBuf> = match cancelfile {
        Some(str) => {
            Some(PathBuf::from(str))
        }
        None => { None }
    };
    let mut handle: Option<LoggingHandle> = match logfile {
        Some(pb) => {
            Some(LoggingHandle::get_handle(&PathBuf::from_str(&pb).unwrap()).await.unwrap())
        }
        None => None,
    };
    let mut l = String::new();
    for p in params {
        l.push_str(&p.decode().join(" "));
        l.push(' ');
        encoder.insert_param(p);
    }
    l.push('\n');
    log!(handle, &l);
    let ffmpeg = encoder.as_mut();
    let mut command = tokio::process::Command::new(ffmpeg.out.get_program());
    command.args(ffmpeg.out.get_args());
    command.stderr(Stdio::piped());
    command.stdout(Stdio::null());
    let mut child = command.spawn().expect("Failed to spawn ffmpeg");
    log!(handle, "FFmpeg spawned\n");
    let stderr = child.stderr.take().expect("No stderr");
    log!(handle, "stderr taken\n");

    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    let frame_re   = Regex::new(r"frame=\s*(\d+)").unwrap();
    let fps_re     = Regex::new(r"fps=\s*([\d\.]+)").unwrap();
    let bitrate_re = Regex::new(r"bitrate=\s*([\d\.]+)").unwrap();
    let fontselect_re = Regex::new(r"fontselect:\s*\(([^,\)]*).*->\s*(.*)$").unwrap();

    let mut last_fps: u64 = 0;
    let mut last_frame: u64 = 0;
    let mut last_bitrate: u64 = 0;
    let total_frame: u64 = totalframe.unwrap_or(0);
    let mut emitted_warnings: HashSet<String> = HashSet::new();

    while let Ok(Some(line)) = lines.next_line().await {
        log!(handle, &format!("{line}\n"));
        if let Some(ref cancelfile) = cfile {
            if cancelfile.try_exists().unwrap_or(false) {
                let _ = child.kill().await;
                tx.send(RpbData::CancelFile).unwrap();
                return;
            }
        }
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

        if let Some(warning) = fontselect_warning(&line, &fontselect_re) {
            if emitted_warnings.insert(warning.clone()) {
                let _ = tx.send(RpbData::Warning(warning));
            }
        }

        if line.contains("frame=") {
            let _ = tx.send(
                RpbData::Progress(
                    last_fps,
                    last_frame,
                    total_frame,
                    last_bitrate
                )
            );
        }
    }
    if let Some(mut a) = handle {
        a.flush().await;
    }
    let status = child.wait().await.expect("Failed to wait ffmpeg");

    if status.success() {
        let _ = tx.send(RpbData::Done("DONE".into()));
    } else {
        let _ = tx.send(RpbData::Fail);
    }
}

fn fontselect_warning(line: &str, re: &Regex) -> Option<String> {
    if !line.contains("fontselect") {
        return None;
    }
    let cap = re.captures(line)?;
    let source = cap.get(1)?.as_str().trim();
    let target = cap.get(2)?.as_str();
    if !target.contains("ArialMT") || source.to_ascii_lowercase().contains("arial") || source.is_empty() {
        return None;
    }
    Some(format!("{} -> ArialMT", source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_non_arial_fontselect_fallback_to_arialmt() {
        let re = Regex::new(r"fontselect:\s*\(([^,\)]*).*->\s*(.*)$").unwrap();
        let line = "[Parsed_ass_0] fontselect: (Gumbo DEMO, 400, 0) -> ArialMT, 0, ArialMT";

        assert_eq!(
            fontselect_warning(line, &re),
            Some("Gumbo DEMO -> ArialMT".to_string())
        );
    }

    #[test]
    fn ignores_arial_fontselect_fallback_to_arialmt() {
        let re = Regex::new(r"fontselect:\s*\(([^,\)]*).*->\s*(.*)$").unwrap();
        let line = "[Parsed_ass_0] fontselect: (Arial, 400, 0) -> ArialMT, 0, ArialMT";

        assert_eq!(fontselect_warning(line, &re), None);
    }

    #[test]
    fn detects_path_fontselect_fallback_to_arialmt() {
        let re = Regex::new(r"fontselect:\s*\(([^,\)]*).*->\s*(.*)$").unwrap();
        let line = "[Parsed_ass_0] fontselect: (Vesta-Bold, 700, 0) -> /usr/share/fonts/arial.ttf, 0, ArialMT";

        assert_eq!(
            fontselect_warning(line, &re),
            Some("Vesta-Bold -> ArialMT".to_string())
        );
    }
}
