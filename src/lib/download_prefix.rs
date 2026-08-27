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
