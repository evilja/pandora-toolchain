// Pandora's side of Caramel Analysis: probing the source, decoding the sample windows through
// ffmpeg, and feeding them to `caramel_kernel`.
//
// The cost is a fraction of a second of decoding per window. Each window is one ffmpeg process that
// seeks with `-noaccurate_seek`, so it starts decoding at the keyframe it lands on and stops after a
// few frames — nothing between windows is ever decoded, and the container is never fully demuxed.
// What the container header says (codec, bitrate, bit depth) is read with one ffprobe call.

use crate::lib::bin::resolve_runtime_binary;
use caramel_kernel::{EncodeContext, Estimate, Frame, Plane, SourceContext, WindowAnalyzer, WindowFeatures};
use serde::Serialize;
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

pub const CARAMEL_ANALYSIS: &str = "caramel-analysis";

// Consecutive frames decoded per window: five frame pairs for the temporal features.
const WINDOW_FRAMES: usize = 6;
// A whole file gets a window about every 40 seconds — 24 across a TV episode.
const FILE_WINDOW_SPACING: f64 = 40.0;
const FILE_WINDOWS_MIN: usize = 6;
const FILE_WINDOWS_MAX: usize = 24;
// A frame range (one AOT chunk) is short and IDR-bounded, so it is sampled more densely but with
// far fewer windows in absolute terms.
const RANGE_WINDOW_SPACING: f64 = 8.0;
const RANGE_WINDOWS_MIN: usize = 2;
const RANGE_WINDOWS_MAX: usize = 6;

// What `--aot-crf` asks for: a CRF, or Caramel Analysis anchored at the preset's CRF (or at the
// one written after the colon, `caramel-analysis:17`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AotCrf {
    Fixed(f32),
    CaramelAnalysis { anchor: Option<f32> },
}

impl FromStr for AotCrf {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let parse_crf = |text: &str| {
            text.parse::<f32>()
                .ok()
                .filter(|crf| crf.is_finite() && (-12.0..=63.0).contains(crf))
                .ok_or_else(|| format!("`{text}` is not a CRF"))
        };
        if value == CARAMEL_ANALYSIS {
            return Ok(Self::CaramelAnalysis { anchor: None });
        }
        if let Some(anchor) = value.strip_prefix(CARAMEL_ANALYSIS).and_then(|rest| rest.strip_prefix(':')) {
            return Ok(Self::CaramelAnalysis { anchor: Some(parse_crf(anchor)?) });
        }
        parse_crf(value).map(Self::Fixed).map_err(|error| format!("{error}; expected a number or `{CARAMEL_ANALYSIS}`"))
    }
}

impl fmt::Display for AotCrf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(crf) => write!(f, "{crf}"),
            Self::CaramelAnalysis { anchor: None } => f.write_str(CARAMEL_ANALYSIS),
            Self::CaramelAnalysis { anchor: Some(anchor) } => write!(f, "{CARAMEL_ANALYSIS}:{anchor}"),
        }
    }
}

// Overrides of the sampling plan, for experiments with density against accuracy. Anything left
// unset is the built-in plan for a whole file or a frame range.
#[derive(Clone, Copy, Debug, Default, Serialize, serde::Deserialize)]
pub struct Sampling {
    #[serde(default)]
    pub frames_per_window: Option<usize>,
    #[serde(default)]
    pub spacing_seconds: Option<f64>,
    #[serde(default)]
    pub min_windows: Option<usize>,
    #[serde(default)]
    pub max_windows: Option<usize>,
}

// Hard ceilings on an override, so a request cannot turn a sampled analysis into a full decode.
const SAMPLING_FRAMES_CEILING: usize = 48;
const SAMPLING_WINDOWS_CEILING: usize = 128;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SamplingPlan {
    pub frames_per_window: usize,
    pub spacing_seconds: f64,
    pub min_windows: usize,
    pub max_windows: usize,
}

impl SamplingPlan {
    // The built-in plan: for a frame range (an AOT chunk) or for a whole file.
    pub fn default_for(range: bool) -> Self {
        Self::resolve(range, &Sampling::default())
    }

    fn resolve(range: bool, overrides: &Sampling) -> Self {
        let (spacing, min, max) = if range {
            (RANGE_WINDOW_SPACING, RANGE_WINDOWS_MIN, RANGE_WINDOWS_MAX)
        } else {
            (FILE_WINDOW_SPACING, FILE_WINDOWS_MIN, FILE_WINDOWS_MAX)
        };
        let max_windows = overrides.max_windows.unwrap_or(max).clamp(1, SAMPLING_WINDOWS_CEILING);
        Self {
            frames_per_window: overrides.frames_per_window.unwrap_or(WINDOW_FRAMES).clamp(1, SAMPLING_FRAMES_CEILING),
            spacing_seconds: overrides.spacing_seconds.filter(|s| s.is_finite() && *s >= 1.0).unwrap_or(spacing),
            min_windows: overrides.min_windows.unwrap_or(min).clamp(1, max_windows),
            max_windows,
        }
    }
}

// The x264 settings a verification encodes its clips with — the preset's own, so the measured
// quality is the quality that preset would deliver at that CRF.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyEncoder {
    pub preset: Option<String>,
    pub tune: Option<String>,
    pub x264_params: Option<String>,
}

// Ground truth for the estimate: short clips at a few sample points, each encoded at every CRF
// listed and measured against the source. This is the brute-force search the estimate exists to
// replace, run small and only on request, to see how far off the estimate is.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRequest {
    pub crfs: Vec<f32>,
    pub clip_seconds: f64,
    pub max_clips: usize,
    pub encoder: VerifyEncoder,
    // Where the clips are written while they are measured; removed afterwards.
    pub scratch: PathBuf,
    // Checked before every clip: a verification is minutes of encoding, and a cancel should not
    // have to wait for all of it.
    pub cancel_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyPoint {
    pub crf: f32,
    pub clips: usize,
    pub mean_ssim: f64,
    pub min_ssim: f64,
    pub mean_psnr: f64,
    pub min_psnr: f64,
    pub kbps: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyReport {
    pub encoder: VerifyEncoder,
    pub clip_seconds: f64,
    pub positions: Vec<f64>,
    pub points: Vec<VerifyPoint>,
    pub elapsed_ms: u128,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameRange {
    pub start: u64,
    pub frames: u64,
}

pub struct CaramelRequest<'a> {
    pub input: &'a Path,
    // Analyse only these frames (an AOT chunk); None for the whole file.
    pub range: Option<FrameRange>,
    // The frame rate the range is counted in, when the caller already knows it.
    pub fps: Option<f64>,
    pub encode: EncodeContext,
    // Windows decoded at once. An AOT chunk runs beside other single-threaded encoders and uses 1.
    pub workers: usize,
    // ffmpeg's decoder `-threads`; 0 lets it choose.
    pub decoder_threads: u32,
    pub sampling: Sampling,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaramelReport {
    pub input: String,
    pub source: SourceContext,
    pub encode: EncodeContext,
    pub duration_seconds: Option<f64>,
    pub sampling: SamplingPlan,
    pub windows_planned: usize,
    pub windows_skipped: usize,
    pub elapsed_ms: u128,
    pub estimate: Estimate,
    pub windows: Vec<WindowFeatures>,
    // Filled in from `run_verification` when one was asked for, or why it could not be.
    pub verify: Option<VerifyReport>,
    pub verify_error: Option<String>,
}

#[derive(Clone, Debug)]
struct SourceProbe {
    context: SourceContext,
    duration: Option<f64>,
}

fn parse_rate(value: Option<&str>) -> Option<f64> {
    let value = value?;
    let rate = match value.split_once('/') {
        Some((n, d)) => n.parse::<f64>().ok()? / d.parse::<f64>().ok().filter(|d| *d != 0.0)?,
        None => value.parse().ok()?,
    };
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

fn json_number(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn bit_depth(pix_fmt: Option<&str>, raw_bits: Option<f64>) -> u8 {
    if let Some(bits) = raw_bits.filter(|bits| *bits >= 8.0 && *bits <= 16.0) {
        return bits as u8;
    }
    match pix_fmt.unwrap_or_default() {
        name if name.contains("12") => 12,
        name if name.contains("10") => 10,
        _ => 8,
    }
}

fn parse_probe(stdout: &[u8]) -> Option<SourceProbe> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let stream = value.get("streams")?.as_array()?.first()?;
    let format = value.get("format");
    let width = json_number(stream.get("width"))? as u32;
    let height = json_number(stream.get("height"))? as u32;
    let fps = parse_rate(stream.get("avg_frame_rate").and_then(|v| v.as_str()))
        .or_else(|| parse_rate(stream.get("r_frame_rate").and_then(|v| v.as_str())))
        .unwrap_or(24000.0 / 1001.0);
    // Matroska rarely carries a stream bitrate, but mkvmerge writes its statistics tags; the
    // container bitrate includes audio, which only errs toward a lower CRF.
    let tags = stream.get("tags");
    let video_bitrate = json_number(stream.get("bit_rate"))
        .or_else(|| json_number(tags.and_then(|tags| tags.get("BPS"))))
        .or_else(|| json_number(tags.and_then(|tags| tags.get("BPS-eng"))))
        .or_else(|| json_number(format.and_then(|format| format.get("bit_rate"))));
    let pix_fmt = stream.get("pix_fmt").and_then(|v| v.as_str());
    Some(SourceProbe {
        context: SourceContext {
            width,
            height,
            fps,
            video_bitrate,
            codec: stream.get("codec_name").and_then(|v| v.as_str()).map(str::to_owned),
            bit_depth: bit_depth(pix_fmt, json_number(stream.get("bits_per_raw_sample"))),
        },
        duration: json_number(format.and_then(|format| format.get("duration"))).filter(|d| *d > 0.0),
    })
}

fn probe_source(input: &Path) -> Result<SourceProbe, String> {
    let output = Command::new(resolve_runtime_binary("ffprobe"))
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries",
            "stream=width,height,codec_name,pix_fmt,avg_frame_rate,r_frame_rate,bit_rate,bits_per_raw_sample:stream_tags=BPS,BPS-eng:format=duration,bit_rate",
            "-of", "json",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffprobe could not start: {e}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe failed on {}", input.display()));
    }
    parse_probe(&output.stdout).ok_or_else(|| format!("ffprobe found no video stream in {}", input.display()))
}

// Window start times spread evenly over [start, start + length): each window sits in the middle of
// its share of the span, so neither end of the file is over-represented.
fn plan_windows(start: f64, length: f64, spacing: f64, min: usize, max: usize) -> Vec<f64> {
    if !(length > 0.0) {
        return vec![start];
    }
    let count = ((length / spacing).round() as usize).clamp(min, max);
    (0..count)
        .map(|index| start + (index as f64 + 0.5) * length / count as f64)
        .collect()
}

// Cheap identity of a window's first frame. With `-noaccurate_seek` two windows inside one long
// GOP land on the same keyframe, and analysing the same frames twice would weigh them double.
fn fingerprint(luma: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in luma.iter().step_by(61) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn analyse_window(
    ffmpeg: &Path,
    input: &Path,
    position: f64,
    frames: usize,
    decoder_threads: u32,
) -> Result<Option<(u64, WindowFeatures)>, String> {
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-threads", &decoder_threads.to_string(), "-noaccurate_seek"])
        .args(["-ss", &format!("{position:.3}"), "-i"])
        .arg(input)
        .args([
            "-map", "0:v:0", "-frames:v", &frames.to_string(), "-an", "-sn", "-dn",
            "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg has no stdout")?;
    let result = (|| {
        let mut reader = match pnx264::y4m::Y4mReader::new(BufReader::with_capacity(1 << 20, stdout)) {
            Ok(reader) => reader,
            // Nothing decoded: a seek past the end of a file whose duration was an estimate.
            Err(_) => return Ok(None),
        };
        let (width, height) = (reader.width, reader.height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let mut window = WindowAnalyzer::new(position);
        let mut identity = None;
        while let Some(frame) = reader.next_frame()? {
            let y = Plane::new(frame.y, width, height, frame.stride_y).ok_or("short luma plane")?;
            let u = Plane::new(frame.u, chroma_width, chroma_height, frame.stride_c);
            let v = Plane::new(frame.v, chroma_width, chroma_height, frame.stride_c);
            identity.get_or_insert_with(|| fingerprint(frame.y));
            window.push(&Frame { y, u, v });
        }
        let features = window.finish();
        Ok(identity.filter(|_| features.frames > 0).map(|identity| (identity, features)))
    })();
    child.kill().ok();
    child.wait().ok();
    result
}

pub fn run_caramel_analysis(request: &CaramelRequest) -> Result<CaramelReport, String> {
    let started = std::time::Instant::now();
    let probe = probe_source(request.input)?;
    let fps = request.fps.filter(|fps| *fps > 0.0).unwrap_or(probe.context.fps);
    let plan = SamplingPlan::resolve(request.range.is_some(), &request.sampling);
    let positions = match request.range {
        Some(range) => plan_windows(
            range.start as f64 / fps,
            // The last window must still have its frames inside the range.
            (range.frames.saturating_sub(plan.frames_per_window as u64)) as f64 / fps,
            plan.spacing_seconds,
            plan.min_windows,
            plan.max_windows,
        ),
        None => match probe.duration {
            Some(duration) => plan_windows(
                0.0,
                (duration - plan.frames_per_window as f64 / fps).max(0.0),
                plan.spacing_seconds,
                plan.min_windows,
                plan.max_windows,
            ),
            // No duration in the header: one window at the start is all that can be placed safely.
            None => vec![0.0],
        },
    };
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let workers = request.workers.clamp(1, positions.len().max(1));
    let mut results: Vec<Option<Result<Option<(u64, WindowFeatures)>, String>>> = vec![None; positions.len()];
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots = std::sync::Mutex::new(&mut results);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&position) = positions.get(index) else { break };
                let outcome = analyse_window(
                    &ffmpeg,
                    request.input,
                    position,
                    plan.frames_per_window,
                    request.decoder_threads,
                );
                if let Ok(mut slots) = slots.lock() {
                    slots[index] = Some(outcome);
                }
            });
        }
    });

    let mut seen = std::collections::HashSet::new();
    let mut windows = Vec::new();
    let mut first_error = None;
    for outcome in results.into_iter().flatten() {
        match outcome {
            Ok(Some((identity, features))) if seen.insert(identity) => windows.push(features),
            Ok(_) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if windows.is_empty() {
        return Err(first_error.unwrap_or_else(|| "no window decoded any frames".to_string()));
    }
    let estimate = caramel_kernel::estimate(&windows, &probe.context, &request.encode);
    Ok(CaramelReport {
        input: request.input.display().to_string(),
        source: probe.context,
        encode: request.encode,
        duration_seconds: probe.duration,
        sampling: plan,
        windows_planned: positions.len(),
        windows_skipped: positions.len() - windows.len(),
        elapsed_ms: started.elapsed().as_millis(),
        estimate,
        windows,
        verify: None,
        verify_error: None,
    })
}

// Clip positions for a verification: an even subset of the windows the estimate was made from, so
// the clips measure the same content.
fn verify_positions(windows: &[WindowFeatures], max: usize) -> Vec<f64> {
    let take = max.min(windows.len());
    (0..take).map(|index| windows[index * windows.len() / take].position_seconds).collect()
}

// `key:value` out of one line of an ffmpeg ssim/psnr stats file. A PSNR of identical frames is
// written as `inf` and is capped, so one perfect frame cannot make an average infinite.
fn stat_value(line: &str, key: &str) -> Option<f64> {
    let token = line.split_whitespace().find_map(|token| token.strip_prefix(key)?.strip_prefix(':'))?;
    if token.starts_with("inf") {
        return Some(100.0);
    }
    token.parse::<f64>().ok().map(|value| value.min(100.0))
}

fn stats_column(path: &Path, key: &str) -> Vec<f64> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| stat_value(line, key))
        .collect()
}

fn run_quiet(command: &mut Command, what: &str) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{what} could not start: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{what} failed: {}", stderr.lines().last().unwrap_or("no output").trim()))
}

struct ClipMeasurement {
    ssim: Vec<f64>,
    psnr: Vec<f64>,
    bytes: u64,
}

// One clip at one CRF: encode it, then decode the source from the same keyframe and compare. Both
// passes seek with `-noaccurate_seek` to the same timestamp and take the same number of frames, so
// they start on the same keyframe and cover the same frames. The length is a frame count, not a
// duration: with `-noaccurate_seek` an input `-t` is measured from the requested timestamp rather
// than the keyframe actually landed on, and can end the clip before its first frame.
fn measure_clip(
    ffmpeg: &Path,
    input: &Path,
    position: f64,
    frames: u64,
    request: &VerifyRequest,
    crf: f32,
    name: &str,
) -> Result<ClipMeasurement, String> {
    let clip = request.scratch.join(format!("{name}.mkv"));
    let seek = format!("{position:.3}");
    let frame_count = frames.to_string();
    let mut encode = Command::new(ffmpeg);
    encode
        .args(["-v", "error", "-nostdin", "-y", "-noaccurate_seek", "-ss", &seek, "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", &frame_count, "-an", "-sn", "-dn", "-pix_fmt", "yuv420p", "-c:v", "libx264"]);
    if let Some(preset) = &request.encoder.preset {
        encode.args(["-preset", preset]);
    }
    if let Some(tune) = &request.encoder.tune {
        encode.args(["-tune", tune]);
    }
    if let Some(params) = &request.encoder.x264_params {
        encode.args(["-x264-params", params]);
    }
    encode.args(["-crf", &crf.to_string()]).arg(&clip);
    run_quiet(&mut encode, "the verification encode")?;
    let bytes = std::fs::metadata(&clip).map(|meta| meta.len()).unwrap_or(0);

    // The filter graph names its stats files relative to the working directory, which keeps paths
    // with `:` or `\` (both special inside a filter graph) out of it entirely.
    let (ssim_file, psnr_file) = (format!("{name}.ssim"), format!("{name}.psnr"));
    let graph = format!(
        "[1:v]setpts=PTS-STARTPTS,split[e1][e2];\
         [0:v]trim=end_frame={frames},format=yuv420p,setpts=PTS-STARTPTS,split[r1][r2];\
         [e1][r1]ssim=stats_file={ssim_file}:shortest=1;[e2][r2]psnr=stats_file={psnr_file}:shortest=1"
    );
    let mut measure = Command::new(ffmpeg);
    measure
        .current_dir(&request.scratch)
        .args(["-v", "error", "-nostdin", "-noaccurate_seek", "-ss", &seek, "-i"])
        .arg(input)
        .arg("-i")
        .arg(&clip)
        .args(["-lavfi", &graph, "-f", "null", "-"]);
    run_quiet(&mut measure, "the verification measurement")?;
    let measured = ClipMeasurement {
        ssim: stats_column(&request.scratch.join(&ssim_file), "All"),
        psnr: stats_column(&request.scratch.join(&psnr_file), "psnr_avg"),
        bytes,
    };
    for file in [clip, request.scratch.join(&ssim_file), request.scratch.join(&psnr_file)] {
        std::fs::remove_file(file).ok();
    }
    if measured.ssim.is_empty() {
        return Err("the verification measurement compared no frames".to_string());
    }
    Ok(measured)
}

fn summarise(crf: f32, clips: &[ClipMeasurement], clip_seconds: f64) -> VerifyPoint {
    let ssim: Vec<f64> = clips.iter().flat_map(|clip| clip.ssim.iter().copied()).collect();
    let psnr: Vec<f64> = clips.iter().flat_map(|clip| clip.psnr.iter().copied()).collect();
    let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
    let min = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
    let bytes: u64 = clips.iter().map(|clip| clip.bytes).sum();
    VerifyPoint {
        crf,
        clips: clips.len(),
        mean_ssim: mean(&ssim),
        min_ssim: if ssim.is_empty() { 0.0 } else { min(&ssim) },
        mean_psnr: mean(&psnr),
        min_psnr: if psnr.is_empty() { 0.0 } else { min(&psnr) },
        kbps: bytes as f64 * 8.0 / (clips.len().max(1) as f64 * clip_seconds) / 1000.0,
    }
}

// The verification pass: every CRF listed, over the same clips. It is the expensive part of a
// report — a real encode of `max_clips × clip_seconds` of video per CRF — and only runs on request.
pub fn run_verification(report: &CaramelReport, request: &VerifyRequest) -> Result<VerifyReport, String> {
    let started = std::time::Instant::now();
    if request.crfs.is_empty() {
        return Err("no CRFs to verify".to_string());
    }
    std::fs::create_dir_all(&request.scratch).map_err(|e| format!("verification scratch: {e}"))?;
    // The measurement runs inside the scratch directory, so every path it is handed — the clip
    // included — has to be absolute.
    let scratch = std::fs::canonicalize(&request.scratch).map_err(|e| format!("verification scratch: {e}"))?;
    let request = &VerifyRequest { scratch, ..request.clone() };
    // Relative inputs would resolve against the scratch directory the measurement runs in.
    let input = std::fs::canonicalize(&report.input).unwrap_or_else(|_| PathBuf::from(&report.input));
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let positions = verify_positions(&report.windows, request.max_clips.max(1));
    let frames = (request.clip_seconds * report.source.fps).round().max(1.0) as u64;
    let mut points = Vec::with_capacity(request.crfs.len());
    for (crf_index, &crf) in request.crfs.iter().enumerate() {
        let mut clips = Vec::with_capacity(positions.len());
        for (clip_index, &position) in positions.iter().enumerate() {
            if request.cancel_file.as_ref().is_some_and(|path| path.exists()) {
                std::fs::remove_dir_all(&request.scratch).ok();
                return Err("cancelled".to_string());
            }
            match measure_clip(&ffmpeg, &input, position, frames, request, crf, &format!("clip-{crf_index}-{clip_index}")) {
                Ok(clip) => clips.push(clip),
                Err(error) => {
                    std::fs::remove_dir_all(&request.scratch).ok();
                    return Err(error);
                }
            }
        }
        points.push(summarise(crf, &clips, request.clip_seconds));
    }
    std::fs::remove_dir(&request.scratch).ok();
    Ok(VerifyReport {
        encoder: request.encoder.clone(),
        clip_seconds: request.clip_seconds,
        positions,
        points,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

// One line for a tool log.
pub fn summary_line(report: &CaramelReport) -> String {
    let estimate = &report.estimate;
    let drivers: Vec<String> = estimate
        .typical
        .iter()
        .chain(estimate.global.iter())
        .filter(|factor| factor.delta.abs() >= 0.1)
        .map(|factor| format!("{} {:+.2}", factor.factor, factor.delta))
        .collect();
    format!(
        "caramel analysis: crf {} (anchor {}, {:+.2}, confidence {:.2}) from {} windows / {} frames in {} ms [{}]",
        estimate.crf,
        estimate.anchor_crf,
        estimate.adjustment,
        estimate.confidence,
        estimate.windows_analyzed,
        estimate.frames_analyzed,
        report.elapsed_ms,
        drivers.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_lines_are_read_by_key() {
        let ssim = "n:12 Y:0.991200 U:0.995000 V:0.994000 All:0.992551 (21.279)";
        assert_eq!(stat_value(ssim, "All"), Some(0.992551));
        let psnr = "n:3 mse_avg:0.00 mse_y:0.00 psnr_avg:inf psnr_y:inf";
        assert_eq!(stat_value(psnr, "psnr_avg"), Some(100.0));
        assert_eq!(stat_value("n:4 mse_avg:2.01 psnr_avg:45.10", "psnr_avg"), Some(45.1));
        assert_eq!(stat_value(ssim, "psnr_avg"), None);
    }

    #[test]
    fn sampling_overrides_are_bounded() {
        let plan = SamplingPlan::resolve(false, &Sampling::default());
        assert_eq!((plan.frames_per_window, plan.max_windows), (WINDOW_FRAMES, FILE_WINDOWS_MAX));
        let wild = Sampling {
            frames_per_window: Some(10_000),
            spacing_seconds: Some(0.0),
            min_windows: Some(500),
            max_windows: Some(10_000),
        };
        let plan = SamplingPlan::resolve(false, &wild);
        assert_eq!(plan.frames_per_window, SAMPLING_FRAMES_CEILING);
        assert_eq!(plan.max_windows, SAMPLING_WINDOWS_CEILING);
        assert_eq!(plan.min_windows, SAMPLING_WINDOWS_CEILING);
        assert_eq!(plan.spacing_seconds, FILE_WINDOW_SPACING);
    }

    #[test]
    fn verification_clips_follow_the_analysed_windows() {
        let windows: Vec<WindowFeatures> = (0..10)
            .map(|index| WindowFeatures { position_seconds: index as f64 * 10.0, ..Default::default() })
            .collect();
        assert_eq!(verify_positions(&windows, 4), vec![0.0, 20.0, 50.0, 70.0]);
        assert_eq!(verify_positions(&windows[..2], 4), vec![0.0, 10.0]);
        assert!(verify_positions(&[], 4).is_empty());
    }

    #[test]
    fn aot_crf_reads_a_number_or_caramel_analysis() {
        assert_eq!("18".parse::<AotCrf>(), Ok(AotCrf::Fixed(18.0)));
        assert_eq!("17.5".parse::<AotCrf>(), Ok(AotCrf::Fixed(17.5)));
        assert_eq!(
            "caramel-analysis".parse::<AotCrf>(),
            Ok(AotCrf::CaramelAnalysis { anchor: None })
        );
        assert_eq!(
            "caramel-analysis:17".parse::<AotCrf>(),
            Ok(AotCrf::CaramelAnalysis { anchor: Some(17.0) })
        );
        assert!("caramel".parse::<AotCrf>().is_err());
        assert!("caramel-analysis:".parse::<AotCrf>().is_err());
        assert!("NaN".parse::<AotCrf>().is_err());
    }

    // The planner hands its CRF on as text; whatever it writes has to parse back to the same value.
    #[test]
    fn aot_crf_round_trips_through_its_text() {
        for value in [AotCrf::Fixed(18.0), AotCrf::CaramelAnalysis { anchor: None }, AotCrf::CaramelAnalysis { anchor: Some(16.5) }] {
            assert_eq!(value.to_string().parse::<AotCrf>(), Ok(value));
        }
    }

    #[test]
    fn windows_spread_across_the_span_within_their_bounds() {
        let episode = plan_windows(0.0, 1440.0, FILE_WINDOW_SPACING, FILE_WINDOWS_MIN, FILE_WINDOWS_MAX);
        assert_eq!(episode.len(), 24);
        assert!(episode[0] > 0.0 && *episode.last().unwrap() < 1440.0);
        assert!(episode.windows(2).all(|pair| pair[0] < pair[1]));

        let short = plan_windows(10.0, 30.0, FILE_WINDOW_SPACING, FILE_WINDOWS_MIN, FILE_WINDOWS_MAX);
        assert_eq!(short.len(), FILE_WINDOWS_MIN);
        assert!(short.iter().all(|position| (10.0..40.0).contains(position)));

        assert_eq!(plan_windows(5.0, 0.0, 8.0, 2, 6), vec![5.0]);
    }

    #[test]
    fn the_probe_reads_matroska_statistics_tags_and_bit_depth() {
        let json = br#"{"streams":[{"codec_name":"hevc","width":1920,"height":1080,"pix_fmt":"yuv420p10le",
            "avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001","tags":{"BPS":"4500000"}}],
            "format":{"duration":"1420.5","bit_rate":"4800000"}}"#;
        let probe = parse_probe(json).unwrap();
        assert_eq!(probe.context.video_bitrate, Some(4_500_000.0));
        assert_eq!(probe.context.bit_depth, 10);
        assert_eq!(probe.context.codec.as_deref(), Some("hevc"));
        assert!((probe.context.fps - 23.976).abs() < 0.001);
        assert_eq!(probe.duration, Some(1420.5));
    }

    #[test]
    fn the_probe_falls_back_to_the_container_bitrate() {
        let json = br#"{"streams":[{"codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p",
            "avg_frame_rate":"0/0","r_frame_rate":"30/1"}],"format":{"bit_rate":"3000000"}}"#;
        let probe = parse_probe(json).unwrap();
        assert_eq!(probe.context.video_bitrate, Some(3_000_000.0));
        assert_eq!(probe.context.fps, 30.0);
        assert_eq!(probe.context.bit_depth, 8);
        assert_eq!(probe.duration, None);
    }
}
