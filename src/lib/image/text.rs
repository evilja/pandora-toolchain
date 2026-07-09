use super::core::{Canvas, Color, ImageError, ImageResult};
use ab_glyph::{point, Font as AbFont, FontVec, GlyphId, PxScale, ScaleFont};
use std::path::Path;

pub struct Font {
    inner: FontVec,
}

impl Font {
    pub fn from_bytes(bytes: Vec<u8>) -> ImageResult<Font> {
        FontVec::try_from_vec_and_index(bytes, 0)
            .map(|inner| Font { inner })
            .map_err(|err| ImageError::InvalidFont(err.to_string()))
    }

    pub fn from_path(path: &Path) -> ImageResult<Font> {
        Font::from_bytes(std::fs::read(path)?)
    }

    pub fn fallback() -> Font {
        Font::from_bytes(include_bytes!("testdata/LiberationMono-Regular.ttf").to_vec())
            .expect("embedded fallback font must be valid")
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct TextOptions {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: Color,
    pub align: Align,
    pub max_width: Option<f32>,
    pub line_height: f32,
}

impl Default for TextOptions {
    fn default() -> Self {
        TextOptions {
            x: 0.0,
            y: 0.0,
            size: 16.0,
            color: Color::BLACK,
            align: Align::Left,
            max_width: None,
            line_height: 1.2,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct TextBounds {
    pub width: f32,
    pub height: f32,
    pub lines: usize,
}

#[derive(Debug, Clone)]
struct Line {
    text: String,
    width: f32,
}

impl Canvas {
    pub fn draw_text(&mut self, text: &str, font: &Font, opts: &TextOptions) -> ImageResult<TextBounds> {
        validate_text_options(opts)?;

        let lines = layout_lines(font, opts.size, text, opts.max_width);
        let scaled = font.inner.as_scaled(PxScale::from(opts.size));
        let ascent = scaled.ascent();
        let line_advance = line_advance(font, opts.size, opts.line_height);
        let max_width = lines.iter().fold(0.0_f32, |acc, line| acc.max(line.width));

        for (line_index, line) in lines.iter().enumerate() {
            let baseline = opts.y + ascent + line_index as f32 * line_advance;
            let mut caret = line_x(opts, line.width);
            let mut previous = None;

            for ch in line.text.chars() {
                let glyph_id = scaled.glyph_id(ch);
                if let Some(previous_id) = previous {
                    caret += scaled.kern(previous_id, glyph_id);
                }

                let glyph = glyph_id
                    .with_scale_and_position(PxScale::from(opts.size), point(caret, baseline));
                if let Some(outlined) = scaled.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    outlined.draw(|x, y, coverage| {
                        let px = bounds.min.x as i32 + x as i32;
                        let py = bounds.min.y as i32 + y as i32;
                        self.blend_pixel(px, py, opts.color, coverage);
                    });
                }

                caret += scaled.h_advance(glyph_id);
                previous = Some(glyph_id);
            }
        }

        Ok(TextBounds {
            width: max_width,
            height: if lines.is_empty() { 0.0 } else { line_advance * lines.len() as f32 },
            lines: lines.len(),
        })
    }
}

fn validate_text_options(opts: &TextOptions) -> ImageResult<()> {
    if !opts.x.is_finite() || !opts.y.is_finite() {
        return Err(ImageError::Dimensions("text position must be finite".to_string()));
    }
    if !opts.size.is_finite() || opts.size <= 0.0 {
        return Err(ImageError::Dimensions("text size must be positive and finite".to_string()));
    }
    if !opts.line_height.is_finite() || opts.line_height <= 0.0 {
        return Err(ImageError::Dimensions("text line height must be positive and finite".to_string()));
    }
    if let Some(max_width) = opts.max_width {
        if !max_width.is_finite() || max_width <= 0.0 {
            return Err(ImageError::Dimensions("text max_width must be positive and finite".to_string()));
        }
    }

    Ok(())
}

fn line_x(opts: &TextOptions, line_width: f32) -> f32 {
    match (opts.align, opts.max_width) {
        (Align::Left, _) => opts.x,
        (Align::Center, Some(max_width)) => opts.x + (max_width - line_width) / 2.0,
        (Align::Right, Some(max_width)) => opts.x + (max_width - line_width),
        (Align::Center, None) => opts.x - line_width / 2.0,
        (Align::Right, None) => opts.x - line_width,
    }
}

fn line_advance(font: &Font, size: f32, multiplier: f32) -> f32 {
    let scaled = font.inner.as_scaled(PxScale::from(size));
    (scaled.height() + scaled.line_gap()) * multiplier
}

fn layout_lines(font: &Font, size: f32, text: &str, max_width: Option<f32>) -> Vec<Line> {
    let mut lines = Vec::new();

    for paragraph in text.split('\n') {
        if let Some(max_width) = max_width {
            wrap_paragraph(font, size, paragraph, max_width, &mut lines);
        } else {
            lines.push(Line {
                text: paragraph.to_string(),
                width: text_width(font, size, paragraph),
            });
        }
    }

    if lines.is_empty() {
        lines.push(Line { text: String::new(), width: 0.0 });
    }

    lines
}

fn wrap_paragraph(font: &Font, size: f32, paragraph: &str, max_width: f32, lines: &mut Vec<Line>) {
    if paragraph.trim().is_empty() {
        lines.push(Line { text: String::new(), width: 0.0 });
        return;
    }

    let mut current = String::new();
    let mut current_width = 0.0;

    for word in paragraph.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current, word)
        };
        let candidate_width = text_width(font, size, &candidate);

        if candidate_width <= max_width || current.is_empty() {
            if candidate_width <= max_width {
                current = candidate;
                current_width = candidate_width;
                continue;
            }

            for broken in break_word(font, size, word, max_width) {
                lines.push(broken);
            }
            current.clear();
            current_width = 0.0;
            continue;
        }

        lines.push(Line { text: current, width: current_width });
        current = word.to_string();
        current_width = text_width(font, size, word);

        if current_width > max_width {
            for broken in break_word(font, size, &current, max_width) {
                lines.push(broken);
            }
            current = String::new();
            current_width = 0.0;
        }
    }

    if !current.is_empty() {
        lines.push(Line { text: current, width: current_width });
    }
}

fn break_word(font: &Font, size: f32, word: &str, max_width: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0.0;

    for ch in word.chars() {
        let candidate = format!("{}{}", current, ch);
        let candidate_width = text_width(font, size, &candidate);

        if candidate_width <= max_width || current.is_empty() {
            current = candidate;
            current_width = candidate_width;
        } else {
            lines.push(Line { text: current, width: current_width });
            current = ch.to_string();
            current_width = text_width(font, size, &current);
        }
    }

    if !current.is_empty() {
        lines.push(Line { text: current, width: current_width });
    }

    lines
}

fn text_width(font: &Font, size: f32, text: &str) -> f32 {
    let scaled = font.inner.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous: Option<GlyphId> = None;

    for ch in text.chars() {
        let glyph_id = scaled.glyph_id(ch);
        if let Some(previous_id) = previous {
            width += scaled.kern(previous_id, glyph_id);
        }
        width += scaled.h_advance(glyph_id);
        previous = Some(glyph_id);
    }

    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font() -> Font {
        Font::from_bytes(include_bytes!("testdata/LiberationMono-Regular.ttf").to_vec()).unwrap()
    }

    #[test]
    fn layout_width_grows_with_text() {
        let font = test_font();
        let short = text_width(&font, 24.0, "A");
        let long = text_width(&font, 24.0, "AAAA");

        assert!(short > 0.0);
        assert!(long > short);
    }

    #[test]
    fn layout_wraps_and_keeps_explicit_newlines() {
        let font = test_font();
        let lines = layout_lines(&font, 18.0, "alpha beta gamma\ndelta", Some(80.0));

        assert!(lines.len() >= 3);
        assert_eq!(lines.last().unwrap().text, "delta");
    }

    #[test]
    fn layout_breaks_words_that_exceed_width() {
        let font = test_font();
        let lines = layout_lines(&font, 24.0, "abcdefghijk", Some(30.0));

        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| !line.text.is_empty()));
    }

    #[test]
    fn draw_text_changes_pixels_inside_bounds() {
        let font = test_font();
        let mut canvas = Canvas::new(220, 80, Color::WHITE).unwrap();
        let bounds = canvas
            .draw_text(
                "Pandora",
                &font,
                &TextOptions {
                    x: 10.0,
                    y: 10.0,
                    size: 28.0,
                    color: Color::BLACK,
                    ..TextOptions::default()
                },
            )
            .unwrap();

        assert_eq!(bounds.lines, 1);
        assert!(bounds.width > 0.0);

        let mut dark_pixels = 0;
        for y in 10..70 {
            for x in 10..160 {
                let px = canvas.pixel_rgba(x, y).unwrap();
                if px.r < 220 || px.g < 220 || px.b < 220 {
                    dark_pixels += 1;
                }
            }
        }

        assert!(dark_pixels > 0);
        assert_eq!(canvas.pixel_rgba(0, 0).unwrap(), Color::WHITE);
    }

    #[test]
    fn text_bounds_follow_line_height() {
        let font = test_font();
        let mut canvas = Canvas::new(220, 120, Color::TRANSPARENT).unwrap();
        let bounds = canvas
            .draw_text(
                "one\ntwo",
                &font,
                &TextOptions {
                    size: 20.0,
                    line_height: 1.5,
                    ..TextOptions::default()
                },
            )
            .unwrap();

        assert_eq!(bounds.lines, 2);
        assert_eq!(bounds.height, line_advance(&font, 20.0, 1.5) * 2.0);
    }
}
