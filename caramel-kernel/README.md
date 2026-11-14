# caramel-kernel

Research and tooling for **content-adaptive CRF selection**: estimate the highest CRF that still
meets a quality target by analysing the source alone, instead of encoding it at several CRFs and
picking the best one.

Pandora's presets currently pin a single CRF per preset (`src/lib/mpeg/preset.rs`: 17 for the
standard presets, 18 for `veryslow`, 25 for the low-bitrate one). A fixed CRF overspends bits on
easy material (flat anime, static dialogue scenes) and can underspend on hard material (heavy
grain, fast action, dark gradients). The goal here is to pick the CRF from the content.

## Implementation: `caramel-scene-1`

**Caramel Analysis** gives every scene its own CRF. It is a prior built from published encoding
practice and calibrated against the feature distribution of real releases, not yet against
measured quality. It moves the preset's own CRF (the *anchor*) by bounded offsets. It does not choose
an absolute CRF, so a deployment that never turns it on encodes exactly as before. The crate depends
only on `serde`: it takes decoded planes and runs no processes. Decoding, encoding and verification
are Pandora's job (`src/lib/mpeg/caramel.rs` and `pnx264::scenes`; see `docs/TOOLS.md`).

- `frame` — borrowed `Plane` / `Frame` views over 8-bit planes (luma, and chroma when present).
- `spatial` — per-frame luma features, evaluated on every second pixel of every second row:
  - P.910 SI (std of the Sobel magnitude) and edge density (magnitude > 64).
  - Immerkær's Laplacian noise σ, taken over the 40% of pixels with the lowest gradient so edges do
    not count as grain. Measured unbiased on synthetic grain: σ 1/2/4/6 reads as 1.04/2.03/4.02/6.01.
  - Shares of 16×16 blocks that are smooth, gradient (smooth next to a smooth neighbour with a
    small mean step: banding risk), dark gradient, line art (edges over flat fill), and dark.
- `temporal` — P.910 TI, plus a quarter-resolution 8×8 block motion search (predictors and a small
  diamond, ±8). It gives the zero-motion and motion-compensated residual, the mean motion, and the
  intra residual of the same low-resolution frame (left/up DPCM). A pair is a cut when the
  histogram distance is large, when it is moderate and the motion residual is real, or when motion
  compensation predicts the frame worse than twice as badly as the frame predicts itself — the
  last catches a cut between two shots that share one palette.
- `window` — `WindowAnalyzer` accumulates consecutive frames into `WindowFeatures`. Frame pairs
  across a cut are kept out of the temporal means.
- `scene` — `SceneDecider`, fed every frame in display order. It finds cuts (ignoring any within
  `min_scene_frames` of the last), splits a scene past `max_scene_frames`, measures each scene's
  first `analysis_frames` and decides its CRF as soon as it has. `decided_frames()` says how many
  frames an encoder may release, so it holds back at most one scene's opening. Deciding from the
  opening is what makes it streamable; it holds because a shot does not change character halfway,
  and the split bounds the damage when one does.
- `model` — `estimate_scene`. Each scene gets an offset from these factors:
  - grain (down to −2.5; a small raise when the source is very clean)
  - gradient banding and dark banding, from 55% and 45% of the frame respectively (stronger when a
    >8-bit source goes to 8-bit)
  - line art
  - texture masking (only where there is no gradient)
  - motion masking (a small cut for a still shot)
  - chroma detail
  - output resolution and source bits per pixel. The starved-source raise applies only when the
    content is detailed, because a flat source is cheap rather than starved.

  A raise is scaled by confidence (frames measured, temporal coverage); a cut is not. The total is
  bounded to −4…+4 and rounded to 0.5. Every factor's contribution is reported, so a wrong CRF shows
  which term caused it.

Calibration so far: `caramel-prior-1`'s banding thresholds (5–35% of the frame) put every window of
two real 1080p TV anime releases at the full penalty and the lower bound, because cel shading puts
30–70% of a typical frame in smooth gradient blocks. That is content the anchor is already right
for, and x264's aq-mode 3 already spends on it. The thresholds now start where a frame is mostly
gradient, so typical scenes sit at the anchor and skies and dark fades are cut. A unit test pins
those releases' median features to the anchor.

The coefficients are still a starting point. Use `--caramel-verify` / `verify:true` (SSIM, PSNR and
bitrate of real scenes at their own CRF and at fixed CRFs) to collect the evidence that calibrates
them. A calibrated model gets a new `MODEL_VERSION`.

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

- **Per scene** (implemented): a CRF per shot, so a static dialogue scene and a fight scene get
  different CRFs in the same episode. Chunk encoders switch CRF per picture; linear ffmpeg encodes
  get x264 `zones`.
- **Per file** was `caramel-prior-1`: one CRF from the 25th percentile of sampled windows. It let the
  hardest windows set the CRF for the easy majority, which is what a per-scene CRF avoids.

## Evaluation

- Prediction error against the brute-force CRF (mean and worst-case, in CRF steps).
- Hit rate: share of encodes whose measured quality actually meets the target.
- Bitrate saved against the fixed preset CRF at equal hit rate.
- Analysis cost as a fraction of encode time.

## Open questions

- Which quality metric and threshold define "visually transparent" for Pandora's releases
  (anime-heavy: SSIMULACRA2 and XPSNR tend to track line-art damage better than VMAF).
- Whether a very cheap encoder-derived feature (for example x264 lookahead/`ultrafast` first-pass
  complexity) is worth its cost alongside the pure source features.
- Answered: the decider plugs into `pnmpeg` as a preset's `crf_mode = "caramel-analysis"`, as
  `--aot-crf caramel-analysis` per AOT chunk, and as the standalone `--caramel-report`.
