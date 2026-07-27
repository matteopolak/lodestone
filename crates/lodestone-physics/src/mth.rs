//! Bit-exact re-implementations of the `net.minecraft.util.Mth` helpers that
//! Minecraft's movement code actually calls.
//!
//! These deliberately mirror vanilla's *exact* arithmetic — including its
//! `float`/`double` widths and its lookup-table trigonometry — because the
//! client's reported positions are validated by the server's anti-cheat and any
//! divergence accumulates into rubber-banding. Where vanilla does something
//! numerically odd, we reproduce the odd thing rather than the "correct" thing.
//!
//! # Sine table
//!
//! `Mth.sin` is not `Math.sin`: it is a 65,536-entry `float` lookup table built
//! as `sin[i] = (float)Math.sin(i / 10430.378350470453)` (see `Mth.java` lines
//! 34–39 in the 26.2 reference source). The table is generated once and checked
//! in as [`sin_table`](crate::sin_table); it is **not** built at runtime. To
//! regenerate it, evaluate that expression for `i in 0..65536`, widen each
//! result to `f32`, and emit the raw bit patterns.
//!
//! The table is validated **element-wise against the real JVM** rather than by
//! any hash: `oracle-java/SinOracle.java` dumps the JVM's `float` bits and the
//! `sin_table_matches_jvm_reference` test diffs all 65,536 entries against the
//! checked-in reference. This is strictly stronger than a checksum (it names the
//! offending index on failure) and needs no non-recomputable magic constant.

use crate::sin_table::SIN_TABLE_BITS;

/// `Mth.SIN_SCALE` — the quantization constant for the sine table.
pub const SIN_SCALE: f64 = 10430.378350470453;

/// Lazily-materialised sine table as `f32` values, reconstructed from the
/// checked-in bit patterns so no rounding can creep in at load time.
fn sin_table() -> &'static [f32; 65536] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[f32; 65536]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0.0f32; 65536];
        for (dst, &bits) in t.iter_mut().zip(SIN_TABLE_BITS.iter()) {
            *dst = f32::from_bits(bits);
        }
        t
    })
}

/// `Mth.sin(double)` — table lookup, **not** `f64::sin`.
///
/// Mirrors `SIN[(int)((long)(i * SIN_SCALE) & 65535L)]`.
#[must_use]
pub fn sin(i: f64) -> f32 {
    let idx = ((i * SIN_SCALE) as i64 & 65535) as usize;
    sin_table()[idx]
}

/// `Mth.cos(double)` — table lookup offset by a quarter turn.
///
/// Mirrors `SIN[(int)((long)(i * SIN_SCALE + 16384.0) & 65535L)]`.
#[must_use]
pub fn cos(i: f64) -> f32 {
    let idx = ((i * SIN_SCALE + 16384.0) as i64 & 65535) as usize;
    sin_table()[idx]
}

/// `Mth.floor(double)` → `(int)Math.floor(v)`.
#[must_use]
pub fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// `Mth.lfloor(double)` → `(long)Math.floor(v)`.
#[must_use]
pub fn lfloor(v: f64) -> i64 {
    v.floor() as i64
}

/// `Mth.ceil(double)` → `(int)Math.ceil(v)`.
#[must_use]
pub fn ceil(v: f64) -> i32 {
    v.ceil() as i32
}

/// `java.lang.Math.min(double, double)`: propagates `NaN`, unlike Rust's
/// `f64::min` which returns the non-`NaN` operand.
#[must_use]
pub fn java_min_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// `java.lang.Math.min(float, float)`.
#[must_use]
pub fn java_min_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else {
        a.min(b)
    }
}

/// `Mth.clamp(double, double, double)`.
///
/// Note vanilla's asymmetric form: `value < min ? min : Math.min(value, max)`.
/// This matters at `NaN` and negative-zero edges (Java's `Math.min` propagates
/// `NaN`), so we reproduce it exactly rather than using `f64::clamp`.
#[must_use]
pub fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else {
        java_min_f64(value, max)
    }
}

/// `Mth.clamp(float, float, float)`.
#[must_use]
pub fn clamp_f32(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else {
        java_min_f32(value, max)
    }
}

/// `Mth.clamp(int, int, int)` → `min(max(value, min), max)`.
#[must_use]
pub fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

/// `Mth.lerp(double, double, double)` → `p0 + alpha * (p1 - p0)`.
///
/// Kept in vanilla's expression order; do not algebraically simplify.
#[must_use]
pub fn lerp_f64(alpha: f64, p0: f64, p1: f64) -> f64 {
    p0 + alpha * (p1 - p0)
}

/// `Mth.lerp(float, float, float)` → `p0 + alpha * (p1 - p0)`.
#[must_use]
pub fn lerp_f32(alpha: f32, p0: f32, p1: f32) -> f32 {
    p0 + alpha * (p1 - p0)
}

/// `Mth.frac(double)` → `num - lfloor(num)` (note the `long` floor).
#[must_use]
pub fn frac_f64(num: f64) -> f64 {
    num - lfloor(num) as f64
}

/// `Mth.frac(float)` → `num - floor(num)`.
#[must_use]
pub fn frac_f32(num: f32) -> f32 {
    num - floor(num as f64) as f32
}

/// `Mth.wrapDegrees(double)`.
#[must_use]
pub fn wrap_degrees_f64(angle: f64) -> f64 {
    let mut a = angle % 360.0;
    if a >= 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

/// `Mth.wrapDegrees(float)`.
#[must_use]
pub fn wrap_degrees_f32(angle: f32) -> f32 {
    let mut a = angle % 360.0;
    if a >= 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

/// `Mth.square(double)`.
#[must_use]
pub fn square_f64(v: f64) -> f64 {
    v * v
}

/// `computeModifiedFriction` from `LivingEntity` (line 515): a private helper
/// used by air-drag and block-friction modifiers.
///
/// `Mth.clamp(1.0F - (1.0F - friction) * modifier, 0.0F, 1.0F)` — all in `f32`.
#[must_use]
pub fn compute_modified_friction(friction: f32, modifier: f32) -> f32 {
    clamp_f32(1.0 - (1.0 - friction) * modifier, 0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_table_matches_jvm_reference() {
        // Authoritative regression guard: every one of the 65,536 checked-in
        // `f32` bit patterns must equal the raw `Float.floatToRawIntBits` values
        // dumped by the real JVM (`oracle-java/SinOracle.java`, temurin:25-jdk —
        // the runtime vanilla 26.2 runs on). This is strictly stronger than a
        // hash: on failure it names the exact divergent index. Regenerate the
        // reference with the documented Docker one-liner in that oracle's dir.
        let reference = include_str!("../tests/support/sin_reference_jvm.txt");
        let jvm: Vec<u32> = reference
            .split_ascii_whitespace()
            .map(|t| t.parse().expect("reference is decimal u32 per line"))
            .collect();
        assert_eq!(jvm.len(), 65536, "JVM reference must have 65536 entries");
        assert_eq!(SIN_TABLE_BITS.len(), 65536);
        for (i, (&rust, &java)) in SIN_TABLE_BITS.iter().zip(jvm.iter()).enumerate() {
            assert_eq!(rust, java, "sin table diverges from JVM at index {i}");
        }
    }

    #[test]
    fn sin_matches_known_anchors() {
        // sin(0) is exactly 0; the table's quarter-turn index yields exactly 1.
        assert_eq!(sin(0.0), 0.0);
        // Mth.sin(PI/2) via the table hits the peak entry (== 1.0f).
        assert_eq!(sin(std::f64::consts::FRAC_PI_2), 1.0);
        // cos(0) == sin(quarter turn) == 1.0.
        assert_eq!(cos(0.0), 1.0);
    }

    #[test]
    fn sin_is_table_lookup_not_libm() {
        // A value where the LUT and true sine differ in the low bits proves we
        // are using the table, not f64::sin. Vanilla's table is deliberately
        // coarse; here we simply assert determinism against a recomputed entry.
        let i = 0.37f64;
        let idx = ((i * SIN_SCALE) as i64 & 65535) as usize;
        assert_eq!(sin(i), f32::from_bits(SIN_TABLE_BITS[idx]));
    }

    #[test]
    fn wrap_degrees_edges() {
        assert_eq!(wrap_degrees_f64(180.0), -180.0);
        assert_eq!(wrap_degrees_f64(-180.0), -180.0);
        assert_eq!(wrap_degrees_f64(360.0), 0.0);
        assert_eq!(wrap_degrees_f32(190.0), -170.0);
    }

    #[test]
    fn clamp_matches_vanilla_form() {
        assert_eq!(clamp_f64(5.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp_f64(-5.0, 0.0, 1.0), 0.0);
        // NaN: vanilla returns Math.min(NaN, max) == NaN when NaN<min is false.
        assert!(clamp_f64(f64::NAN, 0.0, 1.0).is_nan());
    }

    #[test]
    fn compute_modified_friction_identity() {
        // Default modifier 1.0 leaves friction unchanged.
        assert_eq!(compute_modified_friction(0.6, 1.0), 0.6);
        assert_eq!(compute_modified_friction(0.91, 1.0), 0.91);
    }
}
