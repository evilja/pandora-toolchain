// The CRF model: from window features to a CRF.
//
// The estimate is an *offset from an anchor*. The anchor is the CRF the encoder configuration was
// chosen at for typical content (a preset's own CRF), and every factor below moves it by a bounded
// number of CRF steps: up where the content masks artefacts or cannot benefit from more bits, down
// where it is fragile. Anchoring keeps the model honest about what it does not know — the absolute
// CRF-to-quality curve of a particular encoder, preset and AQ configuration — and lets one table of
// factors serve x264, x265 and SVT-AV1 presets alike.
//
// The factor table is a general prior drawn from common encoding practice, not a fit to measured
// data. Every threshold and weight is a named constant here so a calibration pass against
// brute-force CRF searches can replace them without touching the feature code.

use crate::window::WindowFeatures;
use serde::Serialize;

pub const MODEL_VERSION: &str = "caramel-prior-1";

// The furthest the estimate may move from the anchor in either direction.
const MAX_RAISE: f64 = 4.0;
const MAX_LOWER: f64 = 4.0;
// The file's CRF has to hold in its hard scenes, so it is read off a low quantile of the windows
// rather than their mean: a quality target is met on the frames that are hardest to encode.
const WINDOW_QUANTILE: f64 = 0.25;
// Windows at which a raise is trusted in full. Fewer windows scale a raise down (never a cut),
// since an unsampled hard scene is the failure that costs visible quality.
const FULL_CONFIDENCE_WINDOWS: f64 = 8.0;
// Granularity of the reported CRF.
const CRF_STEP: f64 = 0.5;

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

#[derive(Clone, Copy, Debug, Serialize)]
pub struct WindowEstimate {
    pub position_seconds: f64,
    pub frames: usize,
    pub adjustment: f64,
    pub crf: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Estimate {
    pub model: &'static str,
    pub crf: f32,
    pub anchor_crf: f32,
    // The applied offset from the anchor, before rounding.
    pub adjustment: f64,
    pub window_adjustment: f64,
    pub global_adjustment: f64,
    pub confidence: f64,
    pub windows_analyzed: usize,
    pub frames_analyzed: usize,
    pub cuts: usize,
    pub global: Vec<Contribution>,
    // Each window factor's mean delta: what drove the estimate, for the report.
    pub typical: Vec<Contribution>,
    // A CRF per window, the raw material for per-scene zones.
    pub windows: Vec<WindowEstimate>,
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
        delta: -2.0 * ramp(spatial.gradient_share, 0.05, 0.35) * banding_gain,
    });
    factors.push(Contribution {
        factor: "dark-banding",
        value: spatial.dark_gradient_share,
        delta: -1.0 * ramp(spatial.dark_gradient_share, 0.02, 0.15) * banding_gain,
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
        delta: 1.0 * ramp(spatial.si, 30.0, 80.0) * (1.0 - ramp(spatial.gradient_share, 0.05, 0.35)),
    });
    // Unpredictable motion masks artefacts, and x264's qcomp already spends less on such frames, so
    // the raise is modest. A near-static window is scrutinised and gets a small cut.
    if let Some(temporal) = &window.temporal {
        factors.push(Contribution {
            factor: "motion-masking",
            value: temporal.mc_residual,
            delta: 1.5 * ramp(temporal.mc_residual, 2.0, 10.0)
                - 0.5 * (1.0 - ramp(temporal.mc_residual, 0.5, 2.0)),
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

fn weighted_quantile(values: &mut [(f64, f64)], quantile: f64) -> f64 {
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = values.iter().map(|(_, weight)| weight).sum();
    let mut seen = 0.0;
    for &(value, weight) in values.iter() {
        seen += weight;
        if seen >= quantile * total {
            return value;
        }
    }
    values.last().map_or(0.0, |(value, _)| *value)
}

fn round_crf(value: f64, encode: &EncodeContext) -> f32 {
    let rounded = (value / CRF_STEP).round() * CRF_STEP;
    (rounded as f32).clamp(encode.crf_floor, encode.crf_ceiling)
}

pub fn estimate(windows: &[WindowFeatures], source: &SourceContext, encode: &EncodeContext) -> Estimate {
    let anchor = encode.anchor_crf as f64;
    let banding_gain = if source.bit_depth > 8 && encode.output_bit_depth <= 8 { 1.25 } else { 1.0 };
    let measured: Vec<&WindowFeatures> = windows.iter().filter(|window| window.frames > 0).collect();
    let detail = if measured.is_empty() {
        0.0
    } else {
        measured.iter().map(|window| window.spatial.si).sum::<f64>() / measured.len() as f64
    };
    let global = global_contributions(source, encode, detail);
    let global_adjustment: f64 = global.iter().map(|factor| factor.delta).sum();

    let mut per_window = Vec::with_capacity(measured.len());
    let mut typical: Vec<Contribution> = Vec::new();
    let mut weighted = Vec::with_capacity(measured.len());
    for window in &measured {
        let factors = window_contributions(window, banding_gain);
        let adjustment: f64 = factors.iter().map(|factor| factor.delta).sum();
        for factor in &factors {
            match typical.iter_mut().find(|known| known.factor == factor.factor) {
                Some(known) => {
                    known.value += factor.value;
                    known.delta += factor.delta;
                }
                None => typical.push(*factor),
            }
        }
        weighted.push((adjustment, window.frames as f64));
        per_window.push(WindowEstimate {
            position_seconds: window.position_seconds,
            frames: window.frames,
            adjustment,
            crf: round_crf(
                anchor + (adjustment + global_adjustment).clamp(-MAX_LOWER, MAX_RAISE),
                encode,
            ),
        });
    }
    // Averaged over the windows that measured each factor; motion is absent from single-frame ones.
    for factor in typical.iter_mut() {
        let count = measured
            .iter()
            .filter(|window| factor.factor != "motion-masking" || window.temporal.is_some())
            .filter(|window| factor.factor != "chroma-detail" || window.chroma_si.is_some())
            .count()
            .max(1) as f64;
        factor.value /= count;
        factor.delta /= count;
    }

    let window_adjustment = if weighted.is_empty() { 0.0 } else { weighted_quantile(&mut weighted, WINDOW_QUANTILE) };
    let temporal_coverage = if measured.is_empty() {
        0.0
    } else {
        measured.iter().filter(|window| window.temporal.is_some()).count() as f64 / measured.len() as f64
    };
    let confidence = (measured.len() as f64 / FULL_CONFIDENCE_WINDOWS).min(1.0) * (0.5 + 0.5 * temporal_coverage);
    let mut adjustment = if measured.is_empty() { 0.0 } else { window_adjustment + global_adjustment };
    if adjustment > 0.0 {
        adjustment *= confidence;
    }
    let adjustment = adjustment.clamp(-MAX_LOWER, MAX_RAISE);

    Estimate {
        model: MODEL_VERSION,
        crf: round_crf(anchor + adjustment, encode),
        anchor_crf: encode.anchor_crf,
        adjustment,
        window_adjustment,
        global_adjustment,
        confidence,
        windows_analyzed: measured.len(),
        frames_analyzed: measured.iter().map(|window| window.frames).sum(),
        cuts: measured.iter().map(|window| window.cuts).sum(),
        global,
        typical,
        windows: per_window,
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
    fn no_windows_means_the_anchor() {
        let estimate = estimate(&[], &source(), &EncodeContext::anchored(17.0));
        assert_eq!(estimate.crf, 17.0);
        assert_eq!(estimate.confidence, 0.0);
    }

    #[test]
    fn grain_lowers_the_crf_and_clean_moving_content_raises_it() {
        let encode = EncodeContext::anchored(17.0);
        let grainy: Vec<_> = (0..10).map(|_| window(grainy_film(), 3.0)).collect();
        let clean: Vec<_> = (0..10).map(|_| window(clean_anime(), 6.0)).collect();
        let grainy = estimate(&grainy, &source(), &encode);
        let clean = estimate(&clean, &source(), &encode);
        assert!(grainy.crf < 17.0, "{grainy:?}");
        assert!(clean.crf > 17.0, "{clean:?}");
    }

    #[test]
    fn hard_windows_decide_the_file_crf() {
        let encode = EncodeContext::anchored(17.0);
        let mut windows: Vec<_> = (0..7).map(|_| window(clean_anime(), 6.0)).collect();
        windows.extend((0..3).map(|_| window(grainy_film(), 3.0)));
        let mixed = estimate(&windows, &source(), &encode);
        let easy = estimate(&windows[..7], &source(), &encode);
        assert!(mixed.crf < easy.crf, "mixed {mixed:?} easy {easy:?}");
        assert_eq!(mixed.windows.len(), 10);
    }

    #[test]
    fn few_windows_shrink_a_raise_but_not_a_cut() {
        let encode = EncodeContext::anchored(17.0);
        let many: Vec<_> = (0..8).map(|_| window(clean_anime(), 6.0)).collect();
        let few = estimate(&many[..2], &source(), &encode);
        let full = estimate(&many, &source(), &encode);
        assert!(few.adjustment < full.adjustment);

        let grainy: Vec<_> = (0..8).map(|_| window(grainy_film(), 3.0)).collect();
        let few = estimate(&grainy[..2], &source(), &encode);
        let full = estimate(&grainy, &source(), &encode);
        assert_eq!(few.adjustment, full.adjustment);
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
        let windows: Vec<_> = (0..10).map(|_| window(worst, 0.0)).collect();
        let estimate = estimate(&windows, &source(), &encode);
        assert_eq!(estimate.crf, 13.0);
    }

    #[test]
    fn a_low_bitrate_is_not_starved_when_the_content_is_simply_easy() {
        let encode = EncodeContext::anchored(17.0);
        let flat = SpatialFeatures { si: 2.0, ..clean_anime() };
        let windows: Vec<_> = (0..10).map(|_| window(flat, 3.0)).collect();
        let tiny = SourceContext { video_bitrate: Some(500_000.0), ..source() };
        let estimate = estimate(&windows, &tiny, &encode);
        let bitrate = estimate.global.iter().find(|factor| factor.factor == "source-bits-per-pixel").unwrap();
        assert_eq!(bitrate.delta, 0.0);
    }

    #[test]
    fn a_starved_source_tolerates_a_higher_crf_than_a_remux() {
        let encode = EncodeContext::anchored(17.0);
        let windows: Vec<_> = (0..10).map(|_| window(clean_anime(), 3.0)).collect();
        let starved = SourceContext { video_bitrate: Some(2_000_000.0), ..source() };
        let remux = SourceContext { video_bitrate: Some(35_000_000.0), ..source() };
        assert!(
            estimate(&windows, &starved, &encode).crf > estimate(&windows, &remux, &encode).crf
        );
    }
}
