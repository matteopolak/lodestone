//! Vanilla's own noise-chunk-sampler-equivalent block-field sampling.
//!
//! The density-function interpreter in [`super`] evaluates the *raw* noise
//! router at a point (vanilla's own single-point context), which is what the router
//! parity tests prove. But the real server does **not** write that point field
//! to blocks. Vanilla wraps the router in its own per-chunk noise-block field
//! (its own noise-chunk class) which changes two node
//! kinds:
//!
//! * **`interpolated`** samples its wrapped function only at the corners of a
//!   settings-selected (cell-width×cell-height×cell-width) cell grid, then
//!   trilinearly interpolates per block, in vanilla's own three-axis lerp's
//!   nesting — **X innermost**, then Y, then Z. That order is bit-significant
//!   and it is *not* the one vanilla's driver loop appears to produce; see
//!   `## Which interpolation order` below. The usual geometry is 4×8; the End
//!   uses 8×4.
//! * **`flat_cache`** snaps XZ to the quart grid (`blockX >> 2 << 2`) and forces
//!   `y = 0`, so 2D climate/shift fields are sampled once per 4×4 column.
//!
//! All other markers (`cache_2d`, `cache_once`, `cache_all_in_cell`,
//! `blend_density`) are value-transparent *in this evaluator*.
//! [`NoiseChunkSampler`] reproduces that wrapping behaviour so
//! `final_density(x, y, z)` equals vanilla's own interpolated-density query
//! block-for-block.
//!
//! ## This module is a façade
//!
//! Since U4 the evaluation itself lives in [`crate::engine`]: the `Density` tree
//! is compiled to a flattened, index-addressed [`Program`] and evaluated against
//! a pooled [`Scratch`]. This file is only the pairing of those two plus the
//! public API callers already had. The recursive walk over `Box`-linked
//! `Density` nodes that used to be here is gone — read `engine/`'s module doc
//! for the shape and `engine/field.rs` for the semantics.
//!
//! What that buys, and what it does not: corner *lookups* per chunk fall from
//! 786,432 to 6,144 and a per-chunk tree clone becomes an `Arc` bump, but the
//! multiply-adds inside the lerp are unchanged. The win is a lookup win, not an
//! arithmetic one, which bounds how much steady-state cost it can move.
//!
//! ## Which interpolation order
//!
//! Vanilla's own per-chunk noise-interpolator has **two** value paths over
//! the same eight
//! corners, and as floating-point expressions they are different:
//!
//! | vanilla path | expression | nesting |
//! |---|---|---|
//! | the "filling cell" flag is true | vanilla's own three-axis lerp (its own
//! two-axis lerp is `lerp(dy, lerp(dx, x00, x10), lerp(dx, x01, x11))`) | **X inner**, Y, Z |
//! | the "filling cell" flag is false | the incremental per-axis update chain
//! (Y update, then X update, then Z update) | **Y inner**, X, Z |
//!
//! The engine implements the **first**. That looks wrong on a first reading of
//! vanilla's own field type, because the driver loop (its own per-cell
//! selection, then the same Y/X/Z update chain) is visibly feeding the
//! *second*. The resolution
//! is two levels removed from the interpolator: vanilla's own field
//! constructor
//! does not read the router's `final_density`
//! directly, it wraps it —
//!
//! ```text
//! full_noise_value = cache_all_in_cell(
//!         add(wrapped_router.final_density(), beardifier_marker))
//!     .map_all(self.wrap)
//! ```
//!
//! — and that `cache_all_in_cell` is applied **in code, not in data** (no
//! `minecraft:cache_all_in_cell` appears anywhere in 26.2's worldgen JSON, so
//! reading the `noise_settings` document cannot see it). Its cell array is
//! pre-filled inside vanilla's own per-cell selection routine, which brackets
//! the fill with
//! the "filling cell" flag set true then false. So every value vanilla's own
//! interpolated-density query
//! returns for `final_density` was produced in the "filling cell is true"
//! regime — by its own three-axis lerp. The incremental chain is never what
//! `final_density` reads.
//!
//! **Measured, because the difference is ~1 ULP and therefore does not look like
//! a bug.** Swapping the engine's `lerp3` to the incremental chain takes
//! `chunk_parity`'s whole-chunk JVM gate from 98304/98304 to **90563/98304**
//! (7,741 diverged blocks, all 1-ULP). `docs/plans/worldgen-rewrite.md`'s U4 row
//! prescribes "vanilla's incremental cell walk", so the trap is written into the
//! plan; `crates/lodestone-worldgen/tests/interpolation_order.rs` is the
//! standing guard, and `docs/worldgen-density-engine.md` has the full account.
//!
//! No Mojang source is transliterated: the algorithm is derived from the
//! observable per-block field and cross-checked against a JVM oracle
//! (our own `scripts/worldgen-oracle/DensityChunkOracle.java` harness).

use std::cell::RefCell;
use std::sync::Arc;

use super::Density;
use crate::engine::{Bounds, Field, Geom, Program, Scratch, XzProductLattice};

/// A stateless-per-block reimplementation of vanilla's own per-chunk block field
/// for one column of density functions.
///
/// Construct it from a built [`Density`] root (typically `final_density`), the
/// number of cache slots reported by [`super::Builder::slot_count`], and the
/// cell dimensions from the noise settings — or, preferably for anything
/// per-chunk, from an already-compiled [`Program`] via
/// [`from_program`](Self::from_program), which is what makes handing the same
/// graph to the next chunk a refcount bump. Then call [`final_density`] for any
/// block position; corner and flat-cache evaluations are memoised across calls.
///
/// [`final_density`]: Self::final_density
#[allow(missing_debug_implementations)]
pub struct NoiseChunkSampler {
    program: Program,
    geom: Geom,
    bounds: Option<Bounds>,
    products: Option<Arc<XzProductLattice>>,
    /// `Option` only so [`Drop`] can move the scratch out and return it to the
    /// thread's free list. It is `Some` for the whole of the sampler's life.
    scratch: RefCell<Option<Scratch>>,
}

/// A bounded final-density sampler for one request-scoped region.
///
/// The wrapped sampler owns one scratch cache over the complete block rectangle,
/// so adjacent columns share interpolation corners at their boundaries. It is
/// intentionally single-owner: [`NoiseChunkSampler`] uses interior mutability
/// for its scratch and this type must stay on the worker that owns the region.
/// Drop it when the region's products are published; no values are retained in
/// a generator-wide cache.
#[allow(missing_debug_implementations)]
pub struct NoiseChunkRegionSampler {
    sampler: NoiseChunkSampler,
    bounds: Bounds,
}

impl NoiseChunkRegionSampler {
    /// Creates a sampler over an inclusive block rectangle. Every queried
    /// column must lie inside `bounds`; the Y bounds are also used to size the
    /// interpolation corner lattice.
    #[must_use]
    pub fn from_program(
        program: Program,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Bounds,
    ) -> Self {
        Self::from_program_with_xz_products(
            program,
            slot_count,
            cell_width,
            cell_height,
            bounds,
            None,
        )
    }

    /// Creates a bounded sampler with an immutable request-scoped X/Z product
    /// lattice. A fingerprint mismatch remains an exact fallback.
    #[must_use]
    pub fn from_program_with_xz_products(
        program: Program,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Bounds,
        products: Option<Arc<XzProductLattice>>,
    ) -> Self {
        Self {
            sampler: NoiseChunkSampler::from_program_with_xz_products(
                program,
                slot_count,
                cell_width,
                cell_height,
                Some(bounds),
                products,
            ),
            bounds,
        }
    }

    /// Inclusive block bounds covered by this sampler.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Evaluates a contiguous vertical final-density run in the shared region
    /// scratch. Values are bit-identical to isolated chunk samplers.
    pub fn final_density_column(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        output: &mut [f64],
    ) {
        assert!((self.bounds.x.0..=self.bounds.x.1).contains(&x));
        assert!((self.bounds.z.0..=self.bounds.z.1).contains(&z));
        assert!(
            output.is_empty()
                || ((self.bounds.y.0..=self.bounds.y.1).contains(&y_start)
                    && y_start + output.len() as i32 - 1 <= self.bounds.y.1)
        );
        self.sampler
            .final_density_column(x, z, y_start, output);
    }

    /// Whether this bounded region owns the production 4×8×4 final-density
    /// plan. Other routes continue using [`Self::final_density_column`].
    #[must_use]
    pub fn supports_final_density_cells(&self) -> bool {
        self.sampler.supports_final_density_cells()
    }

    /// Evaluates one bounded 4×8×4 cell and preserves the region scratch's
    /// shared corner lattice across adjacent cells.
    pub fn final_density_cell(&self, x0: i32, y0: i32, z0: i32, output: &mut [f64; 128]) {
        assert!(x0 >= self.bounds.x.0 && x0 + 3 <= self.bounds.x.1);
        assert!(y0 >= self.bounds.y.0 && y0 + 7 <= self.bounds.y.1);
        assert!(z0 >= self.bounds.z.0 && z0 + 3 <= self.bounds.z.1);
        self.sampler.final_density_cell(x0, y0, z0, output);
    }

    /// Proves a cell is solid or fills `output` with its exact densities.
    #[must_use]
    pub fn final_density_cell_or_positive(
        &self,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) -> bool {
        assert!(x0 >= self.bounds.x.0 && x0 + 3 <= self.bounds.x.1);
        assert!(y0 >= self.bounds.y.0 && y0 + 7 <= self.bounds.y.1);
        assert!(z0 >= self.bounds.z.0 && z0 + 3 <= self.bounds.z.1);
        self.sampler
            .final_density_cell_or_positive(x0, y0, z0, output)
    }

    /// Proves that every emitted block in a production cell has positive
    /// final density without materialising its 128 values. A false result is
    /// conservative; callers must use [`Self::final_density_cell`] unchanged
    /// to obtain the exact lanes.
    #[must_use]
    pub fn final_density_cell_is_positive(&self, x0: i32, y0: i32, z0: i32) -> bool {
        assert!(x0 >= self.bounds.x.0 && x0 + 3 <= self.bounds.x.1);
        assert!(y0 >= self.bounds.y.0 && y0 + 7 <= self.bounds.y.1);
        assert!(z0 >= self.bounds.z.0 && z0 + 3 <= self.bounds.z.1);
        self.sampler.final_density_cell_is_positive(x0, y0, z0)
    }
}

impl NoiseChunkSampler {
    /// Creates a sampler. `cell_width`/`cell_height` are the settings-derived
    /// cell-width/cell-height values — usually 4 and 8, with 8 and 4 in the End.
    ///
    /// Compiles `root` into a fresh [`Program`]. Callers generating many chunks
    /// should compile once and use [`from_program`](Self::from_program) instead;
    /// this constructor exists for one-shot use and for tests that build a tree
    /// and sample it immediately.
    #[must_use]
    pub fn new(root: Density, slot_count: usize, cell_width: i32, cell_height: i32) -> Self {
        Self::from_program(Program::compile(&root), slot_count, cell_width, cell_height, None)
    }

    /// Creates a sampler whose corner/flat-cache memoisation is a bounded,
    /// hash-free dense array instead of a hash map.
    ///
    /// **Contract**: `x_range`/`y_range`/`z_range` (each `(min, max)`,
    /// inclusive) must bound *every* coordinate this sampler's
    /// [`final_density`](Self::final_density)/[`sample`](Self::sample) will ever
    /// be called with. A query outside the declared bounds trips a
    /// `debug_assert!` in debug builds; in release builds it would silently
    /// alias a different cell, so this constructor is for callers with a known,
    /// small query region — not a drop-in replacement for [`new`](Self::new)
    /// everywhere. `AquiferSystem`'s `erosion`/`depth` samplers query scattered,
    /// not exhaustively-bounded, positions (`is_deep_dark_region`'s padded
    /// grid-cell search legitimately reaches outside the current chunk) and keep
    /// using [`new`](Self::new); its `final_density` sampler is only ever queried
    /// at exact chunk-bounded positions and does use this.
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
        Self::from_program(
            Program::compile(&root),
            slot_count,
            cell_width,
            cell_height,
            Some(Bounds {
                x: x_range,
                y: y_range,
                z: z_range,
            }),
        )
    }

    /// Creates a sampler over an already-compiled [`Program`], taking a scratch
    /// from this thread's free list.
    ///
    /// This is the per-chunk constructor: the `Program` clone the caller passes
    /// is an `Arc` bump, and the scratch is recycled, so building a sampler for
    /// the next chunk allocates nothing in the steady state. `bounds` selects
    /// the dense (bounded) or hashed (unbounded) cache form — see
    /// [`new_bounded`](Self::new_bounded) for the contract the `Some` case
    /// carries.
    #[must_use]
    pub fn from_program(
        program: Program,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Option<Bounds>,
    ) -> Self {
        Self::from_program_with_xz_products(
            program,
            slot_count,
            cell_width,
            cell_height,
            bounds,
            None,
        )
    }

    /// Creates a sampler over an already-compiled program and an optional
    /// immutable request-scoped X/Z product lattice.
    #[must_use]
    pub fn from_program_with_xz_products(
        program: Program,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Option<Bounds>,
        products: Option<Arc<XzProductLattice>>,
    ) -> Self {
        let scratch = Scratch::acquire(slot_count, cell_width, cell_height, bounds);
        Self {
            program,
            geom: Geom {
                cell_width,
                cell_height,
            },
            bounds,
            products,
            scratch: RefCell::new(Some(scratch)),
        }
    }

    /// The interpolated final-density value at a block, matching
    /// vanilla's own noise-chunk sampler's interpolated-density accessor.
    #[must_use]
    pub fn final_density(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval_root(x, y, z)
    }

    /// Evaluates this sampler's root at a block through vanilla's own
    /// noise-chunk-sampler wrapping (so `flat_cache` snaps XZ to the quart grid and forces `y = 0`,
    /// exactly as vanilla's wrapped `router.erosion()` / `router.depth()` do
    /// when the aquifer computes them at a `SinglePointContext`). For an
    /// `interpolated` root this is identical to
    /// [`final_density`](Self::final_density); for the non-interpolated router
    /// routes it is the value-correct point evaluation.
    #[must_use]
    pub fn sample(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval_root(x, y, z)
    }

    /// Evaluates a contiguous vertical run at one `(x, z)` without rebuilding
    /// the field context for every block. The result is bit-identical to
    /// calling [`Self::final_density`] for each y in the run.
    pub fn final_density_column(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        output: &mut [f64],
    ) {
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockField,
            output.len() as u64,
            output.len() as u64 * 8,
        );
        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        Field::new_with_products(
            self.program.graph(),
            self.geom,
            scratch,
            self.products.as_deref(),
        )
        .eval_column(
            self.program.root(),
            x,
            z,
            y_start,
            output,
        );
    }

    /// Returns whether this sampler can use the bounded 4×8×4 production
    /// final-density cell plan.
    #[must_use]
    pub fn supports_final_density_cells(&self) -> bool {
        self.geom.cell_width == 4
            && self.geom.cell_height == 8
            && self.program.has_overworld_final_density_cell_plan()
    }

    /// Evaluates one complete 4×8×4 final-density cell. Matching programs use
    /// the production specialization; all other programs retain the generic
    /// scalar evaluator as a correctness fallback.
    pub fn final_density_cell(&self, x0: i32, y0: i32, z0: i32, output: &mut [f64; 128]) {
        self.assert_cell_in_bounds(x0, y0, z0);
        self.final_density_cell_with_field(x0, y0, z0, output);
    }

    /// Proves a cell is solid, or fills `output` with its exact densities when
    /// the proof is inconclusive. The same field evaluator handles both paths,
    /// so a failed proof can reuse the corner and slot values it populated.
    pub fn final_density_cell_or_positive(
        &self,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) -> bool {
        self.assert_cell_in_bounds(x0, y0, z0);
        let Some(plan) = self.program.overworld_final_density_plan() else {
            self.final_density_cell(x0, y0, z0, output);
            return false;
        };
        if self.geom.cell_width != 4
            || self.geom.cell_height != 8
            || x0.rem_euclid(4) != 0
            || y0.rem_euclid(8) != 0
            || z0.rem_euclid(4) != 0
        {
            self.final_density_cell(x0, y0, z0, output);
            return false;
        }

        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        let mut field = Field::new_with_products(
            self.program.graph(),
            self.geom,
            scratch,
            self.products.as_deref(),
        );
        if field.eval_overworld_final_density_cell_is_positive(plan, x0, y0, z0) {
            return true;
        }
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockField,
            128,
            128 * 8,
        );
        field.eval_overworld_final_density_cell(plan, x0, y0, z0, output);
        false
    }

    fn final_density_cell_with_field(
        &self,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) {
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockField,
            128,
            128 * 8,
        );
        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        let mut field = Field::new_with_products(
            self.program.graph(),
            self.geom,
            scratch,
            self.products.as_deref(),
        );
        if self.geom.cell_width == 4
            && self.geom.cell_height == 8
            && x0.rem_euclid(4) == 0
            && y0.rem_euclid(8) == 0
            && z0.rem_euclid(4) == 0
            && let Some(plan) = self.program.overworld_final_density_plan()
        {
            field.eval_overworld_final_density_cell(plan, x0, y0, z0, output);
            return;
        }

        for lz in 0..4 {
            for lx in 0..4 {
                for ly in 0..8 {
                    let index = ((lz * 4 + lx) * 8 + ly) as usize;
                    output[index] = field.eval::<true>(
                        self.program.root(),
                        x0 + lx,
                        y0 + ly,
                        z0 + lz,
                    );
                }
            }
        }
    }

    /// Proves that every emitted block in a production cell has positive
    /// final density without materialising its 128 values. A false result is
    /// conservative; callers must use [`Self::final_density_cell`] unchanged
    /// to obtain the exact lanes.
    #[must_use]
    pub fn final_density_cell_is_positive(&self, x0: i32, y0: i32, z0: i32) -> bool {
        self.assert_cell_in_bounds(x0, y0, z0);
        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        let Some(plan) = self.program.overworld_final_density_plan() else {
            return false;
        };
        if self.geom.cell_width != 4
            || self.geom.cell_height != 8
            || x0.rem_euclid(4) != 0
            || y0.rem_euclid(8) != 0
            || z0.rem_euclid(4) != 0
        {
            return false;
        }
        Field::new_with_products(
            self.program.graph(),
            self.geom,
            scratch,
            self.products.as_deref(),
        )
        .eval_overworld_final_density_cell_is_positive(plan, x0, y0, z0)
    }

    fn assert_cell_in_bounds(&self, x0: i32, y0: i32, z0: i32) {
        if let Some(bounds) = self.bounds {
            assert!(x0 >= bounds.x.0 && x0 + 3 <= bounds.x.1);
            assert!(y0 >= bounds.y.0 && y0 + 7 <= bounds.y.1);
            assert!(z0 >= bounds.z.0 && z0 + 3 <= bounds.z.1);
        }
    }

    fn eval_root(&self, x: i32, y: i32, z: i32) -> f64 {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockField, 1, 8);
        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        Field::new_with_products(
            self.program.graph(),
            self.geom,
            scratch,
            self.products.as_deref(),
        )
        .eval::<true>(
            self.program.root(),
            x,
            y,
            z,
        )
    }
}

impl Drop for NoiseChunkSampler {
    fn drop(&mut self) {
        if let Some(s) = self.scratch.borrow_mut().take() {
            s.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NoiseChunkRegionSampler, NoiseChunkSampler};
    use crate::density::Density;
    use crate::engine::{Bounds, Program};
    use crate::rng::{Algorithm, PositionalRandomFactory};
    use crate::noise::NormalNoise;

    const CELL_WIDTH: i32 = 4;
    const CELL_HEIGHT: i32 = 8;
    const MIN_Y: i32 = 0;
    const HEIGHT: i32 = 384;

    fn gradient_program() -> Program {
        Program::compile(&Density::Interpolated {
            inner: Box::new(Density::YClampedGradient {
                from_y: -16.0,
                to_y: 400.0,
                from_value: -1.0,
                to_value: 1.0,
            }),
            slot: 0,
        })
    }

    fn coordinate_program() -> Program {
        let mut source = Algorithm::Xoroshiro
            .root_positional(42)
            .from_hash_of("region-test");
        let noise = NormalNoise::create(&mut source, -1, &[1.0, 1.0]);
        Program::compile(&Density::Interpolated {
            inner: Box::new(Density::ShiftA(noise)),
            slot: 0,
        })
    }

    fn bounds(cx: i32, cz: i32) -> Bounds {
        Bounds {
            x: (cx * 16, cx * 16 + 15),
            y: (MIN_Y, MIN_Y + HEIGHT - 1),
            z: (cz * 16, cz * 16 + 15),
        }
    }

    #[test]
    fn column_run_matches_point_queries_bit_for_bit() {
        let root = Density::Interpolated {
            inner: Box::new(Density::YClampedGradient {
                from_y: -16.0,
                to_y: 32.0,
                from_value: -1.0,
                to_value: 1.0,
            }),
            slot: 0,
        };
        let sampler = NoiseChunkSampler::new_bounded(
            root,
            1,
            4,
            8,
            (0, 15),
            (-16, 31),
            (0, 15),
        );
        let mut column = vec![0.0; 48];
        sampler.final_density_column(7, 9, -16, &mut column);
        for (offset, got) in column.iter().enumerate() {
            let want = sampler.final_density(7, -16 + offset as i32, 9);
            assert_eq!(got.to_bits(), want.to_bits(), "y={}", -16 + offset as i32);
        }
    }

    #[test]
    fn region_matches_independent_columns_in_any_order() {
        let program = gradient_program();
        let mut expected = Vec::new();
        for cz in -2..=2 {
            for cx in -2..=2 {
                let sampler = NoiseChunkSampler::from_program(
                    program.clone(),
                    1,
                    CELL_WIDTH,
                    CELL_HEIGHT,
                    Some(bounds(cx, cz)),
                );
                let mut column = vec![0.0; HEIGHT as usize * 256];
                let mut offset = 0;
                for z in cz * 16..cz * 16 + 16 {
                    for x in cx * 16..cx * 16 + 16 {
                        sampler.final_density_column(
                            x,
                            z,
                            MIN_Y,
                            &mut column[offset..offset + HEIGHT as usize],
                        );
                        offset += HEIGHT as usize;
                    }
                }
                expected.push(column);
            }
        }

        let region_bounds = Bounds {
            x: (-32, 47),
            y: (MIN_Y, MIN_Y + HEIGHT - 1),
            z: (-32, 47),
        };
        let forward = NoiseChunkRegionSampler::from_program(
            program.clone(),
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            region_bounds,
        );
        let mut forward_columns = Vec::new();
        for cz in -2..=2 {
            for cx in -2..=2 {
                let mut column = vec![0.0; HEIGHT as usize * 256];
                let mut offset = 0;
                for z in cz * 16..cz * 16 + 16 {
                    for x in cx * 16..cx * 16 + 16 {
                        forward.final_density_column(
                            x,
                            z,
                            MIN_Y,
                            &mut column[offset..offset + HEIGHT as usize],
                        );
                        offset += HEIGHT as usize;
                    }
                }
                forward_columns.push(column);
            }
        }
        assert_eq!(forward_columns, expected);

        let reverse = NoiseChunkRegionSampler::from_program(
            program,
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            region_bounds,
        );
        let mut reverse_columns = vec![vec![0.0; HEIGHT as usize * 256]; expected.len()];
        for index in (0..expected.len()).rev() {
            let cx = index as i32 % 5 - 2;
            let cz = index as i32 / 5 - 2;
            let mut offset = 0;
            for z in cz * 16..cz * 16 + 16 {
                for x in cx * 16..cx * 16 + 16 {
                    reverse.final_density_column(
                        x,
                        z,
                        MIN_Y,
                        &mut reverse_columns[index][offset..offset + HEIGHT as usize],
                    );
                    offset += HEIGHT as usize;
                }
            }
        }
        assert_eq!(reverse_columns, expected);
    }

    #[test]
    fn translated_regions_do_not_alias_coordinates() {
        let program = coordinate_program();
        let first = NoiseChunkRegionSampler::from_program(
            program.clone(),
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            Bounds {
                x: (0, 15),
                y: (MIN_Y, MIN_Y + HEIGHT - 1),
                z: (0, 15),
            },
        );
        let translated = NoiseChunkRegionSampler::from_program(
            program,
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            Bounds {
                x: (64, 79),
                y: (MIN_Y, MIN_Y + HEIGHT - 1),
                z: (64, 79),
            },
        );
        let mut first_values = vec![0.0; HEIGHT as usize];
        let mut translated_values = vec![0.0; HEIGHT as usize];
        first.final_density_column(7, 9, MIN_Y, &mut first_values);
        translated.final_density_column(71, 73, MIN_Y, &mut translated_values);
        assert!(
            first_values
                .iter()
                .zip(&translated_values)
                .any(|(left, right)| left.to_bits() != right.to_bits()),
            "translated coordinate region unexpectedly produced identical noise"
        );
    }

    fn final_density_fixture(control: Density, terrain: Density) -> Density {
        let interpolated = |inner, slot| Density::Interpolated {
            inner: Box::new(inner),
            slot,
        };
        Density::Min(
            Box::new(Density::Squeeze(Box::new(interpolated(terrain, 0)))),
            Box::new(Density::RangeChoice {
                input: Box::new(interpolated(control, 14)),
                min_inclusive: -1_000_000.0,
                max_exclusive: 0.0,
                when_in_range: Box::new(Density::Const(64.0)),
                when_out_of_range: Box::new(Density::Add(
                    Box::new(interpolated(Density::Const(-2.0), 15)),
                    Box::new(Density::Mul(
                        Box::new(Density::Const(1.5)),
                        Box::new(Density::Max(
                            Box::new(Density::Abs(Box::new(interpolated(
                                Density::Const(0.0),
                                16,
                            )))),
                            Box::new(Density::Abs(Box::new(interpolated(
                                Density::Const(0.25),
                                17,
                            )))),
                        )),
                    )),
                )),
            }),
        )
    }

    #[test]
    fn specialized_cell_matches_columns_at_cell_boundaries_and_branch_crossing() {
        let program = Program::compile(&final_density_fixture(
            Density::YClampedGradient {
                from_y: 0.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: 1.0,
            },
            Density::YClampedGradient {
                from_y: -8.0,
                to_y: 16.0,
                from_value: -1.0,
                to_value: 1.0,
            },
        ));
        assert!(program.has_overworld_final_density_cell_plan());
        let sampler = NoiseChunkRegionSampler::from_program(
            program.clone(),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Bounds {
                x: (0, 7),
                y: (0, 15),
                z: (0, 7),
            },
        );
        assert!(sampler.supports_final_density_cells());
        let scalar = NoiseChunkSampler::from_program(
            program,
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 7),
                y: (0, 15),
                z: (0, 7),
            }),
        );
        for z0 in [0, 4] {
            for x0 in [0, 4] {
                for y0 in [0, 8] {
                    let mut cell = [0.0; 128];
                    sampler.final_density_cell(x0, y0, z0, &mut cell);
                    for lz in 0..4 {
                        for lx in 0..4 {
                            let mut column = [0.0; 8];
                            scalar.final_density_column(
                                x0 + lx,
                                z0 + lz,
                                y0,
                                &mut column,
                            );
                            for ly in 0..8 {
                                let index = ((lz * 4 + lx) * 8 + ly) as usize;
                                assert_eq!(
                                    cell[index].to_bits(),
                                    column[ly as usize].to_bits(),
                                    "cell boundary ({x0},{y0},{z0}) local ({lx},{ly},{lz})"
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut crossing_cell = [0.0; 128];
        sampler.final_density_cell(0, 0, 0, &mut crossing_cell);
        let low = crossing_cell[3];
        let high = crossing_cell[4];
        assert_ne!(low.to_bits(), high.to_bits(), "the control branch did not cross inside the cell");
    }

    #[test]
    fn positive_cell_proof_is_bitwise_safe_and_rejects_mixed_negative_noodle() {
        let positive_program = Program::compile(&final_density_fixture(
            Density::Const(-1.0),
            Density::Const(2.0),
        ));
        let positive = NoiseChunkSampler::from_program(
            positive_program.clone(),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        assert!(positive.final_density_cell_is_positive(0, 0, 0));
        let mut positive_cell = [0.0; 128];
        positive.final_density_cell(0, 0, 0, &mut positive_cell);
        let positive_combined = NoiseChunkSampler::from_program(
            positive_program.clone(),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        let mut positive_output = [f64::NAN; 128];
        assert!(positive_combined.final_density_cell_or_positive(0, 0, 0, &mut positive_output));
        assert!(positive_output.iter().all(|value| value.is_nan()));
        let positive_scalar = NoiseChunkSampler::from_program(
            positive_program,
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        for (index, value) in positive_cell.iter().enumerate() {
            let lz = index / 32;
            let lx = (index % 32) / 8;
            let ly = index % 8;
            assert!(
                *value > 0.0,
                "proven positive lane {index} was not positive"
            );
            assert_eq!(
                value.to_bits(),
                positive_scalar
                    .final_density(lx as i32, ly as i32, lz as i32)
                    .to_bits(),
                "proven positive lane {index} changed"
            );
        }

        let mixed_program = Program::compile(&final_density_fixture(
            Density::YClampedGradient {
                from_y: 0.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: 1.0,
            },
            Density::Const(2.0),
        ));
        let mixed = NoiseChunkSampler::from_program(
            mixed_program.clone(),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        assert!(!mixed.final_density_cell_is_positive(0, 0, 0));
        let mut mixed_cell = [0.0; 128];
        mixed.final_density_cell(0, 0, 0, &mut mixed_cell);
        let mixed_combined = NoiseChunkSampler::from_program(
            mixed_program.clone(),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        let mut mixed_output = [0.0; 128];
        assert!(!mixed_combined.final_density_cell_or_positive(0, 0, 0, &mut mixed_output));
        assert_eq!(
            mixed_output
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            mixed_cell
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
        let mixed_scalar = NoiseChunkSampler::from_program(
            mixed_program,
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        for (index, value) in mixed_cell.iter().enumerate() {
            let lz = index / 32;
            let lx = (index % 32) / 8;
            let ly = index % 8;
            assert_eq!(
                value.to_bits(),
                mixed_scalar
                    .final_density(lx as i32, ly as i32, lz as i32)
                    .to_bits(),
                "mixed-control fallback lane {index} changed"
            );
        }
        assert!(mixed_cell.iter().any(|value| *value <= 0.0));
    }

    #[test]
    fn positive_cell_proof_rejects_nonfinite_and_signed_zero_corners() {
        for terrain in [
            Density::Const(f64::NAN),
            Density::Const(f64::INFINITY),
            Density::Const(-0.0),
            Density::Const(0.0),
        ] {
            let sampler = NoiseChunkSampler::from_program(
                Program::compile(&final_density_fixture(
                    Density::Const(-1.0),
                    terrain,
                )),
                18,
                CELL_WIDTH,
                CELL_HEIGHT,
                Some(Bounds {
                    x: (0, 3),
                    y: (0, 7),
                    z: (0, 3),
                }),
            );
            assert!(!sampler.final_density_cell_is_positive(0, 0, 0));
        }
    }

    #[test]
    fn nonmatching_program_keeps_scalar_cell_fallback() {
        let program = gradient_program();
        assert!(!program.has_overworld_final_density_cell_plan());
        let sampler = NoiseChunkSampler::from_program(
            program.clone(),
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        let mut cell = [0.0; 128];
        sampler.final_density_cell(0, 0, 0, &mut cell);
        for lz in 0..4 {
            for lx in 0..4 {
                for ly in 0..8 {
                    let index = ((lz * 4 + lx) * 8 + ly) as usize;
                    assert_eq!(
                        cell[index].to_bits(),
                        sampler.final_density(lx, ly, lz).to_bits(),
                    );
                }
            }
        }
    }

    #[test]
    fn matching_program_falls_back_for_other_geometry_and_unaligned_cells() {
        let program = Program::compile(&final_density_fixture(
            Density::YClampedGradient {
                from_y: 0.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: 1.0,
            },
            Density::YClampedGradient {
                from_y: -8.0,
                to_y: 16.0,
                from_value: -1.0,
                to_value: 1.0,
            },
        ));
        assert!(program.has_overworld_final_density_cell_plan());
        for (cell_width, cell_height, origin) in [(8, 4, (0, 0, 0)), (4, 8, (1, 1, 1))] {
            let sampler = NoiseChunkSampler::from_program(
                program.clone(),
                18,
                cell_width,
                cell_height,
                Some(Bounds {
                    x: (0, 7),
                    y: (0, 15),
                    z: (0, 7),
                }),
            );
            let mut cell = [0.0; 128];
            sampler.final_density_cell(origin.0, origin.1, origin.2, &mut cell);
            for lz in 0..4 {
                for lx in 0..4 {
                    for ly in 0..8 {
                        let index = ((lz * 4 + lx) * 8 + ly) as usize;
                        assert_eq!(
                            cell[index].to_bits(),
                            sampler
                                .final_density(origin.0 + lx, origin.1 + ly, origin.2 + lz)
                                .to_bits(),
                            "fallback geometry ({cell_width},{cell_height}) origin {origin:?}"
                        );
                    }
                }
            }
        }
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn inactive_noodle_slots_are_not_entered_and_active_control_is_a_negative_control() {
        let noisy = |seed| {
            let mut source = Algorithm::Xoroshiro
                .root_positional(seed)
                .from_hash_of("inactive-noodle");
            Density::ShiftA(NormalNoise::create(&mut source, -1, &[1.0, 1.0]))
        };
        let build = |control| {
            let interpolated = |inner, slot| Density::Interpolated {
                inner: Box::new(inner),
                slot,
            };
            Density::Min(
                Box::new(Density::Squeeze(Box::new(interpolated(
                    Density::Const(2.0),
                    0,
                )))),
                Box::new(Density::RangeChoice {
                    input: Box::new(interpolated(control, 14)),
                    min_inclusive: -1_000_000.0,
                    max_exclusive: 0.0,
                    when_in_range: Box::new(Density::Const(64.0)),
                    when_out_of_range: Box::new(Density::Add(
                        Box::new(interpolated(noisy(15), 15)),
                        Box::new(Density::Mul(
                            Box::new(Density::Const(1.5)),
                            Box::new(Density::Max(
                                Box::new(Density::Abs(Box::new(interpolated(noisy(16), 16)))),
                                Box::new(Density::Abs(Box::new(interpolated(noisy(17), 17)))),
                            )),
                        )),
                    )),
                }),
            )
        };
        let bounds = Bounds {
            x: (0, 3),
            y: (0, 7),
            z: (0, 3),
        };
        let mut cell = [0.0; 128];
        let inactive = NoiseChunkSampler::from_program(
            Program::compile(&build(Density::Const(-1.0))),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(bounds),
        );
        crate::counters::reset();
        inactive.final_density_cell(0, 0, 0, &mut cell);
        let inactive_counts = crate::counters::snapshot();
        let shift_a = Density::KIND_NAMES
            .iter()
            .position(|name| *name == "shift_a")
            .expect("shift_a kind");
        assert_eq!(inactive_counts.density_evals[shift_a], 0);

        let active = NoiseChunkSampler::from_program(
            Program::compile(&build(Density::Const(1.0))),
            18,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(bounds),
        );
        crate::counters::reset();
        active.final_density_cell(0, 0, 0, &mut cell);
        assert!(crate::counters::snapshot().density_evals[shift_a] > 0);
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn region_evaluates_each_interpolation_corner_once() {
        crate::counters::reset();
        let sampler = NoiseChunkRegionSampler::from_program(
            gradient_program(),
            1,
            CELL_WIDTH,
            CELL_HEIGHT,
            Bounds {
                x: (-32, 47),
                y: (MIN_Y, MIN_Y + HEIGHT - 1),
                z: (-32, 47),
            },
        );
        let mut density = vec![0.0; HEIGHT as usize];
        for cz in -2..=2 {
            for cx in -2..=2 {
                for z in cz * 16..cz * 16 + 16 {
                    for x in cx * 16..cx * 16 + 16 {
                        sampler.final_density_column(x, z, MIN_Y, &mut density);
                    }
                }
            }
        }
        let snapshot = crate::counters::snapshot();
        let region_corners = 21_u64 * 49 * 21;
        let isolated_corners = 25_u64 * 5 * 49 * 5;
        assert_eq!(snapshot.corner_evals, region_corners);
        assert_eq!(isolated_corners - region_corners, 9_016);
    }
}
