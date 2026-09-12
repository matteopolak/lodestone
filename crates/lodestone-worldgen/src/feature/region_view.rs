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
//! 1. the **overlay** — a bounded direct-address page directory of writes this
//!    decoration pass has made. Local coordinates are packed into a compact key;
//!    each page cell carries an epoch stamp so a recycled view does not need to
//!    clear every page. Consulted first, so a feature placed earlier in the step
//!    is visible to a later one, exactly as a shared mutable block field would
//!    be. This is the property incremental heightmaps depend on, and the reason
//!    the overlay cannot be replaced by a post-pass merge.
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
//! * **This file holds *two* routing tables and they are not interchangeable.**
//!   [`source_slot`] is the only place the **3×3** routing is written — the ore
//!   engine's, sized to the `[`[`REGION_MIN`]`, `[`REGION_MAX`]`)` box its
//!   `RegionHeights` table and [`crate::feature::OreInput::region_local`] clamp
//!   share. [`wide_source_slot`] is the only place the **5×5** routing is written —
//!   vegetation's *read* neighbourhood, widened because a source's own pass must not
//!   depend on which column is the centre (see [`WIDE_RADIUS`]). Each fills its
//!   slots *through the same function* it later reads them with
//!   ([`RegionView::over_sources`] and
//!   [`crate::feature::vegetation::VegGrid::with_sources`] respectively), so a
//!   slot-order convention cannot drift between the two halves. Do not index
//!   `sources` directly, and do not reach for the wrong table's width.
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
//! * **"Present on both sides" is a weaker claim than "the same tree on both
//!   sides", and only the second one is what the player sees.** Both controls above
//!   pass while the two chunks either side of a seam disagree about the tree
//!   crossing it — the defect the owner reported in-game. The gate for *that* is
//!   `crates/lodestone-worldgen/tests/vegetation_seam_consistency.rs`, and
//!   `docs/worldgen-seam-consistency.md` records what it measures and what is still
//!   open. Note the ore driver has the same structural exposure and is untouched.
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
//! form of reuse here, installed by Unit 19. The direct page storage is owned by
//! one view at a time and only recycled on that same thread. A pool behind a lock would
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
/// * **The overlay is not an iteration-ordered container.** It is a bounded
///   direct-address page directory whose cells carry an epoch stamp. The only
///   consumer that exports overlay contents,
///   [`RegionView::centre_writes_in_scan_order`], sorts by the full key before
///   the palette fold-back, so page allocation order cannot become observable.
///   [`VegGrid`] likewise consumes the separate insertion-order [`WriteLog`].
/// * **Take-and-return, never borrow across a body.** [`Overlay`] and
///   [`WriteLog`] own their buffer and hand it back in `Drop`, so a nested or
///   re-entrant construction gets a *fresh* (merely allocating) buffer instead of
///   a `RefCell` panic — the same discipline Unit 8's `place.rs`/`tree.rs`
///   scratch uses, and for the same reason.
mod scratch {
    use std::cell::{Cell, RefCell};

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

    const PAGE_X: usize = 4;
    const PAGE_Z: usize = 4;
    const PAGE_Y: usize = 16;
    const PAGE_X_SHIFT: u32 = PAGE_X.trailing_zeros();
    const PAGE_Z_SHIFT: u32 = PAGE_Z.trailing_zeros();
    const PAGE_Y_SHIFT: u32 = PAGE_Y.trailing_zeros();
    const PAGE_X_MASK: usize = PAGE_X - 1;
    const PAGE_Z_MASK: usize = PAGE_Z - 1;
    const PAGE_Y_MASK: usize = PAGE_Y - 1;
    const XZ_BITS: u32 = 10;
    const Y_BITS: u32 = 12;
    const XZ_MASK: u32 = (1 << XZ_BITS) - 1;
    const Y_MASK: u32 = (1 << Y_BITS) - 1;
    const DEFAULT_MIN_XZ: i32 = -32;
    const DEFAULT_MAX_XZ: i32 = 48;
    /// The default covers the largest current vegetation footprint (`-32..48`)
    /// and all supported generator heights. RegionView constructors still pass
    /// their narrower read window explicitly.
    const DEFAULT_MIN_Y: i32 = -64;
    const DEFAULT_HEIGHT: i32 = 448;

    thread_local! {
        static OVERLAYS: RefCell<Vec<Storage>> = const { RefCell::new(Vec::new()) };
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

    #[derive(Clone, Copy, Debug)]
    struct EntryCell {
        generation: u32,
        state: StateId,
    }

    #[derive(Debug)]
    struct Page {
        cells: [EntryCell; PAGE_X * PAGE_Y * PAGE_Z],
    }

    impl Page {
        fn new() -> Self {
            Self {
                cells: [EntryCell { generation: 0, state: StateId::AIR };
                    PAGE_X * PAGE_Y * PAGE_Z],
            }
        }
    }

    /// A direct-address directory of lazily allocated pages. The directory is
    /// bounded by the view's coordinate window; a cell's packed local offset
    /// addresses one page and one slot without hashing. Pages and cells stay in
    /// the per-thread scratch free-list after a view drops. On reuse the
    /// generation advances and stale cells are ignored without clearing every
    /// page.
    #[derive(Debug)]
    struct Storage {
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
        min_z: i32,
        max_z: i32,
        x_pages: usize,
        z_pages: usize,
        y_pages: usize,
        generation: u32,
        pages: Vec<Option<Box<Page>>>,
        keys: Vec<u32>,
    }

    impl Storage {
        fn new(min_x: i32, max_x: i32, min_y: i32, height: i32, min_z: i32, max_z: i32) -> Self {
            let storage = Self {
                min_x,
                max_x,
                min_y,
                max_y: min_y + height,
                min_z,
                max_z,
                x_pages: 0,
                z_pages: 0,
                y_pages: 0,
                generation: 0,
                pages: Vec::new(),
                keys: Vec::new(),
            };
            storage
        }

        fn reconfigure(
            &mut self,
            min_x: i32,
            max_x: i32,
            min_y: i32,
            height: i32,
            min_z: i32,
            max_z: i32,
        ) {
            assert!(max_x > min_x && max_z > min_z && height > 0);
            assert!(max_x - min_x <= (1 << XZ_BITS));
            assert!(max_z - min_z <= (1 << XZ_BITS));
            assert!(height <= (1 << Y_BITS));
            let x_pages = ((max_x - min_x) as usize).div_ceil(PAGE_X);
            let z_pages = ((max_z - min_z) as usize).div_ceil(PAGE_Z);
            let y_pages = (height as usize).div_ceil(PAGE_Y);
            if self.x_pages != x_pages || self.z_pages != z_pages || self.y_pages != y_pages {
                self.pages.clear();
                self.pages.resize_with(x_pages * z_pages * y_pages, || None);
                self.x_pages = x_pages;
                self.z_pages = z_pages;
                self.y_pages = y_pages;
            }
            self.min_x = min_x;
            self.max_x = max_x;
            self.min_y = min_y;
            self.max_y = min_y + height;
            self.min_z = min_z;
            self.max_z = max_z;
            let next_generation = self.generation.wrapping_add(1);
            if next_generation == 0 {
                for page in self.pages.iter_mut().flatten() {
                    for cell in page.cells.iter_mut() {
                        cell.generation = 0;
                    }
                }
                self.generation = 1;
            } else {
                self.generation = next_generation;
            }
            self.keys.clear();
        }

        fn pack(&self, &(lx, y, lz): &Key) -> Option<u32> {
            if !(self.min_x..self.max_x).contains(&lx)
                || !(self.min_y..self.max_y).contains(&y)
                || !(self.min_z..self.max_z).contains(&lz)
            {
                return None;
            }
            Some(self.pack_unchecked(&(lx, y, lz)))
        }

        #[inline]
        fn pack_unchecked(&self, &(lx, y, lz): &Key) -> u32 {
            let x = (lx - self.min_x) as u32;
            let y = (y - self.min_y) as u32;
            let z = (lz - self.min_z) as u32;
            x | (z << XZ_BITS) | (y << (XZ_BITS * 2))
        }

        fn unpack(&self, packed: u32) -> Key {
            let x = packed & XZ_MASK;
            let z = (packed >> XZ_BITS) & XZ_MASK;
            let y = (packed >> (XZ_BITS * 2)) & Y_MASK;
            (self.min_x + x as i32, self.min_y + y as i32, self.min_z + z as i32)
        }

        fn location(&self, packed: u32) -> (usize, usize) {
            let x = (packed & XZ_MASK) as usize;
            let z = ((packed >> XZ_BITS) & XZ_MASK) as usize;
            let y = ((packed >> (XZ_BITS * 2)) & Y_MASK) as usize;
            let page = ((x >> PAGE_X_SHIFT) * self.z_pages + (z >> PAGE_Z_SHIFT))
                * self.y_pages
                + (y >> PAGE_Y_SHIFT);
            let cell = (y & PAGE_Y_MASK) * PAGE_X * PAGE_Z
                + (z & PAGE_Z_MASK) * PAGE_X
                + (x & PAGE_X_MASK);
            (page, cell)
        }

        fn get_packed(&self, packed: u32) -> Option<StateId> {
            let (page, cell) = self.location(packed);
            let page = self.pages.get(page)?.as_ref()?;
            let cell = &page.cells[cell];
            (cell.generation == self.generation).then_some(cell.state)
        }

        #[cfg(test)]
        fn raw_get_packed(&self, packed: u32) -> Option<StateId> {
            let (page, cell) = self.location(packed);
            let page = self.pages.get(page)?.as_ref()?;
            Some(page.cells[cell].state)
        }

        #[inline]
        fn insert_in_bounds(&mut self, key: Key, state: StateId) {
            debug_assert!(
                self.pack(&key).is_some(),
                "overlay key is outside its configured bounds"
            );
            self.insert_packed(self.pack_unchecked(&key), state);
        }

        fn insert_packed(&mut self, packed: u32, state: StateId) {
            let (page_index, cell_index) = self.location(packed);
            let page = self.pages[page_index].get_or_insert_with(|| Box::new(Page::new()));
            let cell = &mut page.cells[cell_index];
            let is_new = cell.generation != self.generation;
            cell.generation = self.generation;
            cell.state = state;
            if is_new {
                self.keys.push(packed);
            }
        }
    }

    /// The overlay owns one storage instance. It is deliberately not `Sync` or
    /// shared: a view's direct pages belong to the thread running that view.
    #[derive(Debug)]
    pub(crate) struct Overlay {
        storage: Option<Storage>,
    }

    impl Default for Overlay {
        fn default() -> Self {
            Self::with_bounds(
                DEFAULT_MIN_XZ,
                DEFAULT_MAX_XZ,
                DEFAULT_MIN_Y,
                DEFAULT_HEIGHT,
            )
        }
    }

    impl Overlay {
        pub(crate) fn with_bounds(min_x: i32, max_x: i32, min_y: i32, height: i32) -> Self {
            let recycled = OVERLAYS
                .try_with(|free| free.try_borrow_mut().ok().and_then(|mut f| f.pop()))
                .ok()
                .flatten();
            if recycled.is_none() {
                bump_miss();
            }
            let mut storage = recycled.unwrap_or_else(|| {
                Storage::new(min_x, max_x, min_y, height, min_x, max_x)
            });
            storage.reconfigure(min_x, max_x, min_y, height, min_x, max_x);
            Self { storage: Some(storage) }
        }

        fn storage(&self) -> &Storage {
            self.storage.as_ref().expect("overlay storage is only taken in Drop")
        }

        fn storage_mut(&mut self) -> &mut Storage {
            self.storage.as_mut().expect("overlay storage is only taken in Drop")
        }

        #[cfg(test)]
        pub(crate) fn get(&self, key: &Key) -> Option<StateId> {
            self.storage().pack(key).and_then(|packed| self.storage().get_packed(packed))
        }

        #[inline]
        pub(crate) fn get_in_bounds(&self, key: &Key) -> Option<StateId> {
            let storage = self.storage();
            debug_assert!(
                storage.pack(key).is_some(),
                "overlay key is outside its configured bounds"
            );
            storage.get_packed(storage.pack_unchecked(key))
        }

        #[inline]
        pub(crate) fn insert_in_bounds(&mut self, key: Key, state: StateId) {
            self.storage_mut().insert_in_bounds(key, state);
        }

        /// Number of **distinct** cells written. An overwrite updates its stamped
        /// cell in place and does not append another packed key.
        pub(crate) fn len(&self) -> usize {
            self.storage().keys.len()
        }

        /// Every current entry. Consumers impose a total order before exporting
        /// values; page allocation order is never observable.
        pub(crate) fn iter(&self) -> impl Iterator<Item = (Key, StateId)> + '_ {
            let storage = self.storage();
            storage.keys.iter().filter_map(move |&packed| {
                storage.get_packed(packed).map(|state| (storage.unpack(packed), state))
            })
        }

        #[cfg(test)]
        pub(crate) fn raw_get(&self, key: &Key) -> Option<StateId> {
            let storage = self.storage();
            storage.pack(key).and_then(|packed| storage.raw_get_packed(packed))
        }
    }

    impl Drop for Overlay {
        fn drop(&mut self) {
            let Some(mut storage) = self.storage.take() else {
                return;
            };
            // Keep pages and their stamped cells. `reconfigure` advances the
            // generation on the next take, so retaining them avoids a full clear
            // while stale values remain unreadable.
            storage.keys.clear();
            let _ = OVERLAYS.try_with(|free| {
                if let Ok(mut f) = free.try_borrow_mut() {
                    if f.len() < KEEP {
                        f.push(storage);
                    }
                }
            });
        }
    }

    /// A write-order log of local keys whose backing `Vec` is recycled through
    /// this thread's free-list. `VegGrid::dirty`'s medium.
    ///
    /// Write **order** is world-visible here (the unified FEATURES dispatcher folds back in
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

        pub(crate) fn iter_from(&self, cursor: usize) -> impl Iterator<Item = &Key> {
            self.entries().iter().skip(cursor)
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
        let overlays = OVERLAYS.with(|f| f.borrow().len());
        let logs = LOGS.with(|f| f.borrow().len());
        (overlays, logs)
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
        OVERLAYS.with(|f| f.borrow_mut().clear());
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

/// Chebyshev chunk radius of the **read** neighbourhood vegetal decoration routes
/// through, and therefore the half-width of [`wide_source_slot`]'s 5×5 table.
///
/// **Two, not one, and the reason is a measured defect rather than a margin.**
/// Every one of the nine sources the 3×3 driver decorates can write into the
/// centre, so each one's placement decisions have to be a function of that source
/// alone — otherwise the chunk on each side of a seam computes a *different*
/// version of the same tree and one half is never served. A source sitting at
/// offset `(-1, 0)` reads up to [`super::VEG_PADDING`] blocks past its own west
/// edge, which is chunk offset `-2` from the centre. With a radius-1 read table
/// those columns have no slot and answer **air**, and where that boundary falls
/// depends on which column is the centre — so the same source's pass diverges.
/// Measured on a flat 5×5 fixture over all 66 bundled biomes: **94 seam rows
/// carried a canopy that one drive placed across the border and the served world
/// lost a half of; widening this read radius to 2 removed 50 of them** (see
/// `crates/lodestone-worldgen/tests/vegetation_seam_consistency.rs`).
///
/// It is a **read** radius only. Decoration still runs for the inner 3×3, writes
/// are still bounded by `VegGrid`'s own footprint, and only the centre is folded
/// back — so nothing about which chunk owns which write changed.
pub const WIDE_RADIUS: i32 = 2;

/// Which of the 25 chunks of `centre ± `[`WIDE_RADIUS`] owns centre-relative
/// local column `(lx, lz)`, or `None` outside that.
///
/// The wide twin of [`source_slot`], kept in this file **beside** it rather than
/// next to its caller, because this module's own doc makes "the 3×3 routing is
/// written in exactly one place" a rule and a second routing convention living
/// somewhere else is how the two drift apart. `div_euclid` for the same reason
/// [`source_slot`] uses it: local coordinates are negative across the west and
/// north thirds and truncating division folds `-1` onto offset `0`.
///
/// Ore's writer set remains 3×3, while its boundary-probe reads may opt into
/// this same 5×5 source table. Its heightmap uses the matching read bounds, so
/// terrain and height queries widen together rather than disagreeing at the
/// outer source edge.
#[must_use]
#[inline]
pub fn wide_source_slot(lx: i32, lz: i32) -> Option<usize> {
    const MIN: i32 = -WIDE_RADIUS * 16;
    const MAX: i32 = (WIDE_RADIUS + 1) * 16;
    if !(MIN..MAX).contains(&lx) || !(MIN..MAX).contains(&lz) {
        return None;
    }
    // The read window is chunk-aligned. Once the bounds check proves that the
    // coordinates are in it, shifts replace the two floor divisions without
    // changing the negative-coordinate convention (`MIN` is -32). Keep this
    // helper pure so every source-table filler and reader shares the same map.
    let x = ((lx - MIN) >> 4) as usize;
    let z = ((lz - MIN) >> 4) as usize;
    Some(x * (WIDE_RADIUS as usize * 2 + 1) + z)
}

/// The slot index for chunk offset `(dx, dz)` ∈ `[-`[`WIDE_RADIUS`]`, `[`WIDE_RADIUS`]`]²`.
///
/// Private for the same reason [`slot_of_offset`] is: the only filler
/// ([`crate::feature::vegetation::VegGrid::with_sources`]) derives the index by
/// asking [`wide_source_slot`] about that offset's own origin column, so fill and
/// lookup cannot disagree.
pub(crate) fn wide_slot_of_offset(dx: i32, dz: i32) -> usize {
    let side = (WIDE_RADIUS * 2 + 1) as usize;
    (dx + WIDE_RADIUS) as usize * side + (dz + WIDE_RADIUS) as usize
}

/// How many slots [`wide_source_slot`] can return — `(2 · WIDE_RADIUS + 1)²`.
pub(crate) const WIDE_SLOTS: usize = ((WIDE_RADIUS * 2 + 1) * (WIDE_RADIUS * 2 + 1)) as usize;

/// How many recycled buffers of each shape (overlay pages, write log) this thread
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
    /// The nine write-neighbourhood sources, indexed by [`slot_of_offset`]. `None` means "this
    /// offset was not supplied", which reads as air — the single-source debug
    /// paths in [`crate::overworld`] use exactly that.
    sources: [Option<&'a DenseBlockGrid>; 9],
    /// Optional five-by-five read context. When present, reads use it while
    /// writes remain bounded by [`Self::in_region`]'s 3×3 footprint.
    wide_sources: Option<[Option<&'a DenseBlockGrid>; WIDE_SLOTS]>,
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
    /// [`Overlay`]: a bounded direct-address page directory whose local
    /// coordinates are packed into a `u32`; each cell carries a generation
    /// stamp, and the view's pages are recycled through [`scratch`]'s per-thread
    /// free-list. Pages are allocated lazily, so the sparse write set does not
    /// materialise the full region. [`Self::centre_writes_in_scan_order`] sorts
    /// by the full key before palette fold-back; page or insertion order is never
    /// allowed to reach served bytes.
    overlay: Overlay,
    /// Reused ordering buffer for the mixed ore/vegetation bridge. The bridge
    /// consumes the ordered values before the next call, so retaining this
    /// capacity avoids rebuilding a large tuple vector for every ore entry.
    scan_order: Vec<(i32, i32, i32, StateId)>,
    /// Ordered `set_id` events. Mixed replay consumes this as a delta after
    /// each entry; seeded read context is intentionally not recorded.
    write_log: WriteLog,
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
            wide_sources: None,
            origin_x: centre_cx * 16,
            origin_z: centre_cz * 16,
            min_y,
            height,
            overlay: Overlay::with_bounds(REGION_MIN, REGION_MAX, min_y, height),
            scan_order: Vec::new(),
            write_log: WriteLog::default(),
            interner,
        }
    }

    /// A view whose reads route through `centre ± 2`, while writes remain in
    /// the inner 3×3 region. Ore uses this for probes made by sources at the
    /// edge of its writer neighbourhood.
    #[must_use]
    pub fn over_wide_sources(
        interner: Arc<StateInterner>,
        centre_cx: i32,
        centre_cz: i32,
        min_y: i32,
        height: i32,
        source_at: impl Fn(i32, i32) -> Option<&'a DenseBlockGrid>,
    ) -> Self {
        let mut wide_sources: [Option<&'a DenseBlockGrid>; WIDE_SLOTS] = [None; WIDE_SLOTS];
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                let slot = wide_source_slot(dx * 16, dz * 16)
                    .expect("a 5x5 offset's own origin column is inside the read context");
                debug_assert_eq!(slot, wide_slot_of_offset(dx, dz));
                wide_sources[slot] = source_at(dx, dz);
            }
        }
        Self {
            sources: [None; 9],
            wide_sources: Some(wide_sources),
            origin_x: centre_cx * 16,
            origin_z: centre_cz * 16,
            min_y,
            height,
            overlay: Overlay::with_bounds(
                super::ORE_READ_MIN,
                super::ORE_READ_MAX,
                min_y,
                height,
            ),
            scan_order: Vec::new(),
            write_log: WriteLog::default(),
            interner,
        }
    }

    /// A view over **one** grid that is already addressed in centre-relative
    /// region-local coordinates — the shape a parity fixture builds, since a
    /// fixture is naturally one sparse local grid over the whole region rather
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
            wide_sources: None,
            origin_x: 0,
            origin_z: 0,
            min_y,
            height,
            overlay: Overlay::with_bounds(REGION_MIN, REGION_MAX, min_y, height),
            scan_order: Vec::new(),
            write_log: WriteLog::default(),
            interner: Arc::clone(grid.interner()),
        }
    }

    /// This view's interner, for a caller that needs to mint or resolve ids
    /// against it.
    #[must_use]
    pub fn interner(&self) -> &Arc<StateInterner> {
        &self.interner
    }

    /// Whether a local coordinate can be read from this view. A wide source
    /// table extends only this boundary; writes remain in the driven 3×3 box.
    fn in_read_region(&self, lx: i32, y: i32, lz: i32) -> bool {
        let (min, max) = if self.wide_sources.is_some() {
            (super::ORE_READ_MIN, super::ORE_READ_MAX)
        } else {
            (REGION_MIN, REGION_MAX)
        };
        (min..max).contains(&lx)
            && (min..max).contains(&lz)
            && y >= self.min_y
            && y < self.min_y + self.height
    }

    /// Whether local `(lx, y, lz)` is inside the 3×3 writer region. Outside it,
    /// writes are dropped even when a wide read context can supply terrain.
    fn in_region(&self, lx: i32, y: i32, lz: i32) -> bool {
        (REGION_MIN..REGION_MAX).contains(&lx)
            && (REGION_MIN..REGION_MAX).contains(&lz)
            && y >= self.min_y
            && y < self.min_y + self.height
    }

    fn source_at(&self, lx: i32, lz: i32) -> Option<&'a DenseBlockGrid> {
        match &self.wide_sources {
            Some(sources) => wide_source_slot(lx, lz).and_then(|slot| sources[slot]),
            None => source_slot(lx, lz).and_then(|slot| self.sources[slot]),
        }
    }

    /// Interned state at local `(lx, y, lz)`: this pass's own write if there is
    /// one, else the owning source chunk's, else [`StateId::AIR`].
    #[must_use]
    pub fn get_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        if !self.in_read_region(lx, y, lz) {
            return StateId::AIR;
        }
        if let Some(id) = self.overlay.get_in_bounds(&(lx, y, lz)) {
            // Counted here as well as in [`Self::get`] so `ore_probe`'s
            // `region_reads_overlay` stays one number across §12.149's change of read
            // path: `try_place_ore` used to reach the overlay through `get` and now
            // reaches it through `get_id`, and the row is only a control if it counts
            // both. It must therefore stay equal across that change — the read
            // *pattern* did not move, only its cost.
            super::ore_probe::bump_region_read_overlay(1);
            return id;
        }
        match self.source_at(lx, lz) {
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
        if !self.in_read_region(lx, y, lz) {
            return "minecraft:air";
        }
        if let Some(id) = self.overlay.get_in_bounds(&(lx, y, lz)) {
            super::ore_probe::bump_region_read_overlay(1);
            return self.interner.name_of(id);
        }
        match self.source_at(lx, lz) {
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
        self.overlay.insert_in_bounds((lx, y, lz), state);
        self.write_log.push((lx, y, lz));
        true
    }

    /// Seeds a state into this pass's read overlay without making it a new
    /// decoration write.  Replay materializers use this for writes completed
    /// by an earlier source: an ore source may probe the wider read context
    /// even when that cell lies outside the current 3x3 writer region.
    ///
    /// This is deliberately separate from [`Self::set_id`].  Production
    /// feature placement must retain the 3x3 write bound, while a stateful
    /// caller may need to provide an already-resident value in the read-only
    /// rim.  The seeded value participates in normal overlay-first reads and
    /// is present in the raw [`Self::writes_in_scan_order`] view; callers that
    /// report newly produced writes must track and exclude unchanged seeds.
    pub fn seed_read_id(&mut self, lx: i32, y: i32, lz: i32, state: StateId) -> bool {
        if !self.in_read_region(lx, y, lz) {
            return false;
        }
        self.overlay.insert_in_bounds((lx, y, lz), state);
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

    /// Number of write events, including repeated overwrites of one cell.
    #[must_use]
    pub fn write_log_len(&self) -> usize {
        self.write_log.len()
    }

    /// Visits only the final values for cells touched since `cursor`, in the
    /// same `(x, z, y)` order as a complete overlay scan. Duplicate log events
    /// are retained and are filtered by the transfer map, which avoids a
    /// second per-entry deduplication map while preserving last-write wins.
    pub fn with_write_log_since_scan_order<R>(
        &mut self,
        cursor: usize,
        f: impl FnOnce(&[(i32, i32, i32, StateId)]) -> R,
    ) -> R {
        self.scan_order.clear();
        let overlay = &self.overlay;
        self.scan_order.extend(self.write_log.iter_from(cursor).filter_map(
            |&(lx, y, lz)| overlay.get_in_bounds(&(lx, y, lz)).map(|id| (lx, y, lz, id)),
        ));
        self.scan_order
            .sort_unstable_by_key(|&(lx, y, lz, _)| (lx, lz, y));
        f(&self.scan_order)
    }

    /// Every write held by this view's overlay, in deterministic `(x, z, y)`
    /// order.
    ///
    /// Unlike [`Self::centre_writes_in_scan_order`], this includes writes into
    /// every driven source column. Consumers that transfer a complete
    /// decoration result into another medium need the final overlay value at
    /// each coordinate, not the caller-specific centre subset.
    #[must_use]
    pub fn writes_in_scan_order(&self) -> Vec<(i32, i32, i32, StateId)> {
        let mut out: Vec<(i32, i32, i32, StateId)> = self
            .overlay
            .iter()
            .map(|((lx, y, lz), id)| (lx, y, lz, id))
            .collect();
        out.sort_unstable_by_key(|&(lx, y, lz, _)| (lx, lz, y));
        out
    }

    /// Visits every overlay write in deterministic `(x, z, y)` order using a
    /// buffer retained by this view. Mixed ore/vegetation dispatch consumes the
    /// values before the next call, so retaining capacity removes repeated
    /// large temporary vectors without changing the ordering contract.
    pub fn with_writes_in_scan_order<R>(
        &mut self,
        f: impl FnOnce(&[(i32, i32, i32, StateId)]) -> R,
    ) -> R {
        self.scan_order.clear();
        self.scan_order.extend(
            self.overlay
                .iter()
                .map(|((lx, y, lz), id)| (lx, y, lz, id)),
        );
        self.scan_order
            .sort_unstable_by_key(|&(lx, y, lz, _)| (lx, lz, y));
        f(&self.scan_order)
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
    /// Sorting is by the full key over the overlay's unique packed keys, so the
    /// order is total and does not depend on page allocation or recycling order.
    #[must_use]
    pub fn centre_writes_in_scan_order(&self) -> Vec<(i32, i32, i32, StateId)> {
        let mut out: Vec<(i32, i32, i32, StateId)> = self
            .overlay
            .iter()
            .filter(|((lx, _, lz), _)| (0..16).contains(lx) && (0..16).contains(lz))
            .map(|((lx, y, lz), id)| (lx, y, lz, id))
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

    /// The 5×5 read routing, against an independently written band table over the
    /// whole range vegetal decoration can address — its padded footprint plus a
    /// ring on both sides, so the `None` arm is exercised too.
    ///
    /// The expectation is a hand-written five-band walk rather than `div_euclid`
    /// again, so this is not `f(x) == f(x)`. The two halves of the convention are
    /// checked against each other as well: every offset's own origin column must
    /// route to that offset's slot.
    #[test]
    fn wide_source_slot_routes_every_local_column_in_the_five_by_five() {
        let pad = super::super::VEG_PADDING;
        let lo = REGION_MIN - pad - 16;
        let hi = REGION_MAX + pad + 16;
        let band = |v: i32| -> Option<i32> {
            match v {
                -32..=-17 => Some(-2),
                -16..=-1 => Some(-1),
                0..=15 => Some(0),
                16..=31 => Some(1),
                32..=47 => Some(2),
                _ => None,
            }
        };
        let mut inside = 0u32;
        for lx in lo..hi {
            for lz in lo..hi {
                let expected = match (band(lx), band(lz)) {
                    (Some(dx), Some(dz)) => {
                        inside += 1;
                        Some((dx + 2) as usize * 5 + (dz + 2) as usize)
                    }
                    _ => None,
                };
                assert_eq!(
                    wide_source_slot(lx, lz),
                    expected,
                    "local column ({lx}, {lz}) routed to the wrong source chunk",
                );
            }
        }
        // Non-vacuity: the walk really covered the whole 80×80 box, and 48×48 of
        // it really was inside the 5×5 rather than every column answering `None`.
        assert_eq!(inside, 80 * 80, "the 5x5 must cover 80x80 local columns");
        assert_eq!(WIDE_SLOTS, 25);
        // Fill convention and lookup convention agree, for all 25 offsets.
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                assert_eq!(
                    wide_source_slot(dx * 16, dz * 16),
                    Some(wide_slot_of_offset(dx, dz)),
                    "offset ({dx}, {dz})'s own origin column must route to its own slot",
                );
            }
        }
        // Every slot is claimed exactly once — a table that collided would send
        // two chunks to one grid and read like a plausible world.
        let claimed: std::collections::HashSet<usize> = (-WIDE_RADIUS..=WIDE_RADIUS)
            .flat_map(|dx| (-WIDE_RADIUS..=WIDE_RADIUS).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| wide_slot_of_offset(dx, dz))
            .collect();
        assert_eq!(claimed.len(), WIDE_SLOTS, "the slot table collides");
    }

    /// The negative control for the test above: the 3×3 table really does answer
    /// `None` where the 5×5 answers a rim chunk, so a caller that kept using
    /// [`source_slot`] would read **air** over exactly the columns this widening
    /// exists to supply. Without this, the exhaustive test could pass for reasons
    /// unrelated to the radius actually having widened.
    #[test]
    fn the_narrow_table_answers_air_where_the_wide_one_answers_a_rim_chunk() {
        let pad = super::super::VEG_PADDING;
        // The rim columns a source at offset (-1, 0) reads past its own west edge.
        let mut rim = 0u32;
        for lx in (REGION_MIN - pad)..REGION_MIN {
            assert_eq!(
                source_slot(lx, 0),
                None,
                "control: the 3x3 table must have no slot at local x={lx}, or the \
                 widening supplies nothing new",
            );
            assert!(
                wide_source_slot(lx, 0).is_some(),
                "the 5x5 table must own local x={lx}",
            );
            rim += 1;
        }
        assert_eq!(rim, pad as u32, "the control must have walked the whole pad ring");
        // …and the two tables agree everywhere the narrow one has an answer.
        for lx in REGION_MIN..REGION_MAX {
            for lz in REGION_MIN..REGION_MAX {
                assert!(
                    source_slot(lx, lz).is_some() && wide_source_slot(lx, lz).is_some(),
                    "both tables must own the 3x3 at ({lx}, {lz})",
                );
            }
        }
    }

    /// A recycled buffer must be indistinguishable from a fresh one. This is the
    /// whole correctness surface of [`scratch`]: a returned buffer is cleared on
    /// the way *out*, so a stale entry can never be read by whoever takes it next.
    ///
    /// The control half matters as much as the claim. Without the second
    /// assertion — that the buffer really was the recycled one — this test passes
    /// identically against a free-list that never hands anything back, which is
    /// the *premise-false* shape: it would be testing a fresh page directory.
    #[test]
    fn a_recycled_overlay_is_empty_and_is_really_the_recycled_one() {
        scratch::drain_free_lists();
        let bounds = (-4, 8, 0, 64);
        let stale = StateId::from_raw(7);
        {
            let mut first = Overlay::with_bounds(bounds.0, bounds.1, bounds.2, bounds.3);
            for y in 0..64 {
                first.insert_in_bounds((1, y, 2), stale);
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
        let second = Overlay::with_bounds(bounds.0, bounds.1, bounds.2, bounds.3);
        assert_eq!(second.len(), 0, "a recycled overlay must carry no stale keys");
        assert_eq!(
            second.raw_get(&(1, 7, 2)),
            Some(stale),
            "control: the recycled page must still physically contain the old value, or the \
             epoch test below could pass because the page was cleared rather than stamped",
        );
        assert_eq!(
            second.get(&(1, 7, 2)),
            None,
            "a key written through the previous holder must be hidden by the new generation",
        );
        assert_eq!(scratch::free_list_lengths().0, 0, "the take must consume it");
    }

    /// A new view may use a different local origin with the same page shape.
    /// The packed offset can therefore alias a physical slot from the previous
    /// view; only the generation stamp must distinguish the two coordinates.
    #[test]
    fn a_reused_page_does_not_alias_when_bounds_rebase_the_packed_key() {
        scratch::drain_free_lists();
        let stale = StateId::from_raw(19);
        {
            let mut first = Overlay::with_bounds(-4, 8, 0, 16);
            first.insert_in_bounds((-3, 0, -3), stale);
        }
        assert_eq!(scratch::free_list_lengths().0, 1);

        let second = Overlay::with_bounds(0, 12, 0, 16);
        // (-3, 0, -3) from the old view and (1, 0, 1) in this view have the same
        // packed offset. The raw control proves that the page was reused rather
        // than newly allocated, while the stamped lookup must still miss.
        assert_eq!(second.raw_get(&(1, 0, 1)), Some(stale));
        assert_eq!(second.get(&(1, 0, 1)), None);
    }

    /// A nested or re-entrant view must not borrow the outer view's storage.
    /// With the free-list empty while `outer` is live, the inner view allocates
    /// its own storage; both values remain visible to their original owners.
    #[test]
    fn nested_views_keep_independent_overlay_storage() {
        scratch::drain_free_lists();
        let outer_key = (-3, 0, -3);
        let inner_key = (1, 0, 1);
        let outer_state = StateId::from_raw(23);
        let inner_state = StateId::from_raw(29);
        let mut outer = Overlay::with_bounds(-4, 8, 0, 16);
        outer.insert_in_bounds(outer_key, outer_state);
        {
            let mut inner = Overlay::with_bounds(-4, 8, 0, 16);
            inner.insert_in_bounds(inner_key, inner_state);
            assert_eq!(inner.get(&inner_key), Some(inner_state));
            assert_eq!(outer.get(&outer_key), Some(outer_state));
            assert_eq!(outer.get(&inner_key), None);
        }
        assert_eq!(outer.get(&outer_key), Some(outer_state));
        assert_eq!(outer.get(&inner_key), None);
    }

    /// Recycling is thread-local: a worker must not consume a page directory
    /// returned by another worker, even when both use the same bounds.
    #[test]
    fn overlay_recycling_stays_on_the_current_thread() {
        scratch::drain_free_lists();
        {
            let mut owner = Overlay::with_bounds(-4, 8, 0, 16);
            owner.insert_in_bounds((-3, 0, -3), StateId::from_raw(31));
        }
        assert_eq!(scratch::free_list_lengths().0, 1);

        std::thread::spawn(|| {
            scratch::drain_free_lists();
            assert_eq!(scratch::free_list_lengths().0, 0);
            {
                let mut worker = Overlay::with_bounds(-4, 8, 0, 16);
                worker.insert_in_bounds((1, 0, 1), StateId::from_raw(37));
            }
            assert_eq!(scratch::free_list_lengths().0, 1);
        })
        .join()
        .expect("the thread-local recycling control must complete");

        assert_eq!(scratch::free_list_lengths().0, 1);
    }

    /// The direct overlay and the former fast map must have the same final
    /// content, independent of page allocation order. The digest is over the
    /// full coordinate key and state id after sorting, so a coordinate truncation
    /// or a stale-page hit cannot hide behind equal lengths.
    #[test]
    fn direct_overlay_matches_fast_map_content_digest() {
        use lodestone_worldgen_core::hash::FastMap;
        use sha2::{Digest, Sha256};

        fn digest(entries: &[(i32, i32, i32, StateId)]) -> [u8; 32] {
            let mut hash = Sha256::new();
            for &(lx, y, lz, id) in entries {
                hash.update(lx.to_le_bytes());
                hash.update(y.to_le_bytes());
                hash.update(lz.to_le_bytes());
                hash.update(id.raw().to_le_bytes());
            }
            hash.finalize().into()
        }

        // Computed independently from the deterministic key/state stream and
        // checked in as a content oracle, rather than deriving the expected
        // digest from the implementation under test.
        const EXPECTED_DIGEST: [u8; 32] = [
            0xfa, 0x51, 0x5e, 0x76, 0x8d, 0xd0, 0xa1, 0x51, 0x09, 0x89, 0x82, 0x22, 0x3e, 0x35,
            0x4c, 0xea, 0x34, 0x16, 0x4f, 0x48, 0xc0, 0xd2, 0x8c, 0x0d, 0x10, 0x47, 0x41, 0x85,
            0x5b, 0xa5, 0xd2, 0x8a,
        ];

        let mut overlay = Overlay::with_bounds(-16, 32, -64, 384);
        let mut reference: FastMap<(i32, i32, i32), StateId> = FastMap::default();
        for i in 0..1_200i32 {
            let key = ((i * 17).rem_euclid(48) - 16, (i * 13).rem_euclid(384) - 64, (i * 29).rem_euclid(48) - 16);
            let state = StateId::from_raw((i as u16 % 97) + 1);
            overlay.insert_in_bounds(key, state);
            reference.insert(key, state);
        }
        // Seeded cells use the same overlay but are intentionally not represented
        // in RegionView's write log; content parity still includes them.
        let seeded = (-15, 7, 31);
        let seeded_state = StateId::from_raw(333);
        overlay.insert_in_bounds(seeded, seeded_state);
        reference.insert(seeded, seeded_state);

        let mut got: Vec<_> = overlay
            .iter()
            .map(|((lx, y, lz), id)| (lx, y, lz, id))
            .collect();
        let mut expected: Vec<_> = reference
            .iter()
            .map(|(&(lx, y, lz), &id)| (lx, y, lz, id))
            .collect();
        got.sort_unstable_by_key(|&(lx, y, lz, _)| (lx, lz, y));
        expected.sort_unstable_by_key(|&(lx, y, lz, _)| (lx, lz, y));
        assert_eq!(got.len(), 385, "the parity stream must collapse repeated keys predictably");
        assert_eq!(got, expected, "direct pages changed final overlay content");
        assert_eq!(digest(&got), EXPECTED_DIGEST, "direct overlay content digest changed");
        assert_eq!(digest(&expected), EXPECTED_DIGEST, "FastMap baseline content digest changed");
    }

    /// A release-only comparison against the former `FastMap<Key, StateId>`
    /// container. Both arms execute the same warmed mixed read/write stream and
    /// return a checksum so the optimizer cannot discard the work. This is a
    /// diagnostic benchmark, not a direction-only gate: run it on a quiet host
    /// and record the printed medians when comparing machines or revisions.
    #[test]
    #[ignore = "performance measurement; run with --release --ignored --nocapture"]
    fn benchmark_direct_overlay_against_fast_map() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        use lodestone_worldgen_core::hash::FastMap;

        const OPS: usize = 32_768;
        const TRIALS: usize = 7;
        let keys: Vec<_> = (0..OPS)
            .map(|i| {
                let i = i as i32;
                (
                    (i * 17).rem_euclid(80) - 32,
                    (i * 13).rem_euclid(448) - 64,
                    (i * 29).rem_euclid(80) - 32,
                )
            })
            .collect();
        let states: Vec<_> = (0..OPS)
            .map(|i| StateId::from_raw((i as u16 % 251) + 1))
            .collect();

        fn direct_round(keys: &[(i32, i32, i32)], states: &[StateId]) -> u64 {
            let mut overlay = Overlay::with_bounds(-32, 48, -64, 448);
            let mut checksum = 0u64;
            for (i, (&key, &state)) in keys.iter().zip(states).enumerate() {
                if i % 4 == 0 {
                    overlay.insert_in_bounds(key, state);
                } else {
                    checksum = checksum.wrapping_add(
                        overlay.get_in_bounds(&key).map_or(0, |state| state.raw() as u64),
                    );
                }
            }
            black_box(checksum ^ overlay.len() as u64)
        }

        fn fast_map_round(keys: &[(i32, i32, i32)], states: &[StateId]) -> u64 {
            let mut map: FastMap<(i32, i32, i32), StateId> = FastMap::default();
            let mut checksum = 0u64;
            for (i, (&key, &state)) in keys.iter().zip(states).enumerate() {
                if i % 4 == 0 {
                    map.insert(key, state);
                } else {
                    checksum = checksum.wrapping_add(
                        map.get(&key).map_or(0, |state| state.raw() as u64),
                    );
                }
            }
            black_box(checksum ^ map.len() as u64)
        }

        let direct_warm = direct_round(&keys, &states);
        let fast_map_warm = fast_map_round(&keys, &states);
        assert_eq!(direct_warm, fast_map_warm, "benchmark arms must consume identical results");

        let mut direct_elapsed = Duration::ZERO;
        let mut fast_map_elapsed = Duration::ZERO;
        for _ in 0..TRIALS {
            let start = Instant::now();
            let direct = black_box(direct_round(&keys, &states));
            direct_elapsed += start.elapsed();
            let start = Instant::now();
            let fast_map = black_box(fast_map_round(&keys, &states));
            fast_map_elapsed += start.elapsed();
            assert_eq!(direct, fast_map, "benchmark arms diverged during a trial");
        }

        let direct_ns = direct_elapsed.as_nanos() / TRIALS as u128;
        let fast_map_ns = fast_map_elapsed.as_nanos() / TRIALS as u128;
        println!(
            "region_overlay benchmark: ops={OPS} trials={TRIALS} direct_ns_per_round={direct_ns} fast_map_ns_per_round={fast_map_ns} direct_ns_per_op={} fast_map_ns_per_op={}",
            direct_ns / OPS as u128,
            fast_map_ns / OPS as u128,
        );
    }

    /// The same contract for the write log, and additionally that write **order**
    /// starts at index 0 — the log's order reaches the served palette through
    /// the unified FEATURES fold-back, so a recycled log that retained entries
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
        let (overlays, _) = scratch::free_list_lengths();
        assert!(
            overlays <= 4,
            "32 overlays were dropped and the free-list kept {overlays} of them; a \
             thread-local cache with no bound is a leak wearing a cache's clothes",
        );
        assert!(overlays > 0, "control: it must keep at least one, or reuse never happens");
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

    #[test]
    fn wide_read_context_reaches_outer_sources_without_accepting_outer_writes() {
        let interner = Arc::new(StateInterner::new());
        let grids = (-WIDE_RADIUS..=WIDE_RADIUS)
            .flat_map(|dx| {
                (-WIDE_RADIUS..=WIDE_RADIUS).map(move |dz| {
                    let state = if dx == -WIDE_RADIUS && dz == 0 {
                        "minecraft:granite"
                    } else {
                        "minecraft:stone"
                    };
                    (dx, dz, state)
                })
            })
            .map(|(dx, dz, state)| chunk_grid(&interner, 10 + dx, -20 + dz, state))
            .collect::<Vec<_>>();
        let mut view = RegionView::over_wide_sources(
            Arc::clone(&interner),
            10,
            -20,
            0,
            8,
            |dx, dz| grids.get(wide_slot_of_offset(dx, dz)),
        );
        assert_eq!(view.get(-17, 4, 0), "minecraft:granite");
        assert!(!view.set(-17, 4, 0, "minecraft:diorite"));
        assert_eq!(view.get(-17, 4, 0), "minecraft:granite");
        assert!(view.set(0, 4, 0, "minecraft:diorite"));
        assert_eq!(view.get(0, 4, 0), "minecraft:diorite");
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

    #[test]
    fn all_writes_have_a_stable_x_then_z_then_y_order() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (0..9)
            .map(|_| chunk_grid(&interner, 0, 0, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 1) * 3 + (dz + 1)) as usize)
        });
        for &(lx, y, lz) in &[(3, 6, 1), (-1, 2, 3), (3, 2, 1), (1, 2, -3), (-1, 6, 3)] {
            view.set(lx, y, lz, "minecraft:diamond_ore");
        }
        let keys: Vec<(i32, i32, i32)> = view
            .writes_in_scan_order()
            .into_iter()
            .map(|(lx, y, lz, _)| (lx, lz, y))
            .collect();
        assert_eq!(
            keys,
            vec![(-1, 3, 2), (-1, 3, 6), (1, -3, 2), (3, 1, 2), (3, 1, 6)],
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

    /// A lifecycle replay may supply a previously completed state in the
    /// wider ore read context without turning that state into a new 3x3 write.
    #[test]
    fn a_seeded_read_state_shadows_the_wide_source_rim() {
        let interner = Arc::new(StateInterner::new());
        let grids: Vec<DenseBlockGrid> = (-2..=2)
            .flat_map(|dx| (-2..=2).map(move |dz| (dx, dz)))
            .map(|(dx, dz)| chunk_grid(&interner, dx, dz, "minecraft:stone"))
            .collect();
        let mut view = RegionView::over_wide_sources(Arc::clone(&interner), 0, 0, 0, 8, |dx, dz| {
            grids.get(((dx + 2) * 5 + (dz + 2)) as usize)
        });
        // x=-17 is in the source chunk at -2, but outside the current 3x3
        // writer box [-16, 32). It is still in the ore read box [-32, 48).
        assert_eq!(view.get(-17, 4, 0), "minecraft:stone");
        let gold = interner.id_of("minecraft:gold_ore");
        assert!(view.seed_read_id(-17, 4, 0, gold));
        assert_eq!(view.write_log_len(), 0, "seeded read context must not enter the write log");
        assert_eq!(view.get(-17, 4, 0), "minecraft:gold_ore");
        assert!(!view.seed_read_id(crate::feature::ORE_READ_MIN - 1, 4, 0, gold));
        assert!(view.set_id(0, 4, 0, gold));
        assert_eq!(view.write_log_len(), 1, "set_id must append after a seeded read");
        view.with_write_log_since_scan_order(0, |writes| {
            assert_eq!(writes, &[(0, 4, 0, gold)]);
        });
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
