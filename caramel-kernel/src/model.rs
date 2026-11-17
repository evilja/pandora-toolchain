// The CRF model: from the features measured at the start of a scene to that scene's CRF.
//
// The estimate is an *offset from an anchor*. The anchor is the CRF the encoder configuration was
// chosen at for typical content (a preset's own CRF), and the question asked of every scene is how
// far its CRF can move from there and still look like typical content does at the anchor. Anchoring
// keeps the model honest about what it does not know — the absolute CRF-to-quality curve of a
// particular encoder, preset and AQ configuration.
//
// There is no file-wide CRF. A release is a sequence of scenes that need different things — a
// static dialogue shot and a fast pan in one episode — and a single CRF has to be set for the
// hardest of them, overspending on the rest. Each scene is decided on its own.
//
// The model is fitted, not hand-set. The data behind it is 359 two-second scenes from eight
// open-licensed 1080p sources — cel and vector animation, painted animation, CG and live action —
// each encoded with x264 at CRF 11 to 29 and measured against its source with four metrics: XPSNR
// (mean and the worst frames), SSIM and VMAF (the worst frames). For one metric, a scene's offset is
// the CRF change at which it scores what the median scene scores at the anchor; one linear predictor
// per metric estimates that offset from eight features of the scene's first frames.
//
// The four metrics disagree — flat animation is easy by SSIM and ordinary by VMAF, a crowd is the
// reverse — and each predictor is off by about two CRF on a source it has never seen. So the scene
// moves only when all four agree on the direction by more than that error, and then only by what
// the most cautious of them allows. Most scenes stay at the anchor. On held-out sources that costs a
// raised scene 0.1 to 0.3 dB at the 95th percentile against the target and saves up to 8% of the
// bytes on animation and nothing on live action.
//
// Every scene is in the fit twice: as the source has it, and as it reads once a fine pattern lies
// over it. Real sources carry one. A 10-bit source reaches the decider as 8-bit frames, and ffmpeg
// dithers on the way down; an 8-bit web release reads the same way at about half the strength, as
// an encode of a dithered master does and an encode of pristine animation does not. Either way flat
// areas read as noisy (sigma 0.5 to 1.2 where the pristine picture has 0.01) and as full of fine
// detail, and predictors fitted on pristine frames alone answer with 1.4 to 5 CRF more bits per
// metric on every scene: on real releases almost nothing was raised. The pattern costs the encode little
// that the metrics count — against a 10-bit picture, encodes of dithered and of plainly rounded
// frames score the same at every CRF — so both readings are fitted to the one set of offsets, the
// pristine ones. That leaves the predictors trusting noise and fine detail only as far as the two
// readings agree, and the share of smooth blocks carries what they gave up.
//
// What it does not know: every fitted source is 1080p, the dithered readings are simulated from the
// pristine ones, and the encoder was x264 as Pandora's `standard` preset runs it (preset fast,
// aq-mode 3, aq-strength 0.8). Other resolutions get a small prior (`output-resolution`), not a fit.

use crate::curve;
use crate::probe::{PROBE_FROM, Probe};
use crate::window::WindowFeatures;
use serde::{Deserialize, Serialize};

// The fitted predictors below: what decides a scene when there is no network to.
pub const MODEL_VERSION: &str = "caramel-scene-6";

// The name of what decides scenes in this process: the active network's, else the predictors'.
pub fn model_version() -> String {
    curve::active().map_or_else(|| MODEL_VERSION.to_string(), |model| model.version.clone())
}

// The furthest the fitted predictors move a scene's CRF from the anchor in either direction. A
// network has bounds of its own (`curve::MAX_RAISE`, `curve::MAX_LOWER`).
const MAX_RAISE: f64 = 4.0;
const MAX_LOWER: f64 = 4.0;
// How far past zero every metric's offset must be before the scene moves, and what is taken off the
// most cautious one: the predictors' error on an unseen source, a little under 2 CRF on average.
const MARGIN: f64 = 1.5;
// Frames in the window at which a raise is trusted in full. A scene too short to give that many has
// its raise scaled down (never a cut): a misjudged easy scene is the failure that costs quality.
const FULL_CONFIDENCE_FRAMES: f64 = 6.0;
// A scene with no frame pair to measure motion on knows half of what it would.
const NO_MOTION_CONFIDENCE: f64 = 0.5;
// A window with a cut inside it is two shots measured as one, and the scene it decides is mostly the
// second. On the real releases measured, the votes of such scenes were 3.5 CRF from what the encodes
// allowed on the worst frames where the others were 2.1, and two of the three that were raised were
// raised too far, one by 3 CRF for 1.7 dB. Refusing them costs 0.04% of the bytes on the corpus.
const CUT_INSIDE_CONFIDENCE: f64 = 0.0;
// Granularity of the reported CRF. Coarse on purpose: a scene's CRF moving by a tenth between two
// shots of the same content is noise, and every change is a rate-control reconfiguration.
pub(crate) const CRF_STEP: f64 = 0.5;
// The anchors the fit covers. An anchor outside them is read as the nearest one inside.
const FITTED_ANCHORS: (f64, f64) = (15.0, 23.0);
const REFERENCE_ANCHOR: f64 = 19.0;
// The anchors a network's curves are read at. They are drawn from CRF 10 to 34 and a vote looks up
// to eight either way; an anchor outside this is read as the nearest inside.
pub const CURVE_ANCHORS: (f64, f64) = (14.0, 28.0);

// The features a predictor reads, in the order of its coefficients. Each is on a log scale — what
// matters is whether there is twice as much grain, not 0.1 more — and is held inside the range the
// fit saw, so a scene unlike anything measured is read as the nearest thing that was.
//
// In order: noise, what is new in each frame after motion compensation, how much changes between
// frames without it, the share of smooth blocks, temporal information, fine detail, spatial
// information, and the motion in pixels.
const INPUTS: usize = 8;
const INPUT_RANGE: [(f64, f64); INPUTS] = [
    (-1.386, 1.416),
    (-5.809, 0.003),
    (-1.386, 3.648),
    (0.0, 1.0),
    (-1.386, 4.167),
    (-5.809, -0.063),
    (-1.386, 4.981),
    (-1.386, 4.499),
];

struct Predictor {
    metric: &'static str,
    intercept: f64,
    weights: [f64; INPUTS],
    // Per CRF of anchor above the reference: how the offset drifts as the anchor moves.
    anchor: f64,
}

// Least squares over every scene read twice — as it is, and as a 10-bit source dithered down to 8
// bits — against the one set of offsets, at the anchors 15, 17, 19, 21 and 23 together.
const PREDICTORS: [Predictor; 4] = [
    Predictor {
        metric: "xpsnr",
        intercept: -11.1556,
        weights: [0.7599, -2.0333, -0.9224, 5.8412, 1.7263, -1.1685, 0.9566, 0.7769],
        anchor: -0.0797,
    },
    Predictor {
        metric: "xpsnr-low",
        intercept: -10.0106,
        weights: [0.6890, -1.8835, -1.0906, 6.5046, 2.0798, -0.9880, 0.6246, 0.3663],
        anchor: -0.0885,
    },
    Predictor {
        metric: "ssim",
        intercept: -11.8885,
        weights: [-0.3709, -1.5283, -1.7942, 12.1063, 2.5612, -0.3414, 0.5977, 0.0488],
        anchor: -0.1107,
    },
    Predictor {
        metric: "vmaf-low",
        intercept: -7.3637,
        weights: [1.1102, -1.7416, 4.5084, 3.7028, -0.5346, -1.2250, 0.2730, -0.9324],
        anchor: 0.1499,
    },
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceContext {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    // Video stream bitrate in bits per second, when the container reports one.
    pub video_bitrate: Option<f64>,
    // ffprobe codec name of the source, e.g. `h264`, `hevc`, `av1`.
    pub codec: Option<String>,
    pub bit_depth: u8,
}


#[derive(Clone, Copy, Debug, Serialize)]
pub struct EncodeContext {
    pub anchor_crf: f32,
    // The height the encode writes, when it scales; None for the source height.
    pub output_height: Option<u32>,
    pub output_bit_depth: u8,
    pub crf_floor: f32,
    pub crf_ceiling: f32,
}

impl EncodeContext {
    pub fn anchored(anchor_crf: f32) -> Self {
        Self {
            anchor_crf,
            output_height: None,
            output_bit_depth: 8,
            crf_floor: 0.0,
            crf_ceiling: 51.0,
        }
    }
}

// What one metric says about a scene: the CRF offset from the anchor at which the scene would score
// what typical content scores at the anchor. Positive is an easy scene.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MetricVote {
    pub metric: &'static str,
    pub offset: f64,
    // How far a model's networks disagree about the offset, in CRF. Zero from the fitted predictors,
    // which have no second opinion.
    pub spread: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Contribution {
    pub factor: &'static str,
    pub value: f64,
    pub delta: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SceneEstimate {
    pub crf: f32,
    pub anchor_crf: f32,
    // The applied offset from the anchor, before rounding.
    pub adjustment: f64,
    // Every contribution's delta summed, before confidence and bounds.
    pub raw_adjustment: f64,
    pub confidence: f64,
    pub frames_analyzed: usize,
    // The metrics' offsets for the scene: the ones it was decided on.
    pub votes: Vec<MetricVote>,
    // What moved the scene, then the factors that depend on the source and the encode. With the
    // fitted predictors that is `consensus`, what the votes agree on (its value is the vote that
    // held the others back). With a network it is `detail` — how many CRF of ease the scene has
    // over a detailed one — or `banding` when banding's vote held the raise back to that.
    pub contributions: Vec<Contribution>,
    // For a raise of at least `probe::PROBE_FROM` decided by a network: what a test encode of the
    // scene is held to (`probe`). None for every other scene.
    pub probe: Option<crate::probe::Probe>,
}

fn interpolate(points: &[(f64, f64)], x: f64) -> f64 {
    let (first, last) = (points[0], points[points.len() - 1]);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    last.1
}

// Shares of a block's energy sit between 0 and the square of the floor they were clipped at; the
// logarithm is of that share, with a small offset so an empty frame stays finite.
fn log_share(energy: f64, floor: f64) -> f64 {
    (energy / (floor * floor) + 0.003).ln()
}

fn log_level(value: f64) -> f64 {
    (value.max(0.0) + 0.25).ln()
}

// The predictors' inputs for one window, in the order of `INPUT_RANGE`. A window with no frame pair
// is read as a still: there is nothing to say it moves.
fn inputs(window: &WindowFeatures) -> [f64; INPUTS] {
    let spatial = &window.spatial;
    let temporal = window.temporal.unwrap_or_default();
    let raw = [
        log_level(spatial.noise_sigma),
        log_share(temporal.residual_energy[0], crate::temporal::RESIDUAL_FLOORS[0]),
        log_level(temporal.zero_residual),
        spatial.smooth_share,
        log_level(temporal.ti),
        log_share(spatial.detail_energy[0], crate::spatial::DETAIL_FLOORS[0]),
        log_level(spatial.si),
        log_level(temporal.motion_px),
    ];
    let mut held = [0.0; INPUTS];
    for (index, value) in raw.into_iter().enumerate() {
        let (low, high) = INPUT_RANGE[index];
        held[index] = if value.is_finite() { value.clamp(low, high) } else { low };
    }
    held
}

// Every metric's offset for a window at an anchor.
pub fn votes(window: &WindowFeatures, anchor_crf: f64) -> Vec<MetricVote> {
    let inputs = inputs(window);
    let anchor = anchor_crf.clamp(FITTED_ANCHORS.0, FITTED_ANCHORS.1) - REFERENCE_ANCHOR;
    PREDICTORS
        .iter()
        .map(|predictor| MetricVote {
            metric: predictor.metric,
            offset: predictor.intercept
                + predictor.weights.iter().zip(inputs).map(|(weight, input)| weight * input).sum::<f64>()
                + predictor.anchor * anchor,
            spread: 0.0,
        })
        .collect()
}

// What the votes agree on: nothing unless every one of them is past the margin on the same side,
// and then the most cautious of them, less the margin. The second value is that binding vote — the
// one nearest zero, which is what held the scene where it is.
fn consensus(votes: &[MetricVote]) -> (f64, f64) {
    let least = votes.iter().map(|vote| vote.offset).fold(f64::INFINITY, f64::min);
    let most = votes.iter().map(|vote| vote.offset).fold(f64::NEG_INFINITY, f64::max);
    if least > MARGIN {
        (least - MARGIN, least)
    } else if most < -MARGIN {
        (most + MARGIN, most)
    } else {
        (0.0, if least.abs() < most.abs() { least } else { most })
    }
}

// Artefacts in a lower-resolution encode are magnified by the player's upscale; a 4K encode's are
// shrunk by the viewer's distance. 1080p is the reference, and the only resolution the fit saw: this
// is a prior drawn from common practice. A resolution is named by its 16:9 frame, so a 1920x800
// scope picture is 1080p, not 800p.
fn resolution_contribution(source: &SourceContext, encode: &EncodeContext) -> Contribution {
    let scale = encode.output_height.map_or(1.0, |cap| (cap as f64 / source.height.max(1) as f64).min(1.0));
    let height = ((source.height as f64).max(source.width as f64 * 9.0 / 16.0) * scale).round();
    Contribution {
        factor: "output-resolution",
        value: height,
        delta: interpolate(
            &[(360.0, -2.0), (480.0, -1.5), (576.0, -1.0), (720.0, -0.5), (1080.0, 0.0), (1440.0, 0.5), (2160.0, 1.0)],
            height,
        ),
    }
}

fn round_crf(value: f64, encode: &EncodeContext) -> f32 {
    let rounded = (value / CRF_STEP).round() * CRF_STEP;
    (rounded as f32).clamp(encode.crf_floor, encode.crf_ceiling)
}

// One scene's CRF from the window measured at its start, by the model this process decides with.
pub fn estimate_scene(window: &WindowFeatures, source: &SourceContext, encode: &EncodeContext) -> SceneEstimate {
    estimate_scene_with(curve::active().as_deref(), window, source, encode)
}

// What a network's curves say about a window: the votes of the metrics a scene is decided on, and
// what they decide (`curve::decide`): the anchor for a detailed scene, a raise for one with quality
// to spare.
fn curve_votes(model: &curve::Model, window: &WindowFeatures, source: &SourceContext, anchor: f64) -> (Vec<MetricVote>, Contribution, Probe) {
    let inputs = crate::inputs::inputs(window, source.bit_depth > 8);
    let held = anchor.clamp(CURVE_ANCHORS.0, CURVE_ANCHORS.1);
    let (all, curves) = model.read(&inputs, held);
    let decision = curve::decide(&all, model.detail, model.gain);
    let votes = (0..curve::METRICS)
        .filter(|metric| curve::DECIDES[*metric])
        .map(|metric| MetricVote { metric: curve::METRIC_NAMES[metric], offset: all[metric].offset, spread: all[metric].spread })
        .collect();
    // A detailed scene at the anchor scores what typical content scores `detail` CRF the other way,
    // as `train::simulate` holds a raise to it.
    let probe = Probe { anchor, crf: 0.0, target: model.typical[curve::XPSNR].at(held - model.detail), curve: curves[curve::XPSNR].0, measured: None, settled: None };
    (votes, Contribution { factor: if decision.banding { "banding" } else { "detail" }, value: decision.ease - model.detail, delta: decision.offset }, probe)
}

// The same with the model named: a network, or None for the fitted predictors.
//
// A model that reads the fast encode cannot decide a scene that has none — an encode it could not be
// made for, such as a 10-bit one — and that scene is decided by the built-in model instead, which
// reads the source alone.
pub fn estimate_scene_with(model: Option<&curve::Model>, window: &WindowFeatures, source: &SourceContext, encode: &EncodeContext) -> SceneEstimate {
    let fallback;
    let model = match model {
        Some(model) if model.reads_fast_encode() && window.fast_encode.is_none() => {
            fallback = curve::builtin().filter(|builtin| !builtin.reads_fast_encode());
            fallback.as_deref()
        }
        model => model,
    };
    let anchor = encode.anchor_crf as f64;
    if window.frames == 0 || window.measured == 0 {
        return SceneEstimate {
            crf: round_crf(anchor, encode),
            anchor_crf: encode.anchor_crf,
            adjustment: 0.0,
            raw_adjustment: 0.0,
            confidence: 0.0,
            frames_analyzed: window.frames,
            votes: Vec::new(),
            contributions: Vec::new(),
            probe: None,
        };
    }
    let (votes, decided, (max_lower, max_raise), probe) = match model {
        Some(model) => {
            let (votes, decided, probe) = curve_votes(model, window, source, anchor);
            (votes, decided, (curve::MAX_LOWER, curve::MAX_RAISE), Some(probe))
        }
        None => {
            let votes = votes(window, anchor);
            let (agreed, binding) = consensus(&votes);
            (votes, Contribution { factor: "consensus", value: binding, delta: agreed }, (MAX_LOWER, MAX_RAISE), None)
        }
    };
    let contributions = vec![decided, resolution_contribution(source, encode)];
    let raw_adjustment: f64 = contributions.iter().map(|factor| factor.delta).sum();
    let confidence = (window.frames as f64 / FULL_CONFIDENCE_FRAMES).min(1.0)
        * if window.temporal.is_some() { 1.0 } else { NO_MOTION_CONFIDENCE }
        * if window.cuts == 0 { 1.0 } else { CUT_INSIDE_CONFIDENCE };
    let scaled = if raw_adjustment > 0.0 { raw_adjustment * confidence } else { raw_adjustment };
    let adjustment = scaled.clamp(-max_lower, max_raise);
    let crf = round_crf(anchor + adjustment, encode);
    let probe = probe.filter(|_| adjustment >= PROBE_FROM).map(|probe| Probe { crf, ..probe });
    SceneEstimate {
        crf,
        anchor_crf: encode.anchor_crf,
        adjustment,
        raw_adjustment,
        confidence,
        frames_analyzed: window.frames,
        votes,
        contributions,
        probe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::SpatialFeatures;
    use crate::window::TemporalSummary;

    // The fitted predictors, whatever network this build carries.
    fn linear_scene(window: &WindowFeatures, source: &SourceContext, encode: &EncodeContext) -> SceneEstimate {
        estimate_scene_with(None, window, source, encode)
    }

    fn source() -> SourceContext {
        SourceContext {
            width: 1920,
            height: 1080,
            fps: 24000.0 / 1001.0,
            video_bitrate: Some(8_000_000.0),
            codec: Some("h264".to_string()),
            bit_depth: 8,
        }
    }

    // A window as the decider builds one, from the eight figures the model reads: noise, smooth
    // share, SI, TI, zero-motion residual, motion in pixels, and the fine-detail and new-detail
    // energies at the lowest floor.
    fn window(figures: [f64; 8]) -> WindowFeatures {
        let [noise_sigma, smooth_share, si, ti, zero_residual, motion_px, detail, residual] = figures;
        WindowFeatures {
            position_seconds: 0.0,
            frames: 12,
            measured: 4,
            cuts: 0,
            spatial: SpatialFeatures {
                si,
                noise_sigma,
                smooth_share,
                detail_energy: [detail, 0.0, 0.0, 0.0, 0.0],
                ..Default::default()
            },
            chroma_si: Some(8.0),
            temporal: Some(TemporalSummary {
                ti,
                zero_residual,
                motion_px,
                residual_energy: [residual, 0.0, 0.0, 0.0, 0.0],
                ..Default::default()
            }),
            fast_encode: None,
        }
    }

    // A model that reads the fast encode decides a scene that has one itself, and leaves a scene
    // without one to the built-in model, as if it had been handed that.
    #[test]
    fn a_scene_without_a_fast_encode_is_left_to_the_built_in_model() {
        let fast = crate::curve::tests::fast_model(0.7);
        let builtin = crate::curve::builtin().expect("this build has a built-in model");
        let (source, encode) = (SourceContext { width: 1920, height: 1080, fps: 24.0, video_bitrate: None, codec: None, bit_depth: 8 }, EncodeContext::anchored(17.0));
        let bare = window([1.0, 0.6, 20.0, 2.0, 1.0, 1.5, 0.3, 0.2]);
        let without = estimate_scene_with(Some(&fast), &bare, &source, &encode);
        let by_builtin = estimate_scene_with(Some(&builtin), &bare, &source, &encode);
        assert_eq!((without.crf, without.adjustment), (by_builtin.crf, by_builtin.adjustment));
        let measured = WindowFeatures { fast_encode: Some(crate::fast::FastEncode { xpsnr: 42.0, xpsnr_low: 40.0, log_bpp: -5.0, frames: 48 }), ..bare.clone() };
        let with = estimate_scene_with(Some(&fast), &measured, &source, &encode);
        // The flat model raises every scene alike; it is its own answer, not the built-in one's.
        assert_eq!(with.crf, estimate_scene_with(Some(&crate::curve::tests::flat_model(0.7)), &bare, &source, &encode).crf);
    }

    fn offsets(window: &WindowFeatures, anchor: f64) -> Vec<f64> {
        votes(window, anchor).iter().map(|vote| vote.offset).collect()
    }

    fn close(got: &[f64], want: [f64; 4]) -> bool {
        got.iter().zip(want).all(|(got, want)| (got - want).abs() < 0.06)
    }

    // Real scenes of the corpus the model was fitted on, as the kernel measured them.
    // A fast pan across a flat-shaded shot (Morevna ep. 3, frame 152).
    const FLAT_PAN: [f64; 8] = [0.005, 0.817, 31.55, 27.78, 14.24, 89.4, 0.0178, 0.171];
    // A slow, softly lit CG close-up with a little render noise (Sintel, frame 605).
    const CG_CLOSE_UP: [f64; 8] = [0.5453, 0.3863, 26.68, 3.68, 1.519, 1.374, 0.1502, 0.2185];
    // An ordinary painted-animation shot (Pepper & Carrot ep. 6, frame 48).
    const PAINTED: [f64; 8] = [0.0303, 0.6462, 34.05, 10.36, 2.943, 4.16, 0.0579, 0.1547];
    // A crowd of runners, live action (derf crowd_run).
    const CROWD: [f64; 8] = [2.0411, 0.0219, 93.8, 14.32, 6.712, 1.357, 0.2223, 0.25];

    // The same four scenes as the kernel measures them when the clip is a 10-bit source brought down
    // to 8 bits: noise near 1.06 and fine detail near 0.22 whatever the picture, the rest unmoved.
    const FLAT_PAN_DITHERED: [f64; 8] = [1.0588, 0.817, 31.52, 27.79, 14.26, 89.7, 0.2312, 0.1794];
    const CG_CLOSE_UP_DITHERED: [f64; 8] = [1.1481, 0.3861, 26.67, 3.68, 1.52, 1.356, 0.2088, 0.2185];
    const PAINTED_DITHERED: [f64; 8] = [1.0625, 0.6459, 34.02, 10.36, 2.945, 4.308, 0.2232, 0.1553];
    const CROWD_DITHERED: [f64; 8] = [2.2483, 0.0219, 93.8, 14.32, 6.711, 1.349, 0.2257, 0.25];

    // Scenes of a real release as the server measured them: a 24-minute 1080p episode from an 8 Mbps
    // H.264 web source, whose flat areas read sigma 0.4 to 0.75. A still, a fast action shot, and a
    // textured still.
    const WEB_STILL: [f64; 8] = [0.5424, 0.7968, 65.82, 0.001868, 0.0000136, 0.0, 0.1483, 0.00000037];
    const WEB_ACTION: [f64; 8] = [0.4197, 0.7444, 11.37, 48.34, 26.44, 99.76, 0.09506, 0.2318];
    const WEB_TEXTURE: [f64; 8] = [0.7399, 0.1499, 44.38, 0.9275, 0.1817, 0.0382, 0.1813, 0.03182];

    // Dither is not difficulty: a scene reads much the same with and without it, the pan is raised
    // in either reading (by less under dither, where there is less to go on), the close-up is
    // lowered in either, and the painted shot and the crowd stay where they are.
    #[test]
    fn a_scene_is_not_given_bits_for_its_dither() {
        let encode = EncodeContext::anchored(19.0);
        for (clean, dithered) in [(FLAT_PAN, FLAT_PAN_DITHERED), (CG_CLOSE_UP, CG_CLOSE_UP_DITHERED), (PAINTED, PAINTED_DITHERED), (CROWD, CROWD_DITHERED)] {
            let (a, b) = (offsets(&window(clean), 19.0), offsets(&window(dithered), 19.0));
            assert!(a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1.9), "{a:?} {b:?}");
        }
        let pan = window(FLAT_PAN_DITHERED);
        assert!(close(&offsets(&pan, 19.0), [4.67, 4.00, 4.46, 3.65]), "{:?}", offsets(&pan, 19.0));
        assert_eq!(linear_scene(&pan, &source(), &encode).crf, 21.0);
        let painted = window(PAINTED_DITHERED);
        assert!(close(&offsets(&painted, 19.0), [1.48, 1.78, 2.74, -0.19]), "{:?}", offsets(&painted, 19.0));
        assert_eq!(linear_scene(&painted, &source(), &encode).crf, 19.0);
        let close_up = window(CG_CLOSE_UP_DITHERED);
        assert!(close(&offsets(&close_up, 19.0), [-2.82, -2.39, -2.60, -2.82]), "{:?}", offsets(&close_up, 19.0));
        assert_eq!(linear_scene(&close_up, &source(), &encode).crf, 18.0);
        let crowd = window(CROWD_DITHERED);
        assert!(close(&offsets(&crowd, 19.0), [-2.68, -2.68, -5.81, 1.97]), "{:?}", offsets(&crowd, 19.0));
        assert_eq!(linear_scene(&crowd, &source(), &encode).crf, 19.0);
        // The medians of the corpus's kinds of source, read as dithered, stay at the anchor.
        for typical in [
            [1.1061, 0.4713, 31.27, 6.943, 2.567, 4.46, 0.2141, 0.1748],
            [1.0614, 0.7064, 39.92, 6.329, 2.465, 5.768, 0.226, 0.1173],
            [1.0533, 0.3412, 31.42, 8.398, 3.507, 2.0, 0.2201, 0.1863],
            [1.0621, 0.5811, 39.62, 11.85, 3.941, 6.783, 0.2232, 0.1577],
        ] {
            for anchor in [15.0, 19.0, 21.0, 23.0] {
                let estimate = linear_scene(&window(typical), &source(), &EncodeContext::anchored(anchor));
                assert_eq!(estimate.crf, anchor, "{estimate:?}");
            }
        }
    }

    // What the encodes of that release allowed at anchor 21, measured over a ladder of CRFs: the
    // still 6.9 CRF more by XPSNR and more by the others, the action shot 9.6 or more, the textured
    // still nothing (-0.8 to +1.3). `caramel-scene-3`, fitted on pristine frames, left the first
    // two at the anchor: it read the web encode's leftover dither as grain.
    #[test]
    fn a_web_release_has_its_easy_scenes_raised() {
        let encode = EncodeContext::anchored(21.0);
        let still = window(WEB_STILL);
        assert!(close(&offsets(&still, 21.0), [7.41, 7.04, 8.07, 3.30]), "{:?}", offsets(&still, 21.0));
        assert_eq!(linear_scene(&still, &source(), &encode).crf, 23.0);
        let action = window(WEB_ACTION);
        assert!(close(&offsets(&action, 21.0), [3.51, 3.14, 3.23, 5.75]), "{:?}", offsets(&action, 21.0));
        assert_eq!(linear_scene(&action, &source(), &encode).crf, 22.5);
        let texture = window(WEB_TEXTURE);
        assert!(close(&offsets(&texture, 21.0), [-2.21, -1.89, -2.93, -4.26]), "{:?}", offsets(&texture, 21.0));
        assert_eq!(linear_scene(&texture, &source(), &encode).crf, 20.5);
    }

    #[test]
    fn an_unmeasured_scene_keeps_the_anchor() {
        let estimate = linear_scene(&WindowFeatures::default(), &source(), &EncodeContext::anchored(17.0));
        assert_eq!(estimate.crf, 17.0);
        assert_eq!(estimate.confidence, 0.0);
        assert!(estimate.votes.is_empty());
    }

    // Every metric held 8 CRF more on this pan than on typical content; the model says 5 to 6.5 and
    // the scene is raised by the least of them less the margin.
    #[test]
    fn a_fast_pan_over_flat_shading_is_raised() {
        let scene = window(FLAT_PAN);
        assert!(close(&offsets(&scene, 19.0), [6.47, 5.46, 6.00, 5.01]), "{:?}", offsets(&scene, 19.0));
        let estimate = linear_scene(&scene, &source(), &EncodeContext::anchored(19.0));
        assert_eq!(estimate.crf, 22.5, "{estimate:?}");
        assert_eq!(estimate.contributions[0].factor, "consensus");
        assert!((estimate.contributions[0].value - 5.01).abs() < 0.06);
    }

    // Measured offsets -2.2, -1.7, -5.2 and -2.1: every metric wants more bits here.
    #[test]
    fn a_noisy_cg_close_up_is_lowered() {
        let scene = window(CG_CLOSE_UP);
        assert!(close(&offsets(&scene, 19.0), [-2.85, -2.45, -2.28, -3.05]), "{:?}", offsets(&scene, 19.0));
        let estimate = linear_scene(&scene, &source(), &EncodeContext::anchored(19.0));
        assert_eq!(estimate.crf, 18.0, "{estimate:?}");
    }

    // The anchor is the right CRF for typical content, so typical content must land on it — and so
    // must content the metrics disagree about: the crowd is 6 CRF hard by SSIM and easy by VMAF.
    #[test]
    fn typical_and_disputed_scenes_stay_at_the_anchor() {
        let encode = EncodeContext::anchored(19.0);
        let painted = linear_scene(&window(PAINTED), &source(), &encode);
        assert!(close(&offsets(&window(PAINTED), 19.0), [1.85, 2.04, 3.78, -0.23]), "{painted:?}");
        assert_eq!(painted.crf, 19.0);
        let crowd = linear_scene(&window(CROWD), &source(), &encode);
        assert!(close(&offsets(&window(CROWD), 19.0), [-2.72, -2.72, -5.78, 1.89]), "{crowd:?}");
        assert_eq!(crowd.crf, 19.0);
        // The medians of the four kinds of source in the corpus: CG, flat animation, live action
        // and painted animation. None of them is moved.
        for typical in [
            [0.378, 0.471, 31.3, 6.94, 2.57, 4.46, 0.120, 0.174],
            [0.0139, 0.633, 51.4, 8.63, 2.65, 2.96, 0.0502, 0.097],
            [0.199, 0.311, 34.0, 8.79, 4.04, 2.27, 0.0769, 0.211],
            [0.0824, 0.555, 39.1, 10.5, 3.57, 5.21, 0.0694, 0.161],
        ] {
            for anchor in [15.0, 17.0, 21.0, 23.0] {
                let estimate = linear_scene(&window(typical), &source(), &EncodeContext::anchored(anchor));
                assert_eq!(estimate.crf, anchor, "{estimate:?}");
            }
        }
    }

    #[test]
    fn a_short_scene_has_its_raise_shrunk_but_not_its_cut() {
        let encode = EncodeContext::anchored(19.0);
        let short = |figures| WindowFeatures { frames: 2, measured: 1, ..window(figures) };
        assert!(
            linear_scene(&short(FLAT_PAN), &source(), &encode).adjustment
                < linear_scene(&window(FLAT_PAN), &source(), &encode).adjustment
        );
        assert_eq!(
            linear_scene(&short(CG_CLOSE_UP), &source(), &encode).adjustment,
            linear_scene(&window(CG_CLOSE_UP), &source(), &encode).adjustment
        );
        // A single frame has no motion to measure, which halves what a raise is trusted with, and is
        // read as a still.
        let single = WindowFeatures { temporal: None, ..window(FLAT_PAN) };
        let estimate = linear_scene(&single, &source(), &encode);
        assert_eq!(estimate.confidence, NO_MOTION_CONFIDENCE);
        let still = WindowFeatures { temporal: Some(TemporalSummary::default()), ..window(FLAT_PAN) };
        assert_eq!(offsets(&single, 19.0), offsets(&still, 19.0));
    }

    // Two shots in one window: the easy one's raise is not given to the other, and a scene that
    // reads hard is still given its bits.
    #[test]
    fn a_window_with_a_cut_inside_is_not_raised() {
        let encode = EncodeContext::anchored(19.0);
        let split = |figures| WindowFeatures { cuts: 1, ..window(figures) };
        assert!(linear_scene(&window(FLAT_PAN), &source(), &encode).crf > 19.0);
        let estimate = linear_scene(&split(FLAT_PAN), &source(), &encode);
        assert_eq!((estimate.crf, estimate.confidence), (19.0, 0.0));
        assert!(estimate.raw_adjustment > 0.0);
        assert_eq!(
            linear_scene(&split(CG_CLOSE_UP), &source(), &encode).crf,
            linear_scene(&window(CG_CLOSE_UP), &source(), &encode).crf
        );
        assert!(linear_scene(&split(CG_CLOSE_UP), &source(), &encode).crf < 19.0);
    }

    // A scene unlike anything in the corpus is read as the nearest thing that was, and whatever the
    // predictors say the CRF stays within four of the anchor.
    #[test]
    fn the_estimate_stays_inside_its_bounds() {
        let extreme = [
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [90.0, 0.0, 900.0, 900.0, 900.0, 900.0, 0.25, 0.25],
            [f64::NAN, 0.5, f64::INFINITY, 1.0, 1.0, 1.0, 0.1, 0.1],
        ];
        for figures in extreme {
            for anchor in [8.0, 19.0, 30.0] {
                let estimate = linear_scene(&window(figures), &source(), &EncodeContext::anchored(anchor));
                assert!(estimate.votes.iter().all(|vote| vote.offset.is_finite()), "{estimate:?}");
                assert!((estimate.crf - anchor as f32).abs() <= 4.0, "{estimate:?}");
            }
        }
        // An anchor outside the fitted ones is read as the nearest inside.
        assert_eq!(offsets(&window(PAINTED), 30.0), offsets(&window(PAINTED), 23.0));
    }

    #[test]
    fn a_smaller_output_is_given_more_bits() {
        let small = EncodeContext { output_height: Some(720), ..EncodeContext::anchored(19.0) };
        assert_eq!(linear_scene(&window(PAINTED), &source(), &small).crf, 18.5);
        // And the resolution prior counts against a raise the metrics agreed on.
        let full = linear_scene(&window(FLAT_PAN), &source(), &EncodeContext::anchored(19.0));
        assert!(linear_scene(&window(FLAT_PAN), &source(), &small).adjustment < full.adjustment);
        // A letterboxed 1080p picture is 1080p, and its 720p encode is 720p.
        let scope = SourceContext { height: 800, ..source() };
        let estimate = linear_scene(&window(PAINTED), &scope, &EncodeContext::anchored(19.0));
        assert_eq!((estimate.crf, estimate.contributions[1].value), (19.0, 1080.0));
        let scaled = EncodeContext { output_height: Some(534), ..EncodeContext::anchored(19.0) };
        assert_eq!(linear_scene(&window(PAINTED), &scope, &scaled).contributions[1].value, 721.0);
    }

    // A network decides in place of the predictors when one is given: the anchor for a detailed
    // scene, a raise for an easier one, and the bounds, the short-scene confidence and the refusal
    // of a window with a cut inside hold as they do without it.
    #[test]
    fn a_network_decides_when_there_is_one() {
        let mut model = curve::tests::flat_model(1.0);
        let encode = EncodeContext::anchored(17.0);
        // Every scene reads as typical content, two CRF easier than a detailed scene: half of it.
        let typical = estimate_scene_with(Some(&model), &window(PAINTED), &source(), &encode);
        assert_eq!((typical.crf, typical.votes.len()), (18.0, 5));
        assert_eq!(typical.votes.iter().map(|vote| vote.metric).collect::<Vec<_>>(), ["xpsnr", "xpsnr-low", "ssim", "vmaf-low", "banding"]);
        assert_eq!((typical.contributions[0].factor, typical.contributions[0].value), ("detail", 2.0));
        // A raise of one CRF is not test-encoded.
        assert!(typical.probe.is_none());
        // Every level a unit and a half up: six CRF easier still, and banding's slack ahead of
        // that, so the raise is half of eight — held to the bound by nothing yet.
        for member in &mut model.members {
            for metric in 0..curve::METRICS {
                member.layers[1].bias[metric * curve::KNOT_COUNT + 3] = 1.5;
            }
        }
        let easy = estimate_scene_with(Some(&model), &window(PAINTED), &source(), &encode);
        assert!((easy.raw_adjustment - 4.0).abs() < 1e-6, "{easy:?}");
        assert_eq!(easy.crf, 21.0);
        // A raise of four is: at the CRF chosen, held to what a detailed scene scores at the anchor
        // and read along the scene's own XPSNR curve.
        let probe = easy.probe.clone().expect("a raise of four is probed");
        assert_eq!((probe.anchor, probe.crf, probe.measured), (17.0, 21.0, None));
        assert!((probe.target - model.typical[curve::XPSNR].at(17.0 - model.detail)).abs() < 1e-9);
        assert!(probe.curve.windows(2).all(|pair| pair[0] >= pair[1]), "{probe:?}");
        // Followed in full it would be eight: the bound is six.
        model.gain = 1.0;
        assert_eq!(estimate_scene_with(Some(&model), &window(PAINTED), &source(), &encode).crf, 23.0);
        let split = WindowFeatures { cuts: 1, ..window(PAINTED) };
        assert_eq!(estimate_scene_with(Some(&model), &split, &source(), &encode).crf, 17.0);
        // A scene far harder than a detailed one is lowered, by half a CRF at most.
        for member in &mut model.members {
            for metric in 0..curve::METRICS {
                member.layers[1].bias[metric * curve::KNOT_COUNT + 3] = -3.0;
            }
        }
        let hard = estimate_scene_with(Some(&model), &window(PAINTED), &source(), &encode);
        assert_eq!(hard.crf, 16.5, "{hard:?}");
        assert!(hard.probe.is_none());
        // An anchor outside what the curves cover is read as the nearest inside, and stays bounded.
        for anchor in [6.0, 40.0] {
            let estimate = estimate_scene_with(Some(&model), &window(PAINTED), &source(), &EncodeContext::anchored(anchor));
            assert!(estimate.crf - anchor as f32 <= 6.0 && anchor as f32 - estimate.crf <= 0.5 && estimate.votes.iter().all(|vote| vote.offset.is_finite()), "{estimate:?}");
        }
    }
}
