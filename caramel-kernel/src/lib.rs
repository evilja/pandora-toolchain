// Caramel Analysis: estimate the highest CRF that still holds a quality target from source
// features alone, instead of encoding the source at several CRFs and measuring each.
//
// The crate is self-contained and decoder-agnostic — it is handed 8-bit planes and never depends
// back on Pandora. The flow is:
//
//   1. the caller decodes a few short windows of consecutive frames spread across the source;
//   2. each window goes through a `WindowAnalyzer`, which measures spatial features per frame
//      (`spatial`) and motion/cut features between consecutive frames (`temporal`);
//   3. `estimate` turns the windows, plus what the container says about the source and what the
//      encode will be, into a CRF offset from the encoder configuration's anchor CRF.

pub mod frame;
pub mod model;
pub mod spatial;
pub mod temporal;
pub mod window;

pub use frame::{Frame, Plane};
pub use model::{Contribution, EncodeContext, Estimate, MODEL_VERSION, SourceContext, WindowEstimate, estimate};
pub use spatial::SpatialFeatures;
pub use temporal::TemporalFeatures;
pub use window::{TemporalSummary, WindowAnalyzer, WindowFeatures};
