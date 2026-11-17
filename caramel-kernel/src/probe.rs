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

// The smallest raise that is checked on a source whose raises have been falling short
// (`SourceTrust`). Every raise from here up carries a probe; which of them run is the encoder's call.
pub const PROBE_LOWEST: f64 = 2.0;

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

// How the test encodes of one source have gone so far, and what that makes of its next raises.
//
// Whether a raise falls short depends on the source far more than on the size of the raise: on six
// verification episodes 75% of Ping Pong's probed raises fell short and 69% of Haibane Renmei's,
// against 14–19% on two others, and raises of 3–4 CRF fell short as often as raises of 4–5. So once a
// source's raises have been falling short, its smaller raises are checked too — the worst misses
// those episodes had were raises of 1.5–2.5 — and every raise is made smaller, before any probe,
// in proportion. A source whose raises hold keeps the model's raises and the usual threshold.
//
// Only the latest `TRUST_WINDOW` probes count, so a source that settles into scenes the model reads
// well is trusted again. Nothing is concluded from fewer than `TRUST_MIN`.
pub const TRUST_WINDOW: usize = 24;
pub const TRUST_MIN: usize = 4;
// The share of probes falling short that a source may have before it is distrusted, and the least
// share of a raise that a distrusted source keeps.
pub const TRUST_SHORT: f64 = 0.2;
pub const TRUST_FLOOR: f64 = 0.6;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceTrust {
    // What each probe measured over its target, in dB, oldest first.
    margins: Vec<f64>,
}

impl SourceTrust {
    pub fn from_margins(margins: impl IntoIterator<Item = f64>) -> Self {
        let mut trust = Self::default();
        for margin in margins {
            trust.record(margin);
        }
        trust
    }

    pub fn record(&mut self, margin: f64) {
        if margin.is_finite() {
            self.margins.push(margin);
            if self.margins.len() > TRUST_WINDOW {
                self.margins.remove(0);
            }
        }
    }

    pub fn probes(&self) -> usize {
        self.margins.len()
    }

    // The share of the latest probes that fell short; None until there are enough to say.
    pub fn short_share(&self) -> Option<f64> {
        (self.margins.len() >= TRUST_MIN).then(|| self.margins.iter().filter(|margin| **margin < 0.0).count() as f64 / self.margins.len() as f64)
    }

    fn distrusted(&self) -> Option<f64> {
        self.short_share().filter(|share| *share > TRUST_SHORT)
    }

    // The smallest raise to check on this source.
    pub fn probe_from(&self) -> f64 {
        if self.distrusted().is_some() { PROBE_LOWEST } else { PROBE_FROM }
    }

    // The share of the model's raise this source keeps: all of it while trusted, down to
    // `TRUST_FLOOR` as its raises fall short more often — 0.84 at 40%, the floor from Haibane's 69%
    // and Ping Pong's 75% on.
    pub fn raise_scale(&self) -> f64 {
        self.distrusted().map_or(1.0, |share| (1.0 - 0.8 * (share - TRUST_SHORT)).clamp(TRUST_FLOOR, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A source is trusted until enough of its probes fell short, then checked from 2 CRF and its
    // raises shrunk; once its latest probes hold again it is trusted again.
    #[test]
    fn a_source_whose_raises_fall_short_is_checked_more_and_raised_less() {
        let mut trust = SourceTrust::default();
        assert_eq!((trust.probe_from(), trust.raise_scale(), trust.short_share()), (PROBE_FROM, 1.0, None));
        for margin in [-0.5, -0.3, 0.2] {
            trust.record(margin);
        }
        // Three probes say nothing yet, however they went.
        assert_eq!(trust.probe_from(), PROBE_FROM);
        trust.record(-0.1);
        assert_eq!(trust.short_share(), Some(0.75));
        assert_eq!(trust.probe_from(), PROBE_LOWEST);
        assert_eq!(trust.raise_scale(), TRUST_FLOOR);
        // One short in five is within what a trusted source has.
        let held = SourceTrust::from_margins([-0.2, 0.1, 0.3, 0.4, 0.5]);
        assert_eq!((held.probe_from(), held.raise_scale()), (PROBE_FROM, 1.0));
        // A run of probes that hold pushes the misses out of the window.
        for _ in 0..TRUST_WINDOW {
            trust.record(0.4);
        }
        assert_eq!((trust.probe_from(), trust.raise_scale(), trust.probes()), (PROBE_FROM, 1.0, TRUST_WINDOW));
        trust.record(f64::NAN);
        assert_eq!(trust.probes(), TRUST_WINDOW);
    }

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
