// Pandora's side of Caramel Analysis: probing the source, decoding it through ffmpeg into
// `caramel_kernel`'s scene decider, and verifying the CRFs it chose.
//
// There is no file-wide CRF. The decider splits the source into scenes and gives each its own; a
// report is that list. The same decider runs inside every chunk encoder (`pnx264::scenes`), so what
// a report says about a scene is what an encode does with it. Encodes that cannot switch CRF
// themselves — the ffmpeg x264 paths — get the report's scenes as x264 `zones`.
//
// A report decodes every frame, because every frame has to be compared with the one before it to
// find the cuts. That is split across parallel time segments, and the expensive measurement only
// runs on each scene's first `analysis_frames`. What the container header says (codec, bitrate, bit
// depth) is read with one ffprobe call.

use crate::lib::bin::resolve_runtime_binary;
use caramel_kernel::{
    Contribution, EncodeContext, Frame, MODEL_VERSION, Plane, SceneConfig, SceneDecider, SceneDecision, SceneStart,
    SourceContext, TemporalFeatures,
};
use serde::Serialize;
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

pub const CARAMEL_ANALYSIS: &str = "caramel-analysis";

// A segment boundary forces a scene boundary, so segments are long enough that this is rare: about
// a hundred seconds each, whatever the worker count.
const MIN_SEGMENT_FRAMES: u64 = 2400;
// How often a segment looks at the cancel file.
const CANCEL_CHECK_FRAMES: u64 = 48;
// A scene shorter than this is not worth a verification clip: x264's rate control has barely settled.
const VERIFY_MIN_FRAMES: u64 = 12;

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

// The x264 settings a verification encodes its clips with — the preset's own, so the measured
// quality is the quality that preset would deliver at that CRF.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyEncoder {
    pub preset: Option<String>,
    pub tune: Option<String>,
    pub x264_params: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct FrameRange {
    pub start: u64,
    pub frames: u64,
}

pub struct SceneRequest<'a> {
    pub input: &'a Path,
    // Analyse only these frames; None for the whole file.
    pub range: Option<FrameRange>,
    // The frame rate frames are counted in, when the caller already knows it.
    pub fps: Option<f64>,
    pub encode: EncodeContext,
    pub scenes: SceneConfig,
    // Segments decoded at once.
    pub workers: usize,
    // ffmpeg's decoder `-threads` per segment; 0 lets it choose.
    pub decoder_threads: u32,
    pub cancel_file: Option<PathBuf>,
    // Frames decoded so far, summed over the segments, for a caller that shows progress.
    pub progress: Option<&'a AtomicU64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SceneEntry {
    pub start_frame: u64,
    pub frames: u64,
    pub start_seconds: f64,
    pub crf: f32,
    // What began the scene: the start of the analysis or of a segment, a cut, or a long-scene split.
    pub start: SceneStart,
    // For a cut: the histogram distance and residuals of the pair that was read as one.
    pub cut: Option<TemporalFeatures>,
    pub cut_pairs_within: usize,
    pub adjustment: f64,
    pub raw_adjustment: f64,
    pub confidence: f64,
    pub frames_analyzed: usize,
    pub contributions: Vec<Contribution>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CrfBin {
    pub crf: f32,
    pub scenes: usize,
    pub frames: u64,
}

// The CRFs over the whole analysis, weighted by frames — what the bitrate follows.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CrfDistribution {
    pub scenes: usize,
    pub frames: u64,
    pub anchor_crf: f32,
    pub mean_crf: f64,
    pub min_crf: f32,
    pub max_crf: f32,
    pub frames_below_anchor: u64,
    pub frames_at_anchor: u64,
    pub frames_above_anchor: u64,
    pub histogram: Vec<CrfBin>,
}

impl CrfDistribution {
    pub fn of(scenes: &[SceneEntry], anchor: f32) -> Self {
        let mut summary = Self { anchor_crf: anchor, scenes: scenes.len(), ..Default::default() };
        if scenes.is_empty() {
            return summary;
        }
        summary.min_crf = f32::INFINITY;
        summary.max_crf = f32::NEG_INFINITY;
        let mut weighted = 0.0;
        for scene in scenes {
            summary.frames += scene.frames;
            weighted += scene.crf as f64 * scene.frames as f64;
            summary.min_crf = summary.min_crf.min(scene.crf);
            summary.max_crf = summary.max_crf.max(scene.crf);
            match scene.crf.total_cmp(&anchor) {
                std::cmp::Ordering::Less => summary.frames_below_anchor += scene.frames,
                std::cmp::Ordering::Equal => summary.frames_at_anchor += scene.frames,
                std::cmp::Ordering::Greater => summary.frames_above_anchor += scene.frames,
            }
            match summary.histogram.iter_mut().find(|bin| bin.crf == scene.crf) {
                Some(bin) => {
                    bin.scenes += 1;
                    bin.frames += scene.frames;
                }
                None => summary.histogram.push(CrfBin { crf: scene.crf, scenes: 1, frames: scene.frames }),
            }
        }
        summary.histogram.sort_by(|a, b| a.crf.total_cmp(&b.crf));
        summary.mean_crf = if summary.frames == 0 { anchor as f64 } else { weighted / summary.frames as f64 };
        summary
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CaramelReport {
    pub model: &'static str,
    pub input: String,
    pub source: SourceContext,
    pub encode: EncodeContext,
    pub scene_config: SceneConfig,
    pub range: Option<FrameRange>,
    pub frames: u64,
    pub duration_seconds: Option<f64>,
    pub segments: usize,
    pub elapsed_ms: u128,
    pub summary: CrfDistribution,
    pub scenes: Vec<SceneEntry>,
    // The scenes as an x264 `zones` value, in the input's own frame numbers: what an ffmpeg x264
    // encode of the whole input is given to follow the same CRFs. Empty when every scene is at
    // the anchor.
    pub zones: String,
    // Filled in from `run_verification` when one was asked for, or why it could not be.
    pub verify: Option<VerifyReport>,
    pub verify_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SourceProbe {
    pub context: SourceContext,
    pub duration: Option<f64>,
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

pub fn probe_source(input: &Path) -> Result<SourceProbe, String> {
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

// Contiguous segments over [start, start + frames). With no frame count the analysis runs to the
// end of the input in one segment, since nothing says where a second one would begin.
fn plan_segments(start: u64, frames: Option<u64>, workers: usize) -> Vec<(u64, Option<u64>)> {
    let Some(frames) = frames.filter(|frames| *frames > 0) else {
        return vec![(start, None)];
    };
    let count = (frames / MIN_SEGMENT_FRAMES).clamp(1, workers.max(1) as u64);
    (0..count)
        .map(|index| {
            let from = start + index * frames / count;
            let to = start + (index + 1) * frames / count;
            (from, Some(to - from))
        })
        .collect()
}

struct SegmentResult {
    start: u64,
    decoded: u64,
    decisions: Vec<SceneDecision>,
}

// The accurate-seek target that makes `frame` the first frame decoded: half a frame early, so a
// timestamp rounded either way still lands on it and not on its neighbour.
fn seek_seconds(frame: u64, fps: f64) -> Option<String> {
    (frame > 0).then(|| format!("{:.6}", (frame as f64 - 0.5) / fps))
}

fn analyse_segment(
    ffmpeg: &Path,
    request: &SceneRequest,
    source: &SourceContext,
    start: u64,
    frames: Option<u64>,
    fps: f64,
) -> Result<SegmentResult, String> {
    let mut command = Command::new(ffmpeg);
    command.args(["-v", "error", "-nostdin", "-threads", &request.decoder_threads.to_string()]);
    if let Some(seek) = seek_seconds(start, fps) {
        command.args(["-ss", &seek]);
    }
    command.arg("-i").arg(request.input).args(["-map", "0:v:0"]);
    if let Some(frames) = frames {
        command.args(["-frames:v", &frames.to_string()]);
    }
    let mut child = command
        .args(["-an", "-sn", "-dn", "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg has no stdout")?;
    let result = (|| {
        let mut reader = match pnx264::y4m::Y4mReader::new(BufReader::with_capacity(1 << 20, stdout)) {
            Ok(reader) => reader,
            // Nothing decoded: a segment past the end of a file whose length was an estimate.
            Err(_) => return Ok(SegmentResult { start, decoded: 0, decisions: Vec::new() }),
        };
        let (width, height) = (reader.width, reader.height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let mut decider = SceneDecider::new(request.scenes, source.clone(), request.encode);
        let mut decisions = Vec::new();
        let mut decoded = 0u64;
        while let Some(frame) = reader.next_frame()? {
            let y = Plane::new(frame.y, width, height, frame.stride_y).ok_or("short luma plane")?;
            let u = Plane::new(frame.u, chroma_width, chroma_height, frame.stride_c);
            let v = Plane::new(frame.v, chroma_width, chroma_height, frame.stride_c);
            decisions.extend(decider.push(&Frame { y, u, v }));
            decoded += 1;
            if let Some(progress) = request.progress {
                progress.fetch_add(1, Ordering::Relaxed);
            }
            if decoded.is_multiple_of(CANCEL_CHECK_FRAMES) && request.cancel_file.as_ref().is_some_and(|path| path.exists()) {
                return Err("cancelled".to_string());
            }
        }
        decisions.extend(decider.finish());
        for decision in &mut decisions {
            decision.start_frame += start;
        }
        Ok(SegmentResult { start, decoded, decisions })
    })();
    child.kill().ok();
    child.wait().ok();
    result
}

// A segment's decisions as scenes: each runs to the next one's start, the last to the segment's end.
fn segment_scenes(segment: &SegmentResult, fps: f64) -> Vec<SceneEntry> {
    let end = segment.start + segment.decoded;
    segment
        .decisions
        .iter()
        .enumerate()
        .map(|(index, decision)| {
            let next = segment.decisions.get(index + 1).map_or(end, |next| next.start_frame.min(end));
            let estimate = &decision.estimate;
            SceneEntry {
                start_frame: decision.start_frame,
                frames: next.saturating_sub(decision.start_frame),
                start_seconds: decision.start_frame as f64 / fps,
                crf: decision.crf,
                start: decision.start,
                cut: decision.cut,
                cut_pairs_within: decision.cut_pairs_within,
                adjustment: estimate.adjustment,
                raw_adjustment: estimate.raw_adjustment,
                confidence: estimate.confidence,
                frames_analyzed: estimate.frames_analyzed,
                contributions: estimate.contributions.clone(),
            }
        })
        .collect()
}

// Scenes as an x264 `zones` value: runs of equal CRF merged, runs at the anchor left out (the
// encoder is already there), frame numbers inclusive as x264 counts them.
pub fn x264_zones(scenes: &[SceneEntry], anchor: f32) -> String {
    let mut runs: Vec<(u64, u64, f32)> = Vec::new();
    for scene in scenes.iter().filter(|scene| scene.frames > 0) {
        let end = scene.start_frame + scene.frames - 1;
        match runs.last_mut() {
            Some(run) if run.2 == scene.crf && run.1 + 1 == scene.start_frame => run.1 = end,
            _ => runs.push((scene.start_frame, end, scene.crf)),
        }
    }
    runs.iter()
        .filter(|run| run.2 != anchor)
        .map(|(start, end, crf)| format!("{start},{end},crf={crf}"))
        .collect::<Vec<_>>()
        .join("/")
}

// `x264_params` with `zones` set to `zones`, replacing any it already had.
pub fn with_zones(x264_params: Option<&str>, zones: &str) -> Option<String> {
    let mut pairs: Vec<&str> = x264_params
        .unwrap_or_default()
        .split(':')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("zones="))
        .collect();
    let zoned = format!("zones={zones}");
    if !zones.is_empty() {
        pairs.push(&zoned);
    }
    (!pairs.is_empty()).then(|| pairs.join(":"))
}

pub fn run_scene_analysis(request: &SceneRequest) -> Result<CaramelReport, String> {
    let started = std::time::Instant::now();
    let probe = probe_source(request.input)?;
    let fps = request.fps.filter(|fps| *fps > 0.0).unwrap_or(probe.context.fps);
    let source = SourceContext { fps, ..probe.context.clone() };
    let (start, frames) = match request.range {
        Some(range) => (range.start, Some(range.frames)),
        // An estimate, so the last segment runs to the end of the input rather than stopping at it.
        None => (0, probe.duration.map(|duration| (duration * fps).round() as u64)),
    };
    let mut segments = plan_segments(start, frames, request.workers);
    if request.range.is_none() {
        if let Some(last) = segments.last_mut() {
            last.1 = None;
        }
    }
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let results: Vec<Result<SegmentResult, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = segments
            .iter()
            .map(|&(from, length)| {
                let (ffmpeg, source) = (&ffmpeg, &source);
                scope.spawn(move || analyse_segment(ffmpeg, request, source, from, length, fps))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| Err("a segment's analysis panicked".to_string())))
            .collect()
    });
    let mut scenes = Vec::new();
    for result in results {
        scenes.extend(segment_scenes(&result?, fps));
    }
    if scenes.is_empty() {
        return Err("no frames decoded".to_string());
    }
    let frames = scenes.iter().map(|scene| scene.frames).sum();
    let anchor = request.encode.anchor_crf;
    Ok(CaramelReport {
        model: MODEL_VERSION,
        input: request.input.display().to_string(),
        source,
        encode: request.encode,
        scene_config: request.scenes.sanitised(),
        range: request.range,
        frames,
        duration_seconds: probe.duration,
        segments: segments.len(),
        elapsed_ms: started.elapsed().as_millis(),
        summary: CrfDistribution::of(&scenes, anchor),
        zones: x264_zones(&scenes, anchor),
        scenes,
        verify: None,
        verify_error: None,
    })
}

// What a chunk encoder needs to decide scenes the way this report did. Size and frame rate come
// from the frames the encoder decodes; the rest is the container's.
pub fn scene_crf(source: &SourceContext, encode: &EncodeContext, scenes: SceneConfig) -> pnx264::scenes::SceneCrf {
    pnx264::scenes::SceneCrf {
        output_height: encode.output_height,
        output_bit_depth: encode.output_bit_depth,
        scenes,
        source_bitrate: source.video_bitrate,
        source_codec: source.codec.clone(),
        source_bit_depth: source.bit_depth,
    }
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

// Ground truth for the scene CRFs: the scenes the model moved furthest from the anchor, each
// encoded at its own CRF and at every fixed CRF listed, and measured against the source. It is the
// brute-force comparison the estimate exists to replace, run small and only on request.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRequest {
    // Fixed CRFs to compare the adaptive encode with; the anchor when empty.
    pub compare_crfs: Vec<f32>,
    // The longest clip taken from one scene.
    pub clip_seconds: f64,
    pub max_scenes: usize,
    pub encoder: VerifyEncoder,
    // Where the clips are written while they are measured; removed afterwards.
    pub scratch: PathBuf,
    // Checked before every clip: a verification is minutes of encoding, and a cancel should not
    // have to wait for all of it.
    pub cancel_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyMeasure {
    pub crf: f32,
    pub mean_ssim: f64,
    pub min_ssim: f64,
    pub mean_psnr: f64,
    pub min_psnr: f64,
    pub kbps: f64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyScene {
    pub start_frame: u64,
    pub frames: u64,
    pub scene_crf: f32,
    pub adaptive: VerifyMeasure,
    pub fixed: Vec<VerifyMeasure>,
}

// One encode strategy over every clip: the scene CRFs (`crf` None) or one fixed CRF.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyTotal {
    pub crf: Option<f32>,
    pub frames: u64,
    pub mean_ssim: f64,
    pub min_ssim: f64,
    pub mean_psnr: f64,
    pub kbps: f64,
    pub bytes: u64,
    // How much smaller the adaptive clips are than this strategy's, in percent (positive = smaller).
    pub adaptive_saving_percent: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyReport {
    pub encoder: VerifyEncoder,
    pub clip_seconds: f64,
    pub scenes: Vec<VerifyScene>,
    pub adaptive: VerifyTotal,
    pub fixed: Vec<VerifyTotal>,
    pub elapsed_ms: u128,
}

// The scenes a verification measures: the ones moved furthest from the anchor, longest first among
// equals, in the order they play.
fn verify_scenes(scenes: &[SceneEntry], anchor: f32, max: usize) -> Vec<usize> {
    let mut candidates: Vec<usize> = (0..scenes.len()).filter(|&index| scenes[index].frames >= VERIFY_MIN_FRAMES).collect();
    candidates.sort_by(|&a, &b| {
        let distance = |index: usize| (scenes[index].crf - anchor).abs();
        distance(b).total_cmp(&distance(a)).then(scenes[b].frames.cmp(&scenes[a].frames))
    });
    candidates.truncate(max);
    candidates.sort_unstable();
    candidates
}

struct ClipMeasurement {
    ssim: Vec<f64>,
    psnr: Vec<f64>,
    bytes: u64,
}

impl ClipMeasurement {
    fn measure(&self, crf: f32, fps: f64) -> VerifyMeasure {
        let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
        let min = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().copied().fold(f64::INFINITY, f64::min) };
        VerifyMeasure {
            crf,
            mean_ssim: mean(&self.ssim),
            min_ssim: min(&self.ssim),
            mean_psnr: mean(&self.psnr),
            min_psnr: min(&self.psnr),
            kbps: kbps(self.bytes, self.ssim.len() as u64, fps),
            bytes: self.bytes,
        }
    }
}

fn kbps(bytes: u64, frames: u64, fps: f64) -> f64 {
    if frames == 0 || fps <= 0.0 { 0.0 } else { bytes as f64 * 8.0 / (frames as f64 / fps) / 1000.0 }
}

// One clip at one CRF: encode it, then decode the source again from the same frame and compare.
// Both passes seek accurately to the scene's first frame and take the same frame count, so they
// cover exactly the frames the scene's CRF was decided for.
fn measure_clip(
    ffmpeg: &Path,
    input: &Path,
    start: u64,
    frames: u64,
    fps: f64,
    request: &VerifyRequest,
    crf: f32,
    name: &str,
) -> Result<ClipMeasurement, String> {
    let clip = request.scratch.join(format!("{name}.mkv"));
    let seek = seek_seconds(start, fps);
    let frame_count = frames.to_string();
    let mut encode = Command::new(ffmpeg);
    encode.args(["-v", "error", "-nostdin", "-y"]);
    if let Some(seek) = &seek {
        encode.args(["-ss", seek]);
    }
    encode
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", &frame_count, "-an", "-sn", "-dn", "-pix_fmt", "yuv420p", "-c:v", "libx264"]);
    if let Some(preset) = &request.encoder.preset {
        encode.args(["-preset", preset]);
    }
    if let Some(tune) = &request.encoder.tune {
        encode.args(["-tune", tune]);
    }
    if let Some(params) = with_zones(request.encoder.x264_params.as_deref(), "") {
        encode.args(["-x264-params", &params]);
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
    measure.current_dir(&request.scratch).args(["-v", "error", "-nostdin"]);
    if let Some(seek) = &seek {
        measure.args(["-ss", seek]);
    }
    measure
        .arg("-i")
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

// Every clip of one strategy pooled: SSIM and PSNR per frame, the bitrate over all of them.
fn total(crf: Option<f32>, clips: &[&ClipMeasurement], fps: f64) -> VerifyTotal {
    let ssim: Vec<f64> = clips.iter().flat_map(|clip| clip.ssim.iter().copied()).collect();
    let psnr: Vec<f64> = clips.iter().flat_map(|clip| clip.psnr.iter().copied()).collect();
    let bytes = clips.iter().map(|clip| clip.bytes).sum();
    let frames = ssim.len() as u64;
    let pooled = ClipMeasurement { ssim, psnr, bytes }.measure(crf.unwrap_or_default(), fps);
    VerifyTotal {
        crf,
        frames,
        mean_ssim: pooled.mean_ssim,
        min_ssim: pooled.min_ssim,
        mean_psnr: pooled.mean_psnr,
        kbps: pooled.kbps,
        bytes,
        adaptive_saving_percent: 0.0,
    }
}

pub fn run_verification(report: &CaramelReport, request: &VerifyRequest) -> Result<VerifyReport, String> {
    let started = std::time::Instant::now();
    let anchor = report.encode.anchor_crf;
    let mut compare = if request.compare_crfs.is_empty() { vec![anchor] } else { request.compare_crfs.clone() };
    compare.retain(|crf| crf.is_finite());
    compare.sort_by(|a, b| a.total_cmp(b));
    compare.dedup();
    let chosen = verify_scenes(&report.scenes, anchor, request.max_scenes.max(1));
    if chosen.is_empty() {
        return Err(format!("no scene is at least {VERIFY_MIN_FRAMES} frames long"));
    }
    std::fs::create_dir_all(&request.scratch).map_err(|e| format!("verification scratch: {e}"))?;
    // The measurement runs inside the scratch directory, so every path it is handed — the clip
    // included — has to be absolute.
    let scratch = std::fs::canonicalize(&request.scratch).map_err(|e| format!("verification scratch: {e}"))?;
    let request = &VerifyRequest { scratch, ..request.clone() };
    let input = std::fs::canonicalize(&report.input).unwrap_or_else(|_| PathBuf::from(&report.input));
    // `DB/bin/ffmpeg` is relative to the process, and the measurement runs from the scratch
    // directory: without this it cannot start at all. A bare `ffmpeg` is found on PATH either way.
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let ffmpeg = if ffmpeg.components().count() > 1 { std::fs::canonicalize(&ffmpeg).unwrap_or(ffmpeg) } else { ffmpeg };
    let fps = report.source.fps;
    let clip_limit = (request.clip_seconds * fps).round().max(1.0) as u64;
    let fail = |error: String| {
        std::fs::remove_dir_all(&request.scratch).ok();
        Err(error)
    };
    // Per chosen scene: the adaptive clip, then one per fixed CRF (shared when it is the same CRF).
    let mut measured: Vec<(usize, ClipMeasurement, Vec<usize>)> = Vec::new();
    let mut fixed_clips: Vec<ClipMeasurement> = Vec::new();
    for (scene_index, &index) in chosen.iter().enumerate() {
        let scene = &report.scenes[index];
        let frames = scene.frames.min(clip_limit);
        let clip_at = |crf: f32, name: String| {
            if request.cancel_file.as_ref().is_some_and(|path| path.exists()) {
                return Err("cancelled".to_string());
            }
            measure_clip(&ffmpeg, &input, scene.start_frame, frames, fps, request, crf, &name)
        };
        let adaptive = match clip_at(scene.crf, format!("scene-{scene_index}")) {
            Ok(clip) => clip,
            Err(error) => return fail(error),
        };
        let mut fixed = Vec::with_capacity(compare.len());
        for (crf_index, &crf) in compare.iter().enumerate() {
            let clip = if crf == scene.crf {
                ClipMeasurement { ssim: adaptive.ssim.clone(), psnr: adaptive.psnr.clone(), bytes: adaptive.bytes }
            } else {
                match clip_at(crf, format!("scene-{scene_index}-fixed-{crf_index}")) {
                    Ok(clip) => clip,
                    Err(error) => return fail(error),
                }
            };
            fixed.push(fixed_clips.len());
            fixed_clips.push(clip);
        }
        measured.push((index, adaptive, fixed));
    }
    std::fs::remove_dir(&request.scratch).ok();

    let scenes = measured
        .iter()
        .map(|(index, adaptive, fixed)| {
            let scene = &report.scenes[*index];
            VerifyScene {
                start_frame: scene.start_frame,
                frames: adaptive.ssim.len() as u64,
                scene_crf: scene.crf,
                adaptive: adaptive.measure(scene.crf, fps),
                fixed: fixed.iter().zip(&compare).map(|(&clip, &crf)| fixed_clips[clip].measure(crf, fps)).collect(),
            }
        })
        .collect();
    let adaptive = total(None, &measured.iter().map(|(_, clip, _)| clip).collect::<Vec<_>>(), fps);
    let fixed = compare
        .iter()
        .enumerate()
        .map(|(crf_index, &crf)| {
            let clips: Vec<&ClipMeasurement> = measured.iter().map(|(_, _, fixed)| &fixed_clips[fixed[crf_index]]).collect();
            let mut strategy = total(Some(crf), &clips, fps);
            if strategy.bytes > 0 {
                strategy.adaptive_saving_percent = (1.0 - adaptive.bytes as f64 / strategy.bytes as f64) * 100.0;
            }
            strategy
        })
        .collect();
    Ok(VerifyReport {
        encoder: request.encoder.clone(),
        clip_seconds: request.clip_seconds,
        scenes,
        adaptive,
        fixed,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

// One line for a tool log.
pub fn summary_line(report: &CaramelReport) -> String {
    let summary = &report.summary;
    format!(
        "caramel analysis ({}): {} scenes over {} frames, crf {}..{} (mean {:.2}, anchor {}; {} frames below, {} above) in {} ms over {} segments",
        report.model,
        summary.scenes,
        summary.frames,
        summary.min_crf,
        summary.max_crf,
        summary.mean_crf,
        summary.anchor_crf,
        summary.frames_below_anchor,
        summary.frames_above_anchor,
        report.elapsed_ms,
        report.segments,
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

    fn scene(start_frame: u64, frames: u64, crf: f32) -> SceneEntry {
        SceneEntry {
            start_frame,
            frames,
            start_seconds: 0.0,
            crf,
            start: SceneStart::Cut,
            cut: None,
            cut_pairs_within: 0,
            adjustment: 0.0,
            raw_adjustment: 0.0,
            confidence: 1.0,
            frames_analyzed: 12,
            contributions: Vec::new(),
        }
    }

    #[test]
    fn zones_merge_equal_runs_and_leave_the_anchor_out() {
        let scenes = [scene(0, 100, 17.0), scene(100, 50, 19.5), scene(150, 30, 19.5), scene(180, 20, 15.0)];
        assert_eq!(x264_zones(&scenes, 17.0), "100,179,crf=19.5/180,199,crf=15");
        assert_eq!(x264_zones(&scenes[..1], 17.0), "");
    }

    #[test]
    fn zones_replace_any_the_params_had() {
        assert_eq!(
            with_zones(Some("aq-mode=3:zones=0,9,crf=30:deblock=-1,-1"), "0,99,crf=18").as_deref(),
            Some("aq-mode=3:deblock=-1,-1:zones=0,99,crf=18")
        );
        assert_eq!(with_zones(None, "0,99,crf=18").as_deref(), Some("zones=0,99,crf=18"));
        assert_eq!(with_zones(Some("zones=1,2,crf=3"), ""), None);
    }

    #[test]
    fn the_distribution_is_weighted_by_frames() {
        let scenes = [scene(0, 300, 16.0), scene(300, 100, 20.0), scene(400, 100, 17.0)];
        let summary = CrfDistribution::of(&scenes, 17.0);
        assert_eq!((summary.min_crf, summary.max_crf, summary.scenes, summary.frames), (16.0, 20.0, 3, 500));
        assert!((summary.mean_crf - (16.0 * 300.0 + 20.0 * 100.0 + 17.0 * 100.0) / 500.0).abs() < 1e-9);
        assert_eq!((summary.frames_below_anchor, summary.frames_at_anchor, summary.frames_above_anchor), (300, 100, 100));
        assert_eq!(summary.histogram.iter().map(|bin| bin.crf).collect::<Vec<_>>(), vec![16.0, 17.0, 20.0]);
    }

    #[test]
    fn segments_are_contiguous_and_not_too_short() {
        assert_eq!(plan_segments(0, Some(1000), 4), vec![(0, Some(1000))]);
        let segments = plan_segments(100, Some(34_000), 4);
        assert_eq!(segments.len(), 4);
        assert_eq!(segments[0].0, 100);
        assert!(segments.windows(2).all(|pair| pair[0].0 + pair[0].1.unwrap() == pair[1].0));
        let last = segments.last().unwrap();
        assert_eq!(last.0 + last.1.unwrap(), 34_100);
        assert_eq!(plan_segments(0, None, 4), vec![(0, None)]);
    }

    #[test]
    fn verification_takes_the_scenes_moved_furthest() {
        let scenes = [scene(0, 100, 17.0), scene(100, 8, 13.0), scene(108, 40, 19.0), scene(148, 90, 15.0), scene(238, 60, 19.0)];
        // The 8-frame scene is too short to measure; 19.0 twice is a tie broken by length.
        assert_eq!(verify_scenes(&scenes, 17.0, 2), vec![3, 4]);
        assert_eq!(verify_scenes(&scenes, 17.0, 10), vec![0, 2, 3, 4]);
    }

    #[test]
    fn a_scene_starts_decoding_on_its_own_frame() {
        assert_eq!(seek_seconds(0, 24.0), None);
        assert_eq!(seek_seconds(24, 24.0).as_deref(), Some("0.979167"));
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

    // The planner writes `--aot-crf` in pnx264 and pnmpeg reads it here; a caramel encoder has to
    // arrive as caramel at the same anchor, and a fixed one as that CRF.
    #[test]
    fn the_planner_crf_argument_parses_back() {
        let mut encoder = pnx264::Config { crf: 16.5, ..Default::default() };
        assert_eq!(pnx264::planner::aot_crf_arg(&encoder).parse::<AotCrf>(), Ok(AotCrf::Fixed(16.5)));
        encoder.caramel = Some(Default::default());
        assert_eq!(
            pnx264::planner::aot_crf_arg(&encoder).parse::<AotCrf>(),
            Ok(AotCrf::CaramelAnalysis { anchor: Some(16.5) })
        );
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
