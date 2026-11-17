// A CRF per scene in front of an encoder: Caramel Analysis decides each scene's CRF from its first
// frames, and the encoder is switched to it on the scene's first frame.
//
// `SceneSchedule` sits between the decoder and `Encoder`. Every decoded frame is pushed in; a frame
// comes back out once its scene's CRF is known, together with the CRF to switch to when it is the
// first frame of a scene whose CRF differs from the last. Only a scene's first `analysis_frames`
// frames are ever held back, and after them the two the decider is still weighing as a cut (owned
// copies, ~3 MB each at 1080p), so the delay and the memory are bounded by that one number whatever
// the scene's length. A scene raised far enough to be test-encoded (`caramel_kernel::probe`) is held
// until its first `PROBE_FRAMES` have arrived, or the next scene has begun: 24 frames at most.
//
// With a model that reads a scene's fast encode (`caramel_kernel::fast`), every scene is held until
// it has ended — the next one has begun, or `FAST_FRAMES` have arrived — and is encoded fast and
// decided again before any of it is released: up to 48 frames held, about 150 MB at 1080p.

use crate::{Config, Encoder, y4m};
use caramel_kernel::curve::{self, Model};
use caramel_kernel::xpsnr::{self, Xpsnr};
use caramel_kernel::{EncodeContext, FAST_CRF, FAST_FRAMES, FAST_PRESET, FastEncode, Frame, PROBE_FRAMES, Plane, SceneConfig, SceneDecider, SceneDecision, SceneJudge, SourceContext};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Where every model a machine has installed is kept, one file per version, relative to the
// process. An encode names the model it decides with, and chunks started before a retrained model
// was installed are finished with the one they were started with.
pub const MODEL_DIRECTORY: &str = "DB/caramel/models";

// A version is a file name and a `key=value` value: letters, digits, dots and dashes.
pub fn valid_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= 96 && !version.starts_with('.') && version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub fn model_file(version: &str) -> PathBuf {
    Path::new(MODEL_DIRECTORY).join(format!("{version}.model"))
}

// The model of that name: the one this process has active, the one built in, or the installed
// file. None when there is no such model here.
pub fn load_model(version: &str) -> Option<Arc<Model>> {
    if !valid_version(version) {
        return None;
    }
    curve::active()
        .filter(|model| model.version == version)
        .or_else(|| curve::builtin().filter(|model| model.version == version))
        .or_else(|| {
            let model = Model::from_text(&std::fs::read_to_string(model_file(version)).ok()?).ok()?;
            (model.version == version).then(|| Arc::new(model))
        })
}

#[derive(Clone, Debug)]
pub struct SceneCrf {
    // The anchor is the encoder's own `crf`; everything else the model needs about the encode.
    pub output_height: Option<u32>,
    pub output_bit_depth: u8,
    pub scenes: SceneConfig,
    // What the container says about the source beyond its frames — bitrate, codec, bit depth.
    // Size and frame rate come from the frames themselves.
    pub source_bitrate: Option<f64>,
    pub source_codec: Option<String>,
    pub source_bit_depth: u8,
    // The version of the network scenes are decided with; None for the fitted predictors.
    pub model: Option<String>,
    // Whether a far raise is test-encoded before it is used (`caramel_kernel::probe`). Settings
    // written before probes existed decide without them, as the chunks they decided did.
    pub probe: bool,
}

impl Default for SceneCrf {
    fn default() -> Self {
        Self {
            output_height: None,
            output_bit_depth: 8,
            scenes: SceneConfig::default(),
            source_bitrate: None,
            source_codec: None,
            source_bit_depth: 8,
            model: curve::active().map(|model| model.version.clone()),
            probe: true,
        }
    }
}

// The one file in an AOT directory that says how its chunks decided their scenes. The planner
// writes it before launching any chunk, and the encode that takes the chunks over reads it, so a
// chunk it encodes itself decides exactly as the reused ones did — the final encode can see the
// whole file and might otherwise read a bitrate the download could not.
pub const CONTEXT_FILE: &str = "caramel.context";

impl SceneCrf {
    // As one `key=value:...` string, for a command line or the context file.
    pub fn to_arg(&self) -> String {
        let mut pairs = vec![
            format!("depth={}", self.output_bit_depth),
            format!("analysis={}", self.scenes.analysis_frames),
            format!("min={}", self.scenes.min_scene_frames),
            format!("max={}", self.scenes.max_scene_frames),
            format!("source-depth={}", self.source_bit_depth),
        ];
        if let Some(height) = self.output_height {
            pairs.push(format!("height={height}"));
        }
        if let Some(bitrate) = self.source_bitrate {
            pairs.push(format!("bitrate={bitrate}"));
        }
        if let Some(codec) = self.source_codec.as_deref().filter(|codec| codec.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            pairs.push(format!("codec={codec}"));
        }
        if let Some(model) = self.model.as_deref().filter(|model| valid_version(model)) {
            pairs.push(format!("model={model}"));
        }
        if self.probe {
            pairs.push("probe=1".to_string());
        }
        pairs.join(":")
    }

    // The inverse of `to_arg`. Unknown keys are refused rather than skipped: a chunk encoder that
    // half-understood its instructions would decide scenes unlike its siblings.
    pub fn from_arg(text: &str) -> Result<Self, String> {
        // Settings that name no model were written for the fitted predictors.
        let mut value = Self { model: None, probe: false, ..Self::default() };
        for pair in text.trim().split(':').filter(|pair| !pair.is_empty()) {
            let (key, raw) = pair.split_once('=').ok_or_else(|| format!("`{pair}` is not key=value"))?;
            let number = || raw.parse::<u64>().map_err(|_| format!("`{pair}` is not a number"));
            match key {
                "depth" => value.output_bit_depth = number()? as u8,
                "analysis" => value.scenes.analysis_frames = number()? as usize,
                "min" => value.scenes.min_scene_frames = number()? as usize,
                "max" => value.scenes.max_scene_frames = number()? as usize,
                "source-depth" => value.source_bit_depth = number()? as u8,
                "height" => value.output_height = Some(number()? as u32),
                "bitrate" => value.source_bitrate = Some(raw.parse().map_err(|_| format!("`{pair}` is not a number"))?),
                "codec" => value.source_codec = Some(raw.to_string()),
                "model" if valid_version(raw) => value.model = Some(raw.to_string()),
                "probe" => value.probe = number()? != 0,
                _ => return Err(format!("unknown caramel setting `{key}`")),
            }
        }
        value.scenes = value.scenes.sanitised();
        Ok(value)
    }

    fn contexts(&self, width: usize, height: usize, fps: f64, anchor: f32) -> (SourceContext, EncodeContext) {
        (
            SourceContext {
                width: width as u32,
                height: height as u32,
                fps,
                video_bitrate: self.source_bitrate,
                codec: self.source_codec.clone(),
                bit_depth: self.source_bit_depth,
            },
            EncodeContext {
                output_height: self.output_height,
                output_bit_depth: self.output_bit_depth,
                ..EncodeContext::anchored(anchor)
            },
        )
    }
}

pub struct OwnedFrame {
    pub index: u64,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub stride_y: usize,
    pub stride_c: usize,
}

// A scene as it was encoded: its first frame (relative to what was pushed) and its CRF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneMark {
    pub start: u64,
    pub crf: f32,
}

// A scene that has been decided and not yet released, with the first frame after it once the
// decider has settled past one.
struct Queued {
    decision: SceneDecision,
    boundary: Option<u64>,
    // Its fast encode has been tried, whatever came of it.
    fast_done: bool,
}

pub struct SceneSchedule {
    decider: Option<SceneDecider>,
    pending: VecDeque<OwnedFrame>,
    // Decided scenes waiting to be released in order: the front one waits while its probe has not
    // run, and holds back its frames and every frame after them.
    queued: VecDeque<Queued>,
    upcoming: VecDeque<SceneMark>,
    // The encoder settings a far raise is test-encoded with; None encodes every scene as decided.
    probe: Option<Config>,
    // The settings every scene's fast encode is made with, and what decides it again with that,
    // when the model reads one.
    fast: Option<(Config, SceneJudge)>,
    current: f32,
    width: usize,
    height: usize,
    pushed: u64,
    marks: Vec<SceneMark>,
    decisions: Vec<SceneDecision>,
}

impl SceneSchedule {
    // `anchor` is the CRF the encoder was opened at. `encoder` is that encoder's settings, which a
    // far raise is test-encoded with when `caramel.probe` asks for it (`caramel_kernel::probe`).
    pub fn new(caramel: &SceneCrf, width: usize, height: usize, fps_num: u32, fps_den: u32, anchor: f32, encoder: Option<&Config>) -> Self {
        let fps = if fps_den == 0 { 0.0 } else { fps_num as f64 / fps_den as f64 };
        let (source, encode) = caramel.contexts(width, height, fps, anchor);
        let decider = SceneDecider::new(caramel.scenes, source, encode).with_model(caramel.model.as_deref().and_then(load_model));
        let measurable = caramel.output_bit_depth == 8;
        // The lab reads a scene's fast encode at the source's size; a scaled encode decides without.
        let fast = encoder.filter(|_| measurable && caramel.output_height.is_none_or(|output| output as usize == height));
        Self::from_decider(decider, width, height, fps_num, fps_den, anchor, encoder.filter(|_| caramel.probe && measurable), fast)
    }

    // The same around a decider the caller set up, test-encoding far raises with `probe` and making
    // each scene's fast encode with `fast`, when its model reads one — settings for 8-bit frames of
    // exactly this size, or None.
    #[allow(clippy::too_many_arguments)]
    pub fn from_decider(
        decider: SceneDecider,
        width: usize,
        height: usize,
        fps_num: u32,
        fps_den: u32,
        anchor: f32,
        probe: Option<&Config>,
        fast: Option<&Config>,
    ) -> Self {
        let judge = decider.judge();
        let fast = fast
            .filter(|_| judge.reads_fast_encode())
            .map(|config| (fast_config(&Config { fps_num, fps_den, ..config.clone() }, width, height), judge));
        let probe = probe.map(|config| Config {
            width: width as u32,
            height: height as u32,
            fps_num,
            fps_den,
            caramel: None,
            plan_only: false,
            full_recon: true,
            ..config.clone()
        });
        Self {
            decider: Some(decider),
            pending: VecDeque::new(),
            queued: VecDeque::new(),
            upcoming: VecDeque::new(),
            probe,
            fast,
            current: anchor,
            width,
            height,
            pushed: 0,
            marks: Vec::new(),
            decisions: Vec::new(),
        }
    }

    pub fn push(&mut self, frame: &y4m::Frame) -> Result<(), String> {
        let (chroma_width, chroma_height) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let y = Plane::new(frame.y, self.width, self.height, frame.stride_y).ok_or("short luma plane")?;
        let u = Plane::new(frame.u, chroma_width, chroma_height, frame.stride_c);
        let v = Plane::new(frame.v, chroma_width, chroma_height, frame.stride_c);
        let mut progress = None;
        if let Some(decider) = self.decider.as_mut() {
            let decisions = decider.push(&Frame { y, u, v });
            self.queued.extend(decisions.into_iter().map(|decision| Queued { decision, boundary: None, fast_done: false }));
            progress = Some((decider.scene_start(), decider.settled()));
        }
        self.pending.push_back(OwnedFrame {
            index: self.pushed,
            y: frame.y.to_vec(),
            u: frame.u.to_vec(),
            v: frame.v.to_vec(),
            stride_y: frame.stride_y,
            stride_c: frame.stride_c,
        });
        self.pushed += 1;
        if let Some((scene_start, settled)) = progress {
            // The decider settles one frame per push, so the first time a later scene shows here it
            // is the first one after every queued scene that has no boundary yet.
            for queued in self.queued.iter_mut().filter(|queued| queued.boundary.is_none() && queued.decision.start_frame < scene_start) {
                queued.boundary = Some(scene_start);
            }
            self.release(Some(settled));
        }
        Ok(())
    }

    // The stream ended: whatever is still held back is decided now.
    pub fn finish(&mut self) {
        if let Some(last) = self.decider.take().and_then(SceneDecider::finish) {
            self.queued.push_back(Queued { decision: last, boundary: None, fast_done: false });
        }
        self.release(None);
    }

    // Where the front scene's first `count` frames end, or where the scene does when that is sooner;
    // None while that is not known yet. `settled` is how far the decider has settled, None once the
    // stream has ended.
    fn span_end(&self, count: usize, settled: Option<u64>) -> Option<u64> {
        let front = self.queued.front()?;
        let limit = front.decision.start_frame + count as u64;
        let next = self.queued.get(1).map(|queued| queued.decision.start_frame);
        match ([front.boundary, next].into_iter().flatten().min(), settled) {
            (Some(boundary), _) => Some(boundary.min(limit)),
            (None, Some(settled)) if settled >= limit => Some(limit),
            (None, None) => Some(limit.min(self.pushed)),
            (None, Some(_)) => None,
        }
    }

    // Releases queued scenes in order. A scene is first encoded fast and decided again, when the
    // model reads that, once all of it is here; then test-encoded, when it was raised far, once its
    // first `PROBE_FRAMES` are.
    fn release(&mut self, settled: Option<u64>) {
        while let Some(front) = self.queued.front() {
            let start = front.decision.start_frame;
            if self.fast.is_some() && !front.fast_done {
                let Some(end) = self.span_end(FAST_FRAMES, settled) else {
                    break;
                };
                let frames: Vec<&OwnedFrame> = self.pending.iter().filter(|frame| frame.index >= start && frame.index < end).collect();
                let (config, judge) = self.fast.as_ref().expect("checked above");
                // A scene whose fast encode cannot be made is decided as if the model had none.
                let measured = fast_encode(config, &frames, self.width, self.height);
                let front = self.queued.front_mut().expect("checked above");
                front.fast_done = true;
                if let Ok(fast) = measured {
                    judge.redecide(&mut front.decision, fast);
                }
            }
            let front = self.queued.front().expect("checked above");
            let wants_probe = self.probe.is_some() && front.decision.estimate.probe.as_ref().is_some_and(|probe| probe.measured.is_none());
            if wants_probe {
                let Some(end) = self.span_end(PROBE_FRAMES, settled) else {
                    break;
                };
                let frames: Vec<&OwnedFrame> = self.pending.iter().filter(|frame| frame.index >= start && frame.index < end).collect();
                let config = self.probe.as_ref().expect("checked above");
                // A probe that cannot run leaves the scene as the model decided it.
                if let Ok(measured) = probe_xpsnr(config, front.decision.crf, &frames, self.width, self.height) {
                    self.queued.front_mut().expect("checked above").decision.settle_probe(measured);
                }
            }
            let Queued { decision, .. } = self.queued.pop_front().expect("checked above");
            self.upcoming.push_back(SceneMark { start: decision.start_frame, crf: decision.crf });
            self.decisions.push(decision);
        }
    }

    // The next frame whose CRF is known, and the CRF to switch to before encoding it, if any.
    pub fn ready(&mut self) -> Option<(OwnedFrame, Option<f32>)> {
        let decided = match &self.decider {
            Some(decider) => decider.decided_frames(),
            None => self.pushed,
        };
        let waiting = self.queued.front().map_or(u64::MAX, |queued| queued.decision.start_frame);
        if self.pending.front()?.index >= decided.min(waiting) {
            return None;
        }
        let frame = self.pending.pop_front()?;
        let mut switch = None;
        while self.upcoming.front().is_some_and(|mark| mark.start <= frame.index) {
            let mark = self.upcoming.pop_front()?;
            self.marks.push(mark);
            if mark.crf != self.current {
                self.current = mark.crf;
                switch = Some(mark.crf);
            }
        }
        Some((frame, switch))
    }

    // Every scene this schedule released, in order.
    pub fn marks(&self) -> &[SceneMark] {
        &self.marks
    }

    // Every scene decided and released so far, with what its probe found.
    pub fn decisions(&self) -> &[SceneDecision] {
        &self.decisions
    }
}

// The XPSNR of `frames` encoded at `crf` with `config`, measured as the lab measures a clip: the
// frames' errors pooled, the first frame (an IDR here) left out.
pub fn probe_xpsnr(config: &Config, crf: f32, frames: &[&OwnedFrame], width: usize, height: usize) -> Result<f64, String> {
    if frames.len() < 3 {
        return Err("too few frames to measure".to_string());
    }
    let (values, _) = test_encode(&Config { crf, ..config.clone() }, frames, width, height)?;
    xpsnr::pooled(&values[1..]).ok_or_else(|| "nothing measured".to_string())
}

// The settings a scene's fast encode is made with: the encode's own, at `FAST_PRESET` and
// `FAST_CRF`, on one thread so it reads the same wherever it runs.
pub fn fast_config(encoder: &Config, width: usize, height: usize) -> Config {
    Config {
        width: width as u32,
        height: height as u32,
        crf: FAST_CRF,
        threads: 1,
        preset: Some(FAST_PRESET.to_string()),
        caramel: None,
        plan_only: false,
        full_recon: true,
        ..encoder.clone()
    }
}

// A scene's fast encode (`caramel_kernel::fast`): `frames` encoded with `fast_config`'s settings and
// read as the lab reads a clip. `config` is already a `fast_config`.
pub fn fast_encode(config: &Config, frames: &[&OwnedFrame], width: usize, height: usize) -> Result<FastEncode, String> {
    let (values, bytes) = test_encode(config, frames, width, height)?;
    FastEncode::from_frames(&values, bytes, width * height).ok_or_else(|| "the fast encode measured nothing".to_string())
}

// `frames` encoded with `config`, each frame's XPSNR against its source in display order, and the
// coded bytes of all of them.
fn test_encode(config: &Config, frames: &[&OwnedFrame], width: usize, height: usize) -> Result<(Vec<f64>, u64), String> {
    if frames.len() < 2 {
        return Err("too few frames to measure".to_string());
    }
    // Quiet: a chunk encoder's log is for the encode, and every test encode would otherwise close
    // with x264's summary of it.
    let quiet = Some(config.x264_params.as_deref().map_or("log=-1".to_string(), |params| format!("{params}:log=-1")));
    let mut encoder = Encoder::open(&Config { caramel: None, full_recon: true, x264_params: quiet, ..config.clone() })?;
    let mut pictures = Vec::with_capacity(frames.len());
    let mut bytes = 0u64;
    for (pts, frame) in frames.iter().enumerate() {
        bytes += encoder.encode(&frame.y, &frame.u, &frame.v, frame.stride_y, frame.stride_c, frame.stride_c, pts as i64)?.len() as u64;
        pictures.extend(encoder.recon()?);
    }
    while pictures.len() < frames.len() {
        let produced = encoder.flush()?.len() as u64;
        bytes += produced;
        match encoder.recon()? {
            Some(picture) => pictures.push(picture),
            None if produced == 0 => break,
            None => {}
        }
    }
    if pictures.len() != frames.len() {
        return Err(format!("the test encode returned {} of {} frames", pictures.len(), frames.len()));
    }
    pictures.sort_by_key(|picture| picture.pts);
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    let mut meter = Xpsnr::new(width, height, config.fps_num, config.fps_den);
    let mut values = Vec::with_capacity(frames.len());
    for (picture, source) in pictures.iter().zip(frames) {
        let encoded = Frame {
            y: Plane::new(&picture.y, width, height, width).ok_or("short reconstruction")?,
            u: Plane::new(&picture.u, chroma_width, chroma_height, chroma_width),
            v: Plane::new(&picture.v, chroma_width, chroma_height, chroma_width),
        };
        let original = Frame {
            y: Plane::new(&source.y, width, height, source.stride_y).ok_or("short luma plane")?,
            u: Plane::new(&source.u, chroma_width, chroma_height, source.stride_c),
            v: Plane::new(&source.v, chroma_width, chroma_height, source.stride_c),
        };
        values.push(meter.frame(&encoded, &original).ok_or("frames of another size")?);
    }
    Ok((values, bytes))
}

// A chunk's scenes, beside the chunk: `<chunk>.scenes`, one `start|crf` per line. Written by
// whichever process encoded the chunk, so the encode that reuses it can still say which CRFs the
// release carries.
pub fn sidecar_path(chunk: &Path) -> PathBuf {
    let mut name = chunk.as_os_str().to_owned();
    name.push(".scenes");
    PathBuf::from(name)
}

pub fn write_sidecar(chunk: &Path, marks: &[SceneMark]) -> std::io::Result<()> {
    let text: String = marks.iter().map(|mark| format!("{}|{}\n", mark.start, mark.crf)).collect();
    std::fs::write(sidecar_path(chunk), text)
}

pub fn read_sidecar(chunk: &Path) -> Option<Vec<SceneMark>> {
    let text = std::fs::read_to_string(sidecar_path(chunk)).ok()?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (start, crf) = line.split_once('|')?;
            Some(SceneMark { start: start.parse().ok()?, crf: crf.parse().ok()? })
        })
        .collect()
}

// The CRFs a whole encode carried, from every chunk's scenes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneCrfSummary {
    pub scenes: usize,
    pub frames: u64,
    pub min_crf: f32,
    pub max_crf: f32,
    // Weighted by frames, which is what the bitrate follows.
    pub mean_crf: f64,
    // Chunks whose scenes are not known: reused from an encode that kept no sidecar.
    pub unknown_chunks: usize,
}

impl SceneCrfSummary {
    // `chunks` is each chunk's length and its scenes, in order.
    pub fn from_chunks<'a>(chunks: impl IntoIterator<Item = (u64, Option<&'a [SceneMark]>)>) -> Self {
        let mut summary = Self { min_crf: f32::INFINITY, max_crf: f32::NEG_INFINITY, ..Default::default() };
        let mut weighted = 0.0;
        for (length, marks) in chunks {
            let Some(marks) = marks.filter(|marks| !marks.is_empty()) else {
                summary.unknown_chunks += 1;
                continue;
            };
            for (index, mark) in marks.iter().enumerate() {
                let end = marks.get(index + 1).map_or(length, |next| next.start.min(length));
                let frames = end.saturating_sub(mark.start);
                summary.scenes += 1;
                summary.frames += frames;
                weighted += mark.crf as f64 * frames as f64;
                summary.min_crf = summary.min_crf.min(mark.crf);
                summary.max_crf = summary.max_crf.max(mark.crf);
            }
        }
        if summary.scenes == 0 {
            summary.min_crf = 0.0;
            summary.max_crf = 0.0;
        } else if summary.frames > 0 {
            summary.mean_crf = weighted / summary.frames as f64;
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 64;
    const H: usize = 48;

    fn yuv(fill: impl Fn(usize, usize) -> u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..W * H).map(|i| fill(i % W, i / W)).collect();
        (y, vec![128; W * H / 4], vec![128; W * H / 4])
    }

    fn push(schedule: &mut SceneSchedule, planes: &(Vec<u8>, Vec<u8>, Vec<u8>)) {
        schedule
            .push(&y4m::Frame { y: &planes.0, u: &planes.1, v: &planes.2, stride_y: W, stride_c: W / 2 })
            .unwrap();
    }

    // Frames come out in order, each exactly once, only after their scene was decided, and the
    // switch lands on the first frame of the scene that changed the CRF.
    #[test]
    fn frames_leave_in_order_with_their_scene_crf() {
        frames_leave_in_order(None);
        // With far raises test-encoded, frames wait longer and leave the same way.
        let encoder = Config { width: W as u32, height: H as u32, threads: 1, preset: Some("ultrafast".into()), ..Default::default() };
        let decisions = frames_leave_in_order(Some(&encoder));
        for decision in decisions.iter().filter_map(|decision| decision.estimate.probe.as_ref().map(|probe| (decision, probe))) {
            let (decision, probe) = decision;
            assert!(probe.measured.is_some(), "{decision:?}");
            assert!(decision.crf <= probe.crf && decision.crf as f64 >= probe.anchor, "{decision:?}");
        }
    }

    fn frames_leave_in_order(encoder: Option<&Config>) -> Vec<SceneDecision> {
        let caramel = SceneCrf {
            scenes: SceneConfig { analysis_frames: 4, min_scene_frames: 4, max_scene_frames: 250 },
            ..Default::default()
        };
        let mut schedule = SceneSchedule::new(&caramel, W, H, 24, 1, 17.0, encoder);
        let smooth = yuv(|x, y| (60 + x + y) as u8);
        let checker = yuv(|x, y| if (x / 8 + y / 8) % 2 == 0 { 16 } else { 235 });
        let mut out = Vec::new();
        for index in 0..20 {
            push(&mut schedule, if index < 10 { &smooth } else { &checker });
            while let Some(ready) = schedule.ready() {
                out.push((ready.0.index, ready.1));
            }
        }
        schedule.finish();
        while let Some(ready) = schedule.ready() {
            out.push((ready.0.index, ready.1));
        }
        assert_eq!(out.iter().map(|(index, _)| *index).collect::<Vec<_>>(), (0..20).collect::<Vec<_>>());
        let marks = schedule.marks().to_vec();
        assert_eq!(marks.iter().map(|mark| mark.start).collect::<Vec<_>>(), vec![0, 10]);
        // A switch exactly where a scene's CRF differs from the one before it, and nowhere else.
        let mut current = 17.0;
        for (index, switch) in out {
            let expected = marks.iter().find(|mark| mark.start == index).and_then(|mark| {
                let changed = mark.crf != current;
                current = mark.crf;
                changed.then_some(mark.crf)
            });
            assert_eq!(switch, expected, "frame {index}");
        }
        assert_eq!(schedule.decisions().iter().map(|decision| (decision.start_frame, decision.crf)).collect::<Vec<_>>(), marks.iter().map(|mark| (mark.start, mark.crf)).collect::<Vec<_>>());
        schedule.decisions().to_vec()
    }

    // The built-in model, made to read the fast encode too and to give it no weight: it decides as
    // the built-in model does, and a schedule has to encode every scene fast before it may.
    fn reading_fast_encode() -> Arc<Model> {
        let mut model = (*curve::builtin().expect("a built-in model")).clone();
        let all = caramel_kernel::inputs::INPUTS;
        for member in &mut model.members {
            let first = &mut member.layers[0];
            let widened: Vec<f64> = first.weights.chunks(first.inputs).flat_map(|row| row.iter().copied().chain(std::iter::repeat_n(0.0, all - row.len()))).collect();
            first.weights = widened;
            first.inputs = all;
        }
        model.input_mean.resize(all, 0.0);
        model.input_scale.resize(all, 0.0);
        assert!(model.reads_fast_encode());
        Arc::new(model)
    }

    #[test]
    fn every_scene_is_encoded_fast_before_any_of_it_leaves() {
        let config = SceneConfig { analysis_frames: 4, min_scene_frames: 4, max_scene_frames: 250 };
        let source = SourceContext { width: W as u32, height: H as u32, fps: 24.0, video_bitrate: None, codec: None, bit_depth: 8 };
        let encoder = Config { width: W as u32, height: H as u32, threads: 1, ..Default::default() };
        let decider = SceneDecider::new(config, source, EncodeContext::anchored(17.0)).with_model(Some(reading_fast_encode()));
        let mut schedule = SceneSchedule::from_decider(decider, W, H, 24, 1, 17.0, None, Some(&encoder));
        let smooth = yuv(|x, y| (60 + x + y) as u8);
        let checker = yuv(|x, y| if (x / 8 + y / 8) % 2 == 0 { 16 } else { 235 });
        let mut out = Vec::new();
        for index in 0..20 {
            push(&mut schedule, if index < 10 { &smooth } else { &checker });
            while let Some(ready) = schedule.ready() {
                // Nothing of a scene leaves before the scene has ended and was encoded fast.
                assert!(schedule.decisions().iter().any(|decision| decision.start_frame <= ready.0.index && decision.features.fast_encode.is_some()));
                out.push(ready.0.index);
            }
            // The first scene is held whole until the second begins.
            if index < 10 {
                assert!(out.is_empty(), "frame {index}");
            }
        }
        schedule.finish();
        while let Some(ready) = schedule.ready() {
            out.push(ready.0.index);
        }
        assert_eq!(out, (0..20).collect::<Vec<_>>());
        let decisions = schedule.decisions();
        assert_eq!(decisions.iter().map(|decision| decision.start_frame).collect::<Vec<_>>(), vec![0, 10]);
        for decision in decisions {
            let fast = decision.features.fast_encode.expect("every scene was encoded fast");
            assert_eq!(fast.frames, 10);
            assert!(fast.xpsnr > 20.0 && fast.log_bpp.is_finite(), "{fast:?}");
        }
        // The checkerboard costs more bits than the gradient at the same CRF.
        assert!(decisions[1].features.fast_encode.unwrap().log_bpp > decisions[0].features.fast_encode.unwrap().log_bpp);
    }

    // A test encode measures what the encode did: lower at a higher CRF, and on the scale ffmpeg's
    // `xpsnr` reads (`caramel_kernel::xpsnr`).
    #[test]
    fn a_test_encode_reads_lower_at_a_higher_crf() {
        let (w, h) = (128, 96);
        let frames: Vec<OwnedFrame> = (0..8u64)
            .map(|index| {
                let y = (0..w * h).map(|i| (((i % w) * 3 + (i / w) * 5 + index as usize * 2) % 251) as u8 ^ ((i * 7919) % 13) as u8).collect();
                OwnedFrame { index, y, u: vec![120; w * h / 4], v: vec![136; w * h / 4], stride_y: w, stride_c: w / 2 }
            })
            .collect();
        let refs: Vec<&OwnedFrame> = frames.iter().collect();
        let config = Config { width: w as u32, height: h as u32, threads: 1, preset: Some("ultrafast".into()), ..Default::default() };
        let fine = probe_xpsnr(&config, 16.0, &refs, w, h).unwrap();
        let coarse = probe_xpsnr(&config, 36.0, &refs, w, h).unwrap();
        assert!(fine > coarse + 3.0 && coarse > 20.0 && fine < 100.0, "{fine} {coarse}");
        assert!(probe_xpsnr(&config, 16.0, &refs[..2], w, h).is_err());
    }

    #[test]
    fn settings_round_trip_through_their_text() {
        let settings = SceneCrf {
            output_height: Some(720),
            output_bit_depth: 8,
            scenes: SceneConfig { analysis_frames: 8, min_scene_frames: 10, max_scene_frames: 200 },
            source_bitrate: Some(4_500_000.5),
            source_codec: Some("hevc".into()),
            source_bit_depth: 10,
            model: Some("caramel-neural-1.900-a1b2c3".into()),
            probe: true,
        };
        let parsed = SceneCrf::from_arg(&settings.to_arg()).unwrap();
        assert_eq!(parsed.to_arg(), settings.to_arg());
        assert_eq!((parsed.source_bitrate, parsed.scenes), (settings.source_bitrate, settings.scenes));
        assert_eq!(SceneCrf::from_arg(&SceneCrf::default().to_arg()).unwrap().to_arg(), SceneCrf::default().to_arg());
        assert!(SceneCrf::from_arg("depth=8:mystery=1").is_err());
        // The model is named, and a name that could not be a file's is refused. Settings that name
        // none are the fitted predictors', whatever this process has active.
        assert_eq!(parsed.model.as_deref(), Some("caramel-neural-1.900-a1b2c3"));
        assert!(settings.to_arg().ends_with(":model=caramel-neural-1.900-a1b2c3:probe=1"));
        // Settings from before probes decide without them, as the chunks they decided did.
        assert!(parsed.probe && !SceneCrf::from_arg("depth=8").unwrap().probe);
        assert!(SceneCrf::from_arg("depth=8:model=../../etc/passwd").is_err());
        assert_eq!(SceneCrf::from_arg("depth=8").unwrap().model, None);
        assert!(load_model("no-such-model").is_none() && load_model("../x").is_none());
    }

    #[test]
    fn sidecars_round_trip_and_summaries_weigh_by_frames() {
        let directory = std::env::temp_dir().join(format!("pnx264-scenes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let chunk = directory.join("chunk-000000000000-000000000100.264");
        let marks = vec![SceneMark { start: 0, crf: 16.0 }, SceneMark { start: 25, crf: 19.5 }];
        write_sidecar(&chunk, &marks).unwrap();
        assert_eq!(read_sidecar(&chunk), Some(marks.clone()));
        std::fs::remove_dir_all(&directory).ok();

        let summary = SceneCrfSummary::from_chunks([(100, Some(marks.as_slice())), (50, None)]);
        assert_eq!((summary.scenes, summary.frames, summary.unknown_chunks), (2, 100, 1));
        assert_eq!((summary.min_crf, summary.max_crf), (16.0, 19.5));
        assert!((summary.mean_crf - (16.0 * 25.0 + 19.5 * 75.0) / 100.0).abs() < 1e-9);
    }
}
