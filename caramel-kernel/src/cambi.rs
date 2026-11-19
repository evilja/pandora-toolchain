// CAMBI, Netflix's banding detector, on one 8-bit luma plane: the same score libvmaf's `cambi`
// feature gives the frame at its default settings (the frame its own encode, bt1886, limited range,
// a 65-pixel window at 4K scaled to the frame, contrasts of one to four 10-bit codes, the top 60% of
// each scale pooled). A port of libvmaf's `feature/cambi.c` (BSD-2-Clause-Patent, Netflix): the lab
// scores its encodes' banding with libvmaf, and the fast encode is scored with this, so the two
// read a frame alike without the kernel linking libvmaf.
//
// The fast encode reads banding as XPSNR cannot: a flat gradient cut into steps loses almost no
// signal, and was where most of the model's held-out errors lay, raises banding past the limit and
// raises missed because a scene was feared to band.

use crate::frame::Plane;

const SCALES: usize = 5;
const SCALE_WEIGHTS: [f64; SCALES] = [16.0, 8.0, 4.0, 2.0, 1.0];
// One to four 10-bit codes of contrast, the larger counting for more (the suprathreshold response).
const DIFFS: usize = 4;
const DIFF_WEIGHTS: [u32; DIFFS] = [1, 2, 3, 4];
const WINDOW_AT_4K: usize = 65;
const TOP_SHARE: f64 = 0.6;
// The smallest contrast against the luminance it sits on that the eye still sees.
const TVI: f64 = 0.019;
const MASK_FILTER: usize = 7;
const MIN_SIDE: usize = 216;
const MAX_SCORE: f64 = 1000.0;
// Limited-range 10-bit luma.
const FOOT: i32 = 64;
const HEAD: i32 = 940;

// The frame's CAMBI: 0 for none, rising with how much and how visibly it bands. None for a frame
// smaller than libvmaf measures.
pub fn cambi(luma: &Plane) -> Option<f64> {
    let (width, height) = (luma.width, luma.height);
    if width < MIN_SIDE && height < MIN_SIDE {
        return None;
    }
    let tvi = tvi_for_diffs();
    let window = window_size(width, height);
    let mut image = to_10_bit(luma);
    let mut mask = spatial_mask(&image, width, height);
    let (mut w, mut h) = (width, height);
    let mut scores = [0.0; SCALES];
    for (scale, score) in scores.iter_mut().enumerate() {
        if scale > 0 {
            let (next_w, next_h) = (w.div_ceil(2), h.div_ceil(2));
            image = decimate(&image, w, next_w, next_h);
            mask = decimate(&mask, w, next_w, next_h);
            (w, h) = (next_w, next_h);
        }
        image = mode_filter(&image, w, h);
        let mut values = c_values(&image, &mask, w, h, window, &tvi);
        *score = top_mean(&mut values, w * h, TOP_SHARE);
    }
    let pixels_in_window = ((window / 2) * 2 + 1).pow(2) as f64;
    let score = scores.iter().zip(SCALE_WEIGHTS).map(|(score, weight)| score * weight).sum::<f64>() / pixels_in_window;
    Some(score.min(MAX_SCORE))
}

// The plane with its dither and grain wiped out: each pixel the mean of the 5x5 box around it
// (fewer at the edges), rounded back to 8 bits. A gradient a source holds smooth only by its dither
// comes out of this in steps, as it does out of an encoder that spends nothing on the dither, so
// `cambi` of it less `cambi` of the source says what banding the source is ready to show before
// anything is encoded (`fast::added_banding`).
pub fn smoothed(luma: &Plane) -> Vec<u8> {
    const RADIUS: usize = 2;
    let (w, h) = (luma.width, luma.height);
    let mut rows = vec![0u32; w * h];
    for y in 0..h {
        let row = luma.row(y);
        let mut sum: u32 = row[..(RADIUS + 1).min(w)].iter().map(|value| *value as u32).sum();
        for x in 0..w {
            let (left, right) = (x.saturating_sub(RADIUS), (x + RADIUS).min(w - 1));
            if x > 0 {
                if x + RADIUS < w {
                    sum += row[x + RADIUS] as u32;
                }
                if x > RADIUS {
                    sum -= row[x - RADIUS - 1] as u32;
                }
            }
            // Fixed point: the row's mean times 256.
            rows[y * w + x] = (sum << 8) / (right - left + 1) as u32;
        }
    }
    // The vertical pass keeps each column's running sum and walks the rows in memory order.
    let mut sums = vec![0u32; w];
    for y in 0..(RADIUS + 1).min(h) {
        for (sum, value) in sums.iter_mut().zip(&rows[y * w..(y + 1) * w]) {
            *sum += *value;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let (top, bottom) = (y.saturating_sub(RADIUS), (y + RADIUS).min(h - 1));
        if y > 0 {
            if y + RADIUS < h {
                for (sum, value) in sums.iter_mut().zip(&rows[(y + RADIUS) * w..(y + RADIUS + 1) * w]) {
                    *sum += *value;
                }
            }
            if y > RADIUS {
                for (sum, value) in sums.iter_mut().zip(&rows[(y - RADIUS - 1) * w..(y - RADIUS) * w]) {
                    *sum -= *value;
                }
            }
        }
        let count = (bottom - top + 1) as u32;
        let (add, divisor) = (count * 128, count * 256);
        for (out, sum) in out[y * w..(y + 1) * w].iter_mut().zip(&sums) {
            *out = ((*sum + add) / divisor).min(255) as u8;
        }
    }
    out
}

// libvmaf's window, 65 pixels at 4K, scaled by the frame's width plus height and made odd.
fn window_size(width: usize, height: usize) -> usize {
    ((WINDOW_AT_4K * (width + height) / 375) >> 4) | 1
}

fn luminance(sample: i32) -> f64 {
    const GAMMA: f64 = 2.4;
    const WHITE: f64 = 300.0;
    const BLACK: f64 = 0.01;
    let normalized = (sample.clamp(FOOT, HEAD) - FOOT) as f64 / (HEAD - FOOT) as f64;
    let (white, black) = (WHITE.powf(1.0 / GAMMA), BLACK.powf(1.0 / GAMMA));
    let a = (white - black).powf(GAMMA);
    let b = black / (white - black);
    a * (normalized + b).max(0.0).powf(GAMMA)
}

fn visible(sample: i32, diff: i32) -> bool {
    let mean = luminance(sample);
    luminance(sample + diff) - mean > TVI * mean
}

// For each contrast, the brightest 10-bit level at which a step of it is still visible, shifted by
// `DIFFS` as libvmaf keeps it: brighter than that the step is under threshold and is no band.
fn tvi_for_diffs() -> [i32; DIFFS] {
    std::array::from_fn(|d| {
        let diff = d as i32 + 1;
        let head = HEAD - diff - 1;
        let level = if !visible(FOOT, diff) {
            0
        } else {
            let mut sample = FOOT;
            while sample < head && visible(sample + 1, diff) {
                sample += 1;
            }
            if sample == head && visible(head + 1, diff) { 1023 } else { sample }
        };
        level + DIFFS as i32
    })
}

// The plane at 10 bits, then the 2x2 average libvmaf applies to an 8-bit encode so its dither
// does not read as bands.
//
// Each 10-bit sample is the 8-bit one shifted left by 2, so the mean of four is exactly their 8-bit
// sum and the mean of two is their 8-bit sum shifted left by 1; that is what is computed, a row at a
// time.
fn to_10_bit(luma: &Plane) -> Vec<u16> {
    let (w, h) = (luma.width, luma.height);
    let mut out = vec![0u16; w * h];
    for y in 0..h {
        let row = luma.row(y);
        let out_row = &mut out[y * w..(y + 1) * w];
        if y + 1 < h {
            let below = luma.row(y + 1);
            for x in 0..w - 1 {
                out_row[x] = row[x] as u16 + row[x + 1] as u16 + below[x] as u16 + below[x + 1] as u16;
            }
            out_row[w - 1] = (row[w - 1] as u16 + below[w - 1] as u16) << 1;
        } else {
            for x in 0..w - 1 {
                out_row[x] = (row[x] as u16 + row[x + 1] as u16) << 1;
            }
            out_row[w - 1] = (row[w - 1] as u16) << 2;
        }
    }
    out
}

fn ceil_log2(value: u32) -> i32 {
    if value == 0 { 0 } else { 32 - (value - 1).leading_zeros() as i32 }
}

// Where bands can be seen at all: pixels whose 7x7 neighbourhood is mostly flat, a pixel being
// flat when it equals its right and lower neighbours. Detail hides a step, so it is left out.
fn spatial_mask(image: &[u16], w: usize, h: usize) -> Vec<u16> {
    let threshold = (MASK_FILTER * MASK_FILTER) as i32 + 3 * (ceil_log2(((w >> 6) * (h >> 6)) as u32) - 11) - 1;
    let threshold = (threshold >> 1) as u32;
    // A summed-area table with a zero row and column in front. A pixel is flat when it equals its
    // right and lower neighbours; the last column and row compare only with what they have.
    let stride = w + 1;
    let mut sums = vec![0u32; stride * (h + 1)];
    for y in 0..h {
        let row = &image[y * w..(y + 1) * w];
        let below = if y + 1 < h { Some(&image[(y + 1) * w..(y + 2) * w]) } else { None };
        let (above_sums, current_sums) = sums[y * stride..(y + 2) * stride].split_at_mut(stride);
        let mut running = 0u32;
        for x in 0..w {
            let value = row[x];
            let right = x + 1 == w || row[x + 1] == value;
            let down = below.is_none_or(|below| below[x] == value);
            running += (right && down) as u32;
            current_sums[x + 1] = above_sums[x + 1] + running;
        }
    }
    let pad = MASK_FILTER / 2;
    let mut mask = vec![0u16; w * h];
    for y in 0..h {
        let (top, bottom) = (y.saturating_sub(pad), (y + pad + 1).min(h));
        let (top_row, bottom_row) = (&sums[top * stride..(top + 1) * stride], &sums[bottom * stride..(bottom + 1) * stride]);
        let mask_row = &mut mask[y * w..(y + 1) * w];
        for x in 0..w {
            let (left, right) = (x.saturating_sub(pad), (x + pad + 1).min(w));
            let count = bottom_row[right] + top_row[left] - top_row[right] - bottom_row[left];
            mask_row[x] = (count > threshold) as u16;
        }
    }
    mask
}

// Every other pixel of every other row.
fn decimate(plane: &[u16], w: usize, next_w: usize, next_h: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(next_w * next_h);
    for y in 0..next_h {
        out.extend(plane[2 * y * w..(2 * y + 1) * w].iter().step_by(2).take(next_w));
    }
    out
}

// Written as selects rather than branches, so a row of them vectorises.
fn mode3(a: u16, b: u16, c: u16) -> u16 {
    let otherwise = if b == c { b } else { a.min(b).min(c) };
    if a == b || a == c { a } else { otherwise }
}

// A 3x3 mode filter, by rows then by columns, as libvmaf runs it: the outer columns are not
// filtered across, and the first and last rows are left as they were.
fn mode_filter(image: &[u16], w: usize, h: usize) -> Vec<u16> {
    let mut horizontal = image.to_vec();
    for y in 0..h {
        let row = &image[y * w..(y + 1) * w];
        let out = &mut horizontal[y * w..(y + 1) * w];
        for x in 1..w.saturating_sub(1) {
            out[x] = mode3(row[x - 1], row[x], row[x + 1]);
        }
    }
    let mut out = image.to_vec();
    for y in 1..h.saturating_sub(1) {
        let (above, current, below) = (&horizontal[(y - 1) * w..y * w], &horizontal[y * w..(y + 1) * w], &horizontal[(y + 1) * w..(y + 2) * w]);
        for (x, out) in out[y * w..(y + 1) * w].iter_mut().enumerate() {
            *out = mode3(above[x], current[x], below[x]);
        }
    }
    out
}

// Each flat pixel's banding: how many of its window's flat pixels share its level, against how many
// sit one to four codes away on the side with more, weighted by that contrast. A pixel with no
// visible step around it is 0, and only the values above 0 are returned: `top_mean` counts the
// rest from the plane's size.
fn c_values(image: &[u16], mask: &[u16], w: usize, h: usize, window: usize, tvi: &[i32; DIFFS]) -> Vec<f32> {
    let pad = window / 2;
    // Only levels a visible step can involve are counted; the rest could not change any value.
    let band = (tvi[DIFFS - 1] + 1) as usize;
    let mut histograms = vec![0u16; band * w];
    let mut values = Vec::new();
    let add = |histograms: &mut [u16], y: usize, sign: bool| {
        for x in 0..w {
            if mask[y * w + x] == 0 {
                continue;
            }
            let level = image[y * w + x] as usize;
            if level >= band {
                continue;
            }
            let row = &mut histograms[level * w..(level + 1) * w];
            for count in &mut row[x.saturating_sub(pad)..(x + pad + 1).min(w)] {
                if sign { *count += 1 } else { *count -= 1 }
            }
        }
    };
    for y in 0..pad.min(h) {
        add(&mut histograms, y, true);
    }
    for y in 0..h {
        if y + pad < h {
            add(&mut histograms, y + pad, true);
        }
        if y > pad {
            add(&mut histograms, y - pad - 1, false);
        }
        for x in 0..w {
            if mask[y * w + x] == 0 {
                continue;
            }
            let shifted = image[y * w + x] as i32 + DIFFS as i32;
            let level = image[y * w + x] as usize;
            if level >= band {
                continue;
            }
            let count = |level: i64| if level < 0 || level as usize >= band { 0 } else { histograms[level as usize * w + x] as u32 };
            let same = count(level as i64);
            let mut best = 0f32;
            for d in 0..DIFFS {
                if shifted > tvi[d] {
                    continue;
                }
                let step = d as i64 + 1;
                let other = count(level as i64 + step).max(count(level as i64 - step));
                if other + same == 0 {
                    continue;
                }
                let value = (DIFF_WEIGHTS[d] * same * other) as f32 * (1.0 / (other + same) as f32);
                best = best.max(value);
            }
            if best > 0.0 {
                values.push(best);
            }
        }
    }
    values
}

// The mean of the largest `share` of `total` values, of which `positive` are the ones above 0 and the
// rest are 0. When the share reaches past every positive value, the zeros it takes add nothing, so
// only a share smaller than the positives needs choosing among them.
fn top_mean(positive: &mut [f32], total: usize, share: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let count = ((share * total as f64) as usize).clamp(1, total);
    if count < positive.len() {
        positive.select_nth_unstable_by(count - 1, |a, b| b.total_cmp(a));
    }
    positive[..count.min(positive.len())].iter().map(|value| *value as f64).sum::<f64>() / count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(width: usize, height: usize, pixel: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..height * width).map(|at| pixel(at % width, at / width)).collect()
    }

    #[test]
    fn the_window_and_thresholds_are_libvmafs() {
        assert_eq!(window_size(1920, 1080), 33);
        assert_eq!(window_size(3840, 2160), 65);
        let tvi = tvi_for_diffs();
        // Brighter levels need larger steps to be seen; each contrast's limit is above the last.
        assert!(tvi.windows(2).all(|pair| pair[0] < pair[1]), "{tvi:?}");
        assert!(tvi[0] > FOOT && tvi[DIFFS - 1] < HEAD + DIFFS as i32, "{tvi:?}");
    }

    #[test]
    fn a_flat_frame_has_no_bands_and_a_stepped_gradient_does() {
        let (w, h) = (320, 240);
        let flat = plane(w, h, |_, _| 40);
        assert_eq!(cambi(&Plane::new(&flat, w, h, w).unwrap()), Some(0.0));
        // A dark ramp quantized into wide one-code steps: the bands an encode cuts a sky into.
        let stepped = plane(w, h, |x, _| 30 + (x / 40) as u8);
        let banded = cambi(&Plane::new(&stepped, w, h, w).unwrap()).unwrap();
        assert!(banded > 0.5, "{banded}");
        // Noise over the same ramp hides the steps.
        let noisy = plane(w, h, |x, y| 30 + (x / 40) as u8 + ((x * 7 + y * 13) % 5) as u8 * 3);
        let hidden = cambi(&Plane::new(&noisy, w, h, w).unwrap()).unwrap();
        assert!(hidden < banded / 4.0, "{hidden} against {banded}");
        assert!(cambi(&Plane::new(&flat[..100 * 100], 100, 100, 100).unwrap()).is_none());
    }

    #[test]
    fn smoothing_is_a_rounded_box_mean_that_strips_dither_into_steps() {
        let (w, h) = (9, 7);
        let picture = plane(w, h, |x, y| (x * 10 + y) as u8);
        let smooth = smoothed(&Plane::new(&picture, w, h, w).unwrap());
        let box_mean = |x: usize, y: usize| {
            let (xs, ys) = (x.saturating_sub(2)..=(x + 2).min(w - 1), y.saturating_sub(2)..=(y + 2).min(h - 1));
            let (mut sum, mut count) = (0.0, 0.0);
            for yy in ys.clone() {
                for xx in xs.clone() {
                    sum += picture[yy * w + xx] as f64;
                    count += 1.0;
                }
            }
            (sum / count).round() as u8
        };
        for y in 0..h {
            for x in 0..w {
                assert!((smooth[y * w + x] as i32 - box_mean(x, y) as i32).abs() <= 1, "{x},{y}");
            }
        }
        // A dark gradient held smooth by dither reads clean, and in steps once smoothed.
        let (w, h) = (480, 360);
        let dithered = plane(w, h, |x, y| {
            let level = 30.0 + 6.0 * x as f64 / w as f64;
            (level.floor() + if ((x * 7 + y * 13) % 10) as f64 / 10.0 < level.fract() { 1.0 } else { 0.0 }) as u8
        });
        let source = cambi(&Plane::new(&dithered, w, h, w).unwrap()).unwrap();
        let smooth = smoothed(&Plane::new(&dithered, w, h, w).unwrap());
        let exposed = cambi(&Plane::new(&smooth, w, h, w).unwrap()).unwrap();
        assert!(exposed > source + 1.0, "{source} {exposed}");
    }
}
