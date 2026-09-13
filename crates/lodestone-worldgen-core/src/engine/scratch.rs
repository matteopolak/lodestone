//! Per-chunk mutable evaluation state for the flattened engine.
//!
//! [`Scratch`] holds every cache the block-field evaluator needs. Keeping it
//! out of [`super::Graph`] is what makes the graph immutable, `Sync`, and
//! shareable by `Arc` — the whole basis of deleting the per-chunk `Density`
//! deep clones.
//!
//! # Two cache layers, and why both are needed
//!
//! | layer | keyed by | population per chunk | deletes |
//! |---|---|---|---|
//! | **cell** ([`CellStore`]) | cell triple, per `interpolated` slot | 768 octets | the *lookup*: 786,432 → 6,144 |
//! | **slot** ([`SlotStore`]) | corner block position, per slot | 1,225 values | the *evaluation*: 6,144 → 1,225 |
//!
//! Neither subsumes the other, and dropping either is a measurable regression
//! in a different quantity. Without the cell layer, every one of the 98,304
//! blocks in a chunk performs 8 corner lookups (`98_304 × 8 = 786_432`) even
//! though only 768 distinct cells exist. Without the slot layer, each of those
//! 768 cell fills would *evaluate* its 8 corners from scratch — but adjacent
//! cells share corners, so the true number of distinct corners is
//! `5 × 49 × 5 = 1,225`, and dropping the slot layer would multiply the
//! expensive half of the work by five. The counters
//! (`corner_lookups`, `slot_hit`/`slot_miss`) are one per layer for exactly
//! this reason.
//!
//! # The reuse hazard
//!
//! A pooled [`Scratch`] keeps its allocations across chunks, so **every
//! presence flag must be cleared on reconfigure or a stale value from the
//! previous chunk is returned as if it were this chunk's**. That failure is
//! silent, position-dependent, and produces plausible terrain — the same shape
//! as the interpolation-order bug. [`Scratch::reconfigure`] is the only place
//! the clearing happens and `tests::reuse_clears_presence_flags` is the gate;
//! `docs/worldgen-density-engine.md` records the measured cost of removing it.

use std::collections::HashMap;

/// The caches' hasher.
///
/// The default `HashMap` uses SipHash, which is DoS-resistant but slow; these
/// keys are trusted internal cell coordinates, so a multiply-xor fold is both
/// correct and far cheaper. Choice of hasher is value-invariant here — it
/// changes only lookup speed, never which value is stored or returned, because
/// these two maps are point caches (`get`/`insert`/`clear`) and are never
/// iterated.
///
/// **This used to be a private `FxHasher` declared in this file.** U17 found the
/// same construction was wanted by four more maps outside this crate's `engine/`
/// and promoted it to [`crate::hash::fast`], which is now the one copy — go
/// there for the ordering discipline that has to hold before *any* further map
/// adopts it, and for why `finish` deliberately does not rotate. The only
/// behavioural difference from the version that lived here is that `write`
/// folds eight bytes at a time instead of one, and folds the length; no caller
/// in this file hashes a byte string, so the cached values are unchanged either
/// way.
type FxBuild = crate::hash::FastBuildHasher;

/// The declared inclusive query bounds of a sampler, from which both the slot
/// grid and the cell grid are derived.
///
/// Every coordinate the owning sampler is ever queried with must fall inside
/// these bounds — the contract `NoiseChunkSampler::new_bounded` documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Inclusive `(min, max)` block X.
    pub x: (i32, i32),
    /// Inclusive `(min, max)` block Y.
    pub y: (i32, i32),
    /// Inclusive `(min, max)` block Z.
    pub z: (i32, i32),
}

/// A shared, precomputed arithmetic-indexing scheme for every dense slot in one
/// [`Scratch`], replacing a `HashMap::get` (measured at ~10%+ of this crate's
/// total profiled self-time, `docs/worldgen-surface-perf.md`) with direct `Vec`
/// indexing.
///
/// # Why one uniform shape works for both `interpolated` and `flat_cache`
///
/// Slots back two distinct key shapes: `interpolated` corners are multiples of
/// `cell_width` (X/Z) and `cell_height` (Y); `flat_cache` keys are XZ snapped
/// to the quart grid (multiples of 4, hardcoded — not `cell_width`-parameterised)
/// with `y` forced to exactly `0`. Rather than classifying which of the two
/// shapes each slot needs (which would require walking the graph to see which
/// arm assigned it, mirroring the evaluator's exact recursion including which
/// branches it does *not* enter), every dense slot uses **one** shape wide
/// enough to cover both:
///
/// - X/Z step = `gcd(cell_width, 4)` — divides both key families, so every real
///   key lands on an exact grid point with no aliasing between distinct keys.
/// - Y step = `cell_height`, with the bounds unioned with `0` — `0` is already
///   a multiple of `cell_height`, so `flat_cache`'s forced `y = 0` lands on the
///   *same* grid `interpolated` corners use, needing no separate case.
///
/// The tradeoff: a `flat_cache` slot's grid still spans the full Y range even
/// though only its `y = 0` plane is ever populated. That is bounded, cheap
/// waste, not a correctness issue — an unqueried plane is never allocated into,
/// only sized for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    fn for_bounds(cell_width: i32, cell_height: i32, b: Bounds) -> Self {
        let step_xz = gcd(cell_width, 4);
        let step_y = cell_height;

        let xz_bounds = |lo: i32, hi: i32| -> (i32, i32) {
            let interp_lo = lo.div_euclid(cell_width) * cell_width;
            let interp_hi = hi.div_euclid(cell_width) * cell_width + cell_width;
            let flat_lo = lo.div_euclid(4) * 4;
            let flat_hi = hi.div_euclid(4) * 4 + 4;
            (interp_lo.min(flat_lo), interp_hi.max(flat_hi))
        };
        let (x0, x1) = xz_bounds(b.x.0, b.x.1);
        let (z0, z1) = xz_bounds(b.z.0, b.z.1);

        let y_interp_lo = b.y.0.div_euclid(cell_height) * cell_height;
        let y_interp_hi = b.y.1.div_euclid(cell_height) * cell_height + cell_height;
        let y0 = y_interp_lo.min(0);
        let y1 = y_interp_hi.max(0);

        Self {
            x0,
            y0,
            z0,
            step_xz,
            step_y,
            nx: (x1 - x0) / step_xz + 1,
            ny: (y1 - y0) / step_y + 1,
            nz: (z1 - z0) / step_xz + 1,
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
            "corner key outside the sampler's declared bounds: ({x}, {y}, {z}) -> \
             ({ix}, {iy}, {iz}) not within (0..{}, 0..{}, 0..{})",
            self.nx,
            self.ny,
            self.nz
        );
        (ix as usize * self.ny as usize + iy as usize) * self.nz as usize + iz as usize
    }
}

/// The cell grid a bounded sampler's [`CellStore`] indexes into: one entry per
/// `cell_width × cell_height × cell_width` cell the declared bounds touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellShape {
    cx0: i32,
    cy0: i32,
    cz0: i32,
    nx: i32,
    ny: i32,
    nz: i32,
}

impl CellShape {
    fn for_bounds(cell_width: i32, cell_height: i32, b: Bounds) -> Self {
        let cx0 = b.x.0.div_euclid(cell_width);
        let cx1 = b.x.1.div_euclid(cell_width);
        let cy0 = b.y.0.div_euclid(cell_height);
        let cy1 = b.y.1.div_euclid(cell_height);
        let cz0 = b.z.0.div_euclid(cell_width);
        let cz1 = b.z.1.div_euclid(cell_width);
        Self {
            cx0,
            cy0,
            cz0,
            nx: cx1 - cx0 + 1,
            ny: cy1 - cy0 + 1,
            nz: cz1 - cz0 + 1,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.nx as usize * self.ny as usize * self.nz as usize
    }

    #[inline]
    fn index(&self, cx: i32, cy: i32, cz: i32) -> usize {
        let ix = cx - self.cx0;
        let iy = cy - self.cy0;
        let iz = cz - self.cz0;
        debug_assert!(
            ix >= 0 && ix < self.nx && iy >= 0 && iy < self.ny && iz >= 0 && iz < self.nz,
            "cell ({cx}, {cy}, {cz}) outside the sampler's declared bounds"
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

/// One slot's corner memo. `Dense` is the bounded, hash-free form; `Hashed`
/// works for any position and backs the unbounded samplers.
enum SlotStore {
    Hashed(HashMap<(i32, i32, i32), f64, FxBuild>),
    /// Allocated lazily on first write: most of the 1000+ slots a real settings
    /// graph assigns are never reached by the field evaluator at all (anything
    /// nested inside a point-evaluated leaf) or reached only under an
    /// `interpolated` ancestor, which evaluates its inner transparently and so
    /// never touches that inner's own slot either. Eagerly allocating every
    /// slot's grid would pay for hundreds of grids that are never touched.
    Dense {
        values: Vec<f64>,
        has: Vec<bool>,
    },
}

/// Entries in the leaf memo — see [`Scratch::leaf_get`].
///
/// 64 rather than "one per node" because a `Scratch` is sized from
/// `Builder::slot_count` and has never known the graph's op count. The table is
/// direct-mapped on the node id with a **full key compare**, so the size is a
/// hit-rate choice only: a collision costs a miss, never a wrong value. 64 × 24 B
/// = 1,536 B per scratch against `xz_memo`'s 98 KB per thread, and one `fill` per
/// sampler to clear.
const LEAF_MEMO_LEN: usize = 64;

#[derive(Clone, Copy)]
struct LeafMemo {
    /// `u32::MAX` for empty. Node ids are dense from 0, so the sentinel is
    /// unreachable rather than merely unlikely.
    id: u32,
    x: i32,
    y: i32,
    z: i32,
    value: f64,
}

const LEAF_MEMO_EMPTY: LeafMemo = LeafMemo {
    id: u32::MAX,
    x: 0,
    y: 0,
    z: 0,
    value: 0.0,
};

/// One `interpolated` slot's per-cell corner octets.
enum CellStore {
    Hashed(HashMap<(i32, i32, i32), [f64; 8], FxBuild>),
    Dense {
        values: Vec<[f64; 8]>,
        has: Vec<bool>,
    },
}

/// Per-chunk mutable evaluation state: the two cache layers, sized for one
/// sampler's slot count and (when bounded) query region.
#[allow(missing_debug_implementations)]
pub struct Scratch {
    slots: Vec<SlotStore>,
    cells: Vec<CellStore>,
    dense: Option<DenseShape>,
    cell_shape: Option<CellShape>,
    /// The configuration currently installed, so [`Self::reconfigure`] can tell
    /// a compatible reuse (clear flags, keep allocations) from an incompatible
    /// one (rebuild).
    config: Option<(usize, i32, i32, Option<Bounds>)>,
    /// A per-thread monotonic id, re-issued on every [`Self::acquire`], so
    /// `super::redundancy_probe` can tell "this node was already evaluated at
    /// this position **by another sampler**" from "…by this one".
    ///
    /// It must not be the scratch's *address*: the pool recycles a scratch, so
    /// two samplers alive at different moments in one column can share an
    /// address and would be merged into one scope — which biases the
    /// cross-sampler measurement toward zero, the direction that says "there is
    /// nothing here". One `Cell` increment per sampler construction (tens per
    /// column against ~10^9 instructions) is why this is not behind a feature.
    probe_scope: u64,
    /// A one-slot-per-node last-`(x, y, z)` memo for the field evaluator's
    /// **side-effect-free** node kinds — the four point-evaluated leaves and
    /// `Noise`. See [`Self::leaf_get`] for why those and no others.
    leaf_memo: [LeafMemo; LEAF_MEMO_LEN],
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            cells: Vec::new(),
            dense: None,
            cell_shape: None,
            config: None,
            probe_scope: 0,
            leaf_memo: [LEAF_MEMO_EMPTY; LEAF_MEMO_LEN],
        }
    }
}

impl Scratch {
    /// Takes a scratch from this thread's free list, configured for
    /// `slot_count` slots and the given cell geometry, and `bounds` if the
    /// caller is a bounded sampler.
    ///
    /// Reuse is per-thread and lock-free by construction: a `Scratch` is never
    /// shared, so no locked shared pool is involved.
    #[must_use]
    pub fn acquire(
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Option<Bounds>,
    ) -> Self {
        let mut s = POOL.with(|p| p.borrow_mut().pop()).unwrap_or_default();
        s.reconfigure(slot_count, cell_width, cell_height, bounds);
        s.probe_scope = NEXT_SCOPE.with(|c| {
            let n = c.get() + 1;
            c.set(n);
            n
        });
        s
    }

    /// This sampler's measurement-only scope id. See the field's own doc.
    #[inline]
    pub(crate) fn probe_scope(&self) -> u64 {
        self.probe_scope
    }

    /// Returns this scratch to the thread's free list for the next chunk.
    pub fn release(self) {
        POOL.with(|p| {
            let mut v = p.borrow_mut();
            if v.len() < POOL_CAP {
                v.push(self);
            }
        });
    }

    /// Installs a configuration, reusing allocations when the shape is
    /// unchanged.
    ///
    /// **Every presence flag is cleared here, unconditionally.** A reused
    /// `values` buffer still holds the previous chunk's numbers; only `has`
    /// being false is what stops them being returned. See the module doc's
    /// *reuse hazard*.
    fn reconfigure(
        &mut self,
        slot_count: usize,
        cell_width: i32,
        cell_height: i32,
        bounds: Option<Bounds>,
    ) {
        let want = (slot_count, cell_width, cell_height, bounds);
        // Unconditionally, before the early return. A node **id** is only unique
        // within one `Graph`, and a pooled scratch is handed to a sampler over a
        // *different* graph whenever the two happen to share a config — which
        // `final_density`, `erosion` and `depth` do not (bounds differ) but the
        // three vein programs do. Missing this is the module doc's reuse hazard in
        // its worst form: not a stale value from the previous chunk but a value
        // from a different function, at a plausible magnitude.
        self.leaf_memo = [LEAF_MEMO_EMPTY; LEAF_MEMO_LEN];
        if self.config == Some(want) {
            for s in &mut self.slots {
                match s {
                    SlotStore::Hashed(m) => m.clear(),
                    SlotStore::Dense { has, .. } => has.fill(false),
                }
            }
            for c in &mut self.cells {
                match c {
                    CellStore::Hashed(m) => m.clear(),
                    CellStore::Dense { has, .. } => has.fill(false),
                }
            }
            return;
        }

        let dense = bounds.map(|b| DenseShape::for_bounds(cell_width, cell_height, b));
        let cell_shape = bounds.map(|b| CellShape::for_bounds(cell_width, cell_height, b));
        self.slots.clear();
        self.cells.clear();
        self.slots.reserve(slot_count);
        self.cells.reserve(slot_count);
        for _ in 0..slot_count {
            self.slots.push(if dense.is_some() {
                SlotStore::Dense {
                    values: Vec::new(),
                    has: Vec::new(),
                }
            } else {
                SlotStore::Hashed(HashMap::default())
            });
            self.cells.push(if cell_shape.is_some() {
                CellStore::Dense {
                    values: Vec::new(),
                    has: Vec::new(),
                }
            } else {
                CellStore::Hashed(HashMap::default())
            });
        }
        self.dense = dense;
        self.cell_shape = cell_shape;
        self.config = Some(want);
    }

    /// Reads a cached corner octet for one cell of one `interpolated` slot.
    #[inline]
    pub(crate) fn cell_get(&self, slot: usize, cx: i32, cy: i32, cz: i32) -> Option<[f64; 8]> {
        match &self.cells[slot] {
            CellStore::Hashed(m) => m.get(&(cx, cy, cz)).copied(),
            CellStore::Dense { values, has } => {
                if values.is_empty() {
                    return None;
                }
                let i = self.cell_shape.expect("dense cells need a CellShape").index(cx, cy, cz);
                if has[i] { Some(values[i]) } else { None }
            }
        }
    }

    /// Stores a cell's corner octet.
    #[inline]
    pub(crate) fn cell_put(&mut self, slot: usize, cx: i32, cy: i32, cz: i32, v: [f64; 8]) {
        let shape = self.cell_shape;
        match &mut self.cells[slot] {
            CellStore::Hashed(m) => {
                m.insert((cx, cy, cz), v);
            }
            CellStore::Dense { values, has } => {
                let shape = shape.expect("dense cells need a CellShape");
                if values.is_empty() {
                    values.resize(shape.len(), [0.0; 8]);
                    has.resize(shape.len(), false);
                }
                let i = shape.index(cx, cy, cz);
                values[i] = v;
                has[i] = true;
            }
        }
    }

    /// Reads the one-slot last-`(x, y, z)` memo for a side-effect-free node.
    ///
    /// # Why one slot, and why only these kinds
    ///
    /// Measured (`tests/density_redundancy_probe.rs`, 100 interior columns of the
    /// 12×12 sweep, seed 42): the field evaluator visits `old_blended_noise`
    /// **1,954** times per column of which **977** are at an `(x, y, z)` that node
    /// was asked about *immediately before* — 977 duplicated visits and 977 one-slot
    /// hits, i.e. **the one-slot form catches 100% of them**, and the same holds for
    /// `noise` (1,957 of 14,169). That is the opposite of §12.140's finding in the
    /// point interpreter, where the one-slot form hit 2.1% against a map's 78.2%,
    /// and the difference is entirely the *caller*: there the visits alternate over
    /// a cell's four `(x, z)` corners, here a DAG node with two parents is reached
    /// twice inside one subtree walk (`range_choice` evaluates its input, then a
    /// branch that opens with the same subtree). **A one-slot memo is right when the
    /// duplication is adjacency and wrong when it is recurrence; only measuring both
    /// tells you which you have.**
    ///
    /// The kind restriction is a correctness boundary, not a tuning choice. A memo
    /// hit *skips a subtree*, and in this evaluator a skipped subtree can contain a
    /// `slot_put`/`cell_put` that a later query depends on — which is why
    /// `engine/mod.rs`'s "the walk must stay a recursive descent" rule exists. The
    /// four point-evaluated leaves and `Noise` are the only kinds with **no field
    /// children at all**: a leaf hands its whole subtree to the point interpreter,
    /// which touches no `Scratch`, and `Noise` reads only its own noise and params.
    /// They also ignore `interpolate`, so the flag is correctly absent from the key —
    /// for any other kind it would have to be part of it.
    #[inline]
    pub(crate) fn leaf_get(&self, id: u32, x: i32, y: i32, z: i32) -> Option<f64> {
        let e = self.leaf_memo[(id as usize) & (LEAF_MEMO_LEN - 1)];
        if e.id == id && e.x == x && e.y == y && e.z == z {
            #[cfg(feature = "gen-counters")]
            LEAF_MEMO_HITS.with(|c| c.set(c.get() + 1));
            Some(e.value)
        } else {
            #[cfg(feature = "gen-counters")]
            LEAF_MEMO_MISSES.with(|c| c.set(c.get() + 1));
            None
        }
    }

    /// Stores `(node, x, y, z) -> value`, evicting whatever shared the slot.
    #[inline]
    pub(crate) fn leaf_put(&mut self, id: u32, x: i32, y: i32, z: i32, value: f64) {
        self.leaf_memo[(id as usize) & (LEAF_MEMO_LEN - 1)] = LeafMemo {
            id,
            x,
            y,
            z,
            value,
        };
    }

    /// Reads a cached corner (or `flat_cache`) value for one slot.
    #[inline]
    pub(crate) fn slot_get(&self, slot: usize, key: (i32, i32, i32)) -> Option<f64> {
        match &self.slots[slot] {
            SlotStore::Hashed(m) => m.get(&key).copied(),
            SlotStore::Dense { values, has } => {
                if values.is_empty() {
                    return None;
                }
                let i = self
                    .dense
                    .expect("dense slots need a DenseShape")
                    .index(key.0, key.1, key.2);
                if has[i] { Some(values[i]) } else { None }
            }
        }
    }

    /// Stores a corner (or `flat_cache`) value for one slot.
    #[inline]
    pub(crate) fn slot_put(&mut self, slot: usize, key: (i32, i32, i32), v: f64) {
        let dense = self.dense;
        match &mut self.slots[slot] {
            SlotStore::Hashed(m) => {
                m.insert(key, v);
            }
            SlotStore::Dense { values, has } => {
                let shape = dense.expect("dense slots need a DenseShape");
                if values.is_empty() {
                    values.resize(shape.len(), 0.0);
                    has.resize(shape.len(), false);
                }
                let i = shape.index(key.0, key.1, key.2);
                values[i] = v;
                has[i] = true;
            }
        }
    }
}

/// How many scratches one thread keeps. Small: a thread generates one chunk at
/// a time, and the three samplers an `AquiferSystem` builds have different
/// shapes, so a handful covers the working set without holding megabytes
/// hostage per worker thread.
const POOL_CAP: usize = 8;

thread_local! {
    static POOL: std::cell::RefCell<Vec<Scratch>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Hands out [`Scratch::probe_scope`] ids. Per-thread and never reset, so an
    /// id identifies one sampler for the whole process life.
    static NEXT_SCOPE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static LEAF_MEMO_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static LEAF_MEMO_MISSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `(hits, misses)` for the leaf memo on this thread since [`reset_leaf_memo_stats`].
/// Always `(0, 0)` without `gen-counters`.
///
/// This exists because the *predicted* hit rate and the measured one disagreed by
/// a factor of 40 on the first attempt — a direct-mapped table's real hit rate is a
/// property of what else writes to it, which no probe over node visits can see.
#[must_use]
pub fn leaf_memo_stats() -> (u64, u64) {
    #[cfg(feature = "gen-counters")]
    {
        (
            LEAF_MEMO_HITS.with(std::cell::Cell::get),
            LEAF_MEMO_MISSES.with(std::cell::Cell::get),
        )
    }
    #[cfg(not(feature = "gen-counters"))]
    {
        (0, 0)
    }
}

/// Zeroes this thread's leaf-memo hit/miss counters.
pub fn reset_leaf_memo_stats() {
    #[cfg(feature = "gen-counters")]
    {
        LEAF_MEMO_HITS.with(|c| c.set(0));
        LEAF_MEMO_MISSES.with(|c| c.set(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Bounds = Bounds {
        x: (0, 15),
        y: (-64, 319),
        z: (0, 15),
    };

    /// The derived grid sizes the counter predictions rest on. `5 × 49 × 5 =
    /// 1,225` is the corner lattice for one chunk-bounded interpolated slot and
    /// `4 × 48 × 4 = 768` is its cell count; both appear in
    /// `docs/worldgen-density-engine.md` and in the bench's predictions, so
    /// pinning them here makes a geometry change fail loudly next to the
    /// derivation instead of quietly moving a counter.
    #[test]
    fn grid_sizes_match_the_derived_geometry() {
        let d = DenseShape::for_bounds(4, 8, B);
        assert_eq!((d.nx, d.ny, d.nz), (5, 49, 5), "corner lattice");
        assert_eq!(d.len(), 1_225);
        let c = CellShape::for_bounds(4, 8, B);
        assert_eq!((c.nx, c.ny, c.nz), (4, 48, 4), "cell grid");
        assert_eq!(c.len(), 768);
    }

    /// A reused scratch must not serve the previous configuration's values.
    ///
    /// This is the gate for the module doc's *reuse hazard*: the `values`
    /// buffers are deliberately kept across `reconfigure`, so only the `has`
    /// clearing stands between a pooled scratch and a stale read. The negative
    /// control (removing the `has.fill(false)`) is recorded in
    /// `docs/worldgen-density-engine.md` rather than expressed here, because a
    /// test cannot un-write the code it is testing.
    #[test]
    fn reuse_clears_presence_flags() {
        let mut s = Scratch::default();
        s.reconfigure(2, 4, 8, Some(B));
        s.slot_put(0, (0, 0, 0), 12.5);
        s.cell_put(1, 0, 0, 0, [7.0; 8]);
        assert_eq!(s.slot_get(0, (0, 0, 0)), Some(12.5));
        assert_eq!(s.cell_get(1, 0, 0, 0), Some([7.0; 8]));

        // Same configuration: the allocation-reuse path.
        s.reconfigure(2, 4, 8, Some(B));
        assert_eq!(
            s.slot_get(0, (0, 0, 0)),
            None,
            "a reused slot buffer served the previous chunk's value"
        );
        assert_eq!(
            s.cell_get(1, 0, 0, 0),
            None,
            "a reused cell buffer served the previous chunk's octet"
        );

        // Control: the assertions above would also pass if `slot_put` simply
        // never stored anything, which is the vacuous reading of this test.
        s.slot_put(0, (0, 0, 0), 3.5);
        assert_eq!(s.slot_get(0, (0, 0, 0)), Some(3.5));
    }

    /// The unbounded (hashed) form must clear too, and must accept keys far
    /// outside any chunk — which is why the aquifer's `erosion`/`depth`
    /// samplers use it.
    #[test]
    fn hashed_form_is_unbounded_and_also_clears() {
        let mut s = Scratch::default();
        s.reconfigure(1, 4, 8, None);
        s.slot_put(0, (-9_000, 4_000, 12_345), 1.0);
        assert_eq!(s.slot_get(0, (-9_000, 4_000, 12_345)), Some(1.0));
        s.reconfigure(1, 4, 8, None);
        assert_eq!(s.slot_get(0, (-9_000, 4_000, 12_345)), None);
    }

    /// The leaf memo must compare its **whole** key, and must be cleared by
    /// `reconfigure` — the second is the load-bearing one, because a node id is
    /// unique only within one `Graph` and a pooled scratch crosses graphs.
    ///
    /// The "different node" case is not a nicety: with a 64-entry direct-mapped
    /// table, ids `3` and `67` share a row, so a table that compared only the row
    /// would return a *different function's* value at a plausible magnitude.
    #[test]
    fn leaf_memo_needs_the_whole_key_and_clears_on_reconfigure() {
        let mut s = Scratch::default();
        s.reconfigure(1, 4, 8, Some(B));
        s.leaf_put(3, 4, -8, 12, 1.25);
        assert_eq!(s.leaf_get(3, 4, -8, 12), Some(1.25));
        assert_eq!(s.leaf_get(3, 4, -8, 13), None, "a different y must miss");
        assert_eq!(s.leaf_get(3, 5, -8, 12), None, "a different x must miss");
        assert_eq!(s.leaf_get(3, 4, -7, 12), None, "a different z must miss");
        assert_eq!(
            s.leaf_get(3 + LEAF_MEMO_LEN as u32, 4, -8, 12),
            None,
            "a different node sharing the row must miss"
        );

        // The reuse hazard, at the same configuration (the allocation-reuse path).
        s.reconfigure(1, 4, 8, Some(B));
        assert_eq!(
            s.leaf_get(3, 4, -8, 12),
            None,
            "a reused scratch served the previous graph's leaf value"
        );
        // Control: the assertions above are also satisfied by `leaf_put` storing
        // nothing at all, which is the vacuous reading.
        s.leaf_put(3, 4, -8, 12, 9.5);
        assert_eq!(s.leaf_get(3, 4, -8, 12), Some(9.5));
    }

    /// Acquiring after releasing must hand back a *clean* scratch. This is the
    /// pool's own version of the reuse hazard, one level up: `acquire` calls
    /// `reconfigure`, and if it ever stopped doing so the leak would cross
    /// chunk boundaries instead of configuration boundaries.
    #[test]
    fn pool_round_trip_is_clean() {
        let mut s = Scratch::acquire(1, 4, 8, Some(B));
        s.slot_put(0, (4, 8, 4), 99.0);
        assert_eq!(s.slot_get(0, (4, 8, 4)), Some(99.0));
        s.release();
        let s2 = Scratch::acquire(1, 4, 8, Some(B));
        assert_eq!(
            s2.slot_get(0, (4, 8, 4)),
            None,
            "the pool handed back a dirty scratch"
        );
        s2.release();
    }
}
