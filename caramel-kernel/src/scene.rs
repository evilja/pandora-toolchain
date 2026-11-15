// Scenes, and a CRF for each, decided while the frames stream past.
//
// `SceneDecider` is fed every frame in display order, exactly as an encoder is. It compares each
// frame with the one before for a cut, and measures the first `analysis_frames` of every scene. As
// soon as a scene has been measured (or has ended before that), its CRF is decided and handed back.
// An encoder in front of it therefore only has to hold back the few frames of a scene that arrive
// before its decision: everything after streams straight through at the decided CRF.
//
// Whether a frame starts a scene is settled `CUT_PAIRS_AFTER` frames after it arrives, because a
// cut is told from fast motion by what follows it (see `temporal::is_cut`). Those few frames are the
// only ones held back in the middle of a scene.
//
// Comparing frames is cheap and done for all of them. Measuring a frame reads every pixel of it and
// is done for `MEASURED_FRAMES` of a scene's opening, spread evenly: against brute-force encodes,
// four frames of twelve predicted a scene's CRF as well as all twelve did, and one did not.
//
// Deciding from a scene's opening frames is what makes it streamable, and it holds for about two
// seconds: a shot keeps its grain and its drawing style, but not its motion — a character who
// stands for a second and then runs is one shot. Measured on brute-force encodes, the second two
// seconds of a shot wanted a CRF more than one away from the first's in half the shots. A scene
// that runs past `max_scene_frames` is therefore decided again from that point; where nothing has
// changed the decision comes out the same and costs an encoder nothing.

use crate::frame::Frame;
use crate::curve;
use crate::model::{EncodeContext, SceneEstimate, SourceContext, estimate_scene_with};
use crate::temporal::{self, CUT_PAIRS_AFTER, CUT_PAIRS_BEFORE, LowRes, TemporalFeatures};
use crate::window::{self, FrameMeasure, WindowAnalyzer, WindowFeatures};
use serde::Serialize;
use std::collections::VecDeque;

// How many of a scene's opening frames are measured by themselves.
pub const MEASURED_FRAMES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SceneConfig {
    // Frames at the start of every scene its CRF is decided from. Also how many frames an encoder
    // holds back.
    pub analysis_frames: usize,
    // A cut sooner than this after the last one is ignored, so a flash or a run of false cuts in
    // fast action does not make every few frames a scene of its own.
    pub min_scene_frames: usize,
    // A scene this long is split and decided again.
    pub max_scene_frames: usize,
}

impl Default for SceneConfig {
    fn default() -> Self {
        // Half a second of measurement at 24 fps, deciding two seconds: the scene length the model
        // in `model` was fitted on. A longer measurement was no better, a longer scene was worse.
        Self { analysis_frames: 12, min_scene_frames: 12, max_scene_frames: 48 }
    }
}

impl SceneConfig {
    // Anything a caller hands in is made usable: at least one frame measured, and scenes at least as
    // long as their measurement.
    pub fn sanitised(self) -> Self {
        let analysis_frames = self.analysis_frames.clamp(1, 250);
        let min_scene_frames = self.min_scene_frames.max(1);
        Self {
            analysis_frames,
            min_scene_frames,
            max_scene_frames: self.max_scene_frames.max(min_scene_frames).max(analysis_frames),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SceneStart {
    // The first frame the decider saw.
    Start,
    // A detected cut.
    Cut,
    // The previous scene ran past `max_scene_frames`.
    Split,
}

#[derive(Clone, Debug, Serialize)]
pub struct SceneDecision {
    // Index of the scene's first frame, counted from the first frame pushed.
    pub start_frame: u64,
    pub crf: f32,
    pub start: SceneStart,
    // The frame pair that was read as the cut, so a report shows what the detector saw.
    pub cut: Option<TemporalFeatures>,
    // Cuts inside the measured frames that came too soon after this one to start a scene.
    pub cut_pairs_within: usize,
    pub estimate: SceneEstimate,
    // What was measured, as the model was given it.
    pub features: WindowFeatures,
}

// A frame that has arrived and whose scene is not settled yet.
struct Arrived {
    pair: Option<TemporalFeatures>,
    // The pair passed `temporal::may_be_cut` against these residuals of the pairs before it.
    may_be_cut: bool,
    before: Vec<f64>,
    measure: Option<FrameMeasure>,
}

pub struct SceneDecider {
    config: SceneConfig,
    source: SourceContext,
    encode: EncodeContext,
    // The network scenes are decided with; None for the fitted predictors.
    model: Option<std::sync::Arc<curve::Model>>,
    pushed: u64,
    // Frames whose scene is settled: every frame before this index.
    settled: u64,
    scene_start: u64,
    scene_kind: SceneStart,
    scene_cut: Option<TemporalFeatures>,
    // Present while the current scene is still being measured.
    analyzer: Option<WindowAnalyzer>,
    decided: bool,
    previous: Option<LowRes>,
    // The frames from `settled` to `pushed`, oldest first.
    arrived: VecDeque<Arrived>,
    // Residuals of the last `CUT_PAIRS_BEFORE` pairs that arrived, oldest first.
    residuals: VecDeque<f64>,
}

impl SceneDecider {
    pub fn new(config: SceneConfig, source: SourceContext, encode: EncodeContext) -> Self {
        Self {
            config: config.sanitised(),
            source,
            encode,
            model: curve::active(),
            pushed: 0,
            settled: 0,
            scene_start: 0,
            scene_kind: SceneStart::Start,
            scene_cut: None,
            analyzer: None,
            decided: false,
            previous: None,
            arrived: VecDeque::new(),
            residuals: VecDeque::new(),
        }
    }

    // Decides with `model` instead of the one the process has active: for encoders that must decide
    // as others already did, whatever has been installed since. None is the fitted predictors.
    pub fn with_model(mut self, model: Option<std::sync::Arc<curve::Model>>) -> Self {
        self.model = model;
        self
    }

    // Frames pushed so far whose CRF is known: everything before this index may be encoded.
    pub fn decided_frames(&self) -> u64 {
        if self.decided { self.settled } else { self.scene_start }
    }

    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    // The next frame, in display order. Returns the decisions it made final — usually none, at most
    // two (the scene that ended, and the new one when its measurement is already complete).
    pub fn push(&mut self, frame: &Frame) -> Vec<SceneDecision> {
        let lowres = temporal::downsample(&frame.y);
        let pair = self.previous.as_ref().map(|previous| temporal::compare(previous, &lowres));
        let before: Vec<f64> = self.residuals.iter().copied().collect();
        let may_be_cut = pair.as_ref().is_some_and(|pair| temporal::may_be_cut(pair, &before));
        // The frame is measured when its place in its scene's opening is one of the measured ones.
        // Its scene is the one it arrives in, the one a split is about to start, or the one a cut
        // still being weighed would start.
        let position = self.pushed - self.scene_start;
        let split = self.config.max_scene_frames as u64;
        let position = if position >= split { position - split } else { position };
        let opens_scene = may_be_cut
            || self.is_measured(position)
            || self.arrived.iter().enumerate().any(|(ahead, arrived)| {
                arrived.may_be_cut && self.is_measured(self.pushed - self.settled - ahead as u64)
            });
        if let Some(pair) = &pair {
            if self.residuals.len() == CUT_PAIRS_BEFORE {
                self.residuals.pop_front();
            }
            self.residuals.push_back(pair.mc_residual);
        }
        self.arrived.push_back(Arrived { pair, may_be_cut, before, measure: opens_scene.then(|| window::measure(frame)) });
        self.previous = Some(lowres);
        self.pushed += 1;
        let mut decisions = Vec::new();
        while self.arrived.len() > CUT_PAIRS_AFTER {
            self.settle(true, &mut decisions);
        }
        decisions
    }

    // Whether the frame `position` frames into a scene is one of its measured ones.
    fn is_measured(&self, position: u64) -> bool {
        let stride = (self.config.analysis_frames / MEASURED_FRAMES).max(1) as u64;
        position < self.config.analysis_frames as u64 && position % stride == 0
    }

    // The end of the stream: decides a scene that ended before it was fully measured. The last
    // frames join the scene before them — a scene of two frames would be decided from nothing.
    pub fn finish(mut self) -> Option<SceneDecision> {
        let mut decisions = Vec::new();
        while !self.arrived.is_empty() {
            self.settle(false, &mut decisions);
        }
        decisions.extend(self.close());
        decisions.pop()
    }

    // Settles the oldest arrived frame: it starts a scene or joins the current one.
    fn settle(&mut self, cuts: bool, decisions: &mut Vec<SceneDecision>) {
        let Some(frame) = self.arrived.pop_front() else {
            return;
        };
        let index = self.settled;
        let length = index - self.scene_start;
        let cut = cuts
            && frame.may_be_cut
            && frame.pair.as_ref().is_some_and(|pair| {
                let after: Vec<f64> =
                    self.arrived.iter().filter_map(|next| next.pair.as_ref().map(|pair| pair.mc_residual)).collect();
                temporal::is_cut(pair, &frame.before, &after)
            });
        let boundary = if index == 0 {
            Some(SceneStart::Start)
        } else if cut && length >= self.config.min_scene_frames as u64 {
            Some(SceneStart::Cut)
        } else if cuts && length >= self.config.max_scene_frames as u64 {
            Some(SceneStart::Split)
        } else {
            None
        };
        if let Some(kind) = boundary {
            decisions.extend(self.close());
            self.scene_start = index;
            self.scene_kind = kind;
            self.scene_cut = if kind == SceneStart::Cut { frame.pair } else { None };
            self.analyzer = Some(WindowAnalyzer::new(index as f64 / self.source.fps.max(1.0)));
            self.decided = false;
        }
        self.settled += 1;
        // A frame measured for a scene that did not come to be is not one of this scene's measured
        // frames. The pair into a scene's first frame belongs to the scene before it.
        let measure = frame.measure.filter(|_| self.is_measured(index - self.scene_start));
        if let Some(analyzer) = self.analyzer.as_mut() {
            analyzer.push(measure, if boundary.is_some() { None } else { frame.pair.as_ref() }, cut);
            if self.settled - self.scene_start >= self.config.analysis_frames as u64 {
                decisions.push(self.decide());
            }
        }
    }

    fn close(&mut self) -> Option<SceneDecision> {
        (self.settled > 0 && !self.decided).then(|| self.decide())
    }

    fn decide(&mut self) -> SceneDecision {
        let features = self.analyzer.take().map(WindowAnalyzer::finish).unwrap_or_default();
        let estimate = estimate_scene_with(self.model.as_deref(), &features, &self.source, &self.encode);
        self.decided = true;
        SceneDecision {
            start_frame: self.scene_start,
            crf: estimate.crf,
            start: self.scene_kind,
            cut: self.scene_cut,
            cut_pairs_within: features.cuts,
            estimate,
            features,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Plane;
    use crate::spatial::tests::{Noise, picture};

    const W: usize = 320;
    const H: usize = 192;

    fn source() -> SourceContext {
        SourceContext { width: W as u32, height: H as u32, fps: 24.0, video_bitrate: None, codec: None, bit_depth: 8 }
    }

    fn decider(config: SceneConfig) -> SceneDecider {
        SceneDecider::new(config, source(), EncodeContext::anchored(17.0)).with_model(None)
    }

    fn clean(shift: f64) -> Vec<u8> {
        picture(W, H, |x, y| {
            let x = x as f64 + shift;
            128.0 + 50.0 * ((x * 0.21).sin() + (y as f64 * 0.17).cos() * (x * 0.05).sin())
        })
    }

    fn grainy(noise: &mut Noise) -> Vec<u8> {
        picture(W, H, |x, y| 90.0 + 30.0 * ((x as f64 * 0.03).sin() * (y as f64 * 0.02).cos()))
            .into_iter()
            .map(|value| (value as f64 + noise.gaussian() * 5.0).clamp(0.0, 255.0) as u8)
            .collect()
    }

    fn checker() -> Vec<u8> {
        picture(W, H, |x, y| if (x / 40 + y / 40) % 2 == 0 { 20.0 } else { 235.0 })
    }

    fn run(decider: &mut SceneDecider, frames: &[Vec<u8>]) -> Vec<SceneDecision> {
        let mut decisions = Vec::new();
        let mut decided = 0;
        for data in frames {
            decisions.extend(decider.push(&Frame::luma(Plane::new(data, W, H, W).unwrap())));
            // What may be encoded only ever grows, and never past what has arrived.
            assert!(decider.decided_frames() >= decided && decider.decided_frames() <= decider.pushed());
            decided = decider.decided_frames();
        }
        decisions
    }

    #[test]
    fn a_cut_starts_a_scene_with_its_own_crf() {
        let mut noise = Noise::new(3);
        let mut frames: Vec<Vec<u8>> = (0..30).map(|i| clean(i as f64)).collect();
        frames.extend((0..30).map(|_| grainy(&mut noise)));
        let mut scenes = decider(SceneConfig::default());
        let mut decisions = run(&mut scenes, &frames);
        decisions.extend(scenes.finish());
        let starts: Vec<_> = decisions.iter().map(|d| (d.start_frame, d.start)).collect();
        assert_eq!(starts, vec![(0, SceneStart::Start), (30, SceneStart::Cut)]);
        assert!(decisions[1].crf < decisions[0].crf, "{decisions:?}");
        // Every frame of a scene's opening is compared with the one before; four are measured.
        assert!(decisions.iter().all(|d| d.features.frames == 12 && d.features.measured == MEASURED_FRAMES));
        // The cut carries the pair that was read as one; the first scene has none.
        assert!(decisions[0].cut.is_none() && decisions[1].cut.is_some_and(|pair| pair.candidate));
    }

    #[test]
    fn frames_are_held_back_only_until_their_scene_is_decided() {
        let frames: Vec<Vec<u8>> = (0..20).map(|i| clean(i as f64)).collect();
        let mut scenes = decider(SceneConfig { analysis_frames: 5, ..Default::default() });
        for (index, data) in frames.iter().enumerate() {
            scenes.push(&Frame::luma(Plane::new(data, W, H, W).unwrap()));
            // The scene is decided once its fifth frame is settled, two frames after it arrived,
            // and from then on only the frames still being weighed as cuts are held.
            let expected = if index < 4 + CUT_PAIRS_AFTER { 0 } else { (index - CUT_PAIRS_AFTER) as u64 + 1 };
            assert_eq!(scenes.decided_frames(), expected, "after frame {index}");
        }
    }

    #[test]
    fn a_scene_cut_short_is_decided_from_what_it_had() {
        let mut frames: Vec<Vec<u8>> = (0..14).map(|i| clean(i as f64)).collect();
        frames.extend((0..5).map(|_| checker()));
        let mut scenes = decider(SceneConfig { analysis_frames: 20, ..Default::default() });
        let decisions = run(&mut scenes, &frames);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].estimate.frames_analyzed, 14);
        assert_eq!(scenes.decided_frames(), 14);
        let last = scenes.finish().unwrap();
        assert_eq!((last.start_frame, last.estimate.frames_analyzed), (14, 5));
    }

    #[test]
    fn cuts_closer_than_the_minimum_do_not_split_a_scene() {
        let mut frames: Vec<Vec<u8>> = (0..5).map(|i| clean(i as f64)).collect();
        frames.extend((0..30).map(|_| checker()));
        let mut scenes = decider(SceneConfig::default());
        let mut decisions = run(&mut scenes, &frames);
        decisions.extend(scenes.finish());
        assert_eq!(decisions.len(), 1);
    }

    #[test]
    fn a_long_scene_is_decided_again() {
        let frames: Vec<Vec<u8>> = (0..70).map(|i| clean(i as f64 * 0.5)).collect();
        let mut scenes = decider(SceneConfig { analysis_frames: 4, min_scene_frames: 4, max_scene_frames: 30 });
        let mut decisions = run(&mut scenes, &frames);
        decisions.extend(scenes.finish());
        let starts: Vec<_> = decisions.iter().map(|d| (d.start_frame, d.start)).collect();
        assert_eq!(starts, vec![(0, SceneStart::Start), (30, SceneStart::Split), (60, SceneStart::Split)]);
        assert!(decisions.windows(2).all(|pair| pair[0].crf == pair[1].crf));
    }

    // Fast action is one scene. Every frame here is a different piece of one busy picture, further
    // from the last than motion compensation reaches, so every pair looks like a cut by itself —
    // and none of them stands out from the pairs around it.
    #[test]
    fn fast_motion_is_not_a_run_of_cuts() {
        let busy = |shift: usize| {
            picture(W, H, |x, y| {
                let (x, y) = ((x + shift * 97) / 32, (y + shift * 53) / 32);
                ((x * 2654435761 + y * 40503 + x * y * 97) % 251) as f64
            })
        };
        let frames: Vec<Vec<u8>> = (0..40).map(busy).collect();
        let planes: Vec<LowRes> =
            frames.iter().map(|data| temporal::downsample(&Plane::new(data, W, H, W).unwrap())).collect();
        let candidates = planes.windows(2).filter(|pair| temporal::compare(&pair[0], &pair[1]).candidate).count();
        assert_eq!(candidates, 39);
        let mut scenes = decider(SceneConfig::default());
        let mut decisions = run(&mut scenes, &frames);
        decisions.extend(scenes.finish());
        assert_eq!(decisions.len(), 1, "{decisions:?}");
    }

    #[test]
    fn configuration_is_made_usable() {
        let config = SceneConfig { analysis_frames: 0, min_scene_frames: 0, max_scene_frames: 0 }.sanitised();
        assert_eq!(config, SceneConfig { analysis_frames: 1, min_scene_frames: 1, max_scene_frames: 1 });
    }
}
