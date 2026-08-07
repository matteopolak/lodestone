//! `ImprovedNoise` — a single 3D Perlin (improved) noise octave.
//!
//! Reproduces `net.minecraft.world.level.levelgen.synth.ImprovedNoise`: three
//! `nextDouble`-derived offsets, a Fisher–Yates permutation of `0..256` drawn
//! from the source, and the gradient-dot / trilinear-smoothstep sample. Only the
//! non-derivative `noise(x, y, z)` path (the one terrain uses) is implemented.
//!
//! ## Why this kernel is vectorised, and why that is parity-safe
//!
//! Unit 5 of [the worldgen rewrite plan] SIMDs the numeric kernels. This is the
//! one that measured worth doing: a release profile of the C_ss sweep attributes
//! **11.23% of all column CPU** to `noise_scaled` as *self* time — the largest
//! single leaf in the numeric core, and roughly a quarter of it arrives through
//! callers (`aquifer`, `surface`) that query one position at a time, so there is
//! no batched seam to exploit above this function. The vectorisation therefore
//! lives *inside* one call, where it needs no caller change and reaches every
//! call site.
//!
//! [`sample_and_lerp`](ImprovedNoise::sample_and_lerp) evaluates the eight
//! gradient dot products of one noise lattice cell in eight lanes. Those eight
//! corners are **independent positions** — the shape the plan's SIMD policy
//! permits — and every lane computes `gx * x + gy * y + gz * z` in the same
//! left-to-right order the scalar `dot` did, so **no reassociation is possible
//! and the result is bit-identical by construction**, not by tolerance.
//!
//! ## The premise that was false: the old kernel was *already* vectorised
//!
//! Do not read the change here as "scalar code became vector code". A
//! disassembly control on the pre-change binary found the shipped scalar kernel
//! already carried **17 `fmul.2d` and 15 `fadd.2d`** — LLVM had auto-vectorised
//! the eight gradient dots on its own. What it *also* carried, and this is where
//! the win actually came from, was **12 `sshll.2d` + 12 `scvtf.2d` per call**:
//! widening and converting the `i32` `GRADIENT` entries to `f64` on every single
//! sample. Storing the table as `f64` deletes all 24.
//!
//! Measured, four arms interleaved in one process against the same 65,536
//! positions, bit-identical throughout (median-of-paired ratios, reproducible
//! across runs — the absolute times on this shared machine are not):
//!
//! | arm | ratio vs shipped | ns/position |
//! |---|---|---|
//! | shipped scalar (`i32` table, auto-vectorised) | 1.000 | 5.955 |
//! | scalar code, `f64` table, **no `std::simd`** | 0.891 | 5.305 |
//! | this kernel: `std::simd`, 8 lanes | **0.791** | 4.712 |
//! | `std::simd` batched over 4 independent positions | 0.756 | 4.487 |
//!
//! So of the 1.26× total, about **1.12× needs no `std::simd` at all** (the table
//! type) and the remaining ~1.13× is what the explicit lane structure buys. That
//! is the honest accounting, and it is why the fourth row did **not** ship:
//! batching across positions is slightly faster still, but it only reaches the
//! ~42% of this kernel's cost that arrives through U4's interpolated-corner seam
//! — the aquifer and surface callers query one position at a time — so it would
//! deliver *less* total than the in-kernel form while requiring a lane-parallel
//! evaluator, and `Mul`'s `v1 == 0.0` short-circuit makes lane divergence in that
//! evaluator semantically observable, not merely awkward. Full numbers in
//! `docs/worldgen-simd-kernels.md`.
//!
//! The trilinear reduction that follows keeps vanilla's `Mth.lerp3` nesting
//! exactly. Only *sibling* nodes at the same level of that fixed tree share a
//! vector (4 lerps, then 2, then a scalar root); the tree's shape is untouched.
//! A horizontal add across the eight corners would be a different summation
//! order and a different world — see `docs/worldgen-simd-kernels.md`.
//!
//! Two things here are deliberately *not* optimised, both because they would
//! change bits rather than only cost:
//!
//! * **The multiply by a `0.0`/`±1.0` gradient component stays a multiply.**
//!   Replacing `0.0 * x` with `0.0` loses the sign of zero (`0.0 * -x` is
//!   `-0.0`), and `-0.0 + -0.0` is `-0.0` where `0.0 + -0.0` is `0.0`, so the
//!   difference can survive the lerp tree into the returned value. Equal under
//!   `==`, not equal under `to_bits`, and this repo's parity gates read bits.
//! * **No `mul_add`.** Fused rounding differs from vanilla's separate
//!   multiply-then-add. `StdFloat` is deliberately not imported.
//!
//! [the worldgen rewrite plan]: https://example.invalid/ (see docs/plans/worldgen-rewrite.md)

use std::simd::prelude::*;

use crate::math::{floor, lerp, smoothstep};
use crate::rng::RandomSource;

/// The 16 gradient vectors (`SimplexNoise.GRADIENT`), exactly as vanilla lists
/// them.
///
/// **Test-only, and that is the point.** The lanes read the transposed
/// [`GRADIENT_X`]/[`GRADIENT_Y`]/[`GRADIENT_Z`] tables instead, so this array's
/// job is to be the *independent* statement of the same data that
/// `tests::transposed_gradient_tables_agree_with_vanilla_layout` checks them
/// against. Keeping it in vanilla's row-major shape is what makes that check
/// worth running — a hand-written transpose is exactly the kind of table that is
/// silently wrong, and a wrong entry here shifts terrain without failing to
/// compile.
#[cfg(test)]
const GRADIENT: [[i32; 3]; 16] = [
    [1, 1, 0],
    [-1, 1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, 1],
    [1, 0, -1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, 1],
    [0, 1, -1],
    [0, -1, -1],
    [1, 1, 0],
    [0, -1, 1],
    [-1, 1, 0],
    [0, -1, -1],
];

/// X components of [`GRADIENT`], as `f64`, for lane gathering.
///
/// `f64` rather than `i32` so the lanes hold the value `f64::from(g[0])` would
/// have produced, with no per-lane integer conversion. Each entry is a small
/// integer and exactly representable, so this is a re-spelling of the same
/// constant, not a rounding of it.
const GRADIENT_X: [f64; 16] = [
    1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0,
];
/// Y components of [`GRADIENT`]. See [`GRADIENT_X`].
const GRADIENT_Y: [f64; 16] = [
    1.0, 1.0, -1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0,
];
/// Z components of [`GRADIENT`]. See [`GRADIENT_X`].
const GRADIENT_Z: [f64; 16] = [
    0.0, 0.0, 0.0, 0.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 0.0, 1.0, 0.0, -1.0,
];

/// A single improved-noise octave.
#[derive(Debug, Clone)]
pub struct ImprovedNoise {
    p: [u8; 256],
    /// X offset (`nextDouble * 256`).
    pub xo: f64,
    /// Y offset.
    pub yo: f64,
    /// Z offset.
    pub zo: f64,
}

impl ImprovedNoise {
    /// Builds an octave, consuming three `nextDouble`s and a 256-step shuffle
    /// from `random` in exactly vanilla's order.
    pub fn new<R: RandomSource>(random: &mut R) -> Self {
        let xo = random.next_double() * 256.0;
        let yo = random.next_double() * 256.0;
        let zo = random.next_double() * 256.0;
        let mut p = [0u8; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for i in 0..256usize {
            let offset = random.next_int_bounded(256 - i as i32) as usize;
            p.swap(i, i + offset);
        }
        Self { p, xo, yo, zo }
    }

    #[inline]
    fn perm(&self, x: i32) -> i32 {
        i32::from(self.p[(x & 0xFF) as usize])
    }


    /// Samples the noise at `(x, y, z)` (the `yScale = yFudge = 0` path).
    #[must_use]
    pub fn noise(&self, px: f64, py: f64, pz: f64) -> f64 {
        self.noise_scaled(px, py, pz, 0.0, 0.0)
    }

    /// The full `noise(x, y, z, yScale, yFudge)` used by the blended noise.
    #[must_use]
    pub fn noise_scaled(&self, px: f64, py: f64, pz: f64, y_scale: f64, y_fudge: f64) -> f64 {
        let x = px + self.xo;
        let y = py + self.yo;
        let z = pz + self.zo;
        let xf = floor(x);
        let yf = floor(y);
        let zf = floor(z);
        let xr = x - f64::from(xf);
        let yr = y - f64::from(yf);
        let zr = z - f64::from(zf);
        let yr_fudge = if y_scale != 0.0 {
            let fudge_limit = if y_fudge >= 0.0 && y_fudge < yr {
                y_fudge
            } else {
                yr
            };
            f64::from(floor(fudge_limit / y_scale + f64::from(1.0e-7_f32))) * y_scale
        } else {
            0.0
        };
        self.sample_and_lerp(xf, yf, zf, xr, yr - yr_fudge, zr, yr)
    }

    /// The eight gradient dot products of one lattice cell, then vanilla's
    /// `Mth.lerp3` reduction over them.
    ///
    /// Lane order is vanilla's `lerp3` argument order —
    /// `d000, d100, d010, d110, d001, d101, d011, d111` — so the reduction below
    /// is a direct transcription of the nesting `lerp3`/`lerp2` spell out, with
    /// siblings sharing a vector. See the module doc for why this is
    /// bit-identical rather than merely close.
    #[allow(clippy::many_single_char_names)]
    fn sample_and_lerp(
        &self,
        x: i32,
        y: i32,
        z: i32,
        xr: f64,
        yr: f64,
        zr: f64,
        yr_original: f64,
    ) -> f64 {
        // The permutation walk stays scalar: it is a *dependent* chain of byte
        // gathers (`x0` feeds `xy00` feeds the corner hash), so there is nothing
        // for lanes to do here and no vector unit can shorten a dependency.
        let x0 = self.perm(x);
        let x1 = self.perm(x + 1);
        let xy00 = self.perm(x0 + y);
        let xy01 = self.perm(x0 + y + 1);
        let xy10 = self.perm(x1 + y);
        let xy11 = self.perm(x1 + y + 1);

        let hash: [usize; 8] = [
            (self.perm(xy00 + z) & 15) as usize,
            (self.perm(xy10 + z) & 15) as usize,
            (self.perm(xy01 + z) & 15) as usize,
            (self.perm(xy11 + z) & 15) as usize,
            (self.perm(xy00 + z + 1) & 15) as usize,
            (self.perm(xy10 + z + 1) & 15) as usize,
            (self.perm(xy01 + z + 1) & 15) as usize,
            (self.perm(xy11 + z + 1) & 15) as usize,
        ];
        crate::counters::bump_noise_corner_batch();

        let gx = Simd::<f64, 8>::from_array(hash.map(|i| GRADIENT_X[i]));
        let gy = Simd::<f64, 8>::from_array(hash.map(|i| GRADIENT_Y[i]));
        let gz = Simd::<f64, 8>::from_array(hash.map(|i| GRADIENT_Z[i]));

        let xm = xr - 1.0;
        let ym = yr - 1.0;
        let zm = zr - 1.0;
        let xs = Simd::<f64, 8>::from_array([xr, xm, xr, xm, xr, xm, xr, xm]);
        let ys = Simd::<f64, 8>::from_array([yr, yr, ym, ym, yr, yr, ym, ym]);
        let zs = Simd::<f64, 8>::from_array([zr, zr, zr, zr, zm, zm, zm, zm]);

        // Per lane: `((gx * x) + (gy * y)) + (gz * z)` — the scalar `dot`'s exact
        // association. No `mul_add`.
        let d = gx * xs + gy * ys + gz * zs;

        let x_alpha = smoothstep(xr);
        let y_alpha = smoothstep(yr_original);
        let z_alpha = smoothstep(zr);

        // `lerp3`'s innermost level: the four `lerp(x_alpha, ., .)` siblings that
        // `lerp2` performs twice. Lanes are (d000,d100), (d010,d110),
        // (d001,d101), (d011,d111).
        let p0: Simd<f64, 4> = simd_swizzle!(d, [0, 2, 4, 6]);
        let p1: Simd<f64, 4> = simd_swizzle!(d, [1, 3, 5, 7]);
        let l = p0 + Simd::<f64, 4>::splat(x_alpha) * (p1 - p0);

        // `lerp2`'s outer level: the two `lerp(y_alpha, ., .)` siblings.
        let q0: Simd<f64, 2> = simd_swizzle!(l, [0, 2]);
        let q1: Simd<f64, 2> = simd_swizzle!(l, [1, 3]);
        let m = q0 + Simd::<f64, 2>::splat(y_alpha) * (q1 - q0);

        // `lerp3`'s root, over the two `lerp2` results.
        lerp(z_alpha, m[0], m[1])
    }
}

#[cfg(test)]
mod tests {
    use super::{GRADIENT, GRADIENT_X, GRADIENT_Y, GRADIENT_Z, ImprovedNoise};
    use crate::math::{floor, lerp3, smoothstep};

    /// The lane tables are a hand-written transpose, which is exactly the kind of
    /// table that is silently wrong — a bad entry shifts terrain and compiles
    /// fine. `GRADIENT` is kept in vanilla's row-major shape purely so this can
    /// check against it.
    #[test]
    fn transposed_gradient_tables_agree_with_vanilla_layout() {
        for (i, g) in GRADIENT.iter().enumerate() {
            assert_eq!(GRADIENT_X[i], f64::from(g[0]), "GRADIENT_X[{i}]");
            assert_eq!(GRADIENT_Y[i], f64::from(g[1]), "GRADIENT_Y[{i}]");
            assert_eq!(GRADIENT_Z[i], f64::from(g[2]), "GRADIENT_Z[{i}]");
        }
        // Control: the comparison is against a table that is not all one value,
        // so a transpose collapsing to a constant would be caught.
        assert!(
            GRADIENT_X.iter().any(|&v| v != GRADIENT_X[0]),
            "GRADIENT_X is constant — this test would pass against a collapsed table"
        );
    }

    /// An independent, deliberately naive scalar transcription of vanilla's
    /// `ImprovedNoise.sampleAndLerp`, written from the algorithm rather than from
    /// [`ImprovedNoise::sample_and_lerp`], used **only** as a bit-equality oracle.
    ///
    /// This is not a production scalar twin — nothing outside this test module can
    /// reach it, so there is no second path a seed can travel. Its whole job is to
    /// make the vectorised kernel's parity claim checkable in this crate rather
    /// than only end-to-end.
    fn scalar_reference(n: &ImprovedNoise, x: i32, y: i32, z: i32, xr: f64, yr: f64, zr: f64) -> f64 {
        let perm = |v: i32| i32::from(n.p[(v & 0xFF) as usize]);
        let grad = |hash: i32, gx: f64, gy: f64, gz: f64| {
            let g = GRADIENT[(hash & 15) as usize];
            f64::from(g[0]) * gx + f64::from(g[1]) * gy + f64::from(g[2]) * gz
        };
        let x0 = perm(x);
        let x1 = perm(x + 1);
        let xy00 = perm(x0 + y);
        let xy01 = perm(x0 + y + 1);
        let xy10 = perm(x1 + y);
        let xy11 = perm(x1 + y + 1);
        lerp3(
            smoothstep(xr),
            smoothstep(yr),
            smoothstep(zr),
            grad(perm(xy00 + z), xr, yr, zr),
            grad(perm(xy10 + z), xr - 1.0, yr, zr),
            grad(perm(xy01 + z), xr, yr - 1.0, zr),
            grad(perm(xy11 + z), xr - 1.0, yr - 1.0, zr),
            grad(perm(xy00 + z + 1), xr, yr, zr - 1.0),
            grad(perm(xy10 + z + 1), xr - 1.0, yr, zr - 1.0),
            grad(perm(xy01 + z + 1), xr, yr - 1.0, zr - 1.0),
            grad(perm(xy11 + z + 1), xr - 1.0, yr - 1.0, zr - 1.0),
        )
    }

    fn fixture() -> ImprovedNoise {
        let mut p = [0u8; 256];
        for (i, s) in p.iter_mut().enumerate() {
            *s = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        ImprovedNoise { p, xo: 12.3456, yo: 78.9012, zo: 34.5678 }
    }

    #[test]
    fn simd_kernel_is_bit_identical_to_a_scalar_transcription() {
        let n = fixture();
        // Coordinates with fractional parts in all three axes and realistic
        // magnitudes. The "world" vacuity guard: integer coordinates would make
        // every lerp factor exactly 0.0, `lerp(0.0, a, b) == a` exactly, and the
        // whole reduction tree would collapse to the identity — a test that
        // passes while measuring nothing.
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut checked = 0usize;
        let mut fractional = 0usize;
        for _ in 0..20_000 {
            let px = next() * 900.0 - 450.0;
            let py = next() * 380.0 - 64.0;
            let pz = next() * 900.0 - 450.0;
            let (x, y, z) = (px + n.xo, py + n.yo, pz + n.zo);
            let (xf, yf, zf) = (floor(x), floor(y), floor(z));
            let (xr, yr, zr) = (x - f64::from(xf), y - f64::from(yf), z - f64::from(zf));
            if xr != 0.0 && yr != 0.0 && zr != 0.0 {
                fractional += 1;
            }
            let want = scalar_reference(&n, xf, yf, zf, xr, yr, zr);
            let got = n.sample_and_lerp(xf, yf, zf, xr, yr, zr, yr);
            assert_eq!(
                want.to_bits(),
                got.to_bits(),
                "SIMD kernel diverged at ({px}, {py}, {pz}): scalar {want:e} vs simd {got:e}"
            );
            checked += 1;
        }
        assert_eq!(checked, 20_000);
        // Premise check: the inputs really do exercise interpolation. Without
        // this, a degenerate coordinate generator would make the assertion above
        // vacuous in exactly the way the comment warns about.
        assert!(
            fractional > 19_000,
            "only {fractional}/20000 sample positions had all three lerp factors \
             non-zero; this fixture is not exercising the reduction tree"
        );
    }

    /// The `-0.0` hazard the module doc names, made concrete: a gradient
    /// component of `0.0` must stay a *multiply*, because dropping it loses the
    /// sign of zero and that can survive into the result's bits.
    #[test]
    fn zero_gradient_component_keeps_sign_of_zero() {
        // GRADIENT[8] is [0, 1, 1] — its x component is a real zero.
        assert_eq!(GRADIENT_X[8], 0.0);
        let neg = -1.0f64;
        assert_eq!((GRADIENT_X[8] * neg).to_bits(), (-0.0f64).to_bits());
        assert_ne!((-0.0f64 + -0.0f64).to_bits(), (0.0f64 + -0.0f64).to_bits());
    }

    // The counter's exact-prediction gate is deliberately **not** here.
    // `counters` is process-global and other tests in this binary instantiate
    // `NormalNoise`, so a before/after delta measured here races with them and
    // would be flaky in the direction that reads as a real regression. It lives
    // in its own binary, `tests/simd_kernel_counter.rs`, for exactly the reason
    // `engine_counters.rs` does.
}
