use std::process::Command;
use std::string::String;
use std::borrow::Cow;
pub struct FFmpeg {
    out: String
}

pub trait Decode {
    fn decode(&self) -> String;
}

pub enum FfmpegParams {
    Input(Cow<'static, str>),
    X264Params(Cow<'static, str>),
    BasicFilter(Cow<'static, str>),
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
    Output(Cow<'static, str>),
}

impl Decode for FfmpegParams {
    fn decode(&self) -> String {    
        match self {
            Self::Input(a) => format!(" -i {}", a),
            Self::X264Params(a) => format!(" -x264-params {}", a),
            Self::BasicFilter(a) => format!(" -vf {}", a),
            Self::Cv(a) => format!(" -c:v {}", a),
            Self::Profile(a) => format!(" -profile:v {}", a),
            Self::Level(a) => format!(" -level:v {}", a),
            Self::Map(a) => format!(" -map {}", a),
            Self::R(a) => format!(" -r {}", a),
            Self::Quality(a) => format!(" -quality {}", a),
            Self::Qp(a) => format!(" -qp {}", a),
            Self::QpI(a) => format!(" -qp_i {}", a),
            Self::QpP(a) => format!(" -qp_p {}", a),
            Self::Tune(a) => format!(" -tune {}", a),
            Self::Rc(a) => format!(" -rc {}", a),
            Self::Bufsize(a) => format!(" -bufsize {}", a),
            Self::Maxrate(a) => format!(" -maxrate {}", a),
            Self::Crf(a) => format!(" -crf {}", a),
            Self::Preset(a) => format!(" -preset {}", a),
            Self::Ca(a) => format!(" -c:a {}", a),
            Self::Ba(a) => format!(" -b:a {}", a),
            Self::Movflags => String::from(" -movflags +faststart"),
            Self::Output(a) => format!(" {}", a),
        }
    }
}

pub trait Encode<T> {
    fn insert_param(&mut self, param: T);
    fn give(&self) -> String;
}

impl<T> Encode<T> for FFmpeg
where T: Decode
{
    fn insert_param(&mut self, param: T) {
        self.out.push_str(&param.decode());
    }
    fn give(&self) -> String {
        self.out.clone()
    }
}

impl FFmpeg {
    pub fn new() -> Self {
        Self {
            out: String::from("ffmpeg")
        }
    }
}

pub fn do_encode<T, I>(encoder: &mut T, params: Vec::<I>) 
where T: Encode<I>, I: Decode
{
    for i in params {
        encoder.insert_param(i);
    }

    Command::new("sh")
        .arg("-c")
        .arg(encoder.give())
        .status()
        .unwrap();

}
