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
    Duck,
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
    #[serde(default = "default_track_volume_percent")]
    pub volume_percent: u16,
    #[serde(default = "default_duck_volume_percent")]
    pub duck_volume_percent: u8,
    #[serde(default)]
    pub fade_ms: u64,
    #[serde(default)]
    pub trim_start_ms: u64,
    #[serde(default)]
    pub trim_end_ms: u64,
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

    pub fn around_track_start(track_start_ms: u64, total_duration_ms: u64) -> Self {
        let start_ms = track_start_ms.saturating_sub(2_000).min(total_duration_ms);
        let end_ms = track_start_ms.saturating_add(30_000).min(total_duration_ms);
        let duration_ms = end_ms.saturating_sub(start_ms);
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

fn default_duck_volume_percent() -> u8 {
    100
}

fn default_track_volume_percent() -> u16 {
    100
}

fn seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn signed_seconds(ms: i128) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn duck_volume_filter(track: &StudioRenderTrack, render_start: u64) -> String {
    let target = track.duck_volume_percent.min(100) as f64 / 100.0;
    let start_ms = track.offset_ms as i128 - render_start as i128;
    let end_ms = start_ms + track.duration_ms as i128;
    let fade_ms = track.fade_ms.min(track.duration_ms / 2);
    let start = signed_seconds(start_ms);
    let end = signed_seconds(end_ms);
    if fade_ms == 0 {
        return format!("volume={:.4}:enable='between(t,{},{})'", target, start, end);
    }

    let down_end = signed_seconds(start_ms + fade_ms as i128);
    let up_start_ms = end_ms - fade_ms as i128;
    let up_start = signed_seconds(up_start_ms);
    let fade = seconds(fade_ms);
    format!(
        "volume='if(lt(t,{start}),1,if(lt(t,{down_end}),1+({target:.4}-1)*(t-{start})/{fade},if(lt(t,{up_start}),{target:.4},if(lt(t,{end}),{target:.4}+(1-{target:.4})*(t-{up_start})/{fade},1))))':eval=frame"
    )
}

fn apply_ducking(
    graph: &mut Vec<String>,
    manifest: &StudioRenderManifest,
    initial_label: String,
    excluded_track_id: Option<u64>,
    render_start: u64,
    label_prefix: &str,
) -> String {
    let mut label = initial_label;
    for duck in manifest.tracks.iter().filter(|track| {
        track.mode == StudioTrackMode::Duck
            && Some(track.id) != excluded_track_id
            && track.duck_volume_percent < 100
    }) {
        let next = format!("[{}-duck-{}]", label_prefix, duck.id);
        graph.push(format!("{}{}{}", label, duck_volume_filter(duck, render_start), next));
        label = next;
    }
    label
}

pub fn build_studio_audio_filter(manifest: &StudioRenderManifest) -> String {
    let preview = manifest.preview;
    let render_start = preview.map(|p| p.start_ms).unwrap_or(0);
    let render_duration = manifest.render_duration_ms();
    let end = render_start.saturating_add(render_duration);
    let base_trim_start = if preview.is_some() { 0 } else { render_start };
    let mut graph = Vec::new();

    let base_raw = "[studio-base-raw]".to_string();
    if manifest.sources.first().map(|s| s.has_audio).unwrap_or(false) {
        graph.push(format!(
            "[0:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=start={}:duration={},asetpts=PTS-STARTPTS{}",
            seconds(base_trim_start), seconds(render_duration), base_raw
        ));
    } else {
        graph.push(format!(
            "anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration={},asetpts=PTS-STARTPTS{}",
            seconds(render_duration), base_raw
        ));
    }

    let mut base_label = base_raw;
    for track in &manifest.tracks {
        if track.mode != StudioTrackMode::Override {
            continue;
        }
        let start = track.offset_ms.max(render_start);
        let stop = track.offset_ms.saturating_add(track.duration_ms).min(end);
        if stop <= start {
            continue;
        }
        let next = format!("[studio-mute-{}]", track.id);
        graph.push(format!(
            "{}volume=enable='between(t,{}, {})':volume=0{}",
            base_label,
            seconds(start.saturating_sub(render_start)),
            seconds(stop.saturating_sub(render_start)),
            next,
        ));
        base_label = next;
    }
    base_label = apply_ducking(&mut graph, manifest, base_label, None, render_start, "studio-base");

    let mut inputs = vec![base_label];
    for (idx, track) in manifest.tracks.iter().enumerate() {
        let start = track.offset_ms.max(render_start);
        let stop = track.offset_ms.saturating_add(track.duration_ms).min(end);
        if stop <= start {
            continue;
        }
        let track_start = track.trim_start_ms.saturating_add(start.saturating_sub(track.offset_ms));
        let track_duration = stop.saturating_sub(start);
        let delay = start.saturating_sub(render_start);
        let raw_label = format!("[studio-track-{}-raw]", track.id);
        graph.push(format!(
            "[{}:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=start={}:duration={},asetpts=PTS-STARTPTS,adelay={}|{}{}",
            idx + 1,
            seconds(track_start),
            seconds(track_duration),
            delay,
            delay,
            raw_label,
        ));
        let own_volume_label = if track.volume_percent == 100 {
            raw_label
        } else {
            let label = format!("[studio-track-{}-volume]", track.id);
            graph.push(format!(
                "[studio-track-{}-raw]volume={:.4}{}",
                track.id,
                track.volume_percent.min(200) as f64 / 100.0,
                label,
            ));
            label
        };
        let label = apply_ducking(
            &mut graph,
            manifest,
            own_volume_label,
            Some(track.id),
            render_start,
            &format!("studio-track-{}", track.id),
        );
        inputs.push(label);
    }

    let mix_inputs = inputs.concat();
    graph.push(format!(
        "{}amix=inputs={}:duration=longest:dropout_transition=0,alimiter=limit=0.95,atrim=duration={},asetpts=PTS-STARTPTS[studio-aout]",
        mix_inputs, inputs.len(), seconds(render_duration)
    ));
    graph.join(";")
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
        StudioRenderTrack {
            id,
            path: PathBuf::from(format!("track{}.ogg", id)),
            mode,
            offset_ms,
            duration_ms,
            display_name: "x".into(),
            volume_percent: 100,
            duck_volume_percent: 100,
            fade_ms: 0,
            trim_start_ms: 0,
            trim_end_ms: 0,
        }
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
    fn track_start_preview_has_two_second_preroll_and_thirty_seconds_after() {
        assert_eq!(PreviewWindow::around_track_start(5_000, 60_000), PreviewWindow { start_ms: 3_000, duration_ms: 32_000 });
        assert_eq!(PreviewWindow::around_track_start(1_000, 60_000), PreviewWindow { start_ms: 0, duration_ms: 31_000 });
        assert_eq!(PreviewWindow::around_track_start(50_000, 60_000), PreviewWindow { start_ms: 48_000, duration_ms: 12_000 });
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

    #[test]
    fn old_track_json_defaults_to_no_ducking() {
        let raw = r#"{"id":1,"path":"track.ogg","mode":"Insert","offset_ms":0,"duration_ms":1000,"display_name":"x"}"#;
        let parsed: StudioRenderTrack = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.volume_percent, 100);
        assert_eq!(parsed.duck_volume_percent, 100);
        assert_eq!(parsed.fade_ms, 0);
        assert_eq!(parsed.trim_start_ms, 0);
        assert_eq!(parsed.trim_end_ms, 0);
    }

    #[test]
    fn own_track_volume_is_applied_before_mix() {
        let mut m = manifest();
        let mut quieter = track(1, StudioTrackMode::Insert, 5_000, 10_000);
        quieter.volume_percent = 40;
        m.tracks = vec![quieter];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("[studio-track-1-raw]volume=0.4000[studio-track-1-volume]"));
        assert!(graph.contains("[studio-base-raw][studio-track-1-volume]amix"));
    }

    #[test]
    fn trimmed_track_starts_from_retained_audio() {
        let mut m = manifest();
        let mut trimmed = track(1, StudioTrackMode::Override, 10_000, 5_000);
        trimmed.trim_start_ms = 2_500;
        trimmed.trim_end_ms = 1_000;
        m.tracks = vec![trimmed];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("atrim=start=2.500:duration=5.000"));
        assert!(graph.contains("between(t,10.000, 15.000)"));
    }

    #[test]
    fn duck_fades_all_other_inputs_but_not_itself() {
        let mut m = manifest();
        let insert = track(1, StudioTrackMode::Insert, 0, 30_000);
        let mut duck = track(2, StudioTrackMode::Duck, 10_000, 10_000);
        duck.duck_volume_percent = 25;
        duck.fade_ms = 2_000;
        m.tracks = vec![insert, duck];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("[studio-base-raw]volume='if(lt(t,10.000)"));
        assert!(graph.contains("[studio-track-1-raw]volume='if(lt(t,10.000)"));
        assert!(!graph.contains("[studio-track-2-raw]volume='if(lt(t,10.000)"));
        assert!(graph.contains("lt(t,12.000)"));
        assert!(graph.contains("lt(t,18.000),0.2500"));
        assert!(graph.contains("lt(t,20.000)"));
    }

    #[test]
    fn duck_fade_is_clamped_to_half_the_track_duration() {
        let mut m = manifest();
        let mut duck = track(1, StudioTrackMode::Duck, 5_000, 2_000);
        duck.duck_volume_percent = 0;
        duck.fade_ms = 5_000;
        m.tracks = vec![duck];
        let graph = build_studio_audio_filter(&m);
        assert!(graph.contains("lt(t,6.000)"));
        assert!(graph.contains("(t-6.000)/1.000"));
    }
}
