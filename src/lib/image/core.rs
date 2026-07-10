use resvg::tiny_skia;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

pub const MAX_DIM: u32 = 8192;
pub const MAX_SVG_BYTES: usize = 4 * 1024 * 1024;

pub type ImageResult<T> = Result<T, ImageError>;

#[derive(Debug)]
pub enum ImageError {
    Dimensions(String),
    Io(std::io::Error),
    InvalidFont(String),
    Svg(String),
    Decode(String),
    Encode(String),
    TooLarge(String),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Dimensions(msg) => write!(f, "image dimensions invalid: {}", msg),
            ImageError::Io(err) => write!(f, "image io failed: {}", err),
            ImageError::InvalidFont(msg) => write!(f, "image font invalid: {}", msg),
            ImageError::Svg(msg) => write!(f, "svg parse/render failed: {}", msg),
            ImageError::Decode(msg) => write!(f, "png decode failed: {}", msg),
            ImageError::Encode(msg) => write!(f, "png encode failed: {}", msg),
            ImageError::TooLarge(msg) => write!(f, "image input too large: {}", msg),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<std::io::Error> for ImageError {
    fn from(value: std::io::Error) -> Self {
        ImageError::Io(value)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };

    pub(crate) fn to_tiny(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Ratio {
    Of(u32, u32),
}

impl FromStr for Ratio {
    type Err = ImageError;

    fn from_str(value: &str) -> ImageResult<Self> {
        let mut parts = value.split(':');
        let width = parts
            .next()
            .ok_or_else(|| ImageError::Dimensions(format!("invalid ratio '{}'", value)))?
            .trim()
            .parse::<u32>()
            .map_err(|_| ImageError::Dimensions(format!("invalid ratio '{}'", value)))?;
        let height = parts
            .next()
            .ok_or_else(|| ImageError::Dimensions(format!("invalid ratio '{}'", value)))?
            .trim()
            .parse::<u32>()
            .map_err(|_| ImageError::Dimensions(format!("invalid ratio '{}'", value)))?;

        if parts.next().is_some() || width == 0 || height == 0 {
            return Err(ImageError::Dimensions(format!("invalid ratio '{}'", value)));
        }

        Ok(Ratio::Of(width, height))
    }
}

pub struct Canvas {
    pub(crate) pixmap: tiny_skia::Pixmap,
}

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> ImageResult<Canvas> {
        validate_dimensions(width, height)?;

        let mut pixmap = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| ImageError::Dimensions(format!("cannot allocate {}x{} canvas", width, height)))?;
        pixmap.fill(background.to_tiny());

        Ok(Canvas { pixmap })
    }

    pub fn from_png_bytes(data: &[u8]) -> ImageResult<Canvas> {
        let pixmap = tiny_skia::Pixmap::decode_png(data)
            .map_err(|err| ImageError::Decode(err.to_string()))?;
        validate_dimensions(pixmap.width(), pixmap.height())?;
        Ok(Canvas { pixmap })
    }

    pub fn with_ratio(ratio: Ratio, width: u32, background: Color) -> ImageResult<Canvas> {
        if width == 0 || width > MAX_DIM {
            return Err(ImageError::Dimensions(format!("width {} outside 1..={}", width, MAX_DIM)));
        }

        let Ratio::Of(ratio_width, ratio_height) = ratio;
        if ratio_width == 0 || ratio_height == 0 {
            return Err(ImageError::Dimensions("ratio sides must be non-zero".to_string()));
        }

        let numerator = width as u64 * ratio_height as u64;
        let denominator = ratio_width as u64;
        let height = ((numerator + denominator / 2) / denominator).max(1);
        if height > MAX_DIM as u64 {
            return Err(ImageError::Dimensions(format!("derived height {} exceeds {}", height, MAX_DIM)));
        }

        Canvas::new(width, height as u32, background)
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return;
        }

        let left = x.floor().max(0.0) as u32;
        let top = y.floor().max(0.0) as u32;
        let right = (x + width).ceil().min(self.width() as f32).max(0.0) as u32;
        let bottom = (y + height).ceil().min(self.height() as f32).max(0.0) as u32;

        if right <= left || bottom <= top {
            return;
        }

        for py in top..bottom {
            for px in left..right {
                self.blend_pixel(px as i32, py as i32, color, 1.0);
            }
        }
    }

    pub fn blit(&mut self, src: &Canvas, x: u32, y: u32) {
        let copy_width = src.width().min(self.width().saturating_sub(x));
        let copy_height = src.height().min(self.height().saturating_sub(y));
        if copy_width == 0 || copy_height == 0 {
            return;
        }

        let src_width = src.width() as usize;
        let dst_width = self.width() as usize;
        let copy_bytes = copy_width as usize * 4;
        let src_data = src.pixmap.data();
        let dst_data = self.pixmap.data_mut();
        for row in 0..copy_height as usize {
            let src_start = row * src_width * 4;
            let dst_start = ((y as usize + row) * dst_width + x as usize) * 4;
            dst_data[dst_start..dst_start + copy_bytes]
                .copy_from_slice(&src_data[src_start..src_start + copy_bytes]);
        }
    }

    pub fn png_bytes(&self) -> ImageResult<Vec<u8>> {
        self.pixmap
            .encode_png()
            .map_err(|err| ImageError::Encode(err.to_string()))
    }

    pub fn save_png(&self, path: &Path) -> ImageResult<()> {
        std::fs::write(path, self.png_bytes()?)?;
        Ok(())
    }

    pub(crate) fn pixmap_mut(&mut self) -> &mut tiny_skia::Pixmap {
        &mut self.pixmap
    }

    pub(crate) fn blend_pixel(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width() as i32 || y >= self.height() as i32 {
            return;
        }

        let coverage = coverage.clamp(0.0, 1.0);
        if coverage <= 0.0 || color.a == 0 {
            return;
        }

        let src_a = (color.a as f32 / 255.0) * coverage;
        let src_r = (color.r as f32 / 255.0) * src_a;
        let src_g = (color.g as f32 / 255.0) * src_a;
        let src_b = (color.b as f32 / 255.0) * src_a;

        let offset = ((y as u32 * self.width() + x as u32) as usize) * 4;
        let data = self.pixmap.data_mut();

        let dst_r = data[offset] as f32 / 255.0;
        let dst_g = data[offset + 1] as f32 / 255.0;
        let dst_b = data[offset + 2] as f32 / 255.0;
        let dst_a = data[offset + 3] as f32 / 255.0;
        let inv_a = 1.0 - src_a;

        data[offset] = float_to_u8(src_r + dst_r * inv_a);
        data[offset + 1] = float_to_u8(src_g + dst_g * inv_a);
        data[offset + 2] = float_to_u8(src_b + dst_b * inv_a);
        data[offset + 3] = float_to_u8(src_a + dst_a * inv_a);
    }

    #[cfg(test)]
    pub(crate) fn pixel_rgba(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width() || y >= self.height() {
            return None;
        }

        let offset = ((y * self.width() + x) as usize) * 4;
        let data = self.pixmap.data();
        let alpha = data[offset + 3] as u32;
        if alpha == 0 {
            return Some(Color::TRANSPARENT);
        }

        Some(Color {
            r: unpremultiply(data[offset], alpha),
            g: unpremultiply(data[offset + 1], alpha),
            b: unpremultiply(data[offset + 2], alpha),
            a: data[offset + 3],
        })
    }
}

pub(crate) fn validate_dimensions(width: u32, height: u32) -> ImageResult<()> {
    if width == 0 || height == 0 {
        return Err(ImageError::Dimensions("dimensions must be non-zero".to_string()));
    }
    if width > MAX_DIM || height > MAX_DIM {
        return Err(ImageError::Dimensions(format!("{}x{} exceeds {}px side cap", width, height, MAX_DIM)));
    }
    Ok(())
}

pub(crate) fn placement_dimension(label: &str, value: f32) -> ImageResult<u32> {
    if !value.is_finite() || value <= 0.0 {
        return Err(ImageError::Dimensions(format!("{} must be positive and finite", label)));
    }
    if value > MAX_DIM as f32 {
        return Err(ImageError::Dimensions(format!("{} {} exceeds {}", label, value, MAX_DIM)));
    }

    Ok(value.round().max(1.0) as u32)
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
fn unpremultiply(value: u8, alpha: u32) -> u8 {
    (((value as u32 * 255) + alpha / 2) / alpha).min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_from_str_accepts_colon_form() {
        assert_eq!("16:9".parse::<Ratio>().unwrap(), Ratio::Of(16, 9));
        assert_eq!(" 4 : 3 ".parse::<Ratio>().unwrap(), Ratio::Of(4, 3));
    }

    #[test]
    fn ratio_from_str_rejects_bad_values() {
        assert!("16".parse::<Ratio>().is_err());
        assert!("a:b".parse::<Ratio>().is_err());
        assert!("0:5".parse::<Ratio>().is_err());
        assert!("1:2:3".parse::<Ratio>().is_err());
    }

    #[test]
    fn canvas_with_ratio_derives_height_from_width() {
        let canvas = Canvas::with_ratio(Ratio::Of(16, 9), 1280, Color::TRANSPARENT).unwrap();
        assert_eq!(canvas.width(), 1280);
        assert_eq!(canvas.height(), 720);

        let rounded = Canvas::with_ratio(Ratio::Of(16, 9), 100, Color::TRANSPARENT).unwrap();
        assert_eq!(rounded.height(), 56);
    }

    #[test]
    fn canvas_rejects_zero_and_oversized_dimensions() {
        assert!(Canvas::new(0, 1, Color::TRANSPARENT).is_err());
        assert!(Canvas::new(MAX_DIM + 1, 1, Color::TRANSPARENT).is_err());
        assert!(Canvas::with_ratio(Ratio::Of(1, MAX_DIM), 2, Color::TRANSPARENT).is_err());
    }

    #[test]
    fn background_and_rect_fill_are_visible() {
        let mut canvas = Canvas::new(8, 8, Color::TRANSPARENT).unwrap();
        assert_eq!(canvas.pixel_rgba(0, 0).unwrap().a, 0);

        canvas.fill_rect(2.0, 2.0, 3.0, 3.0, Color::WHITE);
        assert_eq!(canvas.pixel_rgba(3, 3).unwrap(), Color::WHITE);
        assert_eq!(canvas.pixel_rgba(0, 0).unwrap(), Color::TRANSPARENT);
    }

    #[test]
    fn blit_copies_pixels_and_clips_at_canvas_edges() {
        let mut destination = Canvas::new(4, 4, Color::BLACK).unwrap();
        let source = Canvas::new(3, 2, Color { r: 255, g: 0, b: 0, a: 255 }).unwrap();

        destination.blit(&source, 2, 3);

        assert_eq!(destination.pixel_rgba(2, 3).unwrap(), Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(destination.pixel_rgba(3, 3).unwrap(), Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(destination.pixel_rgba(1, 3).unwrap(), Color::BLACK);
        assert_eq!(destination.pixel_rgba(3, 2).unwrap(), Color::BLACK);
    }

    #[test]
    fn png_round_trip_preserves_dimensions_and_alpha() {
        let mut canvas = Canvas::new(4, 4, Color::TRANSPARENT).unwrap();
        canvas.fill_rect(1.0, 1.0, 2.0, 2.0, Color::BLACK);

        let bytes = canvas.png_bytes().unwrap();
        let decoded = tiny_skia::Pixmap::decode_png(&bytes).unwrap();
        assert_eq!(decoded.width(), 4);
        assert_eq!(decoded.height(), 4);
        assert_eq!(decoded.data()[3], 0);

        let canvas = Canvas::from_png_bytes(&bytes).unwrap();
        assert_eq!(canvas.width(), 4);
        assert_eq!(canvas.height(), 4);
        assert_eq!(canvas.pixel_rgba(2, 2).unwrap(), Color::BLACK);
    }

    #[test]
    fn canvas_from_png_rejects_bad_input() {
        assert!(matches!(
            Canvas::from_png_bytes(b"not png"),
            Err(ImageError::Decode(_))
        ));
    }

    #[test]
    fn save_png_writes_file() {
        let canvas = Canvas::new(2, 2, Color::WHITE).unwrap();
        let path = std::env::temp_dir().join(format!("pandora-image-{}.png", std::process::id()));

        canvas.save_png(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
