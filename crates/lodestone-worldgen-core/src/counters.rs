//! Structural counters for the generation pipeline — the instrument the
//! worldgen rewrite's acceptance criteria are written in
//! (`docs/plans/worldgen-rewrite.md`, Unit 1).
//!
//! # What it is
//!
//! A flat set of process-global relaxed atomics, incremented from a handful of
//! named hook points in the hot path, **compiled out entirely unless the
//! `gen-counters` cargo feature is on**. Every hook is an `#[inline(always)]`
//! function with an empty body in the default build, so a release build without
//! the feature contains no counter code at all and no `#[cfg]` clutter appears
//! at any call site.
//!
//! # Why this exists rather than another timing
//!
//! `DESIGN.md` §12's operational record carries a **585×** mis-attributed
//! timing, and this crate's own `overworld.rs` module doc records a 9×
//! structural regression that was discovered by a 700-second test run rather
//! than by an assertion. A counter is reproducible under machine load, predicts
//! an exact value, and can therefore *gate*; a duration on a shared machine is
//! a sample. Diagnostic D6 in the rewrite plan is precisely "no counters, so
//! every past performance story here was a timing".
//!
//! The rule that follows: **a counter that cannot predict is a counter that
//! cannot gate.** Every counter here has a hand-derivable expected value on a
//! known chunk, and `benches/generation.rs`'s `assert_calibration` asserts
//! those values exactly. If a counter can only be described ("it went down"),
//! it belongs in a profiler, not here.
//!
//! # How it works
//!
//! * **Counts** are process-global `AtomicU64`s with `Relaxed` ordering. Relaxed
//!   is correct for a counter that is only ever read after the measured work has
//!   been joined: no other memory is being synchronised through it. It is also
//!   the only ordering cheap enough to put inside `AquiferSystem::block_at`,
//!   which runs 98,304 times per chunk fill.
//! * **Stage attribution** is a `thread_local` [`Stage`] tag, not a global, so a
//!   parallel sweep attributes each thread's draws to the stage that thread is
//!   actually in. [`StageGuard`] saves and restores the previous tag on drop, so
//!   attribution is to the **innermost** stage: a neighbour chunk's `fill`
//!   driven from inside `ore_stage` counts as `Shape`, not `Ore`. That is the
//!   attribution the plan's per-stage questions want — "how much RNG does the
//!   vegetation walk draw" must not silently include the 25 neighbour fills its
//!   dependency closure triggered.
//! * **Reading** goes through [`snapshot`], which returns a plain-`u64`
//!   [`Snapshot`]; [`reset`] zeroes everything. A measurement is
//!   `reset(); work(); snapshot()`.
//!
//! # How to change it
//!
//! Adding a counter is three edits: a field on [`Snapshot`] and its
//! feature-gated twin, a `bump_*` hook here, and the one call site. **Then add
//! its hand-derived expected value to `benches/generation.rs`'s calibration
//! assertions** — an uncalibrated counter is the thing this module exists to
//! avoid.
//!
//! Gotchas:
//!
//! * **Do not read a counter without resetting first.** These are process
//!   globals that outlive any one measurement; an absolute reading is the
//!   "duration species" of vacuous test (`CLAUDE.md`) — it mostly reports
//!   whatever warm-up happened to run.
//! * **Do not add a counter inside a `#[cfg(feature)]` block at the call site.**
//!   Call the hook unconditionally; it is empty when the feature is off. A
//!   `#[cfg]` at the call site is how a hook silently stops being called.
//! * **`slot_misses_by_slot` is bounded** ([`MAX_TRACKED_SLOTS`]). Slots at or
//!   above the bound fold into the last bucket rather than panicking, so a
//!   settings file with more density slots than expected degrades the *detail*
//!   of this counter and nothing else. The totals stay exact.
//! * **Stage tags are per-thread, counts are per-process.** A multi-threaded
//!   sweep produces correct totals and correct per-stage splits, but no
//!   per-thread breakdown. If you need one, that is a new module, not a new
//!   ordering.
//!
//! # Configuration
//!
//! One cargo feature, `gen-counters`, default **off**. Nothing else. It is
//! additive (it only adds atomics and hook bodies) so enabling it cannot change
//! generated terrain — but it is measurably slower, so a timing and a counter
//! measurement are two different runs, never one.
//!
//! # Dependencies
//!
//! None beyond `core`/`std`. Deliberately: this module is linked into every
//! build of the crate.

/// Pipeline stages, in the order `OverworldGenerator::column_timed` runs them.
///
/// Mirrors `StageTimes`' fields one-for-one so a µs figure and a counter figure
/// can be put in the same table row. [`Stage::Other`] is the tag outside any
/// stage — generator construction, tests, direct calls into a stage function
/// from a bench.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Aquifer = 0,
    Shape = 1,
    Biome = 2,
    Surface = 3,
    Materialize = 4,
    Carve = 5,
    Ore = 6,
    Vegetation = 7,
    TopLayer = 8,
    Intern = 9,
    /// Structure-generation stages (`structure_starts`, `structure_place`).
    ///
    /// **Not one of `StageTimes`' ten compatibility slots** and deliberately has no
    /// `StageTimes`
    /// field: it runs *above* `pre_ore` (starts/refs) and *inside* it (placement),
    /// so it does not fit the flat per-column timing table. It exists because the
    /// allocation binning in `benches/generation.rs` reads [`current_stage`], and
    /// before this variant every structure allocation landed in [`Stage::Other`]
    /// alongside generator construction — which is exactly the bin an
    /// attribution cannot act on.
    Structure = 10,
    Other = 11,
}

/// Number of [`Stage`] variants, including [`Stage::Other`].
pub const STAGE_COUNT: usize = 12;

/// [`Stage`] names in discriminant order — the same strings `StageTimes`'
/// fields use, so a counter table and a timing table can be joined by name.
pub const STAGE_NAMES: [&str; STAGE_COUNT] = [
    "aquifer",
    "shape",
    "biome",
    "surface",
    "materialize",
    "carve",
    "ore",
    "vegetation",
    "top_layer",
    "intern",
    "structure",
    "other",
];

#[cfg(feature = "stage-pmu")]
mod stage_pmu {
    use std::sync::OnceLock;

    use super::Stage;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        Enter,
        Exit,
    }

    type Observer = fn(Stage, Event);

    static OBSERVER: OnceLock<Observer> = OnceLock::new();

    pub fn install(observer: Observer) -> bool {
        OBSERVER.set(observer).is_ok()
    }

    #[inline(always)]
    pub fn notify(stage: Stage, event: Event) {
        if let Some(observer) = OBSERVER.get() {
            observer(stage, event);
        }
    }
}

/// Logical cache boundaries visible to the world-generation implementation.
/// These are software representations, not CPU cache levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryBoundary {
    BlockField = 0,
    CellCache = 1,
    SlotCache = 2,
    LeafMemo = 3,
    BlockGrid = 4,
}

/// Number of logical representation boundaries tracked by the counters.
pub const MEMORY_BOUNDARY_COUNT: usize = 5;

/// Names in [`MemoryBoundary`] discriminant order.
pub const MEMORY_BOUNDARY_NAMES: [&str; MEMORY_BOUNDARY_COUNT] = [
    "block_field",
    "cell_cache",
    "slot_cache",
    "leaf_memo",
    "block_grid",
];

/// Software cache populations with existing lookup hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CacheKind {
    Cell = 0,
    Slot = 1,
    Leaf = 2,
}

/// Number of software cache populations.
pub const CACHE_COUNT: usize = 3;

/// Names in [`CacheKind`] discriminant order.
pub const CACHE_NAMES: [&str; CACHE_COUNT] = ["cell", "slot", "leaf"];

/// How many density-function slots get their own `slot_misses_by_slot` bucket.
///
/// The overworld's `slot_count` is data-derived (`DensityBuilder::slot_count`),
/// so this is a display bound, not a correctness one — see the module doc's
/// gotcha. 64 covers the 26.2 overworld router with room to spare.
pub const MAX_TRACKED_SLOTS: usize = 64;

/// A plain-`u64` reading of every counter, taken by [`snapshot`].
///
/// Always defined, whatever the feature setting — so a bench can hold one, print
/// one, and diff two without any `#[cfg]` of its own. With the feature off every
/// field reads zero, which is why anything *asserting* on a field must be gated
/// (an assertion against zeros is the vacuous-assertion species).
/// `Default` is hand-written rather than derived: `[u64; 64]`
/// ([`MAX_TRACKED_SLOTS`]) exceeds the 32-element ceiling on `Default`'s array
/// impls. Widening a bucket array is therefore an edit here too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Cache lookups that returned a value, indexed by [`CacheKind`].
    pub cache_hits: [u64; CACHE_COUNT],
    /// Cache lookups that did not return a value, indexed by [`CacheKind`].
    pub cache_misses: [u64; CACHE_COUNT],
    /// Values computed after a cache miss, indexed by [`CacheKind`].
    pub cache_computes: [u64; CACHE_COUNT],
    /// Existing entries displaced by a bounded cache, indexed by [`CacheKind`].
    /// The current implementation has displacement only in the direct-mapped
    /// leaf memo; map and dense stores do not evict entries.
    pub cache_evictions: [u64; CACHE_COUNT],
    /// Logical representation reads, indexed by [`MemoryBoundary`].
    /// A read is an attempted payload lookup; a miss still counts one lookup.
    pub logical_reads: [u64; MEMORY_BOUNDARY_COUNT],
    /// Logical representation writes, indexed by [`MemoryBoundary`].
    pub logical_writes: [u64; MEMORY_BOUNDARY_COUNT],
    /// Payload bytes returned by logical reads, indexed by boundary. Metadata
    /// probes and CPU cache-line traffic are not represented by this field.
    pub logical_read_bytes: [u64; MEMORY_BOUNDARY_COUNT],
    /// Payload bytes written by logical writes, indexed by boundary.
    pub logical_write_bytes: [u64; MEMORY_BOUNDARY_COUNT],
    /// Compatibility view of `logical_reads[MemoryBoundary::BlockField]`.
    /// Keeping this derived avoids a second atomic increment per query.
    pub block_field_queries: u64,
    /// Scratch instances acquired from the thread-local free list.
    pub scratch_pool_reuses: u64,
    /// Scratch instances created because the free list was empty.
    pub scratch_pool_allocations: u64,
    /// Scratch instances discarded because the free-list bound was full.
    pub scratch_pool_evictions: u64,
    /// Cumulative logical payload bytes added by scratch buffer growth.
    pub scratch_buffer_allocated_bytes: u64,
    /// Logical bytes retained by all live and pooled scratch buffers at read
    /// time. This excludes allocator metadata and hash-table overhead.
    pub scratch_retained_bytes: u64,
    /// Maximum observed [`Self::scratch_retained_bytes`] since reset.
    pub scratch_retained_bytes_high_water: u64,
    /// Full-column scans performed by an owning pipeline boundary. The core
    /// sampler has no knowledge of whether a caller's sequence is a scan, so
    /// this remains zero until such a boundary calls the hook.
    pub full_column_scans: u64,
    /// Logical block cells visited by full-column scans.
    pub full_column_scan_cells: u64,
    /// Representation conversions over complete columns.
    pub full_column_conversions: u64,
    /// Logical block cells covered by complete-column conversions.
    pub full_column_conversion_cells: u64,
    /// `AquiferSystem::block_at` calls. Exactly `256 * height` per chunk fill —
    /// **plus** [`Self::structure_probe_block_at`] and
    /// [`Self::structure_context_block_at`], which are structure consumers with
    /// no per-chunk shape at all. Never divide this by the fill count without
    /// subtracting both terms first.
    pub block_at: u64,
    /// `NoiseChunkSampler::eval` entries, indexed by
    /// [`Density::kind_index`](crate::density::Density::kind_index) — the
    /// per-block chunk-field evaluator, diagnostic D1's hot path.
    pub density_evals: [u64; crate::density::Density::KIND_COUNT],
    /// `Density::compute` entries, same indexing — the **other** density
    /// evaluator, for arbitrary points outside a `NoiseChunk` (the aquifer's
    /// `preliminary_surface_level`, the surface system, the carvers).
    ///
    /// Split from [`density_evals`](Self::density_evals) deliberately rather
    /// than summed into it: they are two different interpreters over the same
    /// AST, U4 replaces them on different schedules, and a single merged number
    /// would make it impossible to tell which one a regression came from. A
    /// survey of this crate found the second evaluator only by grepping for the
    /// `match`; a merged counter would have hidden it again.
    pub density_point_computes: [u64; crate::density::Density::KIND_COUNT],
    /// Preliminary-surface requests after the caller's integer `(>> 2) << 2`
    /// snap. This includes cache hits and misses from both aquifer and surface
    /// consumers.
    pub preliminary_surface_requests: u64,
    /// Distinct preliminary-surface keys served by the shared cache. This is
    /// derived from cache computations rather than maintaining a set on the
    /// request path, so it has no lock or per-key allocation.
    pub preliminary_surface_unique: u64,
    /// Preliminary-surface density evaluations that were not answered by a
    /// preliminary cache.
    pub preliminary_surface_computations: u64,
    /// Corner *lookups* — one per corner fetched while filling a cell, hit or
    /// miss. Since U4's hoist this is exactly `8 * cell_fills`, **not** 8 per
    /// block: the eight corners of a cell are fetched once for the whole cell.
    /// For one chunk-bounded interpolated slot that is `768 * 8 = 6,144` where
    /// the pre-hoist walker measured `98,304 * 8 = 786,432`.
    pub corner_lookups: u64,
    /// Cell fills — one per `interpolated` cell whose eight corners were
    /// assembled, i.e. per miss in the per-cell corner cache. The hoist's
    /// primary quantity: [`corner_lookups`](Self::corner_lookups) is
    /// mechanically `8 *` this, so asserting both pins the identity as well as
    /// the magnitude.
    pub cell_fills: u64,
    /// Corner *evaluations* — cell-corner fetches that missed the per-slot memo
    /// and had to evaluate the subtree.
    ///
    /// Split out from [`slot_misses`](Self::slot_misses) because that total also
    /// counts `flat_cache` misses, and because `slot_misses_by_slot` folds every
    /// slot at [`MAX_TRACKED_SLOTS`] — the real router's interpolated slots are
    /// numbered above 1000, so they all land in the fold bin and the per-slot
    /// split cannot separate them. This counter is the one that can be compared
    /// against the derived corner lattice (`5 * 49 * 5 = 1,225` per slot for a
    /// chunk), which is the quantity the hoist must **not** change.
    pub corner_evals: u64,
    /// Vectorised gradient batches in `ImprovedNoise::sample_and_lerp` — one per
    /// call, each computing the **eight** corner dot products of one noise
    /// lattice cell in eight `std::simd` lanes (Unit 5).
    ///
    /// This is the island check for the SIMD kernel, and it is predictive rather
    /// than a smoke test: one `PerlinNoise::get_value` over a stack with `k`
    /// non-`None` octaves is exactly `k` batches, so a caller with a known octave
    /// count pins the expected value from outside the kernel.
    ///
    /// What it does **not** prove is that the lanes stayed vectorised — a counter
    /// cannot see LLVM scalarising a `Simd` op. That question is answered by
    /// disassembly, recorded in `docs/worldgen-simd-kernels.md`.
    pub noise_corner_batches: u64,
    /// Active octave visits in the packed Perlin kernel.
    pub noise_active_visits: u64,
    /// Empty octave slots bypassed by the packed Perlin kernel.
    pub noise_skipped_visits: u64,
    /// Slot-cache lookups that found a memoised value.
    pub slot_hits: u64,
    /// Slot-cache lookups that had to evaluate the subtree — the real
    /// corner/flat-cache evaluation count.
    pub slot_misses: u64,
    /// [`slot_misses`](Self::slot_misses) split by density slot, folded at
    /// [`MAX_TRACKED_SLOTS`].
    pub slot_misses_by_slot: [u64; MAX_TRACKED_SLOTS],
    /// `DenseBlockGrid::set` calls that created a new palette entry (and so
    /// allocated two `String`s).
    pub palette_intern_new: u64,
    /// `DenseBlockGrid::set` calls that found an existing palette entry (a
    /// `HashMap<String, u16>` probe on the hot write path — diagnostic D2).
    pub palette_intern_hit: u64,
    /// `pre_ore_stage` misses: stages 1–4 actually computed for some chunk.
    pub pre_ore_computed: u64,
    /// `pre_ore_stage` hits: served from the memo cache.
    pub pre_ore_hits: u64,
    /// Request-scoped bordered climate grids prepared for biome and surface stages.
    pub climate_grid_preparations: u64,
    /// `biome::nearest_biome` calls — climate nearest-neighbour searches.
    ///
    /// **Not brute-force any more.** U9 (`7ff942dd`) put a ported
    /// R-tree on this path, and `71dd8b22` then matched vanilla's own
    /// traversal rather than its own brute-force search, so a "search" is now a
    /// pruned descent rather than a full table scan. Pair this with
    /// [`biome_rows_compared`](Self::biome_rows_compared) to get evaluations per
    /// search; neither is reconstructible from the other.
    pub biome_searches: u64,
    /// Climate-table rows compared across all
    /// [`biome_searches`](Self::biome_searches). The D5 number.
    ///
    /// `biome_rows_compared == biome_searches * table_len` was true only of the
    /// brute-force scan, and U9 (`7ff942dd`) ended that: the ported
    /// `Climate.RTree` prunes, so the ratio is now node evaluations per search —
    /// measured at 113.7 on the real table, against 7,594 for the linear scan.
    /// Do not reconstruct one of these counters from the other.
    pub biome_rows_compared: u64,
    /// RNG primitive draws, attributed to the innermost [`Stage`].
    pub rng_draws: [u64; STAGE_COUNT],
    /// [`StageGuard`] entries per [`Stage`] — how many times each stage ran.
    /// The counter U6's acceptance criterion ("stage computations == chunks ×
    /// stages exactly") is written in.
    pub stage_entered: [u64; STAGE_COUNT],
    /// Cells copied by `stitch_region` + `stitch_veg_region` — diagnostic D2's
    /// ~2.8M-per-column figure, and U7's acceptance criterion (zero).
    pub stitch_cells: u64,
    /// `String` allocations on the block path.
    ///
    /// **Both original contributors are gone as of Unit 3** — `dense_grid`'s
    /// two-per-new-palette-entry and `stitch_veg_region`'s one-per-cell — so
    /// what remains here is [`Self::state_intern_new`]'s warmup interning. The
    /// per-column figure this counter was built to watch went
    /// **905,459 → 20,684** (measured). The residue is *not* instrumented here
    /// — it lives in code paths this counter never covered — so attribute it
    /// with `benches/generation.rs`'s per-stage allocation binning, which reads
    /// [`current_stage`] from inside the counting allocator.
    ///
    /// **This counter is hand-bumped, so it is an attribution aid, not the
    /// gate.** Zeroing it proves nothing on its own — deleting a `bump` call
    /// would do it. The gate is `benches/generation.rs`'s counting allocator
    /// (`measure_allocs`), which counts real `GlobalAlloc` calls.
    pub string_allocs: u64,
    /// New entries in a [`crate::interner::StateInterner`] — i.e. the first
    /// time that generator sees a given block-state string, which is the one
    /// allocating path in that module. U3's zero-allocation claim rests on this
    /// being **0 for a steady-state column** and non-zero only during warmup;
    /// a non-zero steady-state value means some stage synthesises a state
    /// string the warmup never produced.
    pub state_intern_new: u64,
    /// [`crate::interner::StateInterner::name_of`] calls — id-to-string
    /// resolutions. Not an allocation (the names are interned), but each takes
    /// a shared `RwLock` read guard, so this is the counter that makes a
    /// regression into per-block string resolution visible. Expected to fall
    /// toward zero as Unit 8 ports the vegetation engine off strings.
    pub state_name_lookups: u64,
    /// Chunks whose `structure_starts` really ran (bumped inside the store slot's
    /// once-guard, so a cache hit does not count).
    ///
    /// This is the closure-size counter for the structure engine's stage 0a, and it is the
    /// one that explains why a cold column touches far more chunks than the 5×5
    /// pre-ore closure: `pre_ore` reads `structure_refs`, which walks
    /// `REFS_RADIUS` = 8 chunks in every direction.
    pub structure_starts_computed: u64,
    /// `StartContext::first_occupied_height` calls — structure placement asking
    /// for a heightmap value on a chunk whose terrain does not exist yet.
    pub structure_height_probes: u64,
    /// [`Self::block_at`] calls issued *by* those probes.
    ///
    /// The reason this counter exists: `block_at` used to be exactly
    /// `256 × height` per chunk fill. The calibration asserts the identity
    /// `block_at == fill cells + structure_probe_block_at +
    /// structure_context_block_at` — which keeps the per-fill figure pinned
    /// instead of absorbing structure work into a literal.
    pub structure_probe_block_at: u64,
    /// `AquiferSystem`s built *for* those probes, rather than for a chunk fill.
    ///
    /// Same job as [`Self::structure_probe_block_at`], one level up:
    /// `stage_entered[Aquifer]` used to equal the pre-ore closure size exactly,
    /// and a structure probe needs a real aquifer for a chunk whose terrain may
    /// never be generated. Counted so that assertion can stay an exact
    /// decomposition.
    pub structure_aquifers_built: u64,
    /// `AquiferSystem::block_at` calls made by structure-placement predicates.
    pub structure_context_block_at: u64,
    /// Structure-placement calls made by `is_replaceable_at`.
    pub structure_context_replaceable_block_at: u64,
    /// Structure-placement calls made by `block_kind_at`.
    pub structure_context_kind_block_at: u64,
    /// Structure-reference products computed after a store miss.
    pub structure_reference_computations: u64,
    /// Random-spread placement cells probed while enumerating origin candidates.
    pub structure_candidate_cell_probes: u64,
    /// Raw ring reach lists built for a registry.
    pub structure_ring_reach_builds: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            cache_hits: [0; CACHE_COUNT],
            cache_misses: [0; CACHE_COUNT],
            cache_computes: [0; CACHE_COUNT],
            cache_evictions: [0; CACHE_COUNT],
            logical_reads: [0; MEMORY_BOUNDARY_COUNT],
            logical_writes: [0; MEMORY_BOUNDARY_COUNT],
            logical_read_bytes: [0; MEMORY_BOUNDARY_COUNT],
            logical_write_bytes: [0; MEMORY_BOUNDARY_COUNT],
            block_field_queries: 0,
            scratch_pool_reuses: 0,
            scratch_pool_allocations: 0,
            scratch_pool_evictions: 0,
            scratch_buffer_allocated_bytes: 0,
            scratch_retained_bytes: 0,
            scratch_retained_bytes_high_water: 0,
            full_column_scans: 0,
            full_column_scan_cells: 0,
            full_column_conversions: 0,
            full_column_conversion_cells: 0,
            block_at: 0,
            density_evals: [0; crate::density::Density::KIND_COUNT],
            density_point_computes: [0; crate::density::Density::KIND_COUNT],
            preliminary_surface_requests: 0,
            preliminary_surface_unique: 0,
            preliminary_surface_computations: 0,
            corner_lookups: 0,
            cell_fills: 0,
            corner_evals: 0,
            noise_corner_batches: 0,
            noise_active_visits: 0,
            noise_skipped_visits: 0,
            slot_hits: 0,
            slot_misses: 0,
            slot_misses_by_slot: [0; MAX_TRACKED_SLOTS],
            palette_intern_new: 0,
            palette_intern_hit: 0,
            pre_ore_computed: 0,
            pre_ore_hits: 0,
            climate_grid_preparations: 0,
            biome_searches: 0,
            biome_rows_compared: 0,
            rng_draws: [0; STAGE_COUNT],
            stage_entered: [0; STAGE_COUNT],
            stitch_cells: 0,
            string_allocs: 0,
            state_intern_new: 0,
            state_name_lookups: 0,
            structure_starts_computed: 0,
            structure_height_probes: 0,
            structure_probe_block_at: 0,
            structure_aquifers_built: 0,
            structure_context_block_at: 0,
            structure_context_replaceable_block_at: 0,
            structure_context_kind_block_at: 0,
            structure_reference_computations: 0,
            structure_candidate_cell_probes: 0,
            structure_ring_reach_builds: 0,
        }
    }
}

impl Snapshot {
    /// Total `NoiseChunkSampler::eval` entries across all kinds.
    #[must_use]
    pub fn density_evals_total(&self) -> u64 {
        self.density_evals.iter().sum()
    }

    /// Total `Density::compute` entries across all kinds.
    #[must_use]
    pub fn density_point_computes_total(&self) -> u64 {
        self.density_point_computes.iter().sum()
    }

    /// Total RNG primitive draws across all stages.
    #[must_use]
    pub fn rng_draws_total(&self) -> u64 {
        self.rng_draws.iter().sum()
    }

    /// Per-kind density evaluation counts, highest first, zero-count kinds
    /// dropped — the shape a report wants.
    #[must_use]
    pub fn density_evals_ranked(&self) -> Vec<(&'static str, u64)> {
        let mut rows: Vec<(&'static str, u64)> = crate::density::Density::KIND_NAMES
            .iter()
            .zip(self.density_evals.iter())
            .filter(|&(_, &n)| n > 0)
            .map(|(&name, &n)| (name, n))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        rows
    }
}

#[cfg(feature = "gen-counters")]
mod imp {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use super::{
        CACHE_COUNT, MAX_TRACKED_SLOTS, MEMORY_BOUNDARY_COUNT, STAGE_COUNT, CacheKind,
        MemoryBoundary, Snapshot, Stage,
    };
    #[cfg(feature = "stage-pmu")]
    use super::stage_pmu::{self, Event};

    const KINDS: usize = crate::density::Density::KIND_COUNT;

    struct Counters {
        cache_hits: [AtomicU64; CACHE_COUNT],
        cache_misses: [AtomicU64; CACHE_COUNT],
        cache_computes: [AtomicU64; CACHE_COUNT],
        cache_evictions: [AtomicU64; CACHE_COUNT],
        logical_reads: [AtomicU64; MEMORY_BOUNDARY_COUNT],
        logical_writes: [AtomicU64; MEMORY_BOUNDARY_COUNT],
        logical_read_bytes: [AtomicU64; MEMORY_BOUNDARY_COUNT],
        logical_write_bytes: [AtomicU64; MEMORY_BOUNDARY_COUNT],
        scratch_pool_reuses: AtomicU64,
        scratch_pool_allocations: AtomicU64,
        scratch_pool_evictions: AtomicU64,
        scratch_buffer_allocated_bytes: AtomicU64,
        scratch_retained_bytes: AtomicU64,
        scratch_retained_bytes_high_water: AtomicU64,
        full_column_scans: AtomicU64,
        full_column_scan_cells: AtomicU64,
        full_column_conversions: AtomicU64,
        full_column_conversion_cells: AtomicU64,
        block_at: AtomicU64,
        density_evals: [AtomicU64; KINDS],
        density_point_computes: [AtomicU64; KINDS],
        preliminary_surface_requests: AtomicU64,
        preliminary_surface_computations: AtomicU64,
        corner_lookups: AtomicU64,
        cell_fills: AtomicU64,
        corner_evals: AtomicU64,
        noise_corner_batches: AtomicU64,
        noise_active_visits: AtomicU64,
        noise_skipped_visits: AtomicU64,
        slot_hits: AtomicU64,
        slot_misses: AtomicU64,
        slot_misses_by_slot: [AtomicU64; MAX_TRACKED_SLOTS],
        palette_intern_new: AtomicU64,
        palette_intern_hit: AtomicU64,
        pre_ore_computed: AtomicU64,
        pre_ore_hits: AtomicU64,
        climate_grid_preparations: AtomicU64,
        biome_searches: AtomicU64,
        biome_rows_compared: AtomicU64,
        rng_draws: [AtomicU64; STAGE_COUNT],
        stage_entered: [AtomicU64; STAGE_COUNT],
        stitch_cells: AtomicU64,
        string_allocs: AtomicU64,
        state_intern_new: AtomicU64,
        state_name_lookups: AtomicU64,
        structure_starts_computed: AtomicU64,
        structure_height_probes: AtomicU64,
        structure_probe_block_at: AtomicU64,
        structure_aquifers_built: AtomicU64,
        structure_context_block_at: AtomicU64,
        structure_context_replaceable_block_at: AtomicU64,
        structure_context_kind_block_at: AtomicU64,
        structure_reference_computations: AtomicU64,
        structure_candidate_cell_probes: AtomicU64,
        structure_ring_reach_builds: AtomicU64,
    }

    static C: Counters = Counters {
        cache_hits: [const { AtomicU64::new(0) }; CACHE_COUNT],
        cache_misses: [const { AtomicU64::new(0) }; CACHE_COUNT],
        cache_computes: [const { AtomicU64::new(0) }; CACHE_COUNT],
        cache_evictions: [const { AtomicU64::new(0) }; CACHE_COUNT],
        logical_reads: [const { AtomicU64::new(0) }; MEMORY_BOUNDARY_COUNT],
        logical_writes: [const { AtomicU64::new(0) }; MEMORY_BOUNDARY_COUNT],
        logical_read_bytes: [const { AtomicU64::new(0) }; MEMORY_BOUNDARY_COUNT],
        logical_write_bytes: [const { AtomicU64::new(0) }; MEMORY_BOUNDARY_COUNT],
        scratch_pool_reuses: AtomicU64::new(0),
        scratch_pool_allocations: AtomicU64::new(0),
        scratch_pool_evictions: AtomicU64::new(0),
        scratch_buffer_allocated_bytes: AtomicU64::new(0),
        scratch_retained_bytes: AtomicU64::new(0),
        scratch_retained_bytes_high_water: AtomicU64::new(0),
        full_column_scans: AtomicU64::new(0),
        full_column_scan_cells: AtomicU64::new(0),
        full_column_conversions: AtomicU64::new(0),
        full_column_conversion_cells: AtomicU64::new(0),
        block_at: AtomicU64::new(0),
        density_evals: [const { AtomicU64::new(0) }; KINDS],
        density_point_computes: [const { AtomicU64::new(0) }; KINDS],
        preliminary_surface_requests: AtomicU64::new(0),
        preliminary_surface_computations: AtomicU64::new(0),
        corner_lookups: AtomicU64::new(0),
        cell_fills: AtomicU64::new(0),
        corner_evals: AtomicU64::new(0),
        noise_corner_batches: AtomicU64::new(0),
        noise_active_visits: AtomicU64::new(0),
        noise_skipped_visits: AtomicU64::new(0),
        slot_hits: AtomicU64::new(0),
        slot_misses: AtomicU64::new(0),
        slot_misses_by_slot: [const { AtomicU64::new(0) }; MAX_TRACKED_SLOTS],
        palette_intern_new: AtomicU64::new(0),
        palette_intern_hit: AtomicU64::new(0),
        pre_ore_computed: AtomicU64::new(0),
        pre_ore_hits: AtomicU64::new(0),
        climate_grid_preparations: AtomicU64::new(0),
        biome_searches: AtomicU64::new(0),
        biome_rows_compared: AtomicU64::new(0),
        rng_draws: [const { AtomicU64::new(0) }; STAGE_COUNT],
        stage_entered: [const { AtomicU64::new(0) }; STAGE_COUNT],
        stitch_cells: AtomicU64::new(0),
        string_allocs: AtomicU64::new(0),
        state_intern_new: AtomicU64::new(0),
        state_name_lookups: AtomicU64::new(0),
        structure_starts_computed: AtomicU64::new(0),
        structure_height_probes: AtomicU64::new(0),
        structure_probe_block_at: AtomicU64::new(0),
        structure_aquifers_built: AtomicU64::new(0),
        structure_context_block_at: AtomicU64::new(0),
        structure_context_replaceable_block_at: AtomicU64::new(0),
        structure_context_kind_block_at: AtomicU64::new(0),
        structure_reference_computations: AtomicU64::new(0),
        structure_candidate_cell_probes: AtomicU64::new(0),
        structure_ring_reach_builds: AtomicU64::new(0),
    };

    thread_local! {
        /// The innermost stage this thread is executing. Per-thread, so a
        /// parallel sweep attributes correctly; see the module doc.
        static STAGE: Cell<Stage> = const { Cell::new(Stage::Other) };
    }

    #[inline]
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    #[inline]
    fn bump_by(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Relaxed);
    }

    /// Records one logical payload lookup at a representation boundary. The
    /// payload byte count is deliberately supplied by the caller: it describes
    /// representation bytes, not cache lines or DRAM traffic.
    #[inline]
    pub fn bump_logical_read(boundary: MemoryBoundary, items: u64, bytes: u64) {
        bump_by(&C.logical_reads[boundary as usize], items);
        bump_by(&C.logical_read_bytes[boundary as usize], bytes);
    }

    /// Records one logical payload write at a representation boundary.
    #[inline]
    pub fn bump_logical_write(boundary: MemoryBoundary, items: u64, bytes: u64) {
        bump_by(&C.logical_writes[boundary as usize], items);
        bump_by(&C.logical_write_bytes[boundary as usize], bytes);
    }

    #[inline]
    pub fn bump_cache_lookup(kind: CacheKind, hit: bool) {
        bump(if hit {
            &C.cache_hits[kind as usize]
        } else {
            &C.cache_misses[kind as usize]
        });
    }

    #[inline]
    pub fn bump_cache_compute(kind: CacheKind) {
        bump(&C.cache_computes[kind as usize]);
    }

    #[inline]
    pub fn bump_cache_eviction(kind: CacheKind) {
        bump(&C.cache_evictions[kind as usize]);
    }

    #[inline]
    pub fn bump_scratch_pool_reuse() {
        bump(&C.scratch_pool_reuses);
    }

    #[inline]
    pub fn bump_scratch_pool_allocation() {
        bump(&C.scratch_pool_allocations);
    }

    #[inline]
    pub fn bump_scratch_pool_eviction() {
        bump(&C.scratch_pool_evictions);
    }

    /// Adds bytes to the cumulative logical scratch-buffer growth counter.
    #[inline]
    pub fn bump_scratch_buffer_allocated_bytes(bytes: u64) {
        bump_by(&C.scratch_buffer_allocated_bytes, bytes);
    }

    #[inline]
    fn update_high_water(value: u64) {
        let mut old = C.scratch_retained_bytes_high_water.load(Relaxed);
        while value > old {
            match C.scratch_retained_bytes_high_water.compare_exchange_weak(
                old,
                value,
                Relaxed,
                Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => old = next,
            }
        }
    }

    #[inline]
    pub fn bump_scratch_retained_add(bytes: u64) {
        let value = C.scratch_retained_bytes.fetch_add(bytes, Relaxed) + bytes;
        update_high_water(value);
    }

    #[inline]
    pub fn bump_scratch_retained_remove(bytes: u64) {
        C.scratch_retained_bytes.fetch_sub(bytes, Relaxed);
    }

    /// Records a complete-column scan and its logical cell count. This hook is
    /// for owning pipeline/materialization boundaries; the core sampler cannot
    /// infer a scan from an arbitrary sequence of point queries.
    #[inline]
    pub fn bump_full_column_scan(cells: u64) {
        bump(&C.full_column_scans);
        bump_by(&C.full_column_scan_cells, cells);
    }

    /// Records a complete-column representation conversion.
    #[inline]
    pub fn bump_full_column_conversion(cells: u64) {
        bump(&C.full_column_conversions);
        bump_by(&C.full_column_conversion_cells, cells);
    }

    #[inline]
    pub fn current_stage() -> Stage {
        STAGE.get()
    }

    #[inline]
    pub fn bump_block_at() {
        bump(&C.block_at);
    }

    #[inline]
    pub fn bump_density_eval(kind_index: usize) {
        if let Some(slot) = C.density_evals.get(kind_index) {
            bump(slot);
        }
    }

    #[inline]
    pub fn bump_density_point_compute(kind_index: usize) {
        if let Some(slot) = C.density_point_computes.get(kind_index) {
            bump(slot);
        }
    }

    /// Records one snapped preliminary-surface request. Coordinates are kept
    /// in the API for call-site compatibility; uniqueness is derived from the
    /// shared cache's computation count without retaining a key set.
    #[inline]
    pub fn bump_preliminary_surface_request(_qx: i32, _qz: i32) {
        bump(&C.preliminary_surface_requests);
    }

    #[inline]
    pub fn bump_preliminary_surface_compute() {
        bump(&C.preliminary_surface_computations);
    }

    #[inline]
    pub fn bump_corner_lookup() {
        bump(&C.corner_lookups);
    }

    /// One `interpolated` cell had its eight corners assembled.
    #[inline]
    pub fn bump_cell_fill() {
        bump(&C.cell_fills);
    }

    /// One cell corner missed the per-slot memo and was evaluated.
    #[inline]
    pub fn bump_corner_eval() {
        bump(&C.corner_evals);
    }

    /// One vectorised eight-lane gradient batch ran (Unit 5's SIMD kernel).
    #[inline]
    pub fn bump_noise_corner_batch() {
        bump(&C.noise_corner_batches);
    }

    #[inline]
    pub fn bump_noise_active_visits(n: u64) {
        bump_by(&C.noise_active_visits, n);
    }

    #[inline]
    pub fn bump_noise_skipped_visits(n: u64) {
        bump_by(&C.noise_skipped_visits, n);
    }

    #[inline]
    pub fn bump_slot_hit() {
        bump(&C.slot_hits);
    }

    #[inline]
    pub fn bump_slot_miss(slot: usize) {
        bump(&C.slot_misses);
        bump(&C.slot_misses_by_slot[slot.min(MAX_TRACKED_SLOTS - 1)]);
    }

    #[inline]
    pub fn bump_palette_intern_new() {
        bump(&C.palette_intern_new);
        // No `string_allocs` bump: as of Unit 3 a new palette entry is a
        // `StateId` push plus a `u16`-keyed map insert, so it allocates no
        // `String` at all. It used to cost two (the `palette` push and the
        // `index_of` key) and this counter used to say so — left as a comment
        // rather than deleted because a stale *attribution* is exactly the
        // failure mode `CLAUDE.md`'s rule 2 describes, and a reader comparing
        // against the pre-U3 numbers in `docs/plans/worldgen-rewrite.md` needs
        // to know the 2-per-entry term went away rather than went missing.
    }

    #[inline]
    pub fn bump_palette_intern_hit() {
        bump(&C.palette_intern_hit);
    }

    #[inline]
    pub fn bump_pre_ore(computed: bool) {
        bump(if computed { &C.pre_ore_computed } else { &C.pre_ore_hits });
    }

    #[inline]
    pub fn bump_climate_grid_preparation() {
        bump(&C.climate_grid_preparations);
    }

    #[inline]
    pub fn bump_biome_search(rows: u64) {
        bump(&C.biome_searches);
        bump_by(&C.biome_rows_compared, rows);
    }

    #[inline]
    pub fn bump_rng_draw() {
        bump(&C.rng_draws[STAGE.get() as usize]);
    }

    #[inline]
    pub fn bump_stitch_cells(n: u64) {
        bump_by(&C.stitch_cells, n);
    }

    #[inline]
    pub fn bump_string_allocs(n: u64) {
        bump_by(&C.string_allocs, n);
    }

    #[inline(always)]
    pub fn bump_state_intern_new() {
        bump_by(&C.state_intern_new, 1);
        // A new intern owns its string, so it is also a real `String`
        // allocation — attributed here too, so `string_allocs` stays a complete
        // account of the block path rather than silently losing the ones that
        // moved from `to_string()` into the interner.
        bump_by(&C.string_allocs, 1);
    }

    #[inline(always)]
    pub fn bump_state_name_lookup() {
        bump_by(&C.state_name_lookups, 1);
    }

    /// One chunk's `structure_starts` really ran (call from inside the once-guard).
    #[inline]
    pub fn bump_structure_start() {
        bump(&C.structure_starts_computed);
    }

    /// One `first_occupied_height` probe finished, having issued `queries`
    /// `block_at` calls. Bumped once per probe rather than once per query so the
    /// per-probe scan depth is recoverable as a ratio.
    #[inline]
    pub fn bump_structure_height_probe(queries: u64) {
        bump(&C.structure_height_probes);
        bump_by(&C.structure_probe_block_at, queries);
    }

    /// One `AquiferSystem` was built for a structure probe rather than a fill.
    #[inline]
    pub fn bump_structure_aquifer() {
        bump(&C.structure_aquifers_built);
    }

    #[inline]
    pub fn bump_structure_context_block_at() {
        bump(&C.structure_context_block_at);
    }

    #[inline]
    pub fn bump_structure_context_replaceable_block_at() {
        bump(&C.structure_context_replaceable_block_at);
        bump_structure_context_block_at();
    }

    #[inline]
    pub fn bump_structure_context_kind_block_at() {
        bump(&C.structure_context_kind_block_at);
        bump_structure_context_block_at();
    }

    #[inline(always)]
    pub fn bump_structure_reference_computation() {
        bump(&C.structure_reference_computations);
    }

    #[inline(always)]
    pub fn bump_structure_candidate_cell_probe() {
        bump(&C.structure_candidate_cell_probes);
    }

    #[inline(always)]
    pub fn bump_structure_ring_reach_build() {
        bump(&C.structure_ring_reach_builds);
    }

    /// Enters `stage` on this thread; the previous tag is restored on drop.
    #[derive(Debug)]
    pub struct StageGuard {
        previous: Stage,
        #[cfg(feature = "stage-pmu")]
        observed: bool,
    }

    impl StageGuard {
        #[inline]
        pub fn enter(stage: Stage) -> Self {
            bump(&C.stage_entered[stage as usize]);
            let previous = STAGE.replace(stage);
            #[cfg(feature = "stage-pmu")]
            let observed = previous == Stage::Other;
            #[cfg(feature = "stage-pmu")]
            if observed {
                stage_pmu::notify(stage, Event::Enter);
            }
            Self {
                previous,
                #[cfg(feature = "stage-pmu")]
                observed,
            }
        }
    }

    impl Drop for StageGuard {
        #[inline]
        fn drop(&mut self) {
            #[cfg(feature = "stage-pmu")]
            if self.observed {
                stage_pmu::notify(STAGE.get(), Event::Exit);
            }
            STAGE.set(self.previous);
        }
    }

    #[cfg(feature = "stage-pmu")]
    pub fn install_stage_observer(observer: fn(Stage, Event)) -> bool {
        stage_pmu::install(observer)
    }

    pub fn reset() {
        for a in &C.cache_hits {
            a.store(0, Relaxed);
        }
        for a in &C.cache_misses {
            a.store(0, Relaxed);
        }
        for a in &C.cache_computes {
            a.store(0, Relaxed);
        }
        for a in &C.cache_evictions {
            a.store(0, Relaxed);
        }
        for a in &C.logical_reads {
            a.store(0, Relaxed);
        }
        for a in &C.logical_writes {
            a.store(0, Relaxed);
        }
        for a in &C.logical_read_bytes {
            a.store(0, Relaxed);
        }
        for a in &C.logical_write_bytes {
            a.store(0, Relaxed);
        }
        C.scratch_pool_reuses.store(0, Relaxed);
        C.scratch_pool_allocations.store(0, Relaxed);
        C.scratch_pool_evictions.store(0, Relaxed);
        C.scratch_buffer_allocated_bytes.store(0, Relaxed);
        // Retained bytes describe live/pool state rather than an event stream;
        // preserve the current value across a reset so the next release cannot
        // underflow it. The high-water mark starts at that retained baseline.
        let retained = C.scratch_retained_bytes.load(Relaxed);
        C.scratch_retained_bytes_high_water.store(retained, Relaxed);
        C.full_column_scans.store(0, Relaxed);
        C.full_column_scan_cells.store(0, Relaxed);
        C.full_column_conversions.store(0, Relaxed);
        C.full_column_conversion_cells.store(0, Relaxed);
        C.block_at.store(0, Relaxed);
        for a in &C.density_evals {
            a.store(0, Relaxed);
        }
        for a in &C.density_point_computes {
            a.store(0, Relaxed);
        }
        C.preliminary_surface_requests.store(0, Relaxed);
        C.preliminary_surface_computations.store(0, Relaxed);
        C.corner_lookups.store(0, Relaxed);
        C.cell_fills.store(0, Relaxed);
        C.corner_evals.store(0, Relaxed);
        C.noise_corner_batches.store(0, Relaxed);
        C.noise_active_visits.store(0, Relaxed);
        C.noise_skipped_visits.store(0, Relaxed);
        C.slot_hits.store(0, Relaxed);
        C.slot_misses.store(0, Relaxed);
        for a in &C.slot_misses_by_slot {
            a.store(0, Relaxed);
        }
        C.palette_intern_new.store(0, Relaxed);
        C.palette_intern_hit.store(0, Relaxed);
        C.pre_ore_computed.store(0, Relaxed);
        C.pre_ore_hits.store(0, Relaxed);
        C.climate_grid_preparations.store(0, Relaxed);
        C.biome_searches.store(0, Relaxed);
        C.biome_rows_compared.store(0, Relaxed);
        for a in &C.rng_draws {
            a.store(0, Relaxed);
        }
        for a in &C.stage_entered {
            a.store(0, Relaxed);
        }
        C.stitch_cells.store(0, Relaxed);
        C.string_allocs.store(0, Relaxed);
        C.state_intern_new.store(0, Relaxed);
        C.state_name_lookups.store(0, Relaxed);
        C.structure_starts_computed.store(0, Relaxed);
        C.structure_height_probes.store(0, Relaxed);
        C.structure_probe_block_at.store(0, Relaxed);
        C.structure_aquifers_built.store(0, Relaxed);
        C.structure_context_block_at.store(0, Relaxed);
        C.structure_context_replaceable_block_at.store(0, Relaxed);
        C.structure_context_kind_block_at.store(0, Relaxed);
        C.structure_reference_computations.store(0, Relaxed);
        C.structure_candidate_cell_probes.store(0, Relaxed);
        C.structure_ring_reach_builds.store(0, Relaxed);
    }

    pub fn snapshot() -> Snapshot {
        Snapshot {
            cache_hits: std::array::from_fn(|i| C.cache_hits[i].load(Relaxed)),
            cache_misses: std::array::from_fn(|i| C.cache_misses[i].load(Relaxed)),
            cache_computes: std::array::from_fn(|i| C.cache_computes[i].load(Relaxed)),
            cache_evictions: std::array::from_fn(|i| C.cache_evictions[i].load(Relaxed)),
            logical_reads: std::array::from_fn(|i| C.logical_reads[i].load(Relaxed)),
            logical_writes: std::array::from_fn(|i| C.logical_writes[i].load(Relaxed)),
            logical_read_bytes: std::array::from_fn(|i| C.logical_read_bytes[i].load(Relaxed)),
            logical_write_bytes: std::array::from_fn(|i| C.logical_write_bytes[i].load(Relaxed)),
            block_field_queries: C.logical_reads[MemoryBoundary::BlockField as usize].load(Relaxed),
            scratch_pool_reuses: C.scratch_pool_reuses.load(Relaxed),
            scratch_pool_allocations: C.scratch_pool_allocations.load(Relaxed),
            scratch_pool_evictions: C.scratch_pool_evictions.load(Relaxed),
            scratch_buffer_allocated_bytes: C.scratch_buffer_allocated_bytes.load(Relaxed),
            scratch_retained_bytes: C.scratch_retained_bytes.load(Relaxed),
            scratch_retained_bytes_high_water: C.scratch_retained_bytes_high_water.load(Relaxed),
            full_column_scans: C.full_column_scans.load(Relaxed),
            full_column_scan_cells: C.full_column_scan_cells.load(Relaxed),
            full_column_conversions: C.full_column_conversions.load(Relaxed),
            full_column_conversion_cells: C.full_column_conversion_cells.load(Relaxed),
            block_at: C.block_at.load(Relaxed),
            density_evals: std::array::from_fn(|i| C.density_evals[i].load(Relaxed)),
            density_point_computes: std::array::from_fn(|i| {
                C.density_point_computes[i].load(Relaxed)
            }),
            preliminary_surface_requests: C.preliminary_surface_requests.load(Relaxed),
            preliminary_surface_unique: C.preliminary_surface_computations.load(Relaxed),
            preliminary_surface_computations: C.preliminary_surface_computations.load(Relaxed),
            corner_lookups: C.corner_lookups.load(Relaxed),
            cell_fills: C.cell_fills.load(Relaxed),
            corner_evals: C.corner_evals.load(Relaxed),
            noise_corner_batches: C.noise_corner_batches.load(Relaxed),
            noise_active_visits: C.noise_active_visits.load(Relaxed),
            noise_skipped_visits: C.noise_skipped_visits.load(Relaxed),
            slot_hits: C.slot_hits.load(Relaxed),
            slot_misses: C.slot_misses.load(Relaxed),
            slot_misses_by_slot: std::array::from_fn(|i| C.slot_misses_by_slot[i].load(Relaxed)),
            palette_intern_new: C.palette_intern_new.load(Relaxed),
            palette_intern_hit: C.palette_intern_hit.load(Relaxed),
            pre_ore_computed: C.pre_ore_computed.load(Relaxed),
            pre_ore_hits: C.pre_ore_hits.load(Relaxed),
            climate_grid_preparations: C.climate_grid_preparations.load(Relaxed),
            biome_searches: C.biome_searches.load(Relaxed),
            biome_rows_compared: C.biome_rows_compared.load(Relaxed),
            rng_draws: std::array::from_fn(|i| C.rng_draws[i].load(Relaxed)),
            stage_entered: std::array::from_fn(|i| C.stage_entered[i].load(Relaxed)),
            stitch_cells: C.stitch_cells.load(Relaxed),
            string_allocs: C.string_allocs.load(Relaxed),
            state_intern_new: C.state_intern_new.load(Relaxed),
            state_name_lookups: C.state_name_lookups.load(Relaxed),
            structure_starts_computed: C.structure_starts_computed.load(Relaxed),
            structure_height_probes: C.structure_height_probes.load(Relaxed),
            structure_probe_block_at: C.structure_probe_block_at.load(Relaxed),
            structure_aquifers_built: C.structure_aquifers_built.load(Relaxed),
            structure_context_block_at: C.structure_context_block_at.load(Relaxed),
            structure_context_replaceable_block_at: C.structure_context_replaceable_block_at.load(Relaxed),
            structure_context_kind_block_at: C.structure_context_kind_block_at.load(Relaxed),
            structure_reference_computations: C.structure_reference_computations.load(Relaxed),
            structure_candidate_cell_probes: C.structure_candidate_cell_probes.load(Relaxed),
            structure_ring_reach_builds: C.structure_ring_reach_builds.load(Relaxed),
        }
    }
}

#[cfg(not(feature = "gen-counters"))]
mod imp {
    use super::{CacheKind, MemoryBoundary, Snapshot, Stage};
    #[cfg(feature = "stage-pmu")]
    use super::stage_pmu;
    #[cfg(feature = "stage-pmu")]
    use std::cell::Cell;

    #[cfg(feature = "stage-pmu")]
    thread_local! {
        static STAGE: Cell<Stage> = const { Cell::new(Stage::Other) };
    }

    #[inline(always)]
    #[cfg(not(feature = "stage-pmu"))]
    pub fn current_stage() -> Stage {
        Stage::Other
    }
    #[cfg(feature = "stage-pmu")]
    pub fn current_stage() -> Stage {
        STAGE.get()
    }
    #[inline(always)]
    pub fn bump_logical_read(_boundary: MemoryBoundary, _items: u64, _bytes: u64) {}
    #[inline(always)]
    pub fn bump_logical_write(_boundary: MemoryBoundary, _items: u64, _bytes: u64) {}
    #[inline(always)]
    pub fn bump_cache_lookup(_kind: CacheKind, _hit: bool) {}
    #[inline(always)]
    pub fn bump_cache_compute(_kind: CacheKind) {}
    #[inline(always)]
    pub fn bump_cache_eviction(_kind: CacheKind) {}
    #[inline(always)]
    pub fn bump_scratch_pool_reuse() {}
    #[inline(always)]
    pub fn bump_scratch_pool_allocation() {}
    #[inline(always)]
    pub fn bump_scratch_pool_eviction() {}
    #[inline(always)]
    pub fn bump_scratch_buffer_allocated_bytes(_bytes: u64) {}
    #[inline(always)]
    pub fn bump_scratch_retained_add(_bytes: u64) {}
    #[inline(always)]
    pub fn bump_scratch_retained_remove(_bytes: u64) {}
    #[inline(always)]
    pub fn bump_full_column_scan(_cells: u64) {}
    #[inline(always)]
    pub fn bump_full_column_conversion(_cells: u64) {}
    #[inline(always)]
    pub fn bump_block_at() {}
    #[inline(always)]
    pub fn bump_density_eval(_kind_index: usize) {}
    #[inline(always)]
    pub fn bump_density_point_compute(_kind_index: usize) {}
    #[inline(always)]
    pub fn bump_preliminary_surface_request(_qx: i32, _qz: i32) {}
    #[inline(always)]
    pub fn bump_preliminary_surface_compute() {}
    #[inline(always)]
    pub fn bump_corner_lookup() {}
    /// Inert without the feature.
    #[inline(always)]
    pub fn bump_cell_fill() {}
    /// Inert without the feature.
    #[inline(always)]
    pub fn bump_corner_eval() {}
    /// Inert without the feature.
    #[inline(always)]
    pub fn bump_noise_corner_batch() {}
    #[inline(always)]
    pub fn bump_noise_active_visits(_n: u64) {}
    #[inline(always)]
    pub fn bump_noise_skipped_visits(_n: u64) {}
    #[inline(always)]
    pub fn bump_slot_hit() {}
    #[inline(always)]
    pub fn bump_slot_miss(_slot: usize) {}
    #[inline(always)]
    pub fn bump_palette_intern_new() {}
    #[inline(always)]
    pub fn bump_palette_intern_hit() {}
    #[inline(always)]
    pub fn bump_pre_ore(_computed: bool) {}
    #[inline(always)]
    pub fn bump_climate_grid_preparation() {}
    #[inline(always)]
    pub fn bump_biome_search(_rows: u64) {}
    #[inline(always)]
    pub fn bump_rng_draw() {}
    #[inline(always)]
    pub fn bump_stitch_cells(_n: u64) {}
    #[inline(always)]
    pub fn bump_string_allocs(_n: u64) {}
    #[inline(always)]
    pub fn bump_state_intern_new() {}
    #[inline(always)]
    pub fn bump_state_name_lookup() {}
    #[inline(always)]
    pub fn bump_structure_start() {}
    #[inline(always)]
    pub fn bump_structure_height_probe(_queries: u64) {}
    #[inline(always)]
    pub fn bump_structure_aquifer() {}
    #[inline(always)]
    pub fn bump_structure_context_block_at() {}
    #[inline(always)]
    pub fn bump_structure_context_replaceable_block_at() {}
    #[inline(always)]
    pub fn bump_structure_context_kind_block_at() {}
    #[inline(always)]
    pub fn bump_structure_reference_computation() {}
    #[inline(always)]
    pub fn bump_structure_candidate_cell_probe() {}
    #[inline(always)]
    pub fn bump_structure_ring_reach_build() {}

    #[cfg(not(feature = "stage-pmu"))]
    #[derive(Debug)]
    pub struct StageGuard;

    #[cfg(not(feature = "stage-pmu"))]
    impl StageGuard {
        #[inline(always)]
        pub fn enter(_stage: Stage) -> Self {
            Self
        }
    }

    #[cfg(feature = "stage-pmu")]
    #[derive(Debug)]
    pub struct StageGuard {
        previous: Stage,
        observed: bool,
    }

    #[cfg(feature = "stage-pmu")]
    impl StageGuard {
        #[inline]
        pub fn enter(stage: Stage) -> Self {
            let previous = STAGE.replace(stage);
            let observed = previous == Stage::Other;
            if observed {
                stage_pmu::notify(stage, stage_pmu::Event::Enter);
            }
            Self { previous, observed }
        }
    }

    #[cfg(feature = "stage-pmu")]
    impl Drop for StageGuard {
        #[inline]
        fn drop(&mut self) {
            if self.observed {
                stage_pmu::notify(STAGE.get(), stage_pmu::Event::Exit);
            }
            STAGE.set(self.previous);
        }
    }

    #[cfg(feature = "stage-pmu")]
    pub fn install_stage_observer(observer: fn(Stage, super::StageEvent)) -> bool {
        stage_pmu::install(observer)
    }

    #[inline(always)]
    pub fn reset() {}

    #[inline(always)]
    pub fn snapshot() -> Snapshot {
        Snapshot::default()
    }
}

pub use imp::{
    StageGuard, bump_biome_search, bump_block_at, bump_cache_compute,
    bump_cache_eviction, bump_cache_lookup, bump_cell_fill, bump_corner_eval,
    bump_corner_lookup, bump_density_eval,
    bump_density_point_compute,
    bump_noise_active_visits, bump_noise_corner_batch,
    bump_noise_skipped_visits, bump_palette_intern_hit,
    bump_palette_intern_new,
    bump_full_column_conversion, bump_full_column_scan, bump_logical_read, bump_logical_write,
    bump_climate_grid_preparation, bump_pre_ore, bump_preliminary_surface_compute,
    bump_preliminary_surface_request,
    bump_rng_draw, bump_scratch_buffer_allocated_bytes, bump_scratch_pool_allocation,
    bump_scratch_pool_eviction, bump_scratch_pool_reuse, bump_scratch_retained_add,
    bump_scratch_retained_remove, bump_slot_hit, bump_slot_miss, bump_state_intern_new,
    bump_state_name_lookup, bump_stitch_cells, bump_string_allocs, bump_structure_aquifer,
    bump_structure_context_block_at, bump_structure_context_kind_block_at,
    bump_structure_context_replaceable_block_at, bump_structure_height_probe, bump_structure_start,
    bump_structure_reference_computation, bump_structure_candidate_cell_probe,
    bump_structure_ring_reach_build,
    current_stage, reset, snapshot,
};

#[cfg(feature = "stage-pmu")]
pub use imp::install_stage_observer;
#[cfg(feature = "stage-pmu")]
pub use stage_pmu::Event as StageEvent;

/// Whether this build has counters compiled in.
///
/// A bench uses this to *skip loudly* rather than assert against zeros — an
/// assertion that passes because every counter reads 0 is the assertion species
/// of vacuous test.
#[must_use]
pub const fn enabled() -> bool {
    cfg!(feature = "gen-counters")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter set and the stage table must stay the same length as the
    /// name table, or a per-stage report silently mislabels every row.
    #[test]
    fn stage_names_cover_every_stage() {
        assert_eq!(STAGE_NAMES.len(), STAGE_COUNT);
        assert_eq!(Stage::Other as usize, STAGE_COUNT - 1);
        assert_eq!(STAGE_NAMES[Stage::Vegetation as usize], "vegetation");
        assert_eq!(STAGE_NAMES[Stage::Aquifer as usize], "aquifer");
        assert_eq!(MEMORY_BOUNDARY_NAMES.len(), MEMORY_BOUNDARY_COUNT);
        assert_eq!(CACHE_NAMES.len(), CACHE_COUNT);
    }

    /// With the feature off every hook must be callable and every read zero —
    /// the property that lets hook sites drop their `#[cfg]`s.
    #[cfg(not(feature = "gen-counters"))]
    #[test]
    fn hooks_are_inert_without_the_feature() {
        assert!(!enabled());
        reset();
        bump_block_at();
        bump_structure_context_replaceable_block_at();
        bump_structure_context_kind_block_at();
        bump_structure_reference_computation();
        bump_structure_candidate_cell_probe();
        bump_structure_ring_reach_build();
        bump_rng_draw();
        bump_preliminary_surface_request(-4, 8);
        bump_preliminary_surface_compute();
        bump_stitch_cells(1_000);
        bump_cache_lookup(CacheKind::Cell, true);
        bump_cache_lookup(CacheKind::Slot, false);
        bump_cache_compute(CacheKind::Slot);
        bump_cache_eviction(CacheKind::Leaf);
        bump_logical_read(MemoryBoundary::BlockGrid, 2, 4);
        bump_logical_write(MemoryBoundary::BlockGrid, 1, 2);
        bump_scratch_pool_reuse();
        bump_scratch_pool_allocation();
        bump_scratch_pool_eviction();
        bump_scratch_buffer_allocated_bytes(32);
        bump_scratch_retained_add(32);
        bump_scratch_retained_remove(32);
        bump_full_column_scan(384);
        bump_full_column_conversion(384);
        let guard = StageGuard::enter(Stage::Vegetation);
        bump_rng_draw();
        drop(guard);
        assert_eq!(snapshot(), Snapshot::default());
    }

    /// With the feature on, the hooks must actually count, the stage tag must
    /// restore on drop, and `reset` must clear. This is the control on the
    /// instrument: without it, a counter reading 0 could mean "no work" or
    /// "hook never wired".
    #[cfg(feature = "gen-counters")]
    #[test]
    fn hooks_count_and_stage_tag_restores() {
        assert!(enabled());
        reset();
        bump_block_at();
        bump_block_at();
        bump_structure_context_replaceable_block_at();
        bump_structure_context_kind_block_at();
        bump_structure_reference_computation();
        bump_structure_candidate_cell_probe();
        bump_structure_ring_reach_build();
        bump_rng_draw();
        bump_preliminary_surface_request(-4, 8);
        bump_preliminary_surface_compute();
        bump_cache_lookup(CacheKind::Cell, true);
        bump_cache_lookup(CacheKind::Slot, false);
        bump_cache_compute(CacheKind::Slot);
        bump_cache_eviction(CacheKind::Leaf);
        bump_logical_read(MemoryBoundary::BlockGrid, 2, 4);
        bump_logical_write(MemoryBoundary::BlockGrid, 1, 2);
        bump_scratch_pool_reuse();
        bump_scratch_pool_allocation();
        bump_scratch_pool_eviction();
        bump_scratch_buffer_allocated_bytes(32);
        bump_scratch_retained_add(32);
        bump_scratch_retained_remove(32);
        bump_full_column_scan(384);
        bump_full_column_conversion(384);
        {
            let _veg = StageGuard::enter(Stage::Vegetation);
            bump_rng_draw();
            bump_rng_draw();
            {
                let _shape = StageGuard::enter(Stage::Shape);
                bump_rng_draw();
                assert_eq!(current_stage(), Stage::Shape);
            }
            // Innermost-stage attribution: the inner guard restored
            // `Vegetation`, it did not leak `Shape`.
            assert_eq!(current_stage(), Stage::Vegetation);
            bump_rng_draw();
        }
        assert_eq!(current_stage(), Stage::Other);

        let s = snapshot();
        assert_eq!(s.block_at, 2);
        assert_eq!(s.structure_context_block_at, 2);
        assert_eq!(s.structure_context_replaceable_block_at, 1);
        assert_eq!(s.structure_context_kind_block_at, 1);
        assert_eq!(s.structure_reference_computations, 1);
        assert_eq!(s.structure_candidate_cell_probes, 1);
        assert_eq!(s.structure_ring_reach_builds, 1);
        assert_eq!(s.preliminary_surface_requests, 1);
        assert_eq!(s.preliminary_surface_unique, 1);
        assert_eq!(s.preliminary_surface_computations, 1);
        assert_eq!(s.rng_draws[Stage::Other as usize], 1);
        assert_eq!(s.rng_draws[Stage::Vegetation as usize], 3);
        assert_eq!(s.rng_draws[Stage::Shape as usize], 1);
        assert_eq!(s.stage_entered[Stage::Vegetation as usize], 1);
        assert_eq!(s.stage_entered[Stage::Shape as usize], 1);
        assert_eq!(s.cache_hits[CacheKind::Cell as usize], 1);
        assert_eq!(s.cache_misses[CacheKind::Slot as usize], 1);
        assert_eq!(s.cache_computes[CacheKind::Slot as usize], 1);
        assert_eq!(s.cache_evictions[CacheKind::Leaf as usize], 1);
        assert_eq!(s.logical_reads[MemoryBoundary::BlockGrid as usize], 2);
        assert_eq!(s.logical_read_bytes[MemoryBoundary::BlockGrid as usize], 4);
        assert_eq!(s.logical_writes[MemoryBoundary::BlockGrid as usize], 1);
        assert_eq!(s.logical_write_bytes[MemoryBoundary::BlockGrid as usize], 2);
        assert_eq!(s.scratch_pool_reuses, 1);
        assert_eq!(s.scratch_pool_allocations, 1);
        assert_eq!(s.scratch_pool_evictions, 1);
        assert_eq!(s.scratch_buffer_allocated_bytes, 32);
        assert_eq!(s.scratch_retained_bytes, 0);
        assert_eq!(s.scratch_retained_bytes_high_water, 32);
        assert_eq!(s.full_column_scans, 1);
        assert_eq!(s.full_column_scan_cells, 384);
        assert_eq!(s.full_column_conversions, 1);
        assert_eq!(s.full_column_conversion_cells, 384);

        reset();
        assert_eq!(snapshot(), Snapshot::default());
    }
}
