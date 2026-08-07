//! [`VegGrid`] — vegetal decoration's read/write surface over one column's 3×3
//! neighbourhood — and the [`census`] counters that make a silent no-op step visible.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use std::collections::HashMap;
use std::sync::Arc;

use crate::dense_grid::DenseBlockGrid;
use crate::feature::region_view::source_slot;
use crate::interner::{StateId, StateInterner};

use self::census::bump as census_bump;
use super::base_id;
use super::config::is_fluid;

/// The mutable block field vegetal decoration reads and writes. Defaults to
/// chunk-local (`0..16` × `0..16`, absolute `y`) via [`VegGrid::new`] — see
/// module doc's "Scope" section for why single-chunk was this module's
/// original footprint. [`VegGrid::with_footprint`] widens the local bound to
/// an arbitrary `[lo, hi)` on both axes (issue #427: the real vanilla 3×3
/// `blockStateWriteRadius(1)` driver uses [`crate::feature::REGION_MIN`]/
/// [`crate::feature::REGION_MAX`], the exact constants
/// [`crate::feature::OreInput::region_local`] already established for the
/// ore engine's own 3×3 driver — reused here rather than duplicated, per
/// CLAUDE.md's instruction to follow that precedent) with `origin_x`/
/// `origin_z` fixed at the **centre** chunk's own absolute origin, so every
/// one of the 9 sources' absolute-coordinate writes translates through the
/// same origin and lands (or is dropped) relative to the centre — exactly
/// [`crate::feature::OreInput`]'s `chunk_x`/`chunk_z` (varies per source) vs
/// `center_x`/`center_z` (fixed) split, applied to this module's own grid
/// type instead of introducing a second region-grid mechanism.
#[derive(Debug)]
pub struct VegGrid {
    /// Keyed by **local** `(0..16, y, 0..16)` — every public accessor takes
    /// **absolute world** coordinates (matching every `BlockPos` this
    /// engine's placement modifiers compute — noise sampling, the decoration
    /// seed, `RandomOffset`'s scatter, all of it is absolute-coordinate
    /// arithmetic, not local) and converts via `origin_x`/`origin_z`
    /// internally. Getting this translation wrong is exactly the bug this
    /// comment exists to prevent from recurring: an earlier version of this
    /// struct stored *and exposed* local coordinates, silently accepting the
    /// engine's absolute-coordinate `BlockPos`es and comparing them against
    /// a `0..16` bound that was almost always false — every placement
    /// attempt for any chunk other than `(0, 0)` failed `in_bounds`/`get`'s
    /// implicit "must already be local" assumption, so vegetation composed,
    /// ran, and reached zero blocks in every real served chunk. Caught by a
    /// sweep gate measuring **zero** grass/flowers/logs/leaves over a plains
    /// neighbourhood — this module's own hermetic unit tests never caught it
    /// because every one of them happened to place at `origin = BlockPos { x: 8,
    /// ... z: 8 }`, which is coincidentally already "local" (chunk (0,0)'s own
    /// footprint), the exact island CLAUDE.md's rule 1 describes: a unit
    /// test can be green while the real integration seam is broken.
    ///
    /// **That gate was then deleted, and this comment kept naming it** — it read
    /// `lodestone_server::worldgen_data::tests::diagnostic_vegetation_counts_over_plains_sweep`
    /// up to `074b5e9`, by which point no such test existed anywhere in the tree
    /// (issue #478). So for an unknown span the repo held a written record of a
    /// regression with nothing watching for its return, and the reference read as
    /// coverage on inspection. The live gate is now
    /// `lodestone_server::worldgen_data::tests::vegetation_reaches_real_blocks_over_a_production_sweep`,
    /// with `plains_grass_patch_attempt_count_matches_the_placement_json` carrying
    /// the predicted magnitude — but treat *this sentence* as a claim like any
    /// other and grep for the name before trusting it.
    /// Unit 3 (`docs/plans/worldgen-rewrite.md`) changed the value type from
    /// `String` to [`StateId`]. That single change is where **884,736 of the
    /// 905,459 heap allocations per warm column** went: the seeding loop
    /// (`OverworldGenerator::stitch_veg_region`) copies `48 × 384 × 48` cells
    /// out of the post-ore dense grids into this map, and with a `String` value
    /// every one of those copies allocated. With ids, seeding is a `u16` move.
    ///
    /// The vegetation *engine* around this store is still string-based (that is
    /// Unit 8); its `&str` accessors below are shims over the id path, so the
    /// per-placement cost is unchanged while the per-*cell* cost is gone.
    ///
    /// **Unit 7 then deleted the seeding loop itself, and this map with it became
    /// a sparse *overlay*.** Unit 3 made a seeded cell a `u16` move rather than a
    /// `String` allocation, but there were still 884,736 of them per column —
    /// nine already-computed post-ore chunks copied, cell by cell, into a
    /// `HashMap` that then held 884,736 live entries. Production now supplies the
    /// nine grids as [`VegGrid::sources`] and this map holds **only what
    /// decoration wrote** (a few thousand cells), with a miss falling through to
    /// the source chunk that owns the column. [`Self::seed_id`] still writes here,
    /// which is what keeps every parity fixture — naturally one hand-written
    /// sparse map with no source grids at all — working unchanged against the
    /// identical read path.
    blocks: HashMap<(i32, i32, i32), StateId>,
    /// The nine post-ore source chunks a read falls through to, indexed by
    /// [`crate::feature::region_view::source_slot`] over this grid's **local**
    /// coordinates. Empty for every fixture/unit-test constructor (a miss then
    /// answers air, exactly as an unseeded cell always did); populated by
    /// [`Self::with_sources`] in production.
    ///
    /// Read-only, and that is a rule rather than an accident: these are `Arc`
    /// snapshots shared with every other in-flight column that has the same
    /// neighbour, and the rewrite plan's parallel model requires each chunk's grid
    /// to have exactly one writer — its own serve task. Writes therefore go to
    /// `blocks` and the caller folds them into the one grid it owns.
    ///
    /// Note the footprint is **wider** than the sources cover: `local_lo` /
    /// `local_hi` span `REGION_MIN - VEG_PADDING .. REGION_MAX + VEG_PADDING`, so
    /// the padding ring has no source and reads air there — which is precisely
    /// what the unseeded ring did before, so tree canopy spilling into the pad is
    /// still writable and still readable back.
    sources: [Option<Arc<DenseBlockGrid>>; 9],
    /// Resolves this grid's [`StateId`]s. Shared with the generator's dense
    /// grids, which is what lets `stitch_veg_region` move ids across without a
    /// string round-trip — ids from a different interner are meaningless here
    /// (see [`StateId`]).
    interner: Arc<StateInterner>,
    /// Positions actually written by `set_if_in_bounds`, **local** (see
    /// `blocks`' doc), in write order — a `Vec`, not a re-iterated
    /// `HashMap`, specifically so a caller folding this back into a dense
    /// grid (`OverworldGenerator`'s vegetation stage) has a *deterministic*
    /// order to replay, the same discipline `docs/worldgen-parity.md`'s
    /// "Performance" section describes fixing for `surface_diff` (point
    /// lookups inside a fixed loop, never a raw `HashMap` iteration) — here
    /// achieved even more directly, since insertion order into a `Vec`
    /// carries no ambiguity to begin with. Lets the fold-back touch only the
    /// (typically small) written subset instead of rewriting all
    /// `16 × height × 16` cells.
    dirty: Vec<(i32, i32, i32)>,
    origin_x: i32,
    origin_z: i32,
pub(super)     min_y: i32,
pub(super)     height: i32,
    /// Local-coordinate bound `[local_lo, local_hi)` on both `lx` and `lz` —
    /// `(0, 16)` for the single-chunk case ([`VegGrid::new`]), widened to
    /// [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`] for the
    /// 3×3 driver ([`VegGrid::with_footprint`],
    /// [`apply_vegetal_decoration_step_3x3_per_source`]).
    local_lo: i32,
    local_hi: i32,
    /// The ids of `minecraft:{air,cave_air,void_air}` in [`Self::interner`],
    /// resolved once here so [`Self::height_world_surface`]'s per-cell air test is
    /// three integer compares rather than an interner read guard. See
    /// [`Self::is_air_id`] for why an id comparison is exact for air. Unit 8.
    air_ids: [StateId; 3],
}

impl VegGrid {
    /// `origin_x`/`origin_z` are the chunk's own **absolute** block origin
    /// (`chunk_x * 16`, `chunk_z * 16`) — every other method on this type
    /// takes absolute world coordinates and translates through these.
    /// Single-chunk footprint (`0..16` on both axes) — see
    /// [`VegGrid::with_footprint`] for the 3×3 driver's widened case.
    #[must_use]
    pub fn new(min_y: i32, height: i32, origin_x: i32, origin_z: i32) -> Self {
        Self::with_footprint(min_y, height, origin_x, origin_z, 0, 16)
    }

    /// Like [`VegGrid::new`], but with an explicit local-coordinate bound
    /// `[local_lo, local_hi)` on both `lx` and `lz` instead of the hardcoded
    /// `0..16` — the real vanilla 3×3 `blockStateWriteRadius(1)` driver
    /// passes [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`]
    /// here with `origin_x`/`origin_z` fixed at the **centre** chunk's own
    /// origin (see this struct's own doc comment).
    #[must_use]
    pub fn with_footprint(min_y: i32, height: i32, origin_x: i32, origin_z: i32, local_lo: i32, local_hi: i32) -> Self {
        Self::with_footprint_interned(
            Arc::new(StateInterner::new()),
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
        )
    }

    /// [`VegGrid::with_footprint`] against a **shared** interner — the form
    /// production must use, so ids seeded from the generator's dense grids mean
    /// the same thing here. The string-taking constructors above build a private
    /// interner, which is correct for a self-contained unit test or parity
    /// fixture and wrong for anything that exchanges ids with another grid.
    #[must_use]
    pub fn with_footprint_interned(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
    ) -> Self {
        let air_ids = [
            interner.id_of("minecraft:air"),
            interner.id_of("minecraft:cave_air"),
            interner.id_of("minecraft:void_air"),
        ];
        Self {
            blocks: HashMap::new(),
            sources: std::array::from_fn(|_| None),
            interner,
            dirty: Vec::new(),
            origin_x,
            origin_z,
            min_y,
            height,
            local_lo,
            local_hi,
            air_ids,
        }
    }

    /// [`VegGrid::with_footprint_interned`] over the 3×3 neighbourhood's **own**
    /// post-ore grids instead of a seeded copy of them — the production form
    /// since Unit 7 of `docs/plans/worldgen-rewrite.md`.
    ///
    /// `source_at(dx, dz)` is called once per chunk offset in `[-1, 1]²` and
    /// returns that chunk's post-ore world (absolute-coordinate addressed), or
    /// `None` to make that chunk read as air — which is exactly what the
    /// `LODESTONE_VEG_SINGLE_SOURCE_DEBUG` path wants for the eight neighbours.
    ///
    /// The slots are filled through [`crate::feature::region_view::source_slot`],
    /// the same function every read routes with, so the fill convention and the
    /// lookup convention cannot drift apart. Getting that wrong is the recorded
    /// `VegGrid` failure mode — see this type's own doc comment.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
    ) -> Self {
        let mut grid = Self::with_footprint_interned(
            interner, min_y, height, origin_x, origin_z, local_lo, local_hi,
        );
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                let slot = source_slot(dx * 16, dz * 16)
                    .expect("a 3x3 offset's own origin column is inside the region");
                grid.sources[slot] = source_at(dx, dz);
            }
        }
        grid
    }

    /// The state one of the nine source chunks holds at **local** `(lx, y, lz)`,
    /// or [`StateId::AIR`] when no source owns that column (the padding ring, a
    /// fixture with no sources, or a `None` slot).
    fn source_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        match source_slot(lx, lz).and_then(|slot| self.sources[slot].as_deref()) {
            Some(grid) => grid.get_id(self.origin_x + lx, y, self.origin_z + lz),
            None => StateId::AIR,
        }
    }

    /// This grid's interner, for a caller that needs to resolve or mint ids
    /// against it.
    #[must_use]
    pub fn interner(&self) -> &Arc<StateInterner> {
        &self.interner
    }

    /// Positions written by `set_if_in_bounds` since construction, in write
    /// order, **as absolute world coordinates**, each paired with the state
    /// currently at that position (i.e. the *final* state if the same cell
    /// was written more than once, not an intermediate one) — what a caller
    /// should fold back into a wider grid, with no further translation
    /// needed.
    pub fn dirty_cells(&self) -> impl Iterator<Item = (i32, i32, i32, &str)> {
        self.dirty_cell_ids()
            .map(|(x, y, z, id)| (x, y, z, self.interner.name_of(id)))
    }

    /// [`Self::dirty_cells`] without resolving the states to strings — the
    /// allocation-free form, for a caller folding these writes back into
    /// another id-carrying grid.
    pub fn dirty_cell_ids(&self) -> impl Iterator<Item = (i32, i32, i32, StateId)> {
        self.dirty.iter().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks.get(&(lx, y, lz)).copied().unwrap_or(StateId::AIR),
            )
        })
    }

    /// The number of writes recorded so far — a caller (currently only
    /// [`place_tree`]) that brackets a `dirty_len()` call before and after a
    /// span of writes and then reads `dirty_cells().skip(before)` gets
    /// exactly the absolute-coordinate positions written in that span, in
    /// order. Used to compute one tree's own `trunks ∪ foliage ∪
    /// decorations` bounding box for [`update_leaf_distances`] — see that
    /// function's own doc comment for why the bound matters.
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    fn in_bounds_local(&self, lx: i32, lz: i32) -> bool {
        (self.local_lo..self.local_hi).contains(&lx) && (self.local_lo..self.local_hi).contains(&lz)
    }

    /// Absolute world `(x, z)` -> local `[local_lo, local_hi)`, **clamped**
    /// into range — used only by read paths, which must always answer
    /// something.
    fn to_local_clamped(&self, x: i32, z: i32) -> (i32, i32) {
        (
            (x - self.origin_x).clamp(self.local_lo, self.local_hi - 1),
            (z - self.origin_z).clamp(self.local_lo, self.local_hi - 1),
        )
    }

    /// Absolute world `(x, z)` -> local, **unclamped** — used only by the
    /// write path, which must know whether the position genuinely falls
    /// inside this chunk's own footprint rather than silently relocating a
    /// write to the nearest edge.
    fn to_local_exact(&self, x: i32, z: i32) -> (i32, i32) {
        (x - self.origin_x, z - self.origin_z)
    }

    /// Seeds one column position (absolute world coordinates) from the
    /// post-ore composed grid. Callers fill every `(x, y, z)` in this
    /// chunk's own `16 × height × 16` footprint before running vegetal
    /// decoration.
    /// String-taking shim over [`Self::seed_id`], for parity fixtures and unit
    /// tests. **Not** for the production seeding loop — that is the 884,736
    /// allocations (see the `blocks` field doc).
    pub fn seed(&mut self, x: i32, y: i32, z: i32, state: String) {
        let id = self.interner.id_of(&state);
        self.seed_id(x, y, z, id);
    }

    /// Seeds one column position (absolute world coordinates) from the post-ore
    /// composed grid, by interned id — the zero-allocation seeding path.
    pub fn seed_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let (lx, lz) = self.to_local_exact(x, z);
        self.blocks.insert((lx, y, lz), state);
    }

    /// This pass's own write if there is one, else the source chunk that owns the
    /// column, else air.
    ///
    /// Overlay-first is load-bearing, not an optimisation: vanilla's heightmaps
    /// update as decoration places blocks, so a tree placed earlier in the step
    /// must be visible to a later `height_world_surface` probe in the same step.
    /// A post-pass merge of the writes would answer stale and is parity-unsafe.
    fn get_local_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        if y < self.min_y || y >= self.min_y + self.height {
            return StateId::AIR;
        }
        match self.blocks.get(&(lx, y, lz)) {
            Some(&id) => id,
            None => self.source_id(lx, y, lz),
        }
    }

    fn get_local(&self, lx: i32, y: i32, lz: i32) -> &str {
        self.interner.name_of(self.get_local_id(lx, y, lz))
    }

    /// Reads always succeed (clamped into bounds) — a read past the local
    /// footprint approximates the nearest in-bounds column rather than
    /// panicking or returning a sentinel the caller has to special-case.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local(lx, y, lz)
    }

    /// [`Self::get`] without resolving to a string — the allocation-free and
    /// lock-free read path.
    #[must_use]
    pub fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local_id(lx, y, lz)
    }

    /// Writes past the local footprint (`0..16` for [`VegGrid::new`], wider
    /// for [`VegGrid::with_footprint`]) or outside the vertical build range
    /// are dropped, not clamped — see module doc's "Scope" section; a write
    /// past whatever footprint this grid covers would fabricate a block on
    /// the wrong column. Returns whether the write actually landed.
    pub fn set_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: String) -> bool {
        let id = self.interner.id_of(&state);
        self.set_id_if_in_bounds(x, y, z, id)
    }

    /// [`Self::set_if_in_bounds`] by interned id — the allocation-free write
    /// path. Identical bounds behaviour, including the census bumps, so which
    /// form a caller uses cannot change a placement outcome.
    pub fn set_id_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: StateId) -> bool {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            census_bump(|c| c.writes += 1);
            self.blocks.insert((lx, y, lz), state);
            self.dirty.push((lx, y, lz));
            true
        } else {
            census_bump(|c| c.writes_rejected += 1);
            false
        }
    }

    /// `Heightmap.Types.WORLD_SURFACE`/`WORLD_SURFACE_WG` — topmost non-air,
    /// scanned live against the current (possibly already-modified-this-step)
    /// grid. `x`/`z` are absolute world coordinates. Returns `min_y` (not
    /// `min_y - 1`) for an all-air column, matching vanilla's `y + 1`
    /// convention with `y` floored at one below the lowest placeable block.
    #[must_use]
    pub fn height_world_surface(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            if !self.is_air_id(self.get_local_id(lx, y, lz)) {
                return y + 1;
            }
        }
        self.min_y
    }

    /// Whether `id` is one of the three air states.
    ///
    /// # Why this can be an id comparison and the fluid test below cannot
    ///
    /// **Air carries no block-state properties**, so for an air state
    /// `base_id(name) == name` and "base is one of three names" is exactly "id is
    /// one of three ids" — the three resolved in [`Self::with_footprint_interned`].
    /// `crate::feature::vegetation::config::is_air` is still the definition; this
    /// is that definition pushed through the interner once per grid instead of
    /// once per cell. Adding a property-carrying state to `is_air` would silently
    /// break this, which is why that function's doc says not to.
    ///
    /// A fluid, by contrast, really does carry properties here —
    /// `crate::carver` writes `minecraft:water[level=0]` — so
    /// [`Self::height_ocean_floor`] cannot reduce its test to a fixed id set and
    /// resolves the name for the (few) cells it has already found to be non-air.
    fn is_air_id(&self, id: StateId) -> bool {
        self.air_ids.contains(&id)
    }

    /// `Heightmap.Types.OCEAN_FLOOR`/`OCEAN_FLOOR_WG` — topmost non-air,
    /// non-fluid. `x`/`z` are absolute world coordinates.
    #[must_use]
    pub fn height_ocean_floor(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let id = self.get_local_id(lx, y, lz);
            // The air test first and lock-free, so the ~250 cells of empty sky
            // above a surface column cost three integer compares each instead of
            // an interner read guard each. Only a non-air cell — the surface
            // itself and at most a few fluid cells above it — resolves a name.
            if self.is_air_id(id) {
                continue;
            }
            if !is_fluid(base_id(self.interner.name_of(id))) {
                return y + 1;
            }
        }
        self.min_y
    }
}

/// Runs the whole `VEGETAL_DECORATION` step for one chunk against its own
/// grid — single-source only, see module doc's "Scope" section.
/// `features` is `(raw step index, resolved PlacedRef)`, matching
/// [`super::compose::build_biome_ores`]'s "preserve raw position" convention
/// so `setFeatureSeed`'s index is the JSON array position, not a filtered
/// count.
/// Per-thread census of what the vegetal-decoration placer actually *did* —
/// issue #478's "make absence loud" half.
///
/// # Why this exists, and why the existing gate was not enough
///
/// This module's blanket rule is "an unmodelled feature/trunk/foliage/provider
/// kind degrades to a silent no-op, never a panic" (see the module doc). That
/// rule is right — a datapack naming a feature we don't implement must still
/// produce a world — but on its own it makes *every* quantity of vegetation,
/// including zero, look identical from the outside. Issue #478 was filed
/// against exactly that shape, and the previous instance of the same shape
/// (the absolute-vs-local `VegGrid` coordinate bug recorded in
/// [`VegGrid`]'s own doc comment) reached **zero** blocks in every served
/// chunk with the whole suite green.
///
/// [`collect_unsupported`] plus `lodestone_server::worldgen_data`'s
/// `KNOWN_VEGETATION_GAPS` already make absence loud at **resolve** time: they
/// answer "does this biome's declared step name a placer we don't implement?"
/// They structurally cannot answer "did the placer that *is* implemented reach
/// a block?", because they never run it. This census answers the second
/// question — the one that separates a fully-connected wire carrying real
/// blocks from a fully-connected wire carrying nothing.
///
/// # Thread-local, not global
///
/// `OverworldGenerator` is shared across threads by
/// `lodestone_server::chunk::generate_columns_parallel`, and `cargo test` runs
/// test binaries multi-threaded. A process-global counter would make any gate
/// built on it read another test's work, which is the *duration* species of
/// vacuous test (a counter accumulating past the gate's own lifetime). Each
/// thread sees only its own placements, so a gate resets, generates, and reads
/// back on one thread and measures exactly what it caused.
pub mod census {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Terminal-dispatch and write tallies for one thread's placements.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct VegCensus {
        /// [`super::ConfiguredFeature::SimpleBlock`] terminal dispatches — one
        /// per position that survived the whole placement pipeline.
        pub simple_block: usize,
        /// [`super::ConfiguredFeature::Tree`] terminal dispatches.
        pub tree: usize,
        /// [`super::ConfiguredFeature::BlockColumn`] terminal dispatches.
        pub block_column: usize,
        /// [`super::ConfiguredFeature::RandomSelector`] traversals (not
        /// terminals — each recurses into a branch).
        pub random_selector: usize,
        /// [`super::ConfiguredFeature::SimpleRandomSelector`] traversals.
        pub simple_random_selector: usize,
        /// Unmodelled terminal dispatches, **keyed by the reason string**
        /// [`super::ConfiguredFeature::Unsupported`] carries. This is the loud
        /// part: a new unimplemented feature type shows up here as a named,
        /// counted row instead of as a slightly emptier world.
        pub unsupported: BTreeMap<String, usize>,
        /// `SimpleBlock` dispatches dropped because the state provider
        /// produced nothing.
        pub simple_block_no_state: usize,
        /// `SimpleBlock` dispatches dropped because the block below is not in
        /// `#minecraft:supports_vegetation` (`VegetationBlock.canSurvive`).
        /// Legitimately the majority — `random_offset` scatters positions off
        /// the heightmap column — so this is a diagnostic, not a defect count.
        pub simple_block_unsupported_ground: usize,
        /// Positions handed to a [`super::VegPlacement::BlockPredicateFilter`].
        ///
        /// This is the **last exactly-predictable boundary** in a vanilla
        /// vegetal-decoration pipeline, and the reason it is counted separately
        /// from everything else here. Every 26.2 overworld vegetation
        /// `placed_feature` ends in at least one filter whose outcome depends on
        /// terrain (measured: of 262 bundled placed features, the only three
        /// with no filter at all are `end_spike`, `freeze_top_layer` and
        /// `void_start_platform`), so no *terminal* count can be predicted from
        /// the JSON alone. Everything upstream of the filter can:
        /// `count`/`noise_threshold_count` multiply by a JSON constant,
        /// `in_square`/`biome`/`random_offset` are each exactly
        /// position-preserving, and `heightmap` yields exactly one position for
        /// any column that is not entirely air. So for a single-source run of a
        /// single placed feature this number is a product of JSON constants —
        /// which is what lets a gate *predict* it instead of asserting a sign.
        /// See `lodestone_server::worldgen_data`'s
        /// `plains_grass_patch_attempt_count_matches_the_placement_json`.
        pub block_predicate_filter_in: usize,
        /// Positions that passed a [`super::VegPlacement::BlockPredicateFilter`].
        pub block_predicate_filter_out: usize,
        /// Grid writes that landed.
        pub writes: usize,
        /// Grid writes dropped as outside the grid's own footprint (spill into
        /// a chunk this grid does not cover — expected, see
        /// [`super::VegGrid::set_if_in_bounds`]).
        pub writes_rejected: usize,
    }

    impl VegCensus {
        /// Total unmodelled terminal dispatches across every reason.
        #[must_use]
        pub fn unsupported_total(&self) -> usize {
            self.unsupported.values().sum()
        }

        /// Terminal dispatches that reached a placer this engine implements.
        #[must_use]
        pub fn modelled_terminals(&self) -> usize {
            self.simple_block + self.tree + self.block_column
        }
    }

    thread_local! {
        static CENSUS: RefCell<VegCensus> = RefCell::new(VegCensus::default());
    }

    /// Whether an unmodelled terminal dispatch should panic instead of being
    /// counted — `LODESTONE_VEG_STRICT=1`. Read once per process.
    ///
    /// Off by default on purpose: the module's degrade-don't-crash rule is what
    /// lets a trimmed datapack generate at all, and 26.2's own vanilla data
    /// reaches unmodelled types in nearly every biome (`multiface_growth` alone
    /// is in 55 of them), so strict mode is a *debugging* switch for "which
    /// type am I missing here", not a mode anything ships in.
    #[must_use]
    pub fn strict() -> bool {
        static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *STRICT.get_or_init(|| {
            std::env::var("LODESTONE_VEG_STRICT").is_ok_and(|v| v != "0" && !v.is_empty())
        })
    }

    /// Zeroes this thread's census. Call immediately before the generation a
    /// gate intends to measure.
    pub fn reset() {
        CENSUS.with(|c| *c.borrow_mut() = VegCensus::default());
    }

    /// This thread's census so far.
    #[must_use]
    pub fn snapshot() -> VegCensus {
        CENSUS.with(|c| c.borrow().clone())
    }

    pub(in crate::feature::vegetation) fn bump(f: impl FnOnce(&mut VegCensus)) {
        CENSUS.with(|c| f(&mut c.borrow_mut()));
    }
}
