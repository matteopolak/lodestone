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

/// `java.lang.Math.max(double, double)`: propagates `NaN`, unlike Rust's
/// `f64::max` which returns the non-`NaN` operand.
#[must_use]
pub fn java_max_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// `Mth.absMax(double, double)` (`Mth.java:134-136`) —
/// `Math.max(Math.abs(a), Math.abs(b))`, i.e. the **larger magnitude of the two
/// components**, not the length of the vector they form.
///
/// The distinction is the whole story of `Entity.push(Entity)`, which normalises
/// its horizontal separation by `sqrt(absMax(dx, dz))` where a reader expects
/// `sqrt(dx*dx + dz*dz)`. For `(0.15, 0.08)` those are `0.3873…` and `0.4123…` —
/// a 6% error in the push direction *and* magnitude, on every pair, every tick.
/// Routed through [`java_max_f64`] so a `NaN` component poisons the result the way
/// Java's does (which is what makes `Entity.push`'s `dd >= 0.01F` gate reject it).
#[must_use]
pub fn abs_max(a: f64, b: f64) -> f64 {
    java_max_f64(a.abs(), b.abs())
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

/// `org.joml.Math.invsqrt(float)` — the float inverse square root that
/// `Mth.invSqrt(float)` delegates to (`Mth.java:440-442`).
///
/// **Not** the Quake-style magic-constant Newton iterate: JOML 1.10.8 (the
/// version 26.2 ships in `libraries/org/joml/joml/1.10.8`) compiles
/// `invsqrt(float)` to exactly
///
/// ```text
/// fconst_1
/// fload_0
/// f2d                       // (double)x — IEEE widening, lossless
/// invokestatic Math.sqrt    // correctly-rounded double sqrt
/// d2f                       // round back to float
/// fdiv                      // 1.0f / (float)sqrt(x)
/// ```
///
/// i.e. `1.0F / (float)Math.sqrt((double)x)`. The magic constant `0x5f375a86`
/// appears in *no* class file in that jar — a fast-inverse-sqrt port would be a
/// divergence of up to ~0.17% from the reference, verified by disassembling
/// `Math.class` from the 26.2 cache.
///
/// The only consumer is the client's own steering (`LocalPlayer.updateAutoJump`
/// normalises its look-ahead direction through it), so a divergence could never
/// be seen by the server's anti-cheat — but the `LocalPlayer` is the reference
/// for client-side movement, and a wrong look-ahead changes *when* auto-jump
/// fires, so the bits are reproduced exactly. All three widths (`f64::sqrt` is
/// IEEE-754 correctly rounded, like `Math.sqrt`; both roundings are
/// round-to-nearest-even, like `f2d`/`d2f`) match the JVM, so this is exact.
#[must_use]
pub fn inv_sqrt_f32(x: f32) -> f32 {
    1.0_f32 / (f64::from(x).sqrt() as f32)
}

/// `Math.signum(double)` — **not** Rust's [`f64::signum`].
///
/// The two disagree on zero, which is the whole reason this exists. Java returns
/// *the argument itself* for `±0.0` (so `signum(0.0) == 0.0` and
/// `signum(-0.0) == -0.0`), whereas Rust's `f64::signum` returns `1.0` for `0.0`
/// and `-1.0` for `-0.0`. `Player.maybeBackOffFromEdge` computes its step as
/// `Math.signum(deltaX) * 0.05`, so on a zero component Rust's version would
/// manufacture a `±0.05` step out of nothing.
///
/// It happens to be harmless *there* — the step is only read inside a
/// `while (deltaX != 0.0)` loop, which a zero component never enters — but the
/// discrepancy is exactly the kind that survives review and then bites a later
/// caller. NaN propagates in both.
#[must_use]
pub fn java_signum(v: f64) -> f64 {
    if v == 0.0 || v.is_nan() {
        v
    } else {
        v.signum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_signum_returns_the_argument_for_signed_zero() {
        // Rust's `f64::signum` returns ±1.0 here; Java's `Math.signum` does not.
        assert_eq!(java_signum(0.0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(java_signum(-0.0).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(java_signum(0.2), 1.0);
        assert_eq!(java_signum(-0.2), -1.0);
        assert!(java_signum(f64::NAN).is_nan());
    }

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

    #[test]
    fn inv_sqrt_matches_joml_reference_bits() {
        // Authoritative regression guard: `Mth.invSqrt(float)` delegates to
        // `org.joml.Math.invsqrt(float)` (Mth.java:440-442), which JOML 1.10.8
        // compiles to `1.0f / (float)Math.sqrt((double)x)` — NOT a magic-constant
        // Newton iterate. These raw `Float.floatToRawIntBits` values were dumped
        // by running the actual `joml-1.10.8.jar` from the 26.2 cache
        // (`org/joml/Math.class` disassembled to confirm the `fdiv`/`sqrt` form).
        // Asserting bits, not values, catches a subtly-wrong-but-close port
        // (e.g. fast-inverse-sqrt) that a 1e-7 tolerance would wave through.
        let cases: &[(f32, u32)] = &[
            (0.0001, 1120403456), // 100.0
            (0.001, 1107098481),  // 31.622774
            (0.01, 1092616192),   // 10.0, exact
            (0.1, 1078616770),    // 3.1622777
            (0.5, 1068827891),    // 1.4142135
            (1.0, 1065353216),    // 1.0, exact
            (2.0, 1060439283),    // 0.70710677
            (4.0, 1056964608),    // 0.5, exact
            (16.0, 1048576000),   // 0.25, exact
            (100.0, 1036831949),  // 0.1
            (0.0, 2139095040),    // +Inf
            (1.0E-30, 1482907561),
            (1.0E30, 646978941),
            (f32::INFINITY, 0),   // 0.0
        ];
        let mut mismatches = Vec::new();
        for &(x, want) in cases {
            let got = inv_sqrt_f32(x).to_bits();
            if got != want {
                mismatches.push(format!("inv_sqrt_f32({x}): got {got}, want {want} (JOML)"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "diverged from JOML:\n{}",
            mismatches.join("\n")
        );

        // A negative input is deliberately **not** in the bit table above,
        // and this is the one place the "assert bits, not values" rule has to
        // give way: the *sign* of a NaN produced by taking the square root of
        // a negative is architecture-specific, not a property of the port.
        // Measured — aarch64 yields the default quiet NaN `0x7FC00000`
        // (2143289344), x86_64 the "real indefinite" `0xFFC00000`
        // (4290772992), because SSE's `sqrtsd` sets the sign bit and AArch64's
        // `fsqrt` does not; `1.0 / NaN` then propagates whichever it got. A
        // raw-bits expectation transcribed on one of the two therefore fails
        // on the other for a reason that has nothing to do with `invSqrt` —
        // which is exactly what happened: this gate was green on the dev Macs
        // and red on every x86_64 CI runner.
        //
        // What is still worth asserting is everything a wrong port would get
        // wrong anyway: the result is NaN (not `Inf`, not a finite number
        // from a magic-constant Newton iterate), and it is *quiet*, so it
        // propagates rather than trapping.
        let negative = inv_sqrt_f32(-1.0);
        assert!(
            negative.is_nan(),
            "inv_sqrt_f32(-1.0) must be NaN like Math.sqrt of a negative double, got {negative}"
        );
        assert_eq!(
            negative.to_bits() & 0x7FFF_FFFF,
            0x7FC0_0000,
            "inv_sqrt_f32(-1.0) must be the canonical *quiet* NaN payload (sign ignored: it is \
             architecture-specific), got {:#010X}",
            negative.to_bits()
        );
    }
}
