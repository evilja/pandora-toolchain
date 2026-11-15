# Caramel's model: the lab, training, and the research worker

[Caramel Analysis](TOOLS.md#pnmpeg-caramel-analysis) gives every scene its own CRF from the source
alone. What it decides with is a **model**, and this file is about where a model comes from: how
scenes are measured (`pnmpeg --caramel-lab`), how a model is trained on the measurements and judged
(`pnmpeg --caramel-train`), how a machine keeps doing both in its idle time (the research worker in
`pndc`), and the files all of it leaves under `DB/caramel/`. The model itself — its inputs, its
curves, how votes become a CRF — is described in `caramel-kernel/README.md`.

Nothing here runs unless it is asked to. A deployment that never sets `caramel_research` and never
runs the two commands decides scenes with the model built into the binary, and only when a preset
or a job asks for Caramel Analysis at all.

## The pieces

| Where | What |
|---|---|
| `caramel-kernel/src/inputs.rs` | The 27 numbers a scene is described by, from the features the decider already measures. |
| `caramel-kernel/src/nn.rs` | A small dense network (SiLU), its gradients and Adam. No dependencies. |
| `caramel-kernel/src/curve.rs` | Quality and size as curves over CRF, the votes read from them, the consensus, and `Model`: an ensemble of networks with its text file format, the built-in model (`builtin.model`) and the installed one. |
| `caramel-kernel/src/train.rs` | Training by series-wise folds, the held-out error, the margin, the simulated policy, and the gate a retrained model has to pass. |
| `src/lib/mpeg/caramel_lab.rs` | The lab: picks scenes, encodes and measures them with ffmpeg, appends `LabSample`s to the dataset. |
| `src/lib/mpeg/caramel_train.rs` | Models on disk (`install`, `activate`, `installed_version`) and `train_dataset`, which trains a candidate and writes the report. |
| `src/pnworker/research.rs` | The idle-time worker: finds sources, runs the lab and the trainer as low-priority children, stops for the queue. |
| `pnx264/src/scenes.rs` | `load_model`: a chunk encoder decides with the model its planner named, read from `DB/caramel/models/`. |

## Which model decides

In order: the model a process **installed** (`caramel_train::activate()`, from `DB/caramel/active`:
`pnmpeg` calls it when it starts, `pndc` before each analysis it runs itself), else the model **built into** the kernel
(`caramel-kernel/src/builtin.model`, trained before the build), else — a build whose file holds no
model — the linear predictors of `caramel-scene-6`. `caramel_kernel::model_version()` names
whichever it is, and reports and `GET /api/v1/caramel/model` carry that name.

A chunked encode fixes its model when it is planned: the planner writes `model=<version>` into
`parallel-aot/caramel.context`, and every chunk child loads exactly that version. A model installed
while an encode is running therefore changes nothing in it — its remaining chunks, and chunks
re-encoded after a restart, decide as the first ones did. A context without the key (written by an
older build) means the linear predictors, and so does a version that names no model the chunk can
find — the active one, the built-in one, or a file under `DB/caramel/models/`.

## The lab: `pnmpeg --caramel-lab`

```
pnmpeg --caramel-lab --preset standard -i <video> -o DB/caramel/dataset/<name>.jsonl
       [--lab-scenes 40] [--lab-crfs 11,15,19,23,27,31] [--lab-points 7] [--lab-workers 3]
       [--lab-source <id>] [--lab-series <name>] [--cancelfile <path>]
```

The input is analysed as `--caramel-report` analyses it, then some of its scenes are encoded at
several CRFs and measured against the source. One scene is one line of the output file.

- **What is encoded.** The scene's frames, up to two seconds (48 frames; scenes under 24 frames
  are not taken), cut out of the source once, losslessly. Video only: no subtitles, no audio, no
  scaling, no watermark. Each encode is libx264 with the named preset's `preset`, `tune` and
  `x264-params`, as `--caramel-verify` runs it. A source deeper than 8 bits is kept and compared at
  10, so the encoder is not marked down for dropping a conversion's dither.
- **What is measured.** Per encode: bytes, XPSNR (pooled, and the worst tenth of the frames),
  SSIM in dB, VMAF (mean and worst tenth) and CAMBI (mean and worst tenth), all over the frames
  after the clip's first, which is a keyframe the whole encode would not have there. CAMBI is also
  taken on the source's own frames, so what a model learns is the banding an encode *adds*. The
  ffmpeg must have `libvmaf` and `xpsnr`; a build without them fails the lab rather than writing
  samples with holes.
- **Which scenes.** Half are taken at even steps through the source. The other half are the scenes
  least like anything already in the dataset — this source's file and every other `.jsonl` beside
  it — weighted by how unsure the model in use is about them. Scenes of this source already in the
  output file are never measured twice, so a lab that was stopped carries on where it was.
- **Which CRFs.** With `--lab-crfs`, exactly those. Without, a search between CRF 10 and 32: three
  encodes to start (14, 21, 28), then the gap where the measured curve bends or crosses the quality
  the model aims at is split, up to `--lab-points` encodes. A scene whose first three points land
  where the model said they would gets no more.
- **Exit code.** 0 done, 3 stopped by the cancel file, 1 failed. The last line of standard output
  is a JSON summary (`eligible`, `already`, `measured`, `failed`, `points`, `cancelled`).

`--lab-series` names what the source's episodes share. It defaults to the file name with the
group, episode number, resolution and checksum taken off (`[Group] Some Show S2 - 07 (1080p)
[ABCD1234].mkv` → `some show s2`; `Some Show S02E07.mkv` gives the same). It matters: training holds a whole series out at a time, and two
episodes filed under different names would let a network be tested on a show it trained on.

### The dataset

`DB/caramel/dataset/*.jsonl`, one `LabSample` per line (`record: 1`): `source_id`, `series`, the
probed `source` (size, fps, bitrate, codec, bit depth), `encoder` (`x264|<preset>|<tune>|<sorted
x264 params>`), `start_frame`, `scene_frames`, `frames`, the decider's `features` for the scene's
opening, `cut_pairs_within`, `reference_bit_depth`, `cambi_source`, `cambi_source_high`, `points`
(`crf`, `bytes`, `xpsnr`, `xpsnr_low`, `ssim_db`, `vmaf`, `vmaf_low`, `cambi`, `cambi_high`),
`picked` (`spread` or `novel`), the guiding `model` and its `surprise`, `collected_unix`,
`elapsed_ms`. Files are append-only and a line that does not parse is skipped. The dataset holds
numbers, never video.

Samples made with different encoder settings are different datasets: a model is trained for one
`encoder` key and the others are left out and counted.

## Training: `pnmpeg --caramel-train`

```
pnmpeg --caramel-train -i DB/caramel/dataset -o <candidate.model | ->
       [--preset <name>] [--train-install] [--train-refit] [--train-seed 1]
       [--train-hidden 32,32] [--train-networks 2] [--train-scenes <file>]
```

Trains a candidate on every sample of one encoder key — the named preset's, or the one most
samples were made with — prints a JSON report, and exits 0 when the candidate may replace the model
in use, 4 when it may not, 1 on an error. `--train-install` installs an accepted candidate.

- **What is fitted.** For each scene a network draws seven curves over CRF — XPSNR, its worst
  frames, SSIM, VMAF, its worst frames, banding added, and log bits per pixel — and each curve is
  pulled towards that scene's readings at the CRFs it was encoded at (Huber loss, in each metric's
  own unit). Curves are monotonic by construction, so quality cannot be learned to rise with CRF.
- **Folds.** Series are dealt into five folds by a hash of their names. Each network trains on four
  and stops when its error on the fifth stops falling, so every scene has networks that never saw
  its series; the default is two networks per fold, ten in the model. What those say about the
  scene is what the model would say about a source it has never seen, and every figure in the
  report is taken from those held-out answers, not from the fit.
- **Targets.** A scene's vote on a metric is how far its CRF can move from the anchor and still
  score what *typical content* scores at the anchor. Typical content is the median over series of
  each series' median scene. A first training takes it from the dataset; a retrain keeps the
  installed model's, so the quality being held does not drift with whatever was measured last.
  `--train-refit` takes it from the dataset again.
- **Margin.** A scene is raised by the smallest vote among the deciding metrics, less each vote's
  disagreement between networks, less a margin. The trainer picks the smallest margin between 1
  and 3 CRF at which, on held-out scenes at anchors 17, 19, 21 and 23, at most 10% of raises go
  more than half a CRF past what the encodes allowed and the raised scenes lose no more than one
  unit (0.5 dB of XPSNR) against the target at the 95th percentile.
- **The gate.** With a model in use, candidate and current are both scored on the scenes measured
  *since the current one was trained* — which it has never seen, and which the candidate answers
  with its held-out networks. The candidate is accepted when there are at least 40 such scenes and
  its mean vote error over the deciding metrics is no more than 0.05 CRF above the current one's.
  With no model in use, a first model needs 200 scenes from 5 series and held-out raises that stay
  within the margin's own limits.
- **The report.** `version`, `encoder`, `scenes`, `series`, `other_encoders`, `members`, `epochs`,
  `margin`, `error` and per-metric `metrics` (held-out vote error in CRF, and the typical curve),
  `policy` (what the candidate would have decided for held-out scenes: shares raised and lowered,
  bytes against the fixed anchor, raises `too_far`, `shortfall_mean`/`shortfall_p95` in units,
  `banding_p95`, and `needed` — the share of lowerings whose scene really was short of the target),
  `predictors` (the same for the linear predictors, on the same scenes and targets), `current`,
  `fresh_scenes`, `current_error`, `candidate_error`, `accepted`, `reason`, `installed`.

`--train-scenes <file>` writes what the report's figures are sums of: one JSON line per scene and
anchor with the `votes` and `spreads` of the networks that never trained on the scene's series,
what its encodes `allowed`, per metric in the report's order, and the `offset` the candidate would
have applied. It is where to look when a series is judged badly. `--train-hidden` sets the width
of each hidden layer and `--train-networks` how many networks are trained per fold.

A candidate is named `caramel-neural-1.<scenes>-<hash>`: the family (it changes when the inputs,
metrics or curve shape do), how many scenes it was trained on, and six hex digits of its weights.
Training is deterministic for a dataset and a seed, and takes seconds.

### Installed models

`caramel_train::install` writes `DB/caramel/models/<version>.model` and then `DB/caramel/active`
(one line: the version), each to a scratch name and renamed, so a reader never sees half a file.
The newest eight other models are kept. To go back, put an older version's name in
`DB/caramel/active`; to return to the built-in model, delete that file. Nothing has to be restarted:
a running `pndc` reports the new version at once (`installed_version`), its next `pnmpeg` children
decide with it, and so does its next Caramel Analysis job.

A model file is text: a `caramel-model 1` header, then `version`, `encoder`, `margin`, `scenes`,
`sources`, `trained` (the newest `collected_unix` it saw), `input-mean`, `input-scale`, `offsets`,
`error`, seven `typical <metric>` lines, and per network `member <layers>` followed by `layer
<inputs> <outputs>` and its weights and biases. Unknown keys are skipped; a file whose shapes do not
match this build's inputs and metrics is refused and the built-in model stays.

## The research worker

`pndc` runs it, except as a Mini node or an orchestrator. It is **off** unless `env.pandora` says:

| Key | Default | Meaning |
|---|---|---|
| `caramel_research` | off | `on` turns the worker on. Read every 20 s, so it is switched without a restart. |
| `caramel_research_feed` | — | An RSS feed (nyaa-style, with `nyaa:size` and `nyaa:seeders`) to find sources on. Without it nothing is ever downloaded. |
| `caramel_research_scenes` | 40 | Scenes one source gives (4–400). |
| `caramel_research_retrain` | 200 | New scenes between two trainings (at least 20). |
| `caramel_research_preset` | `standard` | The preset whose encoder settings are measured. |

What it does, once the queue has been empty for five minutes and no encode holds
`DB/work/.foreground-encode`:

1. If the dataset has grown by `caramel_research_retrain` scenes since the last training, run
   `pnmpeg --caramel-train --train-install`. The gate above decides whether anything changes.
2. Otherwise take the next source and run `pnmpeg --caramel-lab` on it into
   `DB/caramel/dataset/<slug>.jsonl`. Sources, in order: files under `DB/caramel/sources/` (any
   depth to four; `.mkv`, `.mp4`, `.m2ts`, `.webm` of 37 MB or more), then
   `DB/caramel/sources/fetched/`, then the inputs of past jobs still in `DB/cache/inputs`.
3. Otherwise, with a feed, read it (at most every 30 minutes), pick one item, download it with
   `pnp2p` into `DB/caramel/sources/fetched/<key>/`, and measure it next round. An item is taken
   only if it is a single 1080p release of 150 MB to 4 GB with five or more seeders, its link is an
   `https://….torrent` that passes the same fetch-URL checks as a job's, and it was not tried
   before. A download gets 45 minutes. A fetched source is deleted as soon as it has been measured,
   or given up on.

**Deduplication.** A source is done once (`research.json` remembers it by id, whatever came of it),
and a series gives at most two sources: counting both what the dataset holds and what the worker
has finished. The fortieth episode of one show teaches a model nothing the second did not. Within
a source the lab's own scene selection avoids scenes like those already measured.

**A release always comes first.** The lab and the trainer run under `nice -n 19`. The worker checks
the queue every two seconds while a child runs; when a job is queued it writes the child's cancel
file, waits up to 30 s, kills it, and removes the lab's scratch directory
(`DB/caramel/dataset/.caramel-lab-<slug>/`). A scene that was half measured is measured again
later; finished scenes are already in the dataset. The worker never touches a job, the queue or
Discord, and holds no slot.

State is `DB/caramel/research.json`: `sources` (id → series, time, `measured` / `failed`),
`feed_seen`, `feed_read_unix`, `trained_scenes`, `last_training` (the trainer's report without the
per-metric tables), `activity` and `updated_unix`. Child logs are in `DB/caramel/log/`.
`GET /api/v1/caramel/model` returns the same as `research`: whether it is on, what it is doing,
scenes and series in the dataset, and what the last training said.

## Files

| Path | Written by | What |
|---|---|---|
| `DB/caramel/dataset/*.jsonl` | the lab | Measured scenes. Safe to copy between machines and to merge. |
| `DB/caramel/dataset/.caramel-lab-*/` | the lab | Scratch clips of a running lab; removed when it ends. |
| `DB/caramel/models/<version>.model` | `install` | Installed models, the active one and up to eight before it. |
| `DB/caramel/active` | `install` | The version new encodes decide with. Absent: the built-in model. |
| `DB/caramel/research.json` | the research worker | Its state and last training report. |
| `DB/caramel/sources/` | the operator | Videos to measure. `fetched/` below it belongs to the worker. |
| `DB/caramel/log/` | the research worker | Logs and cancel files of its children. |
