//! Conservative value ranges of a density function, in 32-bit float.
//!
//! Compile-time shortcuts (a `min`/`max` whose operands cannot overlap, and the
//! early-out thresholds of the scalar `min`/`max` samplers) are decided from these
//! ranges, so they are part of the numeric contract: a range that is a little too
//! narrow changes which operand a scalar query evaluates.

/// A closed range, or the "not an interval" marker (`NaI`) that propagates through
/// every operation. `NaI` is a distinct value, not an interval containing NaN.
#[derive(Clone, Copy, Debug)]
pub struct Interval {
    min: f32,
    max: f32,
    nai: bool,
}

impl PartialEq for Interval {
    fn eq(&self, other: &Self) -> bool {
        self.nai == other.nai && (self.nai || (self.min == other.min && self.max == other.max))
    }
}

impl Interval {
    pub const NAI: Self = Self { min: f32::NAN, max: f32::NAN, nai: true };
    pub const INFINITE: Self = Self { min: f32::NEG_INFINITY, max: f32::INFINITY, nai: false };

    /// A range with ordered, non-NaN bounds; anything else is `NaI` (the reference
    /// rejects such input, which only reachable data never produces).
    #[must_use]
    pub fn of(min: f32, max: f32) -> Self {
        if min.is_nan() || max.is_nan() || max < min { Self::NAI } else { Self { min, max, nai: false } }
    }

    #[must_use]
    pub fn symmetric(range: f32) -> Self { Self::of(-range, range) }

    #[must_use]
    pub fn exact(value: f32) -> Self { Self::of(value, value) }

    #[must_use]
    pub fn is_nai(self) -> bool { self.nai }
    #[must_use]
    pub fn min(self) -> f32 { self.min }
    #[must_use]
    pub fn max(self) -> f32 { self.max }

    #[must_use]
    pub fn contains(self, value: f32) -> bool { !self.nai && value >= self.min && value <= self.max }

    #[must_use]
    pub fn encapsulating_all(intervals: &[Self]) -> Self {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for i in intervals.iter().filter(|i| !i.nai) {
            min = jmin(i.min, min);
            max = jmax(i.max, max);
        }
        if max < min { Self::NAI } else { Self::of(min, max) }
    }

    #[must_use]
    pub fn encapsulating2(first: f32, second: f32) -> Self {
        match (first.is_nan(), second.is_nan()) {
            (true, true) => Self::NAI,
            (true, false) => Self::exact(second),
            (false, true) => Self::exact(first),
            (false, false) => Self::of(jmin(first, second), jmax(first, second)),
        }
    }

    #[must_use]
    pub fn add(l: Self, r: Self) -> Self {
        let (min, max) = (l.min + r.min, l.max + r.max);
        if !min.is_nan() && !max.is_nan() { Self::of(min, max) } else { Self::NAI }
    }

    #[must_use]
    pub fn sub(l: Self, r: Self) -> Self {
        let (min, max) = (l.min - r.max, l.max - r.min);
        if !min.is_nan() && !max.is_nan() { Self::of(min, max) } else { Self::NAI }
    }

    #[must_use]
    pub fn mul(l: Self, r: Self) -> Self {
        if l.nai || r.nai { return Self::NAI; }
        let a = mul_bound(l.min, r.min);
        let b = mul_bound(l.min, r.max);
        let c = mul_bound(l.max, r.min);
        let d = mul_bound(l.max, r.max);
        Self::of(jmin(jmin(a, b), jmin(c, d)), jmax(jmax(a, b), jmax(c, d)))
    }

    #[must_use]
    pub fn reciprocal(i: Self) -> Self {
        if i.nai || (i.min == 0.0 && i.max == 0.0) { return Self::NAI; }
        if !i.contains(0.0) {
            Self::of(1.0 / i.max, 1.0 / i.min)
        } else if i.max == 0.0 {
            Self::of(f32::NEG_INFINITY, 1.0 / i.min)
        } else if i.min == 0.0 {
            Self::of(1.0 / i.max, f32::INFINITY)
        } else {
            Self::INFINITE
        }
    }

    #[must_use]
    pub fn div(l: Self, r: Self) -> Self { Self::mul(l, Self::reciprocal(r)) }

    #[must_use]
    pub fn min_of(l: Self, r: Self) -> Self {
        if l.nai || r.nai { Self::NAI } else { Self::of(jmin(l.min, r.min), jmin(l.max, r.max)) }
    }

    #[must_use]
    pub fn max_of(l: Self, r: Self) -> Self {
        if l.nai || r.nai { Self::NAI } else { Self::of(jmax(l.min, r.min), jmax(l.max, r.max)) }
    }

    #[must_use]
    pub fn clamp(i: Self, min: f32, max: f32) -> Self {
        if i.nai { Self::NAI }
        else if i.min >= max { Self::of(max, max) }
        else if i.max <= min { Self::of(min, min) }
        else { Self::of(jmax(i.min, min), jmin(i.max, max)) }
    }

    #[must_use]
    pub fn abs(i: Self) -> Self {
        if i.nai { return Self::NAI; }
        let max = jmax(i.min.abs(), i.max.abs());
        if i.contains(0.0) { Self::of(0.0, max) } else { Self::of(jmin(i.min.abs(), i.max.abs()), max) }
    }

    #[must_use]
    pub fn square(i: Self) -> Self {
        if i.nai { return Self::NAI; }
        let max = jmax(i.min * i.min, i.max * i.max);
        if i.contains(0.0) { Self::of(0.0, max) } else { Self::of(jmin(i.min * i.min, i.max * i.max), max) }
    }

    /// Applies a monotonic operator to both bounds.
    #[must_use]
    pub fn map_monotonic(i: Self, op: impl Fn(f32) -> f32) -> Self {
        if i.nai { return Self::NAI; }
        let (a, b) = (op(i.min), op(i.max));
        if a.is_nan() || b.is_nan() { Self::NAI } else { Self::of(jmin(a, b), jmax(a, b)) }
    }

    #[must_use]
    pub fn log(i: Self) -> Self {
        if i.max < 0.0 { return Self::NAI; }
        Self::map_monotonic(Self::max_of(i, Self::exact(0.0)), |x| f64::from(x).ln() as f32)
    }

    #[must_use]
    pub fn sign(i: Self) -> Self {
        if i.nai { return Self::NAI; }
        if i.min == i.max { return Self::exact(signum(i.min)); }
        if i.contains(0.0) {
            if i.min == 0.0 { Self::of(0.0, 1.0) } else if i.max == 0.0 { Self::of(-1.0, 0.0) } else { Self::of(-1.0, 1.0) }
        } else {
            Self::exact(if i.min > 0.0 { 1.0 } else { -1.0 })
        }
    }

    #[must_use]
    pub fn pow(base: Self, exponent: Self) -> Self {
        if base.nai || exponent.nai { return Self::NAI; }
        if base.min == base.max { return pow_point(base.min, exponent); }
        let mut result = Self::encapsulating_all(&[pow_point(base.min, exponent), pow_point(base.max, exponent)]);
        if base.contains(0.0) {
            if base.max > 0.0 { result = Self::encapsulating_all(&[result, pow_point(0.0, exponent)]); }
            if base.min < 0.0 { result = Self::encapsulating_all(&[result, pow_point(-0.0, exponent)]); }
        }
        result
    }

    #[must_use]
    pub fn lerp(alpha: Self, first: Self, second: Self) -> Self {
        if alpha.nai || first.nai || second.nai { return Self::NAI; }
        Self::encapsulating_all(&[
            lerp_point(alpha, first.min, second.min),
            lerp_point(alpha, first.max, second.min),
            lerp_point(alpha, first.min, second.max),
            lerp_point(alpha, first.max, second.max),
        ])
    }
}

fn lerp_point(alpha: Interval, first: f32, second: f32) -> Interval {
    if alpha.nai || first.is_nan() || second.is_nan() { return Interval::NAI; }
    if first.is_finite() && second.is_finite() {
        let bound = |a: f32| first + mul_bound(a, second - first);
        Interval::encapsulating2(bound(alpha.min), bound(alpha.max))
    } else {
        if first == second { return Interval::exact(first); }
        let bound = |a: f32| {
            let first_part = mul_bound(1.0 - a, first);
            let second_part = mul_bound(a, second);
            if !first_part.is_infinite() || !second_part.is_infinite() { first_part + second_part }
            else if a <= 0.0 { if second > first { f32::NEG_INFINITY } else { f32::INFINITY } }
            else if a >= 1.0 { if second > first { f32::INFINITY } else { f32::NEG_INFINITY } }
            else { f32::NAN }
        };
        let (lo, hi) = (bound(alpha.min), bound(alpha.max));
        if !lo.is_nan() && !hi.is_nan() { Interval::encapsulating2(lo, hi) } else { Interval::NAI }
    }
}

fn pow_point(base: f32, exponent: Interval) -> Interval {
    if base.is_nan() || exponent.nai { return Interval::NAI; }
    if exponent.min == exponent.max {
        let value = f64::from(base).powf(f64::from(exponent.min)) as f32;
        return if value.is_nan() { Interval::NAI } else { Interval::exact(value) };
    }
    if base == 0.0 {
        return Interval::mul(pow_zero_base(exponent), Interval::exact(1.0_f32.copysign(base)));
    }
    if base == 1.0 { return Interval::exact(1.0); }
    if base > 0.0 { pow_positive_base(base, exponent) } else { pow_negative_base(base, exponent) }
}

fn pf(base: f32, e: f32) -> f32 { f64::from(base).powf(f64::from(e)) as f32 }

fn pow_positive_base(base: f32, e: Interval) -> Interval {
    if e.min.is_finite() && e.max.is_finite() {
        Interval::encapsulating2(pf(base, e.min), pf(base, e.max))
    } else if e.min.is_infinite() && e.max.is_infinite() {
        Interval::of(0.0, f32::INFINITY)
    } else if e.min.is_infinite() {
        if base < 1.0 { Interval::of(pf(base, e.max), f32::INFINITY) } else { Interval::of(0.0, pf(base, e.max)) }
    } else if base < 1.0 {
        Interval::of(0.0, pf(base, e.min))
    } else {
        Interval::of(pf(base, e.min), f32::INFINITY)
    }
}

fn pow_zero_base(e: Interval) -> Interval {
    if e.contains(0.0) {
        if e.max == 0.0 { Interval::of(1.0, f32::INFINITY) }
        else if e.min == 0.0 { Interval::of(0.0, 1.0) }
        else { Interval::of(0.0, f32::INFINITY) }
    } else if e.max < 0.0 {
        Interval::exact(f32::INFINITY)
    } else {
        Interval::exact(0.0)
    }
}

fn pow_negative_base(base: f32, e: Interval) -> Interval {
    let min_int = e.min.ceil();
    let max_int = e.max.floor();
    if max_int < min_int { return Interval::NAI; }
    let to_min = pf(base, min_int);
    let to_max = pf(base, max_int);
    let mut result = Interval::encapsulating2(to_min, to_max);
    if min_int.is_infinite() {
        result = Interval::encapsulating_all(&[result, Interval::exact(-to_min)]);
    } else if min_int + 1.0 < max_int {
        result = Interval::encapsulating_all(&[result, Interval::exact(pf(base, min_int + 1.0))]);
    }
    if max_int.is_infinite() {
        result = Interval::encapsulating_all(&[result, Interval::exact(-to_max)]);
    } else if max_int - 1.0 > min_int {
        result = Interval::encapsulating_all(&[result, Interval::exact(pf(base, max_int - 1.0))]);
    }
    result
}

fn mul_bound(l: f32, r: f32) -> f32 { if l != 0.0 && r != 0.0 { l * r } else { 0.0 } }

/// `Math.signum` for floats: zero and NaN pass through unchanged.
#[must_use]
pub fn signum(v: f32) -> f32 { if v == 0.0 || v.is_nan() { v } else { v.signum() } }

/// `Math.min(float, float)`: NaN wins, and `-0.0` is smaller than `+0.0`.
#[must_use]
pub fn jmin(a: f32, b: f32) -> f32 {
    if a.is_nan() { return a; }
    if b.is_nan() { return b; }
    if a == 0.0 && b == 0.0 { return f32::from_bits(a.to_bits() | b.to_bits()); }
    if a <= b { a } else { b }
}

/// `Math.max(float, float)`: NaN wins, and `+0.0` is larger than `-0.0`.
#[must_use]
pub fn jmax(a: f32, b: f32) -> f32 {
    if a.is_nan() { return a; }
    if b.is_nan() { return b; }
    if a == 0.0 && b == 0.0 { return f32::from_bits(a.to_bits() & b.to_bits()); }
    if a >= b { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nai_propagates_and_is_not_an_ordinary_interval() {
        assert!(Interval::add(Interval::NAI, Interval::exact(1.0)).min().is_nan());
        assert!(Interval::mul(Interval::of(1.0, 2.0), Interval::NAI).is_nai());
        assert!(!Interval::INFINITE.is_nai());
    }

    #[test]
    fn mul_treats_zero_times_infinity_as_zero() {
        let r = Interval::mul(Interval::of(0.0, 1.0), Interval::INFINITE);
        assert_eq!((r.min(), r.max()), (f32::NEG_INFINITY, f32::INFINITY));
        assert_eq!(Interval::mul(Interval::exact(0.0), Interval::INFINITE), Interval::exact(0.0));
    }

    #[test]
    fn clamp_collapses_when_input_lies_outside_the_bounds() {
        assert_eq!(Interval::clamp(Interval::of(5.0, 6.0), -1.0, 1.0), Interval::exact(1.0));
        assert_eq!(Interval::clamp(Interval::of(-6.0, -5.0), -1.0, 1.0), Interval::exact(-1.0));
    }

    #[test]
    fn java_min_max_order_signed_zero() {
        assert_eq!(jmin(0.0, -0.0).to_bits(), (-0.0_f32).to_bits());
        assert_eq!(jmax(-0.0, 0.0).to_bits(), 0);
    }
}
