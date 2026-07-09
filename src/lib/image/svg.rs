use super::core::{placement_dimension, Canvas, ImageError, ImageResult, MAX_SVG_BYTES};
use resvg::tiny_skia;
use resvg::usvg;
use std::path::{Path, PathBuf};

pub struct SvgImage {
    tree: usvg::Tree,
}

impl SvgImage {
    pub fn from_bytes(data: &[u8]) -> ImageResult<SvgImage> {
        SvgImage::from_bytes_with_fonts::<PathBuf>(data, &[])
    }

    pub fn from_path(path: &Path) -> ImageResult<SvgImage> {
        let data = read_capped(path)?;
        SvgImage::from_bytes(&data)
    }

    pub fn from_bytes_with_fonts<P: AsRef<Path>>(data: &[u8], font_paths: &[P]) -> ImageResult<SvgImage> {
        if data.len() > MAX_SVG_BYTES {
            return Err(ImageError::TooLarge(format!("svg is {} bytes, cap is {}", data.len(), MAX_SVG_BYTES)));
        }

        let mut options = svg_options();
        for path in font_paths {
            let font_data = std::fs::read(path.as_ref())?;
            let before = options.fontdb.len();
            options.fontdb_mut().load_font_data(font_data);
            if options.fontdb.len() == before {
                return Err(ImageError::InvalidFont(format!("could not load svg font {}", path.as_ref().display())));
            }
        }

        usvg::Tree::from_data(data, &options)
            .map(|tree| SvgImage { tree })
            .map_err(|err| ImageError::Svg(err.to_string()))
    }

    pub fn from_path_with_fonts<P: AsRef<Path>>(path: &Path, font_paths: &[P]) -> ImageResult<SvgImage> {
        let data = read_capped(path)?;
        SvgImage::from_bytes_with_fonts(&data, font_paths)
    }

    pub fn size(&self) -> (f32, f32) {
        let size = self.tree.size();
        (size.width(), size.height())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FitMode {
    Contain,
    Cover,
    Stretch,
}

pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub fit: FitMode,
    pub opacity: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            fit: FitMode::Contain,
            opacity: 1.0,
        }
    }
}

impl Canvas {
    pub fn draw_svg(&mut self, svg: &SvgImage, place: Placement) -> ImageResult<()> {
        validate_placement(&place)?;

        let scratch_width = placement_dimension("placement width", place.width)?;
        let scratch_height = placement_dimension("placement height", place.height)?;
        let mut scratch = tiny_skia::Pixmap::new(scratch_width, scratch_height)
            .ok_or_else(|| ImageError::Dimensions(format!("cannot allocate {}x{} svg scratch", scratch_width, scratch_height)))?;

        let (source_width, source_height) = svg.size();
        let fit = fit_rect(source_width, source_height, scratch_width as f32, scratch_height as f32, place.fit);
        let transform = tiny_skia::Transform::from_scale(fit.scale_x, fit.scale_y)
            .post_translate(fit.offset_x, fit.offset_y);
        let mut scratch_mut = scratch.as_mut();
        resvg::render(&svg.tree, transform, &mut scratch_mut);

        let mut paint = tiny_skia::PixmapPaint::default();
        paint.opacity = place.opacity.clamp(0.0, 1.0);
        paint.blend_mode = tiny_skia::BlendMode::SourceOver;

        self.pixmap_mut().draw_pixmap(
            place.x.round() as i32,
            place.y.round() as i32,
            scratch.as_ref(),
            &paint,
            tiny_skia::Transform::identity(),
            None,
        );

        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct Fit {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
}

fn validate_placement(place: &Placement) -> ImageResult<()> {
    if !place.x.is_finite() || !place.y.is_finite() {
        return Err(ImageError::Dimensions("placement position must be finite".to_string()));
    }
    if !place.opacity.is_finite() {
        return Err(ImageError::Dimensions("placement opacity must be finite".to_string()));
    }

    let _ = placement_dimension("placement width", place.width)?;
    let _ = placement_dimension("placement height", place.height)?;
    Ok(())
}

fn fit_rect(source_width: f32, source_height: f32, target_width: f32, target_height: f32, fit: FitMode) -> Fit {
    match fit {
        FitMode::Stretch => Fit {
            scale_x: target_width / source_width,
            scale_y: target_height / source_height,
            offset_x: 0.0,
            offset_y: 0.0,
        },
        FitMode::Contain | FitMode::Cover => {
            let scale_x = target_width / source_width;
            let scale_y = target_height / source_height;
            let scale = match fit {
                FitMode::Contain => scale_x.min(scale_y),
                FitMode::Cover => scale_x.max(scale_y),
                FitMode::Stretch => unreachable!(),
            };
            let drawn_width = source_width * scale;
            let drawn_height = source_height * scale;

            Fit {
                scale_x: scale,
                scale_y: scale,
                offset_x: (target_width - drawn_width) / 2.0,
                offset_y: (target_height - drawn_height) / 2.0,
            }
        }
    }
}

fn read_capped(path: &Path) -> ImageResult<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_SVG_BYTES as u64 {
        return Err(ImageError::TooLarge(format!("svg is {} bytes, cap is {}", metadata.len(), MAX_SVG_BYTES)));
    }

    Ok(std::fs::read(path)?)
}

fn svg_options() -> usvg::Options<'static> {
    let mut options = usvg::Options::default();
    options.resources_dir = None;
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: Box::new(|_, _, _| None),
        resolve_string: Box::new(|_, _| None),
    };
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::image::core::{Color, MAX_DIM};

    const RED_RECT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="red"/></svg>"#;
    const VIEWBOX_RECT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50" fill="red"/></svg>"#;

    #[test]
    fn svg_parses_size() {
        let svg = SvgImage::from_bytes(VIEWBOX_RECT.as_bytes()).unwrap();
        assert_eq!(svg.size(), (100.0, 50.0));
    }

    #[test]
    fn svg_rejects_malformed_and_oversized_input() {
        assert!(SvgImage::from_bytes(b"<svg><").is_err());
        assert!(SvgImage::from_bytes(&vec![b' '; MAX_SVG_BYTES + 1]).is_err());
    }

    #[test]
    fn stretch_draws_svg_into_target_box() {
        let svg = SvgImage::from_bytes(RED_RECT.as_bytes()).unwrap();
        let mut canvas = Canvas::new(120, 80, Color::TRANSPARENT).unwrap();
        canvas
            .draw_svg(
                &svg,
                Placement {
                    x: 10.0,
                    y: 10.0,
                    width: 80.0,
                    height: 40.0,
                    fit: FitMode::Stretch,
                    opacity: 1.0,
                },
            )
            .unwrap();

        let center = canvas.pixel_rgba(50, 30).unwrap();
        assert!(center.r > 240);
        assert!(center.g < 10);
        assert!(center.b < 10);
        assert_eq!(canvas.pixel_rgba(0, 0).unwrap().a, 0);
    }

    #[test]
    fn contain_letterboxes_and_cover_clips() {
        let svg = SvgImage::from_bytes(RED_RECT.as_bytes()).unwrap();

        let mut contain = Canvas::new(100, 100, Color::TRANSPARENT).unwrap();
        contain
            .draw_svg(
                &svg,
                Placement {
                    width: 100.0,
                    height: 100.0,
                    fit: FitMode::Contain,
                    opacity: 1.0,
                    ..Placement::default()
                },
            )
            .unwrap();
        assert_eq!(contain.pixel_rgba(50, 5).unwrap().a, 0);
        assert!(contain.pixel_rgba(50, 50).unwrap().r > 240);

        let mut cover = Canvas::new(100, 100, Color::TRANSPARENT).unwrap();
        cover
            .draw_svg(
                &svg,
                Placement {
                    width: 100.0,
                    height: 100.0,
                    fit: FitMode::Cover,
                    opacity: 1.0,
                    ..Placement::default()
                },
            )
            .unwrap();
        assert!(cover.pixel_rgba(50, 5).unwrap().r > 240);
    }

    #[test]
    fn opacity_affects_composited_alpha() {
        let svg = SvgImage::from_bytes(RED_RECT.as_bytes()).unwrap();
        let mut canvas = Canvas::new(20, 20, Color::TRANSPARENT).unwrap();
        canvas
            .draw_svg(
                &svg,
                Placement {
                    width: 20.0,
                    height: 20.0,
                    fit: FitMode::Stretch,
                    opacity: 0.5,
                    ..Placement::default()
                },
            )
            .unwrap();

        let px = canvas.pixel_rgba(10, 10).unwrap();
        assert!((120..=136).contains(&px.a));
    }

    #[test]
    fn placement_math_matches_fit_modes() {
        assert_eq!(
            fit_rect(100.0, 50.0, 200.0, 200.0, FitMode::Contain),
            Fit { scale_x: 2.0, scale_y: 2.0, offset_x: 0.0, offset_y: 50.0 }
        );
        assert_eq!(
            fit_rect(100.0, 50.0, 200.0, 200.0, FitMode::Cover),
            Fit { scale_x: 4.0, scale_y: 4.0, offset_x: -100.0, offset_y: 0.0 }
        );
        assert_eq!(
            fit_rect(100.0, 50.0, 200.0, 200.0, FitMode::Stretch),
            Fit { scale_x: 2.0, scale_y: 4.0, offset_x: 0.0, offset_y: 0.0 }
        );
    }

    #[test]
    fn placement_rejects_bad_dimensions() {
        let svg = SvgImage::from_bytes(RED_RECT.as_bytes()).unwrap();
        let mut canvas = Canvas::new(20, 20, Color::TRANSPARENT).unwrap();
        assert!(
            canvas
                .draw_svg(
                    &svg,
                    Placement {
                        width: (MAX_DIM + 1) as f32,
                        height: 10.0,
                        ..Placement::default()
                    },
                )
                .is_err()
        );
    }
}
