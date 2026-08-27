// Streaming yuv4mpeg reader.
//
// This is not test scaffolding: the chunk workers read their frames from an ffmpeg process's
// stdout, and y4m is that pipe's format. Reading is incremental so a worker can begin encoding
// while ffmpeg is still decoding ahead of it.

use std::io::{Read, Seek, SeekFrom};

pub struct Y4mReader<R: Read> {
    src: R,
    pub width: usize,
    pub height: usize,
    pub fps_num: u32,
    pub fps_den: u32,
    frame: Vec<u8>,
    // Byte offset of the first frame header, and the fixed per-frame stride, so a chunk worker
    // can seek straight to its own range instead of decoding everything before it.
    body_start: u64,
    frame_stride: u64,
}

// Frame planes borrowed from the reader's internal buffer, valid until the next read.
pub struct Frame<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub stride_y: usize,
    pub stride_c: usize,
}

fn read_line<R: Read>(src: &mut R) -> Result<Option<String>, String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match src.read(&mut b).map_err(|e| e.to_string())? {
            0 if out.is_empty() => return Ok(None),
            0 => return Err("truncated y4m line".into()),
            _ => {}
        }
        if b[0] == b'\n' {
            return Ok(Some(String::from_utf8_lossy(&out).into_owned()));
        }
        out.push(b[0]);
    }
}

impl<R: Read> Y4mReader<R> {
    pub fn new(mut src: R) -> Result<Self, String> {
        let hdr = read_line(&mut src)?.ok_or("empty y4m stream")?;
        if !hdr.starts_with("YUV4MPEG2") {
            return Err(format!("not a y4m stream: {hdr}"));
        }
        let (mut width, mut height, mut fps_num, mut fps_den) = (0usize, 0usize, 0u32, 1u32);
        for tag in hdr.split_whitespace().skip(1) {
            let (k, v) = tag.split_at(1);
            match k {
                "W" => width = v.parse().map_err(|_| "bad W tag")?,
                "H" => height = v.parse().map_err(|_| "bad H tag")?,
                "F" => {
                    let (n, d) = v.split_once(':').ok_or("bad F tag")?;
                    fps_num = n.parse().map_err(|_| "bad F numerator")?;
                    fps_den = d.parse().map_err(|_| "bad F denominator")?;
                }
                // Only 4:2:0 is relevant: every CPU_* preset burns subtitles through
                // format=yuv420p before the encoder sees a frame.
                "C" if !v.starts_with("420") => return Err(format!("unsupported colorspace {v}")),
                _ => {}
            }
        }
        if width == 0 || height == 0 || fps_num == 0 {
            return Err(format!("incomplete y4m header: {hdr}"));
        }
        let ysize = width * height;
        let csize = width.div_ceil(2) * height.div_ceil(2);
        // +1 for the newline terminating the FRAME marker. Frame headers may legally carry
        // parameters, but nothing in this pipeline emits them; seek_to_frame re-validates the
        // marker so a stream that does will fail loudly rather than decode garbage.
        let frame_stride = (ysize + 2 * csize + "FRAME".len() + 1) as u64;
        Ok(Self {
            src, width, height, fps_num, fps_den,
            frame: vec![0u8; ysize + 2 * csize],
            body_start: (hdr.len() + 1) as u64,
            frame_stride,
        })
    }

    // Ok(None) at end of stream.
    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>, String> {
        let Some(marker) = read_line(&mut self.src)? else {
            return Ok(None);
        };
        if !marker.starts_with("FRAME") {
            return Err(format!("expected FRAME marker, got {marker:?}"));
        }
        self.src.read_exact(&mut self.frame).map_err(|e| e.to_string())?;
        let ysize = self.width * self.height;
        let csize = self.width.div_ceil(2) * self.height.div_ceil(2);
        let (y, rest) = self.frame.split_at(ysize);
        let (u, v) = rest.split_at(csize);
        Ok(Some(Frame {
            y, u, v,
            stride_y: self.width,
            stride_c: self.width.div_ceil(2),
        }))
    }
}

impl<R: Read + Seek> Y4mReader<R> {
    // Position the reader at a frame index, for chunk workers that own one frame range each.
    pub fn seek_to_frame(&mut self, index: u64) -> Result<(), String> {
        self.src
            .seek(SeekFrom::Start(self.body_start + index * self.frame_stride))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
