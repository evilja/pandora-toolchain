// Caramel's lab: ground truth for the CRF model, made from real sources while nothing else needs
// the machine.
//
// A model that picks a scene's CRF from the source alone has to have been shown what encodes of
// such scenes actually do. The lab produces that: it takes scenes from one source, encodes each at
// several CRFs with the encoder settings a preset uses — video only, no subtitles, no scaling — and
// measures every encode against the source with XPSNR, SSIM, VMAF and CAMBI. One scene is one
// `LabSample`: what the analysis measured on it, and how quality and size fall as the CRF rises.
// Samples are appended to a JSON-lines dataset, which is what `caramel_train` fits a model to.
//
// Three things keep the encodes worth their time:
//
//   - Which scenes. Half are taken at even steps through the source, so the dataset says what a
//     release is made of. The other half are the scenes least like anything already measured, in
//     this source or any other, weighted by how unsure the current model is about them: a model
//     learns nothing from the hundredth quiet dialogue shot and a great deal from the one scene it
//     has no neighbour for.
//   - Which CRFs. Three to start with, then wherever the curve between two of them is in doubt and
//     matters: a gap is split when the measured quality bends there or crosses the quality the
//     model aims at, and left alone when the scene is far above or below it throughout.
//   - When to stop. A scene whose first three points land where the model said they would has
//     nothing more to teach and gets no further encodes.
//
// A scene is cut out of the source once, losslessly, and every encode and measurement reads that
// clip: a source deeper than 8 bits is kept at 10 and compared at 10, as verification does, so the
// encoder is not marked down for losing the dither of a conversion.
//
// Asked for, a scene is also encoded with a treatment (`DEBAND`) at a few CRFs, measured the same
// way. Those points are kept apart from the scene's curves, so an experiment never changes what the
// CRF model learns.
//
// Given an encode's decisions (`VerifyPlan`), the lab verifies them instead of choosing scenes: it
// takes the encode's raised shots and the kept ones that look as if they should have been raised
// (`verify_picks`), measures each at its usual CRFs and at the anchor, the CRF it was encoded at
// and two past that, and says what the decision did (`caramel_kernel::train::scene_verdict`).

use super::caramel::{
    CaramelReport, SceneEntry, VerifyEncoder, ffmpeg_version, has_filter, run_quiet, seek_seconds, stats_column, with_zones,
    xpsnr_frame,
};
use crate::lib::bin::resolve_runtime_binary;
use caramel_kernel::curve::{self, METRICS, Observation};
use caramel_kernel::{FastEncode, FastPoint, SourceContext, WindowFeatures, inputs};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const LAB_RECORD: u32 = 1;
// A scene shorter than a second says more about its opening keyframe than about itself.
pub const LAB_MIN_FRAMES: u64 = 24;
// Two seconds: the longest scene the decider makes, and the length the model decides for.
pub const LAB_MAX_FRAMES: u64 = 48;
// The CRFs a search may use. Anchors run from 17 to 25 and a scene may be judged up to eight away.
pub const LAB_CRF_RANGE: (f64, f64) = (10.0, 32.0);
const START_CRFS: [f64; 3] = [14.0, 21.0, 28.0];
// A gap narrower than this is not split: the curve between two points three CRF apart is a line to
// within what an encode's own noise is.
const MIN_SPLIT_GAP: f64 = 3.0;
const SPLIT_SCORE: f64 = 0.5;
// Mean distance between the model's curve and the first measured points, in each metric's own
// unit, under which the scene is taken as known.
const KNOWN_SURPRISE: f64 = 0.6;

// One encode of a scene, measured. Every figure but `bytes` is pooled over the frames after the
// clip's first, which is a keyframe a whole encode would not have there.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LabPoint {
    pub crf: f32,
    pub bytes: u64,
    // XPSNR pooled over the frames' errors, and the mean of the worst tenth of the frames.
    pub xpsnr: f64,
    pub xpsnr_low: f64,
    // SSIM as -10 log10(1 - mean).
    pub ssim_db: f64,
    pub vmaf: f64,
    pub vmaf_low: f64,
    // CAMBI of the encode: its mean, and the mean of the worst tenth of the frames.
    pub cambi: f64,
    pub cambi_high: f64,
    // Empty for a plain encode, else the treatment it was made with (`Treatment::name`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub treatment: String,
}

// A change to how a scene is encoded that the lab measures beside the plain encodes: a filter put
// before the encoder, at 8 bits, and x264 settings put after the preset's.
pub struct Treatment {
    pub name: &'static str,
    pub filter: &'static str,
    pub x264: &'static str,
}

// x264 smooths a gradient's dither away and leaves steps, even at CRF 17 and even from a source with
// none. `gradfun` rebuilds the gradient and dithers it, and a weaker deblock keeps more of the dither
// through the encode. On clean sources that took banding down by about 5 CAMBI at CRF 17 for fewer
// bytes. By CRF 22 the encoder smooths that dither away too, and on a source that bands itself the
// filter repaints its banding, which fidelity metrics mark down by 1.5 to 5 dB. Measured on 480
// ordinary scenes it barely moved banding at CRF 17 and not at all at 20, and the scenes it helped
// came mostly from one show: a choice for a source, not a scene, so the lab no longer spends encodes
// on it unless asked.
pub const DEBAND: Treatment = Treatment { name: "deband", filter: "gradfun=strength=1.2:radius=16", x264: "deblock=-2,-2" };

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabSample {
    pub record: u32,
    // What the scene came from: a stable id for the file, and the name its episodes share. Sources
    // of one series are near-duplicates, and a series is held out of training as a whole.
    pub source_id: String,
    pub series: String,
    pub source: SourceContext,
    // The encoder settings every point was made with: see `encoder_key`.
    pub encoder: String,
    pub start_frame: u64,
    pub scene_frames: u64,
    // Frames encoded and measured: the scene, up to `LAB_MAX_FRAMES`.
    pub frames: u64,
    pub features: WindowFeatures,
    // The same features over every frame the lab encoded rather than the scene's opening: what a
    // decision made from the whole scene would see. Absent from samples measured before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_features: Option<WindowFeatures>,
    pub cut_pairs_within: usize,
    pub reference_bit_depth: u8,
    // CAMBI of the source's own frames, as the encodes' is pooled.
    pub cambi_source: f64,
    pub cambi_source_high: f64,
    pub points: Vec<LabPoint>,
    // The scene's fast encode at each of `FAST_LAB_CRFS`, the one in `features` among them. Empty
    // for a sample measured before the lab read more than one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fast_ladder: Vec<FastPoint>,
    // Why the scene was taken: `spread` or `novel`.
    pub picked: String,
    // For a scene verified after an encode: what that encode decided and what came of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<Verified>,
    // The model that guided the search, and how far its curve was from the first points.
    pub model: String,
    pub surprise: Option<f64>,
    pub collected_unix: u64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Verified {
    // The model the encode decided with, its anchor, and the CRF it gave the scene.
    pub model: String,
    pub anchor: f32,
    pub decided: f32,
    // `held`, `overshot` or `missed`; None when the scene's points could not say.
    pub verdict: Option<String>,
}

impl LabSample {
    // Picked for being doubtful, so not a fair sample of what the model decides: a verified scene
    // other than a random control.
    pub fn chosen_for_doubt(&self) -> bool {
        self.picked.starts_with("verify-") && self.picked != "verify-control"
    }
    // The sample as the kernel fits it: every point as an observation of each metric at its CRF.
    pub fn observations(&self) -> Vec<Observation> {
        let pixels = (self.source.width as f64 * self.source.height as f64 * self.frames as f64).max(1.0);
        self.points
            .iter()
            .filter(|point| point.treatment.is_empty())
            .map(|point| Observation {
                crf: point.crf as f64,
                values: [
                    point.xpsnr,
                    point.xpsnr_low,
                    point.ssim_db,
                    point.vmaf,
                    point.vmaf_low,
                    // Banding the encode added: none is 0, and it only goes down from there.
                    -(point.cambi_high - self.cambi_source_high).max(0.0),
                    (point.bytes.max(1) as f64 * 8.0 / pixels).log2(),
                ],
            })
            .collect()
    }

    pub fn deep(&self) -> bool {
        self.source.bit_depth > 8
    }

    // A fast encode measured before it read banding is given the banding the lab's own encodes
    // added at `FAST_CRF` in its place (`banding_stand_in`), so the samples from before still train
    // the input. A scene measured before the banding its source is ready to show was read is given
    // its fast encode's banding, which that moves with on the same scale, or the same stand-in.
    pub fn inputs(&self) -> [f64; inputs::INPUTS] {
        let mut features = self.features.clone();
        if let Some(fast) = features.fast_encode.as_mut().filter(|fast| fast.banding.is_none()) {
            fast.banding = self.banding_stand_in();
        }
        if features.exposed_banding.is_none() {
            features.exposed_banding = features.fast_encode.and_then(|fast| fast.banding).or_else(|| self.banding_stand_in());
        }
        inputs::inputs(&features, self.deep())
    }

    // What the fast encode would have read as its banding, for a sample measured before it read
    // any: the lab's at `FAST_CRF`, scaled to the fast encode's (`STAND_IN_BANDING`).
    pub fn banding_stand_in(&self) -> Option<f64> {
        self.lab_banding(caramel_kernel::FAST_CRF as f64).map(|banding| ((banding + 0.05) * STAND_IN_BANDING - 0.05).max(0.0))
    }

    // The banding the lab's encodes added at `crf`, as the banding metric reads it (the worst
    // frames' CAMBI less the source's), drawn straight between the two encodes either side of it.
    // None without an encode to read.
    pub fn lab_banding(&self, crf: f64) -> Option<f64> {
        let mut points: Vec<(f64, f64)> = self.points.iter().filter(|point| point.treatment.is_empty()).map(|point| (point.crf as f64, point.cambi_high)).collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (first, last) = (*points.first()?, *points.last()?);
        let banding = if crf <= first.0 {
            first.1
        } else if crf >= last.0 {
            last.1
        } else {
            let after = points.iter().position(|point| point.0 >= crf)?;
            let ((low_crf, low), (high_crf, high)) = (points[after - 1], points[after]);
            low + (high - low) * (crf - low_crf) / (high_crf - low_crf).max(1e-9)
        };
        Some((banding - self.cambi_source_high).max(0.0))
    }

    // The sample as a model deciding from the whole scene would see it, when it was measured so.
    pub fn with_scene_features(&self) -> Option<LabSample> {
        let features = self.scene_features.clone()?;
        Some(LabSample { features, ..self.clone() })
    }
}

// Frames apart that whole-scene features measure by themselves: as densely as a scene's opening is
// measured, four frames of every twelve.
pub fn scene_feature_stride() -> usize {
    (caramel_kernel::SceneConfig::default().analysis_frames / caramel_kernel::scene::MEASURED_FRAMES).max(1)
}

// The x264 settings an encoder key names (`encoder_key`), as pnx264 takes them: what a sample's fast
// encode is made with, so samples measured since and samples backfilled alike read the settings
// their key says.
pub fn key_encoder(key: &str) -> Option<pnx264::Config> {
    let mut parts = key.splitn(4, '|');
    if parts.next()? != "x264" {
        return None;
    }
    let (preset, tune, params) = (parts.next()?, parts.next()?, parts.next().unwrap_or(""));
    Some(pnx264::Config {
        threads: 1,
        preset: Some(preset.to_string()),
        tune: (tune != "-").then(|| tune.to_string()),
        x264_params: (!params.is_empty()).then(|| params.to_string()),
        ..Default::default()
    })
}

// What the lab reads off a scene's clip besides its encodes: its whole-scene features, its fast
// encode (`caramel_kernel::fast`) made with `encoder`'s settings, and the same at every one of
// `FAST_LAB_CRFS`. `frames` frames of `input` from `start`, decoded at 8 bits as the scene analysis
// decodes them and cut out as the lab's clip is.
pub fn measure_scene(ffmpeg: &Path, input: &Path, fps: f64, start: u64, frames: u64, encoder: &pnx264::Config) -> Result<(WindowFeatures, FastEncode, Vec<FastPoint>), String> {
    let mut command = Command::new(ffmpeg);
    command.args(["-v", "error", "-nostdin"]);
    if let Some(seek) = seek_seconds(start, fps) {
        command.args(["-ss", &seek]);
    }
    let mut child = command
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", &frames.to_string(), "-an", "-sn", "-dn"])
        .args(["-vf", "setpts=N/FRAME_RATE/TB", "-pix_fmt", "yuv420p", "-strict", "-1", "-f", "yuv4mpegpipe", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg has no stdout")?;
    let result = (|| {
        let mut reader = pnx264::y4m::Y4mReader::new(std::io::BufReader::with_capacity(1 << 20, stdout))?;
        let (width, height) = (reader.width, reader.height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let mut window = caramel_kernel::SceneWindow::new(start as f64 / fps.max(1.0), scene_feature_stride());
        // Kept for the fast encode, which reads at most its first `FAST_FRAMES`, as an encoder does.
        let mut owned = Vec::new();
        while let Some(frame) = reader.next_frame()? {
            let y = caramel_kernel::frame::Plane::new(frame.y, width, height, frame.stride_y).ok_or("short luma plane")?;
            let u = caramel_kernel::frame::Plane::new(frame.u, chroma_width, chroma_height, frame.stride_c);
            let v = caramel_kernel::frame::Plane::new(frame.v, chroma_width, chroma_height, frame.stride_c);
            window.push(&caramel_kernel::frame::Frame { y, u, v });
            if owned.len() < caramel_kernel::FAST_FRAMES {
                owned.push(pnx264::scenes::OwnedFrame {
                    index: owned.len() as u64,
                    y: frame.y.to_vec(),
                    u: frame.u.to_vec(),
                    v: frame.v.to_vec(),
                    stride_y: frame.stride_y,
                    stride_c: frame.stride_c,
                });
            }
        }
        if owned.len() < 2 {
            return Err("the scene decoded to fewer than two frames".to_string());
        }
        let (fps_num, fps_den) = super::caramel::fps_ratio(fps);
        let config = pnx264::scenes::fast_config(&pnx264::Config { fps_num, fps_den, ..encoder.clone() }, width, height);
        let refs: Vec<&pnx264::scenes::OwnedFrame> = owned.iter().collect();
        let source = pnx264::scenes::source_banding(&refs, width, height);
        let fast = pnx264::scenes::fast_encode_from(&config, &refs, width, height, &source)?;
        let mut ladder = Vec::with_capacity(caramel_kernel::FAST_LAB_CRFS.len());
        for crf in caramel_kernel::FAST_LAB_CRFS {
            let encode = if crf == config.crf { fast } else { pnx264::scenes::fast_encode_from(&pnx264::Config { crf, ..config.clone() }, &refs, width, height, &source)? };
            ladder.push(FastPoint { crf, encode });
        }
        let mut whole = window.finish();
        whole.fast_encode = Some(fast);
        whole.exposed_banding = pnx264::scenes::exposed_banding_from(&refs, width, height, &source);
        Ok((whole, fast, ladder))
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

// What the current model says about a scene, for a lab that has one. Without it the search splits
// gaps by their width and their bend alone and never stops early.
pub trait LabGuide: Sync {
    fn version(&self) -> String;
    // The model's curve for the scene at a CRF, as `LabSample::observations` orders the metrics.
    fn predict(&self, features: &WindowFeatures, deep: bool, crf: f64) -> [f64; METRICS];
    // The quality a decision can turn on, per metric: what typical content scores at the highest
    // and at the lowest anchor. None for a metric no decision is made on.
    fn band(&self, metric: usize) -> Option<(f64, f64)>;
    // How unsure the model is about the scene, in CRF.
    fn uncertainty(&self, features: &WindowFeatures, deep: bool) -> f64;
}

// One decision to verify: the scene of the encode it was made for and why it was taken.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyPick {
    pub start: u64,
    pub frames: u64,
    pub anchor: f32,
    pub decided: f32,
    pub picked: &'static str,
}

// An encode's decisions to verify, and what its decisions were held to: the model's typical curves
// and its detail.
pub struct VerifyPlan {
    pub model: String,
    pub picks: Vec<VerifyPick>,
    pub typical: curve::Curves,
    pub detail: f64,
}

// The share of plain kept scenes verified at random: the honest estimate of the savings the model
// misses, since every other pick was chosen for looking doubtful.
pub const VERIFY_CONTROL_EVERY: u64 = 20;
// A kept scene is doubtful when two deciding metrics wanted a raise of at least this and another
// did not; when its ease over a detailed scene is at least `VERIFY_EASE`; or when its members
// disagree by at least `VERIFY_SPREAD`.
const VERIFY_SPLIT_VOTE: f64 = 1.0;
const VERIFY_EASE: f64 = 1.0;
const VERIFY_SPREAD: f64 = 2.0;
// The share of kept scenes with the easiest fast encodes that are doubtful for it.
const VERIFY_FAST_SHARE: f64 = 0.1;

// Which of an encode's decisions to verify, at most `count`, most worth it first.
//
// Consecutive scenes of one shot — a scene that ran to `max_scene_frames` and the one split off
// after it — are verified once, by the one raised most. Every raised shot is taken, largest raise
// first, then kept shots that look like missed savings: split votes, ease with no raise, an easy
// fast encode, members that disagree. Shots lowered below the anchor are not taken. One plain kept
// shot in `VERIFY_CONTROL_EVERY`, chosen by its start frame, is a control and has a share of the
// count kept for it.
pub fn verify_picks(records: &[pnx264::scenes::DecisionRecord], max_scene_frames: u64, count: usize) -> Vec<VerifyPick> {
    use pnx264::scenes::DecisionRecord;
    let mut shots: Vec<&DecisionRecord> = Vec::new();
    let mut previous: Option<&DecisionRecord> = None;
    for record in records {
        let continues = previous.is_some_and(|previous| previous.frames >= max_scene_frames && previous.start + previous.frames == record.start);
        match (continues, shots.last_mut()) {
            (true, Some(last)) if record.crf > last.crf => *last = record,
            (true, Some(_)) => {}
            _ => shots.push(record),
        }
        previous = Some(record);
    }
    let pick = |record: &DecisionRecord, picked: &'static str| VerifyPick { start: record.start, frames: record.frames, anchor: record.anchor, decided: record.crf, picked };
    let kept: Vec<&DecisionRecord> = shots.iter().copied().filter(|record| record.crf == record.anchor).collect();
    let mut fast: Vec<f64> = kept.iter().filter_map(|record| record.fast.map(|(xpsnr, _)| xpsnr)).collect();
    fast.sort_by(|a, b| b.total_cmp(a));
    let fast_floor = (fast.len() >= 10).then(|| fast[((fast.len() as f64 * VERIFY_FAST_SHARE) as usize).min(fast.len() - 1)]);
    let mut raised: Vec<(f64, VerifyPick)> = Vec::new();
    let mut doubtful: Vec<(usize, f64, VerifyPick)> = Vec::new();
    let mut controls: Vec<VerifyPick> = Vec::new();
    for record in shots {
        if record.crf > record.anchor {
            raised.push(((record.crf - record.anchor) as f64, pick(record, "verify-raise")));
            continue;
        }
        if record.crf < record.anchor {
            continue;
        }
        let wanting = record.votes.iter().filter(|(_, offset, _)| *offset >= VERIFY_SPLIT_VOTE).count();
        let holding = record.votes.iter().any(|(_, offset, _)| *offset <= 0.0);
        let spread = record.votes.iter().map(|(_, _, spread)| *spread).fold(0.0, f64::max);
        let easy_fast = record.fast.zip(fast_floor).is_some_and(|((xpsnr, _), floor)| xpsnr >= floor);
        if wanting >= 2 && holding {
            doubtful.push((0, wanting as f64, pick(record, "verify-split")));
        } else if record.ease >= VERIFY_EASE {
            doubtful.push((1, record.ease, pick(record, "verify-ease")));
        } else if easy_fast {
            doubtful.push((2, record.fast.map_or(0.0, |(xpsnr, _)| xpsnr), pick(record, "verify-fast")));
        } else if spread >= VERIFY_SPREAD {
            doubtful.push((3, spread, pick(record, "verify-spread")));
        } else if (record.start.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) % VERIFY_CONTROL_EVERY == 0 {
            controls.push(pick(record, "verify-control"));
        }
    }
    raised.sort_by(|a, b| b.0.total_cmp(&a.0));
    doubtful.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)));
    controls.truncate(count.div_ceil(10));
    let mut picks: Vec<VerifyPick> = raised.into_iter().map(|(_, pick)| pick).chain(doubtful.into_iter().map(|(_, _, pick)| pick)).take(count.saturating_sub(controls.len())).collect();
    picks.extend(controls);
    picks
}

pub struct LabRequest<'a> {
    pub report: &'a CaramelReport,
    pub source_id: String,
    pub series: String,
    pub encoder: VerifyEncoder,
    // How many scenes of this source the dataset should hold, counting the ones it already does.
    pub scenes: usize,
    // CRFs every scene is encoded at. Empty for a search.
    pub ladder: Vec<f64>,
    // CRFs every scene is also encoded at with `DEBAND`. Empty for none.
    pub treated: Vec<f64>,
    pub max_points: usize,
    pub workers: usize,
    pub scratch: PathBuf,
    // The dataset file samples are appended to. Scenes of this source already in it are not
    // measured again.
    pub dataset: PathBuf,
    // Inputs of the scenes other sources already gave, for novelty.
    pub known: Vec<[f64; inputs::INPUTS]>,
    pub guide: Option<&'a dyn LabGuide>,
    pub cancel_file: Option<PathBuf>,
    // Verify these decisions instead of choosing scenes.
    pub verify: Option<&'a VerifyPlan>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LabSummary {
    pub source_id: String,
    pub eligible: usize,
    pub already: usize,
    pub measured: usize,
    pub failed: usize,
    pub points: usize,
    pub cancelled: bool,
    pub elapsed_ms: u128,
    // For a verification: how many decisions came out `held`, `overshot` and `missed`, by why they
    // were picked (`verify-raise`, `verify-control`, …).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub verdicts: std::collections::BTreeMap<String, std::collections::BTreeMap<String, usize>>,
}

// The encoder settings as one stable string: samples made with different settings are different
// datasets, and a model is fitted to one of them.
pub fn encoder_key(encoder: &VerifyEncoder) -> String {
    let mut params: Vec<&str> = encoder.x264_params.as_deref().unwrap_or_default().split(':').filter(|pair| !pair.is_empty() && !pair.starts_with("zones=")).collect();
    params.sort_unstable();
    format!(
        "x264|{}|{}|{}",
        encoder.preset.as_deref().unwrap_or("medium"),
        encoder.tune.as_deref().unwrap_or("-"),
        params.join(":")
    )
}

// The name a source's episodes share, from its file name: the release group, the episode number,
// the resolution and the checksum taken off, what is left lowercased. `[Group] Some Show S2 - 07
// (1080p) [ABCD1234].mkv` and its episode 8 both come out as `some show s2`.
pub fn series_key(name: &str) -> String {
    let stem = Path::new(name).file_stem().and_then(|stem| stem.to_str()).unwrap_or(name);
    let mut text = String::new();
    let mut depth = 0usize;
    for c in stem.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(if c == '_' || c == '.' { ' ' } else { c }),
            _ => {}
        }
    }
    let mut words: Vec<String> = text.split_whitespace().map(|word| word.to_lowercase()).collect();
    // `S01E03`: the episode and everything after it goes, and the season stays as `s1`, which is
    // how a release that writes `S1 - 03` comes out too.
    let numbered = |digits: &str, most: usize| !digits.is_empty() && digits.len() <= most && digits.chars().all(|c| c.is_ascii_digit());
    let season_episode = words.iter().enumerate().skip(1).find_map(|(index, word)| {
        let (season, episode) = word.strip_prefix('s')?.split_once('e')?;
        (numbered(season, 2) && numbered(episode.split('v').next().unwrap_or_default(), 4)).then(|| (index, season.parse::<u32>().unwrap_or(0)))
    });
    if let Some((index, season)) = season_episode {
        words.truncate(index);
        words.push(format!("s{season}"));
        return words.iter().filter(|word| word.as_str() != "-").cloned().collect::<Vec<_>>().join(" ");
    }
    // The episode number is the last bare number, and with it goes everything after: a version
    // suffix, an episode title.
    let episode = words.iter().rposition(|word| {
        let digits = word.trim_start_matches(['e', 'p', '#']).split('v').next().unwrap_or_default();
        !digits.is_empty() && digits.len() <= 4 && digits.chars().all(|c| c.is_ascii_digit())
    });
    let kept = match episode {
        Some(index) if index > 0 => &words[..index],
        _ => &words[..],
    };
    let key = kept.iter().filter(|word| word.as_str() != "-").cloned().collect::<Vec<_>>().join(" ");
    if key.is_empty() { stem.to_lowercase() } else { key }
}

// Every sample of a dataset file. A line that does not parse — a write cut short by a kill — is
// skipped rather than failing the file.
pub fn read_samples(path: &Path) -> Vec<LabSample> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<LabSample>(line).ok())
        .filter(|sample| sample.record == LAB_RECORD)
        .collect()
}

// Every sample under a dataset directory, file by file in name order.
pub fn read_dataset(directory: &Path) -> Vec<LabSample> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .map(|entries| entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect())
        .unwrap_or_default();
    files.retain(|path| path.extension().and_then(|extension| extension.to_str()) == Some("jsonl"));
    files.sort();
    files.iter().flat_map(|path| read_samples(path)).collect()
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
}

// The mean of the tenth of `values` at one end: the lowest when `lowest`, the highest otherwise.
fn tail_mean(values: &[f64], lowest: bool) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| if lowest { a.total_cmp(b) } else { b.total_cmp(a) });
    sorted.truncate((values.len() / 10).max(1));
    mean(&sorted)
}

// Without the opening keyframe, unless that is all there is.
fn settled(values: &[f64]) -> &[f64] {
    if values.len() > 1 { &values[1..] } else { values }
}

// The scale each input is compared on: its spread over `vectors`, so no one figure decides what
// counts as different. An input that does not vary counts for nothing.
fn input_scales(vectors: &[[f64; inputs::INPUTS]]) -> [f64; inputs::INPUTS] {
    let mut scales = [0.0; inputs::INPUTS];
    if vectors.is_empty() {
        return scales;
    }
    for (index, scale) in scales.iter_mut().enumerate() {
        let column: Vec<f64> = vectors.iter().map(|vector| vector[index]).collect();
        let centre = mean(&column);
        let deviation = (column.iter().map(|value| (value - centre).powi(2)).sum::<f64>() / column.len() as f64).sqrt();
        *scale = if deviation > 1e-6 { 1.0 / deviation } else { 0.0 };
    }
    scales
}

fn distance(a: &[f64; inputs::INPUTS], b: &[f64; inputs::INPUTS], scales: &[f64; inputs::INPUTS]) -> f64 {
    a.iter().zip(b).zip(scales).map(|((a, b), scale)| ((a - b) * scale).powi(2)).sum::<f64>().sqrt()
}

// Which of `candidates` to measure, `count` of them, each with why it was taken. `taken` and
// `known` are the inputs of scenes already measured, in this source and in others; `uncertainty`
// is the model's, per candidate, in CRF (zeros without a model).
pub fn select_scenes(
    candidates: &[[f64; inputs::INPUTS]],
    uncertainty: &[f64],
    known: &[[f64; inputs::INPUTS]],
    count: usize,
) -> Vec<(usize, &'static str)> {
    let count = count.min(candidates.len());
    let mut chosen: Vec<(usize, &'static str)> = Vec::new();
    if count == 0 {
        return chosen;
    }
    // Even steps first: the middle of each of `spread` equal runs.
    let spread = count.div_ceil(2);
    for slot in 0..spread {
        let index = (2 * slot + 1) * candidates.len() / (2 * spread);
        if !chosen.iter().any(|(taken, _)| *taken == index) {
            chosen.push((index, "spread"));
        }
    }
    let all: Vec<[f64; inputs::INPUTS]> = candidates.iter().chain(known).copied().collect();
    let scales = input_scales(&all);
    // Each candidate's distance to the nearest measured scene, kept current as scenes are taken.
    let mut nearest: Vec<f64> = candidates
        .iter()
        .map(|candidate| {
            known
                .iter()
                .chain(chosen.iter().map(|(index, _)| &candidates[*index]))
                .map(|other| distance(candidate, other, &scales))
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    while chosen.len() < count {
        let best = (0..candidates.len())
            .filter(|index| !chosen.iter().any(|(taken, _)| taken == index))
            .max_by(|&a, &b| {
                let score = |index: usize| nearest[index].min(1e9) * (1.0 + uncertainty.get(index).copied().unwrap_or(0.0).clamp(0.0, 4.0));
                score(a).total_cmp(&score(b))
            });
        let Some(best) = best else {
            break;
        };
        chosen.push((best, "novel"));
        for (index, slot) in nearest.iter_mut().enumerate() {
            *slot = slot.min(distance(&candidates[index], &candidates[best], &scales));
        }
    }
    chosen.sort_unstable_by_key(|(index, _)| *index);
    chosen
}

// The CRF to encode next, given what has been measured, or None when the scene is done. `bands`
// holds, per metric, the quality a decision can turn on.
pub fn next_crf(measured: &[Observation], bands: &[Option<(f64, f64)>; METRICS], max_points: usize) -> Option<f64> {
    let has = |crf: f64| measured.iter().any(|point| (point.crf - crf).abs() < 0.25);
    if let Some(start) = START_CRFS.iter().copied().find(|crf| !has(*crf)).filter(|_| measured.len() < START_CRFS.len()) {
        return Some(start);
    }
    if measured.len() >= max_points {
        return None;
    }
    let mut sorted: Vec<Observation> = measured.to_vec();
    sorted.sort_by(|a, b| a.crf.total_cmp(&b.crf));
    let (first, last) = (&sorted[0], &sorted[sorted.len() - 1]);
    let judged: Vec<usize> = (0..METRICS).filter(|metric| curve::DECIDES[*metric] && bands[*metric].is_some()).collect();
    // The scene is still short of the best quality aimed at, at the lowest CRF measured: what it
    // needs lies below. And the reverse at the top.
    if first.crf > LAB_CRF_RANGE.0 + 0.5 && judged.iter().any(|&metric| first.values[metric] < bands[metric].unwrap().1) {
        return Some((first.crf - 4.0).max(LAB_CRF_RANGE.0));
    }
    if last.crf < LAB_CRF_RANGE.1 - 0.5 && judged.iter().any(|&metric| last.values[metric] > bands[metric].unwrap().0) {
        return Some((last.crf + 4.0).min(LAB_CRF_RANGE.1));
    }
    // How far the middle point of three lies from the line through its neighbours, per metric in
    // its own unit: the bend of the curve there.
    let bend = |index: usize| -> f64 {
        if index == 0 || index + 1 >= sorted.len() {
            return 0.0;
        }
        let (a, b, c) = (&sorted[index - 1], &sorted[index], &sorted[index + 1]);
        let t = (b.crf - a.crf) / (c.crf - a.crf);
        (0..METRICS)
            .map(|metric| (b.values[metric] - (a.values[metric] + (c.values[metric] - a.values[metric]) * t)).abs() / curve::SCALES[metric])
            .sum::<f64>()
            / METRICS as f64
    };
    let mut best: Option<(f64, f64)> = None;
    for index in 0..sorted.len() - 1 {
        let (a, b) = (&sorted[index], &sorted[index + 1]);
        let width = b.crf - a.crf;
        if width < MIN_SPLIT_GAP {
            continue;
        }
        // The share of the judged metrics whose target lies between the gap's two ends.
        let relevance = if judged.is_empty() {
            0.75
        } else {
            judged
                .iter()
                .filter(|&&metric| {
                    let (low, high) = bands[metric].unwrap();
                    a.values[metric] >= low && b.values[metric] <= high
                })
                .count() as f64
                / judged.len() as f64
        };
        let score = width / 4.0 * (0.25 + relevance) * (0.5 + bend(index).max(bend(index + 1)));
        if score >= SPLIT_SCORE && best.is_none_or(|(_, top)| score > top) {
            best = Some((((a.crf + b.crf) / 2.0).round(), score));
        }
    }
    best.map(|(crf, _)| crf).filter(|crf| !has(*crf))
}

// How far the model's curve lies from the measured points, per metric in its own unit, averaged.
pub fn surprise(measured: &[Observation], predict: &dyn Fn(f64) -> [f64; METRICS]) -> f64 {
    let mut total = 0.0;
    let mut count = 0usize;
    for point in measured {
        let predicted = predict(point.crf);
        for metric in 0..METRICS {
            total += ((predicted[metric] - point.values[metric]).abs() / curve::SCALES[metric]).min(8.0);
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { total / count as f64 }
}

struct Lab<'a> {
    request: &'a LabRequest<'a>,
    ffmpeg: PathBuf,
    deep: bool,
    fps: f64,
    input: PathBuf,
    scratch: PathBuf,
    encoder: String,
}

// The clip's per-frame figures out of libvmaf's JSON log: VMAF, the encode's CAMBI, the source's.
fn vmaf_frames(path: &Path) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let frames = value.get("frames")?.as_array()?;
    let column = |key: &str| -> Vec<f64> {
        frames.iter().filter_map(|frame| frame.get("metrics")?.get(key)?.as_f64()).collect()
    };
    Some((column("vmaf"), column("cambi"), column("cambi_source")))
}

impl Lab<'_> {
    fn cancelled(&self) -> bool {
        self.request.cancel_file.as_ref().is_some_and(|path| path.exists())
    }

    // The scene's frames, cut out once and kept exactly: 8-bit sources as they are, deeper ones at
    // 10 bits. Frames are renumbered so irregular source timestamps cannot repeat or drop one.
    fn extract(&self, directory: &Path, start: u64, frames: u64) -> Result<PathBuf, String> {
        let reference = directory.join("ref.y4m");
        let mut command = Command::new(&self.ffmpeg);
        command.args(["-v", "error", "-nostdin", "-y"]);
        if let Some(seek) = seek_seconds(start, self.fps) {
            command.args(["-ss", &seek]);
        }
        command
            .arg("-i")
            .arg(&self.input)
            .args(["-map", "0:v:0", "-frames:v", &frames.to_string(), "-an", "-sn", "-dn"])
            .args(["-vf", "setpts=N/FRAME_RATE/TB", "-pix_fmt", if self.deep { "yuv420p10le" } else { "yuv420p" }])
            .args(["-strict", "-1", "-f", "yuv4mpegpipe"])
            .arg(&reference);
        run_quiet(&mut command, "the lab's clip")?;
        Ok(reference)
    }

    // One encode of the clip, plain or treated, and its measurement. The second value is the
    // source's own CAMBI per frame, which every measurement of a clip reads again and the first one
    // is kept from.
    fn point(&self, directory: &Path, reference: &Path, crf: f64, treatment: Option<&Treatment>) -> Result<(LabPoint, Vec<f64>), String> {
        let clip = directory.join("enc.mkv");
        let mut encode = Command::new(&self.ffmpeg);
        encode.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(reference).arg("-an");
        if let Some(treatment) = treatment {
            encode.args(["-vf", &format!("format=yuv420p,{}", treatment.filter)]);
        }
        encode.args(["-pix_fmt", "yuv420p", "-c:v", "libx264"]);
        let encoder = &self.request.encoder;
        if let Some(preset) = &encoder.preset {
            encode.args(["-preset", preset]);
        }
        if let Some(tune) = &encoder.tune {
            encode.args(["-tune", tune]);
        }
        // The treatment's settings come last, so they win over the preset's.
        let params = match (with_zones(encoder.x264_params.as_deref(), ""), treatment) {
            (Some(params), Some(treatment)) => Some(format!("{params}:{}", treatment.x264)),
            (None, Some(treatment)) => Some(treatment.x264.to_string()),
            (params, None) => params,
        };
        if let Some(params) = params {
            encode.args(["-x264-params", &params]);
        }
        encode.args(["-crf", &format!("{crf}")]).arg(&clip);
        run_quiet(&mut encode, "the lab's encode")?;
        let bytes = std::fs::metadata(&clip).map(|meta| meta.len()).unwrap_or(0);

        // Frames are paired by index on both sides, as verification pairs them. The stats files
        // are named relative to the working directory, which keeps paths with `:` out of the graph.
        const BY_INDEX: &str = "settb=1/1000,setpts=N*1000";
        let raised = if self.deep { "format=yuv420p10le," } else { "" };
        let graph = format!(
            "[1:v]{raised}{BY_INDEX},split=3[e1][e2][e3];[0:v]{BY_INDEX},split=3[r1][r2][r3];\
             [e1][r1]ssim=stats_file=ssim.log:shortest=1;[e2][r2]xpsnr=stats_file=xpsnr.log:shortest=1;\
             [e3][r3]libvmaf=log_path=vmaf.json:log_fmt=json:n_threads=4:feature=name=cambi\\\\:full_ref=true:shortest=1"
        );
        let mut measure = Command::new(&self.ffmpeg);
        measure
            .current_dir(directory)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(reference)
            .arg("-i")
            .arg(&clip)
            .args(["-lavfi", &graph, "-f", "null", "-"]);
        run_quiet(&mut measure, "the lab's measurement")?;
        let ssim = stats_column(&directory.join("ssim.log"), "All");
        let xpsnr: Vec<f64> = std::fs::read_to_string(directory.join("xpsnr.log")).unwrap_or_default().lines().filter_map(xpsnr_frame).collect();
        let (vmaf, cambi, cambi_source) = vmaf_frames(&directory.join("vmaf.json")).ok_or("the lab's measurement wrote no VMAF log")?;
        if ssim.is_empty() || xpsnr.is_empty() || vmaf.is_empty() || cambi.is_empty() {
            return Err("the lab's measurement compared no frames".to_string());
        }
        let xpsnr = settled(&xpsnr);
        let point = LabPoint {
            crf: crf as f32,
            bytes,
            xpsnr: -10.0 * mean(&xpsnr.iter().map(|db| 10f64.powf(-db / 10.0)).collect::<Vec<_>>()).log10(),
            xpsnr_low: tail_mean(xpsnr, true),
            ssim_db: -10.0 * (1.0 - mean(settled(&ssim))).max(1e-6).log10(),
            vmaf: mean(settled(&vmaf)),
            vmaf_low: tail_mean(settled(&vmaf), true),
            cambi: mean(settled(&cambi)),
            cambi_high: tail_mean(settled(&cambi), false),
            treatment: treatment.map(|treatment| treatment.name.to_string()).unwrap_or_default(),
        };
        Ok((point, cambi_source))
    }

    // The CRFs a verified decision is measured at: the lab's own, so the sample is a whole curve
    // like any other, and the anchor, the decided CRF and `VERIFY_STEP` past it, for the verdict.
    fn verify_ladder(&self, pick: &VerifyPick) -> Vec<f64> {
        let base: Vec<f64> = if self.request.ladder.is_empty() { START_CRFS.to_vec() } else { self.request.ladder.clone() };
        let step = caramel_kernel::train::VERIFY_STEP;
        let mut ladder: Vec<f64> = base.into_iter().chain([pick.anchor as f64, pick.decided as f64, pick.decided as f64 + step]).collect();
        ladder.sort_by(f64::total_cmp);
        ladder.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        ladder
    }

    fn scene(&self, slot: usize, scene: &SceneEntry, picked: &str, verify: Option<&VerifyPick>) -> Result<LabSample, String> {
        let started = std::time::Instant::now();
        let request = self.request;
        let directory = self.scratch.join(format!("scene-{slot}"));
        std::fs::create_dir_all(&directory).map_err(|e| format!("lab scratch: {e}"))?;
        let frames = scene.frames.min(LAB_MAX_FRAMES);
        let mut sample = LabSample {
            record: LAB_RECORD,
            source_id: request.source_id.clone(),
            series: request.series.clone(),
            source: request.report.source.clone(),
            encoder: self.encoder.clone(),
            start_frame: scene.start_frame,
            scene_frames: scene.frames,
            frames,
            features: scene.features.clone(),
            scene_features: None,
            cut_pairs_within: scene.cut_pairs_within,
            reference_bit_depth: if self.deep { 10 } else { 8 },
            cambi_source: 0.0,
            cambi_source_high: 0.0,
            points: Vec::new(),
            fast_ladder: Vec::new(),
            picked: picked.to_string(),
            verified: None,
            model: request.guide.map(|guide| guide.version()).unwrap_or_default(),
            surprise: None,
            collected_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            elapsed_ms: 0,
        };
        let result = (|| {
            let reference = self.extract(&directory, scene.start_frame, frames)?;
            let encoder = key_encoder(&self.encoder).ok_or("the lab measures x264 encodes only")?;
            let (whole, fast, ladder) = measure_scene(&self.ffmpeg, &self.input, self.fps, scene.start_frame, frames, &encoder)?;
            sample.features.exposed_banding = whole.exposed_banding;
            sample.scene_features = Some(whole);
            sample.features.fast_encode = Some(fast);
            sample.fast_ladder = ladder;
            let bands: [Option<(f64, f64)>; METRICS] = std::array::from_fn(|metric| request.guide.and_then(|guide| guide.band(metric)));
            let max_points = request.max_points.max(START_CRFS.len());
            let ladder = verify.map_or_else(|| request.ladder.clone(), |pick| self.verify_ladder(pick));
            loop {
                if self.cancelled() {
                    return Err("cancelled".to_string());
                }
                let measured = sample.observations();
                let next = if ladder.is_empty() {
                    next_crf(&measured, &bands, max_points)
                } else {
                    ladder.iter().copied().find(|crf| !sample.points.iter().any(|point| point.treatment.is_empty() && (point.crf as f64 - crf).abs() < 0.01))
                };
                let Some(crf) = next else {
                    break;
                };
                let (point, cambi_source) = self.point(&directory, &reference, crf, None)?;
                if sample.points.is_empty() {
                    sample.cambi_source = mean(settled(&cambi_source));
                    sample.cambi_source_high = tail_mean(settled(&cambi_source), false);
                }
                sample.points.push(point);
                // The model's curve against the first points: a scene it already knows gets no more.
                if let (Some(guide), true) = (request.guide, ladder.is_empty() && sample.points.len() == START_CRFS.len()) {
                    let deep = self.deep;
                    let measured = sample.observations();
                    let off = surprise(&measured, &|crf| guide.predict(&scene.features, deep, crf));
                    sample.surprise = Some(off);
                    if off < KNOWN_SURPRISE {
                        break;
                    }
                }
            }
            for crf in &request.treated {
                if self.cancelled() {
                    return Err("cancelled".to_string());
                }
                sample.points.push(self.point(&directory, &reference, *crf, Some(&DEBAND))?.0);
            }
            Ok(())
        })();
        std::fs::remove_dir_all(&directory).ok();
        result?;
        sample.points.sort_by(|a, b| a.crf.total_cmp(&b.crf));
        if let (Some(pick), Some(plan)) = (verify, request.verify) {
            let verdict = caramel_kernel::train::scene_verdict(&sample.observations(), &plan.typical, plan.detail, pick.anchor as f64, pick.decided as f64);
            sample.verified = Some(Verified { model: plan.model.clone(), anchor: pick.anchor, decided: pick.decided, verdict: verdict.map(|verdict| verdict.name().to_string()) });
        }
        sample.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(sample)
    }
}

// How much of the lab's banding at `FAST_CRF` a fast encode reads, offset as the banding input is.
// Fitted on 544 scenes of 17 series measured both ways (log-scale correlation 0.89): the fast
// encode reads about half as much. The share differs by series (0.24 for Cyberpunk Edgerunners to
// 1.33 for Patlabor 2, grainy films and CG lowest) and nothing in the source's features predicted
// it on held-out series, so the stand-in is noisy; a real reading replaces it whenever the source
// can be read again.
pub const STAND_IN_BANDING: f64 = 0.53;

// What a backfill did to one source's samples in a dataset file.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BackfillSummary {
    pub source_id: String,
    pub samples: usize,
    pub filled: usize,
    pub already: usize,
    pub failed: usize,
}

// Gives the samples of `source_id` in `dataset` the whole-scene features, the fast encode, the fast
// encodes at `FAST_LAB_CRFS` and the banding the source is ready to show that they were measured
// without — or a fast encode made before it read banding — read from `input`, which must be the source they were measured from. Other lines
// are left as they are, and the file is replaced only once every sample was tried.
pub fn backfill_scene_features(dataset: &Path, input: &Path, source_id: &str, workers: usize) -> Result<BackfillSummary, String> {
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let text = std::fs::read_to_string(dataset).map_err(|e| format!("{}: {e}", dataset.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut summary = BackfillSummary { source_id: source_id.to_string(), ..Default::default() };
    let mut samples: Vec<Option<LabSample>> =
        lines.iter().map(|line| serde_json::from_str::<LabSample>(line).ok().filter(|sample| sample.source_id == source_id)).collect();
    let todo: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| sample.as_ref().filter(|sample| sample.scene_features.is_none() || sample.features.exposed_banding.is_none() || sample.features.fast_encode.is_none_or(|fast| fast.banding.is_none()) || sample.fast_ladder.is_empty()).map(|_| index))
        .collect();
    summary.samples = samples.iter().flatten().count();
    summary.already = summary.samples - todo.len();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<(WindowFeatures, FastEncode, Vec<FastPoint>), String>)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, todo.len().max(1)) {
            scope.spawn(|| {
                while let Some(&index) = todo.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let sample = samples[index].as_ref().unwrap();
                    let measured = key_encoder(&sample.encoder)
                        .ok_or_else(|| format!("`{}` is not an x264 encoder", sample.encoder))
                        .and_then(|encoder| measure_scene(&ffmpeg, input, sample.source.fps, sample.start_frame, sample.frames, &encoder));
                    crate::lib::sync::lock(&results).push((index, measured));
                }
            });
        }
    });
    for (index, measured) in results.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner()) {
        match measured {
            Ok((whole, fast, ladder)) => {
                let sample = samples[index].as_mut().unwrap();
                sample.features.exposed_banding = whole.exposed_banding;
                sample.scene_features = Some(whole);
                sample.features.fast_encode = Some(fast);
                sample.fast_ladder = ladder;
                summary.filled += 1;
            }
            Err(_) => summary.failed += 1,
        }
    }
    if summary.filled == 0 {
        return Ok(summary);
    }
    let rewritten: String = lines
        .iter()
        .zip(&samples)
        .map(|(line, sample)| match sample {
            Some(sample) => serde_json::to_string(sample).unwrap() + "\n",
            None => format!("{line}\n"),
        })
        .collect();
    let temporary = dataset.with_extension("jsonl.backfill");
    std::fs::write(&temporary, rewritten).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, dataset).map_err(|e| format!("{}: {e}", dataset.display()))?;
    Ok(summary)
}

// Measures scenes of the report's source into the dataset file, and says what it did. Scenes are
// measured `workers` at a time and each is appended as soon as it is done, so a lab that is killed
// — which is how it is told to make way for an encode — keeps everything it finished.
pub fn run_lab(request: &LabRequest) -> Result<LabSummary, String> {
    let started = std::time::Instant::now();
    let report = request.report;
    let ffmpeg: PathBuf = resolve_runtime_binary("ffmpeg").into();
    let ffmpeg = if ffmpeg.components().count() > 1 { std::fs::canonicalize(&ffmpeg).unwrap_or(ffmpeg) } else { ffmpeg };
    for filter in ["xpsnr", "libvmaf"] {
        if !has_filter(&ffmpeg, filter) {
            return Err(format!("this ffmpeg has no `{filter}` filter ({})", ffmpeg_version(&ffmpeg)));
        }
    }
    // A killed lab leaves its clips behind; nothing else writes here.
    std::fs::remove_dir_all(&request.scratch).ok();
    std::fs::create_dir_all(&request.scratch).map_err(|e| format!("lab scratch: {e}"))?;
    let scratch = std::fs::canonicalize(&request.scratch).map_err(|e| format!("lab scratch: {e}"))?;
    if let Some(parent) = request.dataset.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("lab dataset: {e}"))?;
    }
    let lab = Lab {
        request,
        ffmpeg,
        deep: report.source.bit_depth > 8,
        fps: report.source.fps,
        input: std::fs::canonicalize(&report.input).unwrap_or_else(|_| PathBuf::from(&report.input)),
        scratch,
        encoder: encoder_key(&request.encoder),
    };
    let existing: Vec<LabSample> =
        read_samples(&request.dataset).into_iter().filter(|sample| sample.source_id == request.source_id && sample.encoder == lab.encoder).collect();
    let already: HashSet<u64> = existing.iter().map(|sample| sample.start_frame).collect();
    // A scene with a cut inside its measured frames is two shots read as one: its features describe
    // neither, and the decider does not raise it whatever a model says.
    let eligible: Vec<&SceneEntry> = report
        .scenes
        .iter()
        .filter(|scene| scene.frames >= LAB_MIN_FRAMES && scene.cut_pairs_within == 0 && !already.contains(&scene.start_frame))
        .collect();
    let mut summary = LabSummary { source_id: request.source_id.clone(), eligible: eligible.len(), already: existing.len(), ..Default::default() };
    let candidates: Vec<[f64; inputs::INPUTS]> = eligible.iter().map(|scene| inputs::inputs(&scene.features, lab.deep)).collect();
    let uncertainty: Vec<f64> =
        eligible.iter().map(|scene| request.guide.map_or(0.0, |guide| guide.uncertainty(&scene.features, lab.deep))).collect();
    let known: Vec<[f64; inputs::INPUTS]> = request.known.iter().copied().chain(existing.iter().map(LabSample::inputs)).collect();
    // Each scene to measure, why, and the decision it verifies.
    let chosen: Vec<(usize, &'static str, Option<&VerifyPick>)> = match request.verify {
        None => select_scenes(&candidates, &uncertainty, &known, request.scenes.saturating_sub(existing.len())).into_iter().map(|(index, picked)| (index, picked, None)).collect(),
        // The analysis splits scenes as the encode's chunks did not, so a decision is matched to the
        // scene that starts where it does, else to the one it starts inside.
        Some(plan) => {
            let mut taken: Vec<(usize, &'static str, Option<&VerifyPick>)> = Vec::new();
            for pick in &plan.picks {
                let index = eligible
                    .iter()
                    .position(|scene| scene.start_frame == pick.start)
                    .or_else(|| eligible.iter().position(|scene| scene.start_frame <= pick.start && pick.start < scene.start_frame + scene.frames));
                if let Some(index) = index.filter(|index| !taken.iter().any(|(other, _, _)| other == index)) {
                    taken.push((index, pick.picked, Some(pick)));
                }
            }
            taken
        }
    };

    let next = AtomicUsize::new(0);
    let output = Mutex::new((std::fs::OpenOptions::new().create(true).append(true).open(&request.dataset).map_err(|e| format!("lab dataset: {e}"))?, 0usize, 0usize, 0usize));
    let verdicts: Mutex<std::collections::BTreeMap<String, std::collections::BTreeMap<String, usize>>> = Mutex::default();
    std::thread::scope(|scope| {
        for _ in 0..request.workers.clamp(1, chosen.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let slot = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(index, picked, verify)) = chosen.get(slot) else {
                        break;
                    };
                    if lab.cancelled() {
                        break;
                    }
                    let result = lab.scene(slot, eligible[index], picked, verify);
                    let mut output = crate::lib::sync::lock(&output);
                    match result {
                        Ok(sample) => {
                            let line = serde_json::to_string(&sample).unwrap();
                            if writeln!(output.0, "{line}").and_then(|_| output.0.flush()).is_ok() {
                                output.1 += 1;
                                output.2 += sample.points.len();
                                if let Some(verdict) = sample.verified.as_ref().and_then(|verified| verified.verdict.clone()) {
                                    *crate::lib::sync::lock(&verdicts).entry(sample.picked.clone()).or_default().entry(verdict).or_default() += 1;
                                }
                            }
                        }
                        Err(error) if error == "cancelled" => {}
                        Err(_) => output.3 += 1,
                    }
                }
            });
        }
    });
    std::fs::remove_dir_all(&lab.scratch).ok();
    let output = output.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
    summary.measured = output.1;
    summary.points = output.2;
    summary.failed = output.3;
    summary.cancelled = lab.cancelled();
    summary.elapsed_ms = started.elapsed().as_millis();
    summary.verdicts = verdicts.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(crf: f64, quality: f64) -> Observation {
        Observation { crf, values: [quality, quality - 1.0, quality - 20.0, 95.0, 93.0, 0.0, -3.0 - crf * 0.19] }
    }

    fn bands(low: f64, high: f64) -> [Option<(f64, f64)>; METRICS] {
        let mut bands = [None; METRICS];
        bands[0] = Some((low, high));
        bands
    }

    #[test]
    fn a_search_starts_at_three_crfs_spread_over_the_range() {
        let mut measured = Vec::new();
        for expected in START_CRFS {
            assert_eq!(next_crf(&measured, &[None; METRICS], 7), Some(expected));
            measured.push(observation(expected, 50.0 - expected * 0.5));
        }
    }

    // A line needs no more points than the ones that drew it: with no bend and no target inside a
    // gap, the scene is done. A target inside one is what makes the gap worth an encode.
    #[test]
    fn a_gap_is_split_where_the_target_lies() {
        let line: Vec<Observation> = START_CRFS.iter().map(|crf| observation(*crf, 50.0 - crf * 0.5)).collect();
        // Aimed-at quality 38.0 to 41.0: crossed between CRF 18 and 24, either side of 21.
        let next = next_crf(&line, &bands(38.0, 41.0), 7).unwrap();
        assert!(next == 18.0 || next == 25.0 || next == 17.0 || next == 24.0, "{next}");
        // Nowhere near the scene's quality at any CRF measured, and the range is used up above.
        let far: Vec<Observation> = [10.0, 14.0, 21.0, 28.0, 32.0].iter().map(|crf| observation(*crf, 80.0 - crf * 0.5)).collect();
        assert_eq!(next_crf(&far, &bands(38.0, 41.0), 7), None);
        assert_eq!(next_crf(&line, &bands(38.0, 41.0), 3), None);
    }

    #[test]
    fn a_scene_short_of_the_target_everywhere_is_measured_lower() {
        let hard: Vec<Observation> = START_CRFS.iter().map(|crf| observation(*crf, 44.0 - crf * 0.5)).collect();
        // At CRF 14 it scores 37.0, under the 41.0 aimed at: the answer is below 14.
        assert_eq!(next_crf(&hard, &bands(38.0, 41.0), 7), Some(10.0));
        let easy: Vec<Observation> = START_CRFS.iter().map(|crf| observation(*crf, 60.0 - crf * 0.5)).collect();
        assert_eq!(next_crf(&easy, &bands(38.0, 41.0), 7), Some(32.0));
    }

    #[test]
    fn a_bend_is_measured_even_without_a_target() {
        let mut bent: Vec<Observation> = START_CRFS.iter().map(|crf| observation(*crf, 50.0 - crf * 0.5)).collect();
        bent[1] = observation(21.0, 44.0);
        assert!(next_crf(&bent, &[None; METRICS], 7).is_some());
    }

    #[test]
    fn surprise_is_the_distance_from_the_models_curve() {
        let measured: Vec<Observation> = START_CRFS.iter().map(|crf| observation(*crf, 50.0 - crf * 0.5)).collect();
        let exact = surprise(&measured, &|crf| observation(crf, 50.0 - crf * 0.5).values);
        assert!(exact < 1e-9);
        let off = surprise(&measured, &|crf| observation(crf, 51.0 - crf * 0.5).values);
        // One dB out on three of the seven metrics, each in units of half a dB.
        assert!((off - 3.0 * 2.0 / 7.0).abs() < 1e-6, "{off}");
    }

    fn vector(noise: f64, motion: f64) -> [f64; inputs::INPUTS] {
        let mut vector = [0.0; inputs::INPUTS];
        vector[0] = noise;
        vector[18] = motion;
        vector
    }

    // Twenty near-identical scenes and one unlike them: the odd one is always taken, the rest are
    // sampled evenly, and a scene the dataset already has a twin of is not what novelty picks.
    #[test]
    fn selection_takes_even_steps_and_then_what_is_new() {
        let mut candidates: Vec<[f64; inputs::INPUTS]> = (0..20).map(|index| vector(0.5 + index as f64 * 0.001, 1.0)).collect();
        candidates.push(vector(3.0, 5.0));
        let chosen = select_scenes(&candidates, &vec![0.0; 21], &[], 6);
        assert_eq!(chosen.len(), 6);
        assert!(chosen.iter().any(|(index, why)| *index == 20 && *why == "novel"), "{chosen:?}");
        assert_eq!(chosen.iter().filter(|(_, why)| *why == "spread").count(), 3);
        let chosen = select_scenes(&candidates, &vec![0.0; 21], &[vector(3.0, 5.0)], 4);
        assert!(!chosen.iter().any(|(index, why)| *index == 20 && *why == "novel"), "{chosen:?}");
        // Where two scenes are equally new, the one the model is unsure of goes first.
        let pair = [vector(0.0, 0.0), vector(1.0, 0.0), vector(-1.0, 0.0)];
        let chosen = select_scenes(&pair, &[0.0, 0.0, 3.0], &[], 2);
        assert!(chosen.iter().any(|(index, _)| *index == 2), "{chosen:?}");
        assert!(select_scenes(&pair, &[0.0; 3], &[], 0).is_empty());
        assert_eq!(select_scenes(&pair, &[0.0; 3], &[], 9).len(), 3);
    }

    fn record(start: u64, frames: u64, crf: f32) -> pnx264::scenes::DecisionRecord {
        pnx264::scenes::DecisionRecord {
            start,
            frames,
            anchor: 17.0,
            crf,
            model_crf: crf,
            tempered: None,
            probe: None,
            fast: None,
            factor: "detail".to_string(),
            ease: 0.0,
            votes: Vec::new(),
            cut_pairs_within: 0,
        }
    }

    // Raised shots first, largest raise first, one per shot; then kept shots that look like missed
    // savings; lowered and plain ones are not taken, but for controls; the count is kept to.
    #[test]
    fn verification_takes_raises_then_doubtful_kept_shots() {
        let mut records = vec![
            // One shot split at 48 frames: verified once, at its larger raise.
            record(0, 48, 19.0),
            record(48, 30, 21.0),
            record(78, 40, 18.0),
            record(118, 40, 16.0),
            record(158, 40, 17.0),
            record(198, 40, 17.0),
            record(238, 40, 17.0),
        ];
        records[4].votes = vec![("xpsnr".into(), 2.0, 0.1), ("ssim".into(), 1.5, 0.1), ("cambi".into(), 0.0, 0.0)];
        records[5].ease = 2.5;
        records[6].votes = vec![("xpsnr".into(), 0.2, 3.0)];
        let picks = verify_picks(&records, 48, 10);
        let named: Vec<(u64, &str)> = picks.iter().map(|pick| (pick.start, pick.picked)).collect();
        assert_eq!(named, vec![(48, "verify-raise"), (78, "verify-raise"), (158, "verify-split"), (198, "verify-ease"), (238, "verify-spread")]);
        assert_eq!((picks[0].anchor, picks[0].decided), (17.0, 21.0));
        assert_eq!(verify_picks(&records, 48, 2).len(), 2);
        // Plain kept shots give controls, about one in `VERIFY_CONTROL_EVERY`, and keep a slot.
        let plain: Vec<_> = (0..400).map(|index| record(index * 40, 40, 17.0)).collect();
        let controls = verify_picks(&plain, 48, 1000);
        assert!(controls.iter().all(|pick| pick.picked == "verify-control"));
        assert!((8..=40).contains(&controls.len()), "{}", controls.len());
        let mut mixed = plain.clone();
        mixed.extend((0..30).map(|index| record(20_000 + index * 40, 40, 20.0)));
        let picks = verify_picks(&mixed, 48, 20);
        assert_eq!(picks.len(), 20);
        assert_eq!(picks.iter().filter(|pick| pick.picked == "verify-control").count(), 2);
    }

    #[test]
    fn episodes_of_one_series_share_a_key() {
        let a = series_key("[SubsPlease] Ao Ashi S2 - 07 (1080p) [D6C86AC6].mkv");
        assert_eq!(a, "ao ashi s2");
        assert_eq!(a, series_key("[SubsPlease] Ao Ashi S2 - 08v2 (1080p) [00000000].mkv"));
        assert_eq!(series_key("[Group] Detective.Conan.-.1220.(1080p).mkv"), "detective conan");
        assert_ne!(series_key("[A] Show One - 01.mkv"), series_key("[A] Show Two - 01.mkv"));
        // Nothing to take off: the name stands.
        assert_eq!(series_key("sintel.mkv"), "sintel");
        assert_eq!(series_key("[x] 86 - 03 [y].mkv"), "86");
        // A Blu-ray release numbers its episodes inside the season.
        assert_eq!(series_key("Mushishi S01E03 The Soft Horn (BD 1080p).mkv"), "mushishi s1");
        assert_eq!(series_key("Mushishi.S01E04v2.mkv"), "mushishi s1");
        assert_eq!(series_key("[G] Some Show S2 - 03 (1080p).mkv"), series_key("Some Show S02E11.mkv"));
        assert_ne!(series_key("Some Show S01E01.mkv"), series_key("Some Show S02E01.mkv"));
    }

    #[test]
    fn the_encoder_key_ignores_order_and_zones() {
        let a = VerifyEncoder { preset: Some("fast".into()), tune: None, x264_params: Some("aq-strength=0.8:aq-mode=3".into()) };
        let b = VerifyEncoder { preset: Some("fast".into()), tune: None, x264_params: Some("aq-mode=3:zones=0,9,crf=20:aq-strength=0.8".into()) };
        assert_eq!(encoder_key(&a), "x264|fast|-|aq-mode=3:aq-strength=0.8");
        assert_eq!(encoder_key(&a), encoder_key(&b));
    }

    #[test]
    fn a_sample_reads_back_and_orients_every_metric_downwards() {
        let sample = LabSample {
            record: LAB_RECORD,
            source_id: "s".into(),
            series: "s".into(),
            source: SourceContext { width: 1920, height: 1080, fps: 24.0, video_bitrate: None, codec: None, bit_depth: 10 },
            encoder: "x264|fast|-|".into(),
            start_frame: 48,
            scene_frames: 48,
            frames: 48,
            features: WindowFeatures::default(),
            scene_features: None,
            cut_pairs_within: 0,
            reference_bit_depth: 10,
            cambi_source: 0.2,
            cambi_source_high: 0.5,
            points: vec![
                LabPoint { crf: 14.0, bytes: 400_000, cambi_high: 0.4, ..Default::default() },
                LabPoint { crf: 28.0, bytes: 100_000, cambi_high: 6.5, ..Default::default() },
                LabPoint { crf: 17.0, bytes: 200_000, cambi_high: 0.1, treatment: DEBAND.name.into(), ..Default::default() },
            ],
            fast_ladder: vec![FastPoint { crf: 23.0, encode: FastEncode { xpsnr: 41.5, xpsnr_low: 39.0, log_bpp: -4.5, frames: 48, banding: Some(0.75) } }],
            picked: "spread".into(),
            verified: None,
            model: String::new(),
            surprise: None,
            collected_unix: 0,
            elapsed_ms: 0,
        };
        let line = serde_json::to_string(&sample).unwrap();
        let back: LabSample = serde_json::from_str(&line).unwrap();
        assert_eq!(back.points, sample.points);
        // A fast encode on the ladder is written flat beside its CRF, and a line written before the
        // ladder reads as one without it.
        assert_eq!(back.fast_ladder, sample.fast_ladder);
        assert!(line.contains(r#""fast_ladder":[{"crf":23.0,"xpsnr":41.5,"#), "{line}");
        let before: LabSample = serde_json::from_str(&line.replace(r#""fast_ladder":"#, r#""unread_ladder":"#)).unwrap();
        assert!(before.fast_ladder.is_empty());
        let observed = back.observations();
        // A treated encode is not a point on the scene's curves, and a plain one names no treatment.
        assert_eq!(observed.len(), 2);
        assert_eq!(line.matches("treatment").count(), 1);
        // Less banding than the source is no banding added; six more than it is minus six.
        assert_eq!((observed[0].values[5], observed[1].values[5]), (0.0, -6.0));
        // A quarter of the bytes is two fewer bits per pixel, in log2.
        assert!((observed[0].values[6] - observed[1].values[6] - 2.0).abs() < 1e-9);
        assert!(back.deep() && back.inputs()[26] == 1.0);
        // The deband encode is no point on the line: banding 0.4 at 14 to 6.5 at 28 is 3.45 at 21,
        // less the source's 0.5.
        assert!((back.lab_banding(21.0).unwrap() - 2.95).abs() < 1e-9);
        assert_eq!((back.lab_banding(10.0), back.lab_banding(40.0)), (Some(0.0), Some(6.0)));
        // Without a fast encode there is no banding input; one from before banding was read is
        // given the lab's, and one that read its own keeps it.
        assert_eq!(back.inputs()[30], 0.0);
        let fast = caramel_kernel::FastEncode { xpsnr: 42.0, xpsnr_low: 40.0, log_bpp: -3.0, frames: 48, banding: None };
        let old = LabSample { features: WindowFeatures { fast_encode: Some(fast), ..Default::default() }, fast_ladder: Vec::new(), ..back.clone() };
        let expected = old.lab_banding(caramel_kernel::FAST_CRF as f64).unwrap();
        assert!((old.inputs()[30] - ((expected + 0.05) * STAND_IN_BANDING).ln()).abs() < 1e-12);
        let line = serde_json::to_string(&old).unwrap();
        assert!(!line.contains("banding"), "a sample from before reads as it did");
        let measured = LabSample { features: WindowFeatures { fast_encode: Some(caramel_kernel::FastEncode { banding: Some(1.0), ..fast }), ..Default::default() }, ..back.clone() };
        assert!((measured.inputs()[30] - 1.05f64.ln()).abs() < 1e-12);
        // The banding the source is ready to show stands in from the fast encode's, or the lab's.
        assert_eq!((measured.inputs()[31], old.inputs()[31]), (measured.inputs()[30], old.inputs()[30]));
        let read = LabSample { features: WindowFeatures { exposed_banding: Some(3.0), ..measured.features.clone() }, ..back };
        assert!((read.inputs()[31] - 3.05f64.ln()).abs() < 1e-12 && read.inputs()[30] == measured.inputs()[30]);
    }
}
