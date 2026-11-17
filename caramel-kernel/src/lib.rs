// Caramel Analysis: decide, scene by scene, the highest CRF that still holds a quality target, from
// source features alone instead of encoding the source at several CRFs and measuring each.
//
// The crate is self-contained and decoder-agnostic — it is handed 8-bit planes and never depends
// back on Pandora. The flow is:
//
//   1. the caller pushes every frame, in display order, into a `SceneDecider` (`scene`);
//   2. the decider looks for cuts between consecutive frames (`temporal`) and measures the first
//      frames of each scene (`window`, built on `spatial` and `temporal`);
//   3. `estimate_scene` turns those measurements, plus what the container says about the source and
//      what the encode will be, into the scene's CRF as an offset from the encoder configuration's
//      anchor CRF (`model`).

pub mod curve;
pub mod fast;
pub mod frame;
pub mod inputs;
pub mod model;
pub mod nn;
pub mod probe;
pub mod scene;
pub mod spatial;
pub mod temporal;
pub mod train;
pub mod window;
pub mod xpsnr;

pub use fast::{FAST_CRF, FAST_FRAMES, FAST_PRESET, FastEncode};
pub use frame::{Frame, Plane};
pub use probe::{PROBE_FRAMES, PROBE_FROM, PROBE_LOWEST, Probe, SourceTrust};
pub use model::{Contribution, EncodeContext, MODEL_VERSION, MetricVote, SceneEstimate, SourceContext, estimate_scene, estimate_scene_with, model_version};
pub use scene::{SceneConfig, SceneDecider, SceneDecision, SceneJudge, SceneStart};
pub use spatial::SpatialFeatures;
pub use temporal::TemporalFeatures;
pub use window::{SceneWindow, TemporalSummary, WindowAnalyzer, WindowFeatures};
