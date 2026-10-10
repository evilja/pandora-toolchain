use std::path::{Path, PathBuf};

const VERSION: &str = "PNPREFIX1";

// A downloaded file may already have its final apparent length (torrent storage preallocates it),
// so consumers must never infer readability from metadata. This sidecar is the authority for the
// contiguous, verified byte prefix that can safely be streamed to a decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPrefixState {
    pub source: PathBuf,
    pub available: u64,
    // Zero means the server did not provide a content length yet.
    pub total: u64,
    pub complete: bool,
}

impl DownloadPrefixState {
    pub fn encode(&self) -> Result<String, String> {
        let source = self.source.to_string_lossy();
        if source.contains('\n') || source.contains('\r') {
            return Err("prefix source path contains a newline".to_string());
        }
        if self.total != 0 && self.available > self.total {
            return Err("available prefix exceeds total bytes".to_string());
        }
        Ok(format!(
            "{VERSION}\n{}\n{}\n{}\n{}\n",
            self.available,
            self.total,
            u8::from(self.complete),
            source,
        ))
    }

    pub fn decode(value: &str) -> Result<Self, String> {
        let mut lines = value.lines();
        if lines.next() != Some(VERSION) {
            return Err("unsupported download prefix state".to_string());
        }
        let available = lines
            .next()
            .ok_or("prefix state has no available byte count")?
            .parse()
            .map_err(|_| "invalid available byte count")?;
        let total = lines
            .next()
            .ok_or("prefix state has no total byte count")?
            .parse()
            .map_err(|_| "invalid total byte count")?;
        let complete = match lines.next() {
            Some("0") => false,
            Some("1") => true,
            _ => return Err("invalid prefix completion flag".to_string()),
        };
        let source = lines.next().ok_or("prefix state has no source path")?;
        if source.is_empty() {
            return Err("prefix state source path is empty".to_string());
        }
        if total != 0 && available > total {
            return Err("available prefix exceeds total bytes".to_string());
        }
        Ok(Self {
            source: PathBuf::from(source),
            available,
            total,
            complete,
        })
    }
}

// One writer owns a job's state file. Rename keeps readers from observing a partially rewritten
// byte count while ffmpeg is being fed from the source.
pub fn write_download_prefix(path: &Path, state: &DownloadPrefixState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let cutoff = path.with_file_name("linear-aot.download");
    if state.complete {
        if let Err(e) = pnx264::linear::freeze_download_metrics(&path.with_file_name("linear-aot.state"), &cutoff) {
            eprintln!("[metrics] could not freeze download-time AOT progress: {e}");
        }
    } else {
        // A fresh/retried download cannot retain the preceding attempt's cutoff.
        std::fs::remove_file(cutoff).ok();
    }
    let tmp = path.with_extension(format!(
        "prefix-tmp-{}",
        std::process::id(),
    ));
    std::fs::write(&tmp, state.encode()?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn read_download_prefix(path: &Path) -> Result<DownloadPrefixState, String> {
    let value = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    DownloadPrefixState::decode(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_freezes_aot_before_completion_and_resets_on_retry() {
        let root = std::env::temp_dir().join(format!("pandora-prefix-cutoff-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let prefix = root.join("download.prefix");
        let linear = root.join("linear-aot.state");
        let cutoff = root.join("linear-aot.download");
        let mut state = DownloadPrefixState {
            source: root.join("input.mkv"), available: 12, total: 20, complete: false,
        };
        write_download_prefix(&prefix, &state).unwrap();
        assert!(!cutoff.exists());
        std::fs::write(&linear, "PNLINEAR2\ncomplete\n123\n42\n1200\n9000\n50000000\nstandard-v1\n1000\n3000\n").unwrap();
        state.available = 20;
        state.complete = true;
        write_download_prefix(&prefix, &state).unwrap();
        assert!(read_download_prefix(&prefix).unwrap().complete);
        assert_eq!(std::fs::read_to_string(&cutoff).unwrap().trim(), "123 42 1000 1200 3000");
        std::fs::write(&linear, "PNLINEAR2\ncomplete\n123\n42\n3600\n27000\n150000000\nstandard-v1\n1000\n20000\n").unwrap();
        write_download_prefix(&prefix, &state).unwrap();
        assert_eq!(std::fs::read_to_string(&cutoff).unwrap().trim(), "123 42 1000 1200 3000");
        state.complete = false;
        state.available = 0;
        write_download_prefix(&prefix, &state).unwrap();
        assert!(!cutoff.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_round_trips_paths_with_spaces() {
        let state = DownloadPrefixState {
            source: PathBuf::from("DB/work/7/episode 01.mkv"),
            available: 123,
            total: 456,
            complete: false,
        };
        assert_eq!(DownloadPrefixState::decode(&state.encode().unwrap()).unwrap(), state);
    }

    #[test]
    fn state_rejects_preallocated_bytes_past_the_total() {
        let value = "PNPREFIX1\n457\n456\n0\ninput.mkv\n";
        assert!(DownloadPrefixState::decode(value).is_err());
    }
}
