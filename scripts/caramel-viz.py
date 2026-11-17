#!/usr/bin/env python3
# Caramel, slowed down: an animated page that walks through how the installed model picks each
# scene's CRF in a stretch of video. For every scene it shows the frame, the 27 measurements, every
# layer of the network lighting up in turn, the quality curves the outputs become, the votes read
# off those curves, and the arithmetic that turns the votes into a CRF.
#
#   python3 scripts/caramel-viz.py <video> [--start 4000] [--frames 1440] [--anchor 17]
#
# Run it from the checkout whose DB/ holds the installed model: the exporter decides with whatever
# model an encode started there would. The numbers come from `examples/caramel_explain.rs`, which
# runs the same scene analysis as `pnmpeg --caramel-report` and checks that its recomputed
# decisions match the analysis's own. The page is one self-contained HTML file; nothing is fetched.
#
# Standard library only. Building the exporter needs the same environment as any other cargo
# build of this crate (PNX264_INCLUDE_DIR / PNX264_LIB_DIR where x264 is not on the default path).

import argparse
import base64
import json
import os
import shutil
import subprocess
import sys
import tempfile
import webbrowser
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXPORTER = REPO / "target" / "debug" / "examples" / "caramel_explain"


def runtime_binary(name):
    # DB/bin portable builds win over PATH, as they do for every Pandora tool.
    local = Path.cwd() / "DB" / "bin" / name
    return str(local) if local.is_file() else shutil.which(name)


def export(args, workdir):
    if not args.no_build:
        built = subprocess.run(["cargo", "build", "--example", "caramel_explain"], cwd=REPO)
        if built.returncode != 0:
            sys.exit("building the exporter failed (see above)")
    output = Path(workdir) / "scenes.json"
    command = [str(EXPORTER), args.input, str(args.start), str(args.frames), str(args.anchor), str(output)]
    if subprocess.run(command).returncode != 0:
        sys.exit("the exporter failed (see above)")
    return json.loads(output.read_text())


# One frame from inside each scene's analysed opening, as a JPEG data URI, in scene order.
def thumbnails(data, video, workdir):
    ffmpeg = runtime_binary("ffmpeg")
    if not ffmpeg:
        print("ffmpeg not found: the page will have no frames", file=sys.stderr)
        return [None] * len(data["scenes"])
    first = data["range"]["start"]
    fps = data["source"]["fps"] or 24000 / 1001
    picks = [scene["start"] - first + min(6, scene["frames"] // 2) for scene in data["scenes"]]
    expression = "+".join(f"eq(n\\,{pick})" for pick in picks)
    pattern = str(Path(workdir) / "frame-%04d.jpg")
    subprocess.run(
        [ffmpeg, "-v", "error", "-ss", f"{first / fps:.6f}", "-i", video, "-frames:v", str(len(picks)),
         "-vf", f"select='{expression}',scale=480:-2", "-fps_mode", "passthrough", "-q:v", "4", pattern],
        check=False,
    )
    frames = []
    for index in range(len(picks)):
        path = Path(workdir) / f"frame-{index + 1:04d}.jpg"
        frames.append("data:image/jpeg;base64," + base64.b64encode(path.read_bytes()).decode() if path.is_file() else None)
    return frames


def main():
    parser = argparse.ArgumentParser(description="Animate how Caramel's model picks each scene's CRF.")
    parser.add_argument("input", help="the video to analyse")
    parser.add_argument("--start", type=int, default=0, help="first frame of the stretch (default 0)")
    parser.add_argument("--frames", type=int, default=1440, help="how many frames to analyse (default 1440, a minute at 24 fps)")
    parser.add_argument("--anchor", type=float, default=17.0, help="the preset's CRF (default 17)")
    parser.add_argument("--out", help="where to write the page (default caramel-viz-<video>.html here)")
    parser.add_argument("--json", help="reuse an exporter output instead of analysing again")
    parser.add_argument("--no-build", action="store_true", help="use the exporter already built")
    parser.add_argument("--no-open", action="store_true", help="write the page without opening it")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="caramel-viz-") as workdir:
        data = json.loads(Path(args.json).read_text()) if args.json else export(args, workdir)
        frames = thumbnails(data, args.input, workdir)
    if not all(scene["check"] for scene in data["scenes"]):
        print("warning: some scenes were recomputed differently from the analysis; the page marks them", file=sys.stderr)
    data["title"] = Path(args.input).stem
    page = TEMPLATE.replace("__DATA__", json.dumps(data, separators=(",", ":"))).replace("__FRAMES__", json.dumps(frames))
    out = Path(args.out) if args.out else Path.cwd() / f"caramel-viz-{Path(args.input).stem[:60]}.html"
    out.write_text(page, encoding="utf-8")
    print(f"{len(data['scenes'])} scenes → {out}")
    if not args.no_open:
        webbrowser.open(out.resolve().as_uri())


TEMPLATE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Caramel, slowed down</title>
<style>
:root {
  --bg: #0d1117; --panel: #151b24; --line: #263040; --text: #e6edf3; --muted: #8b98a9; --faint: #4b5869;
  --pos: #fb923c; --neg: #60a5fa; --target: #fbbf24; --curve: #67e8f9;
}
* { box-sizing: border-box; }
html, body { margin: 0; background: var(--bg); color: var(--text); font: 13px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
header { display: flex; flex-wrap: wrap; gap: 10px 18px; align-items: center; padding: 10px 16px; border-bottom: 1px solid var(--line); }
header h1 { font-size: 16px; margin: 0; font-weight: 650; letter-spacing: .2px; }
header .meta { color: var(--muted); font-size: 12px; }
.controls { display: flex; gap: 6px; align-items: center; margin-left: auto; flex-wrap: wrap; }
button, select { background: var(--panel); color: var(--text); border: 1px solid var(--line); border-radius: 6px; padding: 4px 9px; font: inherit; cursor: pointer; }
button:hover, select:hover { border-color: var(--faint); }
input[type=range] { width: 160px; accent-color: #67e8f9; }
main { display: grid; gap: 10px; padding: 10px 16px 16px; grid-template-columns: minmax(0, 1.5fr) minmax(0, 1fr);
  grid-template-areas: "net frame" "net curves" "net decision" "timeline timeline"; }
section { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; min-width: 0; }
section h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .8px; color: var(--muted); margin: 0 0 6px; font-weight: 600; display: flex; justify-content: space-between; gap: 8px; }
section h2 span.right { text-transform: none; letter-spacing: 0; font-weight: 500; color: var(--text); }
#net { grid-area: net; display: flex; flex-direction: column; }
#frame { grid-area: frame; }
#curves { grid-area: curves; }
#decision { grid-area: decision; }
#timeline { grid-area: timeline; }
canvas { display: block; width: 100%; }
#netCanvas { flex: 1 1 auto; height: 560px; min-height: 560px; }
#members { height: 96px; }
#curveCanvas { height: 330px; }
#voteCanvas { height: 196px; }
#timeCanvas { height: 170px; }
#phase { font-size: 14px; color: var(--text); margin: 0 0 4px; min-height: 21px; }
#phase b { color: #67e8f9; font-weight: 600; }
.frameRow { display: flex; gap: 12px; align-items: flex-start; }
#thumb { width: 52%; aspect-ratio: 16/9; object-fit: cover; border-radius: 6px; background: #000; border: 1px solid var(--line); }
#sceneInfo { flex: 1; font-size: 12.5px; color: var(--muted); }
#sceneInfo .big { font-size: 34px; font-weight: 700; color: var(--text); line-height: 1.1; font-variant-numeric: tabular-nums; }
#sceneInfo .kind { color: var(--text); }
#eq { font-variant-numeric: tabular-nums; font-size: 12.5px; margin-top: 6px; }
#eq div { opacity: .12; transition: opacity .35s; padding: 2px 0; }
#eq div.on { opacity: 1; }
#eq .result { font-size: 15px; font-weight: 650; }
#eq code { font: inherit; color: #67e8f9; }
#stats { color: var(--muted); font-size: 12px; text-transform: none; letter-spacing: 0; font-weight: 500; }
#tip { position: fixed; pointer-events: none; background: #0b0f15ee; border: 1px solid var(--line); border-radius: 6px; padding: 5px 8px; font-size: 12px; display: none; z-index: 5; max-width: 300px; }
.warn { color: #fb7185; }
.legend { color: var(--muted); font-size: 11.5px; margin-top: 4px; }
.legend i { display: inline-block; width: 18px; height: 0; border-top: 2px solid; vertical-align: middle; margin: 0 4px 0 10px; }
kbd { border: 1px solid var(--line); border-radius: 4px; padding: 0 4px; font-size: 11px; color: var(--muted); }
@media (max-width: 1100px) {
  main { grid-template-columns: minmax(0, 1fr); grid-template-areas: "frame" "net" "curves" "decision" "timeline"; }
  #netCanvas { height: 480px; min-height: 480px; }
}
</style>
</head>
<body>
<header>
  <h1>Caramel, slowed down</h1>
  <div class="meta" id="meta"></div>
  <div class="controls">
    <button id="prev" title="previous scene (←)">⏮</button>
    <button id="play" title="play / pause (space)">❚❚</button>
    <button id="next" title="next scene (→)">⏭</button>
    <select id="speed" title="speed (↑ ↓)">
      <option value="0.25">0.25×</option><option value="0.5">0.5×</option><option value="1" selected>1×</option>
      <option value="2">2×</option><option value="4">4×</option><option value="12">12×</option>
    </select>
    <input type="range" id="scrub" min="0" value="0">
    <span class="meta" id="sceneCount"></span>
  </div>
</header>
<main>
  <section id="net">
    <h2>Network <span class="right" id="netTitle"></span></h2>
    <div id="phase"></div>
    <canvas id="netCanvas"></canvas>
    <h2 style="margin-top:8px">The ensemble: every network sees the same scene <span class="right meta">click one to follow it · number = its XPSNR vote</span></h2>
    <canvas id="members" height="96"></canvas>
  </section>
  <section id="frame">
    <h2>Scene <span class="right" id="frameTitle"></span></h2>
    <div class="frameRow">
      <img id="thumb" alt="">
      <div id="sceneInfo"></div>
    </div>
  </section>
  <section id="curves">
    <h2>Outputs → curves: each metric against CRF <span class="right meta">votes are read where a curve meets its target</span></h2>
    <canvas id="curveCanvas" height="330"></canvas>
    <div class="legend"><i style="border-color:#67e8f9"></i>the ensemble's curve<i style="border-color:#67e8f955"></i>each network<i style="border-color:#8b98a9;border-top-style:dashed"></i>typical content<i style="border-color:#fbbf24;border-top-style:dotted"></i>target: typical at the anchor</div>
  </section>
  <section id="decision">
    <h2>Votes → decision <span class="right meta">CRF the scene could move before falling to the target</span></h2>
    <canvas id="voteCanvas" height="196"></canvas>
    <div id="eq"></div>
  </section>
  <section id="timeline">
    <h2>CRF over the stretch <span id="stats"></span></h2>
    <canvas id="timeCanvas" height="170"></canvas>
    <div class="legend"><kbd>space</kbd> pause · <kbd>←</kbd><kbd>→</kbd> scene · <kbd>↑</kbd><kbd>↓</kbd> speed · click the timeline to jump · hover the network for values</div>
  </section>
</main>
<div id="tip"></div>
<script>
const D = __DATA__;
const FRAMES = __FRAMES__;
const M = D.model, S = D.scenes, K = M.knots, METRICS = M.metrics;
const HELD = D.held_anchor, ANCHOR = D.anchor;
const QUALITY = [0, 1, 2, 4], BANDING = 5;
const UNITS = ["dB", "dB", "dB", "", "", "−CAMBI", "log₂ bpp"];
const $ = id => document.getElementById(id);

// ---- timing: one scene is a sequence of phases, in seconds at 1× ----
const T = { inputs: [0, 0.9], l0: [0.9, 2.0], l1: [2.0, 3.1], l2: [3.1, 4.2], curves: [4.2, 5.6], votes: [5.6, 7.0], decide: [7.0, 8.4], end: 9.4 };
const PHASES = [
  ["inputs", "Reading the scene: <b>27 measurements</b> of its first frames (noise, edges, detail, motion …), standardised"],
  ["l0", "Hidden layer 1: <b>32 units</b>, each a weighted sum of the inputs through SiLU"],
  ["l1", "Hidden layer 2: <b>32 units</b> combine the first layer's features"],
  ["l2", "Output layer: <b>49 numbers</b>, 7 per metric: a level at CRF 22 and six steps"],
  ["curves", "The outputs become <b>curves</b>: how each metric would fall as the CRF rises"],
  ["votes", "Each curve is read where it meets the <b>target</b>: how far the CRF can move from the anchor"],
  ["decide", "The votes become <b>one CRF</b>"],
];
let scene = 0, t = 0, playing = true, speed = 1, member = 0, lastStamp = null;
const clamp01 = x => Math.max(0, Math.min(1, x));
const ease = x => x < 0.5 ? 2 * x * x : 1 - Math.pow(-2 * x + 2, 2) / 2;
const phase = name => clamp01((t - T[name][0]) / (T[name][1] - T[name][0]));
const fmt = (x, d = 2) => (x >= 0 ? "+" : "−") + Math.abs(x).toFixed(d);
const num = (x, d = 2) => (x < 0 ? "−" : "") + Math.abs(x).toFixed(d);

// ---- colours ----
function mix(a, b, f) { return a.map((v, i) => Math.round(v + (b[i] - v) * f)); }
const hex = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
const rgb = (c, a = 1) => `rgba(${c[0]},${c[1]},${c[2]},${a})`;
const NEG = hex("#60a5fa"), POS = hex("#fb923c"), MID = hex("#2a3442");
function activationColour(v, scale, a = 1) {
  const f = Math.tanh(Math.abs(v) / scale);
  return rgb(mix(MID, v < 0 ? NEG : POS, f), a);
}
const CRF_STOPS = [[-0.5, hex("#c084fc")], [0, hex("#2dd4bf")], [1.5, hex("#a3e635")], [3, hex("#facc15")], [4.5, hex("#fb923c")], [6, hex("#f43f5e")]];
function crfColour(crf, a = 1) {
  const x = crf - ANCHOR;
  for (let i = 1; i < CRF_STOPS.length; i++) {
    if (x <= CRF_STOPS[i][0]) {
      const [x0, c0] = CRF_STOPS[i - 1], [x1, c1] = CRF_STOPS[i];
      return rgb(mix(c0, c1, clamp01((x - x0) / (x1 - x0))), a);
    }
  }
  return rgb(CRF_STOPS[CRF_STOPS.length - 1][1], a);
}

// ---- curve maths, as curve.rs does it ----
function lineAt(values, crf) {
  let after = K.findIndex(k => k >= crf);
  if (after < 0) after = K.length - 1;
  after = Math.max(1, Math.min(K.length - 1, after));
  const a = after - 1;
  return values[a] + (values[after] - values[a]) * (crf - K[a]) / (K[after] - K[a]);
}
// line_crossing: the highest CRF at which the falling line still reaches the target.
function crossing(values, target) {
  const n = K.length, beyond = 2 * M.vote_limit;
  if (values[n - 1] >= target) {
    const slope = (values[n - 1] - values[n - 2]) / (K[n - 1] - K[n - 2]);
    return slope < -1e-9 ? Math.min(K[n - 1] + (target - values[n - 1]) / slope, K[n - 1] + beyond) : K[n - 1] + beyond;
  }
  if (values[0] < target) {
    const slope = (values[1] - values[0]) / (K[1] - K[0]);
    return slope < -1e-9 ? Math.max(K[0] + (target - values[0]) / slope, K[0] - beyond) : K[0] - beyond;
  }
  for (let i = n - 2; i >= 0; i--)
    if (values[i] >= target && values[i + 1] < target) return K[i] + (K[i + 1] - K[i]) * (values[i] - target) / (values[i] - values[i + 1]);
  return K[0];
}
// Every network's vote on every metric for the scene on screen, read once when the scene starts.
let MEMBER_VOTES = [];

// ---- canvases ----
const dpr = () => window.devicePixelRatio || 1;
function fit(canvas) {
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (canvas.width !== Math.round(w * dpr()) || canvas.height !== Math.round(h * dpr())) {
    canvas.width = Math.round(w * dpr()); canvas.height = Math.round(h * dpr());
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr(), 0, 0, dpr(), 0, 0);
  return [ctx, w, h];
}
function text(ctx, s, x, y, colour = "#8b98a9", size = 11, align = "left", weight = 400) {
  ctx.fillStyle = colour; ctx.font = `${weight} ${size}px system-ui, -apple-system, sans-serif`;
  ctx.textAlign = align; ctx.textBaseline = "middle"; ctx.fillText(s, x, y);
}

// ---- per-scene preparation ----
let EDGES = [], NODES = [], SKELETON = null;
function contributions() {
  const acts = S[scene].activations[member];
  MEMBER_VOTES = S[scene].member_curves.map(curves => curves.map((c, m) => Math.max(-M.vote_limit, Math.min(M.vote_limit, crossing(c, D.targets[m]) - HELD))));
  EDGES = M.members[member].map((layer, l) => {
    const a = acts[l], list = [];
    for (let o = 0; o < layer.outputs; o++)
      for (let i = 0; i < layer.inputs; i++) list.push([i, o, layer.weights[o * layer.inputs + i] * a[i]]);
    list.sort((x, y) => Math.abs(y[2]) - Math.abs(x[2]));
    const kept = list.slice(0, 240);
    return { list: kept, max: Math.abs(kept[0][2]) || 1 };
  });
  // The strongest weights of this network, whatever the scene: the faint wiring underneath.
  SKELETON = M.members[member].map(layer => {
    const list = [];
    for (let o = 0; o < layer.outputs; o++)
      for (let i = 0; i < layer.inputs; i++) list.push([i, o, layer.weights[o * layer.inputs + i]]);
    list.sort((x, y) => Math.abs(y[2]) - Math.abs(x[2]));
    return list.slice(0, 380);
  });
}

// ---- the network ----
const netCanvas = $("netCanvas");
function layout(w, h) {
  const left = 168, right = 168, top = 16, bottom = h - 10;
  const x0 = left, x3 = w - right, cols = [x0, x0 + (x3 - x0) / 3, x0 + 2 * (x3 - x0) / 3, x3];
  const sizes = [27, 32, 32, 49];
  const positions = sizes.map((n, c) => {
    if (c < 3) return Array.from({ length: n }, (_, i) => [cols[c], top + (bottom - top) * (i + 0.5) / n]);
    const slots = 49 + 6 * 1.4, unit = (bottom - top) / slots;
    return Array.from({ length: n }, (_, i) => [cols[c], top + unit * (i + Math.floor(i / 7) * 1.4 + 0.5)]);
  });
  return { cols, positions, radius: Math.max(2.4, Math.min(6, (bottom - top) / 49 * 0.36)) };
}
function columnScale(values) { return Math.max(0.6, ...values.map(Math.abs)) * 0.6; }

function drawNetwork() {
  const [ctx, w, h] = fit(netCanvas);
  ctx.clearRect(0, 0, w, h);
  const s = S[scene], acts = s.activations[member];
  const L = layout(w, h), P = L.positions;
  NODES = [];
  const reveal = [phase("inputs"), phase("l0"), phase("l1"), phase("l2")];
  // faint wiring
  ctx.lineWidth = 0.6;
  SKELETON.forEach((list, l) => list.forEach(([i, o, wgt]) => {
    ctx.strokeStyle = wgt < 0 ? "rgba(96,165,250,0.05)" : "rgba(251,146,60,0.05)";
    ctx.beginPath(); ctx.moveTo(...P[l][i]); ctx.lineTo(...P[l + 1][o]); ctx.stroke();
  }));
  // what this scene sends through each layer
  EDGES.forEach((layer, l) => {
    const p = reveal[l + 1];
    if (p <= 0) return;
    layer.list.forEach(([i, o, c], rank) => {
      const strength = Math.abs(c) / layer.max;
      ctx.strokeStyle = c < 0 ? `rgba(96,165,250,${(0.06 + 0.7 * strength) * Math.min(1, p * 1.6)})` : `rgba(251,146,60,${(0.06 + 0.7 * strength) * Math.min(1, p * 1.6)})`;
      ctx.lineWidth = 0.4 + 2.2 * strength;
      ctx.beginPath(); ctx.moveTo(...P[l][i]); ctx.lineTo(...P[l + 1][o]); ctx.stroke();
      if (p < 1 && rank < 90) {
        const f = clamp01((p - (rank % 9) / 9 * 0.35) / 0.6);
        if (f > 0 && f < 1) {
          const [ax, ay] = P[l][i], [bx, by] = P[l + 1][o];
          ctx.fillStyle = c < 0 ? "#bfdbfe" : "#fed7aa";
          ctx.beginPath(); ctx.arc(ax + (bx - ax) * f, ay + (by - ay) * f, 1.2 + 1.8 * strength, 0, 7); ctx.fill();
        }
      }
    });
  });
  // nodes
  acts.forEach((values, c) => {
    const scale = c === 0 ? 1.6 : c === 3 ? 1.6 : columnScale(values);
    const p = ease(reveal[c]);
    values.forEach((v, i) => {
      const [x, y] = P[c][i];
      const r = L.radius * (0.75 + 0.45 * Math.tanh(Math.abs(v) / scale) * p);
      ctx.fillStyle = p > 0 ? activationColour(v * p, scale) : "#1b2330";
      ctx.strokeStyle = c === 3 && i % 7 === 3 ? "#e6edf3" : "#3a4658";
      ctx.lineWidth = c === 3 && i % 7 === 3 ? 1.4 : 0.8;
      ctx.beginPath(); ctx.arc(x, y, r, 0, 7); ctx.fill(); ctx.stroke();
      NODES.push({ x, y, c, i, v });
    });
  });
  // input names, with a bar for each standardised value
  const pi = ease(reveal[0]);
  P[0].forEach(([x, y], i) => {
    const v = s.standardised[i];
    text(ctx, M.inputs[i], 8, y, pi > 0 ? "#c9d4e0" : "#4b5869", Math.min(11, L.radius * 2.6));
    const cx = x - 34, half = 22;
    ctx.fillStyle = "#222b38"; ctx.fillRect(cx - half, y - 2.5, half * 2, 5);
    ctx.fillStyle = activationColour(v, 1.6);
    const len = half * Math.max(-1, Math.min(1, v / 4)) * pi;
    ctx.fillRect(Math.min(cx, cx + len), y - 2.5, Math.abs(len), 5);
    ctx.fillStyle = "#4b5869"; ctx.fillRect(cx - 0.5, y - 4, 1, 8);
  });
  // output groups: metric names and the curve each group draws
  const pc = ease(phase("curves"));
  for (let m = 0; m < METRICS.length; m++) {
    const ys = P[3].slice(m * 7, m * 7 + 7).map(p => p[1]);
    const top = ys[0], bottom = ys[6], x = L.cols[3] + 12;
    ctx.strokeStyle = "#3a4658"; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(x, top); ctx.lineTo(x + 4, top); ctx.moveTo(x, top); ctx.lineTo(x, bottom); ctx.lineTo(x + 4, bottom); ctx.stroke();
    const deciding = M.decides[m];
    text(ctx, METRICS[m], x + 10, (top + bottom) / 2 - 7, deciding ? "#e6edf3" : "#6b7889", 11.5, "left", 600);
    text(ctx, deciding ? "decides" : "shown only", x + 10, (top + bottom) / 2 + 8, "#6b7889", 10);
    if (pc > 0) {
      const curve = s.member_curves[member][m], lo = Math.min(...curve), hi = Math.max(...curve);
      const sx = x + 82, sw = 60, sy = top, sh = bottom - top;
      ctx.strokeStyle = deciding ? "#67e8f9" : "#6b7889"; ctx.lineWidth = 1.5; ctx.beginPath();
      const n = Math.max(2, Math.round(7 * pc));
      for (let k = 0; k < n; k++) {
        const px = sx + sw * k / 6, py = sy + sh * (1 - (curve[k] - lo) / ((hi - lo) || 1));
        k ? ctx.lineTo(px, py) : ctx.moveTo(px, py);
      }
      ctx.stroke();
    }
  }
  // column headings
  const heads = ["27 inputs", "32 hidden · SiLU", "32 hidden · SiLU", "49 outputs"];
  L.cols.forEach((x, c) => text(ctx, heads[c], x, 6, reveal[c] > 0 ? "#c9d4e0" : "#4b5869", 10.5, "center", 600));
}

// ---- the ensemble strip ----
const membersCanvas = $("members");
let tiles = [];
function drawMembers() {
  const [ctx, w, h] = fit(membersCanvas);
  ctx.clearRect(0, 0, w, h);
  const s = S[scene], n = M.members.length, gap = 8, tw = (w - gap * (n - 1)) / n, th = h - 18;
  const reveal = [phase("inputs"), phase("l0"), phase("l1"), phase("l2")];
  const target = D.targets[0];
  tiles = [];
  for (let k = 0; k < n; k++) {
    const x = k * (tw + gap), acts = s.activations[k];
    ctx.fillStyle = k === member ? "#1e2a3a" : "#111720";
    ctx.strokeStyle = k === member ? "#67e8f9" : "#263040"; ctx.lineWidth = k === member ? 1.5 : 1;
    ctx.beginPath(); ctx.roundRect(x, 0, tw, th, 5); ctx.fill(); ctx.stroke();
    const cw = (tw - 10) / 3;
    [1, 2, 3].forEach((c, j) => {
      const values = acts[c], scale = c === 3 ? 1.6 : columnScale(values), p = ease(reveal[c]);
      const ch = (th - 8) / values.length;
      values.forEach((v, i) => {
        ctx.fillStyle = p > 0 ? activationColour(v * p, scale) : "#1b2330";
        ctx.fillRect(x + 4 + j * (cw + 1), 4 + i * ch, cw - 1, Math.max(0.6, ch - 0.4));
      });
    });
    const vote = MEMBER_VOTES[k][0];
    const shown = phase("votes") > 0.3;
    text(ctx, shown ? fmt(vote, 1) : "·", x + tw / 2, h - 7, shown ? crfColour(ANCHOR + Math.max(-0.5, vote)) : "#4b5869", 11, "center", 600);
    tiles.push([x, x + tw]);
  }
}
membersCanvas.addEventListener("click", e => {
  const r = membersCanvas.getBoundingClientRect(), x = e.clientX - r.left;
  const k = tiles.findIndex(([a, b]) => x >= a && x <= b);
  if (k >= 0) { member = k; contributions(); $("netTitle").textContent = `network ${member + 1} of ${M.members.length}`; }
});

// ---- curves ----
const curveCanvas = $("curveCanvas");
const XMIN = 8, XMAX = 36;
function drawCurves() {
  const [ctx, w, h] = fit(curveCanvas);
  ctx.clearRect(0, 0, w, h);
  const s = S[scene], cols = w < 560 ? 2 : 4, rows = Math.ceil(8 / cols);
  const cw = w / cols, ch = h / rows;
  const pc = ease(phase("curves")), pv = ease(phase("votes"));
  for (let m = 0; m < METRICS.length; m++) {
    const ox = (m % cols) * cw, oy = Math.floor(m / cols) * ch;
    const px0 = ox + 30, px1 = ox + cw - 8, py0 = oy + 18, py1 = oy + ch - 18;
    const deciding = M.decides[m];
    const all = [M.typical[m], s.curves[m], ...s.member_curves.map(c => c[m])];
    const values = all.flatMap(c => [lineAt(c, XMIN), lineAt(c, XMAX), ...c]).concat([D.targets[m]]);
    let lo = Math.min(...values), hi = Math.max(...values);
    const padding = (hi - lo) * 0.06 || 1; lo -= padding; hi += padding;
    const X = crf => px0 + (px1 - px0) * (crf - XMIN) / (XMAX - XMIN);
    const Y = v => py1 - (py1 - py0) * (v - lo) / (hi - lo);
    ctx.globalAlpha = deciding ? 1 : 0.45;
    text(ctx, METRICS[m] + (m === BANDING ? " · holds raises back" : deciding ? "" : " · not deciding"), ox + 6, oy + 8, "#c9d4e0", 11.5, "left", 600);
    text(ctx, UNITS[m], ox + cw - 8, oy + 8, "#6b7889", 10, "right");
    ctx.strokeStyle = "#263040"; ctx.lineWidth = 1; ctx.strokeRect(px0, py0, px1 - px0, py1 - py0);
    [10, 18, 26, 34].forEach(k => text(ctx, k, X(k), py1 + 9, "#6b7889", 9.5, "center"));
    text(ctx, num(hi - padding, 1), px0 - 3, py0 + 4, "#6b7889", 9.5, "right");
    text(ctx, num(lo + padding, 1), px0 - 3, py1 - 4, "#6b7889", 9.5, "right");
    ctx.save(); ctx.beginPath(); ctx.rect(px0, py0, px1 - px0, py1 - py0); ctx.clip();
    // anchor and target
    ctx.strokeStyle = "#4b5869"; ctx.setLineDash([]); ctx.beginPath(); ctx.moveTo(X(HELD), py0); ctx.lineTo(X(HELD), py1); ctx.stroke();
    ctx.strokeStyle = "#fbbf24"; ctx.setLineDash([2, 3]); ctx.beginPath(); ctx.moveTo(px0, Y(D.targets[m])); ctx.lineTo(px1, Y(D.targets[m])); ctx.stroke();
    // typical content
    const path = (c, upto) => { ctx.beginPath(); for (let x = XMIN, first = true; x <= upto; x += 0.25, first = false) first ? ctx.moveTo(X(x), Y(lineAt(c, x))) : ctx.lineTo(X(x), Y(lineAt(c, x))); ctx.stroke(); };
    ctx.strokeStyle = "#8b98a9"; ctx.setLineDash([4, 3]); ctx.lineWidth = 1; path(M.typical[m], XMAX);
    ctx.setLineDash([]);
    if (pc > 0) {
      const upto = XMIN + (XMAX - XMIN) * pc;
      ctx.lineWidth = 1; ctx.strokeStyle = "rgba(103,232,249,0.28)";
      s.member_curves.forEach((c, k) => { if (k !== member) path(c[m], upto); });
      ctx.strokeStyle = "rgba(103,232,249,0.6)"; ctx.lineWidth = 1.3; path(s.member_curves[member][m], upto);
      ctx.strokeStyle = "#67e8f9"; ctx.lineWidth = 2.4; path(s.curves[m], upto);
    }
    if (pv > 0 && m !== 6) {
      // every network's crossing as a dot, the ensemble's as the vote
      MEMBER_VOTES.forEach(votes => {
        ctx.fillStyle = "rgba(103,232,249,0.55)"; ctx.beginPath(); ctx.arc(X(HELD + votes[m] * pv), Y(D.targets[m]), 2, 0, 7); ctx.fill();
      });
      const vote = s.votes[m].offset, x = X(HELD + vote * pv), y = Y(D.targets[m]);
      ctx.strokeStyle = crfColour(ANCHOR + Math.max(-0.5, Math.min(6, vote))); ctx.lineWidth = 2.5;
      ctx.beginPath(); ctx.moveTo(X(HELD), y); ctx.lineTo(x, y); ctx.stroke();
      ctx.fillStyle = ctx.strokeStyle; ctx.beginPath(); ctx.arc(x, y, 4.5, 0, 7); ctx.fill();
      ctx.restore();
      const label = Math.abs(vote) >= 8 ? (vote > 0 ? "≥ +8" : "≤ −8") : fmt(vote, 1);
      text(ctx, label, Math.max(px0 + 16, Math.min(px1 - 16, x)), y + (y > (py0 + py1) / 2 ? -12 : 12), "#e6edf3", 11, "center", 650);
    } else ctx.restore();
    ctx.globalAlpha = 1;
  }
  // the last cell explains the axes
  const ox = (7 % cols) * cw, oy = Math.floor(7 / cols) * ch;
  const lines = ["x: CRF 8–36, grey line: the anchor (" + HELD + ")",
    "Each network draws a curve per metric:", "its level at CRF 22 plus six falling", "steps (softplus), so it can only fall.",
    "Vote = where the curve meets the", "target, minus the anchor. Right = the", "scene has quality to spare."];
  lines.forEach((l, i) => text(ctx, l, ox + 8, oy + 14 + i * 15, i ? "#8b98a9" : "#c9d4e0", 10.5));
}

// ---- votes and the decision ----
const voteCanvas = $("voteCanvas");
function drawVotes() {
  const [ctx, w, h] = fit(voteCanvas);
  ctx.clearRect(0, 0, w, h);
  const s = S[scene], d = s.decision;
  const rows = [
    ...QUALITY.map(m => [METRICS[m], s.votes[m].offset, "votes", QUALITY.indexOf(m) / 4, s.votes[m].spread]),
    ["quality ease (mean)", s.quality_ease, "decide", 0, null],
    ["banding", s.votes[BANDING].offset, "votes", 0.8, s.votes[BANDING].spread],
    ["ease = lower of the two", d.ease, "decide", 0.15, null],
  ];
  const left = 140, right = w - 12, top = 16, rh = (h - top - 22) / rows.length;
  const X = v => left + (right - left) * (v + 8) / 16;
  // axis
  for (let v = -8; v <= 8; v += 2) {
    ctx.strokeStyle = v === 0 ? "#4b5869" : "#1e2633"; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(X(v), top - 4); ctx.lineTo(X(v), h - 18); ctx.stroke();
    text(ctx, v === 0 ? "anchor" : fmt(v, 0), X(v), h - 9, "#6b7889", 9.5, "center");
  }
  // where a detailed scene sits, and how hard a scene must be to be lowered
  const marks = [[M.detail, "detailed", "#2dd4bf"], [M.detail - M.lower_below, "lower below", "#c084fc"]];
  marks.forEach(([v, label, colour]) => {
    ctx.strokeStyle = colour; ctx.setLineDash([3, 3]);
    ctx.beginPath(); ctx.moveTo(X(v), top - 6); ctx.lineTo(X(v), h - 18); ctx.stroke(); ctx.setLineDash([]);
    text(ctx, label, X(v) + 3, top - 9, colour, 9.5);
  });
  rows.forEach(([name, value, ph, delay, spread], r) => {
    const y = top + rh * (r + 0.5);
    const strong = name.startsWith("quality") || name.startsWith("ease");
    text(ctx, name, 6, y, strong ? "#e6edf3" : "#a9b6c6", 11, "left", strong ? 650 : 400);
    const p = ease(clamp01((phase(ph) - delay * 0.6) / 0.55));
    if (p <= 0) return;
    const v = value * p, colour = crfColour(ANCHOR + Math.max(-0.5, Math.min(6, M.gain * (value - M.detail))));
    ctx.fillStyle = colour;
    const bh = Math.min(10, rh * 0.5);
    ctx.fillRect(Math.min(X(0), X(v)), y - bh / 2, Math.abs(X(v) - X(0)), bh);
    if (spread && p >= 1) {
      ctx.strokeStyle = "rgba(230,237,243,0.6)"; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(X(Math.max(-8, value - spread)), y); ctx.lineTo(X(Math.min(8, value + spread)), y); ctx.stroke();
    }
    text(ctx, Math.abs(value) >= 8 ? (value > 0 ? "≥ +8" : "≤ −8") : fmt(value, 2), X(v) + (v >= 0 ? 6 : -6), y, "#e6edf3", 10.5, v >= 0 ? "left" : "right", 600);
  });
}

function equation() {
  const s = S[scene], d = s.decision, p = phase("decide");
  const lines = [];
  const b = s.votes[BANDING].offset;
  lines.push(d.banding
    ? `banding (${fmt(b)}) is below the quality ease (${fmt(s.quality_ease)}), so <b>ease = ${fmt(d.ease)}</b>: banding holds the raise back`
    : `banding (${Math.abs(b) >= 8 ? "≥ +8" : fmt(b)}) does not hold it back, so <b>ease = quality ease = ${fmt(d.ease)}</b>`);
  if (s.measured === 0) {
    lines.push(`this scene was too short to measure, so it stays at the anchor`);
  } else if (d.ease > M.detail) {
    lines.push(`easier than a detailed scene (${num(M.detail)}): raise = <code>gain × (ease − detail)</code> = ${M.gain} × (${num(d.ease)} − (${num(M.detail)})) = <b>${fmt(d.offset)}</b>`);
  } else if (s.quality_ease < M.detail - M.lower_below) {
    lines.push(`far harder than a detailed scene (below ${num(M.detail - M.lower_below)}): lowered by <b>${num(M.max_lower, 1)}</b>`);
  } else {
    lines.push(`about as hard as a detailed scene: <b>no change</b>`);
  }
  const other = s.contributions.slice(1).filter(c => Math.abs(c.delta) > 1e-9);
  const extra = other.map(c => ` ${fmt(c.delta)} (${c.factor})`).join("");
  const conf = s.confidence < 1
    ? `× confidence ${num(s.confidence)}${s.cuts_inside ? " — a cut inside the analysed frames, so the raise is not trusted" : ""}`
    : `× confidence 1 (the analysed frames were one shot)`;
  lines.push(`${extra ? "plus" + extra + " " : ""}${s.raw_adjustment > 0 ? conf : ""}${s.raw_adjustment > 0 ? ", " : ""}held to ${fmt(-M.max_lower, 1)}…${fmt(M.max_raise, 0)}: <b>${fmt(s.adjustment)}</b>`);
  lines.push(`<span class="result">CRF = ${ANCHOR} ${s.adjustment >= 0 ? "+" : "−"} ${Math.abs(s.adjustment).toFixed(2)} → <span style="color:${crfColour(s.crf)}">${s.crf.toFixed(1)}</span></span> <span class="meta">(to the 0.5 step)</span>${s.check ? "" : ' <span class="warn">recomputed differently from the analysis</span>'}`);
  const html = lines.map((l, i) => `<div class="${p > i / lines.length + 0.02 ? "on" : ""}">${l}</div>`).join("");
  if ($("eq").innerHTML !== html) $("eq").innerHTML = html;
}

// ---- timeline ----
const timeCanvas = $("timeCanvas");
const first = D.range.start, last = S.length ? S[S.length - 1].start + S[S.length - 1].frames : first + 1;
const crfLo = Math.min(ANCHOR - 1, ...S.map(s => s.crf)) , crfHi = Math.max(ANCHOR + 4, ...S.map(s => s.crf + 0.5));
function decided(j) { return j < scene || (j === scene && phase("decide") > 0.85); }
function drawTimeline() {
  const [ctx, w, h] = fit(timeCanvas);
  ctx.clearRect(0, 0, w, h);
  const left = 44, right = w - 10, top = 8, bottom = h - 20;
  const X = f => left + (right - left) * (f - first) / (last - first), Y = c => bottom - (bottom - top) * (c - crfLo) / (crfHi - crfLo);
  for (let c = Math.ceil(crfLo); c <= crfHi; c++) {
    ctx.strokeStyle = c === ANCHOR ? "#4b5869" : "#1e2633"; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(left, Y(c)); ctx.lineTo(right, Y(c)); ctx.stroke();
    text(ctx, c === ANCHOR ? `${c} anchor` : String(c), left - 4, Y(c), c === ANCHOR ? "#c9d4e0" : "#6b7889", 10, "right");
  }
  const fps = D.source.fps || 24;
  for (let s = Math.ceil(first / fps / 10) * 10; s * fps <= last; s += 10)
    text(ctx, `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`, X(s * fps), h - 7, "#6b7889", 9.5, "center");
  S.forEach((s, j) => {
    const x0 = X(s.start), x1 = X(s.start + s.frames) - 1;
    if (j === scene) { ctx.fillStyle = "rgba(103,232,249,0.08)"; ctx.fillRect(x0, top, x1 - x0, bottom - top); ctx.strokeStyle = "#67e8f9"; ctx.strokeRect(x0 + 0.5, top + 0.5, x1 - x0 - 1, bottom - top - 1); }
    if (!decided(j)) { ctx.fillStyle = "#222b38"; ctx.fillRect(x0, Y(ANCHOR) - 1, x1 - x0, 2); return; }
    const y = Y(s.crf), ya = Y(ANCHOR);
    ctx.fillStyle = crfColour(s.crf, 0.9);
    if (Math.abs(y - ya) < 3) ctx.fillRect(x0, ya - 2, x1 - x0, 4); else ctx.fillRect(x0, Math.min(y, ya), x1 - x0, Math.abs(ya - y));
    if (s.kind === "cut" || s.kind === "start") { ctx.fillStyle = "#6b7889"; ctx.fillRect(x0, bottom, 1, 4); }
  });
  const done = S.filter((_, j) => decided(j));
  if (done.length) {
    const weighted = done.reduce((a, s) => a + s.crf * s.frames, 0) / done.reduce((a, s) => a + s.frames, 0);
    const raised = done.filter(s => s.crf > ANCHOR).length, lowered = done.filter(s => s.crf < ANCHOR).length;
    $("stats").textContent = `${done.length} of ${S.length} scenes decided · frame-weighted mean CRF ${weighted.toFixed(2)} · ${raised} raised · ${done.length - raised - lowered} at the anchor · ${lowered} lowered`;
  } else $("stats").textContent = "";
  timeCanvas.onclick = e => {
    const r = timeCanvas.getBoundingClientRect(), f = first + (e.clientX - r.left - left) / (right - left) * (last - first);
    const j = S.findIndex(s => f >= s.start && f < s.start + s.frames);
    if (j >= 0) go(j);
  };
}

// ---- scene panel ----
const KINDS = { start: "the first scene of the stretch", cut: "a new shot (cut detected)", split: `a long shot, split every ${D.scene_config.max_scene_frames} frames at most` };
function sceneInfo() {
  const s = S[scene], shown = phase("decide") > 0.85;
  $("thumb").src = FRAMES[scene] || ""; $("thumb").style.visibility = FRAMES[scene] ? "visible" : "hidden";
  $("frameTitle").textContent = `frames ${s.start}–${s.start + s.frames - 1}`;
  $("sceneInfo").innerHTML =
    `<div class="big" style="color:${shown ? crfColour(s.crf) : "#4b5869"}">${shown ? "CRF " + s.crf.toFixed(1) : "CRF …"}</div>` +
    `<div style="margin-top:4px"><span class="kind">${s.kind}</span>: ${KINDS[s.kind] || s.kind}</div>` +
    `<div>${s.frames} frames (${(s.frames / (D.source.fps || 24)).toFixed(1)} s); the model reads its first ${s.frames_analyzed}</div>` +
    (s.cuts_inside ? `<div class="warn">a cut falls inside those frames</div>` : "");
}

// ---- driving it ----
function go(j) { scene = Math.max(0, Math.min(S.length - 1, j)); t = 0; contributions(); $("scrub").value = scene; $("sceneCount").textContent = `scene ${scene + 1} / ${S.length}`; }
function frame(stamp) {
  if (lastStamp !== null && playing) {
    t += Math.min(0.1, (stamp - lastStamp) / 1000) * speed;
    if (t >= T.end) { if (scene < S.length - 1) go(scene + 1); else { t = T.end; playing = false; $("play").textContent = "▶"; } }
  }
  lastStamp = stamp;
  const current = PHASES.find(([name]) => t < T[name][1]) || PHASES[PHASES.length - 1];
  $("phase").innerHTML = `${PHASES.indexOf(current) + 1}/7 · ${current[1]}`;
  drawNetwork(); drawMembers(); drawCurves(); drawVotes(); equation(); drawTimeline(); sceneInfo();
  requestAnimationFrame(frame);
}
$("play").onclick = () => { playing = !playing; if (playing && scene === S.length - 1 && t >= T.end) go(0); $("play").textContent = playing ? "❚❚" : "▶"; };
$("prev").onclick = () => go(t > 1.5 ? scene : scene - 1);
$("next").onclick = () => go(scene + 1);
$("speed").onchange = e => { speed = parseFloat(e.target.value); };
$("scrub").max = S.length - 1;
$("scrub").oninput = e => go(parseInt(e.target.value));
document.addEventListener("keydown", e => {
  if (e.key === " ") { e.preventDefault(); $("play").click(); }
  else if (e.key === "ArrowRight") go(scene + 1);
  else if (e.key === "ArrowLeft") $("prev").click();
  else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
    const select = $("speed"); select.selectedIndex = Math.max(0, Math.min(select.options.length - 1, select.selectedIndex + (e.key === "ArrowUp" ? 1 : -1)));
    select.onchange({ target: select }); e.preventDefault();
  }
});
netCanvas.addEventListener("mousemove", e => {
  const r = netCanvas.getBoundingClientRect(), x = e.clientX - r.left, y = e.clientY - r.top;
  let best = null, dist = 12;
  NODES.forEach(n => { const d = Math.hypot(n.x - x, n.y - y); if (d < dist) { dist = d; best = n; } });
  const tip = $("tip");
  if (!best) { tip.style.display = "none"; return; }
  const s = S[scene];
  let html;
  if (best.c === 0) html = `<b>${M.inputs[best.i]}</b><br>measured ${num(s.inputs[best.i], 3)}<br>standardised ${fmt(best.v, 2)} σ`;
  else if (best.c < 3) html = `<b>hidden layer ${best.c}, unit ${best.i + 1}</b><br>output ${fmt(best.v, 3)}`;
  else {
    const m = Math.floor(best.i / 7), k = best.i % 7;
    html = `<b>${METRICS[m]}</b>, ${k === 3 ? "level at CRF 22" : k < 3 ? `rise from CRF ${K[k + 1]} to ${K[k]}` : `fall from CRF ${K[k - 1]} to ${K[k]}`}<br>raw output ${fmt(best.v, 3)}<br>curve at CRF ${K[k]}: ${num(s.member_curves[member][m][k], 2)} ${UNITS[m]}`;
  }
  tip.innerHTML = html; tip.style.display = "block";
  tip.style.left = Math.min(window.innerWidth - 310, e.clientX + 14) + "px"; tip.style.top = (e.clientY + 12) + "px";
});
netCanvas.addEventListener("mouseleave", () => { $("tip").style.display = "none"; });

$("meta").textContent = `${D.title} · ${D.source.width}×${D.source.height} · model ${M.version} · ${M.members.length} networks · anchor ${ANCHOR}`;
$("netTitle").textContent = `network ${member + 1} of ${M.members.length}`;
go(0);
requestAnimationFrame(frame);
</script>
</body>
</html>
"""

if __name__ == "__main__":
    main()
