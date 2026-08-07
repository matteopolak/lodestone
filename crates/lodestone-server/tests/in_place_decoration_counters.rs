//! **Unit 7's acceptance criterion, in counters: a served column copies ZERO cells
//! to make its 3×3 neighbourhood addressable.**
//!
//! # What it is
//!
//! `docs/plans/worldgen-rewrite.md`'s diagnostic D2 says the decoration stages
//! re-copied ~2.8M cells per served column even when every cache hit, because the
//! only way the 3×3 neighbourhood was addressable was to stitch it into one grid.
//! `crate::counters::Counters::stitch_cells` is the counter that measured those
//! loops. Unit 7 deleted both of them
//! ([`lodestone_worldgen::feature::region_view::RegionView`] and
//! `VegGrid::with_sources` route reads to the source grid instead), so this reads
//! zero — and the plan expresses the criterion in this counter rather than in a
//! timing on purpose. The same instrument measured ±3% run to run with a **22%
//! swing on its vegetation stage** across three runs of one identical binary,
//! while an allocation counter read 905,459 to the digit three times of three.
//!
//! # Its own test binary, and the measurement that forced that
//!
//! These counters are **process-global atomics**, so any other test in the same
//! binary that generates terrain lands inside this one's window. Not hypothetical:
//! `staged_store_counters.rs`'s own header records the first version of Unit 6's
//! gate sharing a binary and reading `pre_ore_computed = 502` against a true 256 —
//! a 96% over-count that looked exactly like a broken store. So the rule for this
//! binary is the same as that one's, and stronger than "use a lock": **nothing here
//! may generate terrain except [`the_only_generation_in_this_binary`]**. Adding a
//! second test that calls `column()` silently corrupts every number below.
//!
//! # Running it
//!
//! ```text
//! cargo test --release -p lodestone-server -p lodestone-worldgen \
//!   --features lodestone-worldgen/gen-counters \
//!   --test in_place_decoration_counters -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d because it is real embedded-data generation. Without
//! `gen-counters` the counter arm structurally cannot fire, so it forks and asserts
//! an **instrument-independent** property instead of skipping — a skip would be the
//! *precondition* species of vacuous test.
//!
//! # Why the data must come from `lodestone-server`
//!
//! `lodestone-worldgen`'s own fixture resolvers supply density functions and noise
//! but **no biome documents**, so `ores_by_biome`/`vegetation_by_biome` come out
//! empty, both 3×3 drivers early-return before they would have stitched anything,
//! and a `stitch_cells == 0` gate written against them would pass by generating a
//! world with no decoration in it at all. That is the *world* species of vacuous
//! test — the flaw is in the input and invisible in the test source — which is why
//! the stage-entry assertions below run *before* the zero is believed.

use lodestone_worldgen::counters::{self, Stage};

const SEED: i64 = 42;

/// One cold column's pre-ore closure: `vegetation_stage` reads post-ore over the
/// 3×3, each of those runs `ore_stage` over *its* 3×3, so pre-ore spans 5×5.
const COLD_PRE_ORE_CHUNKS: usize = 25;
/// Ore RNG walks on a cold column: the 3×3 `post_ore_world` closure.
const COLD_ORE_WALKS: u64 = 9;
/// Sources each stitch used to copy, and cells per source (`256 * height`).
const STITCH_SOURCES: u64 = 9;
const CELLS_PER_SOURCE: u64 = 256 * 384;

#[test]
#[ignore = "real embedded-data generation; counter arm needs --features lodestone-worldgen/gen-counters"]
fn the_only_generation_in_this_binary() {
    let generator = lodestone_server::overworld_generator(SEED);
    counters::reset();
    let column = generator.column(0, 0);
    let snapshot = counters::snapshot();

    // The column is real terrain, not an empty grid — checked first, because every
    // "zero copies" reading below is trivially satisfied by generating nothing.
    assert!(
        column.non_air_count() > 10_000,
        "premise failed: chunk (0,0) produced only {} non-air blocks, so this is not a \
         real column and a zero cell-copy count means nothing",
        column.non_air_count(),
    );

    if !counters::enabled() {
        // Instrument-independent arm. `stitch_cells` cannot move in this build, so
        // asserting it is 0 would be vacuous; assert instead that the counters
        // really are off (so nobody reads the other arm's silence as a pass) and
        // that the store's own closure is the expected 5×5 with nothing evicted —
        // the property that holds with the instrument absent.
        assert_eq!(
            snapshot.block_at, 0,
            "counters are compiled out, so every hook must read 0; got block_at = {}",
            snapshot.block_at,
        );
        assert_eq!(
            generator.store_len(),
            COLD_PRE_ORE_CHUNKS,
            "one cold column should close over exactly {COLD_PRE_ORE_CHUNKS} chunks",
        );
        assert_eq!(generator.store_evictions(), 0, "nothing may be evicted");
        println!(
            "in-place decoration: counters OFF — asserted the store closure instead \
             ({} entries, 0 evictions). Re-run with --features \
             lodestone-worldgen/gen-counters for the stitch_cells arm.",
            generator.store_len(),
        );
        return;
    }

    // --- The world-vacuity guard: both drivers actually ran ----------------
    assert_eq!(
        snapshot.stage_entered[Stage::Ore as usize], COLD_ORE_WALKS,
        "the ore driver must have run {COLD_ORE_WALKS} times (the 3×3 post-ore closure) \
         before a zero stitch count means anything; got {}",
        snapshot.stage_entered[Stage::Ore as usize],
    );
    assert_eq!(
        snapshot.stage_entered[Stage::Vegetation as usize], 1,
        "the vegetation driver must have run once; got {}",
        snapshot.stage_entered[Stage::Vegetation as usize],
    );

    // --- The criterion, as two hypotheses far apart ------------------------
    //
    // Pre-U7: `ore_stage` stitched 9 sources per ore walk and `vegetation_stage`
    // stitched 9 once, each copying `256 * height` cells. Post-U7: exactly 0 —
    // not "small", because the counter is bumped from the stitch loops themselves
    // and both are deleted. A *partial* revert lands on neither and fails.
    let pre_u7 = (COLD_ORE_WALKS * STITCH_SOURCES + STITCH_SOURCES) * CELLS_PER_SOURCE;
    assert_eq!(
        pre_u7, 8_847_360,
        "arithmetic check on the pre-U7 hypothesis itself: (9 × 9 + 9) × 98,304"
    );
    assert_eq!(
        snapshot.stitch_cells, 0,
        "U7's acceptance criterion: a served column must copy ZERO cells to make its \
         3×3 neighbourhood addressable. The pre-U7 hypothesis for this same column is \
         {pre_u7}; got {}. Anything non-zero means a region stitch is back — grep \
         `bump_stitch_cells` for the caller.",
        snapshot.stitch_cells,
    );

    // --- Control for the absence claim above -------------------------------
    //
    // "Assertions of an absence need a control proving the detector works", and
    // `== 0` is the exact shape that reads as a pass when the instrument is dead.
    // Bumping by hand must move the live counter. Safe here: `snapshot` was already
    // taken, and this is the last thing the binary does.
    counters::bump_stitch_cells(7);
    assert_eq!(
        counters::snapshot().stitch_cells, 7,
        "control: the stitch_cells counter must be observed moving in this very build, \
         or the assertion above proves only that its hook is compiled out"
    );

    println!(
        "in-place decoration: stitch_cells = 0 (pre-U7 hypothesis {pre_u7}), \
         ore walks {}, vegetation 1, detector control passed",
        snapshot.stage_entered[Stage::Ore as usize],
    );
}
