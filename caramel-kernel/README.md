# caramel-kernel

Research and tooling for **content-adaptive CRF selection**: estimate the highest CRF that still
meets a quality target by analysing the source alone, instead of encoding it at several CRFs and
picking the best one.

Pandora's presets currently pin a single CRF per preset (`src/lib/mpeg/preset.rs`: 17 for the
standard presets, 18 for `veryslow`, 25 for the low-bitrate one). A fixed CRF overspends bits on
easy material (flat anime, static dialogue scenes) and can underspend on hard material (heavy
grain, fast action, dark gradients). The goal here is to pick the CRF from the content.

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
- How the estimate plugs into `pnmpeg` / `pnx264`: an `--aot-crf auto` mode, a preset option, or a
  standalone analysis tool that writes the CRF (or zones) for the encode to use.
