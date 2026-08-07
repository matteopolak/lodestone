//! [`RegionView`] — the **in-place** decoration medium: a read/write surface over
//! the 3×3 neighbourhood's own already-computed grids, with writes held in a
//! sparse overlay instead of a stitched copy of the neighbourhood.
//!
//! # What it is
//!
//! Unit 7 of `docs/plans/worldgen-rewrite.md`. Every decoration stage in this
//! engine needs to read *and write* across a 3×3 chunk neighbourhood, because
//! vanilla's `blockStateWriteRadius(1)` at the FEATURES stage lets a feature
//! placed in one chunk write into its neighbour. Until this type existed, the way
//! that neighbourhood was made addressable was to **copy** all nine chunks'
//! `16 × height × 16` fields into one fresh `48 × height × 48` grid, decorate
//! that, and copy the centre back out:
//!
//! | pass | cells |
//! |---|---|
//! | `stitch_region` — 9 sources into a fresh `RegionGrid` | 884,736 |
//! | `apply_ore_step_3x3_per_source`'s own `grid.clone()` | 884,736 |
//! | `stitch_veg_region` — 9 sources into a `VegGrid` `HashMap` | 884,736 |
//! | the two fold-backs of the centre 16×16 | 196,608 |
//!
//! ~2.85M cell copies per served column, **every one of them warm** — the
//! neighbours were already computed and memoised in the store; the copy existed
//! only to make them addressable through one coordinate space. Vanilla decorates
//! in place and copies nothing. `docs/plans/worldgen-rewrite.md`'s diagnostic D2
//! names this, and the unit's acceptance criterion is
//! `crate::counters::Counters::stitch_cells == 0`.
//!
//! # How it works
//!
//! A read is *routed* rather than pre-copied:
//!
//! 1. the **overlay** — a sparse `HashMap` of writes this decoration pass has
//!    made. Consulted first, so a feature placed earlier in the step is visible
//!    to a later one, exactly as a shared mutable block field would be. This is
//!    the property vanilla's incremental heightmaps depend on, and the reason the
//!    overlay cannot be replaced by a post-pass merge.
//! 2. the **source grid that owns that column** — [`source_slot`] maps a
//!    centre-relative local `(lx, lz)` to one of the nine chunks, and the read
//!    goes straight into that chunk's own `DenseBlockGrid` at absolute
//!    coordinates. The sources are borrowed, never copied, and never written.
//! 3. **air**, for anything outside the driven region.
//!
//! Step 3 is not a fallback for "we forgot to stitch it": the grid this replaced
//! was constructed with `StateId::AIR` as its default over exactly
//! `[REGION_MIN, REGION_MAX)`, so a read outside the 3×3 answered air *there
//! too*. Preserving that is why this type re-derives the region bound from the
//! same [`REGION_MIN`]/[`REGION_MAX`] constants the copy did, rather than from
//! its own constant.
//!
//! # Why the sources are read-only, and what that buys
//!
//! `docs/plans/worldgen-rewrite.md`'s parallel model requires that **every
//! chunk's grid have exactly one writer — its own serve task**. So this view may
//! not write through to a neighbour's grid even though it can read one: a
//! neighbour's product arrives as a read-only `Arc` snapshot out of the staged
//! store, shared with every other in-flight column that has the same neighbour.
//! Holding writes in the overlay is what keeps that true. It is also why the
//! centre's writes are folded back by the *caller* — the caller owns the one
//! grid that is allowed to change.
//!
//! # How to change it, and the trap
//!
//! **This is the coordinate space the `VegGrid` absolute-vs-local bug lived in.**
//! That bug stored and exposed local coordinates while the placement engine
//! handed it absolute `BlockPos`es, so every write outside chunk `(0, 0)` failed
//! an implicit bounds test and **vegetation reached zero blocks in every served
//! chunk with the unit suite green** — see [`crate::feature::vegetation::VegGrid`]'s
//! own doc comment, and note that the gate which caught it was later deleted while
//! a comment went on naming it. Consequences for anyone editing this file:
//!
//! * **This type is addressed in centre-relative *local* coordinates on every
//!   method**, matching [`crate::feature::OreInput::region_local`]'s key space,
//!   which is what the ore engine already computes. `VegGrid` is the absolute
//!   coordinate adapter over the same idea and translates at its own boundary.
//!   Do not add an absolute-coordinate method here; add it there.
//! * **[`source_slot`] is the only place the 3×3 routing is written**, and
//!   [`RegionView::over_sources`] fills the slots *through the same function* it
//!   later reads them with, so a slot-order convention cannot drift between the
//!   two halves. Do not index `sources` directly.
//! * A boundary-write control is a permanent requirement of this file, not a
//!   one-off: a feature that legitimately spills across the seam must be asserted
//!   **present on both sides**. `region_view_carries_a_write_across_the_chunk_seam`
//!   below is the unit-level half; the production-seam half is
//!   `a_canopy_spans_the_chunk_seam_in_both_served_chunks` in
//!   `crates/lodestone-server/tests/decoration_seam_spill.rs`. Both were observed
//!   **failing** against a `/ 16` routing bug before being trusted (5 of the 9
//!   tests here fail, and the production control drops from 20 crossings to 0).
//!   Treat those names as claims and grep for them — the predecessor of the
//!   production control was deleted while a comment went on naming it.
//!
//! # Configuration
//!
//! None. The region bound comes from [`REGION_MIN`]/[`REGION_MAX`] and the
//! vertical bound from the generator's `min_y`/`height`.
//!
//! # Dependencies
//!
//! [`crate::dense_grid::DenseBlockGrid`] for the sources, and
//! [`crate::interner`] for the `StateId`↔`&str` shim `get`/`set` still need.
//! There is still **no shared buffer pool** — see [`scratch`], which is the
//! `thread_local` free-list this module's own doc named as the only acceptable
//! form of reuse here, installed by Unit 19. A pool behind a lock would
//! re-create exactly the contention [`crate::overworld::store`] exists to
//! delete, and `4307b59` is the scar for getting that wrong.

use std::sync::Arc;

use crate::dense_grid::DenseBlockGrid;
use crate::interner::{StateId, StateInterner};

use super::{REGION_MAX, REGION_MIN};

pub(crate) use scratch::{Overlay, WriteLog};

/// Per-thread recycling for the two container shapes decoration writes into.
///
/// # Why this exists
///
/// Unit 8 took a warm served column from 20,678 heap allocations to 87, of which
/// 41 are the returned column's own palette/blocks buffers (the rewrite plan's
/// explicit O(1) output allowance) and **30 were these two containers growing
/// geometrically from empty on every column** — `VegGrid`'s write overlay and its
/// `dirty` log, plus [`RegionView`]'s overlay. `O(log writes)` each, so ~13 per
/// vegetation pass. Unit 8 could not remove them because they are this unit's
/// medium, and Unit 7 had recorded a deliberate decision not to pool them.
///
/// # Why it is a `thread_local` free-list and not a pool
///
/// The constraint that made Unit 7 avoid reuse is real and unchanged: a source
/// grid is an `Arc` shared with every other in-flight column that has the same
/// neighbour, and the rewrite plan's parallel model gives each chunk's grid
/// exactly one writer. A **shared** pool behind a lock would put 289 concurrent
/// generator calls back into cache contention on one cache line —
/// [`crate::overworld::store`]'s own doc makes "no shared pool here, ever" a
/// change rule, and commit `4307b59` is the measured incident behind it. A
/// free-list in thread-local storage has no cross-thread edge at all: a buffer
/// that migrates (taken on one thread, dropped on another) is still correct,
/// merely relocated.
///
/// # How to change it, and the two traps
///
/// * **A recycled `HashMap` has a different capacity, and therefore a different
///   iteration order, from a fresh one.** That is only safe because neither
///   consumer observes iteration order: `VegGrid`'s map is private to its module
///   and reached solely through `get`/`insert` (never iterated at all), and
///   [`RegionView::centre_writes_in_scan_order`] sorts by the full key precisely
///   so order cannot be observed — see the ordering argument on
///   [`lodestone_worldgen_core::hash::fast`], and `crate::overworld`'s module doc
///   for the palette-order bug this repo has already shipped once. **Before
///   letting anything else share this free-list, establish that half of the
///   argument at the new consumer**, exactly as a `FastMap` swap would require.
/// * **Take-and-return, never borrow across a body.** [`Overlay`] and
///   [`WriteLog`] own their buffer and hand it back in `Drop`, so a nested or
///   re-entrant construction gets a *fresh* (merely allocating) buffer instead of
///   a `RefCell` panic — the same discipline Unit 8's `place.rs`/`tree.rs`
///   scratch uses, and for the same reason.
mod scratch {
    use std::cell::{Cell, RefCell};

    use lodestone_worldgen_core::hash::FastMap;

    use crate::interner::StateId;

    /// The overlay's key: centre-relative local `(lx, y, lz)` for
    /// [`super::RegionView`], `VegGrid`-local for the vegetation grid. Both are
    /// "local coordinates in that medium's own space" — the distinction the
    /// absolute-vs-local bug lived in — so this alias deliberately does **not**
    /// claim which.
    type Key = (i32, i32, i32);

    /// How many buffers of each shape one thread keeps. Steady state needs one
    /// per shape per simultaneously-live medium (ore's view and vegetation's grid
    /// are sequential within a column, so one); the rest of the headroom exists so
    /// a re-entrant or nested construction does not permanently lose its buffer.
    ///
    /// A bound rather than an unbounded `Vec` because this is thread-local storage
    /// that lives as long as the thread: an unbounded free-list is a leak wearing
    /// a cache's clothes.
    const KEEP: usize = 4;

    thread_local! {
        static MAPS: RefCell<Vec<FastMap<Key, StateId>>> = const { RefCell::new(Vec::new()) };
        static LOGS: RefCell<Vec<Vec<Key>>> = const { RefCell::new(Vec::new()) };
        /// Takes that found the free-list empty and had to build a container from
        /// scratch. `const`-initialised so reading it cannot itself allocate.
        ///
        /// **This is the instrument that makes a residual allocation count
        /// attributable.** A warm column still allocating a handful of times says
        /// nothing on its own about *which* container is responsible; this
        /// separates "one of these containers escaped the free-list" from "the
        /// residual is somewhere else entirely" without needing a profiler or a
        /// call-site attribution pass. Always compiled in, like `ids`' fast/slow counters and
        /// for the same reason: it is per-*medium*, not per-block, so it costs a
        /// thread-local increment a few times per column.
        static MISSES: Cell<u64> = const { Cell::new(0) };
    }

    /// A sparse `(local) -> StateId` overlay whose backing map is recycled
    /// through this thread's free-list.
    ///
    /// Cleared on **return** rather than on take, so a buffer sitting in the
    /// free-list holds no keys: a stale entry can never be read by whoever takes
    /// it next, which is the property that makes reuse invisible.
    #[derive(Debug)]
    pub(crate) struct Overlay {
        /// `Option` only so [`Drop`] can move the map out. Every method sees it
        /// as present; `expect` is unreachable outside `Drop`.
        map: Option<FastMap<Key, StateId>>,
    }

    impl Default for Overlay {
        fn default() -> Self {
            let recycled = MAPS
                .try_with(|free| free.try_borrow_mut().ok().and_then(|mut f| f.pop()))
                .ok()
                .flatten();
            if recycled.is_none() {
                bump_miss();
            }
            Self { map: Some(recycled.unwrap_or_default()) }
        }
    }

    impl Overlay {
        fn map(&self) -> &FastMap<Key, StateId> {
            self.map.as_ref().expect("overlay map is only taken in Drop")
        }

        fn map_mut(&mut self) -> &mut FastMap<Key, StateId> {
            self.map.as_mut().expect("overlay map is only taken in Drop")
        }

        pub(crate) fn get(&self, key: &Key) -> Option<StateId> {
            self.map().get(key).copied()
        }

        pub(crate) fn insert(&mut self, key: Key, state: StateId) {
            self.map_mut().insert(key, state);
        }

        /// Number of **distinct** cells written — `HashMap::len`, unchanged, so
        /// overwriting a cell does not grow it.
        pub(crate) fn len(&self) -> usize {
            self.map().len()
        }

        /// Every entry. Only for a consumer that imposes a total order of its own
        /// — see this module's doc.
        pub(crate) fn iter(&self) -> impl Iterator<Item = (&Key, &StateId)> {
            self.map().iter()
        }
    }

    impl Drop for Overlay {
        fn drop(&mut self) {
            let Some(mut map) = self.map.take() else {
                return;
            };
            // `clear` keeps the capacity — that is the whole point — and is what
            // makes the next taker's view identical to a fresh map's.
            map.clear();
            let _ = MAPS.try_with(|free| {
                if let Ok(mut f) = free.try_borrow_mut() {
                    if f.len() < KEEP {
                        f.push(map);
                    }
                }
            });
        }
    }

    /// A write-order log of local keys whose backing `Vec` is recycled through
    /// this thread's free-list. `VegGrid::dirty`'s medium.
    ///
    /// Write **order** is world-visible here (`vegetation_stage` folds back in
    /// insertion order and a `DenseBlockGrid` appends to its palette in
    /// first-write order), so this stays a `Vec` and nothing about recycling
    /// touches that: a returned log is empty and a taken one is appended to from
    /// index 0, exactly as `Vec::new()` was.
    #[derive(Debug)]
    pub(crate) struct WriteLog {
        entries: Option<Vec<Key>>,
    }

    impl Default for WriteLog {
        fn default() -> Self {
            let recycled = LOGS
                .try_with(|free| free.try_borrow_mut().ok().and_then(|mut f| f.pop()))
                .ok()
                .flatten();
            if recycled.is_none() {
                bump_miss();
            }
            Self { entries: Some(recycled.unwrap_or_default()) }
        }
    }

    impl WriteLog {
        fn entries(&self) -> &Vec<Key> {
            self.entries.as_ref().expect("write log is only taken in Drop")
        }

        pub(crate) fn push(&mut self, key: Key) {
            self.entries.as_mut().expect("write log is only taken in Drop").push(key);
        }

        pub(crate) fn len(&self) -> usize {
            self.entries().len()
        }

        pub(crate) fn iter(&self) -> impl Iterator<Item = &Key> {
            self.entries().iter()
        }
    }

    impl Drop for WriteLog {
        fn drop(&mut self) {
            let Some(mut entries) = self.entries.take() else {
                return;
            };
            entries.clear();
            let _ = LOGS.try_with(|free| {
                if let Ok(mut f) = free.try_borrow_mut() {
                    if f.len() < KEEP {
                        f.push(entries);
                    }
                }
            });
        }
    }

    /// How many buffers of each shape this thread is currently holding — for the
    /// gates in `tests/vegetation_allocs.rs`, which need to distinguish "the
    /// free-list served the buffer" from "the count happened to be low".
    #[must_use]
    pub(crate) fn free_list_lengths() -> (usize, usize) {
        let maps = MAPS.with(|f| f.borrow().len());
        let logs = LOGS.with(|f| f.borrow().len());
        (maps, logs)
    }

    fn bump_miss() {
        let _ = MISSES.try_with(|m| m.set(m.get().wrapping_add(1)));
    }

    /// Takes on this thread that found the free-list empty and had to allocate.
    pub(crate) fn misses() -> u64 {
        MISSES.try_with(Cell::get).unwrap_or(0)
    }

    /// Zeroes [`misses`] for this thread.
    pub(crate) fn reset_misses() {
        let _ = MISSES.try_with(|m| m.set(0));
    }

    /// Drops every buffer this thread is holding. The control half of the
    /// allocation gate — see [`super::drain_scratch_free_lists`].
    pub(crate) fn drain_free_lists() {
        MAPS.with(|f| f.borrow_mut().clear());
        LOGS.with(|f| f.borrow_mut().clear());
    }
}

/// Which of the nine source chunks owns centre-relative local column
/// `(lx, lz)`, or `None` for a column outside the driven 3×3 region.
///
/// `div_euclid` rather than `/ 16`: local coordinates are negative across the
/// west/north third of the region (`REGION_MIN` is -16), and truncating division
/// maps both `-1` and `-16` to chunk offset `0`, which would route the whole
/// western neighbour into the centre. That is the same off-by-a-chunk shape as
/// the absolute-vs-local bug this module's doc records, so it is spelled out
/// here and tested exhaustively over the full region below.
#[must_use]
pub fn source_slot(lx: i32, lz: i32) -> Option<usize> {
    let dx = lx.div_euclid(16);
    let dz = lz.div_euclid(16);
    if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dz) {
        return None;
    }
    Some(slot_of_offset(dx, dz))
}

/// The slot index for chunk offset `(dx, dz)` ∈ `[-1, 1]²`.
///
/// Private on purpose: [`RegionView::over_sources`] is the only filler and it
/// derives the index from [`source_slot`] applied to that offset's own origin
/// column, so the fill and the lookup cannot disagree.
fn slot_of_offset(dx: i32, dz: i32) -> usize {
    ((dx + 1) * 3 + (dz + 1)) as usize
}

/// How many recycled buffers of each shape (overlay map, write log) this thread
/// currently holds — for a gate that needs to tell "the free-list served this
/// buffer" apart from "the count happened to be low".
#[must_use]
pub fn scratch_free_list_lengths() -> (usize, usize) {
    scratch::free_list_lengths()
}

/// Empties this thread's scratch free-lists, so the next medium constructed on
/// this thread has to allocate its containers from scratch.
///
/// **This exists to make the allocation gate's control real.** An assertion that
/// a warm pass allocates zero is worth exactly as much as the evidence the
/// mechanism would have fired, and the mechanism here is the free-list. Draining
/// it and re-running the *same* pass must put the allocations back — the same
/// binary, the same code path, the same scene, one variable. See
/// `recycling_is_what_removes_the_containers_and_draining_the_free_list_puts_them_back`
/// in `tests/vegetation_allocs.rs`.
pub fn drain_scratch_free_lists() {
    scratch::drain_free_lists();
}

/// How many decoration media on this thread had to **allocate** their containers
/// because the free-list was empty, since [`reset_scratch_misses`].
///
/// This is what makes a residual allocation count attributable rather than merely
/// small. A warm column that still allocates a handful of times leaves open which
/// container is responsible; a zero here says **neither of these two is**, so the
/// residual belongs to something else and the search moves elsewhere. It is the
/// counter form of the question, which `DESIGN.md` §12 prefers to a duration.
#[must_use]
pub fn scratch_misses() -> u64 {
    scratch::misses()
}

/// Zeroes [`scratch_misses`] for this thread.
pub fn reset_scratch_misses() {
    scratch::reset_misses();
}

/// A read/write view over one column's 3×3 decoration neighbourhood.
///
/// Addressed in **centre-relative local** coordinates on every method
/// (`lx, lz ∈ [REGION_MIN, REGION_MAX)`, `y` absolute) — see the module doc.
#[allow(missing_debug_implementations)]
pub struct RegionView<'a> {
    /// The nine sources, indexed by [`slot_of_offset`]. `None` means "this
    /// offset was not supplied", which reads as air — the single-source debug
    /// paths in [`crate::overworld`] use exactly that.
    sources: [Option<&'a DenseBlockGrid>; 9],
    /// Absolute block coordinate that local `(0, 0)` maps to. The centre
    /// chunk's own origin in production; `(0, 0)` for a fixture whose single
    /// backing grid is *already* addressed in region-local coordinates.
    origin_x: i32,
    origin_z: i32,
    min_y: i32,
    height: i32,
    /// Writes made through this view, keyed local. Sparse: decoration writes a
    /// few thousand cells per column against a 884,736-cell region, which is
    /// the whole reason the region does not need materialising.
    ///
    /// [`Overlay`]: a [`lodestone_worldgen_core::hash::FastMap`] whose buffer is
    /// recycled through [`scratch`]'s per-thread free-list. Not the default
    /// hasher: this was the single hottest hash consumer in the whole pipeline
    /// when U17 profiled it — **39.5% of all SipHash time**, because ore
    /// placement probes it on every read and inserts on every write — and
    /// `reserve_rehash` showed up on top of that at 6.8% as it grew, which is the
    /// half U19's recycling removes (a reused buffer is already at capacity).
    ///
    /// Both re-hashing it *and* recycling it are safe for the same single reason:
    /// [`Self::centre_writes_in_scan_order`] sorts by the full key rather than
    /// trusting iteration order — see the ordering argument on
    /// [`lodestone_worldgen_core::hash::fast`], and the doc on that method,
    /// which was already written to defend against exactly this.
    overlay: Overlay,
    interner: Arc<StateInterner>,
}

impl<'a> RegionView<'a> {
    /// A view over the nine chunks of `centre ± 1`.
    ///
    /// `source_at(dx, dz)` is called once per offset in `[-1, 1]²` and returns
    /// that chunk's own already-computed grid, addressed in **absolute** world
    /// coordinates. Returning `None` makes that chunk read as air.
    ///
    /// The slots are filled by looking each offset's own origin column up
    /// through [`source_slot`] — the same function every read uses — so there is
    /// no second copy of the routing convention to keep in step.
    #[must_use]
    pub fn over_sources(
        interner: Arc<StateInterner>,
        centre_cx: i32,
        centre_cz: i32,
        min_y: i32,
        height: i32,
        source_at: impl Fn(i32, i32) -> Option<&'a DenseBlockGrid>,
    ) -> Self {
        let mut sources: [Option<&'a DenseBlockGrid>; 9] = [None; 9];
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                let slot = source_slot(dx * 16, dz * 16)
                    .expect("a 3x3 offset's own origin column is inside the region");
                debug_assert_eq!(slot, slot_of_offset(dx, dz));
                sources[slot] = source_at(dx, dz);
            }
        }
        Self {
            sources,
            origin_x: centre_cx * 16,
            origin_z: centre_cz * 16,
            min_y,
            height,
            overlay: Overlay::default(),
            interner,
        }
    }

    /// A view over **one** grid that is already addressed in centre-relative
    /// region-local coordinates — the shape a parity fixture builds, since a
    /// fixture is naturally one sparse `HashMap` over the whole region rather
    /// than nine per-chunk fields.
    ///
    /// Every slot points at that same grid with `origin = (0, 0)`, so a read
    /// still goes through [`source_slot`] and the routing is exercised by the
    /// JVM fixtures rather than only by production. This is deliberate: a
    /// fixture that bypassed the routing would be the "world" species of vacuous
    /// test — a transport complete enough to pass while resolving to a different
    /// implementation than production uses.
    #[must_use]
    pub fn over_region_grid(grid: &'a DenseBlockGrid, min_y: i32, height: i32) -> Self {
        Self {
            sources: [Some(grid); 9],
            origin_x: 0,
            origin_z: 0,
            min_y,
            height,
            overlay: Overlay::default(),
            interner: Arc::clone(grid.interner()),
        }
    }

    /// This view's interner, for a caller that needs to mint or resolve ids
    /// against it.
    #[must_use]
    pub fn interner(&self) -> &Arc<StateInterner> {
        &self.interner
    }

    /// Whether local `(lx, y, lz)` is inside the driven region — i.e. inside the
    /// box the stitched `RegionGrid` this view replaced was constructed over.
    /// Outside it, reads answer air and writes are dropped, exactly as
    /// `DenseBlockGrid`'s own out-of-box contract did.
    fn in_region(&self, lx: i32, y: i32, lz: i32) -> bool {
        (REGION_MIN..REGION_MAX).contains(&lx)
            && (REGION_MIN..REGION_MAX).contains(&lz)
            && y >= self.min_y
            && y < self.min_y + self.height
    }

    /// Interned state at local `(lx, y, lz)`: this pass's own write if there is
    /// one, else the owning source chunk's, else [`StateId::AIR`].
    #[must_use]
    pub fn get_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        if !self.in_region(lx, y, lz) {
            return StateId::AIR;
        }
        if let Some(id) = self.overlay.get(&(lx, y, lz)) {
            return id;
        }
        match source_slot(lx, lz).and_then(|slot| self.sources[slot]) {
            Some(grid) => grid.get_id(self.origin_x + lx, y, self.origin_z + lz),
            None => StateId::AIR,
        }
    }

    /// [`Self::get_id`] resolved to a canonical state string.
    ///
    /// A source hit is a plain array read out of that grid's own resolved
    /// palette (no lock, no allocation). An **overlay** hit costs one
    /// `StateInterner::name_of` read guard, which is why the overlay is the
    /// smaller of the two cases by orders of magnitude: it holds only cells this
    /// decoration pass has already written.
    #[must_use]
    pub fn get(&self, lx: i32, y: i32, lz: i32) -> &str {
        if !self.in_region(lx, y, lz) {
            return "minecraft:air";
        }
        if let Some(id) = self.overlay.get(&(lx, y, lz)) {
            return self.interner.name_of(id);
        }
        match source_slot(lx, lz).and_then(|slot| self.sources[slot]) {
            Some(grid) => grid.get(self.origin_x + lx, y, self.origin_z + lz),
            None => "minecraft:air",
        }
    }

    /// Records a write at local `(lx, y, lz)`. Dropped outside the driven
    /// region, matching the no-op-outside-the-box contract of the grid this
    /// replaced. Returns whether the write landed.
    pub fn set_id(&mut self, lx: i32, y: i32, lz: i32, state: StateId) -> bool {
        if !self.in_region(lx, y, lz) {
            return false;
        }
        self.overlay.insert((lx, y, lz), state);
        true
    }

    /// [`Self::set_id`] taking a state string, interning it first.
    pub fn set(&mut self, lx: i32, y: i32, lz: i32, state: &str) -> bool {
        let id = self.interner.id_of(state);
        self.set_id(lx, y, lz, id)
    }

    /// Number of distinct cells written through this view.
    #[must_use]
    pub fn writes(&self) -> usize {
        self.overlay.len()
    }

    /// Every write that landed in the **centre** chunk's own 16×16 columns, in
    /// `(y, lz, lx)` order — what the caller folds back into the one grid it
    /// owns.
    ///
    /// # The ordering is load-bearing, and it is not about determinism alone
    ///
    /// The fold-back this replaced walked the *whole* centre `16 × height × 16`
    /// box in exactly `(y, lz, lx)` order and called `set_id` on every cell,
    /// unchanged ones included. A `DenseBlockGrid` appends to its local palette
    /// in **first-write order**, and that palette is what
    /// `into_palette_and_blocks` emits to the wire — so the order in which new
    /// states are first written decides the served bytes.
    ///
    /// Applying only the changed cells is byte-identical to the full walk *iff*
    /// they are applied in the same order, because:
    ///
    /// * an **unchanged** cell's state came out of the centre grid itself, so it
    ///   is already in that grid's palette and re-writing it cannot append; and
    /// * therefore every state that is *new* to the palette lives at a written
    ///   cell, and the subsequence of new states seen in `(y, lz, lx)` order is
    ///   the same whether the walk visits the unchanged cells or skips them.
    ///
    /// Sorting is by the full key over a `HashMap`'s unique keys, so the order is
    /// total and does not depend on iteration order — the `RandomState` trap
    /// `crate::overworld`'s module doc records is avoided by construction rather
    /// than by hoping the map iterates the same way twice.
    #[must_use]
    pub fn centre_writes_in_scan_order(&self) -> Vec<(i32, i32, i32, StateId)> {
        let mut out: Vec<(i32, i32, i32, StateId)> = self
            .overlay
            .iter()
            .filter(|((lx, _, lz), _)| (0..16).contains(lx) && (0..16).contains(lz))
            .map(|(&(lx, y, lz), &id)| (lx, y, lz, id))
            .collect();
        out.sort_unstable_by_key(|&(lx, y, lz, _)| (y, lz, lx));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The routing, checked against an independently written formula over the
    /// **entire** local range the two decoration media use — the ore region
    /// `[REGION_MIN, REGION_MAX)` plus `VegGrid`'s padding ring on both sides.
    ///
    /// The independent expectation is written as an explicit table walk rather
    /// than as `div_euclid` again, so this is not `f(x) == f(x)`.
    #[test]
    fn source_slot_routes_every_local_column_to_the_chunk_that_owns_it() {
        let pad = super::super::VEG_PADDING;
        for lx in (REGION_MIN - pad)..(REGION_MAX + pad) {
            for lz in (REGION_MIN - pad)..(REGION_MAX + pad) {
                // Independent derivation: walk the three 16-wide bands by hand.
                let band = |v: i32| -> Option<i32> {
                    match v {
                        -16..=-1 => Some(-1),
                        0..=15 => Some(0),
                        16..=31 => Some(1),
                        _ => None,
                    }
                };
                let expected = match (band(lx), band(lz)) {
                    (Some(dx), Some(dz)) => Some(((dx + 1) * 3 + (dz + 1)) as usize),
                    _ => None,
                };
                assert_eq!(
                    source_slot(lx, lz),
                    expected,
                    "local column ({lx}, {lz}) routed to the wrong source chunk",
                );
            }
        }
    }

    /// The negative control for the test above: truncating division — the
    /// obvious way to write this, and the way that is wrong — really does
    /// misroute the western/northern third into the centre. Without this, the
    /// exhaustive test could be passing for a reason unrelated to `div_euclid`.
    #[test]
    fn truncating_division_would_misroute_the_western_third() {
        let truncating = |lx: i32, lz: i32| -> Option<usize> {
            let (dx, dz) = (lx / 16, lz / 16);
            if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dz) {
                return None;
            }
            Some(((dx + 1) * 3 + (dz + 1)) as usize)
        };
        // (-1, -1) is the last column of the north-west neighbour.
        assert_eq!(source_slot(-1, -1), Some(0), "north-west neighbour");
        assert_eq!(
            truncating(-1, -1),
            Some(4),
            "control: the truncating form must be observed routing this to the \
             CENTRE, or the exhaustive routing test proves nothing about div_euclid",
        );
        assert_ne!(source_slot(-1, -1), truncating(-1, -1));
    }

    /// A recycled buffer must be indistinguishable from a fresh one. This is the
    /// whole correctness surface of [`scratch`]: a returned buffer is cleared on
    /// the way *out*, so a stale entry can never be read by whoever takes it next.
    ///
    /// The control half matters as much as the claim. Without the second
    /// assertion — that the buffer really was the recycled one — this test passes
    /// identically against a free-list that never hands anything back, which is
    /// the *premise-false* shape: it would be testing `HashMap::new()`.
    #[test]
    fn a_recycled_overlay_is_empty_and_is_really_the_recycled_one() {
        scratch::drain_free_lists();
        {
            let mut first = Overlay::default();
            for y in 0..64 {
                first.insert((1, y, 2), StateId::AIR);
            }
            assert_eq!(first.len(), 64);
        }
        // Control: the drop really did return it, so the take below is a reuse and
        // not a fresh allocation wearing the same name.
        assert_eq!(
            scratch::free_list_lengths().0,
            1,
            "control: the dropped overlay must be on this thread's free-list, or the \
             emptiness assertion below is about a brand-new map and proves nothing \
             about recycling",
        );
        let second = Overlay::default();
        assert_eq!(second.len(), 0, "a recycled overlay must carry no stale keys");
        assert_eq!(
            second.get(&(1, 7, 2)),
            None,
            "a key written through the previous holder must not be visible",
        );
        assert_eq!(scratch::free_list_lengths().0, 0, "the take must consume it");
    }

    /// The same contract for the write log, and additionally that write **order**
    /// starts at index 0 — the log's order reaches the served palette through
    /// `vegetation_stage`'s fold-back, so a recycled log that retained entries
    /// would be a world-visible defect, not a leak.
    #[test]
    fn a_recycled_write_log_is_empty_and_restarts_at_index_zero() {
        scratch::drain_free_lists();
        {
            let mut first = WriteLog::default();
            for y in 0..32 {
                first.push((3, y, 4));
            }
            assert_eq!(first.len(), 32);
        }
        assert_eq!(
            scratch::free_list_lengths().1,
            1,
            "control: the dropped log must be on the free-list, or this test is \
             about a fresh Vec",
        );
        let mut second = WriteLog::default();
        assert_eq!(second.len(), 0, "a recycled write log must be empty");
        second.push((9, 9, 9));
        assert_eq!(
            second.iter().copied().collect::<Vec<_>>(),
            vec![(9, 9, 9)],
            "the first push into a recycled log must land at index 0",
        );
    }

    /// The free-list is bounded, so it cannot become a leak that only shows up on a
    /// long-lived worker thread.
    #[test]
    fn the_free_list_does_not_grow_without_bound() {
        scratch::drain_free_lists();
        let overlays: Vec<Overlay> = (0..32).map(|_| Overlay::default()).collect();
        drop(overlays);
        let (maps, _) = scratch::free_list_lengths();
        assert!(
            maps <= 4,
            "32 overlays were dropped and the free-list kept {maps} of them; a \
             thread-local cache with no bound is a leak wearing a cache's clothes",
        );
        assert!(maps > 0, "control: it must keep at least one, or reuse never happens");
    }

    fn chunk_grid(interner: &Arc<StateInterner>, cx: i32, cz: i32, state: &str) -> DenseBlockGrid {
        let air = interner.id_of("minecraft:air");
        let mut grid =
            DenseBlockGrid::with_interner(Arc::clone(interner), cx * 16, 0, cz * 16, 16, 8, 16, air);
        for lx in 0..16 {
            for lz in 0..16 {
                grid.set(cx * 16 + lx, 4, cz * 16 + lz, state);
            }
        }
        grid
    }

    /// Each of the nine chunks must be read back through its own grid. The
    /// distinct per-chunk state is what makes a misroute visible: with one
    /// shared state every routing bug would look correct.
    #[test]
    fn a_read_resolves_to_the_source_chunk_that_owns_the_column() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| {
                chunk_grid(
                    &interner,
                    10 + dx,
                    -20 + dz,
                    &format!("minecraft:marker_{dx}_{dz}"),
                )
            })
            .collect();
        let view = RegionView::over_sources(Arc::clone(&interner), 10, -20, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                for (lx, lz) in [(0, 0), (15, 15), (7, 3)] {
                    let (qx, qz) = (dx * 16 + lx, dz * 16 + lz);
                    assert_eq!(
                        view.get(qx, 4, qz),
                        format!("minecraft:marker_{dx}_{dz}"),
                        "local ({qx}, {qz}) must read out of chunk offset ({dx}, {dz})",
                    );
                }
            }
        }
    }

    /// A view over nine real chunk grids must answer **cell for cell** what the
    /// stitched `48 × height × 48` copy answered — the differential control that
    /// makes "the copy is gone" a claim about equivalence and not just about a
    /// counter reaching zero.
    ///
    /// The expected side is built by the deleted algorithm itself (copy all nine
    /// sources into one dense region grid), so this compares the new routing
    /// against the old copy over the whole box rather than against a restatement
    /// of the new routing.
    #[test]
    fn a_view_answers_cell_for_cell_what_the_stitched_copy_answered() {
        let interner = Arc::new(StateInterner::new());
        // Terrain with per-column variety, so a transposition or an off-by-a-chunk
        // shows up rather than being masked by a uniform field.
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| {
                let air = interner.id_of("minecraft:air");
                let mut g = DenseBlockGrid::with_interner(
                    Arc::clone(&interner),
                    (3 + dx) * 16,
                    -64,
                    (7 + dz) * 16,
                    16,
                    12,
                    16,
                    air,
                );
                for lx in 0..16 {
                    for lz in 0..16 {
                        for ly in 0..12 {
                            let x = (3 + dx) * 16 + lx;
                            let z = (7 + dz) * 16 + lz;
                            g.set(x, -64 + ly, z, &format!("minecraft:s{}", (x * 31 + z * 7 + ly) % 5));
                        }
                    }
                }
                g
            })
            .collect();

        // The deleted algorithm, verbatim in shape: one dense region grid, all
        // nine sources copied in.
        let air = interner.id_of("minecraft:air");
        let size = REGION_MAX - REGION_MIN;
        let mut stitched = DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            REGION_MIN,
            -64,
            REGION_MIN,
            size,
            12,
            size,
            air,
        );
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                let src = &grids[((dx + 1) * 3 + (dz + 1)) as usize];
                for ly in 0..12 {
                    for lz in 0..16 {
                        for lx in 0..16 {
                            let y = -64 + ly;
                            let id = src.get_id((3 + dx) * 16 + lx, y, (7 + dz) * 16 + lz);
                            stitched.set_id(dx * 16 + lx, y, dz * 16 + lz, id);
                        }
                    }
                }
            }
        }

        let view = RegionView::over_sources(Arc::clone(&interner), 3, 7, -64, 12, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });

        let mut compared = 0u64;
        for y in -64..-52 {
            for lz in REGION_MIN..REGION_MAX {
                for lx in REGION_MIN..REGION_MAX {
                    assert_eq!(
                        view.get_id(lx, y, lz),
                        stitched.get_id(lx, y, lz),
                        "view disagreed with the stitched copy at local ({lx}, {y}, {lz})",
                    );
                    compared += 1;
                }
            }
        }
        // Non-vacuity: the loop really walked the whole region, and the field
        // really had variety in it (5 distinct states plus air).
        assert_eq!(compared, 12 * 48 * 48, "the comparison did not cover the region");
        let distinct: std::collections::HashSet<StateId> = (REGION_MIN..REGION_MAX)
            .flat_map(|lx| (REGION_MIN..REGION_MAX).map(move |lz| (lx, lz)))
            .map(|(lx, lz)| view.get_id(lx, -60, lz))
            .collect();
        assert!(
            distinct.len() >= 5,
            "the test field is too uniform to detect a misroute: {} distinct states",
            distinct.len(),
        );
    }

    /// The boundary-write control at unit level: a write placed in the centre
    /// but reaching past its own edge must be readable **on both sides of the
    /// seam** — in the centre and in the neighbour's third of the region — and
    /// must not have touched the neighbour's own grid.
    ///
    /// This is the property vanilla's `blockStateWriteRadius(1)` requires and the
    /// one a coordinate-space bug destroys silently.
    #[test]
    fn region_view_carries_a_write_across_the_chunk_seam() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| chunk_grid(&interner, dx, dz, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });

        // A canopy straddling the centre's east edge: x = 14..18, so 14/15 are
        // the centre's own columns and 16/17 are the eastern neighbour's.
        for lx in 14..18 {
            assert!(
                view.set(lx, 5, 8, "minecraft:oak_leaves"),
                "write at local x={lx} must land inside the driven region",
            );
        }
        for lx in 14..18 {
            assert_eq!(
                view.get(lx, 5, 8),
                "minecraft:oak_leaves",
                "the spilled canopy must be readable at local x={lx}",
            );
        }
        // Both sides, named as such rather than implied by the loop above.
        assert_eq!(source_slot(15, 8), source_slot(0, 0), "x=15 is the centre");
        assert_ne!(
            source_slot(16, 8),
            source_slot(15, 8),
            "x=16 must be a different chunk, or this test is not at a seam",
        );
        // The neighbour's own grid is untouched: sources are read-only, because a
        // neighbour's product is an `Arc` shared with every other in-flight column
        // and the parallel model gives each chunk exactly one writer.
        let east = &grids[slot_of_offset(1, 0)];
        assert_eq!(
            east.get(16, 5, 8),
            "minecraft:air",
            "the spilled write must have landed in the overlay, not in the \
             neighbour's shared grid",
        );
        // …and the neighbour grid really is the one being read, not an empty
        // stand-in: its own terrain row is still there.
        assert_eq!(
            east.get(16, 4, 8),
            "minecraft:stone",
            "control: the east neighbour's own grid must be non-empty, or the \
             assertion above is satisfied by looking at nothing",
        );

        // Only the centre's own columns are folded back.
        let folded = view.centre_writes_in_scan_order();
        assert_eq!(folded.len(), 2, "exactly x=14 and x=15 belong to the centre");
        assert_eq!(folded[0].0, 14);
        assert_eq!(folded[1].0, 15);
    }

    /// The fold-back order must be `(y, lz, lx)` — the order the full-box walk
    /// visited cells in, and therefore the order that reproduces its palette.
    #[test]
    fn centre_writes_come_back_in_y_then_z_then_x_order() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (0..9)
            .map(|_| chunk_grid(&interner, 0, 0, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });
        // Deliberately written in an order that is neither the expected one nor
        // its reverse.
        for &(lx, y, lz) in &[(3, 6, 1), (1, 2, 3), (9, 2, 3), (1, 2, 0), (0, 6, 1)] {
            view.set(lx, y, lz, "minecraft:diamond_ore");
        }
        let keys: Vec<(i32, i32, i32)> = view
            .centre_writes_in_scan_order()
            .into_iter()
            .map(|(lx, y, lz, _)| (y, lz, lx))
            .collect();
        assert_eq!(
            keys,
            vec![(2, 0, 1), (2, 3, 1), (2, 3, 9), (6, 1, 0), (6, 1, 3)],
        );
    }

    /// A read past the driven region answers air, and a write there is dropped —
    /// the contract the `48 × height × 48` grid had, preserved so the vegetation
    /// grid's unseeded padding ring keeps behaving the way it did.
    #[test]
    fn reads_and_writes_outside_the_region_are_air_and_dropped() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| chunk_grid(&interner, dx, dz, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });
        for (lx, lz) in [(REGION_MIN - 1, 0), (REGION_MAX, 0), (0, REGION_MIN - 1), (0, REGION_MAX)] {
            assert_eq!(view.get(lx, 4, lz), "minecraft:air");
            assert!(!view.set(lx, 4, lz, "minecraft:stone"));
            assert_eq!(view.get(lx, 4, lz), "minecraft:air");
        }
        // Vertically too.
        assert_eq!(view.get(0, -1, 0), "minecraft:air");
        assert_eq!(view.get(0, 8, 0), "minecraft:air");
        assert!(!view.set(0, 8, 0, "minecraft:stone"));
        assert_eq!(view.writes(), 0, "no out-of-region write may have landed");
    }

    /// An overlay write shadows the source underneath it, and a later read in the
    /// same pass sees it — the property vanilla's incremental heightmaps depend
    /// on, and the reason writes cannot be merged after the pass instead.
    #[test]
    fn a_write_shadows_the_source_for_later_reads_in_the_same_pass() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| chunk_grid(&interner, dx, dz, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });
        assert_eq!(view.get(5, 4, 5), "minecraft:stone");
        view.set(5, 4, 5, "minecraft:gold_ore");
        assert_eq!(view.get(5, 4, 5), "minecraft:gold_ore");
        // Overwriting keeps the final value and does not grow the write set.
        view.set(5, 4, 5, "minecraft:iron_ore");
        assert_eq!(view.get(5, 4, 5), "minecraft:iron_ore");
        assert_eq!(view.writes(), 1);
    }

    /// The fixture constructor must answer identically to a nine-source view over
    /// the same content, so a JVM fixture and production resolve to the same read
    /// path rather than to two implementations that merely agree today.
    #[test]
    fn the_region_grid_constructor_answers_like_a_nine_source_view() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let size = REGION_MAX - REGION_MIN;
        let mut region = DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            REGION_MIN,
            0,
            REGION_MIN,
            size,
            8,
            size,
            air,
        );
        let grids: Vec<DenseBlockGrid> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| chunk_grid(&interner, dx, dz, &format!("minecraft:m{dx}_{dz}")))
            .collect();
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                for lx in 0..16 {
                    for lz in 0..16 {
                        region.set(dx * 16 + lx, 4, dz * 16 + lz, &format!("minecraft:m{dx}_{dz}"));
                    }
                }
            }
        }
        let fixture_view = RegionView::over_region_grid(&region, 0, 8);
        let production_view =
            RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
                grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
            });
        for lz in REGION_MIN..REGION_MAX {
            for lx in REGION_MIN..REGION_MAX {
                assert_eq!(
                    fixture_view.get(lx, 4, lz),
                    production_view.get(lx, 4, lz),
                    "fixture and production views disagreed at local ({lx}, {lz})",
                );
            }
        }
    }
}
