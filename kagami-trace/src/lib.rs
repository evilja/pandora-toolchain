mod contour;
mod model;
mod options;
mod quantize;
mod svg;

#[cfg(feature = "decode")]
mod decode;

use std::fmt;

pub use model::{Color, Path, Point, Segment, TRACE_SCHEMA_VERSION, Trace, TraceLayer};
pub use options::{MAX_COLOR_COUNT, TraceOptions, TracePreset};

#[cfg(feature = "decode")]
pub use decode::trace_image;

pub const MAX_IMAGE_DIMENSION: u32 = 8192;
pub const MAX_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_ENCODED_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const TRANSPARENT_LABEL: u16 = u16::MAX;

#[derive(Debug)]
pub enum TraceError {
    Decode(String),
    Dimensions(String),
    InvalidOptions(String),
    PixelLength { expected: usize, actual: usize },
    TooLarge(String),
}

impl fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceError::Decode(message) => write!(formatter, "image decode failed: {message}"),
            TraceError::Dimensions(message) => {
                write!(formatter, "invalid image dimensions: {message}")
            }
            TraceError::InvalidOptions(message) => {
                write!(formatter, "invalid trace options: {message}")
            }
            TraceError::PixelLength { expected, actual } => {
                write!(
                    formatter,
                    "RGBA pixel data is {actual} bytes; expected {expected}"
                )
            }
            TraceError::TooLarge(message) => {
                write!(formatter, "image input is too large: {message}")
            }
        }
    }
}

impl std::error::Error for TraceError {}

pub type TraceResult<T> = Result<T, TraceError>;

pub fn trace_rgba(
    width: u32,
    height: u32,
    pixels: &[u8],
    options: &TraceOptions,
) -> TraceResult<Trace> {
    options.validate().map_err(TraceError::InvalidOptions)?;
    validate_dimensions(width, height)?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| TraceError::TooLarge("RGBA byte length overflowed".to_string()))?;
    if pixels.len() != expected {
        return Err(TraceError::PixelLength {
            expected,
            actual: pixels.len(),
        });
    }

    let (sampled_width, sampled_height) = sampled_dimensions(width, height, options.max_dimension);
    let resized = if sampled_width != width || sampled_height != height {
        Some(resize_rgba(
            pixels,
            width,
            height,
            sampled_width,
            sampled_height,
        ))
    } else {
        None
    };
    let sampled_pixels = resized.as_deref().unwrap_or(pixels);
    let smoothed = if options.color_smoothing > 0 {
        Some(edge_aware_smooth_rgba(
            sampled_pixels,
            sampled_width,
            sampled_height,
            options.color_smoothing,
            options.alpha_threshold,
        ))
    } else {
        None
    };
    let trace_pixels = smoothed.as_deref().unwrap_or(sampled_pixels);
    let quantized = quantize::quantize(
        trace_pixels,
        options.alpha_threshold,
        options.color_count as usize,
        options.preserve_gradients,
    );
    if quantized.palette.is_empty() {
        return Ok(Trace::new(
            width,
            height,
            sampled_width,
            sampled_height,
            Vec::new(),
            Vec::new(),
        ));
    }

    let mut labels = labels_from_pixels(trace_pixels, options.alpha_threshold, &quantized);
    despeckle(
        &mut labels,
        sampled_width,
        sampled_height,
        options.min_area,
        &quantized.palette,
    );
    let (palette, counts) = compact_labels(&mut labels, quantized.palette);
    if palette.is_empty() {
        return Ok(Trace::new(
            width,
            height,
            sampled_width,
            sampled_height,
            Vec::new(),
            Vec::new(),
        ));
    }

    let paths = contour::paths_for_labels(
        &labels,
        sampled_width,
        sampled_height,
        palette.len(),
        width,
        height,
        options.path_simplify,
        options.curve_fit,
        options.corner_threshold,
        options.preserve_gradients,
    );
    let layers = paths
        .into_iter()
        .enumerate()
        .filter_map(|(index, paths)| {
            (!paths.is_empty()).then_some(TraceLayer {
                palette_index: index as u16,
                sample_pixel_count: counts[index],
                paths,
            })
        })
        .collect();

    Ok(Trace::new(
        width,
        height,
        sampled_width,
        sampled_height,
        palette,
        layers,
    ))
}

fn validate_dimensions(width: u32, height: u32) -> TraceResult<()> {
    if width == 0 || height == 0 {
        return Err(TraceError::Dimensions(
            "width and height must be non-zero".to_string(),
        ));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(TraceError::TooLarge(format!(
            "{}x{} exceeds the {}px side limit",
            width, height, MAX_IMAGE_DIMENSION
        )));
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(TraceError::TooLarge(format!(
            "{} pixels exceeds the {} pixel limit",
            pixels, MAX_IMAGE_PIXELS
        )));
    }
    Ok(())
}

fn sampled_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if max_dimension == 0 || width.max(height) <= max_dimension {
        return (width, height);
    }
    let scale = max_dimension as f64 / width.max(height) as f64;
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

fn resize_rgba(
    source: &[u8],
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let mut output = vec![0u8; target_width as usize * target_height as usize * 4];
    let scale_x = width as f64 / target_width as f64;
    let scale_y = height as f64 / target_height as f64;

    for target_y in 0..target_height {
        let source_y =
            ((target_y as f64 + 0.5) * scale_y - 0.5).clamp(0.0, height.saturating_sub(1) as f64);
        let y0 = source_y.floor() as u32;
        let y1 = (y0 + 1).min(height - 1);
        let fy = source_y - y0 as f64;
        for target_x in 0..target_width {
            let source_x = ((target_x as f64 + 0.5) * scale_x - 0.5)
                .clamp(0.0, width.saturating_sub(1) as f64);
            let x0 = source_x.floor() as u32;
            let x1 = (x0 + 1).min(width - 1);
            let fx = source_x - x0 as f64;
            let weights = [
                ((1.0 - fx) * (1.0 - fy), x0, y0),
                (fx * (1.0 - fy), x1, y0),
                ((1.0 - fx) * fy, x0, y1),
                (fx * fy, x1, y1),
            ];
            let mut alpha = 0.0f64;
            let mut red = 0.0f64;
            let mut green = 0.0f64;
            let mut blue = 0.0f64;
            for (weight, x, y) in weights {
                let offset = ((y * width + x) * 4) as usize;
                let pixel_alpha = source[offset + 3] as f64 / 255.0;
                alpha += pixel_alpha * weight;
                red += source[offset] as f64 * pixel_alpha * weight;
                green += source[offset + 1] as f64 * pixel_alpha * weight;
                blue += source[offset + 2] as f64 * pixel_alpha * weight;
            }
            let offset = ((target_y * target_width + target_x) * 4) as usize;
            if alpha > f64::EPSILON {
                output[offset] = (red / alpha).clamp(0.0, 255.0).round() as u8;
                output[offset + 1] = (green / alpha).clamp(0.0, 255.0).round() as u8;
                output[offset + 2] = (blue / alpha).clamp(0.0, 255.0).round() as u8;
                output[offset + 3] = (alpha * 255.0).clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    output
}

fn edge_aware_smooth_rgba(
    source: &[u8],
    width: u32,
    height: u32,
    strength: u8,
    alpha_threshold: u8,
) -> Vec<u8> {
    let mut current = source.to_vec();
    let color_limit = 4i32 + strength as i32 * 4;
    let color_limit_squared = color_limit * color_limit;

    for _ in 0..strength {
        let mut output = current.clone();
        for y in 0..height {
            for x in 0..width {
                let center_offset = ((y * width + x) * 4) as usize;
                let center = &current[center_offset..center_offset + 4];
                if center[3] <= alpha_threshold {
                    continue;
                }
                let mut sums = [0u64; 4];
                let mut total_weight = 0u64;
                let min_y = y.saturating_sub(1);
                let max_y = (y + 1).min(height - 1);
                let min_x = x.saturating_sub(1);
                let max_x = (x + 1).min(width - 1);

                for neighbor_y in min_y..=max_y {
                    for neighbor_x in min_x..=max_x {
                        let offset = ((neighbor_y * width + neighbor_x) * 4) as usize;
                        let neighbor = &current[offset..offset + 4];
                        if neighbor[3] <= alpha_threshold {
                            continue;
                        }
                        let alpha_difference = center[3].abs_diff(neighbor[3]) as i32;
                        if alpha_difference > color_limit {
                            continue;
                        }
                        let red = center[0] as i32 - neighbor[0] as i32;
                        let green = center[1] as i32 - neighbor[1] as i32;
                        let blue = center[2] as i32 - neighbor[2] as i32;
                        let distance = red * red + green * green + blue * blue;
                        if distance > color_limit_squared {
                            continue;
                        }
                        let spatial_weight = if neighbor_x == x && neighbor_y == y {
                            4u64
                        } else if neighbor_x == x || neighbor_y == y {
                            2u64
                        } else {
                            1u64
                        };
                        let color_weight = 1u64
                            + ((color_limit_squared - distance) as u64 * 3
                                / color_limit_squared as u64);
                        let weight = spatial_weight * color_weight;
                        for channel in 0..4 {
                            sums[channel] += neighbor[channel] as u64 * weight;
                        }
                        total_weight += weight;
                    }
                }

                if total_weight > 0 {
                    for channel in 0..4 {
                        output[center_offset + channel] =
                            ((sums[channel] + total_weight / 2) / total_weight) as u8;
                    }
                }
            }
        }
        current = output;
    }
    current
}

fn labels_from_pixels(
    pixels: &[u8],
    alpha_threshold: u8,
    quantized: &quantize::Quantized,
) -> Vec<u16> {
    pixels
        .chunks_exact(4)
        .map(|rgba| {
            if rgba[3] <= alpha_threshold {
                return TRANSPARENT_LABEL;
            }
            let color = Color {
                r: rgba[0],
                g: rgba[1],
                b: rgba[2],
                a: rgba[3],
            };
            quantized.label(color).unwrap_or(TRANSPARENT_LABEL)
        })
        .collect()
}

fn despeckle(labels: &mut Vec<u16>, width: u32, height: u32, min_area: u32, palette: &[Color]) {
    if min_area <= 1 || labels.is_empty() {
        return;
    }
    let original = labels.clone();
    let mut visited = vec![false; labels.len()];
    let mut queue = Vec::new();
    let mut component = Vec::new();
    let mut neighbors = vec![0u32; palette.len()];

    for start in 0..original.len() {
        let label = original[start];
        if label == TRANSPARENT_LABEL || visited[start] {
            continue;
        }
        queue.clear();
        component.clear();
        neighbors.fill(0);
        visited[start] = true;
        queue.push(start);
        let mut cursor = 0usize;

        while cursor < queue.len() {
            let index = queue[cursor];
            cursor += 1;
            component.push(index);
            let x = index as u32 % width;
            let y = index as u32 / width;
            for neighbor in pixel_neighbors(x, y, width, height).into_iter().flatten() {
                let neighbor_label = original[neighbor];
                if neighbor_label == label {
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        queue.push(neighbor);
                    }
                } else if neighbor_label != TRANSPARENT_LABEL {
                    neighbors[neighbor_label as usize] += 1;
                }
            }
        }

        if component.len() >= min_area as usize {
            continue;
        }
        let replacement = neighbors
            .iter()
            .enumerate()
            .filter(|(index, count)| **count > 0 && *index != label as usize)
            .max_by(|(left_index, left_count), (right_index, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| {
                        color_distance(palette[label as usize], palette[*right_index]).total_cmp(
                            &color_distance(palette[label as usize], palette[*left_index]),
                        )
                    })
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, _)| index as u16)
            .unwrap_or(TRANSPARENT_LABEL);
        for index in component.iter().copied() {
            labels[index] = replacement;
        }
    }
}

fn pixel_neighbors(x: u32, y: u32, width: u32, height: u32) -> [Option<usize>; 4] {
    [
        (x > 0).then(|| (y * width + x - 1) as usize),
        (x + 1 < width).then(|| (y * width + x + 1) as usize),
        (y > 0).then(|| ((y - 1) * width + x) as usize),
        (y + 1 < height).then(|| ((y + 1) * width + x) as usize),
    ]
}

fn color_distance(left: Color, right: Color) -> f32 {
    let red = left.r as f32 - right.r as f32;
    let green = left.g as f32 - right.g as f32;
    let blue = left.b as f32 - right.b as f32;
    let alpha = (left.a as f32 - right.a as f32) * 0.5;
    red * red + green * green + blue * blue + alpha * alpha
}

fn compact_labels(labels: &mut [u16], palette: Vec<Color>) -> (Vec<Color>, Vec<u64>) {
    let mut counts = vec![0u64; palette.len()];
    for label in labels.iter().copied() {
        if label != TRANSPARENT_LABEL {
            counts[label as usize] += 1;
        }
    }
    let mut active: Vec<usize> = (0..palette.len())
        .filter(|index| counts[*index] > 0)
        .collect();
    active.sort_by(|left, right| {
        counts[*right]
            .cmp(&counts[*left])
            .then_with(|| color_sort_key(palette[*left]).cmp(&color_sort_key(palette[*right])))
    });

    let mut remap = vec![TRANSPARENT_LABEL; palette.len()];
    let mut compact_palette = Vec::with_capacity(active.len());
    let mut compact_counts = Vec::with_capacity(active.len());
    for (new_index, old_index) in active.into_iter().enumerate() {
        remap[old_index] = new_index as u16;
        compact_palette.push(palette[old_index]);
        compact_counts.push(counts[old_index]);
    }
    for label in labels {
        if *label != TRANSPARENT_LABEL {
            *label = remap[*label as usize];
        }
    }
    (compact_palette, compact_counts)
}

fn color_sort_key(color: Color) -> u32 {
    ((color.r as u32) << 24) | ((color.g as u32) << 16) | ((color.b as u32) << 8) | color.a as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> TraceOptions {
        TraceOptions {
            color_count: 4,
            preserve_gradients: false,
            color_smoothing: 0,
            path_simplify: 0.0,
            curve_fit: 0.0,
            corner_threshold: 55.0,
            min_area: 1,
            alpha_threshold: 0,
            max_dimension: 0,
        }
    }

    #[test]
    fn solid_rgba_traces_to_one_color_and_path() {
        let pixels = [255, 0, 0, 255].repeat(4);
        let trace = trace_rgba(2, 2, &pixels, &options()).unwrap();

        assert_eq!(trace.width, 2);
        assert_eq!(trace.palette.len(), 1);
        assert_eq!(trace.layers.len(), 1);
        assert_eq!(trace.path_count(), 1);
        assert_eq!(trace.layers[0].paths[0].area, 4.0);
        assert!(trace.to_svg().contains("#FF0000"));
        trace.validate().unwrap();
    }

    #[test]
    fn transparent_pixels_do_not_create_a_palette_entry() {
        let pixels = [0, 0, 0, 0].repeat(4);
        let trace = trace_rgba(2, 2, &pixels, &options()).unwrap();

        assert!(trace.palette.is_empty());
        assert!(trace.layers.is_empty());
    }

    #[test]
    fn trace_is_deterministic() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let first = trace_rgba(2, 2, &pixels, &options()).unwrap();
        let second = trace_rgba(2, 2, &pixels, &options()).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.to_json().unwrap(), second.to_json().unwrap());
    }

    #[test]
    fn subtle_gradient_steps_are_not_collapsed_by_the_histogram() {
        let mut pixels = Vec::new();
        for _ in 0..2 {
            for value in 80..112 {
                pixels.extend([value, value + 2, value - 3, 255]);
            }
        }
        let mut settings = options();
        settings.color_count = 32;
        settings.preserve_gradients = true;
        let trace = trace_rgba(32, 2, &pixels, &settings).unwrap();

        assert_eq!(trace.palette.len(), 32);
        assert_eq!(trace.layers.len(), 32);
    }

    #[test]
    fn trace_can_return_512_color_layers() {
        let mut pixels = Vec::new();
        for index in 0..MAX_COLOR_COUNT {
            pixels.extend([((index % 32) * 8) as u8, ((index / 32) * 8) as u8, 0, 255]);
        }
        let mut settings = options();
        settings.color_count = MAX_COLOR_COUNT;
        let trace = trace_rgba(MAX_COLOR_COUNT as u32, 1, &pixels, &settings).unwrap();

        assert_eq!(trace.palette.len(), MAX_COLOR_COUNT as usize);
        assert_eq!(trace.layers.len(), MAX_COLOR_COUNT as usize);
    }

    #[test]
    fn max_dimension_preserves_output_coordinate_space() {
        let pixels = [255, 0, 0, 255].repeat(16);
        let mut settings = options();
        settings.max_dimension = 2;
        let trace = trace_rgba(4, 4, &pixels, &settings).unwrap();

        assert_eq!((trace.sampled_width, trace.sampled_height), (2, 2));
        assert_eq!(trace.layers[0].paths[0].area, 16.0);
        assert!(
            trace.layers[0].paths[0]
                .segments
                .iter()
                .any(|segment| segment.end() == Point { x: 4.0, y: 4.0 })
        );
    }

    #[test]
    fn invalid_pixel_length_is_reported() {
        let error = trace_rgba(2, 2, &[0; 4], &options()).unwrap_err();
        assert!(matches!(
            error,
            TraceError::PixelLength {
                expected: 16,
                actual: 4
            }
        ));
    }

    #[test]
    fn edge_aware_smoothing_reduces_noise_without_crossing_hard_edges() {
        let noisy = [100, 100, 100, 255, 106, 106, 106, 255, 100, 100, 100, 255];
        let smoothed = edge_aware_smooth_rgba(&noisy, 3, 1, 2, 0);
        assert!(smoothed[4] < 106);
        assert!(smoothed[4] > 100);

        let edge = [0, 0, 0, 255, 255, 255, 255, 255];
        let smoothed = edge_aware_smooth_rgba(&edge, 2, 1, 4, 0);
        assert_eq!(smoothed, edge);
    }

    #[test]
    fn small_island_is_reassigned_to_its_neighbor() {
        let mut labels = vec![0, 0, 0, 0, 1, 0, 0, 0, 0];
        let palette = vec![
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
        ];
        despeckle(&mut labels, 3, 3, 2, &palette);

        assert!(labels.iter().all(|label| *label == 0));
    }
}
