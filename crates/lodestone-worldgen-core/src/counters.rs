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
    /// **Not one of "the ten stages"** and deliberately has no `StageTimes`
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
    /// `AquiferSystem::block_at` calls. Exactly `256 * height` per chunk fill —
    /// **plus** [`Self::structure_probe_block_at`], which is a second consumer
    /// with no per-chunk shape at all. Never divide this by the fill count without
    /// subtracting that term first.
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
    /// `post_ore_world` misses: the ore RNG walk actually run for some chunk.
    pub post_ore_computed: u64,
    /// `post_ore_world` hits: served from the memo cache.
    pub post_ore_hits: u64,
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
    /// `256 × height` per chunk fill, and `benches/generation.rs`'s calibration
    /// asserted that. Structure starts added a second, data-dependent consumer,
    /// so the calibration is now the identity
    /// `block_at == fill cells + structure_probe_block_at` — which keeps the
    /// per-fill figure pinned instead of absorbing the new term into a literal.
    pub structure_probe_block_at: u64,
    /// `AquiferSystem`s built *for* those probes, rather than for a chunk fill.
    ///
    /// Same job as [`Self::structure_probe_block_at`], one level up:
    /// `stage_entered[Aquifer]` used to equal the pre-ore closure size exactly,
    /// and a structure probe needs a real aquifer for a chunk whose terrain may
    /// never be generated. Counted so that assertion can stay an exact
    /// decomposition.
    pub structure_aquifers_built: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            block_at: 0,
            density_evals: [0; crate::density::Density::KIND_COUNT],
            density_point_computes: [0; crate::density::Density::KIND_COUNT],
            corner_lookups: 0,
            cell_fills: 0,
            corner_evals: 0,
            noise_corner_batches: 0,
            slot_hits: 0,
            slot_misses: 0,
            slot_misses_by_slot: [0; MAX_TRACKED_SLOTS],
            palette_intern_new: 0,
            palette_intern_hit: 0,
            pre_ore_computed: 0,
            pre_ore_hits: 0,
            post_ore_computed: 0,
            post_ore_hits: 0,
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

    use super::{MAX_TRACKED_SLOTS, STAGE_COUNT, Snapshot, Stage};

    const KINDS: usize = crate::density::Density::KIND_COUNT;

    struct Counters {
        block_at: AtomicU64,
        density_evals: [AtomicU64; KINDS],
        density_point_computes: [AtomicU64; KINDS],
        corner_lookups: AtomicU64,
        cell_fills: AtomicU64,
        corner_evals: AtomicU64,
        noise_corner_batches: AtomicU64,
        slot_hits: AtomicU64,
        slot_misses: AtomicU64,
        slot_misses_by_slot: [AtomicU64; MAX_TRACKED_SLOTS],
        palette_intern_new: AtomicU64,
        palette_intern_hit: AtomicU64,
        pre_ore_computed: AtomicU64,
        pre_ore_hits: AtomicU64,
        post_ore_computed: AtomicU64,
        post_ore_hits: AtomicU64,
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
    }

    static C: Counters = Counters {
        block_at: AtomicU64::new(0),
        density_evals: [const { AtomicU64::new(0) }; KINDS],
        density_point_computes: [const { AtomicU64::new(0) }; KINDS],
        corner_lookups: AtomicU64::new(0),
        cell_fills: AtomicU64::new(0),
        corner_evals: AtomicU64::new(0),
        noise_corner_batches: AtomicU64::new(0),
        slot_hits: AtomicU64::new(0),
        slot_misses: AtomicU64::new(0),
        slot_misses_by_slot: [const { AtomicU64::new(0) }; MAX_TRACKED_SLOTS],
        palette_intern_new: AtomicU64::new(0),
        palette_intern_hit: AtomicU64::new(0),
        pre_ore_computed: AtomicU64::new(0),
        pre_ore_hits: AtomicU64::new(0),
        post_ore_computed: AtomicU64::new(0),
        post_ore_hits: AtomicU64::new(0),
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
    pub fn bump_post_ore(computed: bool) {
        bump(if computed { &C.post_ore_computed } else { &C.post_ore_hits });
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

    /// Enters `stage` on this thread; the previous tag is restored on drop.
    #[derive(Debug)]
    pub struct StageGuard(Stage);

    impl StageGuard {
        #[inline]
        pub fn enter(stage: Stage) -> Self {
            bump(&C.stage_entered[stage as usize]);
            Self(STAGE.replace(stage))
        }
    }

    impl Drop for StageGuard {
        #[inline]
        fn drop(&mut self) {
            STAGE.set(self.0);
        }
    }

    pub fn reset() {
        C.block_at.store(0, Relaxed);
        for a in &C.density_evals {
            a.store(0, Relaxed);
        }
        for a in &C.density_point_computes {
            a.store(0, Relaxed);
        }
        C.corner_lookups.store(0, Relaxed);
        C.cell_fills.store(0, Relaxed);
        C.corner_evals.store(0, Relaxed);
        C.noise_corner_batches.store(0, Relaxed);
        C.slot_hits.store(0, Relaxed);
        C.slot_misses.store(0, Relaxed);
        for a in &C.slot_misses_by_slot {
            a.store(0, Relaxed);
        }
        C.palette_intern_new.store(0, Relaxed);
        C.palette_intern_hit.store(0, Relaxed);
        C.pre_ore_computed.store(0, Relaxed);
        C.pre_ore_hits.store(0, Relaxed);
        C.post_ore_computed.store(0, Relaxed);
        C.post_ore_hits.store(0, Relaxed);
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
    }

    pub fn snapshot() -> Snapshot {
        Snapshot {
            block_at: C.block_at.load(Relaxed),
            density_evals: std::array::from_fn(|i| C.density_evals[i].load(Relaxed)),
            density_point_computes: std::array::from_fn(|i| {
                C.density_point_computes[i].load(Relaxed)
            }),
            corner_lookups: C.corner_lookups.load(Relaxed),
            cell_fills: C.cell_fills.load(Relaxed),
            corner_evals: C.corner_evals.load(Relaxed),
            noise_corner_batches: C.noise_corner_batches.load(Relaxed),
            slot_hits: C.slot_hits.load(Relaxed),
            slot_misses: C.slot_misses.load(Relaxed),
            slot_misses_by_slot: std::array::from_fn(|i| C.slot_misses_by_slot[i].load(Relaxed)),
            palette_intern_new: C.palette_intern_new.load(Relaxed),
            palette_intern_hit: C.palette_intern_hit.load(Relaxed),
            pre_ore_computed: C.pre_ore_computed.load(Relaxed),
            pre_ore_hits: C.pre_ore_hits.load(Relaxed),
            post_ore_computed: C.post_ore_computed.load(Relaxed),
            post_ore_hits: C.post_ore_hits.load(Relaxed),
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
        }
    }
}

#[cfg(not(feature = "gen-counters"))]
mod imp {
    use super::{Snapshot, Stage};

    #[inline(always)]
    pub fn current_stage() -> Stage {
        Stage::Other
    }
    #[inline(always)]
    pub fn bump_block_at() {}
    #[inline(always)]
    pub fn bump_density_eval(_kind_index: usize) {}
    #[inline(always)]
    pub fn bump_density_point_compute(_kind_index: usize) {}
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
    pub fn bump_post_ore(_computed: bool) {}
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

    /// Zero-sized in the default build: `StageGuard::enter` compiles to nothing.
    #[derive(Debug)]
    pub struct StageGuard;

    impl StageGuard {
        #[inline(always)]
        pub fn enter(_stage: Stage) -> Self {
            Self
        }
    }

    #[inline(always)]
    pub fn reset() {}

    #[inline(always)]
    pub fn snapshot() -> Snapshot {
        Snapshot::default()
    }
}

pub use imp::{
    StageGuard, bump_biome_search, bump_block_at, bump_cell_fill, bump_corner_eval,
    bump_corner_lookup, bump_density_eval,
    bump_density_point_compute, bump_noise_corner_batch, bump_palette_intern_hit,
    bump_palette_intern_new, bump_post_ore,
    bump_pre_ore, bump_rng_draw, bump_slot_hit, bump_slot_miss, bump_state_intern_new,
    bump_state_name_lookup, bump_stitch_cells, bump_string_allocs, bump_structure_aquifer,
    bump_structure_height_probe, bump_structure_start, current_stage, reset, snapshot,
};

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
    }

    /// With the feature off every hook must be callable and every read zero —
    /// the property that lets hook sites drop their `#[cfg]`s.
    #[cfg(not(feature = "gen-counters"))]
    #[test]
    fn hooks_are_inert_without_the_feature() {
        assert!(!enabled());
        reset();
        bump_block_at();
        bump_rng_draw();
        bump_stitch_cells(1_000);
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
        bump_rng_draw();
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
        assert_eq!(s.rng_draws[Stage::Other as usize], 1);
        assert_eq!(s.rng_draws[Stage::Vegetation as usize], 3);
        assert_eq!(s.rng_draws[Stage::Shape as usize], 1);
        assert_eq!(s.stage_entered[Stage::Vegetation as usize], 1);
        assert_eq!(s.stage_entered[Stage::Shape as usize], 1);

        reset();
        assert_eq!(snapshot(), Snapshot::default());
    }
}
