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
    Contribution, EncodeContext, MetricVote, SceneConfig, SceneDecider, SceneDecision,
    SceneStart, SourceContext, TemporalFeatures, WindowFeatures,
};
use pnx264::scenes::SceneSchedule;
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
    // The x264 settings the encode will use, to test-encode far raises with
    // (`caramel_kernel::probe`); None decides without probes. Used only when the encode writes
    // 8-bit frames at the source's own size, the frames this analysis decodes.
    pub probe: Option<pnx264::Config>,
    // The same settings, for every scene's fast encode when the model reads one
    // (`caramel_kernel::fast`); None decides scenes without, as the built-in model does. Used under
    // the same condition as `probe`.
    pub fast: Option<pnx264::Config>,
    // The model to decide with instead of the one this process has active, which stays as it is
    // for every other encode: `/caramel-analysis-encode` decides with one that reads fast encodes.
    pub model: Option<std::sync::Arc<caramel_kernel::curve::Model>>,
    // Where segments report scenes as they decide them, for a caller that shows the analysis as it
    // goes (`LiveScenes::snapshot`).
    pub live: Option<&'a LiveScenes>,
}

// Scenes as the segments decide them, read while the analysis runs. Segments run side by side, so
// the latest decisions come from several places in the video at once.
#[derive(Default)]
pub struct LiveScenes {
    state: std::sync::Mutex<LiveState>,
}

#[derive(Default)]
struct LiveState {
    // Each segment, by its first frame: its scenes so far (first frame, CRF) and how far it has
    // decoded, which is where its last scene ends for now.
    segments: std::collections::BTreeMap<u64, (Vec<(u64, f32)>, u64)>,
    recent: std::collections::VecDeque<LiveScene>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveScene {
    pub start_frame: u64,
    pub crf: f32,
    // The CRF the model chose before a test encode pulled it back, when one did.
    pub proposed: Option<f32>,
    // What the scene's fast encode read, when it had one.
    pub fast_xpsnr: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LiveSnapshot {
    pub scenes: usize,
    // Frames the decided scenes cover, and their CRF weighted by those frames.
    pub frames: u64,
    pub mean_crf: Option<f64>,
    // The most recent decisions, oldest first.
    pub recent: Vec<LiveScene>,
}

// How many of the latest decisions a snapshot carries.
pub const LIVE_RECENT: usize = 10;

impl LiveScenes {
    fn record(&self, segment: u64, decided: &[SceneDecision], decoded: u64) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = state.segments.entry(segment).or_default();
        entry.1 = decoded;
        entry.0.extend(decided.iter().map(|decision| (segment + decision.start_frame, decision.crf)));
        for decision in decided {
            let proposed = decision.estimate.probe.as_ref().map(|probe| probe.crf).filter(|crf| *crf > decision.crf);
            let fast_xpsnr = decision.features.fast_encode.as_ref().map(|fast| fast.xpsnr);
            state.recent.push_back(LiveScene { start_frame: segment + decision.start_frame, crf: decision.crf, proposed, fast_xpsnr });
            if state.recent.len() > LIVE_RECENT {
                state.recent.pop_front();
            }
        }
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut scenes, mut frames, mut weighted) = (0usize, 0u64, 0.0f64);
        for (segment, (decided, decoded)) in &state.segments {
            let end = segment + decoded;
            for (index, (start, crf)) in decided.iter().enumerate() {
                let next = decided.get(index + 1).map_or(end, |next| next.0.min(end));
                let length = next.saturating_sub(*start);
                scenes += 1;
                frames += length;
                weighted += *crf as f64 * length as f64;
            }
        }
        LiveSnapshot { scenes, frames, mean_crf: (frames > 0).then(|| weighted / frames as f64), recent: state.recent.iter().cloned().collect() }
    }
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
    // What each quality metric would let the scene's CRF move by, and what of that was applied.
    pub votes: Vec<MetricVote>,
    pub contributions: Vec<Contribution>,
    // The test encode of a far raise: the CRF the model chose, what it measured and where it settled.
    pub probe: Option<caramel_kernel::Probe>,
    // What was measured on the scene's opening frames, as the model was given it: with `verify`,
    // a report is a row of calibration data per scene.
    pub features: WindowFeatures,
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
    pub model: String,
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
    // The whole input encoded at these scene CRFs and uploaded, when that was asked for
    // (`/caramel-analysis-encode`), or why it could not be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted: Option<HostedEncode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_error: Option<String>,
    // Every scene's decision as an encode records it (`caramel.decisions`), for the encodes made
    // from this report to keep for verification.
    #[serde(skip)]
    pub decisions: Vec<pnx264::scenes::DecisionRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostedEncode {
    // The file on Drive, and the same encode as Lumiere HLS (kept 12 hours); each None when its
    // publication failed, with the reason in `errors`.
    pub drive: Option<String>,
    pub hls: Option<String>,
    pub errors: Vec<String>,
    pub bytes: u64,
    // Of the video alone, as the scene CRFs made it.
    pub kbps: f64,
    pub encode_seconds: f64,
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
pub(crate) fn seek_seconds(frame: u64, fps: f64) -> Option<String> {
    (frame > 0).then(|| format!("{:.6}", (frame as f64 - 0.5) / fps))
}

fn analyse_segment(
    ffmpeg: &Path,
    request: &SceneRequest,
    source: &SourceContext,
    start: u64,
    frames: Option<u64>,
    fps: f64,
    trust: &pnx264::scenes::TrustShare,
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
        let mut decider = SceneDecider::new(request.scenes, source.clone(), request.encode);
        if let Some(model) = &request.model {
            decider = decider.with_model(Some(model.clone()));
        }
        let encode = &request.encode;
        let measurable = encode.output_bit_depth == 8 && encode.output_height.is_none_or(|output| output as usize == height);
        let probe = request.probe.as_ref().filter(|_| measurable);
        let fast = request.fast.as_ref().filter(|_| measurable);
        let (fps_num, fps_den) = fps_ratio(fps);
        let mut schedule = SceneSchedule::from_decider(decider, width, height, fps_num, fps_den, encode.anchor_crf, probe, fast).with_trust(trust.clone());
        let mut decoded = 0u64;
        let mut reported = 0usize;
        while let Some(frame) = reader.next_frame()? {
            schedule.push(&frame)?;
            while schedule.ready().is_some() {}
            decoded += 1;
            if let Some(live) = request.live.filter(|_| schedule.decisions().len() > reported) {
                live.record(start, &schedule.decisions()[reported..], decoded);
                reported = schedule.decisions().len();
            }
            if let Some(progress) = request.progress {
                progress.fetch_add(1, Ordering::Relaxed);
            }
            if decoded.is_multiple_of(CANCEL_CHECK_FRAMES) && request.cancel_file.as_ref().is_some_and(|path| path.exists()) {
                return Err("cancelled".to_string());
            }
        }
        schedule.finish();
        if let Some(live) = request.live {
            live.record(start, &schedule.decisions()[reported..], decoded);
        }
        let mut decisions = schedule.decisions().to_vec();
        for decision in &mut decisions {
            decision.start_frame += start;
        }
        Ok(SegmentResult { start, decoded, decisions })
    })();
    child.kill().ok();
    child.wait().ok();
    result
}

// The settings a far raise is test-encoded with for an encode made with `preset`: the preset's own
// x264 settings, one thread each since segments already run side by side. None for a preset that
// does not encode with x264, whose CRF a libx264 test encode could not check.
pub fn probe_encoder(preset: &crate::lib::mpeg::preset::ResolvedPreset) -> Option<pnx264::Config> {
    let settings = preset.x264_settings();
    preset.encodes_with_x264().then(|| pnx264::Config {
        threads: 1,
        preset: settings.preset,
        tune: settings.tune,
        profile: settings.profile,
        level: settings.level,
        x264_params: settings.x264_params,
        ..Default::default()
    })
}

// A frame rate as the ratio x264 and XPSNR take: NTSC rates as their thousand-and-one ratios.
pub(crate) fn fps_ratio(fps: f64) -> (u32, u32) {
    let ntsc = fps * 1001.0;
    if (ntsc - ntsc.round()).abs() < 0.05 && ntsc.round() % 1000.0 == 0.0 {
        (ntsc.round() as u32, 1001)
    } else {
        ((fps * 1000.0).round() as u32, 1000)
    }
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
                votes: estimate.votes.clone(),
                contributions: estimate.contributions.clone(),
                probe: estimate.probe.clone(),
                features: decision.features.clone(),
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

// What `encode_scene_crfs` encodes with: the preset's x264 settings, and the output it scales to.
pub struct SceneEncodeRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub preset: Option<String>,
    pub tune: Option<String>,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub x264_params: Option<String>,
    pub cancel_file: Option<PathBuf>,
    // Frames written so far, for a caller that shows progress.
    pub progress: Option<&'a AtomicU64>,
}

// The video of `request.input` encoded with x264 at the report's scene CRFs (its `zones`, around
// its anchor), as an MP4 with the first audio track in AAC: what an encode deciding exactly as the
// report did gives, whichever model decided it, in a file Drive previews and HLS can segment as it
// is. The output's size and bit depth are the report's encode's.
pub fn encode_scene_crfs(report: &CaramelReport, request: &SceneEncodeRequest) -> Result<(), String> {
    let ffmpeg = resolve_runtime_binary("ffmpeg");
    let mut command = Command::new(&ffmpeg);
    command.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(request.input);
    command.args(["-map", "0:v:0", "-map", "0:a:0?", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]);
    if let Some(height) = report.encode.output_height.filter(|height| *height != report.source.height) {
        command.args(["-vf", &format!("scale=-2:{height}")]);
    }
    command.args(["-c:v", "libx264", "-crf", &report.encode.anchor_crf.to_string()]);
    for (flag, value) in [("-preset", &request.preset), ("-tune", &request.tune), ("-profile:v", &request.profile), ("-level", &request.level)] {
        if let Some(value) = value {
            command.args([flag, value.as_str()]);
        }
    }
    if let Some(params) = with_zones(request.x264_params.as_deref(), &report.zones) {
        command.args(["-x264-params", &params]);
    }
    let pixel_format = if report.encode.output_bit_depth > 8 { "yuv420p10le" } else { "yuv420p" };
    command.args(["-pix_fmt", pixel_format, "-progress", "pipe:1", "-nostats"]).arg(request.output);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    let stderr = child.stderr.take();
    // Read aside, so a chatty ffmpeg never blocks on a full pipe; its last line says why it failed.
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        }
        text
    });
    let stdout = child.stdout.take().ok_or("ffmpeg has no stdout")?;
    let mut cancelled = false;
    for line in std::io::BufRead::lines(BufReader::new(stdout)).map_while(Result::ok) {
        if let (Some(frames), Some(progress)) = (line.strip_prefix("frame=").and_then(|frame| frame.trim().parse::<u64>().ok()), request.progress) {
            progress.store(frames, Ordering::Relaxed);
        }
        if request.cancel_file.as_ref().is_some_and(|file| file.exists()) {
            cancelled = true;
            let _ = child.kill();
            break;
        }
    }
    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    let errors = errors.join().unwrap_or_default();
    if cancelled {
        return Err("cancelled".to_string());
    }
    if !status.success() {
        let reason = errors.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no message").trim().to_string();
        return Err(format!("ffmpeg failed ({status}): {reason}"));
    }
    Ok(())
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
    // Segments run side by side, so a source's trust is what the segments have probed so far,
    // wherever in the video that was.
    let trust = pnx264::scenes::TrustShare::default();
    let results: Vec<Result<SegmentResult, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = segments
            .iter()
            .map(|&(from, length)| {
                let (ffmpeg, source, trust) = (&ffmpeg, &source, &trust);
                scope.spawn(move || analyse_segment(ffmpeg, request, source, from, length, fps, trust))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| Err("a segment's analysis panicked".to_string())))
            .collect()
    });
    let mut scenes = Vec::new();
    let mut decisions = Vec::new();
    for result in results {
        let result = result?;
        scenes.extend(segment_scenes(&result, fps));
        decisions.extend(pnx264::scenes::DecisionRecord::from_decisions(&result.decisions, result.start + result.decoded));
    }
    if scenes.is_empty() {
        return Err("no frames decoded".to_string());
    }
    let frames = scenes.iter().map(|scene| scene.frames).sum();
    let anchor = request.encode.anchor_crf;
    Ok(CaramelReport {
        model: request.model.as_ref().map(|model| model.version.clone()).unwrap_or_else(caramel_kernel::model_version),
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
        hosted: None,
        hosted_error: None,
        decisions,
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
        model: caramel_kernel::curve::active().map(|model| model.version.clone()),
        probe: true,
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

pub(crate) fn stats_column(path: &Path, key: &str) -> Vec<f64> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| stat_value(line, key))
        .collect()
}

pub(crate) fn run_quiet(command: &mut Command, what: &str) -> Result<(), String> {
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
    pub sample: VerifySample,
    pub encoder: VerifyEncoder,
    // Where the clips are written while they are measured; removed afterwards.
    pub scratch: PathBuf,
    // Checked before every clip: a verification is minutes of encoding, and a cancel should not
    // have to wait for all of it.
    pub cancel_file: Option<PathBuf>,
}

// Which scenes a verification measures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifySample {
    // The scenes the model moved furthest from the anchor: was each move justified?
    #[default]
    Moved,
    // Scenes at even steps through the video, whatever the model did with them. With a ladder of
    // fixed CRFs this is ground truth for the source: the CRF each scene should have had.
    Spread,
}

impl VerifySample {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "moved" => Some(Self::Moved),
            "spread" => Some(Self::Spread),
            _ => None,
        }
    }
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
    // What the model's votes are fitted to, over the frames after the clip's first: a clip opens
    // on a keyframe that an encode of the whole video would not have there, and that one frame is
    // far better than the rest. SSIM as −10·log10(1 − mean); XPSNR pooled over the frames' errors
    // and over the worst tenth of them. The XPSNR pair is absent when this ffmpeg has no `xpsnr`
    // filter (it arrived in 7.1).
    pub ssim_db: f64,
    pub xpsnr: Option<f64>,
    pub xpsnr_low: Option<f64>,
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
    // The first line of `ffmpeg -version`: which build encoded and measured.
    pub ffmpeg: String,
    // The depth the clips were compared at: 8, or 10 for a deeper source.
    pub reference_bit_depth: u8,
    pub sample: VerifySample,
    pub clip_seconds: f64,
    pub scenes: Vec<VerifyScene>,
    pub adaptive: VerifyTotal,
    pub fixed: Vec<VerifyTotal>,
    pub elapsed_ms: u128,
}

// The scenes a verification measures: the ones moved furthest from the anchor, longest first among
// equals, taken in turn from below and above the anchor, in the order they play. Ranking by
// distance alone filled every slot from whichever side the model pushes further — on a real
// episode, six banding scenes at −2 to −2.5 — and left the scenes it raised unmeasured.
fn verify_scenes(scenes: &[SceneEntry], anchor: f32, max: usize, sample: VerifySample) -> Vec<usize> {
    if sample == VerifySample::Spread {
        return spread_scenes(scenes, max);
    }
    let ranked = |keep: &dyn Fn(f32) -> bool| {
        let mut side: Vec<usize> =
            (0..scenes.len()).filter(|&index| scenes[index].frames >= VERIFY_MIN_FRAMES && keep(scenes[index].crf)).collect();
        side.sort_by(|&a, &b| {
            let distance = |index: usize| (scenes[index].crf - anchor).abs();
            distance(b).total_cmp(&distance(a)).then(scenes[b].frames.cmp(&scenes[a].frames))
        });
        side
    };
    let mut below = ranked(&|crf| crf < anchor).into_iter();
    let mut above = ranked(&|crf| crf > anchor).into_iter();
    let mut chosen = Vec::new();
    loop {
        let (a, b) = (below.next(), above.next());
        if a.is_none() && b.is_none() {
            break;
        }
        chosen.extend(a.into_iter().chain(b));
    }
    chosen.extend(ranked(&|crf| crf == anchor));
    chosen.truncate(max);
    chosen.sort_unstable();
    chosen
}

// Scenes long enough to measure, at even steps through the video: the middle of each of `max`
// equal runs of them. Nothing about the scene decides whether it is taken, so what is measured
// says how the whole source behaves and not only where the model acted.
fn spread_scenes(scenes: &[SceneEntry], max: usize) -> Vec<usize> {
    let eligible: Vec<usize> = (0..scenes.len()).filter(|&index| scenes[index].frames >= VERIFY_MIN_FRAMES).collect();
    if eligible.len() <= max {
        return eligible;
    }
    (0..max).map(|slot| eligible[(2 * slot + 1) * eligible.len() / (2 * max)]).collect()
}

#[derive(Clone)]
struct ClipMeasurement {
    ssim: Vec<f64>,
    psnr: Vec<f64>,
    // Empty when XPSNR was not measured.
    xpsnr: Vec<f64>,
    bytes: u64,
}

impl ClipMeasurement {
    fn measure(&self, crf: f32, fps: f64) -> VerifyMeasure {
        let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
        let min = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().copied().fold(f64::INFINITY, f64::min) };
        // Without the opening keyframe, unless that is all there is.
        fn settled(values: &[f64]) -> &[f64] {
            if values.len() > 1 { &values[1..] } else { values }
        }
        let xpsnr = settled(&self.xpsnr);
        let mut worst = xpsnr.to_vec();
        worst.sort_by(|a, b| a.total_cmp(b));
        worst.truncate((xpsnr.len() / 10).max(1));
        VerifyMeasure {
            crf,
            mean_ssim: mean(&self.ssim),
            min_ssim: min(&self.ssim),
            mean_psnr: mean(&self.psnr),
            min_psnr: min(&self.psnr),
            kbps: kbps(self.bytes, self.ssim.len() as u64, fps),
            bytes: self.bytes,
            ssim_db: -10.0 * (1.0 - mean(settled(&self.ssim))).max(1e-6).log10(),
            xpsnr: (!xpsnr.is_empty())
                .then(|| -10.0 * mean(&xpsnr.iter().map(|db| 10f64.powf(-db / 10.0)).collect::<Vec<_>>()).log10()),
            xpsnr_low: (!xpsnr.is_empty()).then(|| mean(&worst)),
        }
    }
}

fn kbps(bytes: u64, frames: u64, fps: f64) -> f64 {
    if frames == 0 || fps <= 0.0 { 0.0 } else { bytes as f64 * 8.0 / (frames as f64 / fps) / 1000.0 }
}

// The filter graph that compares an encoded clip (input 1) with the source it was cut from (input
// 0, already sought to the clip's first frame). ffmpeg's `ssim` and `psnr` pair frames by
// timestamp, and the two streams' timestamps are not the same numbers: Matroska stores them in
// whole milliseconds, a frame at 23.976 fps lasts 41.708 of them, and the clip's are rounded from
// zero where the source's are rounded from wherever the scene starts. Rebased to zero the two
// drift a millisecond apart, and a frame whose partner is a millisecond late is compared with the
// partner's neighbour. Whether that happens depends on where in the second the scene starts. On a
// still it changes nothing; on a moving shot it read SSIM 0.959 for an encode that measures 0.997,
// and the same at every CRF. So both sides are renumbered: frame n is at n seconds.
//
// A source deeper than 8 bits is compared at 10: the encode is raised to it, the source is not
// brought down. Bringing it down dithers it, and the encoder is then marked down for not
// reproducing the dither: the same encodes of a 10-bit clip read 1 to 2 dB of XPSNR lower against
// its dithered frames than against the clip itself at CRF 18 and above, and the same at CRF 15,
// where the dither survives the encode. Against the source, encodes of dithered and of plainly
// rounded frames measure the same (within 0.3 dB on four kinds of content), which is the sense in
// which dither is not something the encode has to carry.
fn measurement_graph(frames: u64, deep: bool, ssim_file: &str, psnr_file: &str, xpsnr_file: Option<&str>) -> String {
    const BY_INDEX: &str = "settb=1/1000,setpts=N*1000";
    let format = if deep { "yuv420p10le" } else { "yuv420p" };
    let raised = if deep { "format=yuv420p10le," } else { "" };
    let (ways, third, xpsnr) = match xpsnr_file {
        Some(file) => (3, ("[e3]", "[r3]"), format!(";[e3][r3]xpsnr=stats_file={file}:shortest=1")),
        None => (2, ("", ""), String::new()),
    };
    format!(
        "[1:v]{raised}{BY_INDEX},split={ways}[e1][e2]{};\
         [0:v]trim=end_frame={frames},format={format},{BY_INDEX},split={ways}[r1][r2]{};\
         [e1][r1]ssim=stats_file={ssim_file}:shortest=1;[e2][r2]psnr=stats_file={psnr_file}:shortest=1{xpsnr}",
        third.0, third.1
    )
}

// One frame's XPSNR out of a line of the `xpsnr` filter's stats file (`n: 1  XPSNR y: 46.2
// XPSNR u: 53.7  XPSNR v: 52.2`): the three planes' errors weighted 4:1:1, as the ground truth
// the model is fitted to weighted them. The file's closing average line is not a frame.
pub(crate) fn xpsnr_frame(line: &str) -> Option<f64> {
    if !line.trim_start().starts_with("n:") {
        return None;
    }
    let error = |plane: &str| {
        let token = line.split(plane).nth(1)?.split_whitespace().next()?;
        let db = if token.starts_with("inf") { 100.0 } else { token.parse::<f64>().ok()?.min(100.0) };
        Some(10f64.powf(-db / 10.0))
    };
    let pooled = (4.0 * error("XPSNR y:")? + error("XPSNR u:")? + error("XPSNR v:")?) / 6.0;
    Some(-10.0 * pooled.log10())
}

// Whether this ffmpeg has a filter. `-h filter=<name>` describes one it has under `Filter <name>`
// and says `Unknown filter` otherwise.
pub(crate) fn has_filter(ffmpeg: &Path, name: &str) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-v", "error", "-h", &format!("filter={name}")])
        .stdin(Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("Filter {name}")))
        .unwrap_or(false)
}

pub(crate) fn ffmpeg_version(ffmpeg: &Path) -> String {
    Command::new(ffmpeg)
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .and_then(|output| String::from_utf8_lossy(&output.stdout).lines().next().map(|line| line.trim().to_string()))
        .unwrap_or_default()
}

// How much of the source the measurement may read, from the clip's first frame. `trim` ends the
// comparison after the clip's last frame, but not every ffmpeg stops reading there: on the
// production server a clip took time in proportion to what was left of the file after it, half a
// minute for a clip from an episode's first minute and seven minutes for twelve clips, where the
// same twelve take four seconds once the reading is bounded. Two frames more than the clip, so
// that rounding cannot take a frame the comparison needs.
pub(crate) fn read_limit_seconds(frames: u64, fps: f64) -> String {
    format!("{:.6}", (frames + 2) as f64 / fps)
}

// The encode's own renumbering: frame n at n frame-durations. Left to itself ffmpeg fits the frames
// to their timestamps, repeating one where the source has a gap and dropping one where two share a
// timestamp — a join between two files, a variable-rate section — and the clip then holds other
// frames than the source it is compared with: three frames of a scene read SSIM 0.64 at every CRF.
// On regular timestamps the encode is the same to the byte.
const ENCODE_BY_INDEX: &str = "setpts=N/FRAME_RATE/TB";

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
    deep: bool,
    xpsnr: bool,
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
        .args(["-map", "0:v:0", "-frames:v", &frame_count, "-an", "-sn", "-dn", "-vf", ENCODE_BY_INDEX])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libx264"]);
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
    let (ssim_file, psnr_file, xpsnr_file) = (format!("{name}.ssim"), format!("{name}.psnr"), format!("{name}.xpsnr"));
    let graph = measurement_graph(frames, deep, &ssim_file, &psnr_file, xpsnr.then_some(xpsnr_file.as_str()));
    let mut measure = Command::new(ffmpeg);
    measure.current_dir(&request.scratch).args(["-v", "error", "-nostdin"]);
    if let Some(seek) = &seek {
        measure.args(["-ss", seek]);
    }
    measure
        .args(["-t", &read_limit_seconds(frames, fps)])
        .arg("-i")
        .arg(input)
        .arg("-i")
        .arg(&clip)
        .args(["-lavfi", &graph, "-f", "null", "-"]);
    run_quiet(&mut measure, "the verification measurement")?;
    let measured = ClipMeasurement {
        ssim: stats_column(&request.scratch.join(&ssim_file), "All"),
        psnr: stats_column(&request.scratch.join(&psnr_file), "psnr_avg"),
        xpsnr: std::fs::read_to_string(request.scratch.join(&xpsnr_file))
            .unwrap_or_default()
            .lines()
            .filter_map(xpsnr_frame)
            .collect(),
        bytes,
    };
    for file in [clip, request.scratch.join(&ssim_file), request.scratch.join(&psnr_file), request.scratch.join(&xpsnr_file)] {
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
    let pooled = ClipMeasurement { ssim, psnr, xpsnr: Vec::new(), bytes }.measure(crf.unwrap_or_default(), fps);
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
    let chosen = verify_scenes(&report.scenes, anchor, request.max_scenes.max(1), request.sample);
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
    let xpsnr = has_filter(&ffmpeg, "xpsnr");
    let deep = report.source.bit_depth > 8;
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
            measure_clip(&ffmpeg, &input, scene.start_frame, frames, fps, request, deep, xpsnr, crf, &name)
        };
        let adaptive = match clip_at(scene.crf, format!("scene-{scene_index}")) {
            Ok(clip) => clip,
            Err(error) => return fail(error),
        };
        let mut fixed = Vec::with_capacity(compare.len());
        for (crf_index, &crf) in compare.iter().enumerate() {
            let clip = if crf == scene.crf {
                adaptive.clone()
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
        ffmpeg: ffmpeg_version(&ffmpeg),
        reference_bit_depth: if deep { 10 } else { 8 },
        sample: request.sample,
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

    // A short generated clip, analysed and then encoded at its scene CRFs: the output is an MP4
    // with the video and the AAC audio, at the source's size, and what the analysis showed live
    // ends as the report says. Skipped where there is no ffmpeg.
    #[test]
    fn a_report_encodes_at_its_scene_crfs() {
        let ffmpeg = resolve_runtime_binary("ffmpeg");
        if Command::new(&ffmpeg).arg("-version").output().map(|output| !output.status.success()).unwrap_or(true) {
            return;
        }
        let directory = std::env::temp_dir().join(format!("caramel-encode-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let (input, output) = (directory.join("in.mkv"), directory.join("out.mp4"));
        let made = Command::new(&ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=24:duration=2", "-f", "lavfi", "-i", "sine=duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "flac", "-shortest"])
            .arg(&input)
            .status()
            .unwrap();
        assert!(made.success());
        let live = LiveScenes::default();
        let report = run_scene_analysis(&SceneRequest {
            input: &input,
            range: None,
            fps: None,
            encode: EncodeContext::anchored(20.0),
            scenes: SceneConfig::default(),
            workers: 1,
            decoder_threads: 1,
            cancel_file: None,
            progress: None,
            probe: None,
            fast: None,
            model: None,
            live: Some(&live),
        })
        .unwrap();
        let snapshot = live.snapshot();
        assert_eq!(snapshot.scenes, report.scenes.len());
        assert_eq!(snapshot.frames, report.frames);
        assert!((snapshot.mean_crf.unwrap() - report.summary.mean_crf).abs() < 0.01, "{snapshot:?} {}", report.summary.mean_crf);
        assert_eq!(snapshot.recent.last().map(|scene| scene.start_frame), report.scenes.last().map(|scene| scene.start_frame));
        let written = AtomicU64::new(0);
        encode_scene_crfs(
            &report,
            &SceneEncodeRequest {
                input: &input,
                output: &output,
                preset: Some("ultrafast".to_string()),
                tune: None,
                profile: None,
                level: None,
                x264_params: Some("aq-mode=3".to_string()),
                cancel_file: None,
                progress: Some(&written),
            },
        )
        .unwrap();
        let streams = Command::new(resolve_runtime_binary("ffprobe"))
            .args(["-v", "error", "-show_entries", "stream=codec_name,width,height", "-of", "csv=p=0"])
            .arg(&output)
            .output()
            .unwrap();
        let streams = String::from_utf8_lossy(&streams.stdout).to_string();
        assert!(streams.contains("h264,320,240") && streams.contains("aac"), "{streams}");
        assert!(written.load(Ordering::Relaxed) >= 40, "{}", written.load(Ordering::Relaxed));
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn stats_lines_are_read_by_key() {
        let ssim = "n:12 Y:0.991200 U:0.995000 V:0.994000 All:0.992551 (21.279)";
        assert_eq!(stat_value(ssim, "All"), Some(0.992551));
        let psnr = "n:3 mse_avg:0.00 mse_y:0.00 psnr_avg:inf psnr_y:inf";
        assert_eq!(stat_value(psnr, "psnr_avg"), Some(100.0));
        assert_eq!(stat_value("n:4 mse_avg:2.01 psnr_avg:45.10", "psnr_avg"), Some(45.1));
        assert_eq!(stat_value(ssim, "psnr_avg"), None);
    }

    // Frames are paired by their index in the clip, never by the timestamps the containers gave
    // them: both sides of both comparisons are renumbered, and nothing is rebased from its PTS.
    #[test]
    fn the_measurement_reads_no_more_of_the_source_than_the_clip() {
        let limit: f64 = read_limit_seconds(48, 24000.0 / 1001.0).parse().unwrap();
        assert!(limit > 48.0 * 1001.0 / 24000.0 && limit < 51.0 * 1001.0 / 24000.0);
    }

    #[test]
    fn verification_pairs_frames_by_index() {
        let graph = measurement_graph(48, false, "a.ssim", "a.psnr", None);
        assert_eq!(graph.matches("settb=1/1000,setpts=N*1000").count(), 2);
        assert!(!graph.contains("STARTPTS"));
        assert!(graph.contains("trim=end_frame=48") && graph.contains("stats_file=a.ssim") && graph.contains("stats_file=a.psnr"));
        assert!(graph.contains("split=2[e1][e2];") && !graph.contains("xpsnr"));
        let graph = measurement_graph(48, false, "a.ssim", "a.psnr", Some("a.xpsnr"));
        assert!(graph.contains("split=3[e1][e2][e3];") && graph.contains("split=3[r1][r2][r3];"));
        assert!(graph.ends_with(";[e3][r3]xpsnr=stats_file=a.xpsnr:shortest=1"));
    }

    // A 10-bit source is never brought down to the encode's depth for the comparison: that would
    // dither it, and the dither would be scored as detail the encode lost.
    #[test]
    fn a_deep_source_is_compared_at_its_own_depth() {
        let graph = measurement_graph(48, true, "a.ssim", "a.psnr", None);
        assert!(graph.starts_with("[1:v]format=yuv420p10le,settb"));
        assert!(graph.contains("trim=end_frame=48,format=yuv420p10le,") && !graph.contains("format=yuv420p,"));
    }

    // Lines of a real stats file. The planes pool in the error domain, so luma's 46.24 dB with
    // chroma seven dB better comes out above luma and far below the plain mean of the three.
    #[test]
    fn xpsnr_is_read_per_frame_and_pooled_over_the_planes() {
        let frame = xpsnr_frame("n:    2  XPSNR y: 46.2406  XPSNR u: 53.6679  XPSNR v: 52.2130").unwrap();
        assert!((frame - 47.55).abs() < 0.01, "{frame}");
        assert!(xpsnr_frame("XPSNR average, 48 frames  y: 47.5224  u: 54.0708  v: 51.8638  (minimum: 47.5224)").is_none());
        assert!((xpsnr_frame("n:    1  XPSNR y: inf  XPSNR u: inf  XPSNR v: inf").unwrap() - 100.0).abs() < 1e-9);
    }

    // The clip's first frame is its keyframe: SSIM 0.999 against 0.99 for the rest. Counting it
    // would call the clip a fifth of a dB better than the encode it stands for.
    #[test]
    fn the_fitted_metrics_leave_out_the_opening_keyframe() {
        let mut ssim = vec![0.99; 21];
        ssim[0] = 0.999;
        let mut xpsnr = vec![40.0; 21];
        xpsnr[0] = 50.0;
        xpsnr[7] = 36.0;
        xpsnr[8] = 38.0;
        let clip = ClipMeasurement { ssim, psnr: vec![40.0; 21], xpsnr, bytes: 1000 };
        let measure = clip.measure(21.0, 24.0);
        assert!((measure.ssim_db - 20.0).abs() < 1e-6);
        assert!(measure.mean_ssim > 0.99);
        // The worst tenth of twenty frames is two of them.
        assert_eq!(measure.xpsnr_low, Some(37.0));
        let pooled = measure.xpsnr.unwrap();
        assert!(pooled < 40.0 && pooled > 39.5, "{pooled}");
        let without = ClipMeasurement { xpsnr: Vec::new(), ..clip }.measure(21.0, 24.0);
        assert_eq!((without.xpsnr, without.xpsnr_low), (None, None));
    }

    #[test]
    fn a_spread_sample_steps_evenly_through_the_scenes_long_enough() {
        let mut scenes: Vec<SceneEntry> = (0..100).map(|index| scene(index * 48, 48, if index < 50 { 25.0 } else { 21.0 })).collect();
        scenes[10].frames = 8;
        let chosen = verify_scenes(&scenes, 21.0, 4, VerifySample::Spread);
        assert_eq!(chosen, vec![13, 38, 62, 87]);
        // The same request for moved scenes would take the raised half alone.
        assert!(verify_scenes(&scenes, 21.0, 4, VerifySample::Moved).iter().all(|&index| index < 50));
        assert_eq!(verify_scenes(&scenes[..3], 21.0, 4, VerifySample::Spread), vec![0, 1, 2]);
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
            votes: Vec::new(),
            contributions: Vec::new(),
            probe: None,
            features: WindowFeatures::default(),
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
        assert_eq!(verify_scenes(&scenes, 17.0, 2, VerifySample::Moved), vec![3, 4]);
        assert_eq!(verify_scenes(&scenes, 17.0, 10, VerifySample::Moved), vec![0, 2, 3, 4]);
    }

    // The first real verify (anchor 21, scenes 18.5–22.5) measured six scenes, all below the
    // anchor, because the model lowers further than it raises. A raised scene has to get a slot.
    #[test]
    fn verification_measures_both_sides_of_the_anchor() {
        let scenes = [
            scene(0, 100, 18.5),
            scene(100, 100, 18.5),
            scene(200, 100, 19.0),
            scene(300, 100, 22.0),
            scene(400, 100, 19.0),
            scene(500, 100, 21.5),
        ];
        assert_eq!(verify_scenes(&scenes, 21.0, 2, VerifySample::Moved), vec![0, 3]);
        assert_eq!(verify_scenes(&scenes, 21.0, 4, VerifySample::Moved), vec![0, 1, 3, 5]);
        // One side running out leaves the rest of the slots to the other.
        assert_eq!(verify_scenes(&scenes, 21.0, 5, VerifySample::Moved), vec![0, 1, 2, 3, 5]);
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
