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
| `caramel-kernel/src/inputs.rs` | The 27 numbers a scene is described by, from the features the decider already measures, and four more from its fast encode. |
| `caramel-kernel/src/fast.rs` | `FastEncode`: what a scene's fast encode read — its XPSNR, its worst frames, its bits and the banding it added (`added_banding`, which also reads the banding a source is ready to show). |
| `caramel-kernel/src/cambi.rs` | CAMBI, Netflix's banding detector, ported from libvmaf: the score libvmaf's `cambi` feature gives an 8-bit frame at its defaults, so the fast encode's banding reads on the lab's scale without linking libvmaf; and `smoothed`, the frame with its dither wiped out. |
| `caramel-kernel/src/nn.rs` | A small dense network (SiLU), its gradients and Adam. No dependencies. |
| `caramel-kernel/src/curve.rs` | Quality and size as curves over CRF, the votes read from them, the rule that decides a scene from them (`decide`), and `Model`: an ensemble of networks with its text file format, the built-in model (`builtin.model`) and the installed one. |
| `caramel-kernel/src/train.rs` | Training by series-wise folds, the held-out error, the detailed scene and the gain, the simulated policy, and the gate a retrained model has to pass. |
| `src/lib/mpeg/caramel_lab.rs` | The lab: picks scenes, encodes and measures them with ffmpeg, appends `LabSample`s to the dataset. |
| `src/lib/mpeg/caramel_train.rs` | Models on disk (`install`, `activate`, `installed_version`) and `train_dataset`, which trains a candidate and writes the report. |
| `src/pnworker/research.rs` | The idle-time worker: finds sources, runs the lab and the trainer as low-priority children, stops for the queue. |
| `caramel-kernel/src/probe.rs` | `Probe`: what a test encode of a far raise is held to, and `settle`, which pulls the raise back from what the encode measured. `SourceTrust`: how a source's probes have gone, and what that makes of its next raises. |
| `caramel-kernel/src/xpsnr.rs` | XPSNR computed exactly as ffmpeg's `xpsnr` filter computes it, for encoders that measure without ffmpeg. |
| `pnx264/src/scenes.rs` | `load_model`: a chunk encoder decides with the model its planner named, read from `DB/caramel/models/`. `SceneSchedule` runs the test encodes (`probe_xpsnr`) and the fast encodes (`fast_encode`), in chunk encoders and in the whole-file analysis alike. |
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
re-encoded after a restart, decide as the first ones did. With the model goes `gain=<gain>` when
the [gain controller](#the-gain-controller) has the model's raises turned down, so a step it takes
mid-encode changes nothing in that encode either. A context without the key (written by an
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
  frames encoded on every source, which is why the threshold is not 2 everywhere — only on sources
  that have earned it (below).

### Sources whose raises fall short

Since shortness belongs to the source, an encode keeps a record of how its test encodes went and
lets that decide its later raises (`caramel_kernel::SourceTrust`). Each probe records its margin —
measured minus target, in dB. Over the latest **24** probes (`TRUST_WINDOW`), once there are at
least **4** (`TRUST_MIN`), a source whose share of short probes is above **20%** (`TRUST_SHORT`)
is *distrusted*:

- **Its raises shrink before they are probed.** A raise keeps `1 − 0.8 × (share − 0.2)` of itself,
  never less than **0.6** (`TRUST_FLOOR`): 84% at a 40% short share, 60% from Haibane's 69% and
  Ping Pong's 75% on. The CRF is rounded down to the 0.5 step, never below the anchor
  (`SceneDecision::temper`). The decision keeps the model's own CRF as `tempered_from`.
- **Its smaller raises are checked.** Raises of **2 CRF** (`PROBE_LOWEST`) and more are probed
  instead of 3. The kernel attaches a probe to every network raise from 2; the schedule drops the
  ones the source's trust does not call for, so a report never shows a probe that did not run.

A source whose latest probes hold is trusted again as the old ones leave the window. A source that
is never distrusted encodes exactly as before. Without probes (`probe` off, 10-bit or resized
output) there is no record and nothing changes.

The record is shared by everything encoding one source (`pnx264::scenes::TrustShare`). The
segments of a whole-file analysis share it in memory. The chunks of a parallel encode share one
file, `caramel.trust` in the AOT directory when there is one and the scratch directory otherwise,
one margin per line, appended — threads and AOT child processes alike. Chunks run side by side, so
a chunk reads whatever its neighbours have probed by the time it gets to a scene, and a chunk
encoded again after a restart may read more than it did the first time and decide its raises
differently. Both outputs are valid encodes; they are just not bit-identical.

## What an encode keeps of its decisions

Every Caramel encode writes `<output>.caramel-decisions` beside its output
(`pnx264::scenes::EncodeDecisions`): a header `#caramel-decisions model=<version> anchor=<crf>
fps=<rate>`, then one `|`-separated line per scene — start frame (from the source's first frame),
frames, anchor, the CRF encoded, the CRF the model chose, the CRF after the source's trust (empty
if untouched), the probe as `crf/measured/target` (empty if none ran), the fast encode as
`xpsnr/log_bpp`, the deciding factor and the scene's ease, the votes as `metric:offset:spread`
joined by commas, and the cut pairs inside the scene. Chunk encoders write the same lines per
chunk, beside the chunk (`<chunk>.decisions`, starts from the chunk's first frame); the encode
gathers them, and a chunk reused from an older build without one is left out. The zones path writes
them from the whole-file analysis. A file that cannot be written is logged and the encode goes on.

When the research worker is on (`caramel_research`), a job that reaches *Encoded* with such a file
is kept for verification in `DB/caramel/verify/<job id>/`: the file as `decisions`, a hard link to
the job's `input.mkv` (a copy where linking fails), and `entry.json` (`job_id`, `series`,
`kept_unix`, `model`, `scenes`). The series is `verify-` and a hash of the job's first source key,
so every episode of one release is one series and training holds them out together. The inputs
are capped at 40 GiB together, oldest dropped first. A job encoded on a linked node keeps nothing
here: its decisions stay on the node.

## Verifying an encode's decisions

A probe checks the opening frames of a far raise while the encode runs; nothing checks the rest,
and nothing tells the model. Verification does, after the episode is out and while the machine is
idle: `pnmpeg --caramel-lab --lab-verify <decisions>` on the encode's input re-measures its raised
scenes, and the kept scenes that look like savings the model missed, as full lab samples. It fixes
the model, not the episode it verifies — the probe and the source's trust still guard the encode.

- **Which scenes** (`caramel_lab::verify_picks`, at most `--lab-scenes`). The decider caps a scene
  at 48 frames, so one long shot is several scenes; a scene that ran to the cap and the one split
  off after it are one shot, verified once, at the larger raise. Every raised shot, largest raise
  first. Then kept shots (at the anchor) that look doubtful, in this order: *split* — two deciding
  metrics voted a raise of 1 CRF or more and another voted none (a flat gradient held by banding's
  vote is the banding guard working, which only the CAMBI measurement can tell apart from
  caution); *ease* — an ease over a detailed scene of 1 CRF or more that still was not raised;
  *fast* — among the top 10% of the encode's kept shots by fast-encode XPSNR; *spread* — members
  that disagree by 2 CRF or more. Shots lowered below the anchor are not taken. One plain kept shot
  in 20, chosen by its start frame, is a *control*, with a tenth of the count kept for them: every
  other pick was chosen for looking doubtful, so the controls are the fair estimate of savings
  missed. The lab analyses the input itself and its scenes need not start where the encode's chunks
  started them, so a decision is matched to the analysed scene that starts where it does, else to
  the one it starts inside; the usual lab rules then apply (24 frames or more, no cut inside).
- **Which CRFs.** The lab's own (`caramel_research_crfs`, or the search's three starting CRFs), so
  the sample is a whole curve like any other, plus the anchor, the CRF the encode used, and 2 past
  that.
- **The verdict** (`caramel_kernel::train::scene_verdict`), judged as the gate judges a raise, with
  the typical curves and detail of the model that decided the encode: the shortfall at the decided
  CRF against a detailed scene at the anchor (the mean over the quality metrics, in units — half a
  dB of XPSNR is one), and the CAMBI added over the anchor. **`overshot`** when that shortfall is
  over 1 unit (`OVERSHOOT`) or the banding over 2 (`BANDED`); **`missed`** when 2 CRF further
  would still have been short of nothing and banded no more than that; **`held`** otherwise.
- **The sample** is an ordinary lab sample with `picked` = `verify-raise`, `verify-split`,
  `verify-ease`, `verify-fast`, `verify-spread` or `verify-control`, and `verified`: `{ model,
  anchor, decided, verdict }`. The lab's summary line counts the verdicts by `picked`.

### The gain controller

A model's raises are only as good as its last dataset, and a training waits for hundreds of new
scenes. Between trainings, the verdicts on raises turn the model's `gain` (how far a scene's ease
raises it) up or down so the share of overshot raises stays near 10% (`GAIN_TARGET`) on what this
machine actually encodes.

- After each verified encode the research worker adds its `verify-raise` verdicts to
  `DB/caramel/feedback/gain.json` — only those of the model this machine now decides with; a
  verdict on an older model steers nothing.
- Every 100 verified raises (`GAIN_WINDOW`) the scale on the trained gain is multiplied by
  `exp(0.5 × (0.10 − overshoot rate))`: a third of raises overshooting takes about 11% off, none
  overshooting gives 5% back. It stays between 0.5 (`GAIN_FLOOR`) and 1: never past the trained
  gain, which only a training moves.
- A newly installed model starts at its own trained gain.
- `caramel_train::activate` installs the model at the controlled gain, so everything that
  activates the model — an encode, an analysis, the bot — decides with it. A chunked encode
  carries it to its chunks in the context (`gain=`). `pnmpeg --debug` names it in its first line
  (`model … at gain 0.500`) when it is not the trained one. A model given with
  `--caramel-model-file` is used at its trained gain.

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
  is a JSON summary (`eligible`, `already`, `measured`, `failed`, `points`, `cancelled`), or
  `{ "error" }` when it failed.

`--lab-series` names what the source's episodes share. It defaults to the file name with the
group, episode number, resolution and checksum taken off (`[Group] Some Show S2 - 07 (1080p)
[ABCD1234].mkv` → `some show s2`; `Some Show S02E07.mkv` gives the same). It matters: training holds a whole series out at a time, and two
episodes filed under different names would let a network be tested on a show it trained on.

### The dataset

`DB/caramel/dataset/*.jsonl`, one `LabSample` per line (`record: 1`): `source_id`, `series`, the
probed `source` (size, fps, bitrate, codec, bit depth), `encoder` (`x264|<preset>|<tune>|<sorted
x264 params>`), `start_frame`, `scene_frames`, `frames`, the decider's `features` for the scene's
opening (with `fast_encode`, the scene's fast encode; see below), `scene_features` (the same
features over every frame the lab encoded; see below),
`cut_pairs_within`, `reference_bit_depth`, `cambi_source`, `cambi_source_high`, `points`
(`crf`, `bytes`, `xpsnr`, `xpsnr_low`, `ssim_db`, `vmaf`, `vmaf_low`, `cambi`, `cambi_high`, and
`treatment` on a treated encode only),
`picked` (`spread` or `novel`), the guiding `model` and its `surprise`, `collected_unix`,
`elapsed_ms`. Files are append-only and a line that does not parse is skipped. The dataset holds
numbers, never video.

Samples made with different encoder settings are different datasets: a model is trained for one
`encoder` key and the others are left out and counted.

### Whole-scene features

Encoders decide a scene from its first twelve frames, four of them measured by themselves, and the
lab encodes up to 48. Half a second of a shot misses what starts after it: a fade-in reads as an
empty frame (one Akira scene has a spatial detail of 0.9 over its opening and 12.4 over all of it),
and a pan that begins a second in reads as a still. So each sample also keeps `scene_features`,
measured by `caramel_kernel::SceneWindow` over the clip the lab encoded — every pair of frames
compared, every third frame measured, as densely as the opening is. Over the first twelve frames it
is exactly what the decider measures (a kernel test holds them equal). Samples measured before it
was kept have none.

```
pnmpeg --caramel-backfill -i <source> -o <dataset.jsonl> [--lab-source <id>]
```

adds it to the samples of one source that lack it, reading the frames from the source they were
measured from, and rewrites the file once every one was tried; it takes seconds a source. Training
with `--train-features scene` feeds the networks `scene_features` instead, on the samples that have
them, and `--train-features paired` feeds the opening features of the same samples, so the two can
be compared on the same scenes.

### Fast encodes

Features of the source say how complicated a picture is; they do not say how hard x264 finds it.
Of the model's error at anchor 17 (1.94 CRF, measured on series it did not train on), about a fifth
is shared by a whole source and four fifths differ scene to scene, so no per-source correction
helps much. What does is asking the encoder: a scene encoded at `FAST_PRESET` (`veryfast`) and
`FAST_CRF` (20) with the encode's own tune and x264 parameters, on one thread, and read as the lab
reads a clip — XPSNR pooled over its frames with the first left out, the mean of its worst tenth,
and log2 bits per pixel. On 278 scenes of 9 series, a fit that adds those three numbers to the
model's prediction, holding a series out at a time, cut the error from 2.24 to 1.30 CRF at anchor
17 and from 2.36 to 0.94 at 21. Coverage is what matters: the first 24 frames gave 1.50, the first
12 gave 2.01, and half resolution 1.89. An encode at a faster preset was worse, and one at the real
preset was barely better (1.02) for twice the time.

So a scene's fast encode covers all of it, up to `FAST_FRAMES` (48): an encoder deciding with a
model that reads it holds every scene until the scene has ended and encodes it before releasing
any frame, about 3.4 s of one core per 48 frames at 1080p — some 44% on top of the encode itself
at the Standard preset when every scene gets one, under half that with a base (below). A model reads it as inputs 28–31 (`fast-xpsnr`, `fast-xpsnr-low`,
`fast-bits`, `fast-banding`); a model's text says how many inputs its networks read, so models of
27, 30, 31 and 32 inputs all load — a 30-input model is one from before the banding was read, a
31-input one from before the exposed banding (below). Models that do not read it are never slowed
by it.

**Banding.** XPSNR barely sees banding — a flat gradient cut into steps loses almost no signal —
and banding was where most of the fast model's held-out mistakes lay: of the decisions 1.5 CRF or
more off what the scene's encodes allowed, the banding vote was furthest out in 181 of 266 raises
that went too far and 452 of 579 that fell short. Banding *source* features (CAMBI of the source,
gradient and dark-gradient shares) explained almost none of that error; the banding an encode at a
nearby CRF actually added tracked the allowed banding closely (log-scale correlation −0.85). So the
fast encode reads its own: CAMBI (`cambi.rs`, a port of libvmaf's, which agrees with libvmaf to the
fourth decimal) of the encoded and the source frames at a quarter, half and three quarters through
the scene (`fast::banding_frames`), the most the encode showed less the most the source showed
(`fast::added_banding`). Three frames of each is about 1 s of one core per scene at 1080p, on top
of the fast encode's 3.4 s. A probe reads XPSNR alone and pays none of it. On 983 scenes of 31
series, held out a series at a time over three seeds, networks reading it against the same networks
without it took the banding vote's error from 3.29 to 1.05 CRF and the overall error from 1.78 to
1.32; the decisions' shortfall p95 fell from 1.92 to 1.32 and the banding they added (p95) from
1.45 to 1.07, for 2% more bytes (0.944 against 0.926), the raises it no longer makes being the
ones that banded. Most of those scenes carry the stand-in below rather than their own reading.

**Exposed banding.** Banding comes from an encoder spending nothing on the dither or grain that
holds a gradient smooth, and that can be seen without encoding: a 5×5 box mean of the frame,
rounded back to 8 bits (`cambi::smoothed`), loses the dither the same way. Its CAMBI less the
source's, over the same three frames, is the scene's `exposed_banding`. On 88 clips of three
series it moved with the banding a veryfast encode at CRF 20 added at a log-scale correlation of
0.97 (radii 1, 3 and 5 did worse), and on the 64 lab scenes of two series measured both ways at
0.99 with the fast encode's own reading and 0.985 with the lab's banding at CRF 21, against 0.994
for the fast encode's — on the same scale (fast ≈ 1.02 × exposed in logs). It costs three more
CAMBI reads, about 0.3 s a scene, and needs only the source, so it can stand in for the encode's
banding where no encode is made. The lab records it with every sample, `--caramel-backfill`
refills it, and a model reads it as input 32 (`exposed-banding`) — after the fast encode's, so the
older models' inputs stay where they were.

Its use is the base below, which never encodes and was blind to banding: a 32-input model's base
reads the 27 source inputs and this one (`inputs::base_reading_exposed`), and names them in its
file (`input-select`). An encoder deciding with such a model reads it for every scene from the
frames the fast encode would cover (`pnx264::scenes::exposed_banding`), before the base decides
whether the scene gets one, and decides the scene again with it (`SceneJudge::reconsider`); a scene
whose frames are not 8-bit reads as showing none. On 2080 scenes of 64 series measured both ways,
held out a series at a time over eight seeds, against the same build trained on 31 inputs:

| Inputs | Base alone: CAMBI added p95 | Gated: gain | Gated: bytes | Gated: shortfall p90 / p95 | Gated: CAMBI added p95 |
|---|---|---|---|---|---|
| 31 (base reads 27) | 1.50 | 0.75–0.8 | 0.919 | 0.69 / 1.14 | 1.69 |
| 32 (base reads 28) | 1.34 | 0.75–0.85 | 0.908 | 0.75 / 1.26 | 1.87 |

The base holds back the raises that would band, so fewer of the scenes it sends to the fast model
band, and the trainer can give the model a larger gain within the same limits: 1.2% fewer bytes.
The fast model reading it itself, with its base on 27, gained a fifth of that (0.917 against
0.919, three seeds), since it already reads its own encode's banding. A sample measured before it was read
trains on its fast encode's banding in its place, or the lab's stand-in below.

**Only the scenes its base raises.** A model that reads the fast encode carries a *base*: a
27-input model — 28 beside a 32-input one, with the exposed banding — trained beside it on the same scenes, seed and targets (`curve::Model::base`). The
base decides every scene first, from the source alone. A scene it keeps at or below the anchor keeps
that decision and is never encoded fast; a scene it raises is encoded fast and decided again by
the full model (`SceneJudge::wants_fast_encode`). The fast encode earns its time on the raises —
it holds back the ones that would fall short of a detailed scene and lets the safe ones go
further — while a scene the base keeps at the anchor would mostly stay there anyway. On 928 scenes
of 29 series, held out a series at a time, against a fixed anchor:

| Decided by | Encoded fast | Bytes | Mean raise | Shortfall p90 / p95 | CAMBI added p95 |
|---|---|---|---|---|---|
| The built-in 27-input model alone | 0% | 0.944 | 2.51 | 1.93 / 2.75 | 1.40 |
| The base alone | 0% | 0.938 | 1.85 | 1.92 / 2.77 | 1.11 |
| The fast model on every scene | 100% | 0.914 | 3.25 | 1.42 / 1.97 | 1.56 |
| The base, then the fast model on its raises | 43% | 0.928 | 3.46 | 1.43 / 2.01 | 1.57 |

The gate has to be the model's own base: gating on the built-in model instead picked fewer scenes
(33%) and kept their raises safe, but spent as many bytes as the built-in model alone (0.947),
because the scenes it keeps at the anchor are ones the fast model would have raised. Ranking scenes
by how far the networks disagreed picked no better than chance.

A scene without a fast encode — one its base did not raise, or a 10-bit or rescaled output that
the encoder cannot measure at the source's size — is decided by the model's base, or, for a model
without one, by the built-in model; both read the source alone. A model without a base encodes
every scene fast.

The lab makes each sample's fast encode itself over the clip it measured, through pnx264, so
lab and encoder read the same numbers; `--caramel-backfill` adds it, with the whole-scene
features and the exposed banding, to samples measured before, and refills a fast encode made
before it read banding. A
sample whose fast encode has no banding is trained on the banding the lab's own encodes added at
`FAST_CRF` instead (`LabSample::banding_stand_in`: `lab_banding` drawn between the encodes either
side, scaled by `STAND_IN_BANDING`). On 544 scenes of 17 series measured both ways the two moved
together (log-scale correlation 0.89), with the fast encode's about half the lab's, hence the scale
of 0.53. The share is not one number: by series it ran from 0.24 (Cyberpunk Edgerunners) to 1.33
(Patlabor 2), lowest on grainy films and CG, and neither bit depth, grain, gradients nor detail
predicted it on held-out series, so a stand-in is a noisy reading and a real one is worth
re-reading a source for. The 8-bit frames are what both encode, so a 10-bit source
reads lower than an ffmpeg encode scored against the 10-bit picture; on 8-bit sources the two
agree to the hundredth of a dB. `--train-features fast` trains networks reading all 32 inputs (or the first N with
`--train-inputs N`, to compare against a model from before a later input existed: 31 leaves the
exposed banding out of the model and its base) on the samples that have one, with their base
beside them, and judges the two together;
`--train-features fast-paired` trains the 27-input networks alone on the same samples.

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
       [--train-features opening|scene|paired|fast|fast-paired]
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
  Banding's vote is read `banding-margin` CRF below what it says (0 in a model from before the
  margin), so a scene that banding holds back stops short of its banding allowance, while a scene
  its quality holds back is raised as before.
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
- **Gain and banding margin.** For each banding margin of 0, 0.5, 1, 1.5, 2, 3 and 4 CRF, the
  trainer picks the largest gain from 1 down to 0.25 in steps of 0.05 at which, on held-out scenes
  at anchors 17, 19, 21 and 23, the raised scenes fall below a detailed scene's quality at the
  anchor by no more than 2.5 units at the 90th percentile and 1.5 at the 95th (the mean over the
  quality metrics; a unit is 0.5 dB of XPSNR), and the raises add no more than 1.5 CAMBI at the
  95th percentile; of the pairs that hold, it keeps the one that spends the fewest bytes
  (`choose_raise`). A model that reads the source alone — a base, which also decides every scene of
  a scaled or 10-bit encode — may fall 3.5 units short at the 95th percentile instead: held to 1.5
  it raised so little that it spent 0.959 of the anchor's bytes where it had spent 0.875. A raised
  scene is allowed to look like a detailed one does at the anchor, a little worse at its tail, and
  not to band visibly.
  The limits were 3.5 units and 2 CAMBI while only the gain could meet them. Banding then bound
  every model's gain and the shortfall never did, and cutting the gain to band less costs bytes on
  every raise. A margin holds back only the raises banding limits: on 352 scenes no model had
  seen, the installed model's networks with a margin of 1 CRF added 1.26 CAMBI at the tail where they had added
  1.48, for 0.890 of the anchor's bytes against 0.885 and the same shortfall, where a gain cut to
  0.65 reached 1.23 CAMBI only at 0.905. The shortfall's tail is held to 1.5 so that the margin
  does not buy its bytes back from the quality metrics. The steps were quarters until models read their fast
  encode's banding: the banding limit then bound between them, and three seeds that met it at
  0.9–0.95 were held to 0.75 — on 992 scenes of 31 series, 0.935 of the anchor's bytes where 0.944
  was spent, with the shortfall p95 at 1.55–1.80 against the 30-input model's 2.01 and the banding p95 at
  1.21–1.34 against its 1.57.
- **Verified scenes.** Scenes from [verification](#verifying-an-encodes-decisions) train like any
  other — every one is a full curve — and one whose decision was found `overshot` counts three
  times as much in the loss (`OVERSHOT_WEIGHT`): those are the mistakes that cost quality. But they
  were picked for looking doubtful, so they are not a fair sample of what the model meets: only
  `verify-control` scenes and ordinary lab scenes set typical content and `detail`, and only they
  count in the policy figures — bytes, raises, shortfall and banding percentiles — by which a gain
  is chosen and a candidate is judged. They do count in the vote error. A verified episode is its
  own series (`verify-…`), so its scenes are held out together.
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
  `detail`, `gain`, `banding_margin`, `error` and per-metric `metrics` (held-out vote error in CRF, and the typical
  curve), `policy` (what the candidate would have decided for held-out scenes: `raised_share`,
  `lowered_share`, `raised_far_share` — raised by 4 CRF or more — `raise_mean`, `bytes` against
  the fixed anchor, `shortfall_p90`/`shortfall_p95` in units below a detailed scene, `banding_p95`
  added by the raise, and `needed` — the share of lowerings whose scene really was harder than a
  detailed one), `predictors` (the same for the linear predictors, on the same scenes), `current`,
  `fresh_scenes`, `current_error`, `candidate_error`, `current_shortfall`, `candidate_shortfall`,
  `current_bytes`, `candidate_bytes` (on the fresh scenes, against the anchor), `current_banding`,
  `candidate_banding` (CAMBI added at the 95th percentile there), `accepted`,
  `reason`, `installed`. For a model with a base, `policy` is the base and the fast model together,
  and `base_policy`, `ungated_policy` (the fast model on every scene) and `fast_encoded_share` (the
  share of decisions the base raised) are added.

`--train-scenes <file>` writes what the report's figures are sums of: one JSON line per scene and
anchor with the `votes` and `spreads` of the networks that never trained on the scene's series,
what its encodes `allowed`, the `levels` the networks' curves read at the anchor, per metric in the
report's order, and the `offset` the candidate would have applied — for a model with a base, the
base's unless it raised the scene, with the base's own in `base_offset`. It is where to look when a series is judged badly. `--train-hidden` sets the width
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

A model trained somewhere else is installed the same way over the API: `PUT /api/v1/caramel/model`
with the model file as the body, and `POST /api/v1/caramel/model/active` with `{ "version" }` (or
`null` for the built-in model) to go back ([API.md](API.md)). This is the other half of copying the
dataset out: a machine that measures in its idle time can decide with a model trained on far more
than it measured itself. Such a machine should keep `caramel_research_retrain` high, or its own
next training, from its own smaller dataset and its own feature set, may replace the model it was
given whenever the gate accepts it.

A model file is text: a `caramel-model 1` header, then `version`, `encoder`, `detail`, `gain`, `banding-margin` (only when it is above 0), `scenes`,
`sources`, `trained` (the newest `collected_unix` it saw), `input-mean`, `input-scale`, for a
model that reads other inputs than the first ones `input-select` (which, in order), `offsets`,
`error`, seven `typical <metric>` lines, and per network `member <layers>` followed by `layer
<inputs> <outputs>` and its weights and biases. A model with a base ends with a `base` line followed
by the base's own file, whole; a base that reads the fast encode is refused. A build from before
bases reads the base's members into the model's own, whose inputs then disagree, and refuses the
file rather than misread it. Only a base carries `input-select`, in the file of a 32-input model,
which a build from before it — 31 inputs at most — refuses whole. Unknown keys are skipped; a file whose shapes do not
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
2. Otherwise [verify](#verifying-an-encodes-decisions) the oldest encode kept in
   `DB/caramel/verify/`: `pnmpeg --caramel-lab --lab-verify <entry>/decisions --lab-scenes 60` on
   its input, into `DB/caramel/dataset/verify-<job id>.jsonl`, then drop the entry (also when it
   fails). One stopped for the queue is carried on with next time, skipping the scenes already
   measured. The verdicts are added up in `research.json` by model and by why each scene was picked.
3. Otherwise take the next source and run `pnmpeg --caramel-lab` on it into
   `DB/caramel/dataset/<slug>.jsonl`. Sources, in order: files under `DB/caramel/sources/` (any
   depth to four; `.mkv`, `.mp4`, `.m2ts`, `.webm` of 37 MB or more), then
   `DB/caramel/sources/fetched/`, then the inputs of past jobs still in `DB/cache/inputs`.
4. Otherwise move the [request queue](#queued-releases) along: measure a release that has been
   downloaded, else download the next one.
5. Otherwise, with a feed, read it (at most every 30 minutes), pick one item, download it with
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
`failed`, with the lab's own reason in `error` when it gave one (the lab prints `{ "error" }` as
its last line on standard output when it fails); its directory is deleted once it ends. A source
measured before is not measured again; one that failed is, when it is queued again. A download stopped for a job keeps what it has and
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
| `DB/caramel/verify/<job id>/` | the worker, at *Encoded* | A Caramel encode's decisions and its input, kept for verification. |
| `DB/caramel/feedback/gain.json` | the research worker | The [gain controller](#the-gain-controller)'s state: `model`, its `trained` gain, the `scale` on it, the `held` / `overshot` / `missed` raises counted towards the next step, `steps` and `last_rate`. Deleting it puts the model back at its trained gain. |
