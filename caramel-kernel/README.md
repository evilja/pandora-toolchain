# caramel-kernel

Research and tooling for **content-adaptive CRF selection**: estimate the highest CRF that still
meets a quality target by analysing the source alone, instead of encoding it at several CRFs and
picking the best one.

Pandora's presets currently pin a single CRF per preset (`src/lib/mpeg/preset.rs`: 17 for the
standard presets, 18 for `veryslow`, 25 for the low-bitrate one). A fixed CRF overspends bits on
easy material (flat anime, static dialogue scenes) and can underspend on hard material (heavy
grain, fast action, dark gradients). The goal here is to pick the CRF from the content.

## Implementation: `caramel-scene-3`

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

`caramel-scene-3` is fitted to brute-force encodes. The corpus is 48 clips from eight
open-licensed 1080p sources (Morevna Project episodes 3 and 4 and Sita Sings the Blues for flat
animation, Pepper & Carrot episodes 4 and 6 for painted animation, Sintel for CG, Tears of Steel
and the Xiph derf sequences for live action), cut into 359 two-second scenes. Each was encoded with
libx264 as Pandora's `standard` preset runs it (preset `fast`, aq-mode 3, aq-strength 0.8) at CRF
11 to 29 in steps of two, and measured per frame with XPSNR, SSIM, PSNR and VMAF.

What it found:

- Quality falls by about 0.49 dB of XPSNR per CRF and size by 13% per CRF. At a fixed CRF the sources are not equally good: flat animation scores what typical content
  scores 4–5 CRF lower, live action 4–6 CRF higher. Equalising quality across scenes would cost
  bytes overall, not save them: a fixed CRF already starves hard scenes.
- The metrics disagree about which scenes are easy, by several CRF. A model of any single one moves
  scenes the others say should stay.
- Leaving one source out and predicting it from the rest, a mean offset is 3.4 CRF off per metric
  and the eight features 2.0. Measuring four frames of a scene's opening predicts as well as
  measuring all twelve (2.00 against 2.00); a 24-frame opening gains 0.09.
- The second two seconds of a shot want a CRF more than one away from the first's in half the
  shots, hence the two-second split.
- With the consensus rule, on sources the fit never saw and anchors 17 to 21: 18–20% of scenes are
  raised and 3–6% lowered, the encode is 1.3–1.9% smaller than at the fixed anchor, and a raised
  scene falls 0.14–0.18 dB below the target at the 95th percentile, 1.1 dB at worst. By source at
  anchor 19 the size is 0.93–0.97 for four of the five animation sources and 0.98–1.02 for the
  fifth, the CG and the live action. The streaming decider reproduces the offline figure end to
  end (0.983).
- One 1080p frame costs 0.42 ms on one core of an Apple M4 (2400 fps), down from 1.86 ms when
  every frame was measured.

On a real release (a 24-minute 1080p TV anime episode from an 8 Mbps H.264 web source, anchor 21)
the model keeps 93% of the frames at the anchor, lowers 4% and raises 2%. Cuts fell from 754 to
343 and scenes of exactly `min_scene_frames` from 220 to 10; the analysis ran at 452 fps where
`caramel-scene-2` ran at 276. The verified lowered scenes were below typical quality at the anchor
(SSIM 0.980 and 0.989) and the raised ones were stills that lose nothing at CRF 25 and cost almost
nothing at any CRF, so on this release the model protects hard scenes and saves no bytes. The
source reads noisier (σ 0.65) than any flat animation in the corpus (σ 0.01): a compressed web
source carries the previous encoder's noise, and the corpus has pristine animation only.

What it does not cover: every source was 1080p, 8-bit and clean; the encoder settings were the
`standard` preset's only; there is no banding metric in the consensus, and grainy film and 10-bit sources are not
in the corpus. Other output resolutions get a prior, not a fit. Use `--caramel-verify` /
`verify:true` on real releases to check it where the corpus does not reach; a report's `features`
and `votes` per scene are what a refit needs. A refitted model gets a new `MODEL_VERSION`.

### History

`caramel-prior-1` chose one CRF per file from sampled windows. `caramel-scene-1` and `-2` decided
per scene from a table of hand-set factors (grain, banding, line art, texture and motion masking,
chroma, source bits per pixel) calibrated against the feature distribution of real releases but not
against measured quality. `caramel-scene-2` compared frames through weighted prediction after the
first full-episode report showed fades read as runs of cuts. Its cut test still looked at one pair
alone: a 24-minute episode came out as 772 scenes, 220 of them exactly `min_scene_frames` long.

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
