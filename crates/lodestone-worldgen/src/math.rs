//! `net.minecraft.util.Mth` helpers needed by the noise stack, reproduced from
//! their published one-line definitions. Kept separate so the noise code reads
//! like the reference and float semantics stay obvious.

use std::sync::LazyLock;

use crate::rng::RandomSource;

/// `Mth.SIN_SCALE` = `65536 / (2*PI)` as a double.
const SIN_SCALE: f64 = 10_430.378_350_470_453;

/// `Mth.SIN` — the 65536-entry float sine lookup table, built exactly as
/// vanilla: `SIN[i] = (float)Math.sin(i / SIN_SCALE)`. Verified bit-for-bit
/// against the JVM's own table (`mth_parity`).
static SIN: LazyLock<Vec<f32>> =
    LazyLock::new(|| (0..65536).map(|i| ((f64::from(i) / SIN_SCALE).sin()) as f32).collect());

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
pub fn random_between_inclusive<R: RandomSource>(random: &mut R, min: i32, max_inclusive: i32) -> i32 {
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
