use crate::y4m::Y4mReader;
use crate::{Config, Encoder};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub const FIXED_STRIDE: u64 = 250;
const MAX_PLAN_TAIL: u64 = 500;

#[derive(Clone, Debug)]
pub struct ParallelConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub subtitle: PathBuf,
    pub cancel_file: Option<PathBuf>,
    pub plan: Option<PathBuf>,
    pub workers: usize,
    pub encoder: Config,
    pub audio_map: String,
}

#[derive(Clone, Debug)]
pub struct ParallelProgress {
    pub frames: u64,
    pub total_frames: u64,
    pub fps: f64,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ParallelSummary {
    pub frames: u64,
    pub chunks: usize,
    pub workers: usize,
    pub reused_chunks: usize,
    pub natural_boundaries: bool,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct AotChunkConfig {
    pub ffmpeg: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub subtitle: PathBuf,
    pub cancel_file: Option<PathBuf>,
    pub stop_file: PathBuf,
    pub busy_file: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub start: u64,
    pub frames: u64,
    pub encoder: Config,
}

#[derive(Clone, Debug)]
struct Source {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    frames: u64,
}

enum WorkerEvent {
    Progress,
    Done,
    Error(String),
}

pub fn aot_directory(plan: &Path) -> PathBuf {
    plan.parent().unwrap_or_else(|| Path::new(".")).join("parallel-aot")
}

pub fn aot_chunk_path(directory: &Path, start: u64, frames: u64) -> PathBuf {
    directory.join(format!("chunk-{start:012}-{frames:012}.264"))
}

fn probe(ffprobe: &Path, input: &Path) -> Result<Source, String> {
    let output = Command::new(ffprobe)
        .args([
            "-v", "error", "-select_streams", "v:0", "-count_frames",
            "-show_entries", "stream=width,height,r_frame_rate,nb_read_frames",
            "-of", "default=nw=1:nk=1",
        ])
        .arg(input)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("ffprobe could not count input frames".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() < 4 {
        return Err(format!("ffprobe returned incomplete video metadata: {fields:?}"));
    }
    let (fps_num, fps_den) = fields[2].split_once('/').ok_or("invalid input frame rate")?;
    Ok(Source {
        width: fields[0].parse().map_err(|_| "invalid input width")?,
        height: fields[1].parse().map_err(|_| "invalid input height")?,
        fps_num: fps_num.parse().map_err(|_| "invalid frame-rate numerator")?,
        fps_den: fps_den.parse().map_err(|_| "invalid frame-rate denominator")?,
        frames: fields[3].parse().map_err(|_| "invalid input frame count")?,
    })
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

fn chunk_worker_count(frames: u64, requested: usize) -> usize {
    requested.max(1).min((frames.div_ceil(FIXED_STRIDE) as usize) / 4).max(1)
}

fn memory_limit_from(available_mib: u64, requested: usize, reserve_mib: u64, per_worker_mib: u64) -> usize {
    let usable = available_mib.saturating_sub(reserve_mib);
    requested.max(1).min((usable / per_worker_mib.max(1)) as usize).max(1)
}

pub fn memory_worker_limit(requested: usize) -> usize {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return requested.max(1);
    };
    let Some(available_kib) = meminfo.lines().find_map(|line| {
        line.strip_prefix("MemAvailable:")?.split_whitespace().next()?.parse::<u64>().ok()
    }) else {
        return requested.max(1);
    };
    let reserve_mib = std::env::var("PN_PARALLEL_MEMORY_RESERVE_MIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4096);
    let per_worker_mib = std::env::var("PN_PARALLEL_MEMORY_PER_WORKER_MIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(800);
    memory_limit_from(available_kib / 1024, requested, reserve_mib, per_worker_mib)
}

fn freeze_aot(directory: &Path) {
    std::fs::create_dir_all(directory).ok();
    std::fs::write(directory.join("STOP"), b"final encoder took ownership\n").ok();
    for _ in 0..20 {
        let partial = std::fs::read_dir(directory).ok().is_some_and(|entries| {
            entries.flatten().any(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        });
        if !partial {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn ranges(source: &Source, plan: Option<&Path>, workers: usize) -> (Vec<(u64, u64)>, bool) {
    let natural = plan
        .and_then(|path| crate::plan::BoundaryPlan::read(path).ok())
        .filter(|plan| {
            plan.complete
                && plan.idrs.first().copied() == Some(0)
                && plan.last_planned_pts.unwrap_or(0) >= source.frames.saturating_sub(MAX_PLAN_TAIL)
                && plan.idrs.len() >= workers.saturating_mul(4)
        });
    let starts = if let Some(plan) = natural.as_ref() {
        plan.idrs
            .iter()
            .copied()
            .filter(|frame| *frame < source.frames)
            .collect::<Vec<_>>()
    } else {
        (0..source.frames).step_by(FIXED_STRIDE as usize).collect()
    };
    let mut out = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(source.frames);
        if end > start {
            out.push((start, end - start));
        }
    }
    (out, natural.is_some())
}

fn reuse_aot_chunk(directory: &Path, output: &Path, start: u64, frames: u64) -> Option<u64> {
    let candidate = aot_chunk_path(directory, start, frames);
    let metadata = std::fs::metadata(&candidate).ok()?;
    if metadata.len() == 0 {
        return None;
    }
    if std::fs::hard_link(&candidate, output).is_err() {
        std::fs::copy(&candidate, output).ok()?;
    }
    Some(metadata.len())
}

fn encode_chunk(
    config: &ParallelConfig,
    source: &Source,
    encoder_config: &Config,
    start: u64,
    frames: u64,
    output: &Path,
    completed: &AtomicU64,
    bytes: &AtomicU64,
    stop: &AtomicBool,
    events: &mpsc::Sender<WorkerEvent>,
    extra_cancel_files: &[&Path],
) -> Result<(), String> {
    let seek = format!("{:.9}", start as f64 * source.fps_den as f64 / source.fps_num as f64);
    let frame_count = frames.to_string();
    let subtitle = std::fs::canonicalize(&config.subtitle).unwrap_or_else(|_| config.subtitle.clone());
    let filter = format!("ass={},format=yuv420p", filter_quote(&subtitle.to_string_lossy()));
    let mut child = Command::new(&config.ffmpeg)
        .args([
            "-v", "error", "-ss", &seek, "-copyts", "-i",
        ])
        .arg(&config.input)
        .args([
            "-map", "0:v:0", "-frames:v", &frame_count,
            "-an", "-sn", "-vf", &filter,
            "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn chunk decoder: {e}"))?;
    let stdout = child.stdout.take().ok_or("chunk decoder has no stdout")?;
    let mut y4m = Y4mReader::new(BufReader::with_capacity(1 << 20, stdout))?;
    let mut encoder = Encoder::open(encoder_config)?;
    let mut writer = BufWriter::new(File::create(output).map_err(|e| e.to_string())?);
    let mut count = 0u64;
    while count < frames {
        if stop.load(Ordering::Relaxed)
            || config.cancel_file.as_ref().is_some_and(|path| path.exists())
            || extra_cancel_files.iter().any(|path| path.exists())
        {
            stop.store(true, Ordering::Relaxed);
            child.kill().ok();
            return Err("cancelled".to_string());
        }
        let Some(frame) = y4m.next_frame()? else {
            break;
        };
        let encoded = encoder.encode(
            frame.y,
            frame.u,
            frame.v,
            frame.stride_y,
            frame.stride_c,
            frame.stride_c,
            count as i64,
        )?;
        writer.write_all(encoded).map_err(|e| e.to_string())?;
        bytes.fetch_add(encoded.len() as u64, Ordering::Relaxed);
        count += 1;
        completed.fetch_add(1, Ordering::Relaxed);
        if count == 1 || count.is_multiple_of(8) {
            events.send(WorkerEvent::Progress).ok();
        }
    }
    loop {
        let encoded = encoder.flush()?;
        if encoded.is_empty() {
            break;
        }
        writer.write_all(encoded).map_err(|e| e.to_string())?;
        bytes.fetch_add(encoded.len() as u64, Ordering::Relaxed);
    }
    writer.flush().map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() || count != frames {
        return Err(format!("chunk at frame {start} decoded {count}/{frames} frames"));
    }
    Ok(())
}

pub fn encode_aot_chunk(config: AotChunkConfig) -> Result<(), String> {
    if config.frames == 0 {
        return Err("AOT chunk is empty".to_string());
    }
    if config.stop_file.exists()
        || config.cancel_file.as_ref().is_some_and(|path| path.exists())
        || config.busy_file.as_ref().is_some_and(|path| path.exists())
    {
        return Err("cancelled".to_string());
    }
    let parent = config.output.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(
        ".{}.{}.part",
        config.output.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
    ));
    std::fs::remove_file(&temporary).ok();
    let parallel = ParallelConfig {
        ffmpeg: config.ffmpeg,
        ffprobe: PathBuf::new(),
        input: config.input,
        output: config.output.clone(),
        subtitle: config.subtitle,
        cancel_file: config.cancel_file,
        plan: None,
        workers: 1,
        encoder: config.encoder.clone(),
        audio_map: String::new(),
    };
    let source = Source {
        width: config.width,
        height: config.height,
        fps_num: config.fps_num,
        fps_den: config.fps_den,
        frames: config.start + config.frames,
    };
    let mut encoder = config.encoder;
    encoder.width = config.width;
    encoder.height = config.height;
    encoder.fps_num = config.fps_num;
    encoder.fps_den = config.fps_den;
    encoder.threads = 1;
    encoder.plan_only = false;
    let completed = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let (events, _receiver) = mpsc::channel();
    let mut extra_cancel_files = vec![config.stop_file.as_path()];
    if let Some(path) = config.busy_file.as_deref() {
        extra_cancel_files.push(path);
    }
    let result = encode_chunk(
        &parallel,
        &source,
        &encoder,
        config.start,
        config.frames,
        &temporary,
        &completed,
        &bytes,
        &stop,
        &events,
        &extra_cancel_files,
    );
    if let Err(error) = result {
        std::fs::remove_file(&temporary).ok();
        return Err(error);
    }
    if config.stop_file.exists() || config.busy_file.as_ref().is_some_and(|path| path.exists()) {
        std::fs::remove_file(&temporary).ok();
        return Err("cancelled".to_string());
    }
    std::fs::rename(&temporary, &config.output).map_err(|e| {
        std::fs::remove_file(&temporary).ok();
        e.to_string()
    })
}

pub fn encode_parallel<F>(config: ParallelConfig, mut progress: F) -> Result<ParallelSummary, String>
where
    F: FnMut(ParallelProgress),
{
    let aot = config.plan.as_deref().map(aot_directory);
    if let Some(directory) = aot.as_ref() {
        freeze_aot(directory);
    }
    let source = Arc::new(probe(&config.ffprobe, &config.input)?);
    let scratch = config
        .output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("pnmpeg-parallel");
    std::fs::remove_dir_all(&scratch).ok();
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let requested_workers = config.workers.max(1);
    let workers = chunk_worker_count(source.frames, memory_worker_limit(requested_workers));
    let (ranges, natural_boundaries) = ranges(&source, config.plan.as_deref(), workers);
    if ranges.len() < workers.saturating_mul(4) {
        if let Some(directory) = aot.as_ref() {
            std::fs::remove_dir_all(directory).ok();
        }
        std::fs::remove_dir_all(&scratch).ok();
        return Err("parallel input has fewer than four chunks per worker".to_string());
    }
    let reused_chunks = Arc::new(AtomicUsize::new(0));
    let audio = scratch.join("audio.m4a");
    let mut audio_child = Command::new(&config.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&config.input)
        .args([
            "-map", &format!("0:{}", config.audio_map),
            "-vn", "-c:a", "aac", "-b:a", "192k", "-y",
        ])
        .arg(&audio)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn audio encoder: {e}"))?;

    let ranges = Arc::new(ranges);
    let next = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (events_tx, events_rx) = mpsc::channel();
    let start = Instant::now();
    let mut error = None;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let ranges = ranges.clone();
            let next = next.clone();
            let completed = completed.clone();
            let bytes = bytes.clone();
            let stop = stop.clone();
            let events = events_tx.clone();
            let source = source.clone();
            let aot = aot.clone();
            let reused_chunks = reused_chunks.clone();
            let config = &config;
            let mut encoder_config = config.encoder.clone();
            encoder_config.width = source.width;
            encoder_config.height = source.height;
            encoder_config.fps_num = source.fps_num;
            encoder_config.fps_den = source.fps_den;
            encoder_config.threads = 1;
            encoder_config.plan_only = false;
            let scratch = scratch.clone();
            scope.spawn(move || {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&(frame, count)) = ranges.get(index) else {
                        break;
                    };
                    let output = scratch.join(format!("chunk-{index:06}.264"));
                    let reused = aot.as_ref().and_then(|directory| {
                        reuse_aot_chunk(directory, &output, frame, count)
                    }).is_some_and(|size| {
                        completed.fetch_add(count, Ordering::Relaxed);
                        bytes.fetch_add(size, Ordering::Relaxed);
                        reused_chunks.fetch_add(1, Ordering::Relaxed);
                        events.send(WorkerEvent::Progress).ok();
                        true
                    });
                    if reused {
                        continue;
                    }
                    if let Err(e) = encode_chunk(
                        config,
                        &source,
                        &encoder_config,
                        frame,
                        count,
                        &output,
                        &completed,
                        &bytes,
                        &stop,
                        &events,
                        &[],
                    ) {
                        stop.store(true, Ordering::Relaxed);
                        events.send(WorkerEvent::Error(e)).ok();
                        break;
                    }
                }
                events.send(WorkerEvent::Done).ok();
            });
        }
        drop(events_tx);
        let mut done = 0usize;
        let mut last_emit = None;
        while done < workers {
            match events_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(WorkerEvent::Done) => done += 1,
                Ok(WorkerEvent::Error(e)) => {
                    if error.is_none() {
                        error = Some(e);
                    }
                }
                Ok(WorkerEvent::Progress) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let now = Instant::now();
            if last_emit.is_none_or(|last: Instant| now.duration_since(last) >= Duration::from_secs(5)) {
                let frames = completed.load(Ordering::Relaxed);
                progress(ParallelProgress {
                    frames,
                    total_frames: source.frames,
                    fps: frames as f64 / start.elapsed().as_secs_f64().max(0.001),
                    bytes: bytes.load(Ordering::Relaxed),
                });
                last_emit = Some(now);
            }
        }
    });
    if let Some(error) = error {
        audio_child.kill().ok();
        audio_child.wait().ok();
        std::fs::remove_dir_all(&scratch).ok();
        if let Some(directory) = aot.as_ref() {
            std::fs::remove_dir_all(directory).ok();
        }
        return Err(error);
    }
    if !audio_child.wait().map_err(|e| e.to_string())?.success() {
        std::fs::remove_dir_all(&scratch).ok();
        if let Some(directory) = aot.as_ref() {
            std::fs::remove_dir_all(directory).ok();
        }
        return Err("audio encode failed".to_string());
    }

    let elementary = scratch.join("video.264");
    let mut combined = BufWriter::new(File::create(&elementary).map_err(|e| e.to_string())?);
    for index in 0..ranges.len() {
        let mut chunk = File::open(scratch.join(format!("chunk-{index:06}.264")))
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut chunk, &mut combined).map_err(|e| e.to_string())?;
    }
    combined.flush().map_err(|e| e.to_string())?;
    let frame_rate = format!("{}/{}", source.fps_num, source.fps_den);
    let timescale = source.fps_num.to_string();
    let video_mp4 = scratch.join("video.mp4");
    // Raw H.264 has no container timestamps. Its parser begins DTS two B-frame delays below zero;
    // normalise the video alone first, otherwise `avoid_negative_ts` also shifts audio by that
    // delay and creates a visible 80ms sync error at 24fps. A numerator timescale represents both
    // integer rates and 24000/1001 exactly.
    let video_status = Command::new(&config.ffmpeg)
        .args(["-v", "error", "-r", &frame_rate, "-i"])
        .arg(&elementary)
        .args([
            "-map", "0:v:0", "-c", "copy",
            "-avoid_negative_ts", "make_zero",
            "-video_track_timescale", &timescale,
            "-y",
        ])
        .arg(&video_mp4)
        .status()
        .map_err(|e| format!("spawn video timestamp mux: {e}"))?;
    if !video_status.success() {
        return Err("video timestamp mux failed".to_string());
    }
    let status = Command::new(&config.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&video_mp4)
        .args(["-i"])
        .arg(&audio)
        .args([
            "-map", "0:v:0", "-map", "1:a:0", "-c", "copy",
            "-movflags", "+faststart", "-y",
        ])
        .arg(&config.output)
        .status()
        .map_err(|e| format!("spawn final mux: {e}"))?;
    if !status.success() {
        return Err("final mux failed".to_string());
    }
    let elapsed = start.elapsed();
    progress(ParallelProgress {
        frames: source.frames,
        total_frames: source.frames,
        fps: source.frames as f64 / elapsed.as_secs_f64().max(0.001),
        bytes: std::fs::metadata(&config.output).map(|value| value.len()).unwrap_or(0),
    });
    std::fs::remove_dir_all(&scratch).ok();
    if let Some(directory) = aot.as_ref() {
        std::fs::remove_dir_all(directory).ok();
    }
    Ok(ParallelSummary {
        frames: source.frames,
        chunks: ranges.len(),
        workers,
        reused_chunks: reused_chunks.load(Ordering::Relaxed),
        natural_boundaries,
        elapsed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source { width: 1920, height: 1080, fps_num: 24, fps_den: 1, frames: 1500 }
    }

    #[test]
    fn an_i9_worker_count_is_capped_to_four_chunks_each() {
        assert_eq!(chunk_worker_count(15_036, 16), 15);
        assert_eq!(chunk_worker_count(15_036, 12), 12);
        assert_eq!(chunk_worker_count(999, 16), 1);
    }

    #[test]
    fn memory_headroom_caps_workers_before_the_oom_killer_does() {
        assert_eq!(memory_limit_from(9_600, 12, 4_096, 800), 6);
        assert_eq!(memory_limit_from(28_000, 16, 4_096, 800), 16);
        assert_eq!(memory_limit_from(2_000, 12, 4_096, 800), 1);
    }

    #[test]
    fn a_fixed_fallback_can_reuse_an_atomic_aot_candidate() {
        let root = std::env::temp_dir().join(format!("pnx264-aot-reuse-{}", std::process::id()));
        let aot = root.join("parallel-aot");
        std::fs::create_dir_all(&aot).unwrap();
        std::fs::write(aot_chunk_path(&aot, 250, 250), b"encoded chunk").unwrap();
        let output = root.join("fixed.264");
        assert_eq!(reuse_aot_chunk(&aot, &output, 250, 250), Some(13));
        assert_eq!(std::fs::read(output).unwrap(), b"encoded chunk");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_completed_plan_uses_only_legal_idrs() {
        let path = std::env::temp_dir().join(format!("pnx264-ranges-{}", std::process::id()));
        std::fs::write(
            &path,
            "PNPLAN1\nidr|0\nidr|224\nidr|584\nidr|834\ncomplete|1500|1500\n",
        ).unwrap();
        let (ranges, natural) = ranges(&source(), Some(&path), 1);
        std::fs::remove_file(path).ok();
        assert!(natural);
        assert_eq!(ranges, vec![(0, 224), (224, 360), (584, 250), (834, 666)]);
    }

    #[test]
    fn a_planner_that_fell_behind_uses_fixed_stride() {
        let path = std::env::temp_dir().join(format!("pnx264-ranges-behind-{}", std::process::id()));
        std::fs::write(&path, "PNPLAN1\nidr|0\nidr|250\nidr|500\nidr|750\nprogress|900|960\n").unwrap();
        let (ranges, natural) = ranges(&source(), Some(&path), 1);
        std::fs::remove_file(path).ok();
        assert!(!natural);
        assert_eq!(ranges.len(), 6);
        assert!(ranges.iter().all(|(_, count)| *count <= FIXED_STRIDE));
    }
}
