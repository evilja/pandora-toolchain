// The `Caramel` job: download a source exactly as `/subs` does, then run Caramel Analysis on it
// instead of encoding it, and report the CRF it gives every scene.
//
// It exists so the analysis can be tried on real releases from Discord or the API without an
// encode: the result is the scene list with each scene's CRF and what drove it, and — when asked —
// a small verification that encodes the scenes moved furthest from the anchor at their own CRF and
// at fixed CRFs and measures both against the source.

use crate::lib::mpeg::caramel::{
    CaramelReport, SceneEntry, SceneRequest, VerifyEncoder, VerifyRequest, VerifySample, probe_source, run_scene_analysis,
    run_verification,
};
use crate::lib::mpeg::preset::resolve as resolve_preset;
use crate::pnworker::core::{CommData, Stage};
use crate::pnworker::messages::{
    CARAMEL_DONE, CARAMEL_FAIL, CARAMEL_PROG, CARAMEL_VERIFY_PROG, JOB_CANCELLED, MessagePayload,
};
use crate::pnworker::util::job_cancelled;
use caramel_kernel::SceneConfig;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::Sender;

// Finished reports, one `<job_id>.json` each: what the API serves and Discord attaches.
pub const CARAMEL_REPORT_ROOT: &str = "DB/cache/caramel";
const REPORT_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
// A verification measures a handful of scenes, each at its own CRF and at every fixed CRF listed.
const VERIFY_CLIP_SECONDS: f64 = 4.0;
const VERIFY_MAX_SCENES: usize = 6;
const VERIFY_CRFS_CEILING: usize = 6;
const VERIFY_SCENES_CEILING: usize = 48;
const VERIFY_SECONDS_CEILING: f64 = 10.0;
// How often the message shows how far the scene pass has got.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);
// The embed lists this many verified scenes; the attached report has them all.
const VERIFY_LINES: usize = 6;

#[derive(Clone, Debug, Default)]
pub struct VerifySettings {
    // Fixed CRFs to compare the scene CRFs with; empty for the anchor alone.
    pub crfs: Vec<f32>,
    pub clip_seconds: Option<f64>,
    pub max_scenes: Option<usize>,
    pub sample: VerifySample,
}

// Overrides of how scenes are found and measured, for experiments. Anything left out is the
// default every encode uses.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct SceneOverrides {
    #[serde(default)]
    pub analysis_frames: Option<usize>,
    #[serde(default)]
    pub min_scene_frames: Option<usize>,
    #[serde(default)]
    pub max_scene_frames: Option<usize>,
}

impl SceneOverrides {
    pub fn config(&self) -> SceneConfig {
        let defaults = SceneConfig::default();
        SceneConfig {
            analysis_frames: self.analysis_frames.unwrap_or(defaults.analysis_frames),
            min_scene_frames: self.min_scene_frames.unwrap_or(defaults.min_scene_frames),
            max_scene_frames: self.max_scene_frames.unwrap_or(defaults.max_scene_frames),
        }
        .sanitised()
    }
}

#[derive(Clone, Debug, Default)]
pub struct CaramelJobRequest {
    // Canonical preset name the scenes are anchored at; None for `standard`.
    pub preset: Option<String>,
    // Overrides the preset's own CRF as the anchor.
    pub anchor: Option<f32>,
    pub verify: Option<VerifySettings>,
    pub scenes: SceneOverrides,
}

pub type CaramelData = (PathBuf, u64, CaramelJobRequest);

pub fn report_path(job_id: u64) -> PathBuf {
    PathBuf::from(CARAMEL_REPORT_ROOT).join(format!("{job_id}.json"))
}

pub async fn read_report(job_id: u64) -> Option<serde_json::Value> {
    let bytes = tokio::fs::read(report_path(job_id)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub async fn cleanup_expired_caramel_reports() {
    let Ok(mut entries) = tokio::fs::read_dir(CARAMEL_REPORT_ROOT).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let expired = entry
            .metadata()
            .await
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > REPORT_TTL);
        if expired {
            tokio::fs::remove_file(entry.path()).await.ok();
        }
    }
}

// The fixed CRFs a verification compares with: the ones asked for, or the anchor. Bounded, because
// each one is a real encode of every chosen scene.
fn verify_crfs(asked: &[f32], anchor: f32) -> Vec<f32> {
    let mut crfs: Vec<f32> = if asked.is_empty() { vec![anchor] } else { asked.to_vec() };
    crfs.retain(|crf| crf.is_finite() && (0.0..=51.0).contains(crf));
    crfs.sort_by(|a, b| a.total_cmp(b));
    crfs.dedup();
    crfs.truncate(VERIFY_CRFS_CEILING);
    crfs
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 * 100.0 / whole as f64 }
}

// Where the frames went relative to the anchor: numbers and arrows only, the same in every locale.
fn anchor_split(report: &CaramelReport) -> String {
    let summary = &report.summary;
    format!(
        "↓{:.0}% · ={:.0}% · ↑{:.0}%",
        percent(summary.frames_below_anchor, summary.frames),
        percent(summary.frames_at_anchor, summary.frames),
        percent(summary.frames_above_anchor, summary.frames),
    )
}

// The CRF distribution, by share of frames.
fn histogram(report: &CaramelReport) -> String {
    let summary = &report.summary;
    summary
        .histogram
        .iter()
        .map(|bin| format!("`{}` {:.0}%", bin.crf, percent(bin.frames, summary.frames)))
        .collect::<Vec<_>>()
        .join(" · ")
}

// Averages a named figure of every scene over the source's frames, keeping the order of first
// appearance.
fn frame_weighted<'a>(
    report: &'a CaramelReport,
    figures: impl Fn(&'a SceneEntry) -> Vec<(&'static str, f64)>,
) -> Vec<(&'static str, f64)> {
    let mut totals: Vec<(&str, f64)> = Vec::new();
    for scene in &report.scenes {
        for (name, value) in figures(scene) {
            let weighted = value * scene.frames as f64;
            match totals.iter_mut().find(|(known, _)| *known == name) {
                Some(total) => total.1 += weighted,
                None => totals.push((name, weighted)),
            }
        }
    }
    let frames = report.summary.frames.max(1) as f64;
    totals.into_iter().map(|(name, total)| (name, total / frames)).collect()
}

// What moved the scenes, across the whole source: how far each quality metric would have let the
// CRF go on average, then what was applied — the part all of them agreed on and the factors that
// do not depend on the scene — the largest first.
fn drivers(report: &CaramelReport) -> String {
    let listed = |figures: &[(&str, f64)]| {
        figures.iter().map(|(name, delta)| format!("`{name}` {delta:+.2}")).collect::<Vec<_>>().join(", ")
    };
    let votes = frame_weighted(report, |scene| scene.votes.iter().map(|vote| (vote.metric, vote.offset)).collect());
    let mut factors: Vec<(&str, f64)> =
        frame_weighted(report, |scene| scene.contributions.iter().map(|factor| (factor.factor, factor.delta)).collect())
            .into_iter()
            .filter(|(_, delta)| delta.abs() >= 0.05)
            .collect();
    factors.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    let applied = if factors.is_empty() { "—".to_string() } else { listed(&factors) };
    if votes.is_empty() { applied } else { format!("{} → {}", listed(&votes), applied) }
}

fn timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

// The embed's verification lines, numbers and units only so they read the same in every locale:
// each measured scene at its own CRF against the fixed ones, then every clip pooled per strategy.
fn verify_lines(report: &CaramelReport) -> String {
    let Some(verify) = &report.verify else {
        return String::new();
    };
    let fps = report.source.fps.max(1.0);
    let mut lines: Vec<String> = verify
        .scenes
        .iter()
        .take(VERIFY_LINES)
        .map(|scene| {
            let fixed = scene
                .fixed
                .iter()
                .map(|measure| format!("CRF {} {:.4} · {:.0} kbps", measure.crf, measure.mean_ssim, measure.kbps))
                .collect::<Vec<_>>()
                .join(" | ");
            format!(
                "`{}` **CRF {}** SSIM {:.4} · {:.0} kbps | {}",
                timestamp(scene.start_frame as f64 / fps),
                scene.scene_crf,
                scene.adaptive.mean_ssim,
                scene.adaptive.kbps,
                fixed,
            )
        })
        .collect();
    let adaptive = &verify.adaptive;
    let fixed = verify
        .fixed
        .iter()
        .map(|total| {
            format!(
                "CRF {} SSIM {:.4} (↓{:.4}) · {:.0} kbps · Δ {:+.1}%",
                total.crf.unwrap_or_default(),
                total.mean_ssim,
                total.min_ssim,
                total.kbps,
                -total.adaptive_saving_percent,
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    lines.push(format!(
        "Σ SSIM {:.4} (↓{:.4}) · {:.0} kbps | {}",
        adaptive.mean_ssim, adaptive.min_ssim, adaptive.kbps, fixed
    ));
    lines.join("\n")
}

pub async fn run_caramel_job(
    directory: PathBuf,
    job_id: u64,
    request: CaramelJobRequest,
    tx: &Sender<CommData>,
) {
    let cancelled = || async {
        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled)))
            .await
            .ok();
    };
    let fail = |reason: String| async move {
        tx.send((job_id, MessagePayload::Progress(CARAMEL_FAIL, vec![reason]), Some(Stage::Failed)))
            .await
            .ok();
    };
    if job_cancelled(&directory) {
        return cancelled().await;
    }
    let preset_name = request.preset.clone().unwrap_or_else(|| "standard".to_string());
    let Some(preset) = resolve_preset(&preset_name) else {
        return fail(format!("unknown preset `{preset_name}`")).await;
    };
    let settings = preset.x264_settings();
    let anchor = request.anchor.or(settings.crf.map(f32::from)).unwrap_or(17.0);
    let input = directory.join("contents").join("torrent").join("input.mkv");
    let progress = |done: u64, total: u64| {
        let shown = if total == 0 { 0 } else { (done * 100 / total).min(99) };
        (job_id, MessagePayload::Progress(CARAMEL_PROG, vec![shown.to_string()]), None)
    };
    tx.send(progress(0, 0)).await.ok();

    let encode = caramel_kernel::EncodeContext {
        output_height: preset.scale_height(),
        ..caramel_kernel::EncodeContext::anchored(anchor)
    };
    let scenes = request.scenes.config();
    // Only for the percentage: the analysis probes the source again for itself.
    let total_frames = probe_source(&input)
        .ok()
        .and_then(|probe| probe.duration.map(|duration| (duration * probe.context.fps) as u64))
        .unwrap_or(0);
    let decoded = Arc::new(AtomicU64::new(0));
    let analysis = {
        let (input, decoded, cancel) = (input.clone(), decoded.clone(), directory.join("CANCEL"));
        tokio::task::spawn_blocking(move || {
            run_scene_analysis(&SceneRequest {
                input: &input,
                range: None,
                fps: None,
                encode,
                scenes,
                workers: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8),
                decoder_threads: 0,
                cancel_file: Some(cancel),
                progress: Some(&decoded),
            })
        })
    };
    tokio::pin!(analysis);
    let analysed = loop {
        tokio::select! {
            result = &mut analysis => break result,
            _ = tokio::time::sleep(PROGRESS_INTERVAL) => {
                tx.send(progress(decoded.load(Ordering::Relaxed), total_frames)).await.ok();
            }
        }
    };
    if job_cancelled(&directory) {
        return cancelled().await;
    }
    let mut report = match analysed {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => return fail(error).await,
        Err(error) => return fail(format!("the analysis stopped: {error}")).await,
    };

    if let Some(verify) = &request.verify {
        let crfs = verify_crfs(&verify.crfs, anchor);
        let listed = crfs.iter().map(|crf| crf.to_string()).collect::<Vec<_>>().join(", ");
        tx.send((job_id, MessagePayload::Progress(CARAMEL_VERIFY_PROG, vec![listed]), None))
            .await
            .ok();
        let verify_request = VerifyRequest {
            compare_crfs: crfs,
            clip_seconds: verify
                .clip_seconds
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .unwrap_or(VERIFY_CLIP_SECONDS)
                .min(VERIFY_SECONDS_CEILING),
            max_scenes: verify.max_scenes.unwrap_or(VERIFY_MAX_SCENES).clamp(1, VERIFY_SCENES_CEILING),
            sample: verify.sample,
            // Non-x264 presets are still measured with libx264 at x264's defaults: the anchor CRF
            // means something different there, and the report says which encoder it used.
            encoder: if preset.encodes_with_x264() {
                VerifyEncoder {
                    preset: settings.preset.clone(),
                    tune: settings.tune.clone(),
                    x264_params: settings.x264_params.clone(),
                }
            } else {
                VerifyEncoder::default()
            },
            scratch: directory.join("work").join("caramel-verify"),
            cancel_file: Some(directory.join("CANCEL")),
        };
        let snapshot = report.clone();
        let verified = tokio::task::spawn_blocking(move || run_verification(&snapshot, &verify_request)).await;
        match verified {
            Ok(Ok(verify)) => report.verify = Some(verify),
            Ok(Err(error)) => report.verify_error = Some(error),
            Err(error) => report.verify_error = Some(format!("the verification stopped: {error}")),
        }
        if job_cancelled(&directory) {
            return cancelled().await;
        }
    }

    let path = report_path(job_id);
    let stored = async {
        tokio::fs::create_dir_all(CARAMEL_REPORT_ROOT).await?;
        let json = serde_json::to_vec_pretty(&report).map_err(std::io::Error::other)?;
        tokio::fs::write(&path, json).await
    }
    .await;
    if let Err(error) = stored {
        return fail(format!("could not store the report: {error}")).await;
    }
    let mut verify_text = verify_lines(&report);
    if let Some(error) = &report.verify_error {
        verify_text = format!("⚠ {error}");
    }
    let summary = &report.summary;
    let args = vec![
        summary.scenes.to_string(),
        preset_name,
        summary.min_crf.to_string(),
        summary.max_crf.to_string(),
        format!("{:.2}", summary.mean_crf),
        summary.anchor_crf.to_string(),
        anchor_split(&report),
        summary.frames.to_string(),
        format!("{:.1}", report.elapsed_ms as f64 / 1000.0),
        histogram(&report),
        drivers(&report),
        verify_text,
        // Never printed: the frontend attaches it and the progress row embeds it.
        path_text(&path),
    ];
    tx.send((job_id, MessagePayload::Progress(CARAMEL_DONE, args), Some(Stage::Uploaded)))
        .await
        .ok();
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_compares_with_the_anchor_by_default() {
        assert_eq!(verify_crfs(&[], 17.0), vec![17.0]);
    }

    #[test]
    fn verification_crfs_are_bounded_and_ordered() {
        let asked = [30.0, 14.0, f32::NAN, 99.0, 16.0, 18.0, 20.0, 22.0, 26.0, 14.0];
        assert_eq!(verify_crfs(&asked, 0.0), vec![14.0, 16.0, 18.0, 20.0, 22.0, 26.0]);
    }

    #[test]
    fn scene_overrides_fall_back_to_the_defaults() {
        assert_eq!(SceneOverrides::default().config(), SceneConfig::default());
        let tight = SceneOverrides { analysis_frames: Some(0), max_scene_frames: Some(5), ..Default::default() }.config();
        assert_eq!(tight.analysis_frames, 1);
        assert_eq!(tight.max_scene_frames, SceneConfig::default().min_scene_frames);
    }
}
