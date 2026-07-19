use crate::lib::image::{Align, Canvas, Color, Font, ImageResult, TextOptions};
use crate::lib::image::core::MAX_DIM;
use crate::lib::mpeg::studio::{StudioRenderTrack, StudioTrackMode};
use std::cmp::{max, min};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineTrack {
    pub id: u64,
    pub name: String,
    pub mode: StudioTrackMode,
    pub offset_ms: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineSpec {
    pub duration_ms: u64,
    pub tracks: Vec<TimelineTrack>,
}

impl TimelineSpec {
    pub fn from_tracks(duration_ms: u64, tracks: &[StudioRenderTrack]) -> Self {
        Self {
            duration_ms,
            tracks: tracks.iter().map(|track| TimelineTrack {
                id: track.id,
                name: track.display_name.clone(),
                mode: track.mode,
                offset_ms: track.offset_ms,
                duration_ms: track.duration_ms,
            }).collect(),
        }
    }
}

pub fn render_timeline(spec: &TimelineSpec) -> ImageResult<Vec<u8>> {
    let duration_ms = spec.duration_ms.max(1);
    let width = min(MAX_DIM, max(900, (duration_ms / 1000).saturating_mul(3).saturating_add(240)) as u32);
    let lanes = spec.tracks.len().saturating_add(1);
    let height = min(MAX_DIM, max(170, lanes.saturating_mul(58).saturating_add(74)) as u32);
    let mut canvas = Canvas::new(width, height, Color { r: 18, g: 22, b: 30, a: 255 })?;
    let font = Font::fallback();
    let left = 190.0_f32;
    let right = 24.0_f32;
    let timeline_width = (width as f32 - left - right).max(1.0);
    let scale = timeline_width / duration_ms as f32;

    canvas.fill_rect(left, 0.0, timeline_width, 44.0, Color { r: 31, g: 39, b: 52, a: 255 });
    canvas.draw_text("Pandora Studio timeline", &font, &TextOptions {
        x: 18.0, y: 14.0, size: 22.0, color: Color::WHITE, ..TextOptions::default()
    })?;
    canvas.draw_text(&format_duration(duration_ms), &font, &TextOptions {
        x: width as f32 - right, y: 16.0, size: 16.0, color: Color { r: 190, g: 202, b: 218, a: 255 },
        align: Align::Right, ..TextOptions::default()
    })?;

    let tick_ms = tick_step(duration_ms, timeline_width);
    let mut tick = 0u64;
    while tick <= duration_ms {
        let x = left + tick as f32 * scale;
        canvas.fill_rect(x, 44.0, 1.0, height as f32 - 44.0, Color { r: 54, g: 64, b: 80, a: 255 });
        canvas.draw_text(&format_duration(tick), &font, &TextOptions {
            x: x + 4.0, y: 47.0, size: 12.0, color: Color { r: 174, g: 185, b: 201, a: 255 }, ..TextOptions::default()
        })?;
        if tick > duration_ms.saturating_sub(tick_ms) { break; }
        tick = tick.saturating_add(tick_ms);
    }

    draw_lane(&mut canvas, &font, "base audio", 0, left, scale, duration_ms, height, Color { r: 72, g: 139, b: 222, a: 220 })?;
    for (index, track) in spec.tracks.iter().enumerate() {
        let color = match track.mode {
            StudioTrackMode::Insert => Color { r: 64, g: 190, b: 130, a: 235 },
            StudioTrackMode::Override => Color { r: 222, g: 112, b: 91, a: 235 },
            StudioTrackMode::Duck => Color { r: 176, g: 116, b: 224, a: 235 },
        };
        let label = format!("#{} {} ({:?})", track.id, truncate(&track.name, 22), track.mode);
        draw_lane(&mut canvas, &font, &label, index + 1, left, scale, duration_ms, height, color)?;
        let start = track.offset_ms.min(duration_ms);
        let end = track.offset_ms.saturating_add(track.duration_ms).min(duration_ms);
        if end > start {
            let y = 75.0 + (index + 1) as f32 * 58.0;
            canvas.fill_rect(left + start as f32 * scale, y, (end - start) as f32 * scale, 25.0, color);
            canvas.draw_text(&format!("{} + {}", format_duration(start), format_duration(end - start)), &font, &TextOptions {
                x: left + start as f32 * scale + 5.0, y: y + 4.0, size: 13.0,
                color: Color::WHITE, max_width: Some(((end - start) as f32 * scale - 8.0).max(12.0)), ..TextOptions::default()
            })?;
        }
    }
    canvas.png_bytes()
}

fn draw_lane(canvas: &mut Canvas, font: &Font, label: &str, lane: usize, left: f32, scale: f32, duration_ms: u64, height: u32, color: Color) -> ImageResult<()> {
    let y = 75.0 + lane as f32 * 58.0;
    canvas.fill_rect(0.0, y - 4.0, left - 8.0, 33.0, Color { r: 25, g: 31, b: 42, a: 255 });
    canvas.draw_text(label, font, &TextOptions {
        x: 16.0, y, size: 14.0, color: Color { r: 224, g: 231, b: 242, a: 255 }, max_width: Some(left - 28.0), ..TextOptions::default()
    })?;
    canvas.fill_rect(left, y, duration_ms as f32 * scale, 25.0, Color { r: 38, g: 47, b: 61, a: 255 });
    if lane == 0 {
        canvas.fill_rect(left, y, duration_ms as f32 * scale, 25.0, color);
    }
    let _ = height;
    Ok(())
}

fn tick_step(duration_ms: u64, width: f32) -> u64 {
    let target = (duration_ms as f64 / (width as f64 / 100.0)).max(1.0);
    let magnitude = 10f64.powi(target.log10().floor() as i32);
    [1.0, 2.0, 5.0, 10.0].iter().map(|n| n * magnitude).find(|v| *v >= target).unwrap_or(10.0 * magnitude) as u64
}

fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 { format!("{}:{:02}:{:02}", hours, minutes, seconds) } else { format!("{}:{:02}", minutes, seconds) }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars { out.push('…'); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64, mode: StudioTrackMode, offset_ms: u64, duration_ms: u64) -> TimelineTrack {
        TimelineTrack { id, name: format!("track-{}", id), mode, offset_ms, duration_ms }
    }

    #[test]
    fn timeline_dimensions_are_bounded_and_lanes_are_stable() {
        let spec = TimelineSpec { duration_ms: 10_000, tracks: vec![t(4, StudioTrackMode::Insert, 2_000, 4_000), t(9, StudioTrackMode::Override, 8_000, 8_000)] };
        let png = render_timeline(&spec).unwrap();
        let pixmap = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!(pixmap.width(), 900);
        assert_eq!(pixmap.height(), 248);
    }

    #[test]
    fn empty_timeline_renders() {
        let png = render_timeline(&TimelineSpec { duration_ms: 60_000, tracks: vec![] }).unwrap();
        let pixmap = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert!(pixmap.width() <= MAX_DIM && pixmap.height() <= MAX_DIM);
    }

    #[test]
    fn sparse_or_clipped_tracks_do_not_fail() {
        let spec = TimelineSpec { duration_ms: 1_000, tracks: vec![t(1, StudioTrackMode::Insert, 900, 10_000), t(2, StudioTrackMode::Override, 2_000, 100)] };
        assert!(!render_timeline(&spec).unwrap().is_empty());
    }
}
