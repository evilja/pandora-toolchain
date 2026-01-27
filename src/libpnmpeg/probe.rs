use std::process::Command;
use serde::Deserialize;

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


pub fn ffprobe(path: &str, f_lang: &str) -> Option<u32> {
    let output = Command::new("ffprobe")
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
