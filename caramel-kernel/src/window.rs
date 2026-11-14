// A window: a short run of consecutive frames, measured together. A scene's CRF is decided from the
// window at its start (see `scene`), and every temporal feature is measured only between frames of
// the same window — the pair that straddles a cut is a new keyframe, not motion.

use crate::frame::{Frame, Plane};
use crate::spatial::{self, SpatialFeatures};
use crate::temporal::{self, LowRes, TemporalFeatures};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TemporalSummary {
    pub ti: f64,
    pub zero_residual: f64,
    pub mc_residual: f64,
    pub motion_px: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct WindowFeatures {
    // Where the window starts in the source, in seconds; a label for reports and per-scene output.
    pub position_seconds: f64,
    pub frames: usize,
    pub cuts: usize,
    // Means over the window's frames.
    pub spatial: SpatialFeatures,
    pub chroma_si: Option<f64>,
    // Means over the window's frame pairs, excluding the pairs that straddle a cut: a cut is a new
    // keyframe, not motion. None when no pair survived.
    pub temporal: Option<TemporalSummary>,
}

struct Previous {
    luma: Vec<u8>,
    width: usize,
    height: usize,
    lowres: LowRes,
}

pub struct WindowAnalyzer {
    position_seconds: f64,
    spatial: Vec<SpatialFeatures>,
    chroma: Vec<f64>,
    temporal: Vec<TemporalFeatures>,
    previous: Option<Previous>,
}

impl WindowAnalyzer {
    pub fn new(position_seconds: f64) -> Self {
        Self {
            position_seconds,
            spatial: Vec::new(),
            chroma: Vec::new(),
            temporal: Vec::new(),
            previous: None,
        }
    }

    // The spatial half of `push`, with the pair against the previous frame measured by the caller —
    // the scene decider compares every frame for cuts anyway, and measuring it twice would double the
    // most expensive step. `None` for the window's first frame.
    pub fn push_measured(&mut self, frame: &Frame, temporal: Option<&TemporalFeatures>) {
        self.spatial.push(spatial::analyze_luma(&frame.y));
        if let (Some(u), Some(v)) = (frame.u, frame.v) {
            self.chroma.push(0.5 * (spatial::sobel_si(&u) + spatial::sobel_si(&v)));
        }
        if let Some(pair) = temporal {
            self.temporal.push(*pair);
        }
    }

    pub fn push(&mut self, frame: &Frame) {
        self.spatial.push(spatial::analyze_luma(&frame.y));
        if let (Some(u), Some(v)) = (frame.u, frame.v) {
            self.chroma.push(0.5 * (spatial::sobel_si(&u) + spatial::sobel_si(&v)));
        }
        let lowres = temporal::downsample(&frame.y);
        if let Some(previous) = &self.previous {
            if let Some(plane) = Plane::new(&previous.luma, previous.width, previous.height, previous.width) {
                self.temporal.push(temporal::compare(&plane, &frame.y, &previous.lowres, &lowres));
            }
        }
        // The previous frame is kept as a packed copy: the caller's buffer is only borrowed until
        // it decodes the next frame into it.
        let mut luma = Vec::with_capacity(frame.y.width * frame.y.height);
        for y in 0..frame.y.height {
            luma.extend_from_slice(frame.y.row(y));
        }
        self.previous = Some(Previous { luma, width: frame.y.width, height: frame.y.height, lowres });
    }

    pub fn finish(self) -> WindowFeatures {
        let frames = self.spatial.len();
        let cuts = self.temporal.iter().filter(|pair| pair.cut).count();
        let continuous: Vec<&TemporalFeatures> = self.temporal.iter().filter(|pair| !pair.cut).collect();
        let temporal = (!continuous.is_empty()).then(|| {
            let n = continuous.len() as f64;
            TemporalSummary {
                ti: continuous.iter().map(|pair| pair.ti).sum::<f64>() / n,
                zero_residual: continuous.iter().map(|pair| pair.zero_residual).sum::<f64>() / n,
                mc_residual: continuous.iter().map(|pair| pair.mc_residual).sum::<f64>() / n,
                motion_px: continuous.iter().map(|pair| pair.motion_px).sum::<f64>() / n,
            }
        });
        WindowFeatures {
            position_seconds: self.position_seconds,
            frames,
            cuts,
            spatial: mean_spatial(&self.spatial),
            chroma_si: (!self.chroma.is_empty())
                .then(|| self.chroma.iter().sum::<f64>() / self.chroma.len() as f64),
            temporal,
        }
    }
}

fn mean_spatial(frames: &[SpatialFeatures]) -> SpatialFeatures {
    if frames.is_empty() {
        return SpatialFeatures::default();
    }
    let n = frames.len() as f64;
    let mean = |field: fn(&SpatialFeatures) -> f64| frames.iter().map(field).sum::<f64>() / n;
    SpatialFeatures {
        si: mean(|f| f.si),
        edge_density: mean(|f| f.edge_density),
        noise_sigma: mean(|f| f.noise_sigma),
        smooth_share: mean(|f| f.smooth_share),
        gradient_share: mean(|f| f.gradient_share),
        dark_gradient_share: mean(|f| f.dark_gradient_share),
        line_art_share: mean(|f| f.line_art_share),
        dark_share: mean(|f| f.dark_share),
        mean_luma: mean(|f| f.mean_luma),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::tests::picture;

    #[test]
    fn a_cut_inside_a_window_is_counted_and_kept_out_of_the_motion_figures() {
        let still = picture(320, 192, |x, y| 128.0 + 40.0 * ((x as f64 * 0.2).sin() * (y as f64 * 0.3).cos()));
        let other = picture(320, 192, |x, y| if (x / 40 + y / 40) % 2 == 0 { 20.0 } else { 235.0 });
        let mut window = WindowAnalyzer::new(12.5);
        for data in [&still, &still, &other, &other] {
            window.push(&Frame::luma(Plane::new(data, 320, 192, 320).unwrap()));
        }
        let features = window.finish();
        assert_eq!(features.frames, 4);
        assert_eq!(features.cuts, 1);
        let temporal = features.temporal.unwrap();
        assert_eq!(temporal.mc_residual, 0.0);
        assert_eq!(features.position_seconds, 12.5);
    }
}
