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
| `caramel-kernel/src/curve.rs` | Quality and size as curves over CRF, the votes read from them, the rule that decides a scene from them (`decide`), and `Model`: an ensemble of networks with its text file format, the built-in model (`builtin.model`) and the installed one. |
| `caramel-kernel/src/train.rs` | Training by series-wise folds, the held-out error, the detailed scene and the gain, the simulated policy, and the gate a retrained model has to pass. |
| `src/lib/mpeg/caramel_lab.rs` | The lab: picks scenes, encodes and measures them with ffmpeg, appends `LabSample`s to the dataset. |
| `src/lib/mpeg/caramel_train.rs` | Models on disk (`install`, `activate`, `installed_version`) and `train_dataset`, which trains a candidate and writes the report. |
| `src/pnworker/research.rs` | The idle-time worker: finds sources, runs the lab and the trainer as low-priority children, stops for the queue. |
| `caramel-kernel/src/probe.rs` | `Probe`: what a test encode of a far raise is held to, and `settle`, which pulls the raise back from what the encode measured. |
| `caramel-kernel/src/xpsnr.rs` | XPSNR computed exactly as ffmpeg's `xpsnr` filter computes it, for encoders that measure without ffmpeg. |
| `pnx264/src/scenes.rs` | `load_model`: a chunk encoder decides with the model its planner named, read from `DB/caramel/models/`. `SceneSchedule` runs the test encodes (`probe_xpsnr`), in chunk encoders and in the whole-file analysis alike. |
| `examples/caramel_explain.rs`, `scripts/caramel-viz.py` | Watching the model decide: see [Seeing a decision](#seeing-a-decision). |

## Which model decides

In order: the model a process **installed** (`caramel_train::activate()`, from `DB/caramel/active`:
`pnmpeg` calls it when it starts, `pndc` before each analysis it runs itself), else the model **built into** the kernel
(`caramel-kernel/src/builtin.model`, trained before the build), else — a build whose file holds no
model — the linear predictors of `caramel-scene-6`. `caramel_kernel::model_version()` names
whichever it is, and reports and `GET /api/v1/caramel/model` carry that name.

The built-in model is `caramel-neural-1.784-365446`: 784 scenes measured with the `standard`
preset's x264 settings (`fast`, `aq-mode=3:aq-strength=0.8`) on an Apple M4 — 608 from 18 anime
series and 176 from open films and test sequences (Morevna, Pepper & Carrot, Sita Sings the Blues,
Sintel, Tears of Steel, the Derf collection) — with detail −2.49 and gain 0.75. The open material
is there on purpose: a model trained on the anime alone predicted held-out anime scenes worse
(2.64 CRF of vote error against 2.56) and was refused by the gate. Held out by series it raises 56%
of scenes (one in six by 4 CRF or more), lowered 2% under the earlier lowering threshold (far fewer
now, see the rule below), and spends 11.5% fewer bytes than the anchor.
On *Ping Pong the Animation* E02 at anchor 17 (24 scenes, `--caramel-verify spread,17,19`), it
came out 19% smaller than CRF 17 and 8% larger than CRF 19. Its mean XPSNR was a little above
CRF 19's and the scenes it kept at 17 scored 0.4–1.1 dB above CRF 19. A raised scene scores below
its own CRF 19 encode by design (by up to 1.1 dB there), and every one still scored at least as
well as the hardest scene it kept at 17.

A retrain on 1296 scenes from 47 series (adding 15 current TV shows, *Mononoke*, *Paranoia
Agent*, *Haibane Renmei* and *Serial Experiments Lain*) voted better on the 512 new scenes — 2.47
CRF against 2.61 — but was refused: its raises there fell 3.03 units below a detailed scene at the
95th percentile, against 2.45 for the model in use. At gain 0.5 it overshoots no more (2.49), but
spends 0.932 of the anchor's bytes on those scenes where the model in use spends 0.914. Full
episodes agreed. On *Ping Pong* it raised
a still, grainy shot to CRF 22 (39.0 dB XPSNR, 3 dB under the hardest scene kept at 17), and on
*Kendeshita* two raised scenes fell 0.4 and 1.1 dB under that mark, where the model in use's
lowest raised scene was 0.2 dB under it. On five episodes both landed about 10–12% above CRF 19's size with mean
XPSNR 0.25–0.35 dB above it.

A chunked encode fixes its model when it is planned: the planner writes `model=<version>` into
`parallel-aot/caramel.context`, and every chunk child loads exactly that version. A model installed
while an encode is running therefore changes nothing in it — its remaining chunks, and chunks
re-encoded after a restart, decide as the first ones did. A context without the key (written by an
older build) means the linear predictors, and so does a version that names no model the chunk can
find — the active one, the built-in one, or a file under `DB/caramel/models/`.

## Test encodes of far raises

The model's votes are about two CRF wrong on a scene it has not seen, most of that between scenes
of one series rather than between series, and a better network has not removed it. The misses
that cost quality are the far raises: a still, grainy shot read as easy and raised by five, which
then falls a dB or more below the hardest scene kept at the anchor. Full-episode verification
found a few in every episode; the lab's held-out scenes found them too.

So a scene the model raises by **3 CRF or more** (`PROBE_FROM`) is checked before the raise is
spent. The encoder encodes the scene's first **24 frames** (`PROBE_FRAMES`, fewer when the scene
is shorter) at the CRF the model chose, with its own x264 settings, and measures their XPSNR
against the frames it was given — the first frame, an IDR in a test encode, left out as the lab
leaves a clip's first frame out. `Probe::settle` then compares that with the **target**: the XPSNR
a detailed scene scores at the anchor, `typical[xpsnr]` at `anchor − detail`, the same line the
gate measures a raise's shortfall against. A test encode at or above the target keeps the raise.
One below it moves the scene's XPSNR curve, as the model drew it, to pass through the measurement,
and takes the CRF where that curve meets the target, rounded down to the 0.5 step and never below
the anchor. A test encode never raises a scene further.

- **Where.** Chunk encoders run it inside `SceneSchedule`, on frames they already hold: the scene's
  frames wait until its first 24 are in (or the next scene has started), so at most about 24 frames
  plus the decider's two are held, instead of 12 plus two. The whole-file analysis behind `zones`
  and `--caramel-report` runs the same schedule over its decoded segments, with the preset's x264
  settings at one thread per segment — but only when the encode writes 8-bit frames at the
  source's own size, since those are the frames it decodes. Lab measurements never probe: the lab
  measures what the model decides.
- **XPSNR without ffmpeg.** Chunk encoders have no ffmpeg graph to measure with, so
  `caramel_kernel::xpsnr` ports ffmpeg's `vf_xpsnr.c` for 8-bit 4:2:0, quirks included. On real
  encodes it agrees with ffmpeg to the four decimals ffmpeg prints, at 640×360, 1280×720, 1920×1080
  and 3840×2160 (the sizes that take the filter's three code paths). The reconstruction comes from
  x264 itself (`pic_out.img`, with `b_full_recon` so every frame is deblocked), so nothing is
  decoded.
- **Why 24 frames.** On 85 lab scenes encoded at CRF 19, 23 and 27, the XPSNR of the first 24
  frames was within 0.5 dB of the whole 48-frame clip's for 81–86% of scenes (median −0.08 dB). The
  first 12 frames — what the decider measures — were within 0.5 dB for only 41–49%, read 0.3–0.4 dB
  low at the median, and missed by up to 6 dB where a scene's opening was unlike the rest of it.
- **Context.** Chunks fix the choice when they are planned, as they fix the model: the context
  carries `probe=1`, and a context without the key decides without test encodes.
- **What it changed.** On the six verification episodes at anchor 17 (9600 frames each, model
  `caramel-neural-1.784-365446`), whether a probed raise fell short depended on the source far more
  than on the size of the raise: 75% of *Ping Pong*'s probed raises and 69% of *Haibane Renmei*'s
  fell short, against 14% of *Kendeshita*'s and 19% of *Tensei Goblin*'s, and raises of 3–4 CRF fell
  short as often (45%) as raises of 4–5 (47%). On *Ping Pong* 228 of 855 scenes were probed and
  pulled back 1.6 CRF on average: the mean CRF went from 18.91 to 18.50, the size from 7.9% to
  12.2% above CRF 19, and the mean XPSNR of the verified clips from 0.04 to 0.29 dB above CRF 19.
  On *Kendeshita* 198 were probed and pulled back 0.12 on average; the other episodes had few far
  raises to check. The test encodes cost 13–16% more frames encoded on the two most-raised
  episodes and 0–6% on the others.
- **What it does not catch.** The worst raised scenes the verification found on those episodes —
  *Ping Pong*'s frame 3314 at CRF 19.5, 1.4 dB under the hardest scene kept at 17, and
  *Kendeshita*'s scenes at 19–19.5 — were raised by 1.5–2.5 CRF, under the threshold. Probing from
  2 CRF would check them: on *Ping Pong* it probed 367 scenes and pulled raises of 2–3 back 1.7 CRF
  on average (mean CRF 18.22); on *Kendeshita* it pulled them back 0.35. Both cost about 25% more
  frames encoded, which is why the threshold stays at 3 for now.

## The lab: `pnmpeg --caramel-lab`

```
pnmpeg --caramel-lab --preset standard -i <video> -o DB/caramel/dataset/<name>.jsonl
       [--lab-scenes 40] [--lab-crfs 11,15,19,23,27,31] [--lab-points 7] [--lab-workers 3]
       [--lab-deband-crfs <crfs>] [--lab-source <id>] [--lab-series <name>] [--cancelfile <path>]
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
- **Deband points.** With `--lab-deband-crfs` (none by default), every scene is also encoded at
  those CRFs with the `deband` treatment: `gradfun=strength=1.2:radius=16` before the encoder, at
  8 bits, and `deblock=-2,-2` after the preset's x264 params. Those points carry
  `treatment: "deband"` and are not part of the scene's curves, so the CRF model is trained on the
  plain encodes alone. They are an experiment, not something the model decides (see
  [Why deband points](#why-deband-points)).
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
(`crf`, `bytes`, `xpsnr`, `xpsnr_low`, `ssim_db`, `vmaf`, `vmaf_low`, `cambi`, `cambi_high`, and
`treatment` on a treated encode only),
`picked` (`spread` or `novel`), the guiding `model` and its `surprise`, `collected_unix`,
`elapsed_ms`. Files are append-only and a line that does not parse is skipped. The dataset holds
numbers, never video.

Samples made with different encoder settings are different datasets: a model is trained for one
`encoder` key and the others are left out and counted.

### Why deband points

x264 adds banding to smooth gradients even at CRF 17 and even from sources with none. On the scenes
where the dataset's encodes added the most banding, the worst tenth of the frames scored 17–25 CAMBI
at CRF 17 and about as much at 22. The lossless 8-bit conversion alone scored 0, so the banding
comes from the encoder, and raising or lowering the CRF barely changes it. Measured on 12 such
scenes against the plain encode at the same CRF (medians):

| Variant | CRF 17: bytes, CAMBI, XPSNR | CRF 22: bytes, CAMBI, XPSNR |
|---|---|---|
| `gradfun` | −3%, −4.3, −0.49 dB | −4%, +0.5, −0.34 dB |
| `gradfun` + `deblock=-2,-2` (`deband`) | −3%, −4.7, −0.34 dB | −3%, +0.5, −0.32 dB |
| `gradfun` + `deblock` + `psy-rd=1.3,0.2` | +8%, −7.0, −0.08 dB | +5%, −0.1, −0.30 dB |
| ffmpeg `deband` at 16 bits, error-diffused to 8 | −7%, +1.1, −2.73 dB | −15%, +0.4, −1.67 dB |
| `psy-rd` + `deblock` without a filter | +18%, −0.3, +0.01 dB | +8%, −0.1, +0.03 dB |

So dither has to be put back in before the encoder, and only at low CRFs does x264 keep it. On
sources that band themselves (CAMBI above 15 before encoding) the filter repaints that banding,
and XPSNR against the source drops by 1.5 to 5 dB: that may look better or worse, and the
fidelity metrics cannot say which. `deblock` and `psy-rd` can change per scene through x264 zones
and reconfiguration and the filter through `enable`; `aq-strength` and the deadzones cannot, as
x264 ignores them in a zone.

Those 12 scenes were picked for their banding. The lab then measured 480 ordinary scenes from 15
series with deband points at CRF 17 and 20, against the plain curve between its neighbouring CRFs
(medians, worst-tenth CAMBI):

| Scenes | CRF 17: bytes, CAMBI, XPSNR | CRF 20: bytes, CAMBI, XPSNR |
|---|---|---|
| all 480 | −1.9%, 0.0, −0.48 dB | −2.1%, +0.1, −0.27 dB |
| plain encode CAMBI 10 or more | −4.4%, −0.8, −0.83 dB | −4.7%, +0.5, −0.47 dB |
| … and a clean source (below 1) | −15%, −4.8, −0.21 dB | −7%, +0.6, −0.25 dB |

The last row is where it works, and 13 of its 17 scenes at CRF 17 come from one show (Paranoia
Agent's Blu-ray and its flat gradients). Elsewhere it costs fidelity for nothing,
and by CRF 20 — where Caramel sends the flat scenes that band — it no longer helps anywhere. So
debanding is a choice for a source, made by whoever encodes it, and not a per-scene decision the
model makes. The lab measures deband points only when `--lab-deband-crfs` asks for them, which
spares two encodes a scene.

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
- **The rule.** The anchor — the preset's `crf` — is the CRF for a *detailed* scene, and a scene
  that is less detailed is raised; one is lowered only rarely and by little. A scene's ease is the
  mean of its quality votes (XPSNR, its worst frames, SSIM, VMAF's worst frames), or banding's vote
  where that is lower, since the flat scenes that are easiest to raise are also the first to band.
  `detail` is the ease of a detailed scene: the quality ease that a quarter of the measured scenes
  fall below, by their own encodes (about 2.5 CRF harder than typical content). A scene easier than
  that is raised by `gain` times the difference, up to 6 CRF. A scene whose quality ease is more
  than 4.5 CRF below `detail` is lowered half a CRF, and never more. The threshold was 3, with a
  whole CRF past 5: grainy Blu-rays (*Paranoia Agent*, *Serial Experiments Lain*) read that hard in
  most of their scenes, and the built-in model lowered 524 of the 822 scenes of a *Paranoia Agent*
  episode (54 of them to 16 from an anchor of 17). At 4.5 it lowers 184 of them, and on held-out
  scenes of the 1296-scene dataset one in five hundred is lowered, each one harder than a detailed
  scene by its own encodes.
  Short windows have their raise scaled down and a window with a cut inside is not raised, as with
  the fitted predictors.
- **Gain.** The trainer picks the largest of 1, 0.75, 0.5 and 0.25 at which, on held-out scenes at
  anchors 17, 19, 21 and 23, the raised scenes fall below a detailed scene's quality at the anchor
  by no more than 2.5 units at the 90th percentile and 3.5 at the 95th (the mean over the quality
  metrics; a unit is 0.5 dB of XPSNR), and the raises add no more than 2 CAMBI at the 95th
  percentile. A raised scene is allowed to look like a detailed one does at the anchor, a little
  worse at its tail, and not to band visibly.
- **The gate.** With a model in use, candidate and current are both scored on the scenes measured
  *since the current one was trained* — which it has never seen, and which the candidate answers
  with its held-out networks. The candidate is accepted when there are at least 40 such scenes and
  its mean vote error over the deciding metrics is no more than 0.05 CRF above the current one's,
  its held-out raises stay within the gain's limits, and its raises on those same scenes fall no
  further below a detailed scene than the current model's: both decide them as they would in use,
  each with its own detailed scene and gain, and the candidate's 95th-percentile shortfall may be at
  most 0.25 units above the current one's. Better votes on average are not enough on their own: a
  model can be closer on most scenes and further out on the few it raises furthest, which is what
  a viewer sees. A candidate that fails only on that is judged again at each smaller gain, and
  kept at the first that passes if its choices there still spend no more bytes on the fresh scenes
  than the current model's — otherwise it is no better a trade than the installed model at a
  smaller gain. With no model in use, a first model needs 200 scenes from 5 series and held-out
  raises within those limits.
- **The report.** `version`, `encoder`, `scenes`, `series`, `other_encoders`, `members`, `epochs`,
  `detail`, `gain`, `error` and per-metric `metrics` (held-out vote error in CRF, and the typical
  curve), `policy` (what the candidate would have decided for held-out scenes: `raised_share`,
  `lowered_share`, `raised_far_share` — raised by 4 CRF or more — `raise_mean`, `bytes` against
  the fixed anchor, `shortfall_p90`/`shortfall_p95` in units below a detailed scene, `banding_p95`
  added by the raise, and `needed` — the share of lowerings whose scene really was harder than a
  detailed one), `predictors` (the same for the linear predictors, on the same scenes), `current`,
  `fresh_scenes`, `current_error`, `candidate_error`, `current_shortfall`, `candidate_shortfall`,
  `current_bytes`, `candidate_bytes` (on the fresh scenes, against the anchor), `accepted`,
  `reason`, `installed`.

`--train-scenes <file>` writes what the report's figures are sums of: one JSON line per scene and
anchor with the `votes` and `spreads` of the networks that never trained on the scene's series,
what its encodes `allowed`, the `levels` the networks' curves read at the anchor, per metric in the
report's order, and the `offset` the candidate would have applied. It is where to look when a series is judged badly. `--train-hidden` sets the width
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

A model file is text: a `caramel-model 1` header, then `version`, `encoder`, `detail`, `gain`, `scenes`,
`sources`, `trained` (the newest `collected_unix` it saw), `input-mean`, `input-scale`, `offsets`,
`error`, seven `typical <metric>` lines, and per network `member <layers>` followed by `layer
<inputs> <outputs>` and its weights and biases. Unknown keys are skipped; a file whose shapes do not
match this build's inputs and metrics is refused and the built-in model stays, as is one with no
`gain` — a model from before scenes were raised by their ease.

## The research worker

`pndc` runs it, except as a Mini node or an orchestrator. It is **off** unless `env.pandora` says:

| Key | Default | Meaning |
|---|---|---|
| `caramel_research` | off | `on` turns the worker on. Read every 20 s, so it is switched without a restart. |
| `caramel_research_feed` | — | An RSS feed (nyaa-style, with `nyaa:size` and `nyaa:seeders`) to find sources on. Without it nothing is ever downloaded. |
| `caramel_research_scenes` | 40 | Scenes one source gives (4–400). |
| `caramel_research_retrain` | 200 | New scenes between two trainings (at least 20). |
| `caramel_research_preset` | `standard` | The preset whose encoder settings are measured. |
| `caramel_research_crfs` | — | Fixed CRFs every scene is encoded at (`11,15,19,23,27,31`). Without it the lab searches each scene's curve. |

All of these but the feed can also be set over the API (`POST /api/v1/caramel/research`, a
privileged token): what a machine downloads on its own is decided on the machine.

What it does, once the queue has been empty for five minutes and no encode holds
`DB/work/.foreground-encode`:

1. If the dataset has grown by `caramel_research_retrain` scenes since the last training, run
   `pnmpeg --caramel-train --train-install --preset <caramel_research_preset>`. The gate above
   decides whether anything changes. Naming the preset trains on the scenes encoded with its
   settings and no others, so a dataset copied in from a machine whose preset differs cannot
   outvote this machine's own measurements.
2. Otherwise take the next source and run `pnmpeg --caramel-lab` on it into
   `DB/caramel/dataset/<slug>.jsonl`. Sources, in order: files under `DB/caramel/sources/` (any
   depth to four; `.mkv`, `.mp4`, `.m2ts`, `.webm` of 37 MB or more), then
   `DB/caramel/sources/fetched/`, then the inputs of past jobs still in `DB/cache/inputs`.
3. Otherwise move the [request queue](#queued-releases) along: measure a release that has been
   downloaded, else download the next one.
4. Otherwise, with a feed, read it (at most every 30 minutes), pick one item, download it with
   `pnp2p` into `DB/caramel/sources/fetched/<key>/`, and measure it next round. An item is taken
   only if it is a single 1080p release of 150 MB to 4 GB with five or more seeders, its link is an
   `https://….torrent` that passes the same fetch-URL checks as a job's, and it was not tried
   before. A download gets 45 minutes. A fetched source is deleted as soon as it has been measured,
   or given up on.

### Queued releases

An operator can name releases for the worker instead of leaving it to a feed, with the series each
one is: `POST /api/v1/caramel/research/sources` with `{ "torrent", "file_index"?, "series" }` (a
privileged token). `torrent` is a `.torrent` link over https, a nyaa view page, or a magnet;
`file_index` picks one file of a batch, and without it the whole torrent is downloaded. The queue is
`DB/caramel/requests.json`; each release is downloaded into
`DB/caramel/sources/requested/<id>/` with `pnp2p` and measured as the series it was queued as, which
matters because a series read from a file name can be wrong, and a wrong one lets a fold test a
network on a show it trained on. Being asked for by name, a release is not held to the two-sources
limit. It moves through `queued`, `downloading`, `downloaded`, `measuring`, and ends `measured` or
`failed`; its directory is deleted once it ends. A download stopped for a job keeps what it has and
resumes next time; one that fails or runs past three hours is tried again, three times in all. The
same file of the same torrent is not queued twice unless the first one failed. `DELETE
/api/v1/caramel/research/sources/:id` takes one off the queue, except while it is downloading or
being measured.

The worker's `pnp2p` is routed as a job's is: through Pandora's torrent proxy (`PNP2P_PROXY`) while
the worker's proxy health check passes, directly while it fails. The `.torrent` file itself is
fetched directly, as a job's `pncurl` fetches it.

**Getting the measurements out.** `GET /api/v1/caramel/dataset` lists the dataset's files (`name`,
`bytes`, `scenes`, `modified`) and `GET /api/v1/caramel/dataset/<name>` returns one, so a machine
that measures in its idle time can feed a training run somewhere else. Both are privileged. Set
`caramel_research_retrain` high on such a machine if it should not train and install models of its
own.

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

## Seeing a decision

`scripts/caramel-viz.py` writes an animated page that walks through a stretch of video one scene at
a time, slowly enough to follow: the scene's frame, its 27 inputs as the networks are given them,
every layer of one network lighting up in turn (with the strongest weighted connections drawn), the
networks of the ensemble side by side, the seven curves the outputs become, the vote each curve
gives where it meets its target, and the arithmetic of `decide` down to the CRF. A timeline below
fills in as scenes are decided.

```bash
python3 scripts/caramel-viz.py <video> --start 4000 --frames 1440 --anchor 17
```

Run it from the checkout whose `DB/` holds the installed model; it decides with the model an encode
started there would. It builds and runs `examples/caramel_explain.rs`, which runs the scene analysis
exactly as `pnmpeg --caramel-report` does, then recomputes every scene's decision from the public
pieces (`inputs`, `Model::standardised`, the layers, `votes_of`, `decide`) and records the
activations, curves and weights along the way. A scene whose recomputed decision differs from the
analysis's own is marked on the page. One frame per scene comes from ffmpeg (`DB/bin` first). The
page is a single HTML file with nothing fetched; `--json` reuses an earlier export, `--no-open`
only writes it.

## Files

| Path | Written by | What |
|---|---|---|
| `DB/caramel/dataset/*.jsonl` | the lab | Measured scenes. Safe to copy between machines and to merge. |
| `DB/caramel/dataset/.caramel-lab-*/` | the lab | Scratch clips of a running lab; removed when it ends. |
| `DB/caramel/models/<version>.model` | `install` | Installed models, the active one and up to eight before it. |
| `DB/caramel/active` | `install` | The version new encodes decide with. Absent: the built-in model. |
| `DB/caramel/research.json` | the research worker | Its state and last training report. |
| `DB/caramel/requests.json` | the API and the research worker | Queued releases and how far each has got. |
| `DB/caramel/sources/` | the operator | Videos to measure. `fetched/` and `requested/` below it belong to the worker. |
| `DB/caramel/log/` | the research worker | Logs and cancel files of its children. |
