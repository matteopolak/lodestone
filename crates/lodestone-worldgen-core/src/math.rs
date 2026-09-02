//! Vanilla's own math-helper functions needed by the noise stack, reproduced from
//! their published one-line definitions. Kept separate so the noise code reads
//! like the reference and float semantics stay obvious.

use std::sync::LazyLock;

use crate::rng::RandomSource;

/// `Mth.SIN_SCALE` = `65536 / (2*PI)` as a double.
const SIN_SCALE: f64 = 10_430.378_350_470_453;

/// `Mth.SIN` — the 65536-entry float sine lookup table, built exactly as
/// vanilla: `SIN[i] = (float)Math.sin(i / SIN_SCALE)`. Verified bit-for-bit
/// against the JVM's own table (`mth_parity`).
///
/// # The 1-ulp question, and why this one is safe
///
/// `Math.sin` is specified only to within 1 ulp, and this reproduces it with
/// Rust's `f64::sin` (the platform libm), so the two are not guaranteed to
/// agree by contract. Two things make that acceptable here rather than a
/// silent cross-platform divergence:
///
/// * **The narrowing cast is a ~29-bit margin.** The `f64` sine is immediately
///   cast to `f32`, so a last-place `f64` disagreement changes the stored entry
///   only when the true value sits on an `f32` rounding boundary.
/// * **A surviving disagreement is detected, not silent.** `mth_parity`'s
///   `mth_sin_table_matches_jvm_bit_for_bit` compares **all 65536** entries
///   against the game's own reflected `SIN` field and asserts it checked 65536
///   of them, so a libm that crossed a boundary for even one `i` fails loudly
///   on the machine where it happens.
///
/// Note that `Mth.sin`/`Mth.cos` themselves — the *lookup*, below — are exactly
/// reproducible: they are an integer index into this table and involve no
/// transcendental at all. The 1-ulp exposure is confined to building the table.
static SIN: LazyLock<Vec<f32>> = LazyLock::new(|| {
    (0..65536)
        .map(|i| ((f64::from(i) / SIN_SCALE).sin()) as f32)
        .collect()
});

/// `Mth.sin(double)` — table lookup, `SIN[(int)((long)(d * SIN_SCALE) & 65535)]`.
#[must_use]
pub fn sin(d: f64) -> f32 {
    SIN[((d * SIN_SCALE) as i64 & 0xFFFF) as usize]
}

/// `Mth.cos(double)` — `SIN[(int)((long)(d * SIN_SCALE + 16384.0) & 65535)]`.
#[must_use]
pub fn cos(d: f64) -> f32 {
    SIN[((d * SIN_SCALE + 16384.0) as i64 & 0xFFFF) as usize]
}

/// `Mth.abs(float)` = `v >= 0.0F ? v : -v` (keeps `-0.0` like vanilla).
#[must_use]
pub fn abs_f32(v: f32) -> f32 {
    if v >= 0.0 { v } else { -v }
}

/// `Mth.randomBetween(random, min, maxExclusive)` = `nextFloat()*(max-min)+min`.
#[must_use]
pub fn random_between<R: RandomSource>(random: &mut R, min: f32, max_exclusive: f32) -> f32 {
    random.next_float() * (max_exclusive - min) + min
}

/// `Mth.randomBetweenInclusive(random, min, maxInclusive)`.
#[must_use]
pub fn random_between_inclusive<R: RandomSource>(
    random: &mut R,
    min: i32,
    max_inclusive: i32,
) -> i32 {
    random.next_int_bounded(max_inclusive - min + 1) + min
}

/// `Mth.floor(double)` = `(int)Math.floor(v)`.
#[must_use]
pub fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// `Mth.lfloor(double)` = `(long)Math.floor(v)`.
#[must_use]
pub fn lfloor(v: f64) -> i64 {
    v.floor() as i64
}

/// `Mth.ceil(double)` / `Mth.ceil(float)` = `(int)Math.ceil(v)`. Pass a widened
/// `f32` (`f as f64`) to match the float overload exactly.
#[must_use]
pub fn ceil(v: f64) -> i32 {
    v.ceil() as i32
}

/// `Math.round(double)` = `(long) Math.floor(v + 0.5)` for any value this
/// engine's noise stack actually produces (the JLS's true definition only
/// diverges from that formula within `0.5` of `Long.MAX_VALUE`/`MIN_VALUE`,
/// never reached by a noise sample scaled by a handful of units). **Not**
/// [`f64::round`] — Rust rounds half *away from zero*
/// (`(-0.5_f64).round() == -1.0`), vanilla rounds half *up*
/// (`Math.round(-0.5) == 0`), and this repo has no existing helper for the
/// difference (`SurfaceSystem.getBand`'s `clayBandsOffsetNoise` rounding is
/// this crate's first use of `Math.round`). The two formulas agree
/// everywhere except exactly on a `.5` boundary, which a continuous noise
/// sample hits with probability zero in practice — reproduced anyway,
/// because "practically never" is not "provably never".
#[must_use]
pub fn round(v: f64) -> i32 {
    floor(v + 0.5)
}

/// `2^k` for an integer `k`, computed exactly by exponent-field construction.
///
/// # Why this exists rather than `powf`/`powi`
///
/// Vanilla writes `Math.pow(2.0, k)` with an integer `k` in four places that
/// feed terrain — vanilla's own Perlin-noise class's `lowestFreqInputFactor` /
/// `lowestFreqValueFactor` and its own multi-octave simplex-noise class's
/// `highestFreq*` twins. **`java.lang.Math.pow` is specified only to within 1
/// ulp**, and `lowestFreqInputFactor` multiplies the noise *input* coordinate,
/// so a last-place difference there is not a rounding curiosity: it moves every
/// sample position in that octave. The same is true of Rust's side —
/// `f64::powi` is not specified to be exactly rounded either, and expands to a
/// repeated-squaring sequence whose exactness is a property of the
/// implementation rather than of the contract.
///
/// The resolution is that the *value* is not in doubt even though both spellings
/// of the computation are: `2^k` is exactly representable in `f64` for every
/// `k` in the normal range, so there is exactly one right answer and it can be
/// written down directly instead of computed. This function does that, which
/// takes our side of these four constants off the platform libm entirely.
///
/// [`tests::exp2_exact_matches_powi_across_the_whole_normal_range`] gates the
/// exact form against the float form for all 2,046 normal exponents — the shape
/// U9 used for `Climate.RTree`'s `Math.pow(6, …)` bucket size.
///
/// # The residual exposure, stated rather than implied
///
/// This removes *our* dependence on a 1-ulp operation; it cannot remove Java's.
/// A JVM whose `Math.pow(2.0, k)` returned `2^k ± 1 ulp` would disagree with
/// this function — but it would also disagree with every other JVM, and vanilla
/// running on it would generate different terrain from vanilla everywhere else.
/// "Exactly `2^k`" is therefore the only value that can be called *the* vanilla
/// value, and that is what this returns.
///
/// # Panics
///
/// Debug-asserts `k` is in the normal exponent range. Subnormal results are
/// deliberately not supported: no octave count or first-octave value in
/// vanilla's noise data comes within three orders of magnitude of the bound, so
/// a `k` outside it means a caller has computed an exponent wrongly, and
/// silently returning a subnormal would hide that.
#[must_use]
pub fn exp2_exact(k: i32) -> f64 {
    debug_assert!(
        (-1022..=1023).contains(&k),
        "exp2_exact({k}) is outside the normal f64 exponent range; a noise \
         octave exponent should never come near it"
    );
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `Mth.smoothstep(x)` = `x^3 (x (6x - 15) + 10)` (the quintic fade).
#[must_use]
pub fn smoothstep(x: f64) -> f64 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// `Mth.lerp(a, p0, p1)` = `p0 + a (p1 - p0)`.
#[must_use]
pub fn lerp(a: f64, p0: f64, p1: f64) -> f64 {
    p0 + a * (p1 - p0)
}

/// `Mth.lerp2` — bilinear, matching vanilla's exact nesting order.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn lerp2(a1: f64, a2: f64, x00: f64, x10: f64, x01: f64, x11: f64) -> f64 {
    lerp(a2, lerp(a1, x00, x10), lerp(a1, x01, x11))
}

/// `Mth.lerp3` — trilinear, matching vanilla's exact nesting order.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn lerp3(
    a1: f64,
    a2: f64,
    a3: f64,
    x000: f64,
    x100: f64,
    x010: f64,
    x110: f64,
    x001: f64,
    x101: f64,
    x011: f64,
    x111: f64,
) -> f64 {
    lerp(
        a3,
        lerp2(a1, a2, x000, x100, x010, x110),
        lerp2(a1, a2, x001, x101, x011, x111),
    )
}

/// `Mth.clamp(v, lo, hi)`.
#[must_use]
pub fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// `Mth.inverseLerp(value, min, max)` = `(value - min) / (max - min)`.
#[must_use]
pub fn inverse_lerp(value: f64, min: f64, max: f64) -> f64 {
    (value - min) / (max - min)
}

/// `Mth.clampedLerp(factor, min, max)`.
#[must_use]
pub fn clamped_lerp(factor: f64, min: f64, max: f64) -> f64 {
    if factor < 0.0 {
        min
    } else if factor > 1.0 {
        max
    } else {
        lerp(factor, min, max)
    }
}

/// `Mth.clampedMap(value, from_min, from_max, to_min, to_max)`.
#[must_use]
pub fn clamped_map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    clamped_lerp(inverse_lerp(value, from_min, from_max), to_min, to_max)
}

/// `Mth.map(value, from_min, from_max, to_min, to_max)` — the **unclamped**
/// remap used by surface rules (`vertical_gradient` probability and
/// `stone_depth` secondary depth).
#[must_use]
pub fn map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    lerp(inverse_lerp(value, from_min, from_max), to_min, to_max)
}

#[cfg(test)]
mod tests {
    use super::exp2_exact;

    /// [`exp2_exact`] must agree with the float spelling it replaces on every
    /// normal exponent — all 2,046 of them.
    ///
    /// This is the U9 shape: where vanilla routes a *structural or continuous*
    /// value through a 1-ulp-specified `java.lang.Math` transcendental, compute
    /// it with an exactly-specified formulation and gate that against the float
    /// form over the whole domain, rather than trusting either spelling.
    ///
    /// A disagreement here means one of the two is wrong for that exponent, and
    /// the bit-construction is the one with a proof, so a failure is a report
    /// about `powi` on this platform — which is exactly the thing worth knowing
    /// and exactly what a transliteration would have hidden.
    #[test]
    fn exp2_exact_matches_powi_across_the_whole_normal_range() {
        let mut checked = 0u32;
        for k in -1022..=1023i32 {
            let exact = exp2_exact(k);
            let via_powi = 2f64.powi(k);
            assert_eq!(
                exact.to_bits(),
                via_powi.to_bits(),
                "2^{k}: exp2_exact gave {exact:e} ({:016x}), powi gave \
                 {via_powi:e} ({:016x})",
                exact.to_bits(),
                via_powi.to_bits()
            );
            checked += 1;
        }
        assert_eq!(
            checked, 2_046,
            "the sweep must cover every normal exponent; a narrowed range \
             would pass while measuring less than it claims"
        );

        // Anchors, so the test still says something if the loop above is ever
        // reduced to a tautology: these are the values by inspection.
        assert_eq!(exp2_exact(0), 1.0);
        assert_eq!(exp2_exact(1), 2.0);
        assert_eq!(exp2_exact(-1), 0.5);
        assert_eq!(exp2_exact(10), 1024.0);
        assert_eq!(exp2_exact(-15), 1.0 / 32_768.0);
    }

    /// The octave exponents vanilla's own noise data actually produces, so the
    /// gate above is known to cover the domain in use rather than an abstract
    /// range. `first_octave` in 26.2's `worldgen/noise/*.json` spans roughly
    /// -15..0 and no amplitude list is longer than a couple of dozen entries.
    #[test]
    fn the_octave_domain_in_use_is_well_inside_the_gated_range() {
        for first_octave in -32..=0i32 {
            let f = exp2_exact(-(-first_octave));
            assert!(f.is_normal() || f == 1.0, "2^{first_octave} is not normal");
        }
        for octaves in 1..=32i32 {
            let num = exp2_exact(octaves - 1);
            let den = exp2_exact(octaves) - 1.0;
            assert!(num.is_normal() && den > 0.0, "octaves = {octaves}");
        }
    }
}
