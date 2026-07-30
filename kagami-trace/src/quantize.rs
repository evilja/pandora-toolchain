use std::collections::HashMap;

use crate::Color;

const MAX_EXACT_HISTOGRAM_COLORS: usize = 8_192;

#[derive(Clone, Copy)]
struct PerceptualColor {
    l: f64,
    a: f64,
    b: f64,
    alpha: f64,
}

struct HistogramBin {
    key: u32,
    count: u64,
    sum_r: u64,
    sum_g: u64,
    sum_b: u64,
    sum_a: u64,
}

struct Sample {
    key: u32,
    count: u64,
    sum_r: u64,
    sum_g: u64,
    sum_b: u64,
    sum_a: u64,
    color: PerceptualColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistogramPrecision {
    Exact,
    Reduced,
}

pub(crate) struct Quantized {
    pub(crate) palette: Vec<Color>,
    labels_by_key: HashMap<u32, u16>,
    precision: HistogramPrecision,
}

impl Quantized {
    pub(crate) fn label(&self, color: Color) -> Option<u16> {
        self.labels_by_key
            .get(&color_key(color, self.precision))
            .copied()
    }
}

fn color_key(color: Color, precision: HistogramPrecision) -> u32 {
    match precision {
        HistogramPrecision::Exact => {
            ((color.r as u32) << 24)
                | ((color.g as u32) << 16)
                | ((color.b as u32) << 8)
                | color.a as u32
        }
        HistogramPrecision::Reduced => {
            ((color.r as u32 >> 3) << 14)
                | ((color.g as u32 >> 3) << 9)
                | ((color.b as u32 >> 3) << 4)
                | (color.a as u32 >> 4)
        }
    }
}

fn build_histogram(
    pixels: &[u8],
    alpha_threshold: u8,
    precision: HistogramPrecision,
    color_limit: Option<usize>,
) -> Option<HashMap<u32, HistogramBin>> {
    let mut bins: HashMap<u32, HistogramBin> = HashMap::new();
    for rgba in pixels.chunks_exact(4) {
        if rgba[3] <= alpha_threshold {
            continue;
        }
        let color = Color {
            r: rgba[0],
            g: rgba[1],
            b: rgba[2],
            a: rgba[3],
        };
        let key = color_key(color, precision);
        if color_limit.is_some_and(|limit| bins.len() >= limit) && !bins.contains_key(&key) {
            return None;
        }
        let bin = bins.entry(key).or_insert(HistogramBin {
            key,
            count: 0,
            sum_r: 0,
            sum_g: 0,
            sum_b: 0,
            sum_a: 0,
        });
        bin.count += 1;
        bin.sum_r += color.r as u64;
        bin.sum_g += color.g as u64;
        bin.sum_b += color.b as u64;
        bin.sum_a += color.a as u64;
    }
    Some(bins)
}

pub(crate) fn quantize(
    pixels: &[u8],
    alpha_threshold: u8,
    color_count: usize,
    preserve_gradients: bool,
) -> Quantized {
    let exact = preserve_gradients.then(|| {
        build_histogram(
            pixels,
            alpha_threshold,
            HistogramPrecision::Exact,
            Some(MAX_EXACT_HISTOGRAM_COLORS),
        )
    });
    let (bins, precision) = match exact.flatten() {
        Some(bins) => (bins, HistogramPrecision::Exact),
        None => (
            build_histogram(pixels, alpha_threshold, HistogramPrecision::Reduced, None).unwrap(),
            HistogramPrecision::Reduced,
        ),
    };

    let mut bins: Vec<HistogramBin> = bins.into_values().collect();
    bins.sort_by_key(|bin| bin.key);
    if bins.is_empty() {
        return Quantized {
            palette: Vec::new(),
            labels_by_key: HashMap::new(),
            precision,
        };
    }

    let samples: Vec<Sample> = bins
        .iter()
        .map(|bin| {
            let count = bin.count as f64;
            let color = Color {
                r: (bin.sum_r as f64 / count).round() as u8,
                g: (bin.sum_g as f64 / count).round() as u8,
                b: (bin.sum_b as f64 / count).round() as u8,
                a: (bin.sum_a as f64 / count).round() as u8,
            };
            Sample {
                key: bin.key,
                count: bin.count,
                sum_r: bin.sum_r,
                sum_g: bin.sum_g,
                sum_b: bin.sum_b,
                sum_a: bin.sum_a,
                color: rgba_to_perceptual(color),
            }
        })
        .collect();

    let target = color_count.min(samples.len()).max(1);
    let mut centroids = initial_centroids(&samples, target);
    let mut assignments = vec![0usize; samples.len()];

    for _ in 0..10 {
        let mut changed = false;
        for (index, sample) in samples.iter().enumerate() {
            let label = nearest(sample.color, &centroids);
            changed |= assignments[index] != label;
            assignments[index] = label;
        }

        let mut weight = vec![0.0f64; target];
        let mut sum_l = vec![0.0f64; target];
        let mut sum_a = vec![0.0f64; target];
        let mut sum_b = vec![0.0f64; target];
        let mut sum_alpha = vec![0.0f64; target];
        for (sample, label) in samples.iter().zip(assignments.iter().copied()) {
            let w = sample.count as f64;
            weight[label] += w;
            sum_l[label] += sample.color.l * w;
            sum_a[label] += sample.color.a * w;
            sum_b[label] += sample.color.b * w;
            sum_alpha[label] += sample.color.alpha * w;
        }
        for index in 0..target {
            if weight[index] > 0.0 {
                centroids[index] = PerceptualColor {
                    l: sum_l[index] / weight[index],
                    a: sum_a[index] / weight[index],
                    b: sum_b[index] / weight[index],
                    alpha: sum_alpha[index] / weight[index],
                };
            }
        }
        if !changed {
            break;
        }
    }

    for (index, sample) in samples.iter().enumerate() {
        assignments[index] = nearest(sample.color, &centroids);
    }

    let palette = source_space_palette(&samples, &assignments, &centroids);
    let labels_by_key = samples
        .iter()
        .zip(assignments)
        .map(|(sample, label)| (sample.key, label as u16))
        .collect();
    Quantized {
        palette,
        labels_by_key,
        precision,
    }
}

fn source_space_palette(
    samples: &[Sample],
    assignments: &[usize],
    centroids: &[PerceptualColor],
) -> Vec<Color> {
    let mut weights = vec![0u64; centroids.len()];
    let mut red = vec![0u64; centroids.len()];
    let mut green = vec![0u64; centroids.len()];
    let mut blue = vec![0u64; centroids.len()];
    let mut alpha = vec![0u64; centroids.len()];
    for (sample, label) in samples.iter().zip(assignments.iter().copied()) {
        weights[label] += sample.count;
        red[label] += sample.sum_r;
        green[label] += sample.sum_g;
        blue[label] += sample.sum_b;
        alpha[label] += sample.sum_a;
    }
    centroids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, centroid)| {
            let weight = weights[index];
            if weight == 0 {
                return perceptual_to_rgba(centroid);
            }
            Color {
                r: ((red[index] + weight / 2) / weight) as u8,
                g: ((green[index] + weight / 2) / weight) as u8,
                b: ((blue[index] + weight / 2) / weight) as u8,
                a: ((alpha[index] + weight / 2) / weight) as u8,
            }
        })
        .collect()
}

fn initial_centroids(samples: &[Sample], count: usize) -> Vec<PerceptualColor> {
    let first = samples
        .iter()
        .enumerate()
        .max_by_key(|(_, sample)| sample.count)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let mut chosen = vec![false; samples.len()];
    chosen[first] = true;
    let mut out = vec![samples[first].color];
    let mut nearest_distances: Vec<f64> = samples
        .iter()
        .map(|sample| perceptual_distance(sample.color, samples[first].color))
        .collect();

    while out.len() < count {
        let mut best_index = None;
        let mut best_score = -1.0f64;
        for (index, sample) in samples.iter().enumerate() {
            if chosen[index] {
                continue;
            }
            let score = nearest_distances[index] * (sample.count as f64).sqrt();
            if score > best_score {
                best_score = score;
                best_index = Some(index);
            }
        }
        let Some(index) = best_index else {
            break;
        };
        chosen[index] = true;
        let centroid = samples[index].color;
        out.push(centroid);
        for (sample, distance) in samples.iter().zip(&mut nearest_distances) {
            *distance = distance.min(perceptual_distance(sample.color, centroid));
        }
    }
    out
}

fn nearest(color: PerceptualColor, centroids: &[PerceptualColor]) -> usize {
    let mut best = 0usize;
    let mut best_distance = f64::INFINITY;
    for (index, centroid) in centroids.iter().copied().enumerate() {
        let distance = perceptual_distance(color, centroid);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best
}

fn perceptual_distance(left: PerceptualColor, right: PerceptualColor) -> f64 {
    let dl = left.l - right.l;
    let da = left.a - right.a;
    let db = left.b - right.b;
    let alpha = (left.alpha - right.alpha) * 0.35;
    dl * dl + da * da + db * db + alpha * alpha
}

fn rgba_to_perceptual(color: Color) -> PerceptualColor {
    let r = srgb_to_linear(color.r as f64 / 255.0);
    let g = srgb_to_linear(color.g as f64 / 255.0);
    let b = srgb_to_linear(color.b as f64 / 255.0);

    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();

    PerceptualColor {
        l: 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        a: 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        b: 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
        alpha: color.a as f64 / 255.0,
    }
}

fn perceptual_to_rgba(color: PerceptualColor) -> Color {
    let l = color.l + 0.396_337_777_4 * color.a + 0.215_803_757_3 * color.b;
    let m = color.l - 0.105_561_345_8 * color.a - 0.063_854_172_8 * color.b;
    let s = color.l - 0.089_484_177_5 * color.a - 1.291_485_548 * color.b;
    let l = l * l * l;
    let m = m * m * m;
    let s = s * s * s;

    let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s;

    Color {
        r: unit_to_u8(linear_to_srgb(r)),
        g: unit_to_u8(linear_to_srgb(g)),
        b: unit_to_u8(linear_to_srgb(b)),
        a: unit_to_u8(color.alpha),
    }
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> f64 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

fn unit_to_u8(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn subtle_ramps_keep_exact_color_steps() {
        let mut pixels = Vec::new();
        let mut colors = Vec::new();
        for value in 80..96 {
            let color = Color {
                r: value,
                g: value + 2,
                b: value - 3,
                a: 255,
            };
            colors.push(color);
            pixels.extend([color.r, color.g, color.b, color.a]);
        }

        let quantized = quantize(&pixels, 0, colors.len(), true);
        let labels: HashSet<u16> = colors
            .iter()
            .map(|color| quantized.label(*color).unwrap())
            .collect();

        assert_eq!(quantized.precision, HistogramPrecision::Exact);
        assert_eq!(quantized.palette.len(), colors.len());
        assert_eq!(labels.len(), colors.len());
        assert_eq!(
            quantized.palette.iter().copied().collect::<HashSet<_>>(),
            colors.iter().copied().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn clustered_palette_uses_the_weighted_source_space_mean() {
        let pixels = [
            10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 20, 40, 60, 255,
        ];
        let quantized = quantize(&pixels, 0, 1, true);

        assert_eq!(
            quantized.palette,
            vec![Color {
                r: 13,
                g: 25,
                b: 38,
                a: 255,
            }]
        );
    }

    #[test]
    fn standard_sampling_keeps_the_noise_resistant_histogram() {
        let mut pixels = Vec::new();
        let mut colors = Vec::new();
        for value in 80..96 {
            let color = Color {
                r: value,
                g: value + 2,
                b: value - 3,
                a: 255,
            };
            colors.push(color);
            pixels.extend([color.r, color.g, color.b, color.a]);
        }

        let quantized = quantize(&pixels, 0, colors.len(), false);
        let labels: HashSet<u16> = colors
            .iter()
            .map(|color| quantized.label(*color).unwrap())
            .collect();

        assert_eq!(quantized.precision, HistogramPrecision::Reduced);
        assert!(quantized.palette.len() < colors.len());
        assert!(labels.len() < colors.len());
    }

    #[test]
    fn complex_images_fall_back_to_a_bounded_histogram() {
        let mut pixels = Vec::new();
        let mut colors = Vec::new();
        for index in 0..=MAX_EXACT_HISTOGRAM_COLORS {
            let color = Color {
                r: (index / 256) as u8,
                g: index as u8,
                b: 0,
                a: 255,
            };
            colors.push(color);
            pixels.extend([color.r, color.g, color.b, color.a]);
        }

        let quantized = quantize(&pixels, 0, 16, true);

        assert_eq!(quantized.precision, HistogramPrecision::Reduced);
        assert!(colors.iter().all(|color| quantized.label(*color).is_some()));
    }
}
