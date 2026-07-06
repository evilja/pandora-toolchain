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
