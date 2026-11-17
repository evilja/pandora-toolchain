// Training a curve model from measured scenes, and saying how good the result is.
//
// A training scene is what the lab measured: the scene's inputs, and what each metric read at each
// CRF it was encoded at. A network is fitted to the measured points directly — its curve for a
// metric should pass through that metric's readings — so a scene measured at three CRFs teaches as
// surely as one measured at eight, and no scene has to be reduced to one number first.
//
// The sources are split into folds, whole series at a time: episodes of one series share their
// look, and a network that has seen one would be flattered by another. Each network trains on every
// fold but one and stops when its error on that one stops falling. That fold is then the network's
// test: every scene has networks that never trained on its series, and what those say about it is
// what the model would say about a source it has never seen. The model's stated error, its gain,
// and the comparison a retrained model has to win are all taken from those held-out answers.

use crate::curve::{
    BANDING, BITS, CEILINGS, Curve, CurveVote, Curves, DECIDES, Decision, KNOT_COUNT, KNOTS, MAX_LOWER, MAX_RAISE, METRICS, Model, Observation, SCALES, curves_from, decide,
    line_at, measured_line, measured_votes, measured_votes_all, quality_ease, value_and_slopes, votes_of,
};
use crate::inputs::INPUTS;
use crate::nn::{Adam, Gradients, Network, Rng, Trace};

pub const FOLDS: usize = 5;
// The anchors a model is judged at: the CRFs presets are set to.
pub const ANCHORS: [f64; 4] = [17.0, 19.0, 21.0, 23.0];
// Beyond this many units a reading is an outlier, and pulls a curve no harder for being further.
const HUBER: f64 = 2.0;
// A detailed scene, the one the anchor is the right CRF for: the scene this share of the measured
// ones is harder than, by the quality its encodes held. A quarter of all scenes is the detail a
// release is judged on — fine linework, texture, grain — and what is easier than that has quality
// to spare at the anchor.
pub const DETAILED_SHARE: f64 = 0.25;
// The gains a trainer may choose between, the largest first, and the one a model with no held-out
// answers gets.
const GAINS: [f64; 4] = [1.0, 0.75, 0.5, 0.25];
pub const DEFAULT_GAIN: f64 = 0.5;
// What a gain has to hold on held-out sources: of the scenes it raises, the 90th and 95th
// percentiles of how far their quality fell below a detailed scene's at the anchor (the mean over
// the quality metrics, in units: half a dB of XPSNR is one), and the 95th percentile of the CAMBI
// the raise added. A raised scene is allowed to look like a detailed one does, a little worse at
// its tail, and not to band visibly.
pub const SHORTFALL_P90: f64 = 2.5;
pub const SHORTFALL_P95: f64 = 3.5;
pub const BANDING_P95: f64 = 2.0;
// The step of a decided CRF: the one `model` applies.
pub const CRF_STEP: f64 = 0.5;

#[derive(Clone, Debug)]
pub struct Sample {
    // The series the scene's source belongs to: what folds are made of.
    pub series: String,
    pub inputs: [f64; INPUTS],
    pub observations: Vec<Observation>,
    // Frames the scene was measured over: its weight where bytes are added up.
    pub frames: f64,
    pub collected: u64,
}

#[derive(Clone, Debug)]
pub struct TrainConfig {
    pub version: String,
    pub encoder: String,
    pub hidden: Vec<usize>,
    // Networks per fold. With more than one, a scene's held-out answer has a disagreement of its
    // own, as the whole model's has.
    pub per_fold: usize,
    pub epochs: usize,
    // Epochs without a better held-out error before a network stops.
    pub patience: usize,
    pub rate: f64,
    pub decay: f64,
    pub batch: usize,
    pub seed: u64,
    // How many of the inputs the networks read, from the first: `inputs::BASE_INPUTS`, or `INPUTS`
    // with the scene's fast encode, which every sample trained on must then have.
    pub inputs: usize,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self { version: String::new(), encoder: String::new(), hidden: vec![32, 32], per_fold: 2, epochs: 400, patience: 40, rate: 2e-3, decay: 1e-4, batch: 32, seed: 1, inputs: crate::inputs::BASE_INPUTS }
    }
}

pub struct Trained {
    pub model: Model,
    // For every sample, the curves of the networks that did not train on its series. Empty when
    // there were too few series to hold any out.
    pub held_out: Vec<Vec<Curves>>,
    // How long each network trained, and the held-out loss it stopped at.
    pub epochs: Vec<usize>,
    pub losses: Vec<f64>,
}

fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

// The fold of every sample. Series are dealt out in the order of their names' hashes, so folds hold
// the same number of series whatever the names are, and a series is never split.
pub fn folds(samples: &[Sample]) -> Vec<usize> {
    let mut series: Vec<&str> = samples.iter().map(|sample| sample.series.as_str()).collect();
    series.sort_by_key(|name| (hash(name), *name));
    series.dedup();
    samples.iter().map(|sample| series.iter().position(|name| *name == sample.series).unwrap_or(0) % FOLDS).collect()
}

pub fn series_count(samples: &[Sample]) -> usize {
    let mut series: Vec<&str> = samples.iter().map(|sample| sample.series.as_str()).collect();
    series.sort();
    series.dedup();
    series.len()
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 { values[middle] } else { 0.5 * (values[middle - 1] + values[middle]) }
}

fn percentile(values: &mut [f64], share: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    values[((values.len() - 1) as f64 * share).round() as usize]
}

// What typical content scores at every knot: for each series the median of its scenes, and then the
// median of the series, so a source that was measured more than the others counts no more than
// they do. A median of falling lines falls.
pub fn typical_curves(samples: &[Sample]) -> Curves {
    let mut names: Vec<&str> = samples.iter().map(|sample| sample.series.as_str()).collect();
    names.sort();
    names.dedup();
    std::array::from_fn(|metric| {
        let lines: Vec<(&str, Vec<(f64, f64)>)> =
            samples.iter().map(|sample| (sample.series.as_str(), measured_line(&sample.observations, metric))).filter(|(_, line)| line.len() >= 2).collect();
        Curve(std::array::from_fn(|knot| {
            let mut per_series: Vec<f64> = names
                .iter()
                .filter_map(|name| {
                    let mut values: Vec<f64> = lines.iter().filter(|(series, _)| series == name).map(|(_, line)| line_at(line, KNOTS[knot]).min(CEILINGS[metric])).collect();
                    (!values.is_empty()).then(|| median(&mut values))
                })
                .collect();
            median(&mut per_series)
        }))
    })
}

// A sample as a network is shown it.
struct Prepared {
    inputs: Vec<f64>,
    points: Vec<Observation>,
    // How many readings it has: each scene weighs the same however many CRFs it was measured at.
    terms: f64,
}

// One scene's loss — the mean over its readings of how far the network's curve is from each, in the
// metric's units — and, when `gradients` is given, its slope added there.
fn scene_loss(network: &Network, scene: &Prepared, offsets: &[f64; METRICS], trace: &mut Trace, slope: &mut Vec<f64>, gradients: Option<&mut Gradients>) -> f64 {
    network.forward_trace(&scene.inputs, trace);
    slope.clear();
    slope.resize(METRICS * KNOT_COUNT, 0.0);
    let mut total = 0.0;
    for point in &scene.points {
        for metric in 0..METRICS {
            let reading = point.values[metric];
            if reading.is_nan() {
                continue;
            }
            let raw = &trace.output[metric * KNOT_COUNT..(metric + 1) * KNOT_COUNT];
            let (value, slopes) = value_and_slopes(raw, offsets[metric], SCALES[metric], point.crf);
            let error = (value - reading) / SCALES[metric];
            let (cost, pull) = if error.abs() <= HUBER { (0.5 * error * error, error) } else { (HUBER * (error.abs() - 0.5 * HUBER), HUBER * error.signum()) };
            total += cost;
            for knot in 0..KNOT_COUNT {
                slope[metric * KNOT_COUNT + knot] += pull * slopes[knot] / scene.terms;
            }
        }
    }
    if let Some(gradients) = gradients {
        network.backward(trace, slope, gradients);
    }
    total / scene.terms
}

// One network: trained on `train`, stopped on `stop`. Without a `stop` set it runs a quarter of the
// epochs, which is about where the others stop.
fn train_member(scenes: &[Prepared], train: &[usize], stop: &[usize], offsets: &[f64; METRICS], config: &TrainConfig, seed: u64) -> (Network, usize, f64) {
    let mut rng = Rng::new(seed);
    let mut sizes = vec![config.inputs.clamp(1, INPUTS)];
    sizes.extend(config.hidden.iter().map(|width| (*width).clamp(1, 512)));
    sizes.push(METRICS * KNOT_COUNT);
    let mut network = Network::new(&sizes, &mut rng);
    // An untrained network draws curves that fall about as fast as real ones do — three units a
    // knot — so training starts from a typical scene and not from a flat line.
    if let Some(last) = network.layers.last_mut() {
        for (index, bias) in last.bias.iter_mut().enumerate() {
            if index % KNOT_COUNT != KNOT_COUNT / 2 {
                *bias = 3.0;
            }
        }
    }
    let mut adam = Adam::new(&network, config.rate, config.decay);
    let mut gradients = network.gradients();
    let mut trace = Trace::default();
    let mut slope = Vec::new();
    let mut order: Vec<usize> = train.to_vec();
    let mut best = (network.clone(), 0usize, f64::INFINITY);
    let epochs = if stop.is_empty() { (config.epochs / 4).max(1) } else { config.epochs.max(1) };
    for epoch in 1..=epochs {
        rng.shuffle(&mut order);
        for batch in order.chunks(config.batch.max(1)) {
            gradients.clear();
            for index in batch {
                scene_loss(&network, &scenes[*index], offsets, &mut trace, &mut slope, Some(&mut gradients));
            }
            adam.step(&mut network, &gradients, 1.0 / batch.len() as f64);
        }
        if stop.is_empty() {
            best = (network.clone(), epoch, f64::NAN);
            continue;
        }
        let loss = stop.iter().map(|index| scene_loss(&network, &scenes[*index], offsets, &mut trace, &mut slope, None)).sum::<f64>() / stop.len() as f64;
        if loss < best.2 {
            best = (network.clone(), epoch, loss);
        } else if epoch - best.1 >= config.patience.max(1) {
            break;
        }
    }
    best
}

// Trains a model. `typical` is the quality votes are measured against; a retrain passes the
// installed model's, so what a scene is asked to hold does not drift with what was measured last,
// and a first training passes None to take it from the samples.
pub fn train(samples: &[Sample], config: &TrainConfig, typical: Option<Curves>) -> Result<Trained, String> {
    let usable: Vec<usize> = (0..samples.len()).filter(|index| samples[*index].observations.len() >= 2).collect();
    if usable.len() < 8 {
        return Err(format!("{} scenes with two or more encodes are too few to train on", usable.len()));
    }
    let count = usable.len() as f64;
    let reads = config.inputs.clamp(1, INPUTS);
    let mut input_mean = vec![0.0; reads];
    let mut input_scale = vec![0.0; reads];
    for input in 0..reads {
        let mean = usable.iter().map(|index| samples[*index].inputs[input]).sum::<f64>() / count;
        let deviation = (usable.iter().map(|index| (samples[*index].inputs[input] - mean).powi(2)).sum::<f64>() / count).sqrt();
        input_mean[input] = mean;
        // An input that never varied taught nothing, and is not listened to.
        input_scale[input] = if deviation > 1e-9 { 1.0 / deviation } else { 0.0 };
    }
    let held = |metric: usize, value: f64| if value.is_nan() { f64::NAN } else { value.min(CEILINGS[metric]) };
    let offsets: [f64; METRICS] = std::array::from_fn(|metric| {
        let mut readings: Vec<f64> = usable.iter().flat_map(|index| samples[*index].observations.iter().map(|point| held(metric, point.values[metric]))).filter(|value| !value.is_nan()).collect();
        median(&mut readings)
    });
    let mut model = Model {
        version: config.version.clone(),
        encoder: config.encoder.clone(),
        detail: 0.0,
        gain: DEFAULT_GAIN,
        scenes: usable.len(),
        sources: series_count(samples),
        trained: samples.iter().map(|sample| sample.collected).max().unwrap_or(0),
        input_mean,
        input_scale,
        offsets,
        typical: typical.unwrap_or_else(|| typical_curves(samples)),
        error: [0.0; METRICS],
        members: Vec::new(),
    };
    model.detail = detailed_ease(samples, &model.typical);
    let scenes: Vec<Prepared> = samples
        .iter()
        .map(|sample| {
            let points: Vec<Observation> =
                sample.observations.iter().map(|point| Observation { crf: point.crf, values: std::array::from_fn(|metric| held(metric, point.values[metric])) }).collect();
            let terms = points.iter().map(|point| point.values.iter().filter(|value| !value.is_nan()).count()).sum::<usize>().max(1) as f64;
            Prepared { inputs: model.standardised(&sample.inputs), points, terms }
        })
        .collect();
    let fold_of = folds(samples);
    let split = series_count(samples) >= 2;
    let per_fold = config.per_fold.clamp(1, 8);
    let plans: Vec<(usize, u64)> = (0..FOLDS * per_fold).map(|member| (member % FOLDS, config.seed.wrapping_mul(1000).wrapping_add(member as u64))).collect();
    let results: Vec<(Network, usize, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = plans
            .iter()
            .map(|(fold, seed)| {
                let (scenes, usable, fold_of, offsets) = (&scenes, &usable, &fold_of, &offsets);
                scope.spawn(move || {
                    let stop: Vec<usize> = if split { usable.iter().copied().filter(|index| fold_of[*index] == *fold).collect() } else { Vec::new() };
                    let train: Vec<usize> = usable.iter().copied().filter(|index| stop.is_empty() || fold_of[*index] != *fold).collect();
                    train_member(scenes, &train, &stop, offsets, config, *seed)
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("a training thread panicked")).collect()
    });
    let mut epochs = Vec::new();
    let mut losses = Vec::new();
    for (network, epoch, loss) in results {
        model.members.push(network);
        epochs.push(epoch);
        losses.push(loss);
    }
    let held_out: Vec<Vec<Curves>> = (0..samples.len())
        .map(|index| {
            if !split {
                return Vec::new();
            }
            let standardised = model.standardised(&samples[index].inputs);
            plans.iter().zip(&model.members).filter(|((fold, _), _)| *fold == fold_of[index]).map(|(_, member)| curves_from(&member.forward(&standardised), &model.offsets)).collect()
        })
        .collect();
    if split {
        model.error = vote_error(samples, &held_out, &model.typical).0;
        model.gain = choose_gain(samples, &held_out, &model.typical, model.detail);
    }
    Ok(Trained { model, held_out, epochs, losses })
}

// How far the votes read from `curves` are from what the scenes' encodes allowed, in CRF, per
// metric, over every anchor — and how many votes that is the mean of. A sample with no curves is
// skipped.
pub fn vote_error(samples: &[Sample], curves: &[Vec<Curves>], typical: &Curves) -> ([f64; METRICS], usize) {
    let mut sums = [0.0; METRICS];
    let mut counts = [0usize; METRICS];
    for (sample, members) in samples.iter().zip(curves).filter(|(_, members)| !members.is_empty()) {
        for anchor in ANCHORS {
            let said = votes_of(members, typical, anchor);
            let allowed = measured_votes(&sample.observations, typical, anchor);
            for metric in 0..METRICS {
                if let Some(allowed) = allowed[metric] {
                    sums[metric] += (said[metric].offset - allowed).abs();
                    counts[metric] += 1;
                }
            }
        }
    }
    (std::array::from_fn(|metric| if counts[metric] == 0 { 0.0 } else { sums[metric] / counts[metric] as f64 }), counts.iter().copied().max().unwrap_or(0))
}

// The mean of a per-metric error over the metrics a scene is decided on: one number for how well a
// model's votes can be trusted.
pub fn deciding_error(error: &[f64; METRICS]) -> f64 {
    let judged: Vec<f64> = (0..METRICS).filter(|metric| DECIDES[*metric]).map(|metric| error[metric]).collect();
    judged.iter().sum::<f64>() / judged.len().max(1) as f64
}

// How easy a detailed scene is: the quality ease, by the scenes' own encodes, that
// `DETAILED_SHARE` of the measured scenes fall below, over every anchor.
pub fn detailed_ease(samples: &[Sample], typical: &Curves) -> f64 {
    let mut eases: Vec<f64> = samples
        .iter()
        .flat_map(|sample| ANCHORS.iter().filter_map(|anchor| measured_votes_all(&measured_votes(&sample.observations, typical, *anchor))).map(|votes| quality_ease(&votes)))
        .collect();
    if eases.is_empty() {
        return 0.0;
    }
    percentile(&mut eases, DETAILED_SHARE)
}

// A decision as it is applied: within the bounds, on the step.
pub fn applied(decision: &Decision) -> f64 {
    (decision.offset.clamp(-MAX_LOWER, MAX_RAISE) / CRF_STEP).round() * CRF_STEP
}

// What a way of deciding scenes would have done to scenes whose encodes were measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Policy {
    pub decisions: usize,
    pub raised: usize,
    pub lowered: usize,
    // How far the raised scenes were raised, on average, and how many by four CRF or more.
    pub raise_mean: f64,
    pub raised_far: usize,
    // Bytes of the encodes it chose over bytes at the anchor.
    pub bytes: f64,
    // Of the raised scenes: how far their quality fell below a detailed scene's at the anchor, as
    // the mean over the quality metrics in units, at the 90th and 95th percentiles.
    pub shortfall_p90: f64,
    pub shortfall_p95: f64,
    // CAMBI the raise added to the raised scenes, 95th percentile.
    pub banding_p95: f64,
    // Of the lowered scenes: the share that were harder than a detailed scene by their encodes, as
    // a lowering claims.
    pub needed: f64,
}

impl Policy {
    // Whether raises this good may be made on sources the model has never seen.
    pub fn holds(&self) -> bool {
        self.raised == 0 || (self.shortfall_p90 <= SHORTFALL_P90 && self.shortfall_p95 <= SHORTFALL_P95 && self.banding_p95 <= BANDING_P95)
    }
}

// `decisions` are (sample, anchor, offset from the anchor). `detail` is the ease of the detailed
// scene raised scenes are held to.
pub fn simulate(samples: &[Sample], typical: &Curves, detail: f64, decisions: &[(usize, f64, f64)]) -> Policy {
    let mut policy = Policy { decisions: decisions.len(), ..Default::default() };
    let (mut chosen, mut anchored) = (0.0, 0.0);
    let mut needed = 0usize;
    let mut raises = Vec::new();
    let mut shortfalls = Vec::new();
    let mut banding = Vec::new();
    let quality = |metric: &usize| DECIDES[*metric] && *metric != BANDING;
    for (index, anchor, offset) in decisions {
        let sample = &samples[*index];
        let lines: [Vec<(f64, f64)>; METRICS] = std::array::from_fn(|metric| measured_line(&sample.observations, metric));
        if lines[BITS].len() >= 2 {
            chosen += sample.frames * line_at(&lines[BITS], anchor + offset).exp2();
            anchored += sample.frames * line_at(&lines[BITS], *anchor).exp2();
        }
        if *offset > 0.0 {
            raises.push(*offset);
            // A detailed scene at the anchor scores what typical content scores `detail` CRF the
            // other way: a scene that holds typical quality `detail` CRF from the anchor.
            let measured: Vec<f64> =
                (0..METRICS).filter(quality).filter(|metric| lines[*metric].len() >= 2).map(|metric| (typical[metric].at(anchor - detail) - line_at(&lines[metric], anchor + offset)) / SCALES[metric]).collect();
            if !measured.is_empty() {
                shortfalls.push((measured.iter().sum::<f64>() / measured.len() as f64).max(0.0));
            }
            if lines[BANDING].len() >= 2 {
                banding.push((line_at(&lines[BANDING], *anchor) - line_at(&lines[BANDING], anchor + offset)).max(0.0));
            }
        } else if *offset < 0.0 {
            policy.lowered += 1;
            let allowed = measured_votes(&sample.observations, typical, *anchor);
            needed += usize::from(measured_votes_all(&allowed).is_some_and(|votes| quality_ease(&votes) < detail));
        }
    }
    policy.bytes = if anchored > 0.0 { chosen / anchored } else { 1.0 };
    policy.raised = raises.len();
    if !raises.is_empty() {
        policy.raise_mean = raises.iter().sum::<f64>() / raises.len() as f64;
        policy.raised_far = raises.iter().filter(|raise| **raise >= 4.0).count();
        policy.shortfall_p90 = percentile(&mut shortfalls, 0.90);
        policy.shortfall_p95 = percentile(&mut shortfalls, 0.95);
        policy.banding_p95 = percentile(&mut banding, 0.95);
    }
    if policy.lowered > 0 {
        policy.needed = needed as f64 / policy.lowered as f64;
    }
    policy
}

// What votes read from `curves` would decide for every sample at every anchor.
pub fn decisions(samples: &[Sample], curves: &[Vec<Curves>], typical: &Curves, detail: f64, gain: f64) -> Vec<(usize, f64, f64)> {
    let mut out = Vec::new();
    for (index, members) in curves.iter().enumerate().filter(|(index, members)| !members.is_empty() && samples[*index].observations.len() >= 2) {
        for anchor in ANCHORS {
            let votes: [CurveVote; METRICS] = votes_of(members, typical, anchor);
            out.push((index, anchor, applied(&decide(&votes, detail, gain))));
        }
    }
    out
}

// The largest gain whose raises, on scenes the networks had not trained on, stay within what a
// raised scene may lose. The smallest when none does: the trainer's verdict then says so.
pub fn choose_gain(samples: &[Sample], curves: &[Vec<Curves>], typical: &Curves, detail: f64) -> f64 {
    for gain in GAINS {
        if simulate(samples, typical, detail, &decisions(samples, curves, typical, detail, gain)).holds() {
            return gain;
        }
    }
    GAINS[GAINS.len() - 1]
}

// Whether a retrained model may replace the one in use, judged on scenes the one in use has never
// seen: the ones measured since it was trained. The candidate answers for each with the networks
// that did not train on its series, the installed model with all of its networks. Better votes on
// average are not enough: a model whose votes are closer on most scenes can still be further out on
// the few it raises furthest, and a raise that overshoots is what a viewer sees. So both models
// decide the fresh scenes as they would in use, and the candidate's raises may not fall further
// below a detailed scene, at the 95th percentile, than the current model's do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    // Scenes both were judged on.
    pub scenes: usize,
    // Mean vote error over the deciding metrics, in CRF.
    pub current: f64,
    pub candidate: f64,
    // How far each model's raises on those scenes fell below a detailed scene, in units, at the
    // 95th percentile.
    pub current_shortfall: f64,
    pub candidate_shortfall: f64,
    // Bytes each one's choices would have spent on those scenes over bytes at the anchor.
    pub current_bytes: f64,
    pub candidate_bytes: f64,
    pub accepted: bool,
}

// Fewer fresh scenes than this say too little to replace a model on.
pub const GATE_SCENES: usize = 40;
// A candidate that is this much worse is still within what a different draw of scenes would do.
pub const GATE_TOLERANCE: f64 = 0.05;
// The same for how far its raises fall short, in units.
const SHORTFALL_TOLERANCE: f64 = 0.25;

pub fn judge(current: &Model, candidate: &Trained, samples: &[Sample]) -> Verdict {
    let fresh: Vec<usize> = (0..samples.len()).filter(|index| samples[*index].collected > current.trained && !candidate.held_out[*index].is_empty() && samples[*index].observations.len() >= 2).collect();
    let subset: Vec<Sample> = fresh.iter().map(|index| samples[*index].clone()).collect();
    let theirs: Vec<Vec<Curves>> = subset.iter().map(|sample| current.member_curves(&sample.inputs)).collect();
    let ours: Vec<Vec<Curves>> = fresh.iter().map(|index| candidate.held_out[*index].clone()).collect();
    // Both are read against the candidate's targets — the installed model's own, unless whoever
    // trained it asked for new ones.
    let typical = &candidate.model.typical;
    let current_error = deciding_error(&vote_error(&subset, &theirs, typical).0);
    let candidate_error = deciding_error(&vote_error(&subset, &ours, typical).0);
    // Each decides with its own targets, detailed scene and gain, as it would in use; both are held
    // to the candidate's detailed scene.
    let detail = candidate.model.detail;
    let current_policy = simulate(&subset, typical, detail, &decisions(&subset, &theirs, &current.typical, current.detail, current.gain));
    let candidate_policy = simulate(&subset, typical, detail, &decisions(&subset, &ours, typical, detail, candidate.model.gain));
    Verdict {
        scenes: subset.len(),
        current: current_error,
        candidate: candidate_error,
        current_shortfall: current_policy.shortfall_p95,
        candidate_shortfall: candidate_policy.shortfall_p95,
        current_bytes: current_policy.bytes,
        candidate_bytes: candidate_policy.bytes,
        accepted: subset.len() >= GATE_SCENES
            && candidate_error <= current_error + GATE_TOLERANCE
            && candidate_policy.shortfall_p95 <= current_policy.shortfall_p95 + SHORTFALL_TOLERANCE,
    }
}

// A candidate whose votes are better but whose raises overshoot more may still be the better model
// at a smaller gain: its raises shrink, and so do its overshoots. It is judged at each smaller gain
// in turn, and kept at the first one it passes at — but only if, there, it still saves at least as
// many bytes on the fresh scenes as the model in use. A candidate that only passes by saving less
// is no better a trade than turning the installed model's gain down. Its gain is left as it was when
// no smaller one will do; either way the verdict at the gain it ends with is returned.
pub fn settle_gain(current: &Model, candidate: &mut Trained, samples: &[Sample]) -> Verdict {
    let verdict = judge(current, candidate, samples);
    if verdict.accepted || verdict.candidate > verdict.current + GATE_TOLERANCE {
        return verdict;
    }
    let chosen = candidate.model.gain;
    for gain in GAINS.into_iter().filter(|gain| *gain < chosen) {
        candidate.model.gain = gain;
        let smaller = judge(current, candidate, samples);
        if smaller.accepted && smaller.candidate_bytes <= smaller.current_bytes {
            return smaller;
        }
    }
    candidate.model.gain = chosen;
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenes whose curves depend on two of their inputs: input 0 lifts every curve, input 3 makes
    // it fall faster. Eight series of twelve scenes, measured at five CRFs with a little noise.
    fn synthetic(seed: u64, series: usize) -> Vec<Sample> {
        let mut rng = Rng::new(seed);
        let mut samples = Vec::new();
        for source in 0..series {
            for _ in 0..12 {
                let mut inputs = [0.0; INPUTS];
                for input in inputs.iter_mut().take(6) {
                    *input = rng.gaussian();
                }
                let (lift, steep) = (inputs[0], 1.0 + 0.3 * inputs[3].clamp(-2.0, 2.0));
                let observations = [12.0, 16.0, 20.0, 24.0, 28.0]
                    .into_iter()
                    .map(|crf: f64| Observation {
                        crf,
                        values: std::array::from_fn(|metric| {
                            let base = [42.0, 40.0, 22.0, 95.0, 93.0, -3.0, -2.0][metric];
                            let value = base + SCALES[metric] * (2.0 * lift - steep * (crf - 20.0) + 0.1 * rng.gaussian());
                            value.min(CEILINGS[metric])
                        }),
                    })
                    .collect();
                samples.push(Sample { series: format!("series-{source}"), inputs, observations, frames: 48.0, collected: 1000 + source as u64 });
            }
        }
        samples
    }

    fn quick() -> TrainConfig {
        TrainConfig { version: "test".to_string(), encoder: "x264".to_string(), hidden: vec![12], per_fold: 1, epochs: 300, patience: 40, batch: 16, rate: 4e-3, ..Default::default() }
    }

    #[test]
    fn a_series_is_never_split_and_folds_are_even() {
        let samples = synthetic(1, 10);
        let fold_of = folds(&samples);
        for (sample, fold) in samples.iter().zip(&fold_of) {
            let first = samples.iter().position(|other| other.series == sample.series).unwrap();
            assert_eq!(*fold, fold_of[first]);
        }
        for fold in 0..FOLDS {
            assert_eq!(fold_of.iter().filter(|other| **other == fold).count(), 24);
        }
        assert_eq!(series_count(&samples), 10);
    }

    #[test]
    fn typical_content_is_the_median_series_not_the_most_measured() {
        let observation = |crf: f64, value: f64| Observation { crf, values: [value.min(0.0); METRICS] };
        let scene = |series: &str, level: f64| Sample {
            series: series.to_string(),
            inputs: [0.0; INPUTS],
            observations: vec![observation(10.0, level), observation(34.0, level - 24.0)],
            frames: 48.0,
            collected: 0,
        };
        // One series measured nine times at -30, two measured once at -10 and -12.
        let mut samples: Vec<Sample> = (0..9).map(|_| scene("much", -30.0)).collect();
        samples.push(scene("a", -10.0));
        samples.push(scene("b", -12.0));
        let typical = typical_curves(&samples);
        assert_eq!(typical[0].0[0], -12.0);
        assert_eq!(typical[0].at(22.0), -24.0);
        assert!(typical.iter().all(|curve| curve.0.windows(2).all(|pair| pair[0] >= pair[1])));
    }

    #[test]
    fn training_learns_what_moves_a_curve_and_says_how_well() {
        let samples = synthetic(3, 8);
        let trained = train(&samples, &quick(), None).unwrap();
        let model = &trained.model;
        assert_eq!((model.members.len(), model.scenes, model.sources, model.trained), (FOLDS, 96, 8, 1007));
        assert!(trained.held_out.iter().all(|members| members.len() == 1));
        // A scene with every input at zero reads 42 at CRF 20 and loses half a dB per CRF: what it
        // votes is how far that is from typical content, which here is the median of few scenes.
        // One two units up holds the target two CRF further.
        let mut lifted = [0.0; INPUTS];
        lifted[0] = 1.0;
        let votes = model.votes(&lifted, 20.0);
        let flat = model.votes(&[0.0; INPUTS], 20.0);
        let truth = (42.0 - model.typical[0].at(20.0)) / 0.5;
        assert!((flat[0].offset - truth).abs() < 0.3, "{flat:?} {truth}");
        assert!((votes[0].offset - truth - 2.0).abs() < 0.3 && votes[0].spread < 0.3, "{votes:?} {truth}");
        // Networks that never saw a series are right about it to within half a CRF.
        assert!(deciding_error(&model.error) < 0.5, "{:?}", model.error);
        // The curves are right at CRFs between the measured ones too.
        let curves = model.curves(&lifted);
        assert!((curves[0].at(22.0) - (42.0 + 0.5 * (2.0 - 2.0))).abs() < 0.3, "{curves:?}");
        assert!((curves[BITS].at(18.0) - (-2.0 + 0.2 * (2.0 + 2.0))).abs() < 0.15, "{curves:?}");
        // Training is reproducible.
        assert_eq!(train(&samples, &quick(), None).unwrap().model.to_text(), model.to_text());
    }

    #[test]
    fn too_few_scenes_or_series_are_handled() {
        assert!(train(&synthetic(1, 1)[..6], &quick(), None).is_err());
        // One series: nothing can be held out, so nothing is claimed about unseen sources.
        let trained = train(&synthetic(1, 1), &quick(), None).unwrap();
        assert!(trained.held_out.iter().all(Vec::is_empty));
        assert_eq!((trained.model.gain, trained.model.error), (DEFAULT_GAIN, [0.0; METRICS]));
        assert_eq!(trained.model.members.len(), FOLDS);
    }

    #[test]
    fn a_policy_is_scored_against_a_detailed_scene() {
        // Banding is added at a CAMBI a CRF past CRF `bands`; the rest read `level - crf`.
        let observation = |crf: f64, level: f64, bands: f64| Observation {
            crf,
            values: std::array::from_fn(|metric| match metric {
                BITS => -crf / 6.0,
                BANDING => (bands - crf).min(0.0),
                _ => (level - crf).min(CEILINGS[metric]),
            }),
        };
        let scene = |level: f64, bands: f64| Sample { series: "s".to_string(), inputs: [0.0; INPUTS], observations: vec![observation(10.0, level, bands), observation(21.0, level, bands), observation(34.0, level, bands)], frames: 10.0, collected: 0 };
        // Typical content reads -crf; the first scene reads three better at every CRF and the
        // second six worse. A detailed scene is two CRF harder than typical. Neither bands; the
        // third is the first, banding from CRF 21 on.
        let typical: Curves = [Curve(std::array::from_fn(|knot| -KNOTS[knot])); METRICS];
        let samples = vec![scene(3.0, 40.0), scene(-6.0, 40.0), scene(3.0, 21.0)];
        // Raising the easy one five CRF brings it to a detailed scene's quality, and the hard one
        // was harder than a detailed scene, as its lowering claimed.
        let fair = simulate(&samples, &typical, -2.0, &[(0, 20.0, 5.0), (1, 20.0, -0.5)]);
        assert_eq!((fair.raised, fair.lowered, fair.raised_far, fair.raise_mean, fair.shortfall_p95, fair.needed), (1, 1, 1, 5.0, 0.0, 1.0));
        assert!((fair.bytes - (2f64.powf(-25.0 / 6.0) + 2f64.powf(-19.5 / 6.0)) / (2.0 * 2f64.powf(-20.0 / 6.0))).abs() < 1e-9, "{fair:?}");
        assert!(fair.holds());
        // Raising it seven loses two of each metric's value: four units on the half-dB metrics and
        // four thirds on VMAF's worst frames, 10/3 in the mean. Lowering the easy one was not needed.
        let far = simulate(&samples, &typical, -2.0, &[(0, 20.0, 7.0), (0, 20.0, -0.5)]);
        assert!((far.shortfall_p95 - 10.0 / 3.0).abs() < 1e-9 && far.banding_p95 == 0.0, "{far:?}");
        assert_eq!(far.needed, 0.0);
        assert!(!far.holds());
        // The same raise of the banding one adds four CAMBI: too many, whatever its quality held.
        let banded = simulate(&samples, &typical, -2.0, &[(2, 20.0, 5.0)]);
        assert!(banded.shortfall_p95 == 0.0 && (banded.banding_p95 - 4.0).abs() < 1e-9, "{banded:?}");
        assert!(!banded.holds());
    }

    #[test]
    fn a_detailed_scene_is_the_quarter_hardest() {
        let samples = synthetic(5, 8);
        let trained = train(&samples, &quick(), None).unwrap();
        let detail = trained.model.detail;
        // The synthetic scenes' ease is two CRF a deviation of input 0, so the quarter hardest are
        // about 1.35 CRF below the middle one.
        assert!(detail < -0.5 && detail > -2.5, "{detail}");
        let eases: Vec<f64> = samples
            .iter()
            .flat_map(|sample| ANCHORS.iter().filter_map(|anchor| measured_votes_all(&measured_votes(&sample.observations, &trained.model.typical, *anchor))).map(|votes| quality_ease(&votes)))
            .collect();
        let below = eases.iter().filter(|ease| **ease < detail).count() as f64 / eases.len() as f64;
        assert!((below - DETAILED_SHARE).abs() < 0.02, "{below}");
    }

    #[test]
    fn a_gain_is_as_large_as_the_held_out_votes_allow() {
        // Scenes that band at no CRF, so the gain is held by quality alone.
        let mut samples = synthetic(5, 8);
        for sample in &mut samples {
            for point in &mut sample.observations {
                point.values[BANDING] = 0.0;
            }
        }
        let trained = train(&samples, &quick(), None).unwrap();
        let (typical, detail) = (&trained.model.typical, trained.model.detail);
        // Votes this accurate may be followed in full.
        assert_eq!(trained.model.gain, GAINS[0]);
        // Curves that claim every scene is twelve units better than it is raise scenes too far to be
        // followed in full, and are given less of what they claim.
        let flattering: Vec<Vec<Curves>> = trained
            .held_out
            .iter()
            .map(|members| {
                members
                    .iter()
                    .map(|curves| {
                        let mut curves = curves.map(|curve| Curve(curve.0.map(|value| value + 12.0)));
                        curves[BANDING] = Curve([0.0; KNOT_COUNT]);
                        curves
                    })
                    .collect()
            })
            .collect();
        assert!(choose_gain(&samples, &flattering, typical, detail) < GAINS[0]);
        assert!(!simulate(&samples, typical, detail, &decisions(&samples, &flattering, typical, detail, GAINS[0])).holds());
    }

    #[test]
    fn a_retrained_model_replaces_one_only_on_scenes_that_one_has_not_seen() {
        let old = train(&synthetic(7, 6), &quick(), None).unwrap();
        // Six more series arrive, of the same kind.
        let mut samples = synthetic(7, 6);
        samples.extend(synthetic(8, 12).into_iter().skip(72).map(|mut sample| { sample.collected += 5000; sample }));
        let mut candidate = train(&samples, &quick(), Some(old.model.typical)).unwrap();
        let verdict = judge(&old.model, &candidate, &samples);
        assert_eq!(verdict.scenes, 72);
        assert!(verdict.accepted && verdict.candidate <= verdict.current + GATE_TOLERANCE, "{verdict:?}");
        // The same networks deciding with a rule that raises four CRF further vote just as well, but
        // their raises overshoot, and they are refused for it.
        // An accepted candidate keeps the gain it was trained with.
        let gain = candidate.model.gain;
        let honest = settle_gain(&old.model, &mut candidate, &samples);
        assert_eq!((honest, candidate.model.gain), (verdict, gain));
        candidate.model.detail -= 4.0;
        candidate.model.gain = 1.0;
        let overshooting = judge(&old.model, &candidate, &samples);
        assert_eq!(overshooting.candidate, verdict.candidate);
        assert!(!overshooting.accepted && overshooting.candidate_shortfall > overshooting.current_shortfall + SHORTFALL_TOLERANCE, "{overshooting:?}");
        // Settling it on a smaller gain either passes while saving as much as the old model, or
        // leaves its gain as it was and refuses it.
        let settled = settle_gain(&old.model, &mut candidate, &samples);
        if settled.accepted {
            assert!(candidate.model.gain < 1.0 && settled.candidate_bytes <= settled.current_bytes, "{settled:?}");
        } else {
            assert_eq!((settled, candidate.model.gain), (overshooting, 1.0));
        }
        // A candidate that learned nothing is refused.
        let mut broken = train(&samples, &TrainConfig { epochs: 1, ..quick() }, Some(old.model.typical)).unwrap();
        for members in &mut broken.held_out {
            for curves in members {
                *curves = curves.map(|curve| Curve(curve.0.map(|value| value - 20.0)));
            }
        }
        assert!(!judge(&old.model, &broken, &samples).accepted);
        // With nothing measured since, there is nothing to judge on.
        let again = train(&synthetic(7, 6), &quick(), Some(old.model.typical)).unwrap();
        let verdict = judge(&old.model, &again, &synthetic(7, 6));
        assert_eq!((verdict.scenes, verdict.accepted), (0, false));
    }
}
