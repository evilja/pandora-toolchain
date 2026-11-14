// The CRF model: from the features measured at the start of a scene to that scene's CRF.
//
// The estimate is an *offset from an anchor*. The anchor is the CRF the encoder configuration was
// chosen at for typical content (a preset's own CRF), and every factor below moves it by a bounded
// number of CRF steps: up where the content masks artefacts or cannot benefit from more bits, down
// where it is fragile. Anchoring keeps the model honest about what it does not know — the absolute
// CRF-to-quality curve of a particular encoder, preset and AQ configuration — and lets one table of
// factors serve x264, x265 and SVT-AV1 presets alike.
//
// There is no file-wide CRF. A release is a sequence of scenes that need different things — a
// static dialogue shot and a grainy night scene in one episode — and a single CRF has to be set for
// the hardest of them, overspending on the rest. Each scene is decided on its own.
//
// The factor table is a general prior drawn from common encoding practice, not a fit to measured
// data. Every threshold and weight is a named constant here so a calibration pass against
// brute-force CRF searches can replace them without touching the feature code.

use crate::window::WindowFeatures;
use serde::Serialize;

pub const MODEL_VERSION: &str = "caramel-scene-1";

// The furthest a scene's CRF may move from the anchor in either direction.
const MAX_RAISE: f64 = 4.0;
const MAX_LOWER: f64 = 4.0;
// Frames measured at which a raise is trusted in full. A scene too short to measure that many has
// its raise scaled down (never a cut): a misjudged easy scene is the failure that costs quality.
const FULL_CONFIDENCE_FRAMES: f64 = 6.0;
// A scene with no frame pair to measure motion on knows half of what it would.
const NO_MOTION_CONFIDENCE: f64 = 0.5;
// Granularity of the reported CRF. Coarse on purpose: a scene's CRF moving by a tenth between two
// shots of the same content is noise, and every change is a rate-control reconfiguration.
const CRF_STEP: f64 = 0.5;
// Where banding starts to count, as a share of the frame in smooth gradient blocks. Fitted to the
// distribution of real TV anime releases: cel fills with soft shading put 30–70% of a typical frame
// in such blocks, and the anchor is already the right CRF for typical content — x264's aq-mode 3
// spends on flat and dark blocks by itself. Only frames that are mostly gradient, a sky or a dark
// fade, are past what the anchor already covers. Thresholds of 5–35% put every anime frame at the
// full penalty and every scene at the lower bound, which leaves nothing for a per-scene CRF to vary.
const BANDING_FROM: f64 = 0.55;
const BANDING_FULL: f64 = 0.90;
const BANDING_DELTA: f64 = -1.5;
const DARK_BANDING_FROM: f64 = 0.45;
const DARK_BANDING_FULL: f64 = 0.85;
const DARK_BANDING_DELTA: f64 = -1.0;
// A still shot is scrutinised, but stills are also most of what a typical anime episode is.
const STILL_DELTA: f64 = -0.25;

#[derive(Clone, Debug, Serialize)]
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
    // Every factor's delta summed, before confidence and bounds.
    pub raw_adjustment: f64,
    pub confidence: f64,
    pub frames_analyzed: usize,
    // The scene's own factors, then the ones that depend on the source and the encode.
    pub contributions: Vec<Contribution>,
}

fn ramp(value: f64, low: f64, high: f64) -> f64 {
    ((value - low) / (high - low)).clamp(0.0, 1.0)
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

// How many bits of H.264 one bit of the source codec is worth, roughly, at equal quality. Only used
// to put sources of different codecs on one bits-per-pixel scale.
fn codec_efficiency(codec: Option<&str>) -> f64 {
    match codec {
        Some("hevc") => 1.6,
        Some("av1") => 2.0,
        Some("vp9") => 1.5,
        Some("mpeg4") => 0.7,
        Some("mpeg2video") => 0.5,
        _ => 1.0,
    }
}

// Factors that depend on the encode and the source as a whole rather than on any window.
fn global_contributions(source: &SourceContext, encode: &EncodeContext, detail: f64) -> Vec<Contribution> {
    let mut factors = Vec::new();
    // Artefacts in a lower-resolution encode are magnified by the player's upscale; a 4K encode's
    // are shrunk by the viewer's distance. 1080p is the reference.
    let height = encode.output_height.map_or(source.height, |cap| cap.min(source.height)) as f64;
    factors.push(Contribution {
        factor: "output-resolution",
        value: height,
        delta: interpolate(
            &[(360.0, -2.0), (480.0, -1.5), (576.0, -1.0), (720.0, -0.5), (1080.0, 0.0), (1440.0, 0.5), (2160.0, 1.0)],
            height,
        ),
    });
    // A starved source has already lost its fine detail; a low CRF only spends bits reproducing the
    // previous encoder's artefacts. A near-lossless source has detail a high CRF would throw away.
    // Low bits per pixel only means starved when there was detail to spend them on: a flat or
    // slow-moving source reaches the same figure by being easy, and gets no raise for it.
    if let Some(bitrate) = source.video_bitrate.filter(|rate| *rate > 0.0) {
        let pixels_per_second = source.width as f64 * source.height as f64 * source.fps.max(1.0);
        let bpp = bitrate / pixels_per_second * codec_efficiency(source.codec.as_deref());
        factors.push(Contribution {
            factor: "source-bits-per-pixel",
            value: bpp,
            delta: 1.0 * (1.0 - ramp(bpp, 0.05, 0.12)) * ramp(detail, 15.0, 30.0)
                - 0.5 * ramp(bpp, 0.25, 0.6),
        });
    }
    factors
}

// Factors measured in one window.
fn window_contributions(window: &WindowFeatures, banding_gain: f64) -> Vec<Contribution> {
    let spatial = &window.spatial;
    let mut factors = Vec::with_capacity(7);
    // Grain is the first thing a high CRF destroys: x264's deadzone and psy decisions turn it into
    // smeared blotches. Clean sources get a small bonus, since nothing there is fragile.
    factors.push(Contribution {
        factor: "grain",
        value: spatial.noise_sigma,
        delta: -2.5 * ramp(spatial.noise_sigma, 1.0, 4.0) + 0.5 * (1.0 - ramp(spatial.noise_sigma, 0.4, 1.0)),
    });
    // Smooth gradients band at 8 bits, and banding is the artefact a high CRF reaches first on
    // otherwise easy content. Dark gradients count twice: that is where it is most visible.
    factors.push(Contribution {
        factor: "banding",
        value: spatial.gradient_share,
        delta: BANDING_DELTA * ramp(spatial.gradient_share, BANDING_FROM, BANDING_FULL) * banding_gain,
    });
    factors.push(Contribution {
        factor: "dark-banding",
        value: spatial.dark_gradient_share,
        delta: DARK_BANDING_DELTA * ramp(spatial.dark_gradient_share, DARK_BANDING_FROM, DARK_BANDING_FULL) * banding_gain,
    });
    // Hard outlines over flat fills ring at high CRF, and the flat fill around them shows it.
    factors.push(Contribution {
        factor: "line-art",
        value: spatial.line_art_share,
        delta: -1.0 * ramp(spatial.line_art_share, 0.05, 0.30),
    });
    // Busy texture masks quantisation noise — unless the frame is also full of gradients, which do
    // not.
    factors.push(Contribution {
        factor: "texture-masking",
        value: spatial.si,
        delta: 1.0 * ramp(spatial.si, 30.0, 80.0) * (1.0 - ramp(spatial.gradient_share, BANDING_FROM, BANDING_FULL)),
    });
    // Unpredictable motion masks artefacts, and x264's qcomp already spends less on such frames, so
    // the raise is modest. A near-static window is scrutinised and gets a small cut.
    if let Some(temporal) = &window.temporal {
        factors.push(Contribution {
            factor: "motion-masking",
            value: temporal.mc_residual,
            delta: 1.5 * ramp(temporal.mc_residual, 2.0, 10.0)
                + STILL_DELTA * (1.0 - ramp(temporal.mc_residual, 0.5, 2.0)),
        });
    }
    // Saturated chroma detail suffers first under 4:2:0 and a high chroma QP offset.
    if let Some(chroma_si) = window.chroma_si {
        factors.push(Contribution {
            factor: "chroma-detail",
            value: chroma_si,
            delta: -0.5 * ramp(chroma_si, 15.0, 40.0),
        });
    }
    factors
}

fn round_crf(value: f64, encode: &EncodeContext) -> f32 {
    let rounded = (value / CRF_STEP).round() * CRF_STEP;
    (rounded as f32).clamp(encode.crf_floor, encode.crf_ceiling)
}

// One scene's CRF from the window measured at its start.
pub fn estimate_scene(window: &WindowFeatures, source: &SourceContext, encode: &EncodeContext) -> SceneEstimate {
    let anchor = encode.anchor_crf as f64;
    if window.frames == 0 {
        return SceneEstimate {
            crf: round_crf(anchor, encode),
            anchor_crf: encode.anchor_crf,
            adjustment: 0.0,
            raw_adjustment: 0.0,
            confidence: 0.0,
            frames_analyzed: 0,
            contributions: Vec::new(),
        };
    }
    let banding_gain = if source.bit_depth > 8 && encode.output_bit_depth <= 8 { 1.25 } else { 1.0 };
    let mut contributions = window_contributions(window, banding_gain);
    contributions.extend(global_contributions(source, encode, window.spatial.si));
    let raw_adjustment: f64 = contributions.iter().map(|factor| factor.delta).sum();
    let confidence = (window.frames as f64 / FULL_CONFIDENCE_FRAMES).min(1.0)
        * if window.temporal.is_some() { 1.0 } else { NO_MOTION_CONFIDENCE };
    let scaled = if raw_adjustment > 0.0 { raw_adjustment * confidence } else { raw_adjustment };
    let adjustment = scaled.clamp(-MAX_LOWER, MAX_RAISE);
    SceneEstimate {
        crf: round_crf(anchor + adjustment, encode),
        anchor_crf: encode.anchor_crf,
        adjustment,
        raw_adjustment,
        confidence,
        frames_analyzed: window.frames,
        contributions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::SpatialFeatures;
    use crate::window::TemporalSummary;

    fn source() -> SourceContext {
        SourceContext {
            width: 1920,
            height: 1080,
            fps: 24000.0 / 1001.0,
            // ~0.16 bpp: a typical 1080p web source, neutral on the bitrate factor.
            video_bitrate: Some(8_000_000.0),
            codec: Some("h264".to_string()),
            bit_depth: 8,
        }
    }

    fn window(spatial: SpatialFeatures, mc_residual: f64) -> WindowFeatures {
        WindowFeatures {
            position_seconds: 0.0,
            frames: 6,
            cuts: 0,
            spatial,
            chroma_si: Some(8.0),
            temporal: Some(TemporalSummary { mc_residual, ..Default::default() }),
        }
    }

    fn clean_anime() -> SpatialFeatures {
        SpatialFeatures { si: 35.0, noise_sigma: 0.3, gradient_share: 0.04, line_art_share: 0.08, ..Default::default() }
    }

    fn grainy_film() -> SpatialFeatures {
        SpatialFeatures { si: 45.0, noise_sigma: 4.5, gradient_share: 0.02, ..Default::default() }
    }

    #[test]
    fn an_unmeasured_scene_keeps_the_anchor() {
        let estimate = estimate_scene(&WindowFeatures::default(), &source(), &EncodeContext::anchored(17.0));
        assert_eq!(estimate.crf, 17.0);
        assert_eq!(estimate.confidence, 0.0);
    }

    #[test]
    fn grain_lowers_the_crf_and_clean_moving_content_raises_it() {
        let encode = EncodeContext::anchored(17.0);
        let grainy = estimate_scene(&window(grainy_film(), 3.0), &source(), &encode);
        let clean = estimate_scene(&window(clean_anime(), 6.0), &source(), &encode);
        assert!(grainy.crf < 17.0, "{grainy:?}");
        assert!(clean.crf > 17.0, "{clean:?}");
    }

    #[test]
    fn a_short_scene_has_its_raise_shrunk_but_not_its_cut() {
        let encode = EncodeContext::anchored(17.0);
        let short = |spatial, mc| WindowFeatures { frames: 2, ..window(spatial, mc) };
        assert!(
            estimate_scene(&short(clean_anime(), 6.0), &source(), &encode).adjustment
                < estimate_scene(&window(clean_anime(), 6.0), &source(), &encode).adjustment
        );
        assert_eq!(
            estimate_scene(&short(grainy_film(), 3.0), &source(), &encode).adjustment,
            estimate_scene(&window(grainy_film(), 3.0), &source(), &encode).adjustment
        );
        // A single frame has no motion to measure, which halves what a raise is trusted with.
        let still = WindowFeatures { temporal: None, ..window(clean_anime(), 6.0) };
        assert_eq!(estimate_scene(&still, &source(), &encode).confidence, NO_MOTION_CONFIDENCE);
    }

    // The anchor is the right CRF for typical content, so typical content must land on it. These are
    // the median features of the windows measured on two real 1080p TV anime releases: most of every
    // frame is smooth cel shading, and the shot is nearly still.
    #[test]
    fn a_typical_anime_scene_stays_at_the_anchor() {
        let typical = SpatialFeatures {
            si: 26.0,
            noise_sigma: 0.75,
            gradient_share: 0.57,
            dark_gradient_share: 0.35,
            line_art_share: 0.07,
            ..Default::default()
        };
        let estimate = estimate_scene(&window(typical, 0.3), &source(), &EncodeContext::anchored(21.0));
        assert!((estimate.crf - 21.0).abs() <= 1.0, "{estimate:?}");
        // A frame that is all dark gradient is past what the anchor covers, and is cut.
        let fade = SpatialFeatures { gradient_share: 0.87, dark_gradient_share: 0.86, ..typical };
        assert!(estimate_scene(&window(fade, 0.3), &source(), &EncodeContext::anchored(21.0)).crf <= 19.0);
    }

    #[test]
    fn the_estimate_stays_inside_its_bounds() {
        let worst = SpatialFeatures {
            noise_sigma: 9.0,
            gradient_share: 0.9,
            dark_gradient_share: 0.9,
            line_art_share: 0.9,
            ..Default::default()
        };
        let encode = EncodeContext { output_height: Some(360), ..EncodeContext::anchored(17.0) };
        assert_eq!(estimate_scene(&window(worst, 0.0), &source(), &encode).crf, 13.0);
    }

    #[test]
    fn a_low_bitrate_is_not_starved_when_the_content_is_simply_easy() {
        let encode = EncodeContext::anchored(17.0);
        let flat = SpatialFeatures { si: 2.0, ..clean_anime() };
        let tiny = SourceContext { video_bitrate: Some(500_000.0), ..source() };
        let estimate = estimate_scene(&window(flat, 3.0), &tiny, &encode);
        let bitrate = estimate.contributions.iter().find(|factor| factor.factor == "source-bits-per-pixel").unwrap();
        assert_eq!(bitrate.delta, 0.0);
    }

    #[test]
    fn a_starved_source_tolerates_a_higher_crf_than_a_remux() {
        let encode = EncodeContext::anchored(17.0);
        let scene = window(clean_anime(), 3.0);
        let starved = SourceContext { video_bitrate: Some(2_000_000.0), ..source() };
        let remux = SourceContext { video_bitrate: Some(35_000_000.0), ..source() };
        assert!(estimate_scene(&scene, &starved, &encode).crf > estimate_scene(&scene, &remux, &encode).crf);
    }
}
