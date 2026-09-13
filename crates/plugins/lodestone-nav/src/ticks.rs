//! The cost unit: **ticks, in 1/256ths, as a `u32`** (`docs/baritone-port.md`
//! §4.1).
//!
//! Ticks because the executor spends ticks and `lodestone-physics` is per-tick,
//! so every cost is directly measurable against the thing that will execute it.
//! Fixed-point because a priority queue ordered by floats is **not
//! reproducible**: equal-cost ties resolve by accumulation order, which changes
//! with expansion order, which changes with hash iteration. 1/256 tick is
//! ~0.2 ms of game time, finer than any real cost difference, and `u32` spans
//! ~9.7 days so saturating addition cannot roll an impossible edge into a cheap
//! one.

/// Fixed-point scale: one game tick is this many raw units.
pub const SCALE: u32 = 256;

/// A duration in game ticks, fixed-point with [`SCALE`] units per tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ticks(u32);

impl Ticks {
    /// Zero cost.
    pub const ZERO: Self = Self(0);

    /// The "impossible" sentinel. Saturating addition lands here rather than
    /// wrapping, so an illegal edge can never be cheaper than a legal one.
    pub const IMPOSSIBLE: Self = Self(u32::MAX);

    /// From raw 1/256-tick units.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Raw 1/256-tick units.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// From whole ticks.
    #[must_use]
    pub const fn from_ticks(ticks: u32) -> Self {
        Self(ticks.saturating_mul(SCALE))
    }

    /// From a fractional tick count, rounding to the nearest 1/256.
    ///
    /// Negative and non-finite inputs clamp to [`Self::ZERO`] rather than
    /// producing a nonsense cost: a cost model that can emit a negative edge
    /// breaks A\*'s admissibility silently, and there is no legitimate caller.
    #[must_use]
    pub fn from_f64(ticks: f64) -> Self {
        if !ticks.is_finite() || ticks <= 0.0 {
            return Self::ZERO;
        }
        let raw = (ticks * f64::from(SCALE)).round();
        if raw >= f64::from(u32::MAX) {
            Self::IMPOSSIBLE
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Self(raw as u32)
        }
    }

    /// As a fractional tick count, for logging and calibration histograms.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / f64::from(SCALE)
    }

    /// Saturating addition. Saturates *at* [`Self::IMPOSSIBLE`].
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Multiply by a non-negative scalar, saturating.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        Self::from_f64(self.as_f64() * factor)
    }
}

impl std::fmt::Display for Ticks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.2}t", self.as_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_ticks_round_trip() {
        assert_eq!(Ticks::from_ticks(5).as_f64(), 5.0);
        assert_eq!(Ticks::from_ticks(1).raw(), 256);
    }

    #[test]
    fn fractional_ticks_survive_to_the_256th() {
        // 4.633 ticks/block is the walk rate; it must not round to 5.
        let t = Ticks::from_f64(4.633);
        assert!((t.as_f64() - 4.633).abs() < 1.0 / 256.0);
        assert_ne!(t, Ticks::from_ticks(5));
    }

    /// A cost model that can emit a negative edge breaks admissibility with no
    /// visible symptom, so the clamp is pinned rather than incidental.
    #[test]
    fn negative_and_nan_costs_clamp_to_zero() {
        assert_eq!(Ticks::from_f64(-1.0), Ticks::ZERO);
        assert_eq!(Ticks::from_f64(f64::NAN), Ticks::ZERO);
        assert_eq!(Ticks::from_f64(f64::INFINITY), Ticks::ZERO);
    }

    #[test]
    fn saturating_addition_cannot_wrap_an_impossible_edge_into_a_cheap_one() {
        assert_eq!(
            Ticks::IMPOSSIBLE.saturating_add(Ticks::from_ticks(1)),
            Ticks::IMPOSSIBLE
        );
    }
}
