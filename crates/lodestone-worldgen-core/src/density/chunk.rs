//! `NoiseChunk`-equivalent block-field sampling.
//!
//! The density-function interpreter in [`super`] evaluates the *raw* noise
//! router at a point (vanilla's own single-point context), which is what the router
//! parity tests prove. But the real server does **not** write that point field
//! to blocks. Vanilla wraps the router in its own per-chunk noise-block field
//! (its own noise-chunk class) which changes two node
//! kinds:
//!
//! * **`interpolated`** samples its wrapped function only at the corners of a
//!   4×8×4 (cell-width×cell-height×cell-width) cell grid, then trilinearly
//!   interpolates per block, in vanilla's own three-axis lerp's nesting — **X innermost**, then
//!   Y, then Z. That order is bit-significant and it is *not* the one vanilla's
//!   driver loop appears to produce; see `## Which interpolation order` below.
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

use super::Density;
use crate::engine::{Bounds, Field, Geom, Program, Scratch};

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
    /// `Option` only so [`Drop`] can move the scratch out and return it to the
    /// thread's free list. It is `Some` for the whole of the sampler's life.
    scratch: RefCell<Option<Scratch>>,
}

impl NoiseChunkSampler {
    /// Creates a sampler. `cell_width`/`cell_height` are
    /// vanilla's own noise-settings cell-width/cell-height queries — 4 and 8 for the
    /// overworld.
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
        let scratch = Scratch::acquire(slot_count, cell_width, cell_height, bounds);
        Self {
            program,
            geom: Geom {
                cell_width,
                cell_height,
            },
            scratch: RefCell::new(Some(scratch)),
        }
    }

    /// The interpolated final-density value at a block, matching
    /// `NoiseChunk.getInterpolatedDensity()`.
    #[must_use]
    pub fn final_density(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval_root(x, y, z)
    }

    /// Evaluates this sampler's root at a block through the `NoiseChunk`
    /// wrapping (so `flat_cache` snaps XZ to the quart grid and forces `y = 0`,
    /// exactly as vanilla's wrapped `router.erosion()` / `router.depth()` do
    /// when the aquifer computes them at a `SinglePointContext`). For an
    /// `interpolated` root this is identical to
    /// [`final_density`](Self::final_density); for the non-interpolated router
    /// routes it is the value-correct point evaluation.
    #[must_use]
    pub fn sample(&self, x: i32, y: i32, z: i32) -> f64 {
        self.eval_root(x, y, z)
    }

    fn eval_root(&self, x: i32, y: i32, z: i32) -> f64 {
        let mut borrow = self.scratch.borrow_mut();
        let scratch = borrow
            .as_mut()
            .expect("the scratch is only taken in Drop, after the last query");
        Field::new(self.program.graph(), self.geom, scratch).eval(
            self.program.root(),
            x,
            y,
            z,
            true,
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
