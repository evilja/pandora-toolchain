use visioncortex::clusters::Cluster;
use visioncortex::{
    BinaryImage, PathF64, PathI32, PathSimplifyMode, PointF64, PointI32, SubdivideSmooth,
};

use crate::{Path, Point, Segment};

const VTRACER_OUTSET_RATIO: f64 = 8.0;
const VTRACER_SPLICE_THRESHOLD: f64 = std::f64::consts::FRAC_PI_4;

pub(crate) fn paths_for_labels(
    labels: &[u16],
    width: u32,
    height: u32,
    layer_count: usize,
    output_width: u32,
    output_height: u32,
    simplify: f32,
    curve_fit: f32,
    corner_threshold: f32,
    preserve_gradients: bool,
) -> Vec<Vec<Path>> {
    let mut layers = vec![Vec::new(); layer_count];
    if labels.is_empty() || width == 0 || height == 0 {
        return layers;
    }

    let scale_x = output_width as f32 / width as f32;
    let scale_y = output_height as f32 / height as f32;
    let sample_tolerance = if simplify <= 0.0 {
        0.0
    } else {
        simplify as f64 / scale_x.max(scale_y) as f64
    };
    let mut visited = vec![false; labels.len()];
    let mut queue: Vec<u32> = Vec::new();

    for start in 0..labels.len() {
        let label = labels[start];
        if label == crate::TRANSPARENT_LABEL || visited[start] {
            continue;
        }

        queue.clear();
        queue.push(start as u32);
        visited[start] = true;
        let mut cursor = 0usize;
        let mut min_x = width;
        let mut min_y = height;
        let mut max_x = 0u32;
        let mut max_y = 0u32;

        while cursor < queue.len() {
            let index = queue[cursor] as usize;
            cursor += 1;
            let x = index as u32 % width;
            let y = index as u32 / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);

            for neighbor in pixel_neighbors(x, y, width, height).into_iter().flatten() {
                if !visited[neighbor] && labels[neighbor] == label {
                    visited[neighbor] = true;
                    queue.push(neighbor as u32);
                }
            }
        }

        let mask_width = (max_x - min_x + 1) as usize;
        let mask_height = (max_y - min_y + 1) as usize;
        let mut mask = BinaryImage::new_w_h(mask_width, mask_height);
        for index in queue.iter().copied() {
            let x = index % width;
            let y = index / width;
            mask.set_pixel((x - min_x) as usize, (y - min_y) as usize, true);
        }

        let paths = trace_component(
            &mask,
            min_x as i32,
            min_y as i32,
            scale_x,
            scale_y,
            sample_tolerance,
            curve_fit,
            corner_threshold,
            preserve_gradients,
        );
        layers[label as usize].extend(paths);
    }

    layers
}

fn pixel_neighbors(x: u32, y: u32, width: u32, height: u32) -> [Option<usize>; 4] {
    [
        (x > 0).then(|| (y * width + x - 1) as usize),
        (x + 1 < width).then(|| (y * width + x + 1) as usize),
        (y > 0).then(|| ((y - 1) * width + x) as usize),
        (y + 1 < height).then(|| ((y + 1) * width + x) as usize),
    ]
}

#[allow(clippy::too_many_arguments)]
fn trace_component(
    mask: &BinaryImage,
    offset_x: i32,
    offset_y: i32,
    scale_x: f32,
    scale_y: f32,
    simplify: f64,
    curve_fit: f32,
    corner_threshold: f32,
    preserve_gradients: bool,
) -> Vec<Path> {
    Cluster::image_to_paths(mask, PathSimplifyMode::None)
        .into_iter()
        .enumerate()
        .filter_map(|(index, raw)| {
            let hole = index > 0;
            let clockwise = !hole;
            let area = path_area(&raw) * scale_x as f64 * scale_y as f64;
            if area <= f64::EPSILON {
                return None;
            }

            let mut polygon = if simplify > 0.0 || curve_fit > 0.0 {
                raw.simplify(clockwise)
            } else {
                raw.clone()
            };
            if simplify > 0.0 {
                if let Some(reduced) = polygon.reduce(simplify) {
                    polygon = reduced;
                }
            }
            polygon = polygon.to_closed();
            if polygon.len() < 4 {
                polygon = raw;
            }

            if curve_fit > 0.0 {
                let iterations = (curve_fit * 10.0).round().clamp(1.0, 10.0) as usize;
                let segment_length = (10.0 - curve_fit as f64 * 6.0).clamp(4.0, 10.0);
                let fitted_path = if preserve_gradients {
                    polygon.to_path_f64()
                } else {
                    polygon.smooth(
                        (corner_threshold as f64).to_radians(),
                        VTRACER_OUTSET_RATIO,
                        segment_length,
                        iterations,
                    )
                };
                let fit_error = if preserve_gradients {
                    (simplify * 0.25).clamp(0.1, 0.35)
                } else if simplify > 0.0 {
                    (simplify * 0.5).clamp(0.1, 0.75)
                } else {
                    0.25
                };
                let curves = fit_closed_beziers(&fitted_path, fit_error);
                if let Some(path) =
                    spline_path(&curves, offset_x, offset_y, scale_x, scale_y, hole, area)
                {
                    return Some(path);
                }
            }

            line_path(&polygon, offset_x, offset_y, scale_x, scale_y, hole, area)
        })
        .collect()
}

fn path_area(path: &PathI32) -> f64 {
    if path.len() < 3 {
        return 0.0;
    }
    let mut twice_area = 0i64;
    for points in path.path.windows(2) {
        twice_area +=
            points[0].x as i64 * points[1].y as i64 - points[1].x as i64 * points[0].y as i64;
    }
    if path.path.first() != path.path.last() {
        let first = path.path[0];
        let last = path.path[path.len() - 1];
        twice_area += last.x as i64 * first.y as i64 - first.x as i64 * last.y as i64;
    }
    twice_area.unsigned_abs() as f64 * 0.5
}

fn line_path(
    path: &PathI32,
    offset_x: i32,
    offset_y: i32,
    scale_x: f32,
    scale_y: f32,
    hole: bool,
    area: f64,
) -> Option<Path> {
    if path.len() < 3 {
        return None;
    }
    let start = scale_i32(path.path[0], offset_x, offset_y, scale_x, scale_y);
    let closed = path.path.first() == path.path.last();
    let body_end = path.len() - usize::from(closed);
    let mut segments: Vec<Segment> = path.path[1..body_end]
        .iter()
        .copied()
        .map(|point| Segment::Line {
            to: scale_i32(point, offset_x, offset_y, scale_x, scale_y),
        })
        .collect();
    segments.push(Segment::Line { to: start });
    (segments.len() >= 3).then_some(Path {
        start,
        segments,
        hole,
        area,
    })
}

fn fit_closed_beziers(path: &PathF64, max_error: f64) -> Vec<[PointF64; 4]> {
    if path.len() < 3 {
        return Vec::new();
    }
    let body_end = path.len() - usize::from(path.path.first() == path.path.last());
    let points = &path.path[..body_end];
    if points.len() < 2 {
        return Vec::new();
    }

    let closed = PathF64::from_points(
        points
            .iter()
            .copied()
            .chain(std::iter::once(points[0]))
            .collect(),
    );
    let splice_points =
        SubdivideSmooth::find_splice_points(&closed, VTRACER_SPLICE_THRESHOLD, true);
    let mut cuts: Vec<usize> = splice_points
        .iter()
        .enumerate()
        .filter_map(|(index, splice)| splice.then_some(index))
        .collect();
    if cuts.is_empty() {
        cuts.push(0);
    }
    if cuts.len() == 1 {
        cuts.push((cuts[0] + points.len() / 2) % points.len());
    }
    cuts.sort_unstable();
    cuts.dedup();
    if cuts.len() == 1 {
        return Vec::new();
    }

    let mut curves = Vec::new();
    for index in 0..cuts.len() {
        let start = cuts[index];
        let end = cuts[(index + 1) % cuts.len()];
        let mut span = Vec::new();
        if start < end {
            span.extend_from_slice(&points[start..=end]);
        } else {
            span.extend_from_slice(&points[start..]);
            span.extend_from_slice(&points[..=end]);
        }
        if span.len() == 2 {
            curves.push([span[0], span[0], span[1], span[1]]);
        } else if span.len() > 2 {
            curves.extend(SubdivideSmooth::fit_points_with_beziers(&span, max_error));
        }
    }
    let first = curves.first().map(|curve| curve[0]);
    if let (Some(first), Some(last)) = (first, curves.last_mut()) {
        last[3] = first;
    }
    curves
}

fn spline_path(
    curves: &[[PointF64; 4]],
    offset_x: i32,
    offset_y: i32,
    scale_x: f32,
    scale_y: f32,
    hole: bool,
    area: f64,
) -> Option<Path> {
    let start = scale_f64(
        curves.first()?.first().copied()?,
        offset_x,
        offset_y,
        scale_x,
        scale_y,
    );
    let mut segments = Vec::with_capacity(curves.len());
    for (index, curve) in curves.iter().enumerate() {
        let control_1 = scale_f64(curve[1], offset_x, offset_y, scale_x, scale_y);
        let control_2 = scale_f64(curve[2], offset_x, offset_y, scale_x, scale_y);
        let to = if index + 1 == curves.len() {
            start
        } else {
            scale_f64(curve[3], offset_x, offset_y, scale_x, scale_y)
        };
        if !point_is_finite(control_1) || !point_is_finite(control_2) || !point_is_finite(to) {
            return None;
        }
        segments.push(Segment::Cubic {
            control_1,
            control_2,
            to,
        });
    }
    (!segments.is_empty()).then_some(Path {
        start,
        segments,
        hole,
        area,
    })
}

fn scale_i32(point: PointI32, offset_x: i32, offset_y: i32, scale_x: f32, scale_y: f32) -> Point {
    Point {
        x: (point.x + offset_x) as f32 * scale_x,
        y: (point.y + offset_y) as f32 * scale_y,
    }
}

fn scale_f64(point: PointF64, offset_x: i32, offset_y: i32, scale_x: f32, scale_y: f32) -> Point {
    Point {
        x: (point.x + offset_x as f64) as f32 * scale_x,
        y: (point.y + offset_y as f64) as f32 * scale_y,
    }
}

fn point_is_finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_square_becomes_one_clockwise_path() {
        let labels = vec![0u16; 4];
        let paths = paths_for_labels(&labels, 2, 2, 1, 2, 2, 0.0, 0.0, 55.0, false);

        assert_eq!(paths[0].len(), 1);
        assert!(!paths[0][0].hole);
        assert_eq!(paths[0][0].area, 4.0);
        assert_eq!(paths[0][0].segments.len(), 4);
        assert!(
            paths[0][0]
                .segments
                .iter()
                .all(|segment| matches!(segment, Segment::Line { .. }))
        );
    }

    #[test]
    fn visioncortex_spline_backend_emits_closed_cubics() {
        let transparent = crate::TRANSPARENT_LABEL;
        let mut labels = vec![transparent; 32 * 32];
        for y in 0..32i32 {
            for x in 0..32i32 {
                let dx = x - 16;
                let dy = y - 16;
                if dx * dx + dy * dy <= 12 * 12 {
                    labels[(y * 32 + x) as usize] = 0;
                }
            }
        }
        let paths = paths_for_labels(&labels, 32, 32, 1, 32, 32, 0.75, 0.8, 55.0, false);

        assert_eq!(paths[0].len(), 1);
        assert!(
            paths[0][0]
                .segments
                .iter()
                .all(|segment| matches!(segment, Segment::Cubic { .. }))
        );
        assert_eq!(
            paths[0][0].segments.last().unwrap().end(),
            paths[0][0].start
        );
        assert!(paths[0][0].segments.iter().all(|segment| match segment {
            Segment::Line { to } => point_is_finite(*to),
            Segment::Cubic {
                control_1,
                control_2,
                to,
            } => point_is_finite(*control_1) && point_is_finite(*control_2) && point_is_finite(*to),
        }));
    }

    #[test]
    fn independently_fitted_shared_edges_stay_within_bounded_fit_range() {
        let mut labels = Vec::new();
        for y in 0..64 {
            let boundary = 32.0 + ((y as f32 / 8.0).sin() * 7.0);
            for x in 0..64 {
                labels.push(u16::from(x as f32 >= boundary));
            }
        }
        let layers = paths_for_labels(&labels, 64, 64, 2, 64, 64, 1.0, 0.65, 55.0, false);
        let sample_edge = |path: &Path| {
            let mut points = Vec::new();
            let mut from = path.start;
            for segment in &path.segments {
                match segment {
                    Segment::Line { to } => points.push(*to),
                    Segment::Cubic {
                        control_1,
                        control_2,
                        to,
                    } => {
                        for step in 1..=64 {
                            let t = step as f32 / 64.0;
                            let u = 1.0 - t;
                            points.push(Point {
                                x: from.x * u * u * u
                                    + control_1.x * 3.0 * u * u * t
                                    + control_2.x * 3.0 * u * t * t
                                    + to.x * t * t * t,
                                y: from.y * u * u * u
                                    + control_1.y * 3.0 * u * u * t
                                    + control_2.y * 3.0 * u * t * t
                                    + to.y * t * t * t,
                            });
                        }
                    }
                }
                from = segment.end();
            }
            points
                .into_iter()
                .filter(|point| point.x > 20.0 && point.x < 44.0 && point.y > 1.0 && point.y < 63.0)
                .collect::<Vec<_>>()
        };
        let left = sample_edge(&layers[0][0]);
        let right = sample_edge(&layers[1][0]);
        let directed_error = |source: &[Point], target: &[Point]| {
            source
                .iter()
                .map(|source| {
                    let distance = target
                        .iter()
                        .map(|target| {
                            let dx = source.x - target.x;
                            let dy = source.y - target.y;
                            (dx * dx + dy * dy).sqrt()
                        })
                        .fold(f32::INFINITY, f32::min);
                    (distance, *source)
                })
                .max_by(|left, right| left.0.total_cmp(&right.0))
                .unwrap()
        };
        let left_error = directed_error(&left, &right);
        let right_error = directed_error(&right, &left);
        let mismatch = left_error.0.max(right_error.0);

        assert!(!left.is_empty() && !right.is_empty());
        assert!(
            mismatch <= 2.0,
            "shared edge mismatch was {mismatch}px at {left_error:?} / {right_error:?}"
        );
    }

    #[test]
    fn all_three_by_three_masks_produce_valid_closed_paths() {
        let transparent = crate::TRANSPARENT_LABEL;
        for mask in 1u16..(1 << 9) {
            let labels: Vec<u16> = (0..9)
                .map(|bit| {
                    if mask & (1 << bit) != 0 {
                        0
                    } else {
                        transparent
                    }
                })
                .collect();
            let paths = paths_for_labels(&labels, 3, 3, 1, 3, 3, 0.75, 0.65, 55.0, false);

            for path in &paths[0] {
                assert!(path.area.is_finite() && path.area > 0.0, "mask {mask}");
                assert_eq!(
                    path.segments.last().unwrap().end(),
                    path.start,
                    "mask {mask}"
                );
            }
        }
    }

    #[test]
    fn center_gap_becomes_a_reverse_winding_hole() {
        let transparent = crate::TRANSPARENT_LABEL;
        let labels = vec![0, 0, 0, 0, transparent, 0, 0, 0, 0];
        let paths = paths_for_labels(&labels, 3, 3, 1, 3, 3, 0.0, 0.0, 55.0, false);

        assert_eq!(paths[0].len(), 2);
        assert_eq!(paths[0].iter().filter(|path| path.hole).count(), 1);
        assert_eq!(paths[0].iter().filter(|path| !path.hole).count(), 1);
    }

    #[test]
    fn diagonal_pixels_stay_as_separate_contours() {
        let transparent = crate::TRANSPARENT_LABEL;
        let labels = vec![0, transparent, transparent, 0];
        let paths = paths_for_labels(&labels, 2, 2, 1, 2, 2, 0.0, 0.0, 55.0, false);

        assert_eq!(paths[0].len(), 2);
        assert!(paths[0].iter().all(|path| !path.hole));
    }
}
