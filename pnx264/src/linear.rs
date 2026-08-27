use crate::Config;
use crate::prefix;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

const VERSION: &str = "PNLINEAR1";

#[derive(Clone, Debug)]
pub struct LinearAotConfig {
    pub ffmpeg: PathBuf,
    pub prefix_state: PathBuf,
    pub output: PathBuf,
    pub state: PathBuf,
    pub subtitle: PathBuf,
    pub cancel_file: Option<PathBuf>,
    pub busy_file: Option<PathBuf>,
    pub lease_file: Option<PathBuf>,
    pub job_id: u64,
    pub compatibility: String,
    pub encoder: Config,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearAotState {
    pub complete: bool,
    pub pid: u32,
    pub job_id: u64,
    pub frames: u64,
    pub bytes: u64,
    pub compatibility: String,
}

impl LinearAotState {
    pub fn read(path: &Path) -> Result<Self, String> {
        let value = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut lines = value.lines();
        if lines.next() != Some(VERSION) {
            return Err("unsupported linear AOT state".to_string());
        }
        let complete = match lines.next() {
            Some("running") => false,
            Some("complete") => true,
            _ => return Err("invalid linear AOT status".to_string()),
        };
        let pid = lines.next().ok_or("linear AOT state has no pid")?.parse().map_err(|_| "invalid linear AOT pid")?;
        let job_id = lines.next().ok_or("linear AOT state has no job id")?.parse().map_err(|_| "invalid linear AOT job id")?;
        let frames = lines.next().ok_or("linear AOT state has no frame count")?.parse().map_err(|_| "invalid linear AOT frame count")?;
        let bytes = lines.next().ok_or("linear AOT state has no byte count")?.parse().map_err(|_| "invalid linear AOT byte count")?;
        let compatibility = lines.next().ok_or("linear AOT state has no compatibility key")?.to_string();
        Ok(Self { complete, pid, job_id, frames, bytes, compatibility })
    }

    pub fn process_alive(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            PathBuf::from("/proc").join(self.pid.to_string()).exists()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }
}

fn publish(path: &Path, state: &LinearAotState) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".linear-aot-state.{}.tmp", std::process::id()));
    let status = if state.complete { "complete" } else { "running" };
    std::fs::write(
        &temporary,
        format!(
            "{VERSION}\n{status}\n{}\n{}\n{}\n{}\n{}\n",
            state.pid, state.job_id, state.frames, state.bytes, state.compatibility,
        ),
    ).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| e.to_string())
}

fn live_marker(path: &Path) -> Option<(u32, Option<u64>)> {
    let value = std::fs::read_to_string(path).ok()?;
    let mut fields = value.trim().split('|');
    let pid = fields.next()?.parse::<u32>().ok()?;
    #[cfg(target_os = "linux")]
    if !PathBuf::from("/proc").join(pid.to_string()).exists() {
        std::fs::remove_file(path).ok();
        return None;
    }
    let job_id = fields.next().and_then(|value| value.parse().ok());
    Some((pid, job_id))
}

struct IdleLease {
    path: Option<PathBuf>,
    owned: bool,
}

impl IdleLease {
    fn new(path: Option<PathBuf>) -> Self {
        Self { path, owned: false }
    }

    fn acquire(&mut self) -> bool {
        if self.owned {
            return true;
        }
        let Some(path) = self.path.as_ref() else {
            self.owned = true;
            return true;
        };
        for _ in 0..2 {
            match OpenOptions::new().create_new(true).write(true).open(path) {
                Ok(mut file) => {
                    if writeln!(file, "{}", std::process::id()).is_ok() {
                        self.owned = true;
                        return true;
                    }
                    std::fs::remove_file(path).ok();
                    return false;
                }
                Err(_) if live_marker(path).is_none() => continue,
                Err(_) => return false,
            }
        }
        false
    }

    fn release(&mut self) {
        if !self.owned {
            return;
        }
        if let Some(path) = self.path.as_ref() {
            if live_marker(path).is_some_and(|(pid, _)| pid == std::process::id()) {
                std::fs::remove_file(path).ok();
            }
        }
        self.owned = false;
    }
}

impl Drop for IdleLease {
    fn drop(&mut self) {
        self.release();
    }
}

fn wait_for_turn(config: &LinearAotConfig, lease: &mut IdleLease) -> Result<(), String> {
    loop {
        if config.cancel_file.as_ref().is_some_and(|path| path.exists()) {
            return Err("cancelled".to_string());
        }
        let blocked = config.busy_file.as_deref().and_then(live_marker)
            .is_some_and(|(_, owner)| owner != Some(config.job_id));
        if blocked {
            lease.release();
        } else if lease.acquire() {
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
}

fn filter_quote(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' | '\'' | ':' | ',' | '[' | ']' | ';' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    format!("'{out}'")
}

pub fn run_linear_aot(config: LinearAotConfig) -> Result<LinearAotState, String> {
    let state = LinearAotState {
        complete: false,
        pid: std::process::id(),
        job_id: config.job_id,
        frames: 0,
        bytes: 0,
        compatibility: config.compatibility.clone(),
    };
    publish(&config.state, &state)?;
    let first = prefix::wait_for_state(&config.prefix_state)?;
    // Hold the original torrent inode before waiting for the idle lease. The download worker may
    // rename the selected file to input.mkv at handoff; this descriptor remains valid.
    let source_file = prefix::open_source(&config.prefix_state, &first)?;
    let mut lease = IdleLease::new(config.lease_file.clone());
    wait_for_turn(&config, &mut lease)?;

    let subtitle = std::fs::canonicalize(&config.subtitle).unwrap_or_else(|_| config.subtitle.clone());
    let filter = format!("ass={},format=yuv420p", filter_quote(&subtitle.to_string_lossy()));
    let parent = config.output.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".linear-aot.{}.part", std::process::id()));
    std::fs::remove_file(&temporary).ok();
    std::fs::remove_file(&config.output).ok();

    let mut command = Command::new(&config.ffmpeg);
    command
        .args(["-v", "error", "-i", "pipe:0", "-map", "0:v:0", "-an", "-sn", "-vf", &filter,
            "-c:v", "libx264", "-crf", &config.encoder.crf.to_string()]);
    if let Some(value) = config.encoder.preset.as_ref() {
        command.args(["-preset", value]);
    }
    if let Some(value) = config.encoder.tune.as_ref() {
        command.args(["-tune", value]);
    }
    if let Some(value) = config.encoder.profile.as_ref() {
        command.args(["-profile:v", value]);
    }
    if let Some(value) = config.encoder.level.as_ref() {
        command.args(["-level:v", value]);
    }
    if let Some(value) = config.encoder.x264_params.as_ref() {
        command.args(["-x264-params", value]);
    }
    command
        .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart", "-progress", "pipe:1", "-nostats",
            "-f", "mp4", "-y"])
        .arg(&temporary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut encoder = command.spawn().map_err(|e| format!("spawn linear AOT ffmpeg: {e}"))?;
    let stdin = encoder.stdin.take().ok_or("linear AOT ffmpeg has no stdin")?;
    let stdout = encoder.stdout.take().ok_or("linear AOT ffmpeg has no progress output")?;

    let shared_state = Arc::new(Mutex::new(state));
    let progress_state = shared_state.clone();
    let progress_path = config.state.clone();
    let progress = std::thread::spawn(move || -> Result<(), String> {
        let mut frames = None;
        let mut bytes = None;
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|e| e.to_string())?;
            if let Some(value) = line.strip_prefix("frame=") {
                frames = value.trim().parse::<u64>().ok();
            } else if let Some(value) = line.strip_prefix("total_size=") {
                bytes = value.trim().parse::<u64>().ok();
            } else if line == "progress=continue" || line == "progress=end" {
                let mut state = progress_state.lock().map_err(|_| "linear AOT state lock poisoned")?;
                if let Some(value) = frames {
                    state.frames = value;
                }
                if let Some(value) = bytes {
                    state.bytes = value;
                }
                publish(&progress_path, &state)?;
            }
        }
        Ok(())
    });

    let source_path = first.source.clone();
    let streamed = prefix::stream_open_file_to_gated(
        &config.prefix_state,
        &source_path,
        source_file,
        stdin,
        || wait_for_turn(&config, &mut lease),
    );
    if let Err(error) = streamed {
        encoder.kill().ok();
        encoder.wait().ok();
        progress.join().ok();
        std::fs::remove_file(&temporary).ok();
        return Err(error);
    }
    let source_bytes = streamed.unwrap();
    let status = encoder.wait().map_err(|e| e.to_string())?;
    progress.join().map_err(|_| "linear AOT progress reader panicked".to_string())??;
    if !status.success() {
        std::fs::remove_file(&temporary).ok();
        return Err(format!("linear AOT ffmpeg failed with {status}"));
    }
    if first.total != 0 && source_bytes != first.total {
        std::fs::remove_file(&temporary).ok();
        return Err(format!("linear AOT consumed {source_bytes}/{} source bytes", first.total));
    }
    std::fs::rename(&temporary, &config.output).map_err(|e| e.to_string())?;
    let mut state = shared_state.lock().map_err(|_| "linear AOT state lock poisoned")?.clone();
    state.complete = true;
    state.bytes = std::fs::metadata(&config.output).map(|value| value.len()).unwrap_or(state.bytes);
    publish(&config.state, &state)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        let root = std::env::temp_dir().join(format!("pnx264-linear-state-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state");
        let state = LinearAotState {
            complete: false,
            pid: std::process::id(),
            job_id: 42,
            frames: 1200,
            bytes: 9000,
            compatibility: "standard-v1".to_string(),
        };
        publish(&path, &state).unwrap();
        assert_eq!(LinearAotState::read(&path).unwrap(), state);
        std::fs::remove_dir_all(root).ok();
    }
}
