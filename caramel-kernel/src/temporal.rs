// Features between two consecutive frames: P.910 temporal information, a cheap motion search on a
// quarter-resolution copy, and scene-cut detection.
//
// The motion search is the same idea as x264's own lookahead: a small block-matching search on a
// downscaled picture says how much of a frame is *predictable* from the last one, which is what
// inter-frame bits actually pay for. Raw frame difference cannot tell a slow pan (cheap) from
// chaotic motion (expensive); the residual after motion compensation can.

use crate::frame::Plane;
use serde::Serialize;

// Downscale factor of the motion-search copy.
pub const LOWRES_FACTOR: usize = 4;
// Motion-search block side, in low-resolution pixels (32 luma pixels at full resolution).
const SEARCH_BLOCK: usize = 8;
// Search range in low-resolution pixels either way (±32 luma pixels).
const SEARCH_RANGE: i32 = 8;
const SEARCH_ITERATIONS: usize = 16;
const HISTOGRAM_BINS: usize = 32;
// A histogram this different is a cut on its own; a moderately different one is a cut when motion
// compensation also fails to explain the new frame.
const CUT_HISTOGRAM: f64 = 0.5;
const CUT_HISTOGRAM_WITH_RESIDUAL: f64 = 0.25;
const CUT_RESIDUAL: f64 = 15.0;

#[derive(Clone, Debug)]
pub struct LowRes {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

// Box-filtered quarter-resolution copy. Edges that do not fill a whole box are dropped.
pub fn downsample(plane: &Plane) -> LowRes {
    let width = plane.width / LOWRES_FACTOR;
    let height = plane.height / LOWRES_FACTOR;
    let mut data = vec![0u8; width * height];
    let mut sums = vec![0u32; width];
    for ly in 0..height {
        sums.iter_mut().for_each(|sum| *sum = 0);
        for dy in 0..LOWRES_FACTOR {
            let row = plane.row(ly * LOWRES_FACTOR + dy);
            for (lx, sum) in sums.iter_mut().enumerate() {
                let start = lx * LOWRES_FACTOR;
                *sum += row[start..start + LOWRES_FACTOR].iter().map(|&v| v as u32).sum::<u32>();
            }
        }
        let area = (LOWRES_FACTOR * LOWRES_FACTOR) as u32;
        for (lx, sum) in sums.iter().enumerate() {
            data[ly * width + lx] = ((sum + area / 2) / area) as u8;
        }
    }
    LowRes { width, height, data }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TemporalFeatures {
    // ITU-T P.910 temporal information: the standard deviation of the frame difference.
    pub ti: f64,
    // Mean absolute difference per low-resolution pixel with no motion compensation.
    pub zero_residual: f64,
    // The same after the motion search: what the encoder still has to code.
    pub mc_residual: f64,
    // Mean motion-vector length, in full-resolution pixels.
    pub motion_px: f64,
    pub histogram_distance: f64,
    pub cut: bool,
}

#[inline]
fn block_sad(current: &LowRes, previous: &LowRes, x: usize, y: usize, dx: i32, dy: i32) -> u32 {
    let mut sad = 0u32;
    for row in 0..SEARCH_BLOCK {
        let cy = y + row;
        let py = (cy as i32 + dy) as usize;
        let current_row = &current.data[cy * current.width + x..cy * current.width + x + SEARCH_BLOCK];
        let px = (x as i32 + dx) as usize;
        let previous_row = &previous.data[py * previous.width + px..py * previous.width + px + SEARCH_BLOCK];
        for (a, b) in current_row.iter().zip(previous_row) {
            sad += a.abs_diff(*b) as u32;
        }
    }
    sad
}

// Best SAD for the block at (x, y): the zero vector and the neighbours' vectors as predictors, then
// a small-diamond descent from the best of them.
fn search_block(
    current: &LowRes,
    previous: &LowRes,
    x: usize,
    y: usize,
    predictors: &[(i32, i32)],
) -> (u32, (i32, i32)) {
    let max_dx = (previous.width - SEARCH_BLOCK) as i32 - x as i32;
    let max_dy = (previous.height - SEARCH_BLOCK) as i32 - y as i32;
    let legal = |(dx, dy): (i32, i32)| {
        dx.abs() <= SEARCH_RANGE
            && dy.abs() <= SEARCH_RANGE
            && dx >= -(x as i32)
            && dy >= -(y as i32)
            && dx <= max_dx
            && dy <= max_dy
    };
    let mut best = (block_sad(current, previous, x, y, 0, 0), (0, 0));
    for &vector in predictors {
        if vector != (0, 0) && legal(vector) {
            let sad = block_sad(current, previous, x, y, vector.0, vector.1);
            if sad < best.0 {
                best = (sad, vector);
            }
        }
    }
    for _ in 0..SEARCH_ITERATIONS {
        let (cx, cy) = best.1;
        let mut improved = false;
        for vector in [(cx + 1, cy), (cx - 1, cy), (cx, cy + 1), (cx, cy - 1)] {
            if !legal(vector) {
                continue;
            }
            let sad = block_sad(current, previous, x, y, vector.0, vector.1);
            if sad < best.0 {
                best = (sad, vector);
                improved = true;
            }
        }
        if !improved || best.0 == 0 {
            break;
        }
    }
    best
}

fn histogram(lowres: &LowRes) -> [f64; HISTOGRAM_BINS] {
    let mut bins = [0f64; HISTOGRAM_BINS];
    for &value in &lowres.data {
        bins[value as usize * HISTOGRAM_BINS / 256] += 1.0;
    }
    let total = lowres.data.len().max(1) as f64;
    bins.iter_mut().for_each(|bin| *bin /= total);
    bins
}

// Standard deviation of the full-resolution difference, on the same every-second-pixel grid the
// spatial pass uses.
fn temporal_information(previous: &Plane, current: &Plane) -> f64 {
    let (mut n, mut sum, mut squares) = (0u64, 0f64, 0f64);
    let mut y = 0;
    while y < current.height.min(previous.height) {
        let (a, b) = (current.row(y), previous.row(y));
        let mut x = 0;
        while x < a.len().min(b.len()) {
            let difference = a[x] as f64 - b[x] as f64;
            n += 1;
            sum += difference;
            squares += difference * difference;
            x += 2;
        }
        y += 2;
    }
    if n == 0 {
        return 0.0;
    }
    let mean = sum / n as f64;
    (squares / n as f64 - mean * mean).max(0.0).sqrt()
}

pub fn compare(
    previous_full: &Plane,
    current_full: &Plane,
    previous: &LowRes,
    current: &LowRes,
) -> TemporalFeatures {
    let ti = temporal_information(previous_full, current_full);
    let distance = {
        let (a, b) = (histogram(previous), histogram(current));
        0.5 * a.iter().zip(b.iter()).map(|(p, q)| (p - q).abs()).sum::<f64>()
    };
    if current.width < SEARCH_BLOCK
        || current.height < SEARCH_BLOCK
        || current.width != previous.width
        || current.height != previous.height
    {
        return TemporalFeatures { ti, histogram_distance: distance, cut: distance > CUT_HISTOGRAM, ..Default::default() };
    }
    let blocks_x = current.width / SEARCH_BLOCK;
    let blocks_y = current.height / SEARCH_BLOCK;
    let mut vectors = vec![(0i32, 0i32); blocks_x * blocks_y];
    let (mut zero, mut compensated, mut length) = (0u64, 0u64, 0f64);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let (x, y) = (bx * SEARCH_BLOCK, by * SEARCH_BLOCK);
            let mut predictors = [(0, 0); 2];
            if bx > 0 {
                predictors[0] = vectors[by * blocks_x + bx - 1];
            }
            if by > 0 {
                predictors[1] = vectors[(by - 1) * blocks_x + bx];
            }
            zero += block_sad(current, previous, x, y, 0, 0) as u64;
            let (sad, vector) = search_block(current, previous, x, y, &predictors);
            compensated += sad as u64;
            length += ((vector.0 * vector.0 + vector.1 * vector.1) as f64).sqrt();
            vectors[by * blocks_x + bx] = vector;
        }
    }
    let blocks = (blocks_x * blocks_y) as f64;
    let pixels = blocks * (SEARCH_BLOCK * SEARCH_BLOCK) as f64;
    let mc_residual = compensated as f64 / pixels;
    TemporalFeatures {
        ti,
        zero_residual: zero as f64 / pixels,
        mc_residual,
        motion_px: length / blocks * LOWRES_FACTOR as f64,
        histogram_distance: distance,
        cut: distance > CUT_HISTOGRAM
            || (distance > CUT_HISTOGRAM_WITH_RESIDUAL && mc_residual > CUT_RESIDUAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial::tests::picture;

    fn plane(data: &[u8], width: usize, height: usize) -> Plane<'_> {
        Plane::new(data, width, height, width).unwrap()
    }

    fn textured(shift: f64) -> Vec<u8> {
        picture(320, 192, |x, y| {
            let x = x as f64 + shift;
            128.0 + 50.0 * ((x * 0.21).sin() + (y as f64 * 0.17).cos() * (x * 0.05).sin())
        })
    }

    #[test]
    fn downsampling_averages_each_box() {
        let data = picture(16, 8, |x, _| if x < 4 { 0.0 } else { 200.0 });
        let lowres = downsample(&plane(&data, 16, 8));
        assert_eq!((lowres.width, lowres.height), (4, 2));
        assert_eq!(lowres.data[0], 0);
        assert_eq!(lowres.data[lowres.width + 1], 200);
    }

    #[test]
    fn a_pan_is_explained_by_motion_compensation() {
        let (a, b) = (textured(0.0), textured(8.0));
        let (pa, pb) = (plane(&a, 320, 192), plane(&b, 320, 192));
        let features = compare(&pa, &pb, &downsample(&pa), &downsample(&pb));
        assert!(features.zero_residual > 5.0, "{features:?}");
        assert!(features.mc_residual < features.zero_residual * 0.35, "{features:?}");
        assert!((features.motion_px - 8.0).abs() < 3.0, "{features:?}");
        assert!(!features.cut);
    }

    #[test]
    fn a_static_frame_has_no_temporal_information() {
        let a = textured(0.0);
        let pa = plane(&a, 320, 192);
        let lowres = downsample(&pa);
        let features = compare(&pa, &pa, &lowres, &lowres);
        assert_eq!(features.ti, 0.0);
        assert_eq!(features.mc_residual, 0.0);
        assert!(!features.cut);
    }

    #[test]
    fn an_unrelated_frame_is_a_cut() {
        let a = textured(0.0);
        let b = picture(320, 192, |x, y| if (x / 40 + y / 40) % 2 == 0 { 20.0 } else { 235.0 });
        let (pa, pb) = (plane(&a, 320, 192), plane(&b, 320, 192));
        let features = compare(&pa, &pb, &downsample(&pa), &downsample(&pb));
        assert!(features.cut, "{features:?}");
    }
}
