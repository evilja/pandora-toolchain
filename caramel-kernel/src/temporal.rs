// Features between two consecutive frames: how well the last frame predicts this one, and whether the
// pair is a cut. Everything here runs on a quarter-resolution copy of the luma plane.
//
// The motion search is the same idea as x264's own lookahead: a small block-matching search on a
// downscaled picture says how much of a frame is *predictable* from the last one, which is what
// inter-frame bits actually pay for. Raw frame difference cannot tell a slow pan (cheap) from
// chaotic motion (expensive); the residual after motion compensation can.
//
// A frame is reduced once (`downsample`) and then only that copy is kept and compared, so the
// decider holds one small picture per frame instead of the full plane.

use crate::frame::Plane;
use serde::Serialize;

// Downscale factor of the motion-search copy.
pub const LOWRES_FACTOR: usize = 4;
// Downscale factor, from the motion-search copy, of the picture the whole-frame motion is found on.
const COARSE_FACTOR: usize = 4;
// Motion-search block side, in low-resolution pixels (32 luma pixels at full resolution).
const SEARCH_BLOCK: usize = 8;
// The longest vector a block may take, in low-resolution pixels either way (±160 luma pixels): the
// whole-frame vector's reach plus the descent from it.
const MAX_VECTOR: i32 = 40;
const SEARCH_ITERATIONS: usize = 16;
// The whole-frame search covers ±8 coarse pixels (±128 luma pixels) in three halving steps.
const GLOBAL_STEPS: [i32; 3] = [4, 2, 1];
// A block the last frame already predicts this well (half a code value a pixel) is not searched.
const SKIP_SAD: u32 = (SEARCH_BLOCK * SEARCH_BLOCK / 2) as u32;
const HISTOGRAM_BINS: usize = 32;
pub const RESIDUAL_FLOORS: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];
// A block counts as changed when the search leaves more than two code values a pixel.
const CHANGED_SAD: u32 = (2 * SEARCH_BLOCK * SEARCH_BLOCK) as u32;
// What makes a pair a cut *candidate*, looked at alone: a histogram this different; a moderately
// different one when motion compensation also fails to explain the new frame;
const CUT_HISTOGRAM: f64 = 0.5;
const CUT_HISTOGRAM_WITH_RESIDUAL: f64 = 0.25;
const CUT_RESIDUAL: f64 = 15.0;
// or the encoder's own test. Two shots with the same palette — common in anime, where a scene keeps
// its colour script across cuts — are not told apart by their histograms, but a frame the previous
// one predicts worse than the frame predicts itself is a new shot. x264's scenecut is the same
// comparison of inter cost against intra cost. The floor keeps grain, which is unpredictable both
// ways, from passing it.
const CUT_INTER_TO_INTRA: f64 = 2.0;
const CUT_INTER_FLOOR: f64 = 6.0;

// A candidate is a cut only against its neighbours. Fast action, a whip pan, a scrolling transition
// and fire all read as candidates on every pair, for as long as they last; a character snapping to
// a new pose reads as one on a single pair. What sets a cut apart is that the picture is quiet
// before it, quiet again after it, and changed nearly everywhere at once:
//
// - most of the frame's blocks changed (a pose change leaves the background where it was);
// - the residual stands out from each of the `CUT_PAIRS_BEFORE` pairs before it and each of the
//   `CUT_PAIRS_AFTER` after it, and from the two sides together by more. Three pairs back sees
//   through animation on twos and threes, where every other pair is a repeated frame.
//
// Hand-labelled on 231 candidates from 44 clips of drawn and rendered animation and live action
// (157 cuts): the candidate test alone made 143 false cuts; this makes 5 and misses 5. Looking only
// backwards made 15 and missed 15, which is why two frames of lookahead are worth their delay.
pub const CUT_PAIRS_BEFORE: usize = 3;
pub const CUT_PAIRS_AFTER: usize = 2;
const CUT_CHANGED_SHARE: f64 = 0.6;
const CUT_STANDS_OUT: f64 = 1.5;
const CUT_STANDS_OUT_BOTH: f64 = 4.0;
// A neighbour quieter than this counts as this: half a code value is still a still frame.
const CUT_QUIET_RESIDUAL: f64 = 0.5;

// The quarter-resolution copy of one frame, with everything about it that a comparison needs and
// that does not depend on the other frame.
#[derive(Clone, Debug)]
pub struct LowRes {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
    coarse_width: usize,
    coarse_height: usize,
    coarse: Vec<u8>,
    mean: f64,
    deviation: f64,
    histogram: [u32; 256],
    intra: f64,
}

// Box-filtered copy at 1/`factor`. Edges that do not fill a whole box are dropped.
fn box_filter(data: &[u8], width: usize, height: usize, stride: usize, factor: usize) -> (Vec<u8>, usize, usize) {
    let (out_width, out_height) = (width / factor, height / factor);
    let mut out = vec![0u8; out_width * out_height];
    let mut sums = vec![0u32; out_width];
    let area = (factor * factor) as u32;
    for oy in 0..out_height {
        sums.iter_mut().for_each(|sum| *sum = 0);
        for dy in 0..factor {
            let start = (oy * factor + dy) * stride;
            let row = &data[start..start + out_width * factor];
            for (sum, chunk) in sums.iter_mut().zip(row.chunks_exact(factor)) {
                *sum += chunk.iter().map(|&v| v as u32).sum::<u32>();
            }
        }
        for (value, sum) in out[oy * out_width..(oy + 1) * out_width].iter_mut().zip(&sums) {
            *value = ((sum + area / 2) / area) as u8;
        }
    }
    (out, out_width, out_height)
}

pub fn downsample(plane: &Plane) -> LowRes {
    let (data, width, height) = box_filter(plane.data, plane.width, plane.height, plane.stride, LOWRES_FACTOR);
    let (coarse, coarse_width, coarse_height) = box_filter(&data, width, height, width, COARSE_FACTOR);
    let mut histogram = [0u32; 256];
    for &value in &data {
        histogram[value as usize] += 1;
    }
    let n = data.len().max(1) as f64;
    let mean = histogram.iter().enumerate().map(|(value, &count)| value as f64 * count as f64).sum::<f64>() / n;
    let variance =
        histogram.iter().enumerate().map(|(value, &count)| (value as f64 - mean).powi(2) * count as f64).sum::<f64>() / n;
    let mut lowres = LowRes {
        width,
        height,
        data,
        coarse_width,
        coarse_height,
        coarse,
        mean,
        deviation: variance.sqrt(),
        histogram,
        intra: 0.0,
    };
    lowres.intra = intra_residual(&lowres);
    lowres
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TemporalFeatures {
    // Temporal information in the sense of ITU-T P.910 — the standard deviation of the frame
    // difference — taken on the quarter-resolution copy.
    pub ti: f64,
    // Mean absolute difference per low-resolution pixel with no motion compensation.
    pub zero_residual: f64,
    // The same after the motion search: what the encoder still has to code.
    pub mc_residual: f64,
    // Mean motion-vector length, in full-resolution pixels.
    pub motion_px: f64,
    // Length of the whole-frame vector the search started from, in full-resolution pixels: a pan.
    pub pan_px: f64,
    // Mean absolute error of predicting each low-resolution pixel from its left and upper
    // neighbours: a cheap stand-in for what the frame costs coded on its own.
    pub intra_residual: f64,
    pub histogram_distance: f64,
    // Share of the motion-search blocks the last frame does not predict even after the search.
    pub changed_share: f64,
    // What is new in the frame below each of `RESIDUAL_FLOORS`: the mean over the blocks of
    // min(r², floor²), r being the block's residual per pixel after the search. The counterpart of
    // `SpatialFeatures::detail_energy` for what the encoder codes between frames.
    pub residual_energy: [f64; 5],
    // The pair, looked at alone, reads as a cut. Whether it is one depends on the pairs around it:
    // see `is_cut`.
    pub candidate: bool,
}

// How many times the pair's residual is that of the loudest of `neighbours`.
fn stands_out(pair: &TemporalFeatures, neighbours: &[f64]) -> f64 {
    let loudest = neighbours.iter().copied().fold(CUT_QUIET_RESIDUAL, f64::max);
    pair.mc_residual / loudest
}

// The half of `is_cut` that is known as soon as the pair is: `before` holds the residuals of up to
// `CUT_PAIRS_BEFORE` pairs before it. A pair that fails this cannot become a cut.
pub fn may_be_cut(pair: &TemporalFeatures, before: &[f64]) -> bool {
    pair.candidate && pair.changed_share >= CUT_CHANGED_SHARE && stands_out(pair, before) >= CUT_STANDS_OUT
}

// Whether the pair is a cut, given the residuals of the pairs before and after it.
pub fn is_cut(pair: &TemporalFeatures, before: &[f64], after: &[f64]) -> bool {
    let (earlier, later) = (stands_out(pair, before), stands_out(pair, after));
    may_be_cut(pair, before) && later >= CUT_STANDS_OUT && earlier * later >= CUT_STANDS_OUT_BOTH
}

fn intra_residual(lowres: &LowRes) -> f64 {
    if lowres.width < 2 || lowres.height < 2 {
        return 0.0;
    }
    let mut total = 0u64;
    for y in 1..lowres.height {
        let (row, above) = (&lowres.data[y * lowres.width..(y + 1) * lowres.width], &lowres.data[(y - 1) * lowres.width..y * lowres.width]);
        let mut sum = 0u32;
        for x in 1..lowres.width {
            let prediction = (row[x - 1] as u32 + above[x] as u32 + 1) / 2;
            sum += (row[x] as u32).abs_diff(prediction);
        }
        total += sum as u64;
    }
    total as f64 / ((lowres.width - 1) * (lowres.height - 1)) as f64
}

// The previous frame with the current frame's mean and contrast: the gain and offset of x264's
// weighted prediction, fitted from the two frames' statistics. A fade or a flash changes every pixel
// by one rule, and the encoder predicts that for almost nothing, so it is compared through this
// rather than raw. Two real shots still differ after it, in shape rather than in level. The gain is
// bounded so a frame that is nearly flat is shifted, not stretched into noise.
const WEIGHT_GAIN_MIN: f64 = 0.25;
const WEIGHT_GAIN_MAX: f64 = 4.0;
const WEIGHT_FLAT_DEVIATION: f64 = 1.0;
// A change of level or contrast smaller than this is not worth a weighted copy.
const WEIGHT_MIN_OFFSET: f64 = 0.5;
const WEIGHT_MIN_GAIN: f64 = 0.02;

// The mapping from a previous-frame value to its weighted value, or None when the two frames
// already agree in level and contrast (most pairs).
fn weights(previous: &LowRes, current: &LowRes) -> Option<[u8; 256]> {
    let gain = if previous.deviation < WEIGHT_FLAT_DEVIATION || current.deviation < WEIGHT_FLAT_DEVIATION {
        1.0
    } else {
        (current.deviation / previous.deviation).clamp(WEIGHT_GAIN_MIN, WEIGHT_GAIN_MAX)
    };
    if (gain - 1.0).abs() < WEIGHT_MIN_GAIN && (current.mean - previous.mean).abs() < WEIGHT_MIN_OFFSET {
        return None;
    }
    let mut table = [0u8; 256];
    for (value, mapped) in table.iter_mut().enumerate() {
        *mapped = ((value as f64 - previous.mean) * gain + current.mean).round().clamp(0.0, 255.0) as u8;
    }
    Some(table)
}

#[inline]
fn sad(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u32).sum()
}

#[inline]
fn block_sad(current: &LowRes, reference: &[u8], x: usize, y: usize, dx: i32, dy: i32) -> u32 {
    let width = current.width;
    let mut total = 0u32;
    for row in 0..SEARCH_BLOCK {
        let cy = y + row;
        let py = (cy as i32 + dy) as usize;
        let px = (x as i32 + dx) as usize;
        total += sad(&current.data[cy * width + x..cy * width + x + SEARCH_BLOCK], &reference[py * width + px..py * width + px + SEARCH_BLOCK]);
    }
    total
}

// Best SAD for the block at (x, y): the zero vector, the neighbours' vectors and the whole-frame
// vector as predictors, then a small-diamond descent from the best of them.
fn search_block(
    current: &LowRes,
    reference: &[u8],
    x: usize,
    y: usize,
    zero: u32,
    predictors: &[(i32, i32)],
) -> (u32, (i32, i32)) {
    let max_dx = (current.width - SEARCH_BLOCK) as i32 - x as i32;
    let max_dy = (current.height - SEARCH_BLOCK) as i32 - y as i32;
    let legal = |(dx, dy): (i32, i32)| {
        dx.abs() <= MAX_VECTOR
            && dy.abs() <= MAX_VECTOR
            && dx >= -(x as i32)
            && dy >= -(y as i32)
            && dx <= max_dx
            && dy <= max_dy
    };
    let mut best = (zero, (0, 0));
    for &vector in predictors {
        if vector != (0, 0) && vector != best.1 && legal(vector) {
            let sad = block_sad(current, reference, x, y, vector.0, vector.1);
            if sad < best.0 {
                best = (sad, vector);
            }
        }
    }
    for _ in 0..SEARCH_ITERATIONS {
        if best.0 <= SKIP_SAD {
            break;
        }
        let (cx, cy) = best.1;
        let mut improved = false;
        for vector in [(cx + 1, cy), (cx - 1, cy), (cx, cy + 1), (cx, cy - 1)] {
            if !legal(vector) {
                continue;
            }
            let sad = block_sad(current, reference, x, y, vector.0, vector.1);
            if sad < best.0 {
                best = (sad, vector);
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }
    best
}

// Mean absolute difference between the coarse pictures with the reference shifted by (dx, dy), over
// the part of the frame both cover.
fn coarse_difference(current: &LowRes, reference: &[u8], dx: i32, dy: i32) -> f64 {
    let (width, height) = (current.coarse_width as i32, current.coarse_height as i32);
    let (x0, x1) = ((-dx).max(0), (width - dx).min(width));
    let (y0, y1) = ((-dy).max(0), (height - dy).min(height));
    if x1 <= x0 || y1 <= y0 {
        return f64::INFINITY;
    }
    let span = (x1 - x0) as usize;
    let mut total = 0u32;
    for y in y0..y1 {
        let a = (y * width + x0) as usize;
        let b = ((y + dy) * width + x0 + dx) as usize;
        total += sad(&current.coarse[a..a + span], &reference[b..b + span]);
    }
    total as f64 / (span * (y1 - y0) as usize) as f64
}

// The one vector that best moves the whole previous frame onto this one, in low-resolution pixels.
// A pan faster than a block's own descent can follow is found here first and handed to every block
// as a predictor, the way an encoder's vector prediction carries a pan across the frame.
fn global_motion(current: &LowRes, reference: &[u8]) -> (i32, i32) {
    if current.coarse_width < 8 || current.coarse_height < 8 {
        return (0, 0);
    }
    let mut best = (coarse_difference(current, reference, 0, 0), (0, 0));
    if best.0 < 0.5 {
        return (0, 0);
    }
    for step in GLOBAL_STEPS {
        let (cx, cy) = best.1;
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let vector = (cx + dx * step, cy + dy * step);
            let difference = coarse_difference(current, reference, vector.0, vector.1);
            // A longer vector has to earn it: a flat frame matches itself at any shift.
            if difference < best.0 * 0.97 {
                best = (difference, vector);
            }
        }
    }
    (best.1.0 * COARSE_FACTOR as i32, best.1.1 * COARSE_FACTOR as i32)
}

fn histogram_distance(previous: &LowRes, current: &LowRes, table: Option<&[u8; 256]>) -> f64 {
    let (mut a, mut b) = ([0u32; HISTOGRAM_BINS], [0u32; HISTOGRAM_BINS]);
    for value in 0..256 {
        let mapped = table.map_or(value, |table| table[value] as usize);
        a[mapped * HISTOGRAM_BINS / 256] += previous.histogram[value];
        b[value * HISTOGRAM_BINS / 256] += current.histogram[value];
    }
    let total = current.data.len().max(1) as f64;
    0.5 * a.iter().zip(&b).map(|(p, q)| p.abs_diff(*q) as f64).sum::<f64>() / total
}

// Standard deviation of the difference between the two low-resolution frames, unweighted.
fn temporal_information(previous: &LowRes, current: &LowRes) -> f64 {
    let n = current.data.len().min(previous.data.len());
    if n == 0 {
        return 0.0;
    }
    let (mut sum, mut squares) = (0i64, 0u64);
    for (a, b) in current.data.iter().zip(&previous.data) {
        let difference = *a as i32 - *b as i32;
        sum += difference as i64;
        squares += (difference * difference) as u64;
    }
    let mean = sum as f64 / n as f64;
    (squares as f64 / n as f64 - mean * mean).max(0.0).sqrt()
}

pub fn compare(previous: &LowRes, current: &LowRes) -> TemporalFeatures {
    let intra = current.intra;
    if current.width != previous.width || current.height != previous.height {
        return TemporalFeatures {
            intra_residual: intra,
            histogram_distance: 1.0,
            changed_share: 1.0,
            candidate: true,
            ..Default::default()
        };
    }
    let ti = temporal_information(previous, current);
    let table = weights(previous, current);
    let distance = histogram_distance(previous, current, table.as_ref());
    if current.width < SEARCH_BLOCK || current.height < SEARCH_BLOCK {
        return TemporalFeatures {
            ti,
            intra_residual: intra,
            histogram_distance: distance,
            changed_share: if distance > CUT_HISTOGRAM { 1.0 } else { 0.0 },
            candidate: distance > CUT_HISTOGRAM,
            ..Default::default()
        };
    }
    let weighted: Option<(Vec<u8>, Vec<u8>)> = table.map(|table| {
        (
            previous.data.iter().map(|&v| table[v as usize]).collect(),
            previous.coarse.iter().map(|&v| table[v as usize]).collect(),
        )
    });
    let (reference, coarse) = match &weighted {
        Some((data, coarse)) => (data.as_slice(), coarse.as_slice()),
        None => (previous.data.as_slice(), previous.coarse.as_slice()),
    };
    let global = global_motion(current, coarse);
    let blocks_x = current.width / SEARCH_BLOCK;
    let blocks_y = current.height / SEARCH_BLOCK;
    let mut vectors = vec![(0i32, 0i32); blocks_x * blocks_y];
    let (mut zero, mut compensated, mut length) = (0u64, 0u64, 0f64);
    let mut changed = 0u32;
    let mut residual_energy = [0f64; 5];
    let mut add_residual = |sad: u32| {
        let level = sad as f64 / (SEARCH_BLOCK * SEARCH_BLOCK) as f64;
        for (slot, floor) in residual_energy.iter_mut().zip(RESIDUAL_FLOORS) {
            *slot += level.min(floor).powi(2);
        }
    };
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let (x, y) = (bx * SEARCH_BLOCK, by * SEARCH_BLOCK);
            let still = block_sad(current, reference, x, y, 0, 0);
            zero += still as u64;
            if still <= SKIP_SAD {
                compensated += still as u64;
                add_residual(still);
                continue;
            }
            let mut predictors = [global, (0, 0), (0, 0)];
            if bx > 0 {
                predictors[1] = vectors[by * blocks_x + bx - 1];
            }
            if by > 0 {
                predictors[2] = vectors[(by - 1) * blocks_x + bx];
            }
            let (sad, vector) = search_block(current, reference, x, y, still, &predictors);
            compensated += sad as u64;
            changed += (sad > CHANGED_SAD) as u32;
            add_residual(sad);
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
        pan_px: ((global.0 * global.0 + global.1 * global.1) as f64).sqrt() * LOWRES_FACTOR as f64,
        intra_residual: intra,
        histogram_distance: distance,
        changed_share: changed as f64 / blocks,
        residual_energy: residual_energy.map(|sum| sum / blocks),
        candidate: distance > CUT_HISTOGRAM
            || (distance > CUT_HISTOGRAM_WITH_RESIDUAL && mc_residual > CUT_RESIDUAL)
            || (mc_residual > CUT_INTER_FLOOR && mc_residual > CUT_INTER_TO_INTRA * intra),
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
        let features = compare(&downsample(&pa), &downsample(&pb));
        assert!(features.zero_residual > 5.0, "{features:?}");
        assert!(features.mc_residual < features.zero_residual * 0.35, "{features:?}");
        assert!((features.motion_px - 8.0).abs() < 3.0, "{features:?}");
        assert!(!features.candidate);
    }

    #[test]
    fn a_static_frame_has_no_temporal_information() {
        let a = textured(0.0);
        let pa = plane(&a, 320, 192);
        let lowres = downsample(&pa);
        let features = compare(&lowres, &lowres);
        assert_eq!(features.ti, 0.0);
        assert_eq!(features.mc_residual, 0.0);
        assert!(!features.candidate);
    }

    // Same values, rearranged: the histograms agree and only prediction can tell the shots apart.
    #[test]
    fn a_cut_between_shots_with_one_palette_is_still_a_cut() {
        let a = textured(0.0);
        let b = picture(320, 192, |x, y| {
            let (x, y) = (x as f64, y as f64);
            128.0 + 50.0 * ((y * 0.23 + 1.0).sin() + (x * 0.13).cos() * (y * 0.07).sin())
        });
        let (pa, pb) = (plane(&a, 320, 192), plane(&b, 320, 192));
        let features = compare(&downsample(&pa), &downsample(&pb));
        assert!(features.histogram_distance < CUT_HISTOGRAM_WITH_RESIDUAL, "{features:?}");
        assert!(features.candidate && features.changed_share > 0.9, "{features:?}");
        assert!(is_cut(&features, &[0.4, 0.0, 0.6], &[0.0, 0.5]));
    }

    // Grain is as unpredictable from the last frame as from the neighbouring pixels.
    #[test]
    fn grain_on_a_still_shot_is_not_a_cut() {
        let mut noise = crate::spatial::tests::Noise::new(7);
        let mut grain = |seed: f64| {
            picture(320, 192, |x, y| 128.0 + 30.0 * ((x as f64 * 0.05 + seed).sin() * (y as f64 * 0.04).cos()))
                .into_iter()
                .map(|value| (value as f64 + noise.gaussian() * 6.0).clamp(0.0, 255.0) as u8)
                .collect::<Vec<u8>>()
        };
        let (a, b) = (grain(0.0), grain(0.0));
        let (pa, pb) = (plane(&a, 320, 192), plane(&b, 320, 192));
        let features = compare(&downsample(&pa), &downsample(&pb));
        assert!(!features.candidate, "{features:?}");
    }

    // Fades change every pixel by the same rule, which an encoder predicts with weighted prediction
    // and is not a new shot: a flat dark frame brightening one code value a frame (its histogram
    // jumps a bin every eight), and line art washing out to white (the frame's own detail falls
    // towards nothing while each step stays several code values).
    #[test]
    fn fades_are_not_cuts() {
        let flat = |level: f64| picture(320, 192, |x, _| level + x as f64 * 0.002);
        let wash = |t: f64| {
            picture(320, 192, |x, y| {
                let art = if (x / 24 + y / 24) % 2 == 0 { 60.0 } else { 140.0 };
                art + (235.0 - art) * t
            })
        };
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..16)
            .map(|i| (flat(28.0 + i as f64), flat(29.0 + i as f64)))
            .chain((0..12).map(|i| (wash(i as f64 / 12.0), wash((i + 1) as f64 / 12.0))))
            .collect();
        for (index, (a, b)) in pairs.iter().enumerate() {
            let (pa, pb) = (plane(a, 320, 192), plane(b, 320, 192));
            let features = compare(&downsample(&pa), &downsample(&pb));
            assert!(!features.candidate, "pair {index}: {features:?}");
        }
    }

    #[test]
    fn an_unrelated_frame_is_a_cut() {
        let a = textured(0.0);
        let b = picture(320, 192, |x, y| if (x / 40 + y / 40) % 2 == 0 { 20.0 } else { 235.0 });
        let (pa, pb) = (plane(&a, 320, 192), plane(&b, 320, 192));
        let features = compare(&downsample(&pa), &downsample(&pb));
        assert!(features.candidate, "{features:?}");
    }

    fn pair(mc_residual: f64, changed_share: f64) -> TemporalFeatures {
        TemporalFeatures { mc_residual, changed_share, candidate: true, ..Default::default() }
    }

    // Each case is a labelled pair from the calibration clips, with the residuals around it.
    #[test]
    fn a_cut_is_judged_against_the_pairs_around_it() {
        // A cut out of action into a still shot barely stands out from what came before, and
        // overwhelmingly from what follows.
        assert!(is_cut(&pair(7.8, 0.9), &[4.7, 3.7, 2.6], &[0.7, 0.8]));
        // A cut inside drawn animation on twos: the repeated frames around it are silent.
        assert!(is_cut(&pair(13.4, 0.95), &[7.0, 5.0, 6.0], &[0.0, 2.2]));
        // A vertical scroll between two scenes: every pair of it is as loud as the last.
        assert!(!is_cut(&pair(24.7, 0.94), &[24.7, 24.6, 24.7], &[24.7, 24.7]));
        // Fire filling the frame: loud, but the pairs after it are too.
        assert!(!is_cut(&pair(13.3, 0.65), &[1.1, 1.1, 3.2], &[9.5, 8.0]));
        // A character snapping to a new pose over a still background: one loud pair between silent
        // ones, exactly like a cut, but most of the frame did not change.
        assert!(!is_cut(&pair(6.6, 0.42), &[2.6, 0.4, 0.4], &[0.4, 0.4]));
        // The first pair of fast motion passes the backward half and fails once the next arrive.
        assert!(may_be_cut(&pair(9.0, 0.8), &[0.5, 0.2, 1.0]));
        assert!(!is_cut(&pair(9.0, 0.8), &[0.5, 0.2, 1.0], &[8.0, 9.5]));
        // Not a candidate at all.
        assert!(!is_cut(&TemporalFeatures { candidate: false, ..pair(9.0, 1.0) }, &[], &[]));
    }
}
