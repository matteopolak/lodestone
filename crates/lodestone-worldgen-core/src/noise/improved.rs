//! `ImprovedNoise` — a single 3D Perlin (improved) noise octave.
//!
//! Reproduces the reference improved-noise algorithm: three
//! `nextDouble`-derived offsets, a Fisher–Yates permutation of `0..256` drawn
//! from the source, and the gradient-dot / trilinear-smoothstep sample. Only the
//! non-derivative `noise(x, y, z)` path (the one terrain uses) is implemented.
//!
//! [`sample_and_lerp`](ImprovedNoise::sample_and_lerp) evaluates the eight
//! gradient dot products of one lattice cell in eight lanes. Transposed f64
//! tables supply each lane's three coefficients without per-call conversion;
//! the dot product and fixed trilinear reduction retain their scalar evaluation
//! order.
//!
//! Two operations deliberately remain literal because changing either changes
//! result bits:
//!
//! * **The multiply by a `0.0`/`±1.0` gradient component stays a multiply.**
//!   Replacing `0.0 * x` with `0.0` loses the sign of zero (`0.0 * -x` is
//!   `-0.0`), and `-0.0 + -0.0` is `-0.0` where `0.0 + -0.0` is `0.0`, so the
//!   difference can survive the lerp tree into the returned value. Equal under
//!   `==`, not equal under `to_bits`, and this repo's parity gates read bits.
//! * **No `mul_add`.** Fused rounding differs from separate
//!   multiply-then-add. `StdFloat` is deliberately not imported.
//!
use std::simd::prelude::*;

use crate::math::{floor, lerp, smoothstep};
use crate::rng::RandomSource;

/// The 16 gradient vectors in reference-table order.
///
/// **Test-only.** This row-major form is independent of the production encoding,
/// so `tests::transposed_gradient_tables_agree_with_row_layout` detects an
/// incorrect entry.
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

/// One lattice cell's X/Z half: the two X permutations and the fractional
/// offsets with their smoothstep weights.
#[derive(Clone, Copy, Debug)]
struct PreparedImprovedXZ {
    z: i32,
    x0: i32,
    x1: i32,
    xr: f64,
    zr: f64,
    x_alpha: f64,
    z_alpha: f64,
}

impl ImprovedNoise {
    /// Builds an octave, consuming three `nextDouble`s and a 256-step shuffle
    /// from `random` in the required order.
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

    #[inline]
    fn prepare_lattice_xz(&self, x: i32, z: i32, xr: f64, zr: f64) -> PreparedImprovedXZ {
        PreparedImprovedXZ {
            z, x0: self.perm(x), x1: self.perm(x + 1), xr, zr,
            x_alpha: smoothstep(xr), z_alpha: smoothstep(zr),
        }
    }


    /// Samples the noise at `(x, y, z)` (the path with zero y scale and y fudge).
    #[inline]
    #[must_use]
    pub fn noise(&self, px: f64, py: f64, pz: f64) -> f64 {
        let x = px + self.xo;
        let y = py + self.yo;
        let z = pz + self.zo;
        let xf = floor(x);
        let yf = floor(y);
        let zf = floor(z);
        let xr = x - f64::from(xf);
        let yr = y - f64::from(yf);
        let zr = z - f64::from(zf);
        self.sample_and_lerp(xf, yf, zf, xr, yr, zr, yr)
    }

    /// The eight gradient dot products of one lattice cell, then the reference
    /// three-axis lerp reduction over them.
    ///
    /// Lane order groups the x=0 siblings before the x=1 siblings —
    /// `d000, d010, d001, d011, d100, d110, d101, d111` — so the first
    /// reduction is contiguous while the later vector swizzles spell out the
    /// same `lerp3`/`lerp2` nesting. See the module doc for why this is
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
        let xz = self.prepare_lattice_xz(x, z, xr, zr);
        self.sample_and_lerp_prepared(&xz, y, yr, yr_original)
    }

    #[inline]
    fn sample_and_lerp_prepared(
        &self, xz: &PreparedImprovedXZ, y: i32, yr: f64, yr_original: f64,
    ) -> f64 {
        let PreparedImprovedXZ { z, x0, x1, xr, zr, x_alpha, z_alpha } = *xz;
        // The permutation walk stays scalar: it is a *dependent* chain of byte
        // gathers (`x0` feeds `xy00` feeds the corner hash), so there is nothing
        // for lanes to do here and no vector unit can shorten a dependency.
        let xy00 = self.perm(x0 + y);
        let xy01 = self.perm(x0 + y + 1);
        let xy10 = self.perm(x1 + y);
        let xy11 = self.perm(x1 + y + 1);

        // Group the four x=0 corners before the four x=1 corners. This keeps
        // each side of the first lerp tree contiguous, so the compiler does
        // not need a pair of de-interleaving shuffles before the first vector
        // reduction. The later y/z sibling grouping remains exactly the same
        // reduction tree and therefore does not change evaluation order.
        let hash: [usize; 8] = [
            (self.perm(xy00 + z) & 15) as usize,
            (self.perm(xy01 + z) & 15) as usize,
            (self.perm(xy00 + z + 1) & 15) as usize,
            (self.perm(xy01 + z + 1) & 15) as usize,
            (self.perm(xy10 + z) & 15) as usize,
            (self.perm(xy11 + z) & 15) as usize,
            (self.perm(xy10 + z + 1) & 15) as usize,
            (self.perm(xy11 + z + 1) & 15) as usize,
        ];
        crate::counters::bump_noise_corner_batch();

        let gx = Simd::<f64, 8>::from_array([
            GRADIENT_X[hash[0]],
            GRADIENT_X[hash[1]],
            GRADIENT_X[hash[2]],
            GRADIENT_X[hash[3]],
            GRADIENT_X[hash[4]],
            GRADIENT_X[hash[5]],
            GRADIENT_X[hash[6]],
            GRADIENT_X[hash[7]],
        ]);
        let gy = Simd::<f64, 8>::from_array([
            GRADIENT_Y[hash[0]],
            GRADIENT_Y[hash[1]],
            GRADIENT_Y[hash[2]],
            GRADIENT_Y[hash[3]],
            GRADIENT_Y[hash[4]],
            GRADIENT_Y[hash[5]],
            GRADIENT_Y[hash[6]],
            GRADIENT_Y[hash[7]],
        ]);
        let gz = Simd::<f64, 8>::from_array([
            GRADIENT_Z[hash[0]],
            GRADIENT_Z[hash[1]],
            GRADIENT_Z[hash[2]],
            GRADIENT_Z[hash[3]],
            GRADIENT_Z[hash[4]],
            GRADIENT_Z[hash[5]],
            GRADIENT_Z[hash[6]],
            GRADIENT_Z[hash[7]],
        ]);

        let xm = xr - 1.0;
        let ym = yr - 1.0;
        let zm = zr - 1.0;
        let xs = Simd::<f64, 8>::from_array([xr, xr, xr, xr, xm, xm, xm, xm]);
        let ys = Simd::<f64, 8>::from_array([yr, ym, yr, ym, yr, ym, yr, ym]);
        let zs = Simd::<f64, 8>::from_array([zr, zr, zm, zm, zr, zr, zm, zm]);

        // Per lane: `((gx * x) + (gy * y)) + (gz * z)` — the scalar `dot`'s exact
        // association. No `mul_add`.
        let d = gx * xs + gy * ys + gz * zs;

        let y_alpha = smoothstep(yr_original);

        // `lerp3`'s innermost level: the four `lerp(x_alpha, ., .)` siblings that
        // `lerp2` performs twice. Lanes are grouped as
        // `(d000,d010,d001,d011)` then `(d100,d110,d101,d111)`, so these
        // swizzles are contiguous loads rather than de-interleaves.
        let p0: Simd<f64, 4> = simd_swizzle!(d, [0, 1, 2, 3]);
        let p1: Simd<f64, 4> = simd_swizzle!(d, [4, 5, 6, 7]);
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

    /// The production tables are a hand-written transpose, while `GRADIENT` is
    /// independently row-major. Compare them so one wrong value cannot silently
    /// shift terrain.
    #[test]
    fn transposed_gradient_tables_agree_with_row_layout() {
        for (i, g) in GRADIENT.iter().enumerate() {
            assert_eq!(GRADIENT_X[i], f64::from(g[0]), "GRADIENT_X[{i}]");
            assert_eq!(GRADIENT_Y[i], f64::from(g[1]), "GRADIENT_Y[{i}]");
            assert_eq!(GRADIENT_Z[i], f64::from(g[2]), "GRADIENT_Z[{i}]");
        }
        assert!(
            GRADIENT_X.iter().any(|&value| value != GRADIENT_X[0]),
            "GRADIENT_X is constant — this test would pass against a collapsed table"
        );
    }

    /// An independent, deliberately naive scalar transcription of the reference
    /// sample-and-lerp routine, written from the algorithm rather than from
    /// [`ImprovedNoise::sample_and_lerp`], used **only** as a bit-equality oracle.
    ///
    /// This is not a production scalar twin — nothing outside this test module can
    /// reach it, so there is no second path a seed can travel. Its whole job is to
    /// make the vectorised kernel's parity claim checkable in this crate rather
    /// than only end-to-end.
    fn scalar_reference(n: &ImprovedNoise, x: i32, y: i32, z: i32,
        xr: f64, yr: f64, zr: f64, yr_original: f64) -> f64
    {
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
            smoothstep(yr_original),
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
            let want = scalar_reference(&n, xf, yf, zf, xr, yr, zr, yr);
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
        let x = GRADIENT_X[8];
        assert_eq!(x, 0.0);
        let neg = -1.0f64;
        assert_eq!((x * neg).to_bits(), (-0.0f64).to_bits());
        assert_ne!((-0.0f64 + -0.0f64).to_bits(), (0.0f64 + -0.0f64).to_bits());
    }

    // The counter's exact-prediction gate is deliberately **not** here.
    // `counters` is process-global and other tests in this binary instantiate
    // `NormalNoise`, so a before/after delta measured here races with them and
    // would be flaky in the direction that reads as a real regression. It lives
    // in its own binary, `tests/simd_kernel_counter.rs`.
}
