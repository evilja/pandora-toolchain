// The `Caramel` job: download a source exactly as `/subs` does, then run Caramel Analysis on it
// instead of encoding it, and report the CRF it would have chosen.
//
// It exists so the analysis can be tried on real releases from Discord or the API without an
// encode: the result is the estimate, what drove it, and — when asked — a small verification that
// encodes sample clips at the estimated and anchor CRFs and measures them against the source.

use crate::lib::mpeg::caramel::{
    CaramelReport, CaramelRequest, Sampling, VerifyEncoder, VerifyRequest, run_caramel_analysis,
    run_verification,
};
use crate::lib::mpeg::preset::resolve as resolve_preset;
use crate::pnworker::core::{CommData, Stage};
use crate::pnworker::messages::{
    CARAMEL_DONE, CARAMEL_FAIL, CARAMEL_PROG, CARAMEL_VERIFY_PROG, JOB_CANCELLED, MessagePayload,
};
use crate::pnworker::util::job_cancelled;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::Sender;

// Finished reports, one `<job_id>.json` each: what the API serves and Discord attaches.
pub const CARAMEL_REPORT_ROOT: &str = "DB/cache/caramel";
const REPORT_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
// A verification that names no CRFs measures the estimate against the anchor it moved from.
const VERIFY_CLIP_SECONDS: f64 = 2.0;
const VERIFY_MAX_CLIPS: usize = 6;
const VERIFY_CRFS_CEILING: usize = 6;
const VERIFY_CLIPS_CEILING: usize = 16;
const VERIFY_SECONDS_CEILING: f64 = 10.0;

#[derive(Clone, Debug, Default)]
pub struct VerifySettings {
    // Empty for the estimate and the anchor.
    pub crfs: Vec<f32>,
    pub clip_seconds: Option<f64>,
    pub max_clips: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct CaramelJobRequest {
    // Canonical preset name the estimate is anchored at; None for `standard`.
    pub preset: Option<String>,
    // Overrides the preset's own CRF as the anchor.
    pub anchor: Option<f32>,
    pub verify: Option<VerifySettings>,
    pub sampling: Sampling,
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

// The CRFs a verification measures: the ones asked for, or the estimate and its anchor. Bounded,
// because each one is a real encode of every clip.
fn verify_crfs(asked: &[f32], estimate: f32, anchor: f32) -> Vec<f32> {
    let mut crfs: Vec<f32> = if asked.is_empty() { vec![estimate, anchor] } else { asked.to_vec() };
    crfs.retain(|crf| crf.is_finite() && (0.0..=51.0).contains(crf));
    crfs.sort_by(|a, b| a.total_cmp(b));
    crfs.dedup();
    crfs.truncate(VERIFY_CRFS_CEILING);
    crfs
}

// The embed's verification lines: numbers and units only, so they read the same in every locale.
fn verify_lines(report: &CaramelReport) -> String {
    let Some(verify) = &report.verify else {
        return String::new();
    };
    verify
        .points
        .iter()
        .map(|point| {
            format!(
                "`CRF {}` SSIM {:.4} (↓{:.4}) · PSNR {:.2} dB (↓{:.2}) · {:.0} kbps",
                point.crf, point.mean_ssim, point.min_ssim, point.mean_psnr, point.min_psnr, point.kbps
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn drivers(report: &CaramelReport) -> String {
    let estimate = &report.estimate;
    let mut factors: Vec<_> = estimate
        .typical
        .iter()
        .chain(estimate.global.iter())
        .filter(|factor| factor.delta.abs() >= 0.05)
        .collect();
    factors.sort_by(|a, b| b.delta.abs().total_cmp(&a.delta.abs()));
    if factors.is_empty() {
        return "—".to_string();
    }
    factors
        .iter()
        .map(|factor| format!("`{}` {:+.2}", factor.factor, factor.delta))
        .collect::<Vec<_>>()
        .join(", ")
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
    tx.send((job_id, MessagePayload::Static(CARAMEL_PROG), None)).await.ok();

    let encode = caramel_kernel::EncodeContext {
        output_height: preset.scale_height(),
        ..caramel_kernel::EncodeContext::anchored(anchor)
    };
    let sampling = request.sampling;
    let analysis_input = input.clone();
    let analysed = tokio::task::spawn_blocking(move || {
        run_caramel_analysis(&CaramelRequest {
            input: &analysis_input,
            range: None,
            fps: None,
            encode,
            workers: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4),
            decoder_threads: 0,
            sampling,
        })
    })
    .await;
    let mut report = match analysed {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => return fail(error).await,
        Err(error) => return fail(format!("the analysis stopped: {error}")).await,
    };
    if job_cancelled(&directory) {
        return cancelled().await;
    }

    if let Some(verify) = &request.verify {
        let crfs = verify_crfs(&verify.crfs, report.estimate.crf, anchor);
        let listed = crfs.iter().map(|crf| crf.to_string()).collect::<Vec<_>>().join(", ");
        tx.send((job_id, MessagePayload::Progress(CARAMEL_VERIFY_PROG, vec![listed]), None))
            .await
            .ok();
        let verify_request = VerifyRequest {
            crfs,
            clip_seconds: verify
                .clip_seconds
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .unwrap_or(VERIFY_CLIP_SECONDS)
                .min(VERIFY_SECONDS_CEILING),
            max_clips: verify.max_clips.unwrap_or(VERIFY_MAX_CLIPS).clamp(1, VERIFY_CLIPS_CEILING),
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
    let estimate = &report.estimate;
    let args = vec![
        estimate.crf.to_string(),
        preset_name,
        estimate.anchor_crf.to_string(),
        format!("{:+.2}", estimate.adjustment),
        format!("{:.2}", estimate.confidence),
        estimate.windows_analyzed.to_string(),
        estimate.frames_analyzed.to_string(),
        format!("{:.1}", report.elapsed_ms as f64 / 1000.0),
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
    fn verification_defaults_to_the_estimate_and_its_anchor() {
        assert_eq!(verify_crfs(&[], 18.5, 17.0), vec![17.0, 18.5]);
        assert_eq!(verify_crfs(&[], 17.0, 17.0), vec![17.0]);
    }

    #[test]
    fn verification_crfs_are_bounded_and_ordered() {
        let asked = [30.0, 14.0, f32::NAN, 99.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0];
        let crfs = verify_crfs(&asked, 0.0, 0.0);
        assert_eq!(crfs, vec![14.0, 16.0, 18.0, 20.0, 22.0, 24.0]);
    }
}
