// What a scene's measurements look like to a model: one fixed-length vector per scene.
//
// Every figure the window analysis produces is in it, not only the eight the fitted linear
// predictors read. Levels are on a log scale — what matters is whether there is twice as much grain,
// not 0.1 more — and shares are left as they are. The order is part of every stored model: a new
// figure goes at the end, and a model that was trained on fewer reads the first ones.
//
// The last three are the scene's fast encode (`fast`). A model reading them is trained only on
// scenes that have one and decides only scenes that have one; one that reads `BASE_INPUTS` never
// sees them.

use crate::spatial::DETAIL_FLOORS;
use crate::temporal::RESIDUAL_FLOORS;
use crate::window::WindowFeatures;

pub const INPUTS: usize = 30;

// The inputs measured on the source alone, before the fast encode's.
pub const BASE_INPUTS: usize = 27;

pub const INPUT_NAMES: [&str; INPUTS] = [
    "noise",
    "si",
    "edges",
    "smooth",
    "gradient",
    "dark-gradient",
    "line-art",
    "dark",
    "luma",
    "detail-0.5",
    "detail-1",
    "detail-2",
    "detail-4",
    "detail-8",
    "chroma-si",
    "ti",
    "zero-residual",
    "mc-residual",
    "motion",
    "changed",
    "residual-0.5",
    "residual-1",
    "residual-2",
    "residual-4",
    "residual-8",
    "still",
    "deep",
    "fast-xpsnr",
    "fast-xpsnr-low",
    "fast-bits",
];

// Shares of a block's energy sit between 0 and the square of the floor they were clipped at; the
// logarithm is of that share, with a small offset so an empty frame stays finite.
pub(crate) fn log_share(energy: f64, floor: f64) -> f64 {
    (energy.max(0.0) / (floor * floor) + 0.003).ln()
}

pub(crate) fn log_level(value: f64) -> f64 {
    (value.max(0.0) + 0.25).ln()
}

// The vector for one window. `deep` says the source is deeper than 8 bits, so the frames that were
// measured carry the dither of the conversion: the same picture reads noisier and finer, and a
// model is told why. A window with no frame pair is a still, and says so in its own input rather
// than by reading as a scene with no motion.
pub fn inputs(window: &WindowFeatures, deep: bool) -> [f64; INPUTS] {
    let spatial = &window.spatial;
    let temporal = window.temporal.unwrap_or_default();
    let mut out = [0.0; INPUTS];
    out[0] = log_level(spatial.noise_sigma);
    out[1] = log_level(spatial.si);
    out[2] = (spatial.edge_density.max(0.0) + 0.003).ln();
    out[3] = spatial.smooth_share;
    out[4] = spatial.gradient_share;
    out[5] = spatial.dark_gradient_share;
    out[6] = spatial.line_art_share;
    out[7] = spatial.dark_share;
    out[8] = spatial.mean_luma / 255.0;
    for floor in 0..5 {
        out[9 + floor] = log_share(spatial.detail_energy[floor], DETAIL_FLOORS[floor]);
        out[20 + floor] = log_share(temporal.residual_energy[floor], RESIDUAL_FLOORS[floor]);
    }
    out[14] = log_level(window.chroma_si.unwrap_or(0.0));
    out[15] = log_level(temporal.ti);
    out[16] = log_level(temporal.zero_residual);
    out[17] = log_level(temporal.mc_residual);
    out[18] = log_level(temporal.motion_px);
    out[19] = temporal.changed_share;
    out[25] = if window.temporal.is_some() { 0.0 } else { 1.0 };
    out[26] = if deep { 1.0 } else { 0.0 };
    if let Some(fast) = &window.fast_encode {
        out[27] = fast.xpsnr;
        out[28] = fast.xpsnr_low;
        out[29] = fast.log_bpp;
    }
    for value in &mut out {
        if !value.is_finite() {
            *value = 0.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::SpatialFeatures;
    use crate::window::TemporalSummary;

    #[test]
    fn every_input_is_finite_and_named() {
        let empty = inputs(&WindowFeatures::default(), false);
        assert!(empty.iter().all(|value| value.is_finite()));
        // No frame pair: a still, flagged as one.
        assert_eq!((empty[25], empty[26]), (1.0, 0.0));
        let wild = WindowFeatures {
            spatial: SpatialFeatures { noise_sigma: f64::NAN, si: f64::INFINITY, ..Default::default() },
            temporal: Some(TemporalSummary { ti: -4.0, ..Default::default() }),
            ..Default::default()
        };
        let read = inputs(&wild, true);
        assert!(read.iter().all(|value| value.is_finite()), "{read:?}");
        assert_eq!((read[25], read[26]), (0.0, 1.0));
        assert_eq!(INPUT_NAMES.len(), INPUTS);
    }

    #[test]
    fn twice_the_grain_is_one_step_wherever_it_starts() {
        let noisy = |sigma: f64| {
            inputs(&WindowFeatures { spatial: SpatialFeatures { noise_sigma: sigma, ..Default::default() }, ..Default::default() }, false)[0]
        };
        let (low, high) = (noisy(4.0) - noisy(2.0), noisy(8.0) - noisy(4.0));
        assert!((low - high).abs() < 0.07, "{low} {high}");
    }
}
