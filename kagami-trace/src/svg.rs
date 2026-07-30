use std::fmt::Write;

use crate::{Segment, Trace};

impl Trace {
    pub fn to_svg(&self) -> String {
        self.to_svg_with_seam_overlap(0.0)
    }

    pub fn to_svg_with_seam_overlap(&self, seam_overlap: f32) -> String {
        let seam_overlap = if seam_overlap.is_finite() {
            seam_overlap.clamp(0.0, 4.0)
        } else {
            0.0
        };
        let mut output = String::new();
        let _ = write!(
            output,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
            self.width, self.height, self.width, self.height
        );
        for layer in &self.layers {
            let Some(color) = self.color(layer) else {
                continue;
            };
            let mut data = String::new();
            for path in &layer.paths {
                let _ = write!(
                    data,
                    "M{} {}",
                    format_number(path.start.x),
                    format_number(path.start.y)
                );
                for segment in &path.segments {
                    match segment {
                        Segment::Line { to } => {
                            let _ =
                                write!(data, "L{} {}", format_number(to.x), format_number(to.y));
                        }
                        Segment::Cubic {
                            control_1,
                            control_2,
                            to,
                        } => {
                            let _ = write!(
                                data,
                                "C{} {} {} {} {} {}",
                                format_number(control_1.x),
                                format_number(control_1.y),
                                format_number(control_2.x),
                                format_number(control_2.y),
                                format_number(to.x),
                                format_number(to.y),
                            );
                        }
                    }
                }
                data.push('Z');
            }
            let underlap = if seam_overlap > 0.0 {
                format!(
                    " stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{}\" stroke-linejoin=\"round\" paint-order=\"stroke fill\"",
                    color.hex_rgb(),
                    format_opacity(color.a),
                    format_number(seam_overlap * 2.0),
                )
            } else {
                String::new()
            };
            let _ = write!(
                output,
                "<path data-color-index=\"{}\" fill=\"{}\" fill-opacity=\"{}\" fill-rule=\"nonzero\"{} d=\"{}\"/>",
                layer.palette_index,
                color.hex_rgb(),
                format_opacity(color.a),
                underlap,
                data,
            );
        }
        output.push_str("</svg>");
        output
    }
}

pub(crate) fn format_number(value: f32) -> String {
    let rounded = value.round();
    if (value - rounded).abs() < 0.0001 {
        return format!("{}", rounded as i64);
    }
    let mut output = format!("{value:.3}");
    while output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    if output == "-0" {
        "0".to_string()
    } else {
        output
    }
}

fn format_opacity(alpha: u8) -> String {
    if alpha == 255 {
        "1".to_string()
    } else if alpha == 0 {
        "0".to_string()
    } else {
        format_number(alpha as f32 / 255.0)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Color, Path, Point, Segment, Trace, TraceLayer};

    #[test]
    fn svg_contains_palette_color_alpha_and_curves() {
        let trace = Trace::new(
            10,
            10,
            10,
            10,
            vec![Color {
                r: 0x12,
                g: 0x34,
                b: 0x56,
                a: 128,
            }],
            vec![TraceLayer {
                palette_index: 0,
                sample_pixel_count: 10,
                paths: vec![Path {
                    start: Point { x: 0.0, y: 0.0 },
                    segments: vec![Segment::Cubic {
                        control_1: Point { x: 1.0, y: 2.0 },
                        control_2: Point { x: 3.0, y: 4.0 },
                        to: Point { x: 0.0, y: 0.0 },
                    }],
                    hole: false,
                    area: 10.0,
                }],
            }],
        );

        let svg = trace.to_svg();
        assert!(svg.contains("fill=\"#123456\""));
        assert!(svg.contains("fill-opacity=\"0.502\""));
        assert!(!svg.contains("stroke="));
        assert!(svg.contains("C1 2 3 4 0 0Z"));

        let overlapped = trace.to_svg_with_seam_overlap(1.0);
        assert!(overlapped.contains("stroke=\"#123456\""));
        assert!(overlapped.contains("stroke-opacity=\"0.502\""));
        assert!(overlapped.contains("stroke-width=\"2\""));
        assert!(overlapped.contains("paint-order=\"stroke fill\""));
    }
}
