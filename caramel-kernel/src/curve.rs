// What an encode of a scene does as its CRF rises, metric by metric, and the network that says so
// from the scene's measurements alone.
//
// The fitted predictors of `model` each answer one question: how far can this scene's CRF move
// before one metric falls to what typical content scores at the anchor. A `Model` here answers the
// question underneath it. For every metric it draws the scene's whole curve — what XPSNR, SSIM and
// VMAF would read, how much banding the encode would add, how many bits it would spend, at any CRF
// — and a vote is read off the curve: the CRF at which it crosses the typical curve's value at the
// anchor. One network therefore serves every anchor, every encode that was ever measured is a
// training point wherever on the curve it lies, and a target can be moved without retraining.
//
// A curve is a level at the middle of the CRF range and a drop for each step away from it. The
// network's outputs are the level and the drops before a softplus, so a drop is never negative:
// whatever the weights are, a predicted curve falls as the CRF rises, as real ones do.
//
// A model is several networks, each trained on a different part of the data. Their mean is the
// prediction, and how far they disagree about a vote is how much the scene is unlike what they were
// trained on: the lab measures the scenes where it is largest.
//
// What a scene's CRF is made of its votes is `decide`: the anchor is the CRF for detailed scenes,
// and a scene is raised by how much less detailed than that it is.

use crate::inputs::INPUTS;
use crate::nn::{Layer, Network};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock, RwLock};

// The model this build carries: the one trained before it was compiled, so a machine that has
// measured nothing of its own still decides scenes with a network. A build without one — the file
// holds no model — falls back to the fitted predictors of `model`.
const BUILTIN: &str = include_str!("builtin.model");

pub fn builtin() -> Option<Arc<Model>> {
    static PARSED: OnceLock<Option<Arc<Model>>> = OnceLock::new();
    PARSED.get_or_init(|| Model::from_text(BUILTIN).ok().map(Arc::new)).clone()
}

static INSTALLED: RwLock<Option<Arc<Model>>> = RwLock::new(None);

// Makes `model` the one this process decides scenes with, in place of the built-in one: what a
// machine that retrains on its own measurements calls with the result.
pub fn install(model: Arc<Model>) {
    *INSTALLED.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(model);
}

// Goes back to the built-in model: what a long-running process calls when the machine's installed
// model was taken away.
pub fn uninstall() {
    *INSTALLED.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

// The model a new `SceneDecider` uses: the installed one, else the built-in one, else none.
pub fn active() -> Option<Arc<Model>> {
    INSTALLED.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().or_else(builtin)
}

// The figures a curve is kept for, in the order every observation and every model holds them. All
// of them fall as the CRF rises: the quality metrics, the banding an encode adds (as the negative
// of the CAMBI it adds over the source's own, so none is 0), and the bits it spends (log2 of bits
// per pixel).
pub const METRICS: usize = 7;
pub const METRIC_NAMES: [&str; METRICS] = ["xpsnr", "xpsnr-low", "ssim", "vmaf", "vmaf-low", "banding", "bits"];
// What counts as one unit of difference in each: half a dB, a VMAF point or so, one CAMBI, a
// seventh of the bytes.
pub const SCALES: [f64; METRICS] = [0.5, 0.5, 0.5, 1.0, 1.5, 1.0, 0.2];
// The metrics a scene's CRF is decided on. Bits are what is saved, not what is judged, and mean
// VMAF says nothing its worst frames do not.
pub const DECIDES: [bool; METRICS] = [true, true, true, false, true, true, false];
// Banding only ever holds a raise back. A scene with none to add is not thereby easy, so it has no
// say in how far a scene is raised past what the others allow, and none in a lowering.
pub const BANDING: usize = 5;
pub const BITS: usize = 6;
// How far below what typical content scores a scene may fall before a metric says it has, in the
// metric's units. Only banding has any: most scenes add none at any CRF, a curve that lies on the
// target has no crossing worth the name, and one CAMBI more than typical is where it starts to show.
pub const SLACK: [f64; METRICS] = [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
// The most a metric is taken to read. An encode that reproduces a still exactly scores infinity, or
// whatever the tool prints for it, and neither is a quality a curve should be bent towards.
pub const CEILINGS: [f64; METRICS] = [60.0, 60.0, 40.0, 100.0, 100.0, 0.0, 8.0];

// Where a curve is pinned down. Between knots it is a line; past the ends it carries on at the
// slope of its last step.
pub const KNOT_COUNT: usize = 7;
pub const KNOTS: [f64; KNOT_COUNT] = [10.0, 14.0, 18.0, 22.0, 26.0, 30.0, 34.0];
// The knot the level is given at: the middle of the range, where decisions are made, so an error in
// a drop far from it does not move the part of the curve that matters.
const LEVEL_KNOT: usize = 3;
// The furthest a vote is read. A curve that never reaches the target says "further than this".
pub const VOTE_LIMIT: f64 = 8.0;
// Standardised inputs are held within this many deviations of what the training saw, so a scene
// unlike anything measured is read as the nearest thing that was.
const INPUT_LIMIT: f64 = 4.0;

const FORMAT: &str = "caramel-model 1";

// One encode of a scene: the CRF, and what each metric read.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub crf: f64,
    pub values: [f64; METRICS],
}

// The value of the line through `points` (CRF, value; ascending CRF) at `crf`.
pub fn line_at(points: &[(f64, f64)], crf: f64) -> f64 {
    match points.len() {
        0 => 0.0,
        1 => points[0].1,
        count => {
            let after = points.iter().position(|point| point.0 >= crf).unwrap_or(count - 1).clamp(1, count - 1);
            let (a, b) = (points[after - 1], points[after]);
            if b.0 <= a.0 { a.1 } else { a.1 + (b.1 - a.1) * (crf - a.0) / (b.0 - a.0) }
        }
    }
}

// The highest CRF at which a falling line still reaches `target`. A line that is above the target
// throughout and flat at its end never comes down to it, and one that is below it throughout and
// flat at its start never reaches it: those answer with a CRF `VOTE_LIMIT` twice over past the end.
pub fn line_crossing(points: &[(f64, f64)], target: f64) -> f64 {
    let Some((&first, &last)) = points.first().zip(points.last()) else {
        return 0.0;
    };
    let beyond = 2.0 * VOTE_LIMIT;
    if points.len() == 1 {
        return if first.1 >= target { first.0 + beyond } else { first.0 - beyond };
    }
    if last.1 >= target {
        let before = points[points.len() - 2];
        let slope = (last.1 - before.1) / (last.0 - before.0);
        return if slope < -1e-9 { (last.0 + (target - last.1) / slope).min(last.0 + beyond) } else { last.0 + beyond };
    }
    if first.1 < target {
        let slope = (points[1].1 - first.1) / (points[1].0 - first.0);
        return if slope < -1e-9 { (first.0 + (target - first.1) / slope).max(first.0 - beyond) } else { first.0 - beyond };
    }
    // The last step that starts at or above the target and ends below it.
    for index in (0..points.len() - 1).rev() {
        let (a, b) = (points[index], points[index + 1]);
        if a.1 >= target && b.1 < target {
            return a.0 + (b.0 - a.0) * (a.1 - target) / (a.1 - b.1);
        }
    }
    first.0
}

// Measured points as a falling line: sorted by CRF, and a value that rose with the CRF — an
// encode's own noise, or banding that a coarser quantiser happened to hide — brought down to the
// lowest seen before it.
pub fn measured_line(observations: &[Observation], metric: usize) -> Vec<(f64, f64)> {
    let mut points: Vec<(f64, f64)> =
        observations.iter().filter(|point| !point.values[metric].is_nan()).map(|point| (point.crf, point.values[metric].min(CEILINGS[metric]))).collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    for index in 1..points.len() {
        points[index].1 = points[index].1.min(points[index - 1].1);
    }
    points
}

// One metric's curve: its value at every knot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve(pub [f64; KNOT_COUNT]);

impl Curve {
    fn points(&self) -> [(f64, f64); KNOT_COUNT] {
        std::array::from_fn(|knot| (KNOTS[knot], self.0[knot]))
    }

    pub fn at(&self, crf: f64) -> f64 {
        line_at(&self.points(), crf)
    }

    pub fn crossing(&self, target: f64) -> f64 {
        line_crossing(&self.points(), target)
    }
}

pub type Curves = [Curve; METRICS];

// A network's outputs for one scene as its curves, in each metric's own unit. Output
// `metric * KNOT_COUNT + LEVEL_KNOT` is the level; the outputs below it are the rises towards lower
// CRFs and the ones above it the drops towards higher ones, each through a softplus.
pub(crate) fn curves_from(outputs: &[f64], offsets: &[f64; METRICS]) -> Curves {
    std::array::from_fn(|metric| {
        let raw = &outputs[metric * KNOT_COUNT..(metric + 1) * KNOT_COUNT];
        let mut values = [0.0; KNOT_COUNT];
        values[LEVEL_KNOT] = offsets[metric] + SCALES[metric] * raw[LEVEL_KNOT];
        for knot in LEVEL_KNOT + 1..KNOT_COUNT {
            values[knot] = values[knot - 1] - SCALES[metric] * softplus(raw[knot]);
        }
        for knot in (0..LEVEL_KNOT).rev() {
            values[knot] = values[knot + 1] + SCALES[metric] * softplus(raw[knot]);
        }
        Curve(values)
    })
}

pub(crate) fn softplus(x: f64) -> f64 {
    if x > 30.0 { x } else { x.exp().ln_1p() }
}

pub(crate) fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

// The slope of a curve's value at `crf` with respect to each of its metric's raw outputs, in units
// of the metric's scale: what the trainer multiplies a point's error by. It also returns the value.
pub(crate) fn value_and_slopes(raw: &[f64], offset: f64, scale: f64, crf: f64) -> (f64, [f64; KNOT_COUNT]) {
    let mut values = [0.0; KNOT_COUNT];
    values[LEVEL_KNOT] = offset + scale * raw[LEVEL_KNOT];
    for knot in LEVEL_KNOT + 1..KNOT_COUNT {
        values[knot] = values[knot - 1] - scale * softplus(raw[knot]);
    }
    for knot in (0..LEVEL_KNOT).rev() {
        values[knot] = values[knot + 1] + scale * softplus(raw[knot]);
    }
    let after = KNOTS.iter().position(|knot| *knot >= crf).unwrap_or(KNOT_COUNT - 1).clamp(1, KNOT_COUNT - 1);
    let t = (crf - KNOTS[after - 1]) / (KNOTS[after] - KNOTS[after - 1]);
    let value = values[after - 1] * (1.0 - t) + values[after] * t;
    // The value is a weighted sum of two knots, and a knot is the level plus or minus every step
    // between it and the level's knot.
    let mut slopes = [0.0; KNOT_COUNT];
    for (knot, weight) in [(after - 1, 1.0 - t), (after, t)] {
        slopes[LEVEL_KNOT] += weight;
        for step in LEVEL_KNOT + 1..=knot {
            slopes[step] -= weight * sigmoid(raw[step]);
        }
        for step in knot..LEVEL_KNOT {
            slopes[step] += weight * sigmoid(raw[step]);
        }
    }
    (value, slopes)
}

// What a model says about one metric of a scene at an anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurveVote {
    // How far the CRF can move from the anchor before the metric falls to what typical content
    // scores at the anchor, within `VOTE_LIMIT`. Positive is an easy scene.
    pub offset: f64,
    // How far the model's networks disagree about it, in CRF.
    pub spread: f64,
}

// The quality a scene has to hold at an anchor, per metric: what typical content scores there, less
// the metric's slack.
pub fn targets(typical: &Curves, anchor: f64) -> [f64; METRICS] {
    std::array::from_fn(|metric| typical[metric].at(anchor) - SLACK[metric] * SCALES[metric])
}

// Every metric's vote at an anchor from the curves several networks drew for one scene.
pub fn votes_of(members: &[Curves], typical: &Curves, anchor: f64) -> [CurveVote; METRICS] {
    let mean = mean_curves(members);
    let targets = targets(typical, anchor);
    let read = |curve: &Curve, metric: usize| (curve.crossing(targets[metric]) - anchor).clamp(-VOTE_LIMIT, VOTE_LIMIT);
    std::array::from_fn(|metric| {
        let offset = read(&mean[metric], metric);
        let count = members.len().max(1) as f64;
        let spread = (members.iter().map(|curves| (read(&curves[metric], metric) - offset).powi(2)).sum::<f64>() / count).sqrt();
        CurveVote { offset, spread }
    })
}

// What a scene's own encodes say: how far its CRF could move from the anchor before each metric fell
// to the target. None for a metric measured at fewer than two CRFs.
pub fn measured_votes(observations: &[Observation], typical: &Curves, anchor: f64) -> [Option<f64>; METRICS] {
    let targets = targets(typical, anchor);
    std::array::from_fn(|metric| {
        let line = measured_line(observations, metric);
        (line.len() >= 2).then(|| (line_crossing(&line, targets[metric]) - anchor).clamp(-VOTE_LIMIT, VOTE_LIMIT))
    })
}

// How easy a scene is by the quality metrics, in CRF: the mean of what XPSNR, its worst frames,
// SSIM and VMAF's worst frames say its CRF could move by. No one metric is the scene — flat
// animation is easy by SSIM and ordinary by VMAF — and on sources the networks had not seen, the
// mean of the four followed what the encodes allowed more closely than the least of them did.
pub fn quality_ease(votes: &[CurveVote; METRICS]) -> f64 {
    let quality = |metric: &usize| DECIDES[*metric] && *metric != BANDING;
    let count = (0..METRICS).filter(quality).count().max(1) as f64;
    (0..METRICS).filter(quality).map(|metric| votes[metric].offset).sum::<f64>() / count
}

// The furthest a scene is raised and lowered from the anchor. A raise is what the rule is for; a
// lowering is kept for the scenes far harder than the detailed ones the anchor was chosen for, and
// is half a CRF, or one for the hardest.
pub const MAX_RAISE: f64 = 6.0;
pub const MAX_LOWER: f64 = 1.0;
// How far below a detailed scene, in CRF of ease, a scene has to read before it is lowered half a
// CRF, and before it is lowered a whole one.
pub const LOWER_BELOW: f64 = 3.0;
pub const LOWER_FURTHER: f64 = 5.0;

// What a scene's votes decide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    // How far the scene moves from the anchor, before bounds and rounding.
    pub offset: f64,
    // What the raise was read from: the quality metrics' ease, or banding's vote where that is
    // lower — and which of the two it was.
    pub ease: f64,
    pub banding: bool,
}

// The anchor is the CRF a detailed scene is encoded at, and `detail` is how easy such a scene is: a
// scene easier than that has quality to spare at the anchor, and is raised by `gain` of the CRF it
// has to spare. Banding holds a raise back — a flat scene is the easiest to raise and the first to
// band — so the raise is read from the lower of the quality metrics' ease and banding's vote. Only
// a scene far harder than a detailed one is lowered, and by little.
pub fn decide(votes: &[CurveVote; METRICS], detail: f64, gain: f64) -> Decision {
    let quality = quality_ease(votes);
    let banded = votes[BANDING].offset < quality;
    let ease = if banded { votes[BANDING].offset } else { quality };
    let offset = if ease > detail {
        gain * (ease - detail)
    } else if quality < detail - LOWER_FURTHER {
        -MAX_LOWER
    } else if quality < detail - LOWER_BELOW {
        -0.5
    } else {
        0.0
    };
    Decision { offset, ease, banding: banded }
}

// The same from a scene's own encodes: the quality ease and banding's vote. None when a deciding
// metric was not measured.
pub fn measured_votes_all(allowed: &[Option<f64>; METRICS]) -> Option<[CurveVote; METRICS]> {
    let mut votes = [CurveVote::default(); METRICS];
    for metric in (0..METRICS).filter(|metric| DECIDES[*metric]) {
        votes[metric].offset = allowed[metric]?;
    }
    Some(votes)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub version: String,
    // The encoder settings the training encodes were made with (`caramel_lab::encoder_key`).
    pub encoder: String,
    // How easy a detailed scene is, in CRF of ease against typical content: the scene the anchor is
    // the right CRF for. Set by the trainer from the measured scenes (`train::DETAILED_SHARE`).
    pub detail: f64,
    // How much of the CRF a scene has to spare over a detailed one it is raised by: set by the
    // trainer from how far raises fell short on sources it had never seen.
    pub gain: f64,
    // What the model was trained on, for whoever reads the file.
    pub scenes: usize,
    pub sources: usize,
    // When the newest scene it was trained on was measured, in Unix seconds: scenes measured since
    // are ones it has never seen.
    pub trained: u64,
    pub input_mean: [f64; INPUTS],
    pub input_scale: [f64; INPUTS],
    pub offsets: [f64; METRICS],
    // What typical content scores at each knot's CRF: the quality a vote is measured against.
    pub typical: Curves,
    // How far each metric's vote was from what the encodes allowed, in CRF, on held-out sources.
    pub error: [f64; METRICS],
    pub members: Vec<Network>,
}

impl Model {
    pub(crate) fn standardised(&self, inputs: &[f64; INPUTS]) -> [f64; INPUTS] {
        std::array::from_fn(|index| ((inputs[index] - self.input_mean[index]) * self.input_scale[index]).clamp(-INPUT_LIMIT, INPUT_LIMIT))
    }

    // Every member's curves for a scene.
    pub fn member_curves(&self, inputs: &[f64; INPUTS]) -> Vec<Curves> {
        let standardised = self.standardised(inputs);
        self.members.iter().map(|member| curves_from(&member.forward(&standardised), &self.offsets)).collect()
    }

    // The model's curves for a scene: the mean of its members'.
    pub fn curves(&self, inputs: &[f64; INPUTS]) -> Curves {
        mean_curves(&self.member_curves(inputs))
    }

    // Every metric's vote for a scene at an anchor.
    pub fn votes(&self, inputs: &[f64; INPUTS], anchor: f64) -> [CurveVote; METRICS] {
        votes_of(&self.member_curves(inputs), &self.typical, anchor)
    }

    // The votes, and the curves they were read from.
    pub fn read(&self, inputs: &[f64; INPUTS], anchor: f64) -> ([CurveVote; METRICS], Curves) {
        let members = self.member_curves(inputs);
        (votes_of(&members, &self.typical, anchor), mean_curves(&members))
    }

    // How unsure the model is about a scene: the mean disagreement over the metrics it decides on,
    // at the middle anchor.
    pub fn uncertainty(&self, inputs: &[f64; INPUTS]) -> f64 {
        let votes = self.votes(inputs, KNOTS[LEVEL_KNOT]);
        let judged: Vec<f64> = (0..METRICS).filter(|metric| DECIDES[*metric]).map(|metric| votes[metric].spread).collect();
        judged.iter().sum::<f64>() / judged.len().max(1) as f64
    }

    // The model's curve values for a scene at one CRF, in the order of `METRIC_NAMES`.
    pub fn predict(&self, inputs: &[f64; INPUTS], crf: f64) -> [f64; METRICS] {
        let curves = self.curves(inputs);
        std::array::from_fn(|metric| curves[metric].at(crf))
    }

    // The model as text: a header line per field and one line of numbers per layer. Weights are
    // written as 32-bit floats, which is more than the training knows about them.
    pub fn to_text(&self) -> String {
        fn numbers(values: &[f64]) -> String {
            values.iter().map(|value| format!("{}", *value as f32)).collect::<Vec<_>>().join(" ")
        }
        let mut text = format!("{FORMAT}\nversion {}\nencoder {}\ndetail {}\ngain {}\nscenes {}\nsources {}\ntrained {}\n", self.version, self.encoder, self.detail as f32, self.gain as f32, self.scenes, self.sources, self.trained);
        text += &format!("input-mean {}\ninput-scale {}\n", numbers(&self.input_mean), numbers(&self.input_scale));
        text += &format!("offsets {}\nerror {}\n", numbers(&self.offsets), numbers(&self.error));
        for (metric, curve) in self.typical.iter().enumerate() {
            text += &format!("typical {} {}\n", METRIC_NAMES[metric], numbers(&curve.0));
        }
        for member in &self.members {
            text += &format!("member {}\n", member.layers.len());
            for layer in &member.layers {
                text += &format!("layer {} {}\n{}\n{}\n", layer.inputs, layer.outputs, numbers(&layer.weights), numbers(&layer.bias));
            }
        }
        text
    }

    pub fn from_text(text: &str) -> Result<Self, String> {
        fn numbers<const N: usize>(rest: &str, what: &str) -> Result<[f64; N], String> {
            let values = list(rest, what)?;
            values.try_into().map_err(|_| format!("`{what}` does not hold {N} numbers"))
        }
        fn list(rest: &str, what: &str) -> Result<Vec<f64>, String> {
            rest.split_whitespace()
                .map(|token| token.parse::<f64>().ok().filter(|value| value.is_finite()).ok_or_else(|| format!("`{what}` holds `{token}`, not a number")))
                .collect()
        }
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(FORMAT) {
            return Err(format!("not a `{FORMAT}` file"));
        }
        let mut model = Model {
            version: String::new(),
            encoder: String::new(),
            detail: 0.0,
            gain: f64::NAN,
            scenes: 0,
            sources: 0,
            trained: 0,
            input_mean: [0.0; INPUTS],
            input_scale: [1.0; INPUTS],
            offsets: [0.0; METRICS],
            typical: [Curve([0.0; KNOT_COUNT]); METRICS],
            error: [0.0; METRICS],
            members: Vec::new(),
        };
        let mut seen_typical = 0usize;
        while let Some(line) = lines.next() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "version" => model.version = rest.trim().to_string(),
                "encoder" => model.encoder = rest.trim().to_string(),
                "detail" => model.detail = numbers::<1>(rest, key)?[0],
                "gain" => model.gain = numbers::<1>(rest, key)?[0],
                "scenes" => model.scenes = rest.trim().parse().map_err(|_| "`scenes` is not a count")?,
                "sources" => model.sources = rest.trim().parse().map_err(|_| "`sources` is not a count")?,
                "trained" => model.trained = rest.trim().parse().map_err(|_| "`trained` is not a time")?,
                "input-mean" => model.input_mean = numbers(rest, key)?,
                "input-scale" => model.input_scale = numbers(rest, key)?,
                "offsets" => model.offsets = numbers(rest, key)?,
                "error" => model.error = numbers(rest, key)?,
                "typical" => {
                    let (name, values) = rest.trim().split_once(' ').ok_or("`typical` has no values")?;
                    let metric = METRIC_NAMES.iter().position(|known| *known == name).ok_or_else(|| format!("`typical` names an unknown metric `{name}`"))?;
                    model.typical[metric] = Curve(numbers(values, key)?);
                    seen_typical += 1;
                }
                "member" => {
                    let count: usize = rest.trim().parse().map_err(|_| "`member` is not a layer count")?;
                    let mut layers = Vec::with_capacity(count);
                    for _ in 0..count {
                        let header = lines.next().ok_or("a member ends before its layers do")?;
                        let mut sizes = header.trim().strip_prefix("layer ").ok_or("a member's layer has no `layer` line")?.split_whitespace();
                        let mut size = || sizes.next().and_then(|token| token.parse::<usize>().ok()).filter(|size| *size > 0 && *size <= 4096);
                        let (inputs, outputs) = size().zip(size()).ok_or("a layer's sizes do not parse")?;
                        let weights = list(lines.next().ok_or("a layer has no weights")?, "weights")?;
                        let bias = list(lines.next().ok_or("a layer has no biases")?, "bias")?;
                        if weights.len() != inputs * outputs || bias.len() != outputs {
                            return Err("a layer's numbers do not match its sizes".to_string());
                        }
                        if layers.last().is_some_and(|before: &Layer| before.outputs != inputs) {
                            return Err("a layer does not take what the one before it gives".to_string());
                        }
                        layers.push(Layer { inputs, outputs, weights, bias });
                    }
                    let network = Network { layers };
                    if network.inputs() != INPUTS || network.outputs() != METRICS * KNOT_COUNT {
                        return Err(format!("a member reads {} inputs and gives {} outputs; this build needs {INPUTS} and {}", network.inputs(), network.outputs(), METRICS * KNOT_COUNT));
                    }
                    model.members.push(network);
                }
                // A field a later build added: this one does not need it.
                _ => {}
            }
        }
        if model.members.is_empty() {
            return Err("the model has no networks".to_string());
        }
        if seen_typical != METRICS || model.version.is_empty() {
            return Err("the model is missing its version or its typical curves".to_string());
        }
        // A model from before scenes were raised by their ease has a margin and no gain.
        if !(0.0..=1.0).contains(&model.gain) || !(-VOTE_LIMIT..=VOTE_LIMIT).contains(&model.detail) {
            return Err("the model has no gain between 0 and 1, or no detailed scene, to decide scenes with".to_string());
        }
        Ok(model)
    }
}

pub fn mean_curves(members: &[Curves]) -> Curves {
    let count = members.len().max(1) as f64;
    std::array::from_fn(|metric| Curve(std::array::from_fn(|knot| members.iter().map(|curves| curves[metric].0[knot]).sum::<f64>() / count)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::nn::Rng;

    // A model whose networks answer with their biases alone: every scene gets the same curves.
    pub(crate) fn flat_model(drop: f64) -> Model {
        let mut rng = Rng::new(1);
        let mut network = Network::new(&[INPUTS, 4, METRICS * KNOT_COUNT], &mut rng);
        for layer in &mut network.layers {
            layer.weights.fill(0.0);
        }
        // softplus(raw) = drop steps of each metric's scale per knot.
        let raw = (drop.exp() - 1.0).ln();
        for (index, bias) in network.layers[1].bias.iter_mut().enumerate() {
            *bias = if index % KNOT_COUNT == LEVEL_KNOT { 0.0 } else { raw };
        }
        let offsets = [40.0, 39.0, 20.0, 95.0, 93.0, 0.0, -3.0];
        let typical = curves_from(&network.forward(&[0.0; INPUTS]), &offsets);
        Model {
            version: "test-1".to_string(),
            encoder: "x264|fast|-|".to_string(),
            detail: -2.0,
            gain: 0.5,
            scenes: 10,
            sources: 2,
            trained: 1_700_000_000,
            input_mean: [0.0; INPUTS],
            input_scale: [1.0; INPUTS],
            offsets,
            typical,
            error: [2.0; METRICS],
            members: vec![network.clone(), network],
        }
    }

    #[test]
    fn a_line_is_read_between_and_past_its_points() {
        let line = [(10.0, 50.0), (20.0, 40.0), (30.0, 38.0)];
        assert_eq!(line_at(&line, 15.0), 45.0);
        assert_eq!(line_at(&line, 20.0), 40.0);
        // Past either end, at the slope of the step next to it.
        assert_eq!(line_at(&line, 5.0), 55.0);
        assert_eq!(line_at(&line, 35.0), 37.0);
        assert_eq!(line_crossing(&line, 45.0), 15.0);
        assert_eq!(line_crossing(&line, 39.0), 25.0);
        assert_eq!(line_crossing(&line, 37.0), 35.0);
        assert_eq!(line_crossing(&line, 52.0), 8.0);
        // Never reached, and never left: as far as a vote can be read, and further.
        let flat = [(10.0, 50.0), (20.0, 50.0)];
        assert_eq!(line_crossing(&flat, 40.0), 20.0 + 2.0 * VOTE_LIMIT);
        assert_eq!(line_crossing(&flat, 60.0), 10.0 - 2.0 * VOTE_LIMIT);
        // A shelf at the target: the scene holds it to the shelf's end.
        let shelf = [(10.0, 50.0), (20.0, 45.0), (30.0, 45.0), (40.0, 30.0)];
        assert_eq!(line_crossing(&shelf, 45.0), 30.0);
    }

    #[test]
    fn measured_points_make_a_falling_line() {
        let observation = |crf: f64, value: f64| Observation { crf, values: [value; METRICS] };
        let line = measured_line(&[observation(21.0, 40.0), observation(14.0, 44.0), observation(28.0, 41.0)], 0);
        assert_eq!(line, vec![(14.0, 44.0), (21.0, 40.0), (28.0, 40.0)]);
        // A lossless still scores infinity: read as the most the metric is taken to mean.
        let line = measured_line(&[observation(11.0, f64::INFINITY), observation(14.0, 52.0)], 2);
        assert_eq!(line, vec![(11.0, 40.0), (14.0, 40.0)]);
        let typical: Curves = [Curve(std::array::from_fn(|knot| -KNOTS[knot])); METRICS];
        // Typical content scores -21 at CRF 21; this scene scores it at 26.
        let scene = [observation(14.0, -4.0), observation(22.0, -18.0), observation(30.0, -24.0)];
        let votes = measured_votes(&scene, &typical, 21.0);
        assert!((votes[0].unwrap() - 5.0).abs() < 1e-9, "{votes:?}");
        // Banding is given one unit of slack: it is read against -22.
        assert!((votes[BANDING].unwrap() - (22.0 + 8.0 * 4.0 / 6.0 - 21.0)).abs() < 1e-9, "{votes:?}");
        assert_eq!(measured_votes(&scene[..1], &typical, 21.0)[0], None);
    }

    #[test]
    fn a_scene_is_raised_by_how_much_less_detailed_it_is() {
        let votes = |offsets: [f64; METRICS]| -> [CurveVote; METRICS] { std::array::from_fn(|metric| CurveVote { offset: offsets[metric], spread: 9.0 }) };
        // Mean VMAF and bits (-9 here) are not asked, nor is the networks' disagreement. The quality
        // metrics' mean is 4.75, which is 6.75 CRF easier than a detailed scene: half of it.
        let easy = decide(&votes([5.0, 4.0, 6.0, -9.0, 4.0, 8.0, -9.0]), -2.0, 0.5);
        assert_eq!((easy.offset, easy.ease, easy.banding), (3.375, 4.75, false));
        // A scene that would band is raised only as far as banding allows.
        let flat = decide(&votes([5.0, 4.0, 6.0, -9.0, 4.0, 1.0, -9.0]), -2.0, 0.5);
        assert_eq!((flat.offset, flat.ease, flat.banding), (1.5, 1.0, true));
        // A detailed scene stays at the anchor, and so does one a little harder than that.
        assert_eq!(decide(&votes([-2.0, -2.5, -1.5, 0.0, -2.0, 8.0, 0.0]), -2.0, 0.5).offset, 0.0);
        assert_eq!(decide(&votes([-4.0, -5.0, -4.0, 0.0, -4.5, 8.0, 0.0]), -2.0, 0.5).offset, 0.0);
        // Far harder, it is lowered half a CRF; further still, one. Banding has no say in either.
        assert_eq!(decide(&votes([-5.5, -5.0, -5.0, 0.0, -5.5, 8.0, 0.0]), -2.0, 0.5).offset, -0.5);
        assert_eq!(decide(&votes([-8.0, -7.5, -7.0, 0.0, -8.0, -8.0, 0.0]), -2.0, 0.5).offset, -MAX_LOWER);
        assert_eq!(measured_votes_all(&[Some(5.0), Some(4.0), Some(6.0), None, Some(4.0), Some(8.0), None]).map(|votes| quality_ease(&votes)), Some(4.75));
        assert!(measured_votes_all(&[Some(5.0), None, Some(6.0), Some(1.0), Some(4.5), Some(8.0), Some(1.0)]).is_none());
    }

    #[test]
    fn a_curve_falls_whatever_the_network_says() {
        let mut rng = Rng::new(9);
        for _ in 0..50 {
            let outputs: Vec<f64> = (0..METRICS * KNOT_COUNT).map(|_| rng.gaussian() * 4.0).collect();
            for curve in curves_from(&outputs, &[0.0; METRICS]) {
                assert!(curve.0.windows(2).all(|pair| pair[0] >= pair[1]), "{curve:?}");
            }
        }
    }

    // The slopes the trainer uses against the slope measured by nudging each raw output, between
    // knots and past both ends.
    #[test]
    fn a_points_slopes_match_finite_differences() {
        let mut rng = Rng::new(4);
        let raw: Vec<f64> = (0..KNOT_COUNT).map(|_| rng.gaussian()).collect();
        for crf in [6.0, 10.0, 12.5, 21.0, 22.0, 27.0, 33.0, 38.0] {
            let (value, slopes) = value_and_slopes(&raw, 40.0, 0.5, crf);
            let curve = curves_from(&[raw.clone(), vec![0.0; (METRICS - 1) * KNOT_COUNT]].concat(), &[40.0; METRICS])[0];
            assert!((value - curve.at(crf)).abs() < 1e-9, "{crf}");
            for index in 0..KNOT_COUNT {
                let mut nudged = raw.clone();
                nudged[index] += 1e-6;
                let up = value_and_slopes(&nudged, 40.0, 0.5, crf).0;
                nudged[index] -= 2e-6;
                let down = value_and_slopes(&nudged, 40.0, 0.5, crf).0;
                assert!(((up - down) / 2e-6 - 0.5 * slopes[index]).abs() < 1e-6, "crf {crf} output {index}");
            }
        }
    }

    #[test]
    fn a_typical_scene_votes_for_the_anchor() {
        let model = flat_model(1.0);
        for anchor in [15.0, 17.0, 21.0, 25.0] {
            for (metric, vote) in model.votes(&[0.0; INPUTS], anchor).iter().enumerate() {
                // Banding's slack is a unit, and a unit is four CRF of this model's curves.
                let expected = if metric == BANDING { 4.0 } else { 0.0 };
                assert!((vote.offset - expected).abs() < 1e-6 && vote.spread < 1e-9, "{vote:?}");
            }
        }
        assert!(model.uncertainty(&[0.3; INPUTS]) < 1e-9);
    }

    #[test]
    fn members_that_disagree_say_so() {
        let mut model = flat_model(1.0);
        // One network puts every level two units higher: its scene holds the target two CRF
        // further (a unit per knot step is a quarter of a unit per CRF, and the level is up two).
        for metric in 0..METRICS {
            model.members[1].layers[1].bias[metric * KNOT_COUNT + LEVEL_KNOT] = 2.0;
        }
        let votes = model.votes(&[0.0; INPUTS], 21.0);
        assert!((votes[0].offset - 4.0).abs() < 1e-6, "{votes:?}");
        assert!((votes[0].spread - 4.0).abs() < 1e-6, "{votes:?}");
        // Banding, a unit of slack ahead, runs into the limit of what a vote reads: 4 and 8 about 8.
        assert!((votes[BANDING].spread - 8f64.sqrt()).abs() < 1e-6, "{votes:?}");
        assert!(model.uncertainty(&[0.0; INPUTS]) > 3.7);
    }

    #[test]
    fn a_model_reads_back_from_its_text() {
        let mut rng = Rng::new(2);
        let mut model = flat_model(0.7);
        model.members[0] = Network::new(&[INPUTS, 6, 5, METRICS * KNOT_COUNT], &mut rng);
        model.input_mean[3] = 0.25;
        let text = model.to_text();
        let back = Model::from_text(&text).unwrap();
        // Written as 32-bit floats: what comes back is what the text says, and stays so.
        assert_eq!(back.to_text(), text);
        assert_eq!((back.version.as_str(), back.members.len(), back.scenes), ("test-1", 2, 10));
        let inputs = [0.2; INPUTS];
        for (a, b) in model.votes(&inputs, 19.0).iter().zip(back.votes(&inputs, 19.0)) {
            assert!((a.offset - b.offset).abs() < 1e-3);
        }
        // A field this build does not know is skipped; a file that is not a model is refused, as is
        // one cut short, one built for other inputs, and one with no gain to raise a scene by —
        // which is what a model from before scenes were raised by their ease is.
        assert!(Model::from_text(&text.replace("gain", "later-field 3\ngain")).is_ok());
        assert!(Model::from_text("").is_err());
        assert!(Model::from_text(&text[..text.len() / 2]).is_err());
        assert!(Model::from_text(&text.replace(&format!("layer {INPUTS} 6"), "layer 3 6")).is_err());
        assert!(Model::from_text(&text.replace("gain 0.5", "gain 1.5")).is_err());
        assert!(Model::from_text(&text.replace("gain 0.5", "margin 1.5")).is_err());
        assert!(Model::from_text(&text.replace("typical bits", "typical bytes")).is_err());
    }
}
