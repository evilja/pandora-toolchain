// A test encode of a scene the model raises far, to check the raise before it is spent.
//
// The model's votes are about 2 CRF wrong on a scene it has not seen, and the misses that cost
// quality are the large raises: a still, grainy shot read as easy and raised by five, which then
// falls a dB or more below a detailed scene kept at the anchor. A full-episode verify finds a few
// of those in every episode; the lab's held-out scenes find them too, and they are not something a
// better network fixes — most of the error is between scenes of one series.
//
// So the encoder checks: it encodes the scene's opening frames (the ones it holds back anyway) at
// the CRF the model chose, measures XPSNR against them as the lab measures it (`xpsnr`), and hands
// the measurement to `Probe::settle`. The scene's curve as the model drew it is moved to pass
// through the measurement, and the CRF is read where that curve meets the target — what a detailed
// scene scores at the anchor, the line every raise is held to. A measurement at or above the target
// keeps the raise; one below it pulls the raise back, never past the anchor. A probe never raises a
// scene further: what it checks is that a raise was safe, not that it was large enough.

use crate::curve::{KNOTS, KNOT_COUNT, line_crossing};
use serde::Serialize;

// The smallest raise that is checked, in CRF. Smaller raises fall short rarely and by little, and
// a probe costs an encode of the scene's opening frames.
pub const PROBE_FROM: f64 = 3.0;

// How many of a scene's first frames the test encode is made of, at most: fewer when the scene is
// shorter. The first is an IDR in a test encode and is left out of the measurement, as the lab
// leaves a clip's first frame out.
pub const PROBE_FRAMES: usize = 24;

#[derive(Clone, Debug, Serialize)]
pub struct Probe {
    pub anchor: f64,
    // The CRF the model chose, which the test encode is made at.
    pub crf: f32,
    // The XPSNR a detailed scene scores at the anchor.
    pub target: f64,
    // The scene's XPSNR at each of the model's knots, as its networks drew it.
    pub curve: [f64; KNOT_COUNT],
    // What the test encode measured, and the CRF it settled on, once it has run.
    pub measured: Option<f64>,
    pub settled: Option<f32>,
}

impl Probe {
    // The CRF the scene is encoded at given the test encode's XPSNR: the proposal when it held the
    // target, otherwise the CRF where the curve through the measurement meets it, on the CRF step
    // and between the anchor and the proposal.
    pub fn settle(&mut self, measured: f64) -> f32 {
        let step = crate::model::CRF_STEP;
        let points: [(f64, f64); KNOT_COUNT] = std::array::from_fn(|knot| (KNOTS[knot], self.curve[knot]));
        let crf = self.crf as f64;
        let settled = if measured >= self.target {
            self.crf
        } else {
            // The curve moved up or down by what it missed the measurement by.
            let missed = measured - crate::curve::line_at(&points, crf);
            let crossing = line_crossing(&points, self.target - missed);
            let floor = (self.anchor / step).ceil() * step;
            (((crossing / step).floor() * step).clamp(floor.min(crf), crf)) as f32
        };
        self.measured = Some(measured);
        self.settled = Some(settled);
        settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A curve falling 0.5 dB per CRF, 40 dB at CRF 22.
    fn probe(crf: f32) -> Probe {
        Probe { anchor: 17.0, crf, target: 41.0, curve: std::array::from_fn(|knot| 40.0 - 0.5 * (KNOTS[knot] - 22.0)), measured: None, settled: None }
    }

    #[test]
    fn a_probe_keeps_a_raise_that_holds_and_pulls_back_one_that_does_not() {
        // The measurement holds the target: the raise stands, however the curve read.
        assert_eq!(probe(22.0).settle(41.2), 22.0);
        // A dB short at CRF 22 on a curve that falls half a dB per CRF: two CRF back.
        let mut short = probe(22.0);
        assert_eq!(short.settle(40.0), 20.0);
        assert_eq!((short.measured, short.settled), (Some(40.0), Some(20.0)));
        // Between steps the CRF is rounded towards the anchor, so the scene is not left short.
        assert_eq!(probe(22.0).settle(40.2), 20.0);
        // Far short: no further back than the anchor.
        assert_eq!(probe(22.0).settle(30.0), 17.0);
        // The model's curve itself does not matter where it sits, only how it falls: the same
        // shortfall pulls the same raise back by the same amount on a curve 5 dB higher.
        let mut higher = probe(22.0);
        higher.curve.iter_mut().for_each(|value| *value += 5.0);
        assert_eq!(higher.settle(40.0), 20.0);
    }
}
