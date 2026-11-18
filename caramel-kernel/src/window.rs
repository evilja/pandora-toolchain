// A window: a short run of consecutive frames, measured together. A scene's CRF is decided from the
// window at its start (see `scene`), and every temporal feature is measured only between frames of
// the same window — the pair that straddles a cut is a new keyframe, not motion.

use crate::frame::Frame;
use crate::spatial::{self, SpatialFeatures};
use crate::temporal::{self, LowRes, TemporalFeatures};
use serde::{Deserialize, Serialize};

// Means of `TemporalFeatures` over a window's pairs.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TemporalSummary {
    pub ti: f64,
    pub zero_residual: f64,
    pub mc_residual: f64,
    pub motion_px: f64,
    pub changed_share: f64,
    pub residual_energy: [f64; 5],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WindowFeatures {
    // Where the window starts in the source, in seconds; a label for reports and per-scene output.
    pub position_seconds: f64,
    pub frames: usize,
    // How many of them were measured by themselves; the rest only against their neighbours.
    pub measured: usize,
    // Cuts inside the window: ones that came too soon after the last to start a scene of their own.
    pub cuts: usize,
    // Means over the window's measured frames.
    pub spatial: SpatialFeatures,
    pub chroma_si: Option<f64>,
    // Means over the window's frame pairs, excluding the pairs that straddle a cut: a cut is a new
    // keyframe, not motion. None when no pair survived.
    pub temporal: Option<TemporalSummary>,
    // A fast encode of the whole scene (`fast`), for a model that reads one. The window analysis
    // cannot make it; whoever encodes adds it once the scene's frames are all there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_encode: Option<crate::fast::FastEncode>,
    // The banding the scene's source is ready to show (`fast::added_banding`), read from the
    // frames its fast encode would cover. Like the fast encode, the window analysis cannot read it:
    // whoever holds the scene's frames adds it. None when they were not 8-bit, or for a scene
    // measured before it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposed_banding: Option<f64>,
}

// What is measured on one frame by itself.
#[derive(Clone, Copy, Debug)]
pub struct FrameMeasure {
    pub spatial: SpatialFeatures,
    pub chroma_si: Option<f64>,
}

// The expensive step of the analysis: every pixel of the frame is read, where the comparison between
// frames reads a sixteenth of them. A caller measures only the frames a window will hold.
pub fn measure(frame: &Frame) -> FrameMeasure {
    FrameMeasure {
        spatial: spatial::analyze_luma(&frame.y),
        chroma_si: match (frame.u, frame.v) {
            (Some(u), Some(v)) => Some(0.5 * (spatial::sobel_si(&u) + spatial::sobel_si(&v))),
            _ => None,
        },
    }
}

pub struct WindowAnalyzer {
    position_seconds: f64,
    frames: usize,
    spatial: Vec<SpatialFeatures>,
    chroma: Vec<f64>,
    temporal: Vec<TemporalFeatures>,
    cuts: usize,
}

impl WindowAnalyzer {
    pub fn new(position_seconds: f64) -> Self {
        Self { position_seconds, frames: 0, spatial: Vec::new(), chroma: Vec::new(), temporal: Vec::new(), cuts: 0 }
    }

    // The next frame of the window. `measure` is there for the frames the caller chose to measure —
    // a few, spread over the window, say as much as all of them. `pair` is the frame's comparison
    // with the one before it — None for the window's first frame — and `cut` says that pair
    // straddles a cut.
    pub fn push(&mut self, measure: Option<FrameMeasure>, pair: Option<&TemporalFeatures>, cut: bool) {
        self.frames += 1;
        if let Some(measure) = measure {
            self.spatial.push(measure.spatial);
            self.chroma.extend(measure.chroma_si);
        }
        match pair {
            Some(_) if cut => self.cuts += 1,
            Some(pair) => self.temporal.push(*pair),
            None => {}
        }
    }

    pub fn finish(self) -> WindowFeatures {
        let continuous = &self.temporal;
        let temporal = (!continuous.is_empty()).then(|| {
            let n = continuous.len() as f64;
            TemporalSummary {
                ti: continuous.iter().map(|pair| pair.ti).sum::<f64>() / n,
                zero_residual: continuous.iter().map(|pair| pair.zero_residual).sum::<f64>() / n,
                mc_residual: continuous.iter().map(|pair| pair.mc_residual).sum::<f64>() / n,
                motion_px: continuous.iter().map(|pair| pair.motion_px).sum::<f64>() / n,
                changed_share: continuous.iter().map(|pair| pair.changed_share).sum::<f64>() / n,
                residual_energy: [0, 1, 2, 3, 4]
                    .map(|floor| continuous.iter().map(|pair| pair.residual_energy[floor]).sum::<f64>() / n),
            }
        });
        WindowFeatures {
            position_seconds: self.position_seconds,
            frames: self.frames,
            measured: self.spatial.len(),
            cuts: self.cuts,
            spatial: mean_spatial(&self.spatial),
            chroma_si: (!self.chroma.is_empty())
                .then(|| self.chroma.iter().sum::<f64>() / self.chroma.len() as f64),
            temporal,
            fast_encode: None,
            exposed_banding: None,
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
        detail_energy: [0, 1, 2, 3, 4].map(|floor| frames.iter().map(|f| f.detail_energy[floor]).sum::<f64>() / n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Plane;
    use crate::spatial::tests::picture;
    use crate::temporal;

    #[test]
    fn a_cut_inside_a_window_is_counted_and_kept_out_of_the_motion_figures() {
        let still = picture(320, 192, |x, y| 128.0 + 40.0 * ((x as f64 * 0.2).sin() * (y as f64 * 0.3).cos()));
        let other = picture(320, 192, |x, y| if (x / 40 + y / 40) % 2 == 0 { 20.0 } else { 235.0 });
        let mut window = WindowAnalyzer::new(12.5);
        let mut previous: Option<temporal::LowRes> = None;
        for (index, data) in [&still, &still, &other, &other].into_iter().enumerate() {
            let frame = Frame::luma(Plane::new(data, 320, 192, 320).unwrap());
            let lowres = temporal::downsample(&frame.y);
            let pair = previous.as_ref().map(|previous| temporal::compare(previous, &lowres));
            window.push((index != 1).then(|| measure(&frame)), pair.as_ref(), index == 2);
            previous = Some(lowres);
        }
        let features = window.finish();
        assert_eq!((features.frames, features.measured), (4, 3));
        assert_eq!(features.cuts, 1);
        let temporal = features.temporal.unwrap();
        assert_eq!(temporal.mc_residual, 0.0);
        assert_eq!(features.position_seconds, 12.5);
    }
}

// A whole scene measured the way its opening window is — every pair of frames compared, every
// `stride`-th frame measured by itself — rather than only its first `analysis_frames`. The lab
// keeps both, to learn whether a CRF decided from all of a scene is better than one decided from
// its start: half a second of a shot does not show the motion that begins a second into it. The
// caller hands frames of one scene, so no cut is looked for.
pub struct SceneWindow {
    analyzer: WindowAnalyzer,
    previous: Option<LowRes>,
    index: usize,
    stride: usize,
}

impl SceneWindow {
    pub fn new(position_seconds: f64, stride: usize) -> Self {
        Self { analyzer: WindowAnalyzer::new(position_seconds), previous: None, index: 0, stride: stride.max(1) }
    }

    pub fn push(&mut self, frame: &Frame) {
        let lowres = temporal::downsample(&frame.y);
        let pair = self.previous.as_ref().map(|previous| temporal::compare(previous, &lowres));
        let measured = self.index.is_multiple_of(self.stride).then(|| measure(frame));
        self.analyzer.push(measured, pair.as_ref(), false);
        self.previous = Some(lowres);
        self.index += 1;
    }

    pub fn finish(self) -> WindowFeatures {
        self.analyzer.finish()
    }
}
