use std::path::{Path, PathBuf};
use std::process::Command;
use serde::{Deserialize, Serialize};
use crate::lib::bin::resolve_runtime_binary;

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Vec<AudioStream>,
}

#[derive(Debug, Deserialize)]
struct AudioStream {
    index: u32,
    tags: Option<Tags>,
}

#[derive(Debug, Deserialize)]
struct Tags {
    language: Option<String>,
}


pub fn ffprobe_lang(path: &str, f_lang: &str) -> Option<u32> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "a",
            "-show_entries", "stream=index:stream_tags=language",
            "-of", "json",
            path,
        ])
        .output().unwrap();

    let data: FfprobeOutput = serde_json::from_slice(&output.stdout).unwrap();

    for stream in data.streams {
        let lang = stream
            .tags
            .and_then(|t| t.language)
            .unwrap_or_else(|| "und".to_string());

        if f_lang == lang {
            return Some(stream.index);
        }
    }
    return None;
}
/*
 * ffprobe -v error -select_streams v:0 -count_packets
 *   -show_entries stream=nb_read_packets -of csv=p=0 input.mp4
 */
pub fn ffprobe_frame(path: &str) -> Option<u64> {
     let output = Command::new(resolve_runtime_binary("ffprobe"))
         .args([
             "-v", "error",
             "-select_streams", "v:0",
             "-count_packets",
             "-show_entries", "stream=nb_read_packets",
             "-of", "csv=p=0",
             path,
         ])
         .output().ok()?;

     let stdout = String::from_utf8(output.stdout).ok()?;
     stdout.trim().parse::<u64>().ok()
 }


#[derive(Debug, Deserialize)]
struct FfprobeFramerate {
    streams: Vec<FramerateStream>,
}
#[derive(Debug, Deserialize)]
struct FramerateStream {
    r_frame_rate: String,
}
#[derive(Debug, Deserialize)]
struct FfprobeSamplerate {
    streams: Vec<SamplerateStream>,
}
#[derive(Debug, Deserialize)]
struct SamplerateStream {
    sample_rate: String,
}

// Returns (numerator, denominator) e.g. (24000, 1001) or (24, 1)
pub fn ffprobe_framerate(path: &str) -> Option<(u32, u32)> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=r_frame_rate",
            "-of", "json",
            path,
        ])
        .output().ok()?;
    let data: FfprobeFramerate = serde_json::from_slice(&output.stdout).ok()?;
    let rate = data.streams.into_iter().next()?.r_frame_rate;
    let mut parts = rate.splitn(2, '/');
    let num = parts.next()?.parse::<u32>().ok()?;
    let den = parts.next()?.parse::<u32>().ok()?;
    Some((num, den))
}

pub fn ffprobe_samplerate(path: &str) -> Option<u32> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "a:0",
            "-show_entries", "stream=sample_rate",
            "-of", "json",
            path,
        ])
        .output().ok()?;
    let data: FfprobeSamplerate = serde_json::from_slice(&output.stdout).ok()?;
    data.streams.into_iter().next()?.sample_rate.parse::<u32>().ok()
}

pub fn ffprobe_duration_centiseconds(path: &str) -> Option<u64> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
            path,
        ])
        .output()
        .ok()?;
    duration_to_centiseconds(String::from_utf8(output.stdout).ok()?.trim())
}

fn duration_to_centiseconds(value: &str) -> Option<u64> {
    let seconds = value.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let centiseconds = (seconds * 100.0).ceil() as u64;
    if centiseconds > 255 * 360_000 + 59 * 6_000 + 59 * 100 + 99 {
        return None;
    }
    Some(centiseconds)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MediaProbe {
    pub duration_ms: u64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
}

pub fn ffprobe_duration_millis(path: &Path) -> Option<u64> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
            &path.to_string_lossy(),
        ])
        .output().ok()?;
    let seconds = String::from_utf8(output.stdout).ok()?.trim().parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some((seconds * 1000.0).ceil() as u64)
}

pub fn ffprobe_dimensions(path: &Path) -> Option<(u32, u32)> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=s=x:p=0",
            &path.to_string_lossy(),
        ])
        .output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.trim().split('x');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

pub fn ffprobe_has_audio_stream(path: &Path) -> bool {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "a:0",
            "-show_entries", "stream=index",
            "-of", "csv=p=0",
            &path.to_string_lossy(),
        ])
        .output();
    output.map(|out| out.status.success() && !out.stdout.is_empty()).unwrap_or(false)
}

pub fn ffprobe_media(path: &Path) -> Option<MediaProbe> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-show_entries", "format=duration:stream=codec_type,width,height,r_frame_rate",
            "-of", "json",
            &path.to_string_lossy(),
        ])
        .output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let streams = value.get("streams")?.as_array()?;
    let video = streams.iter().find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"));
    let audio = streams.iter().any(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"));
    let duration = value.get("format")?.get("duration")?.as_str()
        .or_else(|| value.get("format")?.get("duration")?.as_f64().map(|_| ""))?;
    let duration_ms = if duration.is_empty() {
        value.get("format")?.get("duration")?.as_f64().and_then(|d| {
            if d.is_finite() && d > 0.0 { Some((d * 1000.0).ceil() as u64) } else { None }
        })?
    } else {
        let d = duration.parse::<f64>().ok()?;
        if !d.is_finite() || d <= 0.0 { return None; }
        (d * 1000.0).ceil() as u64
    };
    let (fps_num, fps_den) = video
        .and_then(|s| s.get("r_frame_rate").and_then(|v| v.as_str()))
        .and_then(|rate| {
            let mut p = rate.split('/');
            Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?))
        })
        .unwrap_or((0, 1));
    Some(MediaProbe {
        duration_ms,
        fps_num,
        fps_den,
        width: video.and_then(|s| s.get("width").and_then(|v| v.as_u64())).unwrap_or(0) as u32,
        height: video.and_then(|s| s.get("height").and_then(|v| v.as_u64())).unwrap_or(0) as u32,
        has_video: video.is_some(),
        has_audio: audio,
    })
}

pub async fn probe_media(path: PathBuf) -> Result<MediaProbe, String> {
    tokio::task::spawn_blocking(move || ffprobe_media(&path).ok_or_else(|| {
        format!("ffprobe could not decode `{}`", path.display())
    }))
    .await
    .map_err(|e| e.to_string())?
}

pub fn ffprobe_video_height(path: &str) -> Option<u32> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=height",
            "-of", "csv=p=0",
            path,
        ])
        .output().ok()?;

    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.trim().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::duration_to_centiseconds;

    #[test]
    fn duration_rounds_up_to_ass_centiseconds() {
        assert_eq!(duration_to_centiseconds("61.2301"), Some(6124));
        assert_eq!(duration_to_centiseconds("61.23"), Some(6123));
    }

    #[test]
    fn duration_rejects_invalid_or_unrepresentable_values() {
        assert_eq!(duration_to_centiseconds("0"), None);
        assert_eq!(duration_to_centiseconds("NaN"), None);
        assert_eq!(duration_to_centiseconds("91800000"), None);
    }
}
