// XPSNR of 8-bit 4:2:0 frames, computed as ffmpeg's `xpsnr` filter computes it, so a quality
// measured here reads on the same scale as the lab's measurements and the model's curves.
//
// It is a port of libavfilter/vf_xpsnr.c (Helmrich, Lehmann, Stoffers; Fraunhofer HHI) for one
// bit depth: each 64-pixel block (at 1080p) has its squared error weighted by the inverse of its
// activity — a high-pass of the picture plus its change since the previous frame — so error in
// flat, still areas counts for more than error in busy ones. The arithmetic follows the filter's
// line for line, including where it is odd: a narrow block on a picture larger than 2048x1152
// computes its downsampled activity and drops it, and a block too small to filter keeps no
// temporal memory. Matching the filter matters more here than tidying it.
//
// The filter's first input is the picture whose activity weights the error, and Pandora's
// measurements give it the encode (`[encode][source]xpsnr`); `Xpsnr::frame` takes them in that
// order. The temporal memory starts at zero, so the first frame of a run reads a large activity
// and is left out of the lab's scene values — leave it out here too.

use crate::frame::Frame;

const GAMMA: u64 = 2;
const DEPTH: u32 = 8;
const MAX_ERROR: f64 = 255.0 * 255.0;
// The value a frame with no error at all reads as, as the lab caps it.
pub const CEILING_DB: f64 = 100.0;

pub struct Xpsnr {
    width: usize,
    height: usize,
    // Whole frames per second, rounded down as the filter rounds them: under 32 the temporal
    // activity is a first-order difference, from 32 on a second-order one.
    frame_rate: u32,
    block: usize,
    previous: Vec<i16>,
    before_previous: Vec<i16>,
    sse_luma: Vec<f64>,
    weights: Vec<f64>,
}

impl Xpsnr {
    pub fn new(width: usize, height: usize, fps_num: u32, fps_den: u32) -> Self {
        let ratio = (width * height) as f64 / (3840.0 * 2160.0);
        let block = (4 * ((32.0 * ratio.sqrt() + 0.5) as i32)).max(1) as usize;
        let blocks = width.div_ceil(block) * height.div_ceil(block);
        Self {
            width,
            height,
            frame_rate: if fps_den == 0 { 0 } else { fps_num / fps_den },
            block,
            previous: vec![0; width * height],
            before_previous: vec![0; width * height],
            sse_luma: vec![0.0; blocks],
            weights: vec![0.0; blocks],
        }
    }

    // The frame's XPSNR in dB, its three planes' errors pooled 4:1:1 as the lab pools them. None
    // when the frames are not both 8-bit 4:2:0 of this size.
    pub fn frame(&mut self, main: &Frame, reference: &Frame) -> Option<f64> {
        let (w, h) = (self.width, self.height);
        let planes = |frame: &Frame<'_>| -> Option<[Vec<i16>; 3]> {
            let (u, v) = (frame.u.as_ref()?, frame.v.as_ref()?);
            if frame.y.width != w || frame.y.height != h || u.width != w.div_ceil(2) || u.height != h.div_ceil(2) || v.width != u.width || v.height != u.height {
                return None;
            }
            let copy = |plane: &crate::frame::Plane<'_>| (0..plane.height).flat_map(|y| plane.row(y).iter().map(|value| *value as i16)).collect::<Vec<i16>>();
            Some([copy(&frame.y), copy(u), copy(v)])
        };
        let (main, reference) = (planes(main)?, planes(reference)?);
        let widths = [w, w.div_ceil(2), w.div_ceil(2)];
        let heights = [h, h.div_ceil(2), h.div_ceil(2)];
        let wsse = self.weighted_errors(&main, &reference, &widths, &heights);
        let decibels = |plane: usize| {
            let root = (wsse[plane] as f64).sqrt();
            if root >= 1.0 { (10.0 * ((widths[plane] * heights[plane]) as f64 * MAX_ERROR / (root * root)).log10()).min(CEILING_DB) } else { CEILING_DB }
        };
        let error = |db: f64| 10f64.powf(-db / 10.0);
        let pooled = (4.0 * error(decibels(0)) + error(decibels(1)) + error(decibels(2))) / 6.0;
        Some(-10.0 * pooled.log10())
    }

    fn weighted_errors(&mut self, main: &[Vec<i16>; 3], reference: &[Vec<i16>; 3], widths: &[usize; 3], heights: &[usize; 3]) -> [u64; 3] {
        let (w, h, b) = (self.width, self.height, self.block);
        let ratio = (w * h) as f64 / (3840.0 * 2160.0);
        let average_activity = (16.0 * (1u64 << (2 * DEPTH - 9)) as f64 / ratio.max(0.00001).sqrt()).sqrt();
        let mut wsse = [0u64; 3];
        if b < 4 {
            for plane in 0..3 {
                wsse[plane] = block_sse(&main[plane], &reference[plane], widths[plane], 0, 0, widths[plane], heights[plane]);
            }
            return wsse;
        }
        let blocks_wide = w.div_ceil(b);
        let mut index = 0;
        for y in (0..h).step_by(b) {
            let block_height = if y + b > h { h - y } else { b };
            for x in (0..w).step_by(b) {
                let block_width = if x + b > w { w - x } else { b };
                let (sse, activity) = self.error_and_activity(&main[0], &reference[0], x, y, block_width, block_height);
                self.sse_luma[index] = sse;
                self.weights[index] = 1.0 / activity.sqrt();
                if w * h <= 640 * 480 {
                    self.smooth(index, x, y, blocks_wide);
                }
                index += 1;
            }
        }
        let luma: f64 = self.sse_luma[..index].iter().zip(&self.weights[..index]).map(|(sse, weight)| sse * weight).sum();
        wsse[0] = if luma <= 0.0 { 0 } else { (luma * average_activity + 0.5) as u64 };
        for plane in 1..3 {
            let (plane_width, plane_height) = (widths[plane], heights[plane]);
            let (bx, by) = (b * plane_width / w, b * plane_height / h);
            let mut chroma = 0.0;
            let mut index = 0;
            for y in (0..plane_height).step_by(by.max(1)) {
                let block_height = if y + by > plane_height { plane_height - y } else { by };
                for x in (0..plane_width).step_by(bx.max(1)) {
                    let block_width = if x + bx > plane_width { plane_width - x } else { bx };
                    chroma += block_sse(&main[plane], &reference[plane], plane_width, x, y, block_width, block_height) as f64 * self.weights[index];
                    index += 1;
                }
            }
            wsse[plane] = if chroma <= 0.0 { 0 } else { (chroma * average_activity + 0.5) as u64 };
        }
        wsse
    }

    // A luma block's squared error and its squared activity, updating the temporal memory.
    fn error_and_activity(&mut self, main: &[i16], reference: &[i16], offset_x: usize, offset_y: usize, block_width: usize, block_height: usize) -> (f64, f64) {
        let (w, h) = (self.width, self.height);
        let o = w as isize;
        let base = (offset_y * w + offset_x) as isize;
        let at = |dx: isize, dy: isize| main[(base + dy * o + dx) as usize] as i32;
        let downsampled = w * h > 2048 * 1152;
        let edge = if downsampled { 2 } else { 1 };
        let x_act = if offset_x > 0 { 0 } else { edge };
        let y_act = if offset_y > 0 { 0 } else { edge };
        let w_act = if offset_x + block_width < w { block_width as isize } else { block_width as isize - edge };
        let h_act = if offset_y + block_height < h { block_height as isize } else { block_height as isize - edge };
        let sse = block_sse(main, reference, w, offset_x, offset_y, block_width, block_height) as f64;
        if w_act <= x_act || h_act <= y_act {
            return (sse, 1.0);
        }
        let mut spatial: u64 = 0;
        if downsampled {
            if w_act > 12 {
                for y in (y_act..h_act).step_by(2) {
                    for x in (x_act..w_act).step_by(2) {
                        let f = 12 * (at(x, y) + at(x + 1, y) + at(x, y + 1) + at(x + 1, y + 1))
                            - 3 * (at(x, y - 1) + at(x + 1, y - 1) + at(x, y + 2) + at(x + 1, y + 2))
                            - 3 * (at(x - 1, y) + at(x + 2, y) + at(x - 1, y + 1) + at(x + 2, y + 1))
                            - 2 * (at(x - 1, y - 1) + at(x + 2, y - 1) + at(x - 1, y + 2) + at(x + 2, y + 2))
                            - (at(x - 1, y - 2) + at(x, y - 2) + at(x + 1, y - 2) + at(x + 2, y - 2)
                                + at(x - 1, y + 3) + at(x, y + 3) + at(x + 1, y + 3) + at(x + 2, y + 3)
                                + at(x - 2, y - 1) + at(x - 2, y) + at(x - 2, y + 1) + at(x - 2, y + 2)
                                + at(x + 3, y - 1) + at(x + 3, y) + at(x + 3, y + 1) + at(x + 3, y + 2));
                        spatial += f.unsigned_abs() as u64;
                    }
                }
            }
        } else {
            for y in y_act..h_act {
                for x in x_act..w_act {
                    let f = 12 * at(x, y) - 2 * (at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1)) - (at(x - 1, y - 1) + at(x + 1, y - 1) + at(x - 1, y + 1) + at(x + 1, y + 1));
                    spatial += f.unsigned_abs() as u64;
                }
            }
        }
        let mut activity = spatial as f64 / ((w_act - x_act) as f64 * (h_act - y_act) as f64);
        let mut temporal: u64 = 0;
        let index = |x: usize, y: usize| (offset_y + y) * w + offset_x + x;
        let current = |x: usize, y: usize| main[index(x, y)] as i32;
        if downsampled {
            for y in (0..block_height).step_by(2) {
                for x in (0..block_width).step_by(2) {
                    let cells = [index(x, y), index(x + 1, y), index(x, y + 1), index(x + 1, y + 1)];
                    let now: i32 = cells.iter().map(|cell| main[*cell] as i32).sum();
                    let one: i32 = cells.iter().map(|cell| self.previous[*cell] as i32).sum();
                    let t = if self.frame_rate < 32 {
                        now - one
                    } else {
                        let two: i32 = cells.iter().map(|cell| self.before_previous[*cell] as i32).sum();
                        now - 2 * one + two
                    };
                    temporal += t.unsigned_abs() as u64;
                    for cell in cells {
                        if self.frame_rate >= 32 {
                            self.before_previous[cell] = self.previous[cell];
                        }
                        self.previous[cell] = main[cell];
                    }
                }
            }
            temporal *= GAMMA;
        } else {
            for y in 0..block_height {
                for x in 0..block_width {
                    let cell = index(x, y);
                    let t = if self.frame_rate < 32 {
                        current(x, y) - self.previous[cell] as i32
                    } else {
                        current(x, y) - 2 * self.previous[cell] as i32 + self.before_previous[cell] as i32
                    };
                    temporal += GAMMA * t.unsigned_abs() as u64;
                    if self.frame_rate >= 32 {
                        self.before_previous[cell] = self.previous[cell];
                    }
                    self.previous[cell] = main[cell];
                }
            }
        }
        activity += temporal as f64 / (block_width as f64 * block_height as f64);
        let floor = (1u32 << (DEPTH - 6)) as f64;
        if activity < floor {
            activity = floor;
        }
        (sse, activity * activity)
    }

    // The filter's in-line "min-smoothing" of weights, used only on pictures up to 640x480.
    fn smooth(&mut self, index: usize, x: usize, y: usize, blocks_wide: usize) {
        let (w, h, b) = (self.width, self.height, self.block);
        let weights = &mut self.weights;
        let mut previous = if x == 0 {
            if index > 1 { weights[index - 2] } else { 0.0 }
        } else if x > b {
            weights[index - 2].max(weights[index])
        } else {
            weights[index]
        };
        if index > blocks_wide {
            previous = previous.max(weights[index - 1 - blocks_wide]);
        }
        if index > 0 && weights[index - 1] > previous {
            weights[index - 1] = previous;
        }
        if x + b >= w && y + b >= h && index > blocks_wide {
            let last = weights[index - 1].max(weights[index - blocks_wide]);
            if weights[index] > last {
                weights[index] = last;
            }
        }
    }
}

fn block_sse(main: &[i16], reference: &[i16], stride: usize, x: usize, y: usize, width: usize, height: usize) -> u64 {
    let mut sse = 0u64;
    for row in y..y + height {
        let start = row * stride + x;
        for (a, b) in main[start..start + width].iter().zip(&reference[start..start + width]) {
            let d = (*a as i64 - *b as i64).unsigned_abs();
            sse += d * d;
        }
    }
    sse
}

// The scene value the lab records from frame values: their errors averaged, as dB.
pub fn pooled(frames: &[f64]) -> Option<f64> {
    if frames.is_empty() {
        return None;
    }
    let error = frames.iter().map(|db| 10f64.powf(-db / 10.0)).sum::<f64>() / frames.len() as f64;
    Some(-10.0 * error.log10())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Plane;

    fn planes(width: usize, height: usize, value: impl Fn(usize, usize) -> u8) -> [Vec<u8>; 3] {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        [(0..width * height).map(|i| value(i % width, i / width)).collect(), vec![128; cw * ch], vec![128; cw * ch]]
    }

    fn frame<'a>(data: &'a [Vec<u8>; 3], width: usize, height: usize) -> Frame<'a> {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        Frame { y: Plane::new(&data[0], width, height, width).unwrap(), u: Plane::new(&data[1], cw, ch, cw), v: Plane::new(&data[2], cw, ch, cw) }
    }

    #[test]
    fn identical_frames_read_the_ceiling_and_error_lowers_it() {
        let (w, h) = (320, 192);
        let source = planes(w, h, |x, y| ((x * 7 + y * 3) % 200) as u8);
        let noisy = planes(w, h, |x, y| ((x * 7 + y * 3) % 200) as u8 ^ ((x + y) % 4 == 0) as u8);
        let mut meter = Xpsnr::new(w, h, 24, 1);
        assert_eq!(meter.frame(&frame(&source, w, h), &frame(&source, w, h)), Some(CEILING_DB));
        let mut meter = Xpsnr::new(w, h, 24, 1);
        let db = meter.frame(&frame(&noisy, w, h), &frame(&source, w, h)).unwrap();
        assert!(db > 40.0 && db < CEILING_DB, "{db}");
        // The same error on a flat picture weighs more than on a busy one.
        let flat = planes(w, h, |_, _| 100);
        let flat_noisy = planes(w, h, |x, y| 100 ^ ((x + y) % 4 == 0) as u8);
        let mut meter = Xpsnr::new(w, h, 24, 1);
        let flat_db = meter.frame(&frame(&flat_noisy, w, h), &frame(&flat, w, h)).unwrap();
        assert!(flat_db < db, "{flat_db} {db}");
        assert_eq!(meter.frame(&frame(&source, 64, h), &frame(&source, w, h)), None);
    }

    // Compares against ffmpeg on real clips: CARAMEL_XPSNR_MAIN and CARAMEL_XPSNR_REFERENCE name
    // two raw yuv420p files of CARAMEL_XPSNR_SIZE (`1920x1080`), CARAMEL_XPSNR_STATS the
    // stats_file ffmpeg's `xpsnr` filter wrote for the same pair. Run with --ignored.
    #[test]
    #[ignore]
    fn matches_ffmpeg_on_real_frames() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let (w, h) = var("CARAMEL_XPSNR_SIZE").split_once('x').map(|(w, h)| (w.parse::<usize>().unwrap(), h.parse::<usize>().unwrap())).unwrap();
        let fps: u32 = std::env::var("CARAMEL_XPSNR_FPS").ok().and_then(|value| value.parse().ok()).unwrap_or(23);
        let (main, reference) = (std::fs::read(var("CARAMEL_XPSNR_MAIN")).unwrap(), std::fs::read(var("CARAMEL_XPSNR_REFERENCE")).unwrap());
        let expected: Vec<f64> = std::fs::read_to_string(var("CARAMEL_XPSNR_STATS"))
            .unwrap()
            .lines()
            .filter(|line| line.trim_start().starts_with("n:"))
            .map(|line| {
                let db = |plane: &str| line.split(plane).nth(1).unwrap().split_whitespace().next().unwrap().parse::<f64>().unwrap_or(CEILING_DB).min(CEILING_DB);
                let error = |db: f64| 10f64.powf(-db / 10.0);
                -10.0 * ((4.0 * error(db("XPSNR y:")) + error(db("XPSNR u:")) + error(db("XPSNR v:"))) / 6.0).log10()
            })
            .collect();
        let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        let split = |data: &[u8]| -> [Vec<u8>; 3] {
            let c = w.div_ceil(2) * h.div_ceil(2);
            [data[..w * h].to_vec(), data[w * h..w * h + c].to_vec(), data[w * h + c..w * h + 2 * c].to_vec()]
        };
        let mut meter = Xpsnr::new(w, h, fps, 1);
        let mut worst: f64 = 0.0;
        for (index, expected) in expected.iter().enumerate() {
            let (a, b) = (split(&main[index * size..(index + 1) * size]), split(&reference[index * size..(index + 1) * size]));
            let ours = meter.frame(&frame(&a, w, h), &frame(&b, w, h)).unwrap();
            println!("frame {index}: ours {ours:.4} ffmpeg {expected:.4}");
            worst = worst.max((ours - expected).abs());
        }
        assert!(worst < 0.01, "worst difference {worst}");
    }
}
