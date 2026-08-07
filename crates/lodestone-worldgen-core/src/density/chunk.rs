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
//!   interpolates per block, in the `Mth.lerp3` nesting — **X innermost**, then
//!   Y, then Z. That order is bit-significant and it is *not* the one vanilla's
//!   driver loop appears to produce; see `## Which interpolation order` below
//!   before changing [`lerp3`].
//! * **`flat_cache`** snaps XZ to the quart grid (`blockX >> 2 << 2`) and forces
//!   `y = 0`, so 2D climate/shift fields are sampled once per 4×4 column.
//!
//! All other markers (`cache_2d`, `cache_once`, `cache_all_in_cell`,
//! `blend_density`) are value-transparent *in this evaluator*. This module
//! reimplements that wrapping behaviour so `final_density(x, y, z)` equals
//! `NoiseChunk.getInterpolatedDensity()` block-for-block.
//!
//! ## Which interpolation order
//!
//! `NoiseChunk.NoiseInterpolator` has **two** value paths over the same eight
//! corners, and as floating-point expressions they are different:
//!
//! | vanilla path | expression | nesting |
//! |---|---|---|
//! | `fillingCell == true` | `Mth.lerp3` (`lerp2` is `lerp(dy, lerp(dx, x00, x10), lerp(dx, x01, x11))`) | **X inner**, Y, Z |
//! | `fillingCell == false` | the incremental `updateForY` → `updateForX` → `updateForZ` chain | **Y inner**, X, Z |
//!
//! This module implements the **first**. That looks wrong on a first reading of
//! `NoiseChunk`, because the driver loop (`selectCellYZ` → `updateForY` →
//! `updateForX` → `updateForZ`) is visibly feeding the *second*. The resolution
//! is two levels removed from the interpolator: `NoiseChunk`'s constructor
//! (`NoiseChunk.java:157-160`) does not read the router's `final_density`
//! directly, it wraps it —
//!
//! ```text
//! fullNoiseValue = DensityFunctions.cacheAllInCell(
//!         DensityFunctions.add(wrappedRouter.finalDensity(), BeardifierMarker.INSTANCE))
//!     .mapAll(this::wrap);
//! ```
//!
//! — and that `cache_all_in_cell` is applied **in code, not in data** (no
//! `minecraft:cache_all_in_cell` appears anywhere in 26.2's worldgen JSON, so
//! reading the `noise_settings` document cannot see it). Its cell array is
//! pre-filled inside `selectCellYZ`, which brackets the fill with
//! `fillingCell = true` / `false`. So every value `getInterpolatedDensity()`
//! returns for `final_density` was produced in the `fillingCell == true`
//! regime — by `Mth.lerp3`. The incremental chain is never what
//! `final_density` reads.
//!
//! **Measured, because the difference is ~1 ULP and therefore does not look like
//! a bug.** Swapping [`lerp3`] to the incremental chain takes
//! `chunk_parity`'s whole-chunk JVM gate from 98304/98304 to **90563/98304**
//! (7,741 diverged blocks, all 1-ULP). `docs/plans/worldgen-rewrite.md`'s U4 row
//! prescribes "vanilla's incremental cell walk", so the trap is written into the
//! plan; `crates/lodestone-worldgen/tests/interpolation_order.rs` is the
//! standing guard, and `docs/worldgen-density-engine.md` has the full account.
//!
//! The consequence for a rewrite is not "keep a point query". It is that the
//! correct cell walk **pre-fills a 4×8×4 = 128-value cell array with `Mth.lerp3`
//! from eight corners held once per cell**, exactly as vanilla's
//! `CacheAllInCell` does — same arithmetic as here, hoisted. A walk built on
//! `updateForY/X/Z` would be a different world.
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

/// The per-slot backing store for [`NoiseChunkSampler::slot_get`]'s
/// memoisation. `Hashed` is the original, general-purpose store (works for any
/// `(x, y, z)`, unbounded); `Dense` is the bounded, hash-free alternative — see
/// [`DenseShape`]'s doc comment for why and when it applies.
enum SlotStore {
    Hashed(RefCell<CornerCache>),
    Dense(RefCell<Option<DenseSlot>>),
}

/// The dense backing for one `Dense` slot: a flat, arithmetic-indexed array
/// sized by [`DenseShape`], allocated lazily (only on the slot's first write —
/// most of the 1000+ slots a real settings graph allocates are either never
/// reached through [`NoiseChunkSampler::eval`]'s traversal at all (anything
/// nested inside a `spline`/`old_blended_noise`/`find_top_surface` leaf, which
/// `eval` evaluates as an opaque point-`compute()` rather than recursing into —
/// see `eval`'s own doc) or reached only under an `Interpolated` ancestor
/// (which evaluates its inner transparently, `interpolate = false`, and so
/// never calls `corner`/`slot_get` for that inner node's own slot either), so
/// eagerly allocating every slot's full grid would pay for hundreds of grids
/// that are never touched).
struct DenseSlot {
    values: Vec<f64>,
    has: Vec<bool>,
}

impl DenseSlot {
    fn new(len: usize) -> Self {
        Self {
            values: vec![0.0; len],
            has: vec![false; len],
        }
    }
}

/// A shared, precomputed arithmetic-indexing scheme for every `Dense` slot in
/// one [`NoiseChunkSampler`], replacing `slot_get`'s `HashMap::get` (measured
/// at ~10%+ of this crate's total profiled self-time, `docs/worldgen-surface-perf.md`)
/// with direct `Vec` indexing. **Not** a like-for-like port of vanilla's
/// incremental cell-walk (`NoiseChunk.java`'s `advanceCellX`/`updateForY` etc.,
/// which is a stateful, strictly-ordered API replacing point queries entirely)
/// — this keeps [`NoiseChunkSampler::final_density`]'s point-query API and
/// callers' iteration order completely unchanged. It only replaces *how* a
/// corner value already known to be needed is looked up: a hash + probe
/// becomes an offset computed from the query's own coordinates.
///
/// # Why one uniform shape works for both `interpolated` and `flat_cache`
///
/// `slot_get` backs two distinct key shapes (see `eval`'s `Interpolated` and
/// `FlatCache` arms): `interpolated` corners are multiples of `cell_width`
/// (X/Z) and `cell_height` (Y); `flat_cache` keys are XZ snapped to the quart
/// grid (multiples of 4, hardcoded — not `cell_width`-parameterised) with
/// `y` forced to exactly `0`. Rather than classifying which of the two shapes
/// each slot needs (which would require walking the `Density` tree to see
/// which arm assigned it, mirroring `eval`'s exact recursion including which
/// branches it does *not* recurse into), every `Dense` slot uses **one**
/// shape wide enough to cover both:
///
/// - X/Z step = `gcd(cell_width, 4)` — divides both key families, so every
///   real key (a multiple of `cell_width` or of `4`) lands on an exact grid
///   point with no aliasing between distinct keys.
/// - Y step = `cell_height`, with the bounds unioned with `0` — `0` is
///   already a multiple of `cell_height` (any real `cell_height` divides it),
///   so `flat_cache`'s forced `y = 0` lands on the *same* grid `interpolated`
///   corners use, needing no separate case.
///
/// The tradeoff: a `flat_cache` slot's grid still spans the full Y range even
/// though only its `y = 0` plane is ever populated. That is bounded, cheap
/// waste (a few dozen KB, see `NoiseChunkSampler::new_bounded`'s doc), not a
/// correctness issue — a plane that is never queried is simply never
/// allocated-into via [`DenseSlot::new`]'s lazy-per-slot allocation, only
/// sized-for in the (free, `Copy`) shape math below.
#[derive(Clone, Copy)]
struct DenseShape {
    x0: i32,
    y0: i32,
    z0: i32,
    step_xz: i32,
    step_y: i32,
    nx: i32,
    ny: i32,
    nz: i32,
}

impl DenseShape {
    /// `x_range`/`y_range`/`z_range` are the **inclusive** block-coordinate
    /// bounds every call to [`NoiseChunkSampler::final_density`]/`sample` on
    /// the owning sampler will ever use — the bounded-sampler contract
    /// [`NoiseChunkSampler::new_bounded`] documents. Anything outside those
    /// bounds is a caller bug (checked by `debug_assert!` in
    /// [`Self::index`], not silently wrapped).
    fn for_bounds(
        cell_width: i32,
        cell_height: i32,
        x_range: (i32, i32),
        y_range: (i32, i32),
        z_range: (i32, i32),
    ) -> Self {
        let step_xz = gcd(cell_width, 4);
        let step_y = cell_height;

        let xz_bounds = |lo: i32, hi: i32| -> (i32, i32) {
            // Interpolated corners: floor(v/cell_width)*cell_width and
            // one cell beyond. Flat-cache: floor(v/4)*4 (no "+cell" needed,
            // it is a direct snap, not a corner pair, but the padding is
            // harmless).
            let interp_lo = lo.div_euclid(cell_width) * cell_width;
            let interp_hi = hi.div_euclid(cell_width) * cell_width + cell_width;
            let flat_lo = lo.div_euclid(4) * 4;
            let flat_hi = hi.div_euclid(4) * 4 + 4;
            (interp_lo.min(flat_lo), interp_hi.max(flat_hi))
        };
        let (x0, x1) = xz_bounds(x_range.0, x_range.1);
        let (z0, z1) = xz_bounds(z_range.0, z_range.1);

        let y_interp_lo = y_range.0.div_euclid(cell_height) * cell_height;
        let y_interp_hi = y_range.1.div_euclid(cell_height) * cell_height + cell_height;
        let y0 = y_interp_lo.min(0);
        let y1 = y_interp_hi.max(0);

        let nx = (x1 - x0) / step_xz + 1;
        let ny = (y1 - y0) / step_y + 1;
        let nz = (z1 - z0) / step_xz + 1;

        Self {
            x0,
            y0,
            z0,
            step_xz,
            step_y,
            nx,
            ny,
            nz,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.nx as usize * self.ny as usize * self.nz as usize
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        let ix = (x - self.x0).div_euclid(self.step_xz);
        let iy = (y - self.y0).div_euclid(self.step_y);
        let iz = (z - self.z0).div_euclid(self.step_xz);
        debug_assert!(
            ix >= 0 && ix < self.nx && iy >= 0 && iy < self.ny && iz >= 0 && iz < self.nz,
            "DenseShape::index out of the sampler's declared bounds: ({x}, {y}, {z}) -> \
             ({ix}, {iy}, {iz}) not within (0..{}, 0..{}, 0..{})",
            self.nx,
            self.ny,
            self.nz
        );
        (ix as usize * self.ny as usize + iy as usize) * self.nz as usize + iz as usize
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

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
    caches: Vec<SlotStore>,
    dense_shape: Option<DenseShape>,
}

impl NoiseChunkSampler {
    /// Creates a sampler. `cell_width`/`cell_height` are
    /// `NoiseSettings.getCellWidth()/getCellHeight()` — 4 and 8 for the
    /// overworld.
    #[must_use]
    pub fn new(root: Density, slot_count: usize, cell_width: i32, cell_height: i32) -> Self {
        let caches = (0..slot_count)
            .map(|_| SlotStore::Hashed(RefCell::new(CornerCache::default())))
            .collect();
        Self {
            root,
            cell_width,
            cell_height,
            caches,
            dense_shape: None,
        }
    }

    /// Creates a sampler whose corner/flat-cache memoisation is a bounded,
    /// hash-free dense array instead of a `HashMap` — see [`DenseShape`]'s
    /// doc for the indexing scheme and why one shape serves both key
    /// families. **Contract**: `x_range`/`y_range`/`z_range` (each
    /// `(min, max)`, inclusive) must bound *every* coordinate this sampler's
    /// [`final_density`](Self::final_density)/[`sample`](Self::sample) will
    /// ever be called with — this is the same "chunk-aligned bounds" contract
    /// `docs/worldgen-surface-perf.md` already documents for
    /// `surface/mod.rs`'s corner-cell hoist, applied to this sampler instead.
    /// A query outside the declared bounds trips a `debug_assert!` in
    /// [`DenseShape::index`] in debug builds; in release builds it would
    /// silently alias a different cell, so this constructor is for callers
    /// with a known, small query region — not a drop-in replacement for
    /// [`new`](Self::new) everywhere. `crate::aquifer::AquiferSystem`'s
    /// `erosion`/`depth` samplers query scattered, not exhaustively-bounded,
    /// positions (`is_deep_dark_region`'s padded grid-cell search legitimately
    /// reaches outside the current chunk) and keep using `new`; its
    /// `final_density` sampler is only ever queried at exact chunk-bounded
    /// positions (every caller goes through `block_at`/`carve_substance`) and
    /// does use this constructor (issue #295's performance pass).
    #[must_use]
    pub fn new_bounded(
        root: Density,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        x_range: (i32, i32),
        y_range: (i32, i32),
        z_range: (i32, i32),
    ) -> Self {
        let shape = DenseShape::for_bounds(cell_width, cell_height, x_range, y_range, z_range);
        let caches = (0..slot_count)
            .map(|_| SlotStore::Dense(RefCell::new(None)))
            .collect();
        Self {
            root,
            cell_width,
            cell_height,
            caches,
            dense_shape: Some(shape),
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
        crate::counters::bump_density_eval(node.kind_index());
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
            // `cache_2d` stays transparent in the block field, same as the
            // other three "value-transparent" markers below — this mirrors
            // the pre-split `Density::Marker` handling verbatim (`cache_2d`
            // was one of the four lumped JSON types here), so it carries the
            // same JVM-cross-checked correctness this module's doc comment
            // already claims for it. Only `super::Density::compute` (the
            // point evaluator, used outside `NoiseChunk`'s per-block field)
            // gained real caching for `cache_2d` — see its module doc.
            Density::Cache2D { inner, .. } | Density::Marker(inner) => {
                self.eval(inner, x, y, z, interpolate)
            }

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
        // Counted here rather than inside `slot_get`, which `FlatCache` also
        // calls: this must stay "corner lookups" exactly (8 per interpolated
        // query, hit or miss), because that is the number `benches/generation.rs`
        // predicts from the cell geometry. `slot_get`'s own hit/miss counters
        // are the separate, larger population.
        crate::counters::bump_corner_lookup();
        self.slot_get(slot, (x, y, z), inner)
    }

    fn slot_get(&self, slot: usize, key: (i32, i32, i32), inner: &Density) -> f64 {
        match &self.caches[slot] {
            SlotStore::Hashed(cache) => {
                if let Some(v) = cache.borrow().get(&key) {
                    crate::counters::bump_slot_hit();
                    return *v;
                }
                crate::counters::bump_slot_miss(slot);
                let v = self.eval(inner, key.0, key.1, key.2, false);
                cache.borrow_mut().insert(key, v);
                v
            }
            SlotStore::Dense(cell) => {
                let shape = self
                    .dense_shape
                    .expect("Dense slots only exist on a sampler built with a DenseShape");
                let idx = shape.index(key.0, key.1, key.2);
                if let Some(v) = cell.borrow().as_ref().and_then(|ds| {
                    if ds.has[idx] { Some(ds.values[idx]) } else { None }
                }) {
                    crate::counters::bump_slot_hit();
                    return v;
                }
                crate::counters::bump_slot_miss(slot);
                let v = self.eval(inner, key.0, key.1, key.2, false);
                let mut borrow = cell.borrow_mut();
                let ds = borrow.get_or_insert_with(|| DenseSlot::new(shape.len()));
                ds.values[idx] = v;
                ds.has[idx] = true;
                v
            }
        }
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
