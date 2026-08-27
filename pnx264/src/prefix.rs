// Streams only the contiguous byte prefix authorised by Pandora's download sidecar. Torrent
// targets are preallocated, so reading to the source's apparent length would feed unwritten zeroes
// to ffmpeg and make a truncated Matroska prefix look corrupt.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

const VERSION: &str = "PNPREFIX1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixState {
    pub source: PathBuf,
    pub available: u64,
    pub total: u64,
    pub complete: bool,
}

impl PrefixState {
    pub fn read(path: &Path) -> Result<Self, String> {
        let value = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut lines = value.lines();
        if lines.next() != Some(VERSION) {
            return Err("unsupported download prefix state".to_string());
        }
        let available: u64 = lines
            .next()
            .ok_or("prefix state has no available byte count")?
            .parse()
            .map_err(|_| "invalid available byte count")?;
        let total: u64 = lines
            .next()
            .ok_or("prefix state has no total byte count")?
            .parse()
            .map_err(|_| "invalid total byte count")?;
        let complete = match lines.next() {
            Some("0") => false,
            Some("1") => true,
            _ => return Err("invalid prefix completion flag".to_string()),
        };
        let source = PathBuf::from(lines.next().ok_or("prefix state has no source path")?);
        if source.as_os_str().is_empty() {
            return Err("prefix state source path is empty".to_string());
        }
        if total != 0 && available > total {
            return Err("available prefix exceeds total bytes".to_string());
        }
        Ok(Self { source, available, total, complete })
    }
}

// Waits for the producer to create its first atomic state, without treating a not-yet-created
// sidecar as a failed optimisation.
pub fn wait_for_state(path: &Path) -> Result<PrefixState, String> {
    loop {
        match PrefixState::read(path) {
            Ok(state) => return Ok(state),
            Err(_) if !path.exists() => sleep(Duration::from_millis(50)),
            Err(e) => return Err(e),
        }
    }
}

// Copies a growing source into a pipe without ever presenting an artificial mid-download EOF.
// ffmpeg remains one continuous demux/decode process, preserving decoder state across every prefix
// advance. EOF is sent only after the producer marks the selected file complete.
pub fn open_source(state_path: &Path, first: &PrefixState) -> Result<File, String> {
    loop {
        match File::open(&first.source) {
            Ok(file) => return Ok(file),
            Err(e) => {
                let state = PrefixState::read(state_path)?;
                if state.complete {
                    return Err(e.to_string());
                }
                sleep(Duration::from_millis(50));
            }
        }
    }
}

pub fn stream_open_file_to_gated<W: Write, F: FnMut() -> Result<(), String>>(
    state_path: &Path,
    source_path: &Path,
    mut source: File,
    mut output: W,
    mut wait_until_allowed: F,
) -> Result<u64, String> {
    let mut sent = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        wait_until_allowed()?;
        let state = PrefixState::read(state_path)?;
        if state.source != source_path {
            return Err("download prefix source changed while streaming".to_string());
        }
        if state.available < sent {
            return Err("download prefix moved backwards".to_string());
        }
        while sent < state.available {
            wait_until_allowed()?;
            let wanted = (state.available - sent).min(buffer.len() as u64) as usize;
            source.seek(SeekFrom::Start(sent)).map_err(|e| e.to_string())?;
            source.read_exact(&mut buffer[..wanted]).map_err(|e| e.to_string())?;
            output.write_all(&buffer[..wanted]).map_err(|e| e.to_string())?;
            sent += wanted as u64;
        }
        output.flush().map_err(|e| e.to_string())?;
        if state.complete {
            return Ok(sent);
        }
        sleep(Duration::from_millis(50));
    }
}

pub fn stream_open_file_to<W: Write>(
    state_path: &Path,
    source_path: &Path,
    source: File,
    output: W,
) -> Result<u64, String> {
    stream_open_file_to_gated(state_path, source_path, source, output, || Ok(()))
}

pub fn stream_to<W: Write>(state_path: &Path, output: W) -> Result<u64, String> {
    let first = wait_for_state(state_path)?;
    let source = open_source(state_path, &first)?;
    stream_open_file_to(state_path, &first.source, source, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_worker_sidecar_format() {
        let path = std::env::temp_dir().join(format!("pnx264-prefix-{}", std::process::id()));
        std::fs::write(&path, "PNPREFIX1\n12\n20\n0\nvideo file.mkv\n").unwrap();
        assert_eq!(
            PrefixState::read(&path).unwrap(),
            PrefixState {
                source: PathBuf::from("video file.mkv"),
                available: 12,
                total: 20,
                complete: false,
            }
        );
        std::fs::remove_file(path).ok();
    }
}
