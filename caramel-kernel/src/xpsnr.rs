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

use crate::frame::{Frame, Plane};

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
    previous: Vec<u8>,
    before_previous: Vec<u8>,
    sse_luma: Vec<f64>,
    weights: Vec<f64>,
    // A block row's vertical [1 2 1] sums, kept between blocks so none allocates.
    vertical: Vec<u16>,
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
            vertical: Vec::new(),
        }
    }

    // The frame's XPSNR in dB, its three planes' errors pooled 4:1:1 as the lab pools them. None
    // when the frames are not both 8-bit 4:2:0 of this size.
    pub fn frame<'a>(&mut self, main: &Frame<'a>, reference: &Frame<'a>) -> Option<f64> {
        let (w, h) = (self.width, self.height);
        // The planes are read where they lie, through their strides; nothing is copied.
        let planes = |frame: &Frame<'a>| -> Option<[Plane<'a>; 3]> {
            let (u, v) = (frame.u?, frame.v?);
            if frame.y.width != w || frame.y.height != h || u.width != w.div_ceil(2) || u.height != h.div_ceil(2) || v.width != u.width || v.height != u.height {
                return None;
            }
            Some([frame.y, u, v])
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

    fn weighted_errors(&mut self, main: &[Plane; 3], reference: &[Plane; 3], widths: &[usize; 3], heights: &[usize; 3]) -> [u64; 3] {
        let (w, h, b) = (self.width, self.height, self.block);
        let ratio = (w * h) as f64 / (3840.0 * 2160.0);
        let average_activity = (16.0 * (1u64 << (2 * DEPTH - 9)) as f64 / ratio.max(0.00001).sqrt()).sqrt();
        let mut wsse = [0u64; 3];
        if b < 4 {
            for plane in 0..3 {
                wsse[plane] = block_sse(&main[plane], &reference[plane], 0, 0, widths[plane], heights[plane]);
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
                    chroma += block_sse(&main[plane], &reference[plane], x, y, block_width, block_height) as f64 * self.weights[index];
                    index += 1;
                }
            }
            wsse[plane] = if chroma <= 0.0 { 0 } else { (chroma * average_activity + 0.5) as u64 };
        }
        wsse
    }

    // A luma block's squared error and its squared activity, updating the temporal memory. The
    // high-pass reads past the block's edges into its neighbours, as the filter's does; only the
    // picture's own edges are left out. Every sum is an integer, so the row-by-row loops below add
    // up exactly what the filter's pixel-by-pixel ones do.
    fn error_and_activity(&mut self, main: &Plane, reference: &Plane, offset_x: usize, offset_y: usize, block_width: usize, block_height: usize) -> (f64, f64) {
        let (w, h) = (self.width, self.height);
        let downsampled = w * h > 2048 * 1152;
        let edge = if downsampled { 2 } else { 1 };
        let x_act = if offset_x > 0 { 0 } else { edge };
        let y_act = if offset_y > 0 { 0 } else { edge };
        let w_act = if offset_x + block_width < w { block_width as isize } else { block_width as isize - edge };
        let h_act = if offset_y + block_height < h { block_height as isize } else { block_height as isize - edge };
        let sse = block_sse(main, reference, offset_x, offset_y, block_width, block_height) as f64;
        if w_act <= x_act || h_act <= y_act {
            return (sse, 1.0);
        }
        // The pixel `dx`, `dy` from the block's corner, read through the plane's stride.
        let (stride, data) = (main.stride as isize, main.data);
        let at = |dx: isize, dy: isize| data[((offset_y as isize + dy) * stride + offset_x as isize + dx) as usize] as i32;
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
            // The filter's kernel, 12 at the centre, -2 beside it and -1 at the corners, is 16 times
            // the centre less the separable [1 2 1] x [1 2 1] blur; the same integers, in fewer
            // operations and in 16 bits (|f| <= 16 * 255). Columns `left - 1 ..= right` of the rows
            // above, at and below each filtered one are read.
            let (left, right) = (offset_x + x_act as usize, offset_x + w_act as usize);
            let n = right - left;
            let mut vertical = std::mem::take(&mut self.vertical);
            vertical.clear();
            vertical.resize(n + 2, 0);
            for y in y_act..h_act {
                let row = (offset_y as isize + y) as usize;
                let line = |r: usize| &main.data[r * main.stride + left - 1..r * main.stride + right + 1];
                let (above, current, below) = (line(row - 1), line(row), line(row + 1));
                for (((sum, a), c), b) in vertical.iter_mut().zip(above).zip(current).zip(below) {
                    *sum = *a as u16 + 2 * *c as u16 + *b as u16;
                }
                let (before, at, after) = (&vertical[..n], &vertical[1..n + 1], &vertical[2..n + 2]);
                let mut row_sum: u32 = 0;
                for (((before, at), after), centre) in before.iter().zip(at).zip(after).zip(&current[1..n + 1]) {
                    let f = 16 * *centre as i16 - (*before + 2 * *at + *after) as i16;
                    row_sum += f.unsigned_abs() as u32;
                }
                spatial += row_sum as u64;
            }
            self.vertical = vertical;
        }
        let mut activity = spatial as f64 / ((w_act - x_act) as f64 * (h_act - y_act) as f64);
        let mut temporal: u64 = 0;
        let second_order = self.frame_rate >= 32;
        if downsampled {
            let index = |x: usize, y: usize| (offset_y + y) * w + offset_x + x;
            let pixel = |x: usize, y: usize| main.data[(offset_y + y) * main.stride + offset_x + x];
            for y in (0..block_height).step_by(2) {
                for x in (0..block_width).step_by(2) {
                    let cells = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                    let now: i32 = cells.iter().map(|(x, y)| pixel(*x, *y) as i32).sum();
                    let one: i32 = cells.iter().map(|(x, y)| self.previous[index(*x, *y)] as i32).sum();
                    let t = if !second_order {
                        now - one
                    } else {
                        let two: i32 = cells.iter().map(|(x, y)| self.before_previous[index(*x, *y)] as i32).sum();
                        now - 2 * one + two
                    };
                    temporal += t.unsigned_abs() as u64;
                    for (x, y) in cells {
                        let cell = index(x, y);
                        if second_order {
                            self.before_previous[cell] = self.previous[cell];
                        }
                        self.previous[cell] = pixel(x, y);
                    }
                }
            }
            temporal *= GAMMA;
        } else {
            for y in 0..block_height {
                let memory = (offset_y + y) * w + offset_x;
                let current = &main.row(offset_y + y)[offset_x..offset_x + block_width];
                let previous = &mut self.previous[memory..memory + block_width];
                let mut row_sum: u32 = 0;
                if second_order {
                    let before = &mut self.before_previous[memory..memory + block_width];
                    for ((now, one), two) in current.iter().zip(previous.iter_mut()).zip(before.iter_mut()) {
                        row_sum += (*now as i16 - 2 * *one as i16 + *two as i16).unsigned_abs() as u32;
                        *two = *one;
                        *one = *now;
                    }
                } else {
                    for (now, one) in current.iter().zip(previous.iter_mut()) {
                        row_sum += (*now as i16 - *one as i16).unsigned_abs() as u32;
                        *one = *now;
                    }
                }
                temporal += GAMMA * row_sum as u64;
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

fn block_sse(main: &Plane, reference: &Plane, x: usize, y: usize, width: usize, height: usize) -> u64 {
    let mut sse = 0u64;
    for row in y..y + height {
        // A row's sum fits in 32 bits for any row narrower than 66,000 pixels, and each square in
        // 16, which lets it be a single widening multiply.
        let mut row_sse = 0u32;
        for (a, b) in main.row(row)[x..x + width].iter().zip(&reference.row(row)[x..x + width]) {
            let d = a.abs_diff(*b) as u16;
            row_sse += (d * d) as u32;
        }
        sse += row_sse as u64;
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
