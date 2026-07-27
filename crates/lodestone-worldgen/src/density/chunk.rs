//! `NoiseChunk`-equivalent block-field sampling.
//!
//! The density-function interpreter in [`super`] evaluates the *raw* noise
//! router at a point (vanilla's `SinglePointContext`), which is what the router
//! parity tests prove. But the real server does **not** write that point field
//! to blocks. Vanilla wraps the router in a `NoiseChunk`
//! (`net.minecraft.world.level.levelgen.NoiseChunk`) which changes two node
//! kinds:
//!
//! * **`interpolated`** samples its wrapped function only at the corners of a
//!   4×8×4 (`cellWidth`×`cellHeight`×`cellWidth`) cell grid, then trilinearly
//!   interpolates per block. The interpolation order matches vanilla's
//!   incremental `updateForY` → `updateForX` → `updateForZ` (y, then x, then z),
//!   which is bit-significant.
//! * **`flat_cache`** snaps XZ to the quart grid (`blockX >> 2 << 2`) and forces
//!   `y = 0`, so 2D climate/shift fields are sampled once per 4×4 column.
//!
//! All other markers (`cache_2d`, `cache_once`, `cache_all_in_cell`,
//! `blend_density`) are value-transparent. This module reimplements that
//! wrapping behaviour so `final_density(x, y, z)` equals
//! `NoiseChunk.getInterpolatedDensity()` block-for-block.
//!
//! No Mojang source is transliterated: the algorithm here is derived from the
//! observable per-block field and cross-checked against a JVM oracle
//! (`scripts/worldgen-oracle/DensityChunkOracle.java`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use super::{Context, Density};

/// Minimal FxHash-style hasher for the corner cache's `(i32, i32, i32)` keys.
/// The default `HashMap` uses SipHash, which is DoS-resistant but slow; these
/// keys are trusted internal cell coordinates, so a multiply-xor fold is both
/// correct and far cheaper. Choice of hasher is value-invariant — it changes
/// only lookup speed, never which corner value is stored or returned — so it
/// cannot affect worldgen parity.
#[derive(Default)]
struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, i: u64) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add(u64::from(b));
        }
    }
    #[inline]
    fn write_i32(&mut self, i: i32) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

type FxBuild = BuildHasherDefault<FxHasher>;
type CornerCache = HashMap<(i32, i32, i32), f64, FxBuild>;

/// A stateless-per-block reimplementation of vanilla's `NoiseChunk` block field
/// for one column of density functions.
///
/// Construct it from a built [`Density`] root (typically `final_density`), the
/// number of cache slots reported by [`super::Builder::slot_count`], and the
/// cell dimensions from the noise settings. Then call [`final_density`] for any
/// block position; corner and flat-cache evaluations are memoised across calls.
///
/// [`final_density`]: Self::final_density
#[allow(missing_debug_implementations)]
pub struct NoiseChunkSampler {
    root: Density,
    cell_width: i32,
    cell_height: i32,
    caches: Vec<RefCell<CornerCache>>,
}

impl NoiseChunkSampler {
    /// Creates a sampler. `cell_width`/`cell_height` are
    /// `NoiseSettings.getCellWidth()/getCellHeight()` — 4 and 8 for the
    /// overworld.
    #[must_use]
    pub fn new(root: Density, slot_count: usize, cell_width: i32, cell_height: i32) -> Self {
        let caches = (0..slot_count)
            .map(|_| RefCell::new(CornerCache::default()))
            .collect();
        Self {
            root,
            cell_width,
            cell_height,
            caches,
        }
    }

    /// The interpolated final-density value at a block, matching
    /// `NoiseChunk.getInterpolatedDensity()`.
    #[must_use]
    pub fn final_density(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval(&self.root, x, y, z, true)
    }

    /// Evaluates this sampler's root at a block through the `NoiseChunk` wrapping
    /// (so `flat_cache` snaps XZ to the quart grid and forces `y = 0`, exactly as
    /// vanilla's wrapped `router.erosion()` / `router.depth()` do when the
    /// aquifer computes them at a `SinglePointContext`). For an `interpolated`
    /// root this is identical to [`final_density`](Self::final_density); for the
    /// non-interpolated router routes it is the value-correct point evaluation.
    #[must_use]
    pub fn sample(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval(&self.root, x, y, z, true)
    }

    fn eval(&self, node: &Density, x: i32, y: i32, z: i32, interpolate: bool) -> f64 {
        match node {
            Density::Interpolated { inner, slot } => {
                if interpolate {
                    self.interpolate(inner, *slot, x, y, z)
                } else {
                    // Nested inside a corner sample: vanilla's interpolator is
                    // transparent when the context is not the NoiseChunk itself.
                    self.eval(inner, x, y, z, false)
                }
            }
            Density::FlatCache { inner, slot } => {
                let qx = (x >> 2) << 2;
                let qz = (z >> 2) << 2;
                self.slot_get(*slot, (qx, 0, qz), inner)
            }
            Density::Marker(inner) => self.eval(inner, x, y, z, interpolate),

            Density::Add(a, b) => {
                self.eval(a, x, y, z, interpolate) + self.eval(b, x, y, z, interpolate)
            }
            Density::Mul(a, b) => {
                let v1 = self.eval(a, x, y, z, interpolate);
                if v1 == 0.0 {
                    0.0
                } else {
                    v1 * self.eval(b, x, y, z, interpolate)
                }
            }
            Density::Min(a, b) => {
                self.eval(a, x, y, z, interpolate)
                    .min(self.eval(b, x, y, z, interpolate))
            }
            Density::Max(a, b) => {
                self.eval(a, x, y, z, interpolate)
                    .max(self.eval(b, x, y, z, interpolate))
            }
            Density::Abs(a) => self.eval(a, x, y, z, interpolate).abs(),
            Density::Square(a) => {
                let v = self.eval(a, x, y, z, interpolate);
                v * v
            }
            Density::Cube(a) => {
                let v = self.eval(a, x, y, z, interpolate);
                v * v * v
            }
            Density::HalfNegative(a) => {
                let v = self.eval(a, x, y, z, interpolate);
                if v > 0.0 { v } else { v * 0.5 }
            }
            Density::QuarterNegative(a) => {
                let v = self.eval(a, x, y, z, interpolate);
                if v > 0.0 { v } else { v * 0.25 }
            }
            Density::Squeeze(a) => {
                let c = self.eval(a, x, y, z, interpolate).clamp(-1.0, 1.0);
                c / 2.0 - c * c * c / 24.0
            }
            Density::Invert(a) => 1.0 / self.eval(a, x, y, z, interpolate),
            Density::Clamp { input, min, max } => {
                self.eval(input, x, y, z, interpolate).clamp(*min, *max)
            }
            Density::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let v = self.eval(input, x, y, z, interpolate);
                if v >= *min_inclusive && v < *max_exclusive {
                    self.eval(when_in_range, x, y, z, interpolate)
                } else {
                    self.eval(when_out_of_range, x, y, z, interpolate)
                }
            }
            Density::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let v = self.eval(input, x, y, z, interpolate);
                for (i, t) in thresholds.iter().enumerate() {
                    if v < *t {
                        return self.eval(&functions[i], x, y, z, interpolate);
                    }
                }
                self.eval(&functions[functions.len() - 1], x, y, z, interpolate)
            }
            Density::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => {
                let sx = f64::from(x) * xz_scale + self.eval(shift_x, x, y, z, interpolate);
                let sy = f64::from(y) * y_scale + self.eval(shift_y, x, y, z, interpolate);
                let sz = f64::from(z) * xz_scale + self.eval(shift_z, x, y, z, interpolate);
                noise.get_value(sx, sy, sz)
            }

            // Leaves (no nested density children): identical to point compute.
            Density::Const(_)
            | Density::BlendAlpha
            | Density::BlendOffset
            | Density::Beardifier
            | Density::YClampedGradient { .. }
            | Density::Noise { .. }
            | Density::ShiftA(_)
            | Density::ShiftB(_)
            | Density::Shift(_)
            | Density::Spline(_)
            | Density::Blended(_)
            | Density::FindTopSurface { .. } => node.compute(Context::new(x, y, z)),
        }
    }

    fn interpolate(&self, inner: &Density, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        let cw = self.cell_width;
        let ch = self.cell_height;
        let x0 = x.div_euclid(cw) * cw;
        let y0 = y.div_euclid(ch) * ch;
        let z0 = z.div_euclid(cw) * cw;
        let (x1, y1, z1) = (x0 + cw, y0 + ch, z0 + cw);

        let n000 = self.corner(inner, slot, x0, y0, z0);
        let n100 = self.corner(inner, slot, x1, y0, z0);
        let n010 = self.corner(inner, slot, x0, y1, z0);
        let n110 = self.corner(inner, slot, x1, y1, z0);
        let n001 = self.corner(inner, slot, x0, y0, z1);
        let n101 = self.corner(inner, slot, x1, y0, z1);
        let n011 = self.corner(inner, slot, x0, y1, z1);
        let n111 = self.corner(inner, slot, x1, y1, z1);

        let fx = f64::from(x.rem_euclid(cw)) / f64::from(cw);
        let fy = f64::from(y.rem_euclid(ch)) / f64::from(ch);
        let fz = f64::from(z.rem_euclid(cw)) / f64::from(cw);

        // Vanilla's `fillingCell` path is Mth.lerp3: X-inner, then Y, then Z.
        // lerp3(ax,ay,az, x000,x100,x010,x110,x001,x101,x011,x111)
        //   = lerp(az, lerp2(ax,ay, z0-slice), lerp2(ax,ay, z1-slice))
        // lerp2(ax,ay, x00,x10,x01,x11) = lerp(ay, lerp(ax,x00,x10), lerp(ax,x01,x11))
        lerp3(fx, fy, fz, n000, n100, n010, n110, n001, n101, n011, n111)
    }

    fn corner(&self, inner: &Density, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        self.slot_get(slot, (x, y, z), inner)
    }

    fn slot_get(&self, slot: usize, key: (i32, i32, i32), inner: &Density) -> f64 {
        if let Some(v) = self.caches[slot].borrow().get(&key) {
            return *v;
        }
        let v = self.eval(inner, key.0, key.1, key.2, false);
        self.caches[slot].borrow_mut().insert(key, v);
        v
    }
}

#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

#[inline]
fn lerp2(ax: f64, ay: f64, x00: f64, x10: f64, x01: f64, x11: f64) -> f64 {
    lerp(ay, lerp(ax, x00, x10), lerp(ax, x01, x11))
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn lerp3(
    ax: f64,
    ay: f64,
    az: f64,
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
        az,
        lerp2(ax, ay, x000, x100, x010, x110),
        lerp2(ax, ay, x001, x101, x011, x111),
    )
}
