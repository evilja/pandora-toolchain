# caramel-kernel

Research and tooling for **content-adaptive CRF selection**: estimate the highest CRF that still
meets a quality target by analysing the source alone, instead of encoding it at several CRFs and
picking the best one.

Pandora's presets currently pin a single CRF per preset (`src/lib/mpeg/preset.rs`: 17 for the
standard presets, 18 for `veryslow`, 25 for the low-bitrate one). A fixed CRF overspends bits on
easy material (flat anime, static dialogue scenes) and can underspend on hard material (heavy
grain, fast action, dark gradients). The goal here is to pick the CRF from the content.

## Implementation: `caramel-prior-1`

The first model, **Caramel Analysis**, is a prior built from published encoding practice, not from
measurements yet. It moves the preset's own CRF (the *anchor*) by bounded offsets. It does not
choose an absolute CRF, so a deployment that never turns it on encodes exactly as before. The crate
depends only on `serde`: it takes decoded planes and runs no processes. Decoding, sampling and
verification are Pandora's job (`src/lib/mpeg/caramel.rs`; see `docs/TOOLS.md`).

- `frame` — borrowed `Plane` / `Frame` views over 8-bit planes (luma, and chroma when present).
- `spatial` — per-frame luma features, evaluated on every second pixel of every second row:
  - P.910 SI (std of the Sobel magnitude) and edge density (magnitude > 64).
  - Immerkær's Laplacian noise σ, taken over the 40% of pixels with the lowest gradient so edges do
    not count as grain. Measured unbiased on synthetic grain: σ 1/2/4/6 reads as 1.04/2.03/4.02/6.01.
  - Shares of 16×16 blocks that are smooth, gradient (smooth next to a smooth neighbour with a
    small mean step: banding risk), dark gradient, line art (edges over flat fill), and dark.
- `temporal` — P.910 TI, plus a quarter-resolution 8×8 block motion search (predictors and a small
  diamond, ±8). It gives the zero-motion and motion-compensated residual, the mean motion, and cut
  detection from histogram distance.
- `window` — `WindowAnalyzer` accumulates a few consecutive frames into `WindowFeatures`. Frame
  pairs across a cut are kept out of the temporal means.
- `model` — `estimate`. Each window gets an offset from these factors:
  - grain (down to −2.5; a small raise when the source is very clean)
  - gradient banding and dark banding (stronger when a >8-bit source goes to 8-bit)
  - line art
  - texture masking (only where there is no gradient)
  - motion masking
  - chroma detail

  The file takes the weighted 25th percentile of the window offsets, so the hard windows decide.
  It then adds global factors for output resolution and source bits per pixel. The starved-source
  raise applies only when the content is detailed, because a flat source is cheap rather than
  starved. A raise is scaled by confidence; a cut is not. The total is bounded to −4…+4 and rounded
  to 0.5. Every factor's contribution is reported, so a wrong estimate shows which term caused it.

The coefficients are a starting point. Use `--caramel-verify` / `verify:true` (SSIM, PSNR and
bitrate of real clips at the chosen CRFs) to collect the evidence that calibrates them. A
calibrated model gets a new `MODEL_VERSION`.

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

- **Per file**: one CRF for the whole encode — the simplest integration with today's presets.
- **Per scene**: a CRF per shot via x264/x265 zones, so a static dialogue scene and a fight scene
  get different CRFs in the same episode.

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
- Answered: the estimate plugs into `pnmpeg` as `--aot-crf caramel-analysis`, per AOT chunk, and as
  the standalone `--caramel-report`. See below.
