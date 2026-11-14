// Borrowed 8-bit picture planes. The kernel never decodes anything itself: whoever owns the
// decoder hands it planes, and a stride lets them hand over a decoder's own padded buffer
// without copying it first.

#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl<'a> Plane<'a> {
    // None when the buffer is too short for the geometry it claims, so a truncated read from a
    // decoder is refused here rather than panicking halfway through a frame.
    pub fn new(data: &'a [u8], width: usize, height: usize, stride: usize) -> Option<Self> {
        if width == 0 || height == 0 || stride < width {
            return None;
        }
        if data.len() < stride * (height - 1) + width {
            return None;
        }
        Some(Self { data, width, height, stride })
    }

    pub fn row(&self, y: usize) -> &'a [u8] {
        let start = y * self.stride;
        &self.data[start..start + self.width]
    }
}

// One 4:2:0 (or luma-only) frame. Chroma is optional because every luma feature stands on its own
// and a grey source has nothing to measure there.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub y: Plane<'a>,
    pub u: Option<Plane<'a>>,
    pub v: Option<Plane<'a>>,
}

impl<'a> Frame<'a> {
    pub fn luma(y: Plane<'a>) -> Self {
        Self { y, u: None, v: None }
    }

    // A tightly packed yuv420p buffer, the layout ffmpeg's rawvideo and y4m pipes produce.
    pub fn from_yuv420p(data: &'a [u8], width: usize, height: usize) -> Option<Self> {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let ysize = width * height;
        let csize = cw * ch;
        if data.len() < ysize + 2 * csize {
            return None;
        }
        Some(Self {
            y: Plane::new(&data[..ysize], width, height, width)?,
            u: Some(Plane::new(&data[ysize..ysize + csize], cw, ch, cw)?),
            v: Some(Plane::new(&data[ysize + csize..ysize + 2 * csize], cw, ch, cw)?),
        })
    }
}
