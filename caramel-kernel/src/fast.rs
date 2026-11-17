// A fast encode of a whole scene: what x264 itself makes of the scene, as an input to the model.
//
// Features measured on the source describe the picture; they do not say how x264 will treat it, and
// two scenes alike in every figure can need CRFs four apart. Most of the model's error was that:
// four fifths of it lay between scenes of one source, where no calibration per source reaches. One
// encode of the scene at `FAST_PRESET` and `FAST_CRF`, with the encode's own x264 settings
// otherwise, scored with XPSNR as the lab scores its encodes, says most of what the slow encodes
// will find: on series held out of the fit it took the error of the CRF a scene allows from about
// 2.3 to 1.0-1.3 CRF, near what a real encode at the slow preset gave (0.8-1.0). It has to cover the
// whole scene — the first twelve frames alone barely helped — and the scene at full size: at half
// size most of the gain was gone.
//
// The kernel does not encode. Whoever owns the encoder runs it and hands the frames' XPSNR and the
// bytes here (`FastEncode::from_frames`), so the lab and every encoder read the scene alike.

use serde::{Deserialize, Serialize};

pub const FAST_PRESET: &str = "veryfast";
pub const FAST_CRF: f32 = 20.0;
// The most frames of a scene the fast encode covers: a scene's longest stretch before it is split.
pub const FAST_FRAMES: usize = 48;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FastEncode {
    // The frames' XPSNR pooled as the lab pools a clip's, without the opening keyframe.
    pub xpsnr: f64,
    // The mean of the worst tenth of those frames.
    pub xpsnr_low: f64,
    // log2 of the bits per pixel the encode spent, every frame counted.
    pub log_bpp: f64,
    pub frames: usize,
}

impl FastEncode {
    // `xpsnr` per frame in display order, the first being the encode's keyframe; `bytes` the coded
    // size of all of them; `pixels` a frame's luma samples.
    pub fn from_frames(xpsnr: &[f64], bytes: u64, pixels: usize) -> Option<Self> {
        if xpsnr.len() < 2 || pixels == 0 || bytes == 0 {
            return None;
        }
        let settled = &xpsnr[1..];
        let mut sorted = settled.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted.truncate((settled.len() / 10).max(1));
        Some(Self {
            xpsnr: crate::xpsnr::pooled(settled)?,
            xpsnr_low: sorted.iter().sum::<f64>() / sorted.len() as f64,
            log_bpp: (bytes as f64 * 8.0 / (xpsnr.len() * pixels) as f64).log2(),
            frames: xpsnr.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fast_encode_is_read_as_the_lab_reads_a_clip() {
        // The keyframe is left out of the quality, not out of the bits.
        let frames = [60.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0, 30.0];
        let fast = FastEncode::from_frames(&frames, 1000, 100).unwrap();
        assert!((fast.xpsnr - crate::xpsnr::pooled(&frames[1..]).unwrap()).abs() < 1e-12);
        assert_eq!(fast.xpsnr_low, 30.0);
        assert!((fast.log_bpp - (8000.0f64 / 1100.0).log2()).abs() < 1e-12);
        assert_eq!(fast.frames, 11);
        assert!(FastEncode::from_frames(&[40.0], 10, 10).is_none());
    }
}
