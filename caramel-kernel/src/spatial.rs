// Per-frame spatial features, all gathered in one pass over the luma plane.
//
// The pass evaluates every second pixel of every second row. The 3×3 operators still read the full
// neighbourhood around each evaluated pixel, so a gradient or a Laplacian there is exact; only the
// set of pixels averaged over is thinned, which costs a quarter of the work and changes the
// statistics by sampling noise.

use crate::frame::Plane;
use serde::{Deserialize, Serialize};

// Side of the square blocks the banding and line-art classification works on, in luma pixels.
pub const BLOCK: usize = 16;

// A Sobel magnitude above this is an edge: a clean step of roughly 16 code values.
const EDGE_MAGNITUDE: f32 = 64.0;
// A Sobel magnitude below this is a flat neighbourhood: under about two code values of slope.
const LOW_GRADIENT: f32 = 8.0;
// A block whose luma variance is below this has no texture of its own (σ ≈ 2.4). A linear ramp of
// eight code values across one block still qualifies, which is exactly the content that bands.
const SMOOTH_VARIANCE: f64 = 6.0;
// Two neighbouring smooth blocks whose means differ by this much are one smooth gradient: less is a
// flat fill, which encodes cleanly at any CRF, and more is a real boundary.
const GRADIENT_STEP_MIN: f64 = 0.5;
const GRADIENT_STEP_MAX: f64 = 8.0;
// Below this mean the block is dark, where banding and blocking are most visible on a typical
// display (limited-range black is 16).
const DARK_LUMA: f64 = 64.0;
// A block is line art when a small share of it is hard edge and a large share is flat: cel outlines
// over fills, where ringing and mosquito noise show first.
const LINE_ART_EDGE_MIN: f64 = 0.03;
const LINE_ART_EDGE_MAX: f64 = 0.5;
const LINE_ART_FLAT_MIN: f64 = 0.35;
// Share of the frame, lowest gradient first, whose Laplacian response is read as noise.
const NOISE_SAMPLE_SHARE: f64 = 0.4;
const NOISE_BINS: usize = 256;
// The noise floors the fine detail is weighed against, in code values. An encoder at a given
// quantiser adds noise of some such level everywhere and removes whatever detail lies under it, so
// what a frame loses is the part of its detail below the floor. The floors span what CRF 10–30 does.
pub const DETAIL_FLOORS: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];
// The Laplacian of unit noise has a deviation of 6, and its largest counted response is past the
// highest floor.
const LAPLACIAN_GAIN: f64 = 6.0;
const LAPLACIAN_BINS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct SpatialFeatures {
    // ITU-T P.910 spatial information: the standard deviation of the Sobel magnitude.
    pub si: f64,
    // Share of the frame that is hard edge.
    pub edge_density: f64,
    // Estimated noise/grain standard deviation in code values.
    pub noise_sigma: f64,
    // Shares of the frame's blocks.
    pub smooth_share: f64,
    pub gradient_share: f64,
    pub dark_gradient_share: f64,
    pub line_art_share: f64,
    pub dark_share: f64,
    pub mean_luma: f64,
    // Fine detail below each of `DETAIL_FLOORS`: the mean of min(h², floor²) over the frame, h being
    // the high-pass response in code values. Flat fills and already-compressed pictures have almost
    // none at any floor; grain and texture fill each floor to its square.
    pub detail_energy: [f64; 5],
}

#[derive(Clone, Copy, Default)]
struct BlockAccumulator {
    samples: u32,
    sum: f64,
    sum_squares: f64,
    edges: u32,
    flat: u32,
}

impl BlockAccumulator {
    fn mean(&self) -> f64 {
        self.sum / self.samples as f64
    }

    fn variance(&self) -> f64 {
        let mean = self.mean();
        (self.sum_squares / self.samples as f64 - mean * mean).max(0.0)
    }
}

// Sobel x/y and the Immerkær Laplacian at (x, y), reading the rows above, at and below.
#[inline]
fn operators(above: &[u8], row: &[u8], below: &[u8], x: usize) -> (i32, i32, i32) {
    let (tl, t, tr) = (above[x - 1] as i32, above[x] as i32, above[x + 1] as i32);
    let (ml, c, mr) = (row[x - 1] as i32, row[x] as i32, row[x + 1] as i32);
    let (bl, b, br) = (below[x - 1] as i32, below[x] as i32, below[x + 1] as i32);
    let gx = (tr + 2 * mr + br) - (tl + 2 * ml + bl);
    let gy = (bl + 2 * b + br) - (tl + 2 * t + tr);
    // [[1,-2,1],[-2,4,-2],[1,-2,1]]: blind to constant, linear and most of the quadratic structure
    // of a picture, so what remains is mostly noise.
    let lap = (tl - 2 * t + tr) - 2 * (ml - 2 * c + mr) + (bl - 2 * b + br);
    (gx, gy, lap)
}

// Every spatial feature of one luma plane.
pub fn analyze_luma(plane: &Plane) -> SpatialFeatures {
    let (width, height) = (plane.width, plane.height);
    if width < 3 || height < 3 {
        return SpatialFeatures::default();
    }
    let blocks_x = width.div_ceil(BLOCK);
    let blocks_y = height.div_ceil(BLOCK);
    let mut blocks = vec![BlockAccumulator::default(); blocks_x * blocks_y];
    let mut noise_count = [0u32; NOISE_BINS];
    let mut noise_sum = [0f64; NOISE_BINS];
    let (mut samples, mut edges) = (0u64, 0u64);
    let (mut magnitude_sum, mut magnitude_squares, mut luma_sum) = (0f64, 0f64, 0f64);
    let mut laplacian_count = [0u32; LAPLACIAN_BINS];

    let mut y = 1;
    while y + 1 < height {
        let (above, row, below) = (plane.row(y - 1), plane.row(y), plane.row(y + 1));
        let block_row = (y / BLOCK) * blocks_x;
        let mut x = 1;
        while x + 1 < width {
            let (gx, gy, lap) = operators(above, row, below, x);
            let magnitude = ((gx * gx + gy * gy) as f32).sqrt();
            let value = row[x] as f64;
            samples += 1;
            magnitude_sum += magnitude as f64;
            magnitude_squares += (magnitude as f64) * (magnitude as f64);
            luma_sum += value;
            let block = &mut blocks[block_row + x / BLOCK];
            block.samples += 1;
            block.sum += value;
            block.sum_squares += value * value;
            if magnitude > EDGE_MAGNITUDE {
                edges += 1;
                block.edges += 1;
            } else if magnitude < LOW_GRADIENT {
                block.flat += 1;
            }
            let bin = ((magnitude * 0.5) as usize).min(NOISE_BINS - 1);
            noise_count[bin] += 1;
            noise_sum[bin] += lap.unsigned_abs() as f64;
            laplacian_count[(lap.unsigned_abs() as usize).min(LAPLACIAN_BINS - 1)] += 1;
            x += 2;
        }
        y += 2;
    }
    if samples == 0 {
        return SpatialFeatures::default();
    }
    let n = samples as f64;
    let mean_magnitude = magnitude_sum / n;
    let si = (magnitude_squares / n - mean_magnitude * mean_magnitude).max(0.0).sqrt();

    let mut features = SpatialFeatures {
        si,
        edge_density: edges as f64 / n,
        noise_sigma: noise_sigma(&noise_count, &noise_sum, samples),
        mean_luma: luma_sum / n,
        detail_energy: DETAIL_FLOORS.map(|floor| {
            let limit = (floor * LAPLACIAN_GAIN).powi(2);
            let sum: f64 = laplacian_count
                .iter()
                .enumerate()
                .map(|(response, &count)| count as f64 * ((response * response) as f64).min(limit))
                .sum();
            sum / n / (LAPLACIAN_GAIN * LAPLACIAN_GAIN)
        }),
        ..Default::default()
    };
    classify_blocks(&blocks, blocks_x, blocks_y, &mut features);
    features
}

// Immerkær (1996) fast noise variance estimation, restricted to the flattest share of the frame so
// that edges and texture do not read as grain. Selecting by Sobel magnitude does not bias the
// result on pure noise: the Sobel and Laplacian kernels have opposite symmetry, so their responses
// to white noise are uncorrelated (the tests hold it within a few percent of the true σ).
fn noise_sigma(count: &[u32; NOISE_BINS], sum: &[f64; NOISE_BINS], samples: u64) -> f64 {
    let wanted = (samples as f64 * NOISE_SAMPLE_SHARE).max(1.0);
    let (mut taken, mut response) = (0f64, 0f64);
    for bin in 0..NOISE_BINS {
        if count[bin] == 0 {
            continue;
        }
        let available = count[bin] as f64;
        let take = available.min(wanted - taken);
        response += sum[bin] * (take / available);
        taken += take;
        if taken >= wanted {
            break;
        }
    }
    if taken == 0.0 {
        return 0.0;
    }
    (std::f64::consts::PI / 2.0).sqrt() / 6.0 * (response / taken)
}

fn classify_blocks(
    blocks: &[BlockAccumulator],
    blocks_x: usize,
    blocks_y: usize,
    features: &mut SpatialFeatures,
) {
    let smooth = |block: &BlockAccumulator| block.samples > 0 && block.variance() < SMOOTH_VARIANCE;
    let (mut total, mut smooth_blocks, mut gradient, mut dark_gradient, mut line_art, mut dark) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let block = &blocks[by * blocks_x + bx];
            if block.samples == 0 {
                continue;
            }
            total += 1;
            let mean = block.mean();
            let is_dark = mean < DARK_LUMA;
            if is_dark {
                dark += 1;
            }
            let edge_share = block.edges as f64 / block.samples as f64;
            let flat_share = block.flat as f64 / block.samples as f64;
            if (LINE_ART_EDGE_MIN..=LINE_ART_EDGE_MAX).contains(&edge_share)
                && flat_share >= LINE_ART_FLAT_MIN
            {
                line_art += 1;
            }
            if !smooth(block) {
                continue;
            }
            smooth_blocks += 1;
            let neighbours = [
                (bx + 1 < blocks_x).then(|| &blocks[by * blocks_x + bx + 1]),
                (by + 1 < blocks_y).then(|| &blocks[(by + 1) * blocks_x + bx]),
                (bx > 0).then(|| &blocks[by * blocks_x + bx - 1]),
                (by > 0).then(|| &blocks[(by - 1) * blocks_x + bx]),
            ];
            let in_gradient = neighbours.into_iter().flatten().any(|other| {
                smooth(other)
                    && (GRADIENT_STEP_MIN..=GRADIENT_STEP_MAX).contains(&(other.mean() - mean).abs())
            });
            if in_gradient {
                gradient += 1;
                if is_dark {
                    dark_gradient += 1;
                }
            }
        }
    }
    if total == 0 {
        return;
    }
    let total = total as f64;
    features.smooth_share = smooth_blocks as f64 / total;
    features.gradient_share = gradient as f64 / total;
    features.dark_gradient_share = dark_gradient as f64 / total;
    features.line_art_share = line_art as f64 / total;
    features.dark_share = dark as f64 / total;
}

// P.910-style spatial information alone, for the chroma planes.
pub fn sobel_si(plane: &Plane) -> f64 {
    let (width, height) = (plane.width, plane.height);
    if width < 3 || height < 3 {
        return 0.0;
    }
    let (mut n, mut sum, mut squares) = (0u64, 0f64, 0f64);
    let mut y = 1;
    while y + 1 < height {
        let (above, row, below) = (plane.row(y - 1), plane.row(y), plane.row(y + 1));
        let mut x = 1;
        while x + 1 < width {
            let (gx, gy, _) = operators(above, row, below, x);
            let magnitude = ((gx * gx + gy * gy) as f64).sqrt();
            n += 1;
            sum += magnitude;
            squares += magnitude * magnitude;
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Deterministic xorshift noise, approximately Gaussian by summing uniforms.
    pub(crate) struct Noise(u64);

    impl Noise {
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed.max(1))
        }

        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        // Twelve uniforms minus six: mean 0, variance 1.
        pub(crate) fn gaussian(&mut self) -> f64 {
            (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
        }
    }

    pub(crate) fn picture(width: usize, height: usize, f: impl Fn(usize, usize) -> f64) -> Vec<u8> {
        let mut data = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                data[y * width + x] = f(x, y).round().clamp(0.0, 255.0) as u8;
            }
        }
        data
    }

    fn analyze(data: &[u8], width: usize, height: usize) -> SpatialFeatures {
        analyze_luma(&Plane::new(data, width, height, width).unwrap())
    }

    #[test]
    fn a_flat_frame_has_no_detail_noise_or_gradient() {
        let data = vec![128u8; 320 * 192];
        let features = analyze(&data, 320, 192);
        assert_eq!(features.si, 0.0);
        assert_eq!(features.edge_density, 0.0);
        assert_eq!(features.noise_sigma, 0.0);
        assert_eq!(features.gradient_share, 0.0);
        assert_eq!(features.smooth_share, 1.0);
    }

    fn noisy(sigma: f64, base: impl Fn(usize, usize) -> f64) -> Vec<u8> {
        let mut noise = Noise::new(7);
        let values: Vec<f64> = (0..320 * 192).map(|_| sigma * noise.gaussian()).collect();
        picture(320, 192, |x, y| base(x, y) + values[y * 320 + x])
    }

    // The estimator should land near the true σ across the range that separates clean animation
    // from heavy film grain; the model's thresholds are written in those units.
    #[test]
    fn the_noise_estimate_tracks_the_true_sigma() {
        for sigma in [1.0, 2.0, 4.0, 6.0] {
            let estimate = analyze(&noisy(sigma, |_, _| 128.0), 320, 192).noise_sigma;
            assert!((estimate / sigma - 1.0).abs() < 0.1, "σ={sigma} estimated as {estimate}");
        }
    }

    // Picture structure must not read as grain: the same noise over edges and a ramp.
    #[test]
    fn structure_under_the_grain_does_not_inflate_it() {
        let base = |x: usize, y: usize| {
            let cel = if x % 48 < 2 || y % 48 < 2 { 16.0 } else { 180.0 };
            cel + x as f64 / 8.0
        };
        assert!(analyze(&picture(320, 192, base), 320, 192).noise_sigma < 0.3);
        let estimate = analyze(&noisy(2.0, base), 320, 192).noise_sigma;
        assert!((estimate / 2.0 - 1.0).abs() < 0.2, "estimated as {estimate}");
    }

    #[test]
    fn a_smooth_ramp_reads_as_gradient_and_a_dark_one_as_dark_gradient() {
        // 40 code values over the width: about two per 16-pixel block.
        let bright = picture(320, 192, |x, _| 140.0 + x as f64 / 8.0);
        let features = analyze(&bright, 320, 192);
        assert!(features.gradient_share > 0.9, "{features:?}");
        assert_eq!(features.dark_gradient_share, 0.0);

        let dark = picture(320, 192, |x, _| 20.0 + x as f64 / 8.0);
        let features = analyze(&dark, 320, 192);
        assert!(features.dark_gradient_share > 0.5, "{features:?}");
    }

    #[test]
    fn outlines_over_flat_fills_read_as_line_art() {
        // A 2-pixel black grid every 24 pixels over a flat fill: cel outlines.
        let data = picture(320, 192, |x, y| if x % 24 < 2 || y % 24 < 2 { 16.0 } else { 200.0 });
        let features = analyze(&data, 320, 192);
        assert!(features.line_art_share > 0.5, "{features:?}");
        assert!(features.edge_density > 0.05);
        assert_eq!(features.noise_sigma, 0.0);
    }

    #[test]
    fn texture_raises_spatial_information_and_is_not_smooth() {
        let data = picture(320, 192, |x, y| 128.0 + 60.0 * ((x as f64 * 0.9).sin() * (y as f64 * 0.7).cos()));
        let features = analyze(&data, 320, 192);
        assert!(features.si > 20.0, "{features:?}");
        assert!(features.smooth_share < 0.1);
    }
}
