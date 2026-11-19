// The `Caramel` job: download a source exactly as `/subs` does, then run Caramel Analysis on it
// instead of encoding it, and report the CRF it gives every scene.
//
// It exists so the analysis can be tried on real releases from Discord or the API without an
// encode: the result is the scene list with each scene's CRF and what drove it, and — when asked —
// a small verification that encodes the scenes moved furthest from the anchor at their own CRF and
// at fixed CRFs and measures both against the source. `/caramel-analysis-encode` also encodes the
// whole video at the scene CRFs it decided and publishes it to Drive and as Lumiere HLS, so the
// decisions can be watched.

use crate::lib::mpeg::caramel::{
    CaramelReport, HostedEncode, LiveScene, LiveScenes, LiveSnapshot, SceneEncodeRequest, SceneEntry, SceneRequest, VerifyEncoder, VerifyRequest,
    VerifySample, encode_scene_crfs, probe_source, run_scene_analysis, run_verification,
};
use crate::lib::mpeg::hls::DEFAULT_NAME_TEMPLATE;
use crate::lumiere_broker::{DriveCandidate, DriveUploadSpec, GLOBAL_DRIVE_PROFILE, LumiereClient, publish_hls};
use crate::lib::mpeg::preset::resolve as resolve_preset;
use crate::pnworker::core::{CommData, Stage};
use crate::pnworker::messages::{
    CARAMEL_DONE, CARAMEL_ENCODE_PROG, CARAMEL_FAIL, CARAMEL_PROG, CARAMEL_UPLOAD_PROG, CARAMEL_VERIFY_PROG, JOB_CANCELLED, MessagePayload,
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
    // `/caramel-analysis-encode`: every scene decided after its fast encode, with a model that reads
    // one (`caramel_train::fast_model`) rather than the one encodes use.
    pub fast: bool,
    // Also encode the whole video at the scene CRFs and publish it to Drive and as HLS.
    pub host: bool,
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
    // Only for the progress lines: the analysis probes the source again for itself.
    let source = probe_source(&input).ok();
    let source_fps = source.as_ref().map_or(0.0, |probe| probe.context.fps);
    let total_frames = source
        .as_ref()
        .and_then(|probe| probe.duration.map(|duration| (duration * probe.context.fps) as u64))
        .unwrap_or(0);
    let live = Arc::new(LiveScenes::default());
    let started = std::time::Instant::now();
    let progress = |done: u64| {
        let snapshot = live.snapshot();
        let elapsed = started.elapsed().as_secs_f64();
        (job_id, MessagePayload::Progress(CARAMEL_PROG, analysis_args(done, total_frames, elapsed, source_fps, &snapshot)), None)
    };
    tx.send(progress(0)).await.ok();

    let encode = caramel_kernel::EncodeContext {
        output_height: preset.scale_height(),
        ..caramel_kernel::EncodeContext::anchored(anchor)
    };
    let scenes = request.scenes.config();
    let encoder = crate::lib::mpeg::caramel::probe_encoder(&preset);
    let decoded = Arc::new(AtomicU64::new(0));
    let analysis = {
        let (input, decoded, cancel, fast, live) = (input.clone(), decoded.clone(), directory.join("CANCEL"), request.fast, live.clone());
        tokio::task::spawn_blocking(move || {
            // The report is of what an encode started now would decide: with the model installed
            // on this machine, which a research worker may have replaced since `pndc` started.
            crate::lib::mpeg::caramel_train::activate();
            let model = if fast {
                Some(crate::lib::mpeg::caramel_train::fast_model().ok_or_else(|| "no model on this machine decides scenes after a fast encode".to_string())?)
            } else {
                None
            };
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
                fast: encoder,
                probe: None,
                model,
                live: Some(live.as_ref()),
                scene_report: None,
            })
        })
    };
    tokio::pin!(analysis);
    let analysed = loop {
        tokio::select! {
            result = &mut analysis => break result,
            _ = tokio::time::sleep(PROGRESS_INTERVAL) => {
                tx.send(progress(decoded.load(Ordering::Relaxed))).await.ok();
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

    if request.host {
        match host_encode(&directory, job_id, &report, &settings, tx).await {
            Ok(hosted) => report.hosted = Some(hosted),
            Err(error) => report.hosted_error = Some(error),
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
        hosted_line(&report),
        // Never printed: the frontend attaches it and the progress row embeds it.
        path_text(&path),
    ];
    tx.send((job_id, MessagePayload::Progress(CARAMEL_DONE, args), Some(Stage::Uploaded)))
        .await
        .ok();
}

fn rate(count: f64, seconds: f64) -> f64 {
    if seconds > 0.0 { count / seconds } else { 0.0 }
}

// One decision as a line of the analysis update, numbers only so it reads the same in every locale:
// where the scene starts, its CRF, the CRF the model chose when a test encode pulled it back, and
// what its fast encode read.
fn live_line(scene: &LiveScene, fps: f64) -> String {
    let mut line = format!("`{}` CRF **{}**", timestamp(scene.start_frame as f64 / fps.max(1.0)), scene.crf);
    if let Some(proposed) = scene.proposed {
        line.push_str(&format!(" (← {proposed})"));
    }
    if let Some(xpsnr) = scene.fast_xpsnr {
        line.push_str(&format!(" · fast {xpsnr:.1} dB"));
    }
    line
}

// The analysis update: percent, frames decoded of the source's, decode speed, scenes decided, their
// frame-weighted CRF so far and the latest decisions — then, never printed, those decisions as JSON
// for the API's progress row.
fn analysis_args(done: u64, total: u64, elapsed: f64, fps: f64, snapshot: &LiveSnapshot) -> Vec<String> {
    let percent = if total == 0 { 0 } else { (done * 100 / total).min(99) };
    let recent = if snapshot.recent.is_empty() {
        "—".to_string()
    } else {
        snapshot.recent.iter().map(|scene| live_line(scene, fps)).collect::<Vec<_>>().join("\n")
    };
    vec![
        percent.to_string(),
        done.to_string(),
        if total == 0 { "?".to_string() } else { total.to_string() },
        format!("{:.1}", rate(done as f64, elapsed)),
        snapshot.scenes.to_string(),
        snapshot.mean_crf.map_or_else(|| "—".to_string(), |mean| format!("{mean:.2}")),
        recent,
        serde_json::to_string(snapshot).unwrap_or_default(),
    ]
}

// The encode update: percent, frames written of the report's, speed, the bitrate the file has so
// far (audio included, as an encode's update counts it) and the CRF of the scene being encoded.
fn encode_args(report: &CaramelReport, frames: u64, bytes: u64, elapsed: f64) -> Vec<String> {
    let percent = if report.frames == 0 { 0 } else { (frames * 100 / report.frames).min(99) };
    let seconds = frames as f64 / report.source.fps.max(1.0);
    let scene = report.scenes.partition_point(|scene| scene.start_frame <= frames).saturating_sub(1);
    vec![
        percent.to_string(),
        frames.to_string(),
        report.frames.to_string(),
        format!("{:.1}", rate(frames as f64, elapsed)),
        format!("{:.0}", rate(bytes as f64 * 8.0 / 1000.0, seconds)),
        report.scenes.get(scene).map_or_else(|| report.summary.anchor_crf.to_string(), |scene| scene.crf.to_string()),
    ]
}

// The encode at the scene CRFs, with its size and bitrate and where it can be watched, as one line
// of the finished embed: empty when none was asked for.
fn hosted_line(report: &CaramelReport) -> String {
    match (&report.hosted, &report.hosted_error) {
        (Some(hosted), _) => {
            let mut parts = vec![format!("🎞 {:.1} MB · {:.0} kbps", hosted.bytes as f64 / 1_000_000.0, hosted.kbps)];
            parts.extend(hosted.drive.as_ref().map(|url| format!("[Drive]({url})")));
            parts.extend(hosted.hls.as_ref().map(|url| format!("[HLS]({url})")));
            parts.extend(hosted.errors.iter().map(|error| format!("⚠ {error}")));
            parts.join(" · ")
        }
        (None, Some(error)) => format!("⚠ {error}"),
        (None, None) => String::new(),
    }
}

// Encodes the job's video at the report's scene CRFs with the preset's x264 settings, then
// publishes it to the global Drive and as Lumiere HLS, saying how far each has got on the job
// message. Either publication may fail alone; the encode failing, or both, fails the whole.
async fn host_encode(
    directory: &Path,
    job_id: u64,
    report: &CaramelReport,
    settings: &crate::lib::mpeg::preset::X264Settings,
    tx: &Sender<CommData>,
) -> Result<HostedEncode, String> {
    let output = directory.join("work").join(format!("caramel-{job_id}.mp4"));
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let cancel = directory.join("CANCEL");
    let written = Arc::new(AtomicU64::new(0));
    let started = std::time::Instant::now();
    let encode = {
        let (report, output, written, cancel) = (report.clone(), output.clone(), written.clone(), cancel.clone());
        let settings = settings.clone();
        let input = PathBuf::from(&report.input);
        tokio::task::spawn_blocking(move || {
            encode_scene_crfs(
                &report,
                &SceneEncodeRequest {
                    input: &input,
                    output: &output,
                    preset: settings.preset,
                    tune: settings.tune,
                    profile: settings.profile,
                    level: settings.level,
                    x264_params: settings.x264_params,
                    cancel_file: Some(cancel),
                    progress: Some(&written),
                },
            )
        })
    };
    tokio::pin!(encode);
    let encode_progress = |frames: u64, bytes: u64| {
        let args = encode_args(report, frames, bytes, started.elapsed().as_secs_f64());
        (job_id, MessagePayload::Progress(CARAMEL_ENCODE_PROG, args), None)
    };
    tx.send(encode_progress(0, 0)).await.ok();
    let encoded = loop {
        tokio::select! {
            result = &mut encode => break result,
            _ = tokio::time::sleep(PROGRESS_INTERVAL) => {
                let bytes = tokio::fs::metadata(&output).await.map_or(0, |meta| meta.len());
                tx.send(encode_progress(written.load(Ordering::Relaxed), bytes)).await.ok();
            }
        }
    };
    match encoded {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("the encode failed: {error}")),
        Err(error) => return Err(format!("the encode stopped: {error}")),
    }
    let encode_seconds = started.elapsed().as_secs_f64();
    let bytes = tokio::fs::metadata(&output).await.map_err(|e| format!("{}: {e}", output.display()))?.len();
    let seconds = report.frames as f64 / report.source.fps.max(1.0);
    let kbps = video_kbps(&output, seconds).await.unwrap_or(if seconds > 0.0 { bytes as f64 * 8.0 / seconds / 1000.0 } else { 0.0 });

    tx.send((job_id, MessagePayload::Static(CARAMEL_UPLOAD_PROG), None)).await.ok();
    let client = LumiereClient::from_env().map_err(|error| format!("Lumiere is not configured: {error}"))?;
    let mut errors = Vec::new();
    // HLS first: it remuxes the file, and Drive's upload is the one that may take long.
    let hls = match publish_hls(&output, None, client.config(), Some(cancel.as_path()), DEFAULT_NAME_TEMPLATE).await {
        Ok(publication) => Some(publication.url().to_string()),
        Err(error) => {
            errors.push(format!("HLS: {error}"));
            None
        }
    };
    let drive = match client
        .upload_drive(
            DriveUploadSpec {
                path: output.clone(),
                request_id: format!("pandora:caramel:{job_id}"),
                candidates: vec![DriveCandidate {
                    profile: GLOBAL_DRIVE_PROFILE.to_string(),
                    root: "default".to_string(),
                    folder_path: String::new(),
                    filename: format!("caramel-{job_id} [{}].mp4", report.model),
                }],
                content_type: "video/mp4".to_string(),
                cancel_file: Some(cancel.clone()),
            },
            None,
        )
        .await
    {
        Ok(uploaded) => Some(uploaded.url),
        Err(error) => {
            errors.push(format!("Drive: {error}"));
            None
        }
    };
    let _ = tokio::fs::remove_file(&output).await;
    if drive.is_none() && hls.is_none() {
        return Err(errors.join("; "));
    }
    Ok(HostedEncode { drive, hls, errors, bytes, kbps, encode_seconds })
}

// The bitrate of `path`'s video stream alone, in kbps, so the line reads like the verification's
// figures rather than including the audio.
async fn video_kbps(path: &Path, seconds: f64) -> Option<f64> {
    let output = tokio::process::Command::new(crate::lib::bin::resolve_runtime_binary("ffprobe"))
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=size", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .await
        .ok()?;
    let bytes: u64 = String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| line.trim().trim_end_matches(',').parse::<u64>().ok()).sum();
    (bytes > 0 && seconds > 0.0).then(|| bytes as f64 * 8.0 / seconds / 1000.0)
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

    // Before any scene is decided the update still has all its slots; a decision a probe pulled back
    // shows both CRFs.
    #[test]
    fn analysis_updates_show_the_decisions_so_far() {
        let empty = analysis_args(0, 0, 0.0, 24.0, &LiveSnapshot::default());
        assert_eq!(empty[..7], ["0", "0", "?", "0.0", "0", "—", "—"]);
        let snapshot = LiveSnapshot {
            scenes: 2,
            frames: 480,
            mean_crf: Some(18.25),
            recent: vec![LiveScene { start_frame: 1440, crf: 19.0, proposed: Some(21.5), fast_xpsnr: Some(41.26) }],
        };
        let args = analysis_args(480, 960, 4.0, 24.0, &snapshot);
        assert_eq!(args[..6], ["50", "480", "960", "120.0", "2", "18.25"]);
        assert_eq!(args[6], "`1:00` CRF **19** (← 21.5) · fast 41.3 dB");
        assert!(args[7].contains("\"mean_crf\":18.25"));
    }

    #[test]
    fn scene_overrides_fall_back_to_the_defaults() {
        assert_eq!(SceneOverrides::default().config(), SceneConfig::default());
        let tight = SceneOverrides { analysis_frames: Some(0), max_scene_frames: Some(5), ..Default::default() }.config();
        assert_eq!(tight.analysis_frames, 1);
        assert_eq!(tight.max_scene_frames, SceneConfig::default().min_scene_frames);
    }
}
