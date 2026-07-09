use crate::lib::image::{Align, Canvas, Color, Font, ImageResult, TextOptions};
use crate::libkagami::complex::types::AssTime;
use crate::libkagami::core::SubstationAlpha;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewShot {
    pub centiseconds: u64,
    pub label: String,
}

pub fn select_preview_shots(
    ts: &SubstationAlpha,
    max: usize,
    min_gap_cs: u64,
) -> Vec<PreviewShot> {
    let mut candidates = ts
        .events
        .iter()
        .filter_map(|event| {
            let start = event.start.total_centiseconds();
            let end = event.end.total_centiseconds();
            if end <= start || !event.raw_text().contains(r"\fn") {
                return None;
            }
            Some((start, (start + end) / 2))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(start, midpoint)| (*start, *midpoint));

    let mut shots = Vec::new();
    let mut last_midpoint = None;
    for (_, midpoint) in candidates {
        if shots.len() >= max {
            break;
        }
        if let Some(last) = last_midpoint {
            if midpoint < last + min_gap_cs {
                continue;
            }
        }
        let active = ts
            .events
            .iter()
            .filter(|event| {
                event.start.total_centiseconds() <= midpoint
                    && midpoint <= event.end.total_centiseconds()
            })
            .count();
        let suffix = if active == 1 { "line" } else { "lines" };
        shots.push(PreviewShot {
            centiseconds: midpoint,
            label: format!(
                "{} · {} {}",
                AssTime::from_centiseconds(midpoint),
                active,
                suffix
            ),
        });
        last_midpoint = Some(midpoint);
    }
    shots
}

pub fn compose_preview(
    frame_png: &[u8],
    label: &str,
    watermark_font: &Font,
    label_font: &Font,
) -> ImageResult<Vec<u8>> {
    let mut canvas = Canvas::from_png_bytes(frame_png)?;
    let height = canvas.height() as f32;
    let width = canvas.width() as f32;
    let size = (height / 30.0).clamp(16.0, 48.0);
    let margin = (height / 60.0).clamp(4.0, 32.0);
    let shadow = 2.0_f32.max(size / 18.0);
    let line_height = 1.2;

    let label_shadow = TextOptions {
        x: margin + shadow,
        y: margin + shadow,
        size,
        color: Color::BLACK,
        align: Align::Left,
        max_width: None,
        line_height,
    };
    canvas.draw_text(label, label_font, &label_shadow)?;
    let label_text = TextOptions {
        x: margin,
        y: margin,
        size,
        color: Color::WHITE,
        align: Align::Left,
        max_width: None,
        line_height,
    };
    canvas.draw_text(label, label_font, &label_text)?;

    let watermark = "pandora tools";
    let watermark_y = height - margin - size * line_height;
    let watermark_shadow = TextOptions {
        x: width - margin + shadow,
        y: watermark_y + shadow,
        size,
        color: Color::BLACK,
        align: Align::Right,
        max_width: None,
        line_height,
    };
    canvas.draw_text(watermark, watermark_font, &watermark_shadow)?;
    let watermark_text = TextOptions {
        x: width - margin,
        y: watermark_y,
        size,
        color: Color {
            r: 255,
            g: 255,
            b: 255,
            a: 140,
        },
        align: Align::Right,
        max_width: None,
        line_height,
    };
    canvas.draw_text(watermark, watermark_font, &watermark_text)?;

    canvas.png_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::image::{Canvas, Color};
    use crate::libkagami::complex::types::AssColour;
    use crate::libkagami::core::{Event, ScriptInfo, V4pStyle};
    use crate::libkagami::tags::{ASSLine, ASSText};

    fn test_sub(events: Vec<Event>) -> SubstationAlpha {
        SubstationAlpha {
            script_info: ScriptInfo {
                title: String::new(),
                script_type: "v4.00+".to_string(),
                wrap_style: 2,
                scaled_border_and_shadow: true,
                playresx: 640,
                playresy: 480,
                ycbcr_matrix: "TV.709".to_string(),
                layout_res_x: 640,
                layout_res_y: 480,
            },
            v4p_styles: vec![V4pStyle {
                name: "Default".to_string(),
                fontname: "Arial".to_string(),
                fontsize: 20,
                colours: [
                    AssColour::opaque_white(),
                    AssColour::opaque_white(),
                    AssColour::transparent(),
                    AssColour::transparent(),
                ],
                bold: false,
                italic: false,
                underline: false,
                strikeout: false,
                scale_x: 100,
                scale_y: 100,
                spacing: 0.0,
                angle: 0.0,
                border_style: 1,
                outline: 2.0,
                shadow: 2.0,
                alignment: 2,
                margin_l: 10,
                margin_r: 10,
                margin_v: 10,
                encoding: 1,
            }],
            events,
        }
    }

    fn event(start: u64, end: u64, text: &str) -> Event {
        Event {
            layer: 0,
            start: AssTime::from_centiseconds(start),
            end: AssTime::from_centiseconds(end),
            style: "Default".to_string(),
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            text: ASSLine {
                current_overrides: Vec::new(),
                data: vec![ASSText::RawText(text.to_string())],
            },
        }
    }

    #[test]
    fn select_preview_shots_rejects_files_without_fn_lines() {
        let sub = test_sub(vec![event(0, 200, "{\\pos(1,2)}sign")]);
        assert!(select_preview_shots(&sub, 3, 1000).is_empty());
    }

    #[test]
    fn select_preview_shots_collapses_cluster_by_midpoint_gap() {
        let sub = test_sub(vec![
            event(0, 200, r"{\fnFancy}a"),
            event(100, 300, r"{\fnFancy}b"),
            event(1200, 1400, r"{\fnFancy}c"),
        ]);
        let shots = select_preview_shots(&sub, 3, 1000);
        assert_eq!(
            shots.iter().map(|shot| shot.centiseconds).collect::<Vec<_>>(),
            vec![100, 1300]
        );
    }

    #[test]
    fn select_preview_shots_caps_at_three_and_counts_active_lines() {
        let sub = test_sub(vec![
            event(0, 400, r"{\fnFancy}a"),
            event(100, 300, "layered"),
            event(1200, 1400, r"{\fnFancy}b"),
            event(2400, 2600, r"{\fnFancy}c"),
            event(3600, 3800, r"{\fnFancy}d"),
            event(4800, 5000, r"{\fnFancy}e"),
        ]);
        let shots = select_preview_shots(&sub, 3, 1000);
        assert_eq!(shots.len(), 3);
        assert_eq!(shots[0].centiseconds, 200);
        assert_eq!(shots[0].label, "0:00:02.00 · 2 lines");
        assert_eq!(shots[2].label, "0:00:25.00 · 1 line");
    }

    #[test]
    fn compose_preview_draws_label_and_watermark() {
        let input = Canvas::new(320, 180, Color::BLACK)
            .unwrap()
            .png_bytes()
            .unwrap();
        let font = Font::fallback();
        let output = compose_preview(&input, "0:00:02.00 · 2 lines", &font, &font).unwrap();
        let canvas = Canvas::from_png_bytes(&output).unwrap();

        let top_changed = (0..80).any(|x| {
            (0..40).any(|y| canvas.pixel_rgba(x, y).unwrap_or(Color::BLACK) != Color::BLACK)
        });
        let bottom_changed = (180..320).any(|x| {
            (130..180).any(|y| canvas.pixel_rgba(x, y).unwrap_or(Color::BLACK) != Color::BLACK)
        });
        assert!(top_changed);
        assert!(bottom_changed);
    }
}
