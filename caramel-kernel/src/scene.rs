// Scenes, and a CRF for each, decided while the frames stream past.
//
// `SceneDecider` is fed every frame in display order, exactly as an encoder is. It compares each
// frame with the one before for a cut, and measures the first `analysis_frames` of every scene. As
// soon as a scene has been measured (or has ended before that), its CRF is decided and handed back.
// An encoder in front of it therefore only has to hold back the few frames of a scene that arrive
// before its decision: everything after streams straight through at the decided CRF.
//
// Deciding from a scene's opening frames is what makes it streamable, and it is enough because a
// scene is the unit that is homogeneous — a shot does not turn from clean line art into film grain
// halfway through. A scene that runs past `max_scene_frames` is decided again from that point, so a
// long pan or a missed cut cannot carry one decision across minutes of different content.

use crate::frame::{Frame, Plane};
use crate::model::{EncodeContext, SceneEstimate, SourceContext, estimate_scene};
use crate::temporal::{self, LowRes, TemporalFeatures};
use crate::window::WindowAnalyzer;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SceneConfig {
    // Frames measured at the start of every scene. Also how many frames an encoder holds back.
    pub analysis_frames: usize,
    // A cut sooner than this after the last one is ignored, so a flash or a run of false cuts in
    // fast action does not make every few frames a scene of its own.
    pub min_scene_frames: usize,
    // A scene this long is split and decided again.
    pub max_scene_frames: usize,
}

impl Default for SceneConfig {
    fn default() -> Self {
        // Half a second of measurement at 24 fps, and x264's own default keyint as the longest a
        // decision lasts.
        Self { analysis_frames: 12, min_scene_frames: 12, max_scene_frames: 250 }
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
    // Pairs inside the measured frames that also read as cuts but came too soon after this one. A
    // scene whose every pair is one is a fade, flash or effect the detector cannot follow.
    pub cut_pairs_within: usize,
    pub estimate: SceneEstimate,
}

struct Previous {
    luma: Vec<u8>,
    width: usize,
    height: usize,
    lowres: LowRes,
}

pub struct SceneDecider {
    config: SceneConfig,
    source: SourceContext,
    encode: EncodeContext,
    pushed: u64,
    scene_start: u64,
    scene_kind: SceneStart,
    scene_cut: Option<TemporalFeatures>,
    // Present while the current scene is still being measured.
    analyzer: Option<WindowAnalyzer>,
    measured: usize,
    decided: bool,
    previous: Option<Previous>,
}

impl SceneDecider {
    pub fn new(config: SceneConfig, source: SourceContext, encode: EncodeContext) -> Self {
        Self {
            config: config.sanitised(),
            source,
            encode,
            pushed: 0,
            scene_start: 0,
            scene_kind: SceneStart::Start,
            scene_cut: None,
            analyzer: None,
            measured: 0,
            decided: false,
            previous: None,
        }
    }

    // Frames pushed so far whose CRF is known: everything before this index may be encoded.
    pub fn decided_frames(&self) -> u64 {
        if self.decided { self.pushed } else { self.scene_start }
    }

    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    // The next frame, in display order. Returns the decisions it made final — usually none, at most
    // two (the scene it ended, and the new one when a single frame is all `analysis_frames` asks).
    pub fn push(&mut self, frame: &Frame) -> Vec<SceneDecision> {
        let mut decisions = Vec::new();
        let index = self.pushed;
        let lowres = temporal::downsample(&frame.y);
        let pair = self.previous.as_ref().and_then(|previous| {
            let plane = Plane::new(&previous.luma, previous.width, previous.height, previous.width)?;
            Some(temporal::compare(&plane, &frame.y, &previous.lowres, &lowres))
        });
        let length = index - self.scene_start;
        let boundary = if index == 0 {
            Some(SceneStart::Start)
        } else if pair.as_ref().is_some_and(|pair| pair.cut) && length >= self.config.min_scene_frames as u64 {
            Some(SceneStart::Cut)
        } else if length >= self.config.max_scene_frames as u64 {
            Some(SceneStart::Split)
        } else {
            None
        };
        if let Some(kind) = boundary {
            if let Some(decision) = self.close() {
                decisions.push(decision);
            }
            self.scene_start = index;
            self.scene_kind = kind;
            self.scene_cut = if kind == SceneStart::Cut { pair } else { None };
            self.analyzer = Some(WindowAnalyzer::new(index as f64 / self.source.fps.max(1.0)));
            self.measured = 0;
            self.decided = false;
        }
        if let Some(analyzer) = self.analyzer.as_mut() {
            // The pair into a scene's first frame belongs to the scene before it.
            analyzer.push_measured(frame, if boundary.is_some() { None } else { pair.as_ref() });
            self.measured += 1;
            if self.measured >= self.config.analysis_frames {
                decisions.push(self.decide());
            }
        }
        self.remember(frame, lowres);
        self.pushed += 1;
        decisions
    }

    // The end of the stream: decides a scene that ended before it was fully measured.
    pub fn finish(mut self) -> Option<SceneDecision> {
        self.close()
    }

    fn close(&mut self) -> Option<SceneDecision> {
        (self.pushed > 0 && !self.decided).then(|| self.decide())
    }

    fn decide(&mut self) -> SceneDecision {
        let features = self.analyzer.take().map(WindowAnalyzer::finish).unwrap_or_default();
        let estimate = estimate_scene(&features, &self.source, &self.encode);
        self.decided = true;
        SceneDecision {
            start_frame: self.scene_start,
            crf: estimate.crf,
            start: self.scene_kind,
            cut: self.scene_cut,
            cut_pairs_within: features.cuts,
            estimate,
        }
    }

    // The previous frame is kept as a packed copy, in a buffer reused from frame to frame: the
    // caller's planes are only borrowed until it decodes the next frame into them.
    fn remember(&mut self, frame: &Frame, lowres: LowRes) {
        let (width, height) = (frame.y.width, frame.y.height);
        let mut luma = self.previous.take().map(|previous| previous.luma).unwrap_or_default();
        luma.clear();
        luma.reserve(width * height);
        for y in 0..height {
            luma.extend_from_slice(frame.y.row(y));
        }
        self.previous = Some(Previous { luma, width, height, lowres });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::tests::{Noise, picture};

    const W: usize = 320;
    const H: usize = 192;

    fn source() -> SourceContext {
        SourceContext { width: W as u32, height: H as u32, fps: 24.0, video_bitrate: None, codec: None, bit_depth: 8 }
    }

    fn decider(config: SceneConfig) -> SceneDecider {
        SceneDecider::new(config, source(), EncodeContext::anchored(17.0))
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
        // The cut carries the pair that was read as one; the first scene has none.
        assert!(decisions[0].cut.is_none() && decisions[1].cut.is_some_and(|pair| pair.cut));
    }

    #[test]
    fn frames_are_held_back_only_until_their_scene_is_decided() {
        let frames: Vec<Vec<u8>> = (0..20).map(|i| clean(i as f64)).collect();
        let mut scenes = decider(SceneConfig { analysis_frames: 5, ..Default::default() });
        for (index, data) in frames.iter().enumerate() {
            scenes.push(&Frame::luma(Plane::new(data, W, H, W).unwrap()));
            let expected = if index < 4 { 0 } else { index as u64 + 1 };
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

    #[test]
    fn configuration_is_made_usable() {
        let config = SceneConfig { analysis_frames: 0, min_scene_frames: 0, max_scene_frames: 0 }.sanitised();
        assert_eq!(config, SceneConfig { analysis_frames: 1, min_scene_frames: 1, max_scene_frames: 1 });
    }
}
