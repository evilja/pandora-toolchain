// Everything Caramel's model computes on the way to each scene's CRF, as JSON, for
// `scripts/caramel-viz`: the scene analysis exactly as `pnmpeg --caramel-report` runs it, then for
// every scene the inputs, each network's activations layer by layer, the curves they draw, the
// votes read from those curves and the decision. The model section carries every network's weights.
//
//   cargo run --example caramel_explain -- <input> <start-frame> <frames> <anchor> <out.json>
//
// The decision is recomputed here from the public pieces (`inputs`, `Model::standardised`, the
// layers, `curve::votes_of`, `curve::decide`); `check` in the output says whether it matched the
// analysis's own, so a viewer never shows a story that differs from what the encoder did.

use caramel_kernel::curve::{self, KNOTS, METRIC_NAMES, Model};
use caramel_kernel::inputs::{self, INPUT_NAMES};
use caramel_kernel::model::CURVE_ANCHORS;
use caramel_kernel::nn::Network;
use caramel_kernel::{EncodeContext, SceneConfig};
use pandora_toolchain::lib::mpeg::caramel::{FrameRange, SceneRequest, run_scene_analysis};
use pandora_toolchain::lib::mpeg::caramel_train;
use serde_json::{Value, json};
use std::path::Path;

fn silu(x: f64) -> f64 {
    x / (1.0 + (-x).exp())
}

fn round(x: f64) -> Value {
    if x.is_finite() { json!((x * 10_000.0).round() / 10_000.0) } else { Value::Null }
}

fn rounded(values: &[f64]) -> Value {
    Value::Array(values.iter().map(|x| round(*x)).collect())
}

// Every layer's values for one network: the standardised input, each hidden layer after its SiLU,
// and the raw output the curves are built from. The same sums `Network::forward` makes.
fn activations(network: &Network, input: &[f64]) -> Vec<Vec<f64>> {
    let mut layers = vec![input.to_vec()];
    for (index, layer) in network.layers.iter().enumerate() {
        let last = index + 1 == network.layers.len();
        let current = layers.last().unwrap();
        let next = (0..layer.outputs)
            .map(|output| {
                let row = &layer.weights[output * layer.inputs..(output + 1) * layer.inputs];
                let sum = layer.bias[output] + row.iter().zip(current).map(|(w, v)| w * v).sum::<f64>();
                if last { sum } else { silu(sum) }
            })
            .collect();
        layers.push(next);
    }
    layers
}

fn model_json(model: &Model) -> Value {
    json!({
        "version": model.version,
        "detail": model.detail,
        "gain": model.gain,
        "scenes": model.scenes,
        "sources": model.sources,
        "inputs": INPUT_NAMES,
        "metrics": METRIC_NAMES,
        "decides": curve::DECIDES,
        "scales": curve::SCALES,
        "knots": KNOTS,
        "max_raise": curve::MAX_RAISE,
        "max_lower": curve::MAX_LOWER,
        "lower_below": curve::LOWER_BELOW,
        "vote_limit": curve::VOTE_LIMIT,
        "typical": model.typical.iter().map(|curve| rounded(&curve.0)).collect::<Vec<_>>(),
        "members": model.members.iter().map(|member| {
            member.layers.iter().map(|layer| json!({
                "inputs": layer.inputs,
                "outputs": layer.outputs,
                "weights": rounded(&layer.weights),
                "bias": rounded(&layer.bias),
            })).collect::<Vec<_>>()
        }).collect::<Vec<_>>(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        eprintln!("usage: caramel_explain <input> <start-frame> <frames> <anchor> <out.json>");
        std::process::exit(2);
    }
    let (input, start, frames, anchor, output) = (Path::new(&args[1]), args[2].parse().unwrap(), args[3].parse().unwrap(), args[4].parse::<f32>().unwrap(), &args[5]);
    // The model an encode started now would decide with: the installed one, else the built-in one.
    caramel_train::activate();
    let Some(model) = curve::active() else {
        eprintln!("no Caramel network is installed or built in: scenes are decided by the linear predictors, which have no network to show");
        std::process::exit(1);
    };
    let request = SceneRequest {
        input,
        range: Some(FrameRange { start, frames }),
        fps: None,
        encode: EncodeContext::anchored(anchor),
        scenes: SceneConfig::default(),
        workers: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8),
        decoder_threads: 0,
        cancel_file: None,
        progress: None,
        // What the model decides, before a test encode pulls any raise back. Without a fast encode,
        // a model that reads one leaves every scene to the built-in model.
        probe: None,
        fast: None,
    };
    let report = run_scene_analysis(&request).unwrap_or_else(|error| {
        eprintln!("analysis failed: {error}");
        std::process::exit(1);
    });
    let held = (anchor as f64).clamp(CURVE_ANCHORS.0, CURVE_ANCHORS.1);
    let targets = curve::targets(&model.typical, held);
    let deep = report.source.bit_depth > 8;
    let mut mismatches = 0;
    let scenes: Vec<Value> = report
        .scenes
        .iter()
        .map(|scene| {
            let raw = inputs::inputs(&scene.features, deep);
            let standardised = model.standardised(&raw);
            let members = model.member_curves(&raw);
            let votes = curve::votes_of(&members, &model.typical, held);
            let decision = curve::decide(&votes, model.detail, model.gain);
            let mean = curve::mean_curves(&members);
            let measured = scene.features.measured > 0 && scene.features.frames > 0;
            let reported = scene.contributions.first().map(|contribution| contribution.delta);
            let agrees = !measured || reported.is_some_and(|delta| (delta - decision.offset).abs() < 1e-6);
            mismatches += usize::from(!agrees);
            json!({
                "start": scene.start_frame,
                "frames": scene.frames,
                "kind": scene.start,
                "crf": scene.crf,
                "adjustment": round(scene.adjustment),
                "raw_adjustment": round(scene.raw_adjustment),
                "confidence": round(scene.confidence),
                "frames_analyzed": scene.frames_analyzed,
                "measured": scene.features.measured,
                "cuts_inside": scene.features.cuts,
                "contributions": scene.contributions,
                "inputs": rounded(&raw),
                "standardised": rounded(&standardised),
                "activations": model.members.iter().map(|member| activations(member, &standardised).iter().map(|layer| rounded(layer)).collect::<Vec<_>>()).collect::<Vec<_>>(),
                "member_curves": members.iter().map(|curves| curves.iter().map(|curve| rounded(&curve.0)).collect::<Vec<_>>()).collect::<Vec<_>>(),
                "curves": mean.iter().map(|curve| rounded(&curve.0)).collect::<Vec<_>>(),
                "votes": votes.iter().map(|vote| json!({"offset": round(vote.offset), "spread": round(vote.spread)})).collect::<Vec<_>>(),
                "quality_ease": round(curve::quality_ease(&votes)),
                "decision": {"offset": round(decision.offset), "ease": round(decision.ease), "banding": decision.banding},
                "check": agrees,
            })
        })
        .collect();
    if mismatches > 0 {
        eprintln!("warning: {mismatches} scenes decided differently here than in the analysis");
    }
    let document = json!({
        "input": report.input,
        "source": report.source,
        "anchor": anchor,
        "held_anchor": held,
        "targets": rounded(&targets),
        "range": {"start": start, "frames": report.frames},
        "scene_config": report.scene_config,
        "measured_stride": (report.scene_config.analysis_frames / caramel_kernel::scene::MEASURED_FRAMES).max(1),
        "model": model_json(&model),
        "scenes": scenes,
        "summary": report.summary,
    });
    std::fs::write(output, serde_json::to_vec(&document).unwrap()).unwrap();
    eprintln!("{} scenes, model {}, written to {output}", report.scenes.len(), model.version);
}
