// Caramel's models on disk, and training one from the lab's dataset.
//
// The kernel carries a model that was trained before the build. A machine that runs the lab keeps
// measuring, and what it measures is turned into a model of its own here: the whole dataset is
// fitted again, the result is judged against the model in use on scenes that one has never seen,
// and it is installed only if it is no worse. Installing is two files. Every installed model is
// kept by version under `DB/caramel/models/`, and `DB/caramel/active` names the one new encodes
// decide with — so an encode whose chunks were started under the previous model finishes with it,
// and going back is one line.

use super::caramel_lab::{LabGuide, LabSample};
use caramel_kernel::curve::{self, DECIDES, METRIC_NAMES, METRICS, Model};
use caramel_kernel::train::{self, Policy, Sample, TrainConfig, Trained, Verdict};
use caramel_kernel::{EncodeContext, WindowFeatures, estimate_scene_with, inputs};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const CARAMEL_DIRECTORY: &str = "DB/caramel";
pub const DATASET_DIRECTORY: &str = "DB/caramel/dataset";
pub const ACTIVE_FILE: &str = "DB/caramel/active";
// Installed models kept beside the active one, newest first: enough for any encode in flight and
// for stepping back.
const KEPT_MODELS: usize = 8;
// A first model has nothing to be compared with, and is taken only from a dataset that could
// have taught one: this many scenes from this many series.
pub const FIRST_MODEL_SCENES: usize = 200;
pub const FIRST_MODEL_SERIES: usize = 5;
// The family of the models trained here. The number changes with what a model is — its inputs, its
// metrics, the shape of its curves — and the rest of a version says which training it came from.
pub const MODEL_FAMILY: &str = "caramel-neural-1";

// The version the pointer file names, when it names a model that is there.
fn pointed_version() -> Option<String> {
    let version = std::fs::read_to_string(ACTIVE_FILE).ok()?.trim().to_string();
    (pnx264::scenes::valid_version(&version) && pnx264::scenes::model_file(&version).is_file()).then_some(version)
}

// Makes the installed model the one this process decides scenes with. A tool calls this at its
// start and a long-running process before each analysis, so both decide as every other process on
// the machine does. Without an installed model the built-in one is used; with one that does not
// read, whatever the process had stays. Returns the version now active.
pub fn activate() -> String {
    match pointed_version() {
        Some(version) => {
            if curve::active().is_none_or(|model| model.version != version) {
                if let Some(model) = pnx264::scenes::load_model(&version) {
                    curve::install(model);
                }
            }
        }
        None => curve::uninstall(),
    }
    caramel_kernel::model_version()
}

// The version new encodes on this machine decide with, for a process that only reports it.
pub fn installed_version() -> String {
    pointed_version().unwrap_or_else(caramel_kernel::model_version)
}

fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let directory = path.parent().ok_or("no directory")?;
    std::fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    let scratch = directory.join(format!(".{}.{}", path.file_name().and_then(|name| name.to_str()).unwrap_or("file"), std::process::id()));
    std::fs::write(&scratch, text).map_err(|error| format!("{}: {error}", scratch.display()))?;
    std::fs::rename(&scratch, path).map_err(|error| format!("{}: {error}", path.display()))
}

// Installs a model: its file first, then the pointer, each written whole or not at all. Older
// models beyond the few kept are removed.
pub fn install(model: &Model) -> Result<PathBuf, String> {
    if !pnx264::scenes::valid_version(&model.version) {
        return Err(format!("`{}` is not a version a model can be installed under", model.version));
    }
    let file = pnx264::scenes::model_file(&model.version);
    write_atomically(&file, &model.to_text())?;
    write_atomically(Path::new(ACTIVE_FILE), &format!("{}\n", model.version))?;
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(pnx264::scenes::MODEL_DIRECTORY)
        .map(|entries| entries.filter_map(|entry| entry.ok()).map(|entry| entry.path()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("model") && *path != file)
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    others.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in others.into_iter().skip(KEPT_MODELS) {
        let _ = std::fs::remove_file(path);
    }
    Ok(file)
}

// The lab's samples as the kernel trains on them.
pub fn training_samples(samples: &[LabSample]) -> Vec<Sample> {
    samples
        .iter()
        .map(|sample| Sample {
            series: sample.series.clone(),
            inputs: sample.inputs(),
            observations: sample.observations(),
            frames: sample.frames as f64,
            collected: sample.collected_unix,
        })
        .collect()
}

// A model as the lab is guided by one.
pub struct ModelGuide(pub Arc<Model>);

impl LabGuide for ModelGuide {
    fn version(&self) -> String {
        self.0.version.clone()
    }

    fn predict(&self, features: &WindowFeatures, deep: bool, crf: f64) -> [f64; METRICS] {
        self.0.predict(&inputs::inputs(features, deep), crf)
    }

    fn band(&self, metric: usize) -> Option<(f64, f64)> {
        let (first, last) = (train::ANCHORS[0], train::ANCHORS[train::ANCHORS.len() - 1]);
        DECIDES[metric].then(|| (curve::targets(&self.0.typical, last)[metric], curve::targets(&self.0.typical, first)[metric]))
    }

    fn uncertainty(&self, features: &WindowFeatures, deep: bool) -> f64 {
        self.0.uncertainty(&inputs::inputs(features, deep))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PolicyReport {
    pub decisions: usize,
    pub raised_share: f64,
    pub lowered_share: f64,
    // Of all scenes, the share raised by four CRF or more.
    pub raised_far_share: f64,
    pub raise_mean: f64,
    pub bytes: f64,
    pub shortfall_p90: f64,
    pub shortfall_p95: f64,
    pub banding_p95: f64,
    pub needed: f64,
}

impl From<Policy> for PolicyReport {
    fn from(policy: Policy) -> Self {
        let share = |count: usize| if policy.decisions == 0 { 0.0 } else { count as f64 / policy.decisions as f64 };
        Self {
            decisions: policy.decisions,
            raised_share: share(policy.raised),
            lowered_share: share(policy.lowered),
            raised_far_share: share(policy.raised_far),
            raise_mean: policy.raise_mean,
            bytes: policy.bytes,
            shortfall_p90: policy.shortfall_p90,
            shortfall_p95: policy.shortfall_p95,
            banding_p95: policy.banding_p95,
            needed: policy.needed,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MetricError {
    pub metric: &'static str,
    pub decides: bool,
    // Mean distance, in CRF, between the vote and what the scene's encodes allowed, on scenes the
    // networks that voted had not trained on.
    pub held_out: f64,
    pub typical: Vec<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrainReport {
    pub version: String,
    pub encoder: String,
    pub scenes: usize,
    pub series: usize,
    // Samples in the dataset made with other encoder settings, and left out.
    pub other_encoders: usize,
    pub members: usize,
    pub epochs: Vec<usize>,
    // The ease of a detailed scene and the share of a scene's ease over it it is raised by.
    pub detail: f64,
    pub gain: f64,
    pub error: f64,
    pub metrics: Vec<MetricError>,
    // What the candidate would have decided for scenes it had not trained on, and what the fitted
    // predictors decide for the same scenes: both scored against the same detailed scene.
    pub policy: PolicyReport,
    pub predictors: PolicyReport,
    // The model in use, and how the two compare on the scenes measured since it was trained.
    pub current: Option<String>,
    pub fresh_scenes: usize,
    pub current_error: Option<f64>,
    pub candidate_error: Option<f64>,
    pub accepted: bool,
    pub reason: String,
    pub installed: bool,
    pub elapsed_ms: u128,
}

pub struct TrainRequest<'a> {
    pub samples: &'a [LabSample],
    // The encoder settings to train for; None for the ones most samples were made with.
    pub encoder: Option<String>,
    // The model in use, to be compared with and to take the targets from.
    pub current: Option<Arc<Model>>,
    // Take the targets from the samples even when there is a model in use.
    pub refit_targets: bool,
    // The networks' shape and how they are trained. Its version and encoder are set here.
    pub config: TrainConfig,
}

// One scene at one anchor, as networks that never trained on its series judged it: what each
// metric's vote was, how far the networks disagreed, what the scene's encodes allowed, and what the
// candidate would have done. The report's figures are sums over these.
#[derive(Clone, Debug, Serialize)]
pub struct HeldOutScene {
    pub source_id: String,
    pub series: String,
    pub start_frame: u64,
    pub anchor: f64,
    pub votes: Vec<f64>,
    pub spreads: Vec<f64>,
    pub allowed: Vec<Option<f64>>,
    // What those networks' curves read at the anchor, per metric.
    pub levels: Vec<f64>,
    pub offset: f64,
}

pub struct TrainOutcome {
    pub model: Model,
    pub report: TrainReport,
    pub scenes: Vec<HeldOutScene>,
}

fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

// What the fitted predictors decide for every sample at every anchor.
fn predictor_decisions(samples: &[LabSample]) -> Vec<(usize, f64, f64)> {
    let mut out = Vec::new();
    for (index, sample) in samples.iter().enumerate().filter(|(_, sample)| sample.points.len() >= 2) {
        for anchor in train::ANCHORS {
            let estimate = estimate_scene_with(None, &sample.features, &sample.source, &EncodeContext::anchored(anchor as f32));
            out.push((index, anchor, estimate.crf as f64 - anchor));
        }
    }
    out
}

// Whether a trained candidate may be installed, and why.
fn verdict(current: Option<&Model>, trained: &Trained, samples: &[Sample], policy: &Policy) -> (bool, String, Option<Verdict>) {
    if trained.held_out.iter().all(Vec::is_empty) {
        return (false, "every scene is from one series: nothing could be held out to judge on".to_string(), None);
    }
    match current {
        None => {
            let (scenes, series) = (trained.model.scenes, trained.model.sources);
            if scenes < FIRST_MODEL_SCENES || series < FIRST_MODEL_SERIES {
                return (false, format!("{scenes} scenes from {series} series; a first model needs {FIRST_MODEL_SCENES} from {FIRST_MODEL_SERIES}"), None);
            }
            if !policy.holds() {
                return (
                    false,
                    format!(
                        "held-out raises go too far even at a gain of {}: {:.2} and {:.2} units below a detailed scene at the 90th and 95th percentiles, {:.2} CAMBI added",
                        trained.model.gain, policy.shortfall_p90, policy.shortfall_p95, policy.banding_p95
                    ),
                    None,
                );
            }
            (true, format!("first model: held-out raises hold a detailed scene's quality at a gain of {}", trained.model.gain), None)
        }
        Some(current) => {
            let verdict = train::judge(current, trained, samples);
            let reason = if verdict.accepted && !policy.holds() {
                format!("better votes than `{}`, but held-out raises go too far: {:.2} units below a detailed scene at the 90th percentile, {:.2} CAMBI added", current.version, policy.shortfall_p90, policy.banding_p95)
            } else if verdict.scenes < train::GATE_SCENES {
                format!("{} scenes measured since `{}` was trained; {} are needed to replace it", verdict.scenes, current.version, train::GATE_SCENES)
            } else if verdict.accepted {
                format!("on {} scenes `{}` has not seen: {:.2} CRF against its {:.2}", verdict.scenes, current.version, verdict.candidate, verdict.current)
            } else {
                format!("no better on {} scenes `{}` has not seen: {:.2} CRF against its {:.2}", verdict.scenes, current.version, verdict.candidate, verdict.current)
            };
            (verdict.accepted && policy.holds(), reason, Some(verdict))
        }
    }
}

// Trains a candidate on the dataset and judges it. Nothing is installed here.
pub fn train_dataset(request: &TrainRequest) -> Result<TrainOutcome, String> {
    let started = std::time::Instant::now();
    let encoder = match request.encoder.clone() {
        Some(encoder) => encoder,
        None => {
            let mut keys: Vec<&str> = request.samples.iter().map(|sample| sample.encoder.as_str()).collect();
            keys.sort_unstable();
            keys.chunk_by(|a, b| a == b).max_by_key(|run| run.len()).map(|run| run[0].to_string()).ok_or("the dataset is empty")?
        }
    };
    let kept: Vec<LabSample> = request.samples.iter().filter(|sample| sample.encoder == encoder).cloned().collect();
    let samples = training_samples(&kept);
    let typical = request.current.as_ref().filter(|_| !request.refit_targets).map(|model| model.typical);
    let config = TrainConfig { version: MODEL_FAMILY.to_string(), encoder: encoder.clone(), ..request.config.clone() };
    let mut trained = train::train(&samples, &config, typical)?;
    // The version says which training this is: the family, how many scenes, and a mark of the
    // weights themselves, so two trainings never share a file.
    trained.model.version = format!("{MODEL_FAMILY}.{}-{:06x}", trained.model.scenes, hash(&trained.model.to_text()) & 0xff_ffff);
    let model = &trained.model;
    let policy = train::simulate(&samples, &model.typical, model.detail, &train::decisions(&samples, &trained.held_out, &model.typical, model.detail, model.gain));
    let predictors = train::simulate(&samples, &model.typical, model.detail, &predictor_decisions(&kept));
    let (accepted, reason, judged) = verdict(request.current.as_deref(), &trained, &samples, &policy);
    let mut scenes = Vec::new();
    for (index, members) in trained.held_out.iter().enumerate().filter(|(index, members)| !members.is_empty() && samples[*index].observations.len() >= 2) {
        for anchor in train::ANCHORS {
            let votes = curve::votes_of(members, &model.typical, anchor);
            let drawn = curve::mean_curves(members);
            scenes.push(HeldOutScene {
                source_id: kept[index].source_id.clone(),
                series: kept[index].series.clone(),
                start_frame: kept[index].start_frame,
                anchor,
                votes: votes.iter().map(|vote| vote.offset).collect(),
                spreads: votes.iter().map(|vote| vote.spread).collect(),
                allowed: curve::measured_votes(&samples[index].observations, &model.typical, anchor).to_vec(),
                levels: drawn.iter().map(|curve| curve.at(anchor)).collect(),
                offset: train::applied(&curve::decide(&votes, model.detail, model.gain)),
            });
        }
    }
    let report = TrainReport {
        version: model.version.clone(),
        encoder,
        scenes: model.scenes,
        series: model.sources,
        other_encoders: request.samples.len() - kept.len(),
        members: model.members.len(),
        epochs: trained.epochs.clone(),
        detail: model.detail,
        gain: model.gain,
        error: train::deciding_error(&model.error),
        metrics: (0..METRICS).map(|metric| MetricError { metric: METRIC_NAMES[metric], decides: DECIDES[metric], held_out: model.error[metric], typical: model.typical[metric].0.to_vec() }).collect(),
        policy: policy.into(),
        predictors: predictors.into(),
        current: request.current.as_ref().map(|model| model.version.clone()),
        fresh_scenes: judged.map_or(0, |verdict| verdict.scenes),
        current_error: judged.map(|verdict| verdict.current),
        candidate_error: judged.map(|verdict| verdict.candidate),
        accepted,
        reason,
        installed: false,
        elapsed_ms: started.elapsed().as_millis(),
    };
    Ok(TrainOutcome { model: trained.model, report, scenes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::mpeg::caramel_lab::{LAB_RECORD, LabPoint};
    use caramel_kernel::{SourceContext, SpatialFeatures, TemporalSummary};

    // A sample whose encodes follow its noise: a noisier scene scores lower at every CRF.
    fn sample(series: usize, scene: usize, collected: u64) -> LabSample {
        let noise = 0.2 + 0.35 * ((series * 7 + scene * 3) % 11) as f64;
        let motion = ((series + scene * 5) % 9) as f64;
        let points = [12.0f32, 16.0, 20.0, 24.0, 28.0]
            .into_iter()
            .map(|crf| {
                let quality = 44.0 - 0.5 * (crf as f64 - 20.0) - 1.5 * noise.ln_1p() - 0.1 * motion;
                LabPoint {
                    crf,
                    bytes: (4.0e6 * (-(crf as f64) / 6.0).exp2() * (1.0 + noise)) as u64,
                    xpsnr: quality,
                    xpsnr_low: quality - 1.0,
                    ssim_db: quality - 20.0,
                    vmaf: (quality + 53.0).min(100.0),
                    vmaf_low: (quality + 51.0).min(100.0),
                    cambi: 0.1,
                    cambi_high: 0.2 + 0.05 * crf as f64,
                }
            })
            .collect();
        LabSample {
            record: LAB_RECORD,
            source_id: format!("series {series} - 01.mkv"),
            series: format!("series {series}"),
            source: SourceContext { width: 1920, height: 1080, fps: 23.976, video_bitrate: Some(8e6), codec: Some("h264".into()), bit_depth: 8 },
            encoder: "x264|fast|-|aq-mode=3".to_string(),
            start_frame: scene as u64 * 100,
            scene_frames: 48,
            frames: 48,
            features: WindowFeatures {
                frames: 12,
                measured: 4,
                spatial: SpatialFeatures { noise_sigma: noise, si: 30.0, smooth_share: 0.5, ..Default::default() },
                temporal: Some(TemporalSummary { motion_px: motion, ti: 5.0, ..Default::default() }),
                ..Default::default()
            },
            cut_pairs_within: 0,
            reference_bit_depth: 8,
            cambi_source: 0.05,
            cambi_source_high: 0.1,
            points,
            picked: "spread".to_string(),
            model: String::new(),
            surprise: None,
            collected_unix: collected,
            elapsed_ms: 1,
        }
    }

    fn seeded(seed: u64) -> TrainConfig {
        TrainConfig { seed, ..Default::default() }
    }

    fn dataset(series: std::ops::Range<usize>, collected: u64) -> Vec<LabSample> {
        series.flat_map(|series| (0..30).map(move |scene| sample(series, scene, collected))).collect()
    }

    #[test]
    fn a_first_model_needs_a_dataset_that_could_teach_one() {
        let small = dataset(0..3, 100);
        let outcome = train_dataset(&TrainRequest { samples: &small, encoder: None, current: None, refit_targets: false, config: seeded(1) }).unwrap();
        assert!(!outcome.report.accepted && outcome.report.reason.contains("a first model needs"), "{}", outcome.report.reason);
        assert!(train_dataset(&TrainRequest { samples: &[], encoder: None, current: None, refit_targets: false, config: seeded(1) }).is_err());
    }

    #[test]
    fn a_dataset_trains_a_model_that_is_judged_and_named() {
        let mut samples = dataset(0..8, 100);
        // Samples made with other settings are another dataset.
        samples.push(LabSample { encoder: "x264|slow|-|".to_string(), ..sample(9, 0, 100) });
        let first = train_dataset(&TrainRequest { samples: &samples, encoder: None, current: None, refit_targets: false, config: seeded(1) }).unwrap();
        let report = &first.report;
        assert_eq!((report.scenes, report.series, report.other_encoders, report.members), (240, 8, 1, 10));
        assert!(report.accepted, "{}", report.reason);
        assert!(report.version.starts_with("caramel-neural-1.240-") && pnx264::scenes::valid_version(&report.version), "{}", report.version);
        assert_eq!(first.model.encoder, "x264|fast|-|aq-mode=3");
        assert!(report.error < 1.0, "{report:?}");
        // Every scene is judged at every anchor by networks that did not train on its series.
        assert_eq!(first.scenes.len(), 240 * train::ANCHORS.len());
        assert_eq!(first.scenes.iter().filter(|scene| scene.offset > 0.0).count() as f64 / first.scenes.len() as f64, report.policy.raised_share);
        // The noisier scene is the harder one, and the lab is guided by what the model says.
        let guide = ModelGuide(Arc::new(first.model.clone()));
        let (quiet, noisy) = (sample(0, 0, 0), sample(0, 3, 0));
        assert!(guide.predict(&quiet.features, false, 20.0)[0] > guide.predict(&noisy.features, false, 20.0)[0] + 0.5);
        let (low, high) = guide.band(0).unwrap();
        assert!(low < high && guide.band(curve::BITS).is_none());
        // Retrained on the same scenes, there is nothing new to judge it on.
        let current = Some(Arc::new(first.model.clone()));
        let same = train_dataset(&TrainRequest { samples: &samples, encoder: None, current: current.clone(), refit_targets: false, config: seeded(2) }).unwrap();
        assert!(!same.report.accepted && same.report.fresh_scenes == 0, "{}", same.report.reason);
        assert_ne!(same.report.version, report.version);
        assert_eq!(same.model.typical, first.model.typical);
        // With four more series measured since, it is, and it wins or ties.
        samples.extend(dataset(10..14, 900));
        let more = train_dataset(&TrainRequest { samples: &samples, encoder: Some("x264|fast|-|aq-mode=3".to_string()), current, refit_targets: false, config: seeded(2) }).unwrap();
        assert_eq!(more.report.fresh_scenes, 120);
        assert!(more.report.accepted, "{}", more.report.reason);
        assert_eq!(more.report.current.as_deref(), Some(report.version.as_str()));
    }
}
