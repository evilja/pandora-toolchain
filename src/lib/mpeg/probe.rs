use std::process::Command;
use serde::Deserialize;
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
