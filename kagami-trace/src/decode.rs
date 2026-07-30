use std::io::Cursor;

use image::{ImageReader, Limits};

use crate::{
    MAX_ENCODED_BYTES, MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS, Trace, TraceError, TraceOptions,
    TraceResult, trace_rgba,
};

pub fn trace_image(encoded: &[u8], options: &TraceOptions) -> TraceResult<Trace> {
    if encoded.is_empty() {
        return Err(TraceError::Decode("input is empty".to_string()));
    }
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(TraceError::TooLarge(format!(
            "{} encoded bytes exceeds the {} byte limit",
            encoded.len(),
            MAX_ENCODED_BYTES
        )));
    }

    let mut reader = ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|error| TraceError::Decode(error.to_string()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 8);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| TraceError::Decode(error.to_string()))?
        .into_rgba8();
    trace_rgba(decoded.width(), decoded.height(), decoded.as_raw(), options)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
    use std::io::Cursor;

    use super::*;

    #[test]
    fn png_bytes_can_be_traced() {
        let image =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(3, 2, Rgba([20, 40, 60, 255])));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let trace = trace_image(&bytes, &TraceOptions::default()).unwrap();

        assert_eq!((trace.width, trace.height), (3, 2));
        assert_eq!(trace.palette.len(), 1);
        assert_eq!(trace.path_count(), 1);
    }

    #[test]
    fn invalid_encoded_input_is_reported() {
        assert!(matches!(
            trace_image(b"not an image", &TraceOptions::default()),
            Err(TraceError::Decode(_))
        ));
    }
}
