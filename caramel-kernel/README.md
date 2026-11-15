# caramel-kernel

Research and tooling for **content-adaptive CRF selection**: estimate the highest CRF that still
meets a quality target by analysing the source alone, instead of encoding it at several CRFs and
picking the best one.

Pandora's presets currently pin a single CRF per preset (`src/lib/mpeg/preset.rs`: 17 for the
standard presets, 18 for `veryslow`, 25 for the low-bitrate one). A fixed CRF overspends bits on
easy material (flat anime, static dialogue scenes) and can underspend on hard material (heavy
grain, fast action, dark gradients). The goal here is to pick the CRF from the content.

## Implementation: `caramel-scene-5`

**Caramel Analysis** gives every scene its own CRF. It moves the preset's own CRF (the *anchor*) by
a bounded offset. It does not choose an absolute CRF, so a deployment that never turns it on encodes
exactly as before. The crate depends only on `serde`: it takes decoded planes and runs no processes.
Decoding, encoding and verification are Pandora's job (`src/lib/mpeg/caramel.rs` and
`pnx264::scenes`; see `docs/TOOLS.md`).

- `frame` — borrowed `Plane` / `Frame` views over 8-bit planes (luma, and chroma when present).
- `spatial` — per-frame luma features, evaluated on every second pixel of every second row:
  - P.910 SI (std of the Sobel magnitude) and edge density (magnitude > 64).
  - Immerkær's Laplacian noise σ, taken over the 40% of pixels with the lowest gradient so edges do
    not count as grain. Measured unbiased on synthetic grain: σ 1/2/4/6 reads as 1.04/2.03/4.02/6.01.
  - Fine-detail energy under five noise floors (0.5 to 8 code values): the mean of
    min(h², floor²), h being the high-pass response. An encoder at some quantiser removes the detail
    under its own noise floor, so this is what a frame has to lose.
  - Shares of 16×16 blocks that are smooth, gradient, dark gradient, line art, and dark.
- `temporal` — everything between two frames, on a quarter-resolution copy that is made once per
  frame and kept instead of the frame. P.910 TI; a whole-frame motion vector found on a
  sixteenth-resolution copy (±128 luma pixels), then an 8×8 block search from it and from the
  neighbours' vectors. It gives the zero-motion and motion-compensated residual, the mean motion,
  the share of blocks the search could not explain (`changed_share`), the residual energy under the
  same five floors, and the intra residual of the frame itself. The previous frame is first given
  the current one's mean and contrast — x264's weighted-prediction gain and offset — so a fade or a
  flash is not read as a new shot.
- **Cuts** are judged in two steps. A pair is a *candidate* when its histogram distance is large,
  when it is moderate and the motion residual is real, or when motion compensation predicts the
  frame more than twice as badly as the frame predicts itself (a cut between two shots that share
  one palette). A candidate is a cut only if most of the frame changed and its residual stands out
  from the three pairs before it and the two after it: fast action, whip pans and fire are
  candidates on every pair for as long as they last, and a pose change is one on a single pair
  with the background where it was. On 231 hand-labelled candidates from 44 clips (157 cuts), the
  candidate test alone made 143 false cuts; the full test makes 5 and misses 5.
- `window` — `WindowAnalyzer` accumulates a scene's opening frames into `WindowFeatures`. Frame
  pairs across a cut are kept out of the temporal means.
- `scene` — `SceneDecider`, fed every frame in display order. It settles whether a frame starts a
  scene two frames after it arrives, ignores cuts within `min_scene_frames` of the last, splits a
  scene past `max_scene_frames` (48: two seconds), and decides each scene's CRF from its first
  `analysis_frames` (12). Every frame is compared with the one before it; four of the twelve are
  measured by themselves, which is the expensive step. `decided_frames()` says how many frames an
  encoder may release, so it holds back at most one scene's opening plus two frames.
- `model` — `estimate_scene`. Four linear predictors, one per quality metric (XPSNR, the worst
  tenth of frames by XPSNR, SSIM, the worst tenth by VMAF), each estimate the CRF offset at which
  the scene would score what typical content scores at the anchor. They read eight features: noise
  σ, fine-detail energy, residual energy, zero-motion residual, TI, SI, motion in pixels and the
  smooth-block share. The scene moves only when all four agree on the direction by more than 1.5
  CRF, and then by the smallest of the four less 1.5. A raise is scaled by confidence (frames
  measured, temporal coverage); a cut is not. The output-resolution prior is added, the total is
  bounded to −4…+4 and rounded to 0.5. A report carries every metric's vote and the scene's
  features, so a wrong CRF shows which metric and which measurement caused it.

### Calibration

`caramel-scene-5` is fitted to brute-force encodes. The corpus is 48 clips from eight
open-licensed 1080p sources (Morevna Project episodes 3 and 4 and Sita Sings the Blues for flat
animation, Pepper & Carrot episodes 4 and 6 for painted animation, Sintel for CG, Tears of Steel
and the Xiph derf sequences for live action), cut into 359 two-second scenes. Each was encoded with
libx264 as Pandora's `standard` preset runs it (preset `fast`, aq-mode 3, aq-strength 0.8) at CRF
11 to 29 in steps of two, and measured per frame with XPSNR, SSIM, PSNR and VMAF. Every scene's
features are in the fit twice, against the same measured offsets: as the clip has them, and as
they read when the clip is a 10-bit source dithered down to 8 bits (below).

What it found:

- Quality falls by about 0.49 dB of XPSNR per CRF and size by 13% per CRF. At a fixed CRF the sources are not equally good: flat animation scores what typical content
  scores 4–5 CRF lower, live action 4–6 CRF higher. Equalising quality across scenes would cost
  bytes overall, not save them: a fixed CRF already starves hard scenes.
- The metrics disagree about which scenes are easy, by several CRF. A model of any single one moves
  scenes the others say should stay.
- Leaving one source out and predicting it from the rest, a mean offset is 3.4 CRF off per metric
  and eight features of the pristine frames 2.0. Measuring four frames of a scene's opening
  predicts as well as measuring all twelve (2.00 against 2.00); a 24-frame opening gains 0.09.
- The second two seconds of a shot want a CRF more than one away from the first's in half the
  shots, hence the two-second split.
- Real sources do not read like pristine ones. A 10-bit source reaches the decider as 8-bit frames,
  and ffmpeg dithers on the way down. Every corpus clip was measured again as such a source
  (sub-code-value gradients added in 10 bits, then the default conversion): noise reads σ 1.06
  where a flat-animation clip has 0.01 and the finest detail energy about three times what it is,
  while SI, TI, motion and the smooth share do not move. An 8-bit web release reads the same way
  at about half the strength (σ 0.5–0.65): re-encoding pristine animation at web quality adds no
  noise, re-encoding a dithered 10-bit master at 7.5 Mbps leaves σ 0.78. Predictors fitted to
  pristine frames answer the dither with 1.4 to 5 CRF more bits, depending on the metric: nothing
  is raised, a quarter of the scenes are lowered and the encode is 3% larger than at the fixed
  anchor. Measured against the 10-bit picture, encodes of dithered and of plainly rounded frames
  score the same at every CRF (XPSNR within 0.3 dB, SSIM within 0.17 dB, on four kinds of
  content), so the dither is not something to spend bits on. Predictors fitted to dithered frames
  alone get the bytes back but lean on small differences in the dither itself, and are 4 to 20 CRF
  too low on a scene that carries none.
- Fitted to both readings of every scene, the predictors are 1.9 to 2.7 CRF off per metric on an
  unseen source, 2.2 on average read pristine and 2.4 read dithered.
- With the consensus rule, on sources the fit never saw and anchors 17 to 21: read pristine,
  20–23% of scenes are raised and 2–4% lowered, the encode is 1.7–2.5% smaller than at the fixed
  anchor, and a raised scene falls 0.22–0.33 dB below the target at the 95th percentile, 1.15 dB
  at worst; read dithered, 16–18% are raised and 3–5% lowered, the encode is 0.9–1.4% smaller and
  the shortfall is 0.08–0.17 dB at the 95th percentile, 0.86 dB at worst. By source at anchor 19,
  read pristine, the size is 0.92–0.96 for four of the five animation sources and 0.99–1.01 for
  the fifth, the CG and the live action. The streaming decider, end to end on the pristine clips,
  gives 0.985.
- One 1080p frame costs 0.42 ms on one core of an Apple M4 (2400 fps), down from 1.86 ms when
  every frame was measured.

On a real release (a 24-minute 1080p TV anime episode from an 8 Mbps H.264 web source, anchor 21)
48 scenes at even steps through the episode were each encoded at CRF 15 to 27 on the deployment
and measured against the source. `caramel-scene-3`, fitted on pristine frames only, had kept 93% of
the episode's frames at the anchor, lowered 4% and raised 2%, and of the 48 it raised none: its
XPSNR vote was 2.7 CRF below what the encodes allowed on average and its SSIM vote 9.6. The
predictors of `caramel-scene-5`, given the features that report stored, are 0.3 CRF above on XPSNR
(2.7 off per scene) and 5.0 below on SSIM. They raise 12 of the 48 — stills and fast action — by
0.5 to 2 CRF; eleven of those stay within 0.07 dB of the target on XPSNR and its worst frames, one
falls 0.88 dB below, and the 48 scenes come out 2.1% smaller. Over the whole episode they raise
23% of the frames and lower 1%. Deployed, the sixteen scenes it moved furthest were measured the
same way: the seven it lowered were all short of the target at the anchor, by 0.2 to 5.6 CRF on
XPSNR, and of the nine it raised seven stay above the target, one is 0.13 dB short on its worst
frames, and one — raised 3 CRF — ends 1.7 dB below it there, having been 0.4 dB below at the
anchor. That is the model's error showing where it is largest; nothing in the eight features
tells that scene from the easy ones beside it. The episode is 851 scenes; `caramel-scene-2` cut it into 772 with
220 of them exactly `min_scene_frames` long, where there are now 10, and analysed it at 276 fps
where the deployment now runs 420.

On a 10-bit release (a 24-minute 1080p episode from a 3.2 Mbps HEVC Blu-ray encode, anchor 21)
`caramel-scene-3` raised nothing and lowered 13% of the frames, with the median scene reading
noise σ 1.19 and fine detail 0.219 — the dither's figures, not the picture's. `caramel-scene-5`
raises 5% of the frames and lowers 3%, and 48 scenes at even steps, encoded at CRF 15 to 27 and
compared with the 10-bit source, say that is right: the release is typical content (41.7 dB of
XPSNR at the anchor where the target is 41.1), the XPSNR vote is 0.2 CRF below what the encodes
allow on average and 2.0 off per scene, the worst-frames vote the same, and the SSIM vote 2.8
below. None of the 48 is raised; two are lowered, both short of the target at the anchor by 2 to
3.5 CRF. The dithered half of the fit is simulated, and this is the check that the simulation
reads like a real 10-bit source.

Adding one release's measured scenes to the fit does not improve the other's (2.2 against 2.2 CRF
on XPSNR for the web release, 2.0 against 1.9 for the 10-bit one): at about two CRF per scene the
eight features are the limit, not the corpus.

Earlier figures from verification are not quoted: until it paired frames by index it compared some
frames of a 23.976 fps source with their neighbours, and until it compared a 10-bit source at 10
bits it scored the encode against the source's dither.

What it does not cover: every source was 1080p, and the dithered readings are simulated from the
pristine ones; the encoder settings were the `standard` preset's only; there is no banding metric
in the consensus — which matters most exactly where dither is, since dither is there to hide
banding — and grainy film is not in the corpus. Other output resolutions get a prior, not a fit.
Use `--caramel-verify` / `verify:true` on real releases to check it where the corpus does not
reach — with `spread` it measures scenes at even steps through the video over a ladder of CRFs, in
the model's own metrics; a report's `features` and `votes` per scene are what a refit needs. A
refitted model gets a new `MODEL_VERSION`.

### History

`caramel-prior-1` chose one CRF per file from sampled windows. `caramel-scene-1` and `-2` decided
per scene from a table of hand-set factors (grain, banding, line art, texture and motion masking,
chroma, source bits per pixel) calibrated against the feature distribution of real releases but not
against measured quality. `caramel-scene-2` compared frames through weighted prediction after the
first full-episode report showed fades read as runs of cuts. Its cut test still looked at one pair
alone: a 24-minute episode came out as 772 scenes, 220 of them exactly `min_scene_frames` long.
`caramel-scene-3` replaced the table with predictors fitted to measured encodes and the cut test
with one that looks at the pairs around a candidate. Fitted on pristine frames, it read the dither
of a 10-bit source — and what is left of it in a web release — as grain and detail: on real
releases it raised almost nothing. `caramel-scene-4` gave sources deeper than 8 bits predictors
fitted on both the pristine and the dithered reading of every scene; `caramel-scene-5` uses those
for every source, after a web release measured the same way.

## Problem statement

Given a source video, an encoder configuration (encoder, preset, tune, AQ settings) and a quality
target (for example *SSIMULACRA2 ≥ 80* or *VMAF ≥ 95* on the 5th percentile of frames), predict
the **largest CRF whose encode still meets the target**, without running a CRF search at runtime.

Brute-force search (sample encodes at several CRFs plus a metric pass, as tools like `ab-av1` do)
is still useful offline to produce **ground-truth labels** for training and evaluation. It is what
the predictor replaces at runtime, not something to drop entirely.

## Source features

Computed on decoded luma (and chroma where it matters), ideally on a downscaled or sampled set of
frames so analysis costs a small fraction of the encode.

| Feature | What it captures | Starting method |
|---|---|---|
| Spatial information (SI) | Per-frame texture/detail | ITU-T P.910: std-dev of the Sobel-filtered frame |
| Temporal information (TI) | Motion between frames | ITU-T P.910: std-dev of the frame difference |
| Edge density | Share of the frame that is edges; line art in anime | Fraction of pixels above a Sobel/Canny threshold |
| Scene changes | Cut frequency and shot length; cost of new keyframes | Histogram or SAD difference with an adaptive threshold |
| Grain / noise | Film grain or dithering that eats bits at low CRF | High-pass residual variance measured in flat blocks |
| Flat gradients / dark areas | Banding risk, where high CRF fails first | Low-variance block share, luma histogram, low-APL share |
| Motion complexity | How predictable motion is | Block motion-vector magnitude and variance (or encoder lookahead stats) |
| Chroma complexity | Saturated detail that 4:2:0 and CRF struggle with | SI on the U/V planes |
| Source compression | Existing blocking/ringing; already-lossy sources | Blockiness metric on 8×8 boundaries, source bitrate per pixel |
| Format | Resolution, frame rate, bit depth, HDR | Container/stream metadata |

Each feature is aggregated per scene (mean, percentiles, max), since a single per-file average
hides the hard scenes that decide whether the target is met.

## Model

1. **Dataset** — a corpus of representative sources (anime, live action, grainy film, CG), each
   split into scenes. For each scene, run the offline CRF search once to find the target-meeting
   CRF for a given encoder configuration.
2. **Predictor** — start with something interpretable (linear or gradient-boosted regression from
   scene features to CRF), and only move to heavier models if the error justifies it.
3. **Per-encoder calibration** — the CRF scale differs between x264, x265 and SVT-AV1, and shifts
   with preset and AQ settings, so the model is trained (or offset-calibrated) per configuration.
4. **Safety margin** — the predicted CRF is biased downward by the model's error spread, so a miss
   costs bits rather than visible quality.

## Granularity

- **Per scene** (implemented): a CRF per shot, and per two seconds of a longer one, so a static
  dialogue scene and a fight scene get different CRFs in the same episode. Chunk encoders switch CRF per picture; linear ffmpeg encodes
  get x264 `zones`.
- **Per file** was `caramel-prior-1`: one CRF from the 25th percentile of sampled windows. It let the
  hardest windows set the CRF for the easy majority, which is what a per-scene CRF avoids.

## Evaluation

- Prediction error against the brute-force CRF (mean and worst-case, in CRF steps).
- Hit rate: share of encodes whose measured quality actually meets the target.
- Bitrate saved against the fixed preset CRF at equal hit rate.
- Analysis cost as a fraction of encode time.

## Open questions

- Which quality metric and threshold define "visually transparent" for Pandora's releases. The
  model sidesteps it by requiring four metrics to agree; a banding metric (CAMBI) is the one most
  likely to disagree with all four on dark gradients, and is not in the consensus yet.
- Whether the fit carries to `veryslow`, to x265 and SVT-AV1 anchors, and to resolutions other
  than 1080p.
- Answered: the decider plugs into `pnmpeg` as a preset's `crf_mode = "caramel-analysis"`, as
  `--aot-crf caramel-analysis` per AOT chunk, and as the standalone `--caramel-report`.
