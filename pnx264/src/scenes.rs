// A CRF per scene in front of an encoder: Caramel Analysis decides each scene's CRF from its first
// frames, and the encoder is switched to it on the scene's first frame.
//
// `SceneSchedule` sits between the decoder and `Encoder`. Every decoded frame is pushed in; a frame
// comes back out once its scene's CRF is known, together with the CRF to switch to when it is the
// first frame of a scene whose CRF differs from the last. Only a scene's first `analysis_frames`
// frames are ever held back, and after them the two the decider is still weighing as a cut (owned
// copies, ~3 MB each at 1080p), so the delay and the memory are bounded by that one number whatever
// the scene's length.

use crate::y4m;
use caramel_kernel::{EncodeContext, Frame, Plane, SceneConfig, SceneDecider, SourceContext};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SceneCrf {
    // The anchor is the encoder's own `crf`; everything else the model needs about the encode.
    pub output_height: Option<u32>,
    pub output_bit_depth: u8,
    pub scenes: SceneConfig,
    // What the container says about the source beyond its frames — bitrate, codec, bit depth.
    // Size and frame rate come from the frames themselves.
    pub source_bitrate: Option<f64>,
    pub source_codec: Option<String>,
    pub source_bit_depth: u8,
}

impl Default for SceneCrf {
    fn default() -> Self {
        Self {
            output_height: None,
            output_bit_depth: 8,
            scenes: SceneConfig::default(),
            source_bitrate: None,
            source_codec: None,
            source_bit_depth: 8,
        }
    }
}

// The one file in an AOT directory that says how its chunks decided their scenes. The planner
// writes it before launching any chunk, and the encode that takes the chunks over reads it, so a
// chunk it encodes itself decides exactly as the reused ones did — the final encode can see the
// whole file and might otherwise read a bitrate the download could not.
pub const CONTEXT_FILE: &str = "caramel.context";

impl SceneCrf {
    // As one `key=value:...` string, for a command line or the context file.
    pub fn to_arg(&self) -> String {
        let mut pairs = vec![
            format!("depth={}", self.output_bit_depth),
            format!("analysis={}", self.scenes.analysis_frames),
            format!("min={}", self.scenes.min_scene_frames),
            format!("max={}", self.scenes.max_scene_frames),
            format!("source-depth={}", self.source_bit_depth),
        ];
        if let Some(height) = self.output_height {
            pairs.push(format!("height={height}"));
        }
        if let Some(bitrate) = self.source_bitrate {
            pairs.push(format!("bitrate={bitrate}"));
        }
        if let Some(codec) = self.source_codec.as_deref().filter(|codec| codec.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            pairs.push(format!("codec={codec}"));
        }
        pairs.join(":")
    }

    // The inverse of `to_arg`. Unknown keys are refused rather than skipped: a chunk encoder that
    // half-understood its instructions would decide scenes unlike its siblings.
    pub fn from_arg(text: &str) -> Result<Self, String> {
        let mut value = Self::default();
        for pair in text.trim().split(':').filter(|pair| !pair.is_empty()) {
            let (key, raw) = pair.split_once('=').ok_or_else(|| format!("`{pair}` is not key=value"))?;
            let number = || raw.parse::<u64>().map_err(|_| format!("`{pair}` is not a number"));
            match key {
                "depth" => value.output_bit_depth = number()? as u8,
                "analysis" => value.scenes.analysis_frames = number()? as usize,
                "min" => value.scenes.min_scene_frames = number()? as usize,
                "max" => value.scenes.max_scene_frames = number()? as usize,
                "source-depth" => value.source_bit_depth = number()? as u8,
                "height" => value.output_height = Some(number()? as u32),
                "bitrate" => value.source_bitrate = Some(raw.parse().map_err(|_| format!("`{pair}` is not a number"))?),
                "codec" => value.source_codec = Some(raw.to_string()),
                _ => return Err(format!("unknown caramel setting `{key}`")),
            }
        }
        value.scenes = value.scenes.sanitised();
        Ok(value)
    }

    fn contexts(&self, width: usize, height: usize, fps: f64, anchor: f32) -> (SourceContext, EncodeContext) {
        (
            SourceContext {
                width: width as u32,
                height: height as u32,
                fps,
                video_bitrate: self.source_bitrate,
                codec: self.source_codec.clone(),
                bit_depth: self.source_bit_depth,
            },
            EncodeContext {
                output_height: self.output_height,
                output_bit_depth: self.output_bit_depth,
                ..EncodeContext::anchored(anchor)
            },
        )
    }
}

pub struct OwnedFrame {
    pub index: u64,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub stride_y: usize,
    pub stride_c: usize,
}

// A scene as it was encoded: its first frame (relative to what was pushed) and its CRF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneMark {
    pub start: u64,
    pub crf: f32,
}

pub struct SceneSchedule {
    decider: Option<SceneDecider>,
    pending: VecDeque<OwnedFrame>,
    upcoming: VecDeque<SceneMark>,
    current: f32,
    width: usize,
    height: usize,
    pushed: u64,
    marks: Vec<SceneMark>,
}

impl SceneSchedule {
    // `anchor` is the CRF the encoder was opened at.
    pub fn new(caramel: &SceneCrf, width: usize, height: usize, fps_num: u32, fps_den: u32, anchor: f32) -> Self {
        let fps = if fps_den == 0 { 0.0 } else { fps_num as f64 / fps_den as f64 };
        let (source, encode) = caramel.contexts(width, height, fps, anchor);
        Self {
            decider: Some(SceneDecider::new(caramel.scenes, source, encode)),
            pending: VecDeque::new(),
            upcoming: VecDeque::new(),
            current: anchor,
            width,
            height,
            pushed: 0,
            marks: Vec::new(),
        }
    }

    pub fn push(&mut self, frame: &y4m::Frame) -> Result<(), String> {
        let (chroma_width, chroma_height) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let y = Plane::new(frame.y, self.width, self.height, frame.stride_y).ok_or("short luma plane")?;
        let u = Plane::new(frame.u, chroma_width, chroma_height, frame.stride_c);
        let v = Plane::new(frame.v, chroma_width, chroma_height, frame.stride_c);
        if let Some(decider) = self.decider.as_mut() {
            for decision in decider.push(&Frame { y, u, v }) {
                self.upcoming.push_back(SceneMark { start: decision.start_frame, crf: decision.crf });
            }
        }
        self.pending.push_back(OwnedFrame {
            index: self.pushed,
            y: frame.y.to_vec(),
            u: frame.u.to_vec(),
            v: frame.v.to_vec(),
            stride_y: frame.stride_y,
            stride_c: frame.stride_c,
        });
        self.pushed += 1;
        Ok(())
    }

    // The stream ended: whatever is still held back is decided now.
    pub fn finish(&mut self) {
        if let Some(last) = self.decider.take().and_then(SceneDecider::finish) {
            self.upcoming.push_back(SceneMark { start: last.start_frame, crf: last.crf });
        }
    }

    // The next frame whose CRF is known, and the CRF to switch to before encoding it, if any.
    pub fn ready(&mut self) -> Option<(OwnedFrame, Option<f32>)> {
        let decided = match &self.decider {
            Some(decider) => decider.decided_frames(),
            None => self.pushed,
        };
        if self.pending.front()?.index >= decided {
            return None;
        }
        let frame = self.pending.pop_front()?;
        let mut switch = None;
        while self.upcoming.front().is_some_and(|mark| mark.start <= frame.index) {
            let mark = self.upcoming.pop_front()?;
            self.marks.push(mark);
            if mark.crf != self.current {
                self.current = mark.crf;
                switch = Some(mark.crf);
            }
        }
        Some((frame, switch))
    }

    // Every scene this schedule released, in order.
    pub fn marks(&self) -> &[SceneMark] {
        &self.marks
    }
}

// A chunk's scenes, beside the chunk: `<chunk>.scenes`, one `start|crf` per line. Written by
// whichever process encoded the chunk, so the encode that reuses it can still say which CRFs the
// release carries.
pub fn sidecar_path(chunk: &Path) -> PathBuf {
    let mut name = chunk.as_os_str().to_owned();
    name.push(".scenes");
    PathBuf::from(name)
}

pub fn write_sidecar(chunk: &Path, marks: &[SceneMark]) -> std::io::Result<()> {
    let text: String = marks.iter().map(|mark| format!("{}|{}\n", mark.start, mark.crf)).collect();
    std::fs::write(sidecar_path(chunk), text)
}

pub fn read_sidecar(chunk: &Path) -> Option<Vec<SceneMark>> {
    let text = std::fs::read_to_string(sidecar_path(chunk)).ok()?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (start, crf) = line.split_once('|')?;
            Some(SceneMark { start: start.parse().ok()?, crf: crf.parse().ok()? })
        })
        .collect()
}

// The CRFs a whole encode carried, from every chunk's scenes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneCrfSummary {
    pub scenes: usize,
    pub frames: u64,
    pub min_crf: f32,
    pub max_crf: f32,
    // Weighted by frames, which is what the bitrate follows.
    pub mean_crf: f64,
    // Chunks whose scenes are not known: reused from an encode that kept no sidecar.
    pub unknown_chunks: usize,
}

impl SceneCrfSummary {
    // `chunks` is each chunk's length and its scenes, in order.
    pub fn from_chunks<'a>(chunks: impl IntoIterator<Item = (u64, Option<&'a [SceneMark]>)>) -> Self {
        let mut summary = Self { min_crf: f32::INFINITY, max_crf: f32::NEG_INFINITY, ..Default::default() };
        let mut weighted = 0.0;
        for (length, marks) in chunks {
            let Some(marks) = marks.filter(|marks| !marks.is_empty()) else {
                summary.unknown_chunks += 1;
                continue;
            };
            for (index, mark) in marks.iter().enumerate() {
                let end = marks.get(index + 1).map_or(length, |next| next.start.min(length));
                let frames = end.saturating_sub(mark.start);
                summary.scenes += 1;
                summary.frames += frames;
                weighted += mark.crf as f64 * frames as f64;
                summary.min_crf = summary.min_crf.min(mark.crf);
                summary.max_crf = summary.max_crf.max(mark.crf);
            }
        }
        if summary.scenes == 0 {
            summary.min_crf = 0.0;
            summary.max_crf = 0.0;
        } else if summary.frames > 0 {
            summary.mean_crf = weighted / summary.frames as f64;
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 64;
    const H: usize = 48;

    fn yuv(fill: impl Fn(usize, usize) -> u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..W * H).map(|i| fill(i % W, i / W)).collect();
        (y, vec![128; W * H / 4], vec![128; W * H / 4])
    }

    fn push(schedule: &mut SceneSchedule, planes: &(Vec<u8>, Vec<u8>, Vec<u8>)) {
        schedule
            .push(&y4m::Frame { y: &planes.0, u: &planes.1, v: &planes.2, stride_y: W, stride_c: W / 2 })
            .unwrap();
    }

    // Frames come out in order, each exactly once, only after their scene was decided, and the
    // switch lands on the first frame of the scene that changed the CRF.
    #[test]
    fn frames_leave_in_order_with_their_scene_crf() {
        let caramel = SceneCrf {
            scenes: SceneConfig { analysis_frames: 4, min_scene_frames: 4, max_scene_frames: 250 },
            ..Default::default()
        };
        let mut schedule = SceneSchedule::new(&caramel, W, H, 24, 1, 17.0);
        let smooth = yuv(|x, y| (60 + x + y) as u8);
        let checker = yuv(|x, y| if (x / 8 + y / 8) % 2 == 0 { 16 } else { 235 });
        let mut out = Vec::new();
        for index in 0..20 {
            push(&mut schedule, if index < 10 { &smooth } else { &checker });
            while let Some(ready) = schedule.ready() {
                out.push((ready.0.index, ready.1));
            }
        }
        schedule.finish();
        while let Some(ready) = schedule.ready() {
            out.push((ready.0.index, ready.1));
        }
        assert_eq!(out.iter().map(|(index, _)| *index).collect::<Vec<_>>(), (0..20).collect::<Vec<_>>());
        let marks = schedule.marks().to_vec();
        assert_eq!(marks.iter().map(|mark| mark.start).collect::<Vec<_>>(), vec![0, 10]);
        // A switch exactly where a scene's CRF differs from the one before it, and nowhere else.
        let mut current = 17.0;
        for (index, switch) in out {
            let expected = marks.iter().find(|mark| mark.start == index).and_then(|mark| {
                let changed = mark.crf != current;
                current = mark.crf;
                changed.then_some(mark.crf)
            });
            assert_eq!(switch, expected, "frame {index}");
        }
    }

    #[test]
    fn settings_round_trip_through_their_text() {
        let settings = SceneCrf {
            output_height: Some(720),
            output_bit_depth: 8,
            scenes: SceneConfig { analysis_frames: 8, min_scene_frames: 10, max_scene_frames: 200 },
            source_bitrate: Some(4_500_000.5),
            source_codec: Some("hevc".into()),
            source_bit_depth: 10,
        };
        let parsed = SceneCrf::from_arg(&settings.to_arg()).unwrap();
        assert_eq!(parsed.to_arg(), settings.to_arg());
        assert_eq!((parsed.source_bitrate, parsed.scenes), (settings.source_bitrate, settings.scenes));
        assert_eq!(SceneCrf::from_arg(&SceneCrf::default().to_arg()).unwrap().to_arg(), SceneCrf::default().to_arg());
        assert!(SceneCrf::from_arg("depth=8:mystery=1").is_err());
    }

    #[test]
    fn sidecars_round_trip_and_summaries_weigh_by_frames() {
        let directory = std::env::temp_dir().join(format!("pnx264-scenes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let chunk = directory.join("chunk-000000000000-000000000100.264");
        let marks = vec![SceneMark { start: 0, crf: 16.0 }, SceneMark { start: 25, crf: 19.5 }];
        write_sidecar(&chunk, &marks).unwrap();
        assert_eq!(read_sidecar(&chunk), Some(marks.clone()));
        std::fs::remove_dir_all(&directory).ok();

        let summary = SceneCrfSummary::from_chunks([(100, Some(marks.as_slice())), (50, None)]);
        assert_eq!((summary.scenes, summary.frames, summary.unknown_chunks), (2, 100, 1));
        assert_eq!((summary.min_crf, summary.max_crf), (16.0, 19.5));
        assert!((summary.mean_crf - (16.0 * 25.0 + 19.5 * 75.0) / 100.0).abs() < 1e-9);
    }
}
