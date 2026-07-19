use crate::lib::mpeg::core::FfmpegParams;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudioSourceKind {
    Encode,
    Backup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudioTrackMode {
    Insert,
    Override,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudioVideoPreset {
    Dummy,
    Standard,
    Gpu,
    PseudoLossless,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StudioInput {
    pub path: PathBuf,
    pub duration_ms: u64,
    pub has_audio: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StudioRenderTrack {
    pub id: u64,
    pub path: PathBuf,
    pub mode: StudioTrackMode,
    pub offset_ms: u64,
    pub duration_ms: u64,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct PreviewWindow {
    pub start_ms: u64,
    pub duration_ms: u64,
}

impl PreviewWindow {
    pub fn centered(center_ms: u64, total_duration_ms: u64) -> Self {
        let duration_ms = 30_000.min(total_duration_ms);
        let ideal_start = center_ms.saturating_sub(duration_ms / 2);
        let start_ms = ideal_start.min(total_duration_ms.saturating_sub(duration_ms));
        Self { start_ms, duration_ms }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StudioRenderManifest {
    pub sources: Vec<StudioInput>,
    pub tracks: Vec<StudioRenderTrack>,
    pub total_duration_ms: u64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub source_kind: StudioSourceKind,
    pub video_preset: StudioVideoPreset,
    pub preview: Option<PreviewWindow>,
}

impl StudioRenderManifest {
    pub fn render_duration_ms(&self) -> u64 {
        self.preview
            .map(|window| window.duration_ms)
            .unwrap_or(self.total_duration_ms)
    }

    pub fn is_video_copy(&self) -> bool {
        self.preview.is_none() && self.source_kind == StudioSourceKind::Encode
    }
}

pub fn ffconcat_escape_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "'\\''")
}

pub fn ffconcat_contents(inputs: &[StudioInput]) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    for input in inputs {
        out.push_str("file '");
        out.push_str(&ffconcat_escape_path(&input.path));
        out.push_str("'\n");
    }
    out
}

pub fn write_ffconcat(path: &Path, inputs: &[StudioInput]) -> Result<(), String> {
    std::fs::write(path, ffconcat_contents(inputs)).map_err(|e| e.to_string())
}

fn seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

pub fn build_studio_audio_filter(manifest: &StudioRenderManifest) -> String {
    let preview = manifest.preview;
    let render_start = preview.map(|p| p.start_ms).unwrap_or(0);
    let render_duration = manifest.render_duration_ms();
    let end = render_start.saturating_add(render_duration);
    let mut graph = String::new();

    if manifest.sources.first().map(|s| s.has_audio).unwrap_or(false) {
        graph.push_str("[0:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo");
    } else {
        graph.push_str(&format!(
            "anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration={}",
            seconds(render_duration)
        ));
    }
    let base_trim_start = if preview.is_some() { 0 } else { render_start };
    graph.push_str(&format!(",atrim=start={}:duration={}", seconds(base_trim_start), seconds(render_duration)));
    graph.push_str(",asetpts=PTS-STARTPTS");

    let mut base_label = "[studio-base]".to_string();
    graph.push_str(&base_label);
    for track in &manifest.tracks {
        if track.mode != StudioTrackMode::Override {
            continue;
        }
        let start = track.offset_ms.max(render_start);
        let stop = track.offset_ms.saturating_add(track.duration_ms).min(end);
        if stop <= start {
            continue;
        }
        let local_start = seconds(start.saturating_sub(render_start));
        let local_stop = seconds(stop.saturating_sub(render_start));
        let next = format!("[studio-mute-{}]", track.id);
        graph.push_str(&format!(
            ";{}volume=enable='between(t,{}, {})':volume=0{}",
            base_label, local_start, local_stop, next
        ));
        base_label = next;
    }

    let mut inputs = vec![base_label.clone()];
    for (idx, track) in manifest.tracks.iter().enumerate() {
        let start = track.offset_ms.max(render_start);
        let stop = track.offset_ms.saturating_add(track.duration_ms).min(end);
        if stop <= start {
            continue;
        }
        let track_start = start.saturating_sub(track.offset_ms);
        let track_duration = stop.saturating_sub(start);
        let delay = start.saturating_sub(render_start);
        let label = format!("[studio-track-{}]", track.id);
        graph.push_str(&format!(
            ";[{}:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=start={}:duration={},asetpts=PTS-STARTPTS,adelay={}|{}{}",
            idx + 1,
            seconds(track_start),
            seconds(track_duration),
            delay,
            delay,
            label
        ));
        inputs.push(label);
    }

    graph.push(';');
    for input in &inputs {
        graph.push_str(input);
    }
    graph.push_str(&format!(
        "amix=inputs={}:duration=longest:dropout_transition=0,alimiter=limit=0.95,atrim=duration={},asetpts=PTS-STARTPTS[studio-aout]",
        inputs.len(), seconds(render_duration)
    ));
    graph
}

pub fn studio_ffmpeg_params(
    manifest: &StudioRenderManifest,
    concat_path: &Path,
    output: &Path,
) -> Vec<FfmpegParams> {
    let mut params = Vec::new();
    if let Some(window) = manifest.preview {
        params.push(FfmpegParams::Seek(Cow::Owned(seconds(window.start_ms))));
    }
    params.extend([
        FfmpegParams::Format(Cow::Borrowed("concat")),
        FfmpegParams::Safe(Cow::Borrowed("0")),
        FfmpegParams::Input(Cow::Owned(concat_path.display().to_string())),
    ]);
    for track in &manifest.tracks {
        params.push(FfmpegParams::Input(Cow::Owned(track.path.display().to_string())));
    }
    params.extend([
        FfmpegParams::ComplexFilter(Cow::Owned(build_studio_audio_filter(manifest))),
        FfmpegParams::Map(Cow::Borrowed("0:v:0")),
        FfmpegParams::Map(Cow::Borrowed("[studio-aout]")),
    ]);
    if manifest.is_video_copy() {
        params.push(FfmpegParams::Cv(Cow::Borrowed("copy")));
    } else {
        params.push(FfmpegParams::BasicFilter(Cow::Borrowed("format=yuv420p")));
        match manifest.video_preset {
            StudioVideoPreset::Gpu => params.extend([
                FfmpegParams::Cv(Cow::Borrowed("h264_amf")),
                FfmpegParams::Profile(Cow::Borrowed("high")),
                FfmpegParams::Level(Cow::Borrowed("4.1")),
                FfmpegParams::QpI(Cow::Borrowed("15")),
                FfmpegParams::QpP(Cow::Borrowed("15")),
                FfmpegParams::Rc(Cow::Borrowed("cqp")),
                FfmpegParams::R(Cow::Borrowed("23.976")),
            ]),
            StudioVideoPreset::PseudoLossless => params.extend([
                FfmpegParams::Cv(Cow::Borrowed("libx264")),
                FfmpegParams::X264Params(Cow::Borrowed("me=umh:subme=8:merange=24:trellis=2:psy-rd=1:aq-strength=1.1:aq-mode=3")),
                FfmpegParams::Profile(Cow::Borrowed("high")),
                FfmpegParams::Level(Cow::Borrowed("4.1")),
                FfmpegParams::Crf(17),
                FfmpegParams::Preset(Cow::Borrowed("fast")),
            ]),
            StudioVideoPreset::Standard => params.extend([
                FfmpegParams::Cv(Cow::Borrowed("libx264")),
                FfmpegParams::X264Params(Cow::Borrowed("aq-strength=1.0:aq-mode=3")),
                FfmpegParams::Profile(Cow::Borrowed("high")),
                FfmpegParams::Level(Cow::Borrowed("4.1")),
                FfmpegParams::Crf(17),
                FfmpegParams::Preset(Cow::Borrowed("fast")),
            ]),
            StudioVideoPreset::Dummy => params.extend([
                FfmpegParams::Cv(Cow::Borrowed("libx264")),
                FfmpegParams::Profile(Cow::Borrowed("high")),
                FfmpegParams::Level(Cow::Borrowed("4.1")),
                FfmpegParams::Crf(25),
                FfmpegParams::Preset(Cow::Borrowed("veryfast")),
            ]),
        }
    }
    params.extend([
        FfmpegParams::Ca(Cow::Borrowed("aac")),
        FfmpegParams::Ba(Cow::Borrowed("192k")), 
        FfmpegParams::Movflags,
        FfmpegParams::NoStats,
        FfmpegParams::Progress(Cow::Borrowed("pipe:2")),
        FfmpegParams::Overwrite,
    ]);
    if let Some(window) = manifest.preview {
        params.push(FfmpegParams::Duration(Cow::Owned(seconds(window.duration_ms))));
    } else {
        params.push(FfmpegParams::Duration(Cow::Owned(seconds(manifest.total_duration_ms))));
    }
    params.push(FfmpegParams::Output(Cow::Owned(output.display().to_string())));
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str, audio: bool) -> StudioInput {
        StudioInput { path: PathBuf::from(path), duration_ms: 60_000, has_audio: audio }
    }

    fn track(id: u64, mode: StudioTrackMode, offset_ms: u64, duration_ms: u64) -> StudioRenderTrack {
        StudioRenderTrack { id, path: PathBuf::from(format!("track{}.ogg", id)), mode, offset_ms, duration_ms, display_name: "x".into() }
    }

    fn manifest() -> StudioRenderManifest {
        StudioRenderManifest {
            sources: vec![input("base.mkv", true)], tracks: vec![], total_duration_ms: 60_000,
            fps_num: 24, fps_den: 1, source_kind: StudioSourceKind::Encode,
            video_preset: StudioVideoPreset::Dummy, preview: None,
        }
    }

    #[test]
    fn preview_window_stays_full_length_at_video_edges() {
        assert_eq!(PreviewWindow::centered(39_000, 40_000), PreviewWindow { start_ms: 10_000, duration_ms: 30_000 });
        assert_eq!(PreviewWindow::centered(2_000, 40_000), PreviewWindow { start_ms: 0, duration_ms: 30_000 });
        assert_eq!(PreviewWindow::centered(2_000, 10_000), PreviewWindow { start_ms: 0, duration_ms: 10_000 });
    }

    #[test]
    fn concat_escapes_single_quotes_and_backslashes() {
        assert_eq!(ffconcat_contents(&[input("a\\b'c.mkv", true)]), "ffconcat version 1.0\nfile 'a\\\\b'\\''c.mkv'\n");
    }

    #[test]
    fn insert_delay_and_overlap_are_mixed() {
        let mut m = manifest();
        m.tracks = vec![track(1, StudioTrackMode::Insert, 10_000, 5_000), track(2, StudioTrackMode::Insert, 12_000, 5_000)];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("adelay=10000|10000"));
        assert!(graph.contains("adelay=12000|12000"));
        assert!(graph.contains("amix=inputs=3"));
    }

    #[test]
    fn override_mutes_base_and_preview_is_relative() {
        let mut m = manifest();
        m.tracks = vec![track(3, StudioTrackMode::Override, 20_000, 10_000)];
        m.preview = Some(PreviewWindow { start_ms: 15_000, duration_ms: 30_000 });
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("volume=enable='between(t,5.000, 15.000)':volume=0"));
        assert!(graph.contains("adelay=5000|5000"));
        assert!(graph.contains("atrim=start=0.000:duration=10.000"));
    }

    #[test]
    fn no_base_audio_uses_silence_and_end_clips() {
        let mut m = manifest();
        m.sources[0].has_audio = false;
        m.tracks = vec![track(1, StudioTrackMode::Insert, 59_000, 5_000)];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("anullsrc"));
        assert!(graph.contains("duration=1.000"));
        assert!(graph.contains("adelay=59000|59000"));
    }
}
