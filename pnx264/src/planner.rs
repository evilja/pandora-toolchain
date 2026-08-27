use crate::prefix;
use crate::y4m::Y4mReader;
use crate::{Config, Encoder, PlanEntry};
use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

pub struct PrefixPlannerConfig {
    pub ffmpeg: PathBuf,
    pub executable: PathBuf,
    pub prefix_state: PathBuf,
    pub output: PathBuf,
    pub subtitle: PathBuf,
    pub cancel_file: Option<PathBuf>,
    pub busy_file: Option<PathBuf>,
    pub lease_file: Option<PathBuf>,
    pub workers: usize,
    pub encoder: Config,
}

#[derive(Clone, Debug)]
pub struct PlannerSummary {
    pub planned: u64,
    pub submitted: u64,
    pub idrs: u64,
    pub source_bytes: u64,
    pub aot_chunks: usize,
}

fn fixed_ranges_through(next: &mut u64, planned_exclusive: u64) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    while next.saturating_add(crate::parallel::FIXED_STRIDE) <= planned_exclusive {
        ranges.push((*next, crate::parallel::FIXED_STRIDE));
        *next += crate::parallel::FIXED_STRIDE;
    }
    ranges
}

struct AotScheduler {
    executable: PathBuf,
    input: PathBuf,
    subtitle: PathBuf,
    directory: PathBuf,
    stop_file: PathBuf,
    cancel_file: Option<PathBuf>,
    busy_file: Option<PathBuf>,
    lease_file: Option<PathBuf>,
    owns_lease: bool,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    encoder: Config,
    max_workers: usize,
    fixed_chunks: usize,
    launched: usize,
    pending: VecDeque<(u64, u64)>,
    scheduled: HashSet<(u64, u64)>,
    children: Vec<Child>,
}

impl AotScheduler {
    fn marker_has_live_process(path: &Path) -> bool {
        let Ok(value) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(pid) = value.trim().split('|').next().unwrap_or("").parse::<u32>() else {
            std::fs::remove_file(path).ok();
            return false;
        };
        #[cfg(target_os = "linux")]
        {
            if PathBuf::from("/proc").join(pid.to_string()).exists() {
                return true;
            }
            std::fs::remove_file(path).ok();
            false
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            true
        }
    }

    fn foreground_busy(&self) -> bool {
        self.busy_file.as_deref().is_some_and(Self::marker_has_live_process)
    }

    fn release_lease(&mut self) {
        if !self.owns_lease {
            return;
        }
        if let Some(path) = self.lease_file.as_ref() {
            let owned = std::fs::read_to_string(path)
                .ok()
                .and_then(|value| value.trim().split('|').next()?.parse::<u32>().ok())
                == Some(std::process::id());
            if owned {
                std::fs::remove_file(path).ok();
            }
        }
        self.owns_lease = false;
    }

    fn acquire_lease(&mut self) -> bool {
        if self.owns_lease {
            return true;
        }
        let Some(path) = self.lease_file.as_ref() else {
            self.owns_lease = true;
            return true;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        for _ in 0..2 {
            match OpenOptions::new().create_new(true).write(true).open(path) {
                Ok(mut file) => {
                    if writeln!(file, "{}", std::process::id()).is_ok() {
                        self.owns_lease = true;
                        return true;
                    }
                    std::fs::remove_file(path).ok();
                    return false;
                }
                Err(_) if !Self::marker_has_live_process(path) => continue,
                Err(_) => return false,
            }
        }
        false
    }

    fn reap(&mut self) {
        self.children.retain_mut(|child| match child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(_) => false,
        });
    }

    fn wait_for_turn(&mut self) -> bool {
        while !self.stopped() {
            self.reap();
            if self.foreground_busy() {
                self.release_lease();
            } else if self.acquire_lease() {
                return true;
            }
            sleep(Duration::from_millis(100));
        }
        false
    }

    fn enqueue(&mut self, start: u64, frames: u64, fixed: bool) {
        if frames == 0 {
            return;
        }
        if fixed {
            self.fixed_chunks += 1;
        }
        if self.scheduled.insert((start, frames)) {
            self.pending.push_back((start, frames));
        }
        self.pump();
    }

    fn enqueue_fixed_through(&mut self, next: &mut u64, planned_exclusive: u64) {
        for (start, frames) in fixed_ranges_through(next, planned_exclusive) {
            self.enqueue(start, frames, true);
        }
    }

    fn stopped(&self) -> bool {
        self.stop_file.exists() || self.cancel_file.as_ref().is_some_and(|path| path.exists())
    }

    fn pump(&mut self) {
        self.reap();
        if self.stopped() {
            self.pending.clear();
            return;
        }
        if self.foreground_busy() {
            self.release_lease();
            return;
        }
        if !self.owns_lease {
            return;
        }
        // Fixed ranges are the always-legal fallback, so they—not the combined natural+fixed
        // candidate count—govern the four-chunks-per-worker ramp.
        let active_limit = (self.fixed_chunks / 4).min(self.max_workers);
        while self.children.len() < active_limit {
            let Some((start, frames)) = self.pending.pop_front() else {
                break;
            };
            let output = crate::parallel::aot_chunk_path(&self.directory, start, frames);
            let mut command = Command::new(&self.executable);
            command
                .arg("--aot-chunk")
                .args(["--input", &self.input.to_string_lossy()])
                .args(["--output", &output.to_string_lossy()])
                .args(["--ass", &self.subtitle.to_string_lossy()])
                .args(["--aot-stopfile", &self.stop_file.to_string_lossy()])
                .args(["--start-frame", &start.to_string()])
                .args(["--frame-count", &frames.to_string()])
                .args(["--video-width", &self.width.to_string()])
                .args(["--video-height", &self.height.to_string()])
                .args(["--fps-num", &self.fps_num.to_string()])
                .args(["--fps-den", &self.fps_den.to_string()])
                .args(["--aot-crf", &self.encoder.crf.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            if let Some(path) = self.cancel_file.as_ref() {
                command.args(["--cancelfile", &path.to_string_lossy()]);
            }
            if let Some(path) = self.busy_file.as_ref() {
                command.args(["--aot-busyfile", &path.to_string_lossy()]);
            }
            if let Some(value) = self.encoder.preset.as_ref() {
                command.args(["--aot-preset", value]);
            }
            if let Some(value) = self.encoder.tune.as_ref() {
                command.args(["--aot-tune", value]);
            }
            if let Some(value) = self.encoder.profile.as_ref() {
                command.args(["--aot-profile", value]);
            }
            if let Some(value) = self.encoder.level.as_ref() {
                command.args(["--aot-level", value]);
            }
            if let Some(value) = self.encoder.x264_params.as_ref() {
                command.args(["--aot-x264-params", value]);
            }
            match command.spawn() {
                Ok(child) => {
                    self.children.push(child);
                    self.launched += 1;
                }
                Err(_) => {
                    // AOT is disposable. Stop launching, but never invalidate the boundary plan.
                    self.pending.clear();
                    break;
                }
            }
        }
    }

    fn finish(&mut self) {
        if self.fixed_chunks < 4 {
            self.pending.clear();
            return;
        }
        while (!self.pending.is_empty() || !self.children.is_empty()) && !self.stopped() {
            self.pump();
            sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for AotScheduler {
    fn drop(&mut self) {
        self.release_lease();
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

fn record_entry(
    entry: PlanEntry,
    plan: &mut impl Write,
    scheduler: &mut AotScheduler,
    planned: &mut u64,
    last_planned_pts: &mut u64,
    idrs: &mut u64,
    last_idr: &mut Option<u64>,
    submitted: u64,
) -> Result<(), String> {
    *planned += 1;
    let pts = entry.pts.max(0) as u64;
    *last_planned_pts = (*last_planned_pts).max(pts);
    if entry.is_idr != 0 {
        writeln!(plan, "idr|{pts}").map_err(|e| e.to_string())?;
        plan.flush().map_err(|e| e.to_string())?;
        *idrs += 1;
        if let Some(start) = last_idr.replace(pts) {
            scheduler.enqueue(start, pts.saturating_sub(start), false);
        }
    } else if planned.is_multiple_of(250) {
        writeln!(plan, "progress|{last_planned_pts}|{submitted}").map_err(|e| e.to_string())?;
        plan.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// One ffmpeg process consumes the growing Matroska stream. The feeder blocks instead of closing
// stdin at temporary prefix boundaries, so demux, decode, libass, and x264 lookahead state all stay
// continuous. Closed natural and fixed-stride ranges are offered to disposable subprocesses.
// Natural chunks require a completed plan; fixed chunks remain reusable by the established fallback.
pub fn run_prefix_planner(config: PrefixPlannerConfig) -> Result<PlannerSummary, String> {
    let first = prefix::wait_for_state(&config.prefix_state)?;
    let subtitle = std::fs::canonicalize(&config.subtitle).unwrap_or(config.subtitle);
    let filter = format!("ass={},format=yuv420p", filter_quote(&subtitle.to_string_lossy()));
    let mut child = Command::new(&config.ffmpeg)
        .args([
            "-v", "error", "-i", "pipe:0", "-map", "0:v:0", "-an", "-sn", "-vf", &filter,
            "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg prefix decoder: {e}"))?;
    let stdin = child.stdin.take().ok_or("ffmpeg prefix decoder has no stdin")?;
    let state_for_feeder = config.prefix_state.clone();
    let feeder = std::thread::spawn(move || prefix::stream_to(&state_for_feeder, stdin));

    let stdout = child.stdout.take().ok_or("ffmpeg prefix decoder has no stdout")?;
    let mut y4m = Y4mReader::new(BufReader::with_capacity(1 << 20, stdout))?;
    let mut encoder_config = config.encoder;
    encoder_config.width = y4m.width as u32;
    encoder_config.height = y4m.height as u32;
    encoder_config.fps_num = y4m.fps_num;
    encoder_config.fps_den = y4m.fps_den;
    encoder_config.plan_only = true;
    let mut encoder = Encoder::open(&encoder_config)?;

    let aot_directory = crate::parallel::aot_directory(&config.output);
    std::fs::remove_dir_all(&aot_directory).ok();
    std::fs::create_dir_all(&aot_directory).map_err(|e| e.to_string())?;
    let mut scheduler = AotScheduler {
        executable: config.executable,
        input: first.source,
        subtitle,
        stop_file: aot_directory.join("STOP"),
        directory: aot_directory,
        cancel_file: config.cancel_file,
        busy_file: config.busy_file,
        lease_file: config.lease_file,
        owns_lease: false,
        width: y4m.width as u32,
        height: y4m.height as u32,
        fps_num: y4m.fps_num,
        fps_den: y4m.fps_den,
        encoder: encoder_config,
        max_workers: crate::parallel::memory_worker_limit(config.workers.max(1)),
        fixed_chunks: 0,
        launched: 0,
        pending: VecDeque::new(),
        scheduled: HashSet::new(),
        children: Vec::new(),
    };

    let mut plan = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&config.output)
        .map_err(|e| e.to_string())?;
    plan.write_all(b"PNPLAN1\n").map_err(|e| e.to_string())?;
    plan.flush().map_err(|e| e.to_string())?;

    let mut submitted = 0u64;
    let mut planned = 0u64;
    let mut last_planned_pts = 0u64;
    let mut idrs = 0u64;
    let mut last_idr = None;
    let mut next_fixed = 0u64;
    loop {
        if !scheduler.wait_for_turn() {
            return Err("cancelled".to_string());
        }
        let Some(frame) = y4m.next_frame()? else {
            break;
        };
        if let Some(entry) = encoder.plan_push(
            frame.y,
            frame.u,
            frame.v,
            frame.stride_y,
            frame.stride_c,
            frame.stride_c,
            submitted as i64,
        )? {
            record_entry(
                entry,
                &mut plan,
                &mut scheduler,
                &mut planned,
                &mut last_planned_pts,
                &mut idrs,
                &mut last_idr,
                submitted + 1,
            )?;
            scheduler.enqueue_fixed_through(&mut next_fixed, last_planned_pts + 1);
        }
        scheduler.pump();
        submitted += 1;
    }
    while let Some(entry) = encoder.plan_flush()? {
        record_entry(
            entry,
            &mut plan,
            &mut scheduler,
            &mut planned,
            &mut last_planned_pts,
            &mut idrs,
            &mut last_idr,
            submitted,
        )?;
        scheduler.enqueue_fixed_through(&mut next_fixed, last_planned_pts + 1);
    }

    let source_bytes = feeder.join().map_err(|_| "prefix feeder panicked".to_string())??;
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ffmpeg prefix decoder failed with {status}"));
    }
    scheduler.enqueue_fixed_through(&mut next_fixed, submitted);
    if next_fixed < submitted {
        scheduler.enqueue(next_fixed, submitted - next_fixed, true);
    }
    if let Some(start) = last_idr {
        scheduler.enqueue(start, submitted.saturating_sub(start), false);
    }
    writeln!(plan, "complete|{planned}|{submitted}").map_err(|e| e.to_string())?;
    plan.flush().map_err(|e| e.to_string())?;

    scheduler.finish();
    Ok(PlannerSummary {
        planned,
        submitted,
        idrs,
        source_bytes,
        aot_chunks: scheduler.launched,
    })
}

pub fn read_partial_plan(path: &Path) -> Result<crate::plan::BoundaryPlan, String> {
    crate::plan::BoundaryPlan::read(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn live_pid_markers_block_and_crash_stale_markers_are_removed() {
        let root = std::env::temp_dir().join(format!("pnx264-aot-marker-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("marker");
        std::fs::write(&marker, format!("{}\n", std::process::id())).unwrap();
        assert!(AotScheduler::marker_has_live_process(&marker));
        std::fs::write(&marker, format!("{}\n", u32::MAX)).unwrap();
        assert!(!AotScheduler::marker_has_live_process(&marker));
        assert!(!marker.exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fixed_candidates_stop_at_the_last_frame_proven_by_lookahead() {
        let mut next = 0;
        assert_eq!(
            fixed_ranges_through(&mut next, 749),
            vec![(0, 250), (250, 250)],
        );
        assert_eq!(next, 500);
        assert_eq!(fixed_ranges_through(&mut next, 1000), vec![(500, 250), (750, 250)]);
        assert_eq!(next, 1000);
    }
}
