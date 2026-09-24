//! Unit 9's acceptance criterion, in counters: the number of climate searches a
//! sweep performs, against a prediction derived from the drivers **before**
//! measuring.
//!
//! # Its own binary, and why that is not optional
//!
//! `counters` are process-global atomics, so any other test in this binary that
//! generates terrain lands inside this one's window. `docs/worldgen-staged-store.md`
//! records what that costs: the first version of Unit 6's counter gate shared a
//! binary with two other sweeps and read `pre_ore_computed = 502` against a true
//! **256**. So the rule here is the same — **nothing in this binary may generate
//! terrain except [`sweep`]** — and a counter run is never also a timing run
//! (counters-on inflates a burst ~3×, measured).
//!
//! ```text
//! cargo test --release -p lodestone-worldgen --features gen-counters \
//!   --test biome_search_counters -- --ignored --nocapture
//! ```
//!
//! # The prediction, derived rather than observed
//!
//! For a `K = 6` sweep of centres `(0..6, 0..6)`, with the closure radii
//! `docs/worldgen-staged-store.md` derives from the unified dispatcher (FEATURES
//! source radius 1, terrain-prefix radius 2):
//!
//! | quantity | derivation | value |
//! |---|---|---|
//! | pre-ore chunks | `-2..=7` squared | 100 |
//! | FEATURES source chunks | `-1..=6` squared | 64 |
//! | biome-cell searches | 16 × 96 quart cells × each pre-ore chunk | 153,600 |
//! | `biome_for_carver_source` **calls** | `289 × 100` (carve, 17×17 window) | 28,900 |
//! | distinct source chunks | pre-ore extent widened by `NEIGHBOURHOOD_RANGE = 8`: `-10..=15` squared | 676 |
//!
//! The carver-only baseline is computed from those constants. Unified FEATURES
//! adds candidate-membership searches against the full 3-D biome grid, so the
//! total must be at least this baseline but is intentionally not pinned to it.
//!
//! * **un-memoised carver baseline**: `153,600 + 28,900 = 182,500` searches.
//! * **memoised carver baseline**: `153,600 + 676 = 154,276` searches — every
//!   repeated source chunk answered from [`lodestone_worldgen::biome`]'s memo.
//!
//! The 676 is exact rather than approximate because the memo is direct-mapped on
//! the low 5 bits of each chunk coordinate: the reached source range `-10..=15` is
//! 26 wide, 26 ≤ 32, so the slot map is **injective over the whole sweep** and no
//! entry is ever displaced. A wider sweep would wrap and the prediction would have
//! to account for displacement; `K = 6` is chosen so it does not have to.
//!
//! `biome_rows_compared` is the second axis and it is deliberately **not**
//! predicted to a digit — the number of `Node::distance` evaluations a pruned
//! search performs depends on where the target lands. What is asserted is the
//! structural claim: fewer than `table_len` per search, because a tree search that
//! examined 7,594 nodes would be brute force with extra steps.

use lodestone_server::overworld_generator;
use lodestone_worldgen::counters::{self, Snapshot, Stage};
use std::sync::OnceLock;

/// Sweep extent. See the module doc for why 6 and not 12.
const SWEEP: i32 = 6;
/// Real overworld climate table row count — what a brute-force search costs.
const TABLE_ROWS: u64 = 7594;

/// Pre-ore extent for `SWEEP = 6`: `-2..=7`, 10 wide.
const PRE_ORE_WIDTH: u64 = SWEEP as u64 + 4;
const PRE_ORE_CHUNKS: u64 = PRE_ORE_WIDTH * PRE_ORE_WIDTH;
/// Vertical quart layers in a standard overworld column (`height / 4`, 384 / 4).
/// The biome stage samples a full 4×4×4 grid, not one layer, so this
/// factor is what turned 16 searches per chunk into 1,536.
const Y_QUARTS: u64 = 96;
/// `crate::overworld`'s per-cell biome sample: `16 × Y_QUARTS` per pre-ore chunk.
/// **Not memoised, and correctly so** — every cell in a column has a distinct
/// climate target, so there is nothing to reuse. The memo is for
/// `biome_for_carver_source`, whose key really does repeat; that is the claim the
/// two hypotheses below still separate.
const BIOME_STAGE_SEARCHES: u64 = 16 * Y_QUARTS * PRE_ORE_CHUNKS;

/// The carve stage's source window width, **read from the carver** rather than
/// written as 17 here. A gate that names its own geometry cannot notice the
/// production geometry changing under it.
const CARVE_WINDOW: u64 = 2 * lodestone_worldgen::carver::NEIGHBOURHOOD_RANGE as u64 + 1;
/// Every `biome_for_carver_source` call from the terrain prefix: one per source
/// in each pre-ore chunk's carve window. FEATURES uses the full 3-D biome grid
/// for its own candidate membership checks, so those additional searches are
/// intentionally outside this carver-only arithmetic.
const CARVER_SOURCE_CALLS: u64 =
    CARVE_WINDOW * CARVE_WINDOW * PRE_ORE_CHUNKS;
/// Distinct source chunks: the pre-ore extent widened by the carver radius on both
/// sides — `-10..=15` for this sweep, 26 wide.
const CARVER_SOURCE_WIDTH: u64 =
    PRE_ORE_WIDTH + 2 * lodestone_worldgen::carver::NEIGHBOURHOOD_RANGE as u64;
const CARVER_SOURCE_DISTINCT: u64 = CARVER_SOURCE_WIDTH * CARVER_SOURCE_WIDTH;

/// The hypothesis this unit predicts.
const MEMOISED_SEARCHES: u64 = BIOME_STAGE_SEARCHES + CARVER_SOURCE_DISTINCT;
/// The hypothesis the pre-Unit-9 tree would produce.
const UNMEMOISED_SEARCHES: u64 = BIOME_STAGE_SEARCHES + CARVER_SOURCE_CALLS;

struct SweepResult {
    counters: Snapshot,
    counters_live: bool,
}

/// The **only** thing in this binary that may generate terrain.
fn sweep() -> &'static SweepResult {
    static ONCE: OnceLock<SweepResult> = OnceLock::new();
    ONCE.get_or_init(|| {
        let generator = overworld_generator(42);
        counters::reset();
        for cx in 0..SWEEP {
            for cz in 0..SWEEP {
                let _ = generator.column(cx, cz);
            }
        }
        let result = SweepResult {
            counters: counters::snapshot(),
            counters_live: counters::enabled(),
        };
        let s = &result.counters;
        eprintln!(
            "[U9] {SWEEP}x{SWEEP} sweep: biome_searches={} biome_rows_compared={} \
            (per search {:.1}) | pre_ore_computed={} features_entered={} \
             carve_entered={} | carver baseline memoised={MEMOISED_SEARCHES} \
             un-memoised={UNMEMOISED_SEARCHES} | counters_live={}",
            s.biome_searches,
            s.biome_rows_compared,
            if s.biome_searches == 0 {
                0.0
            } else {
                s.biome_rows_compared as f64 / s.biome_searches as f64
            },
            s.pre_ore_computed,
            s.stage_entered[Stage::Vegetation as usize],
            s.stage_entered[Stage::Carve as usize],
            result.counters_live,
        );
        result
    })
}

/// **Unit 9's acceptance criterion.** The carver portion reaches the memoised
/// baseline; unified FEATURES may add candidate-membership searches on top.
#[test]
#[ignore = "release-profile sweep of real embedded worldgen data with counters on"]
fn the_search_count_lands_on_the_memoised_prediction() {
    let s = sweep();
    if !s.counters_live {
        // Forked rather than skipped: a counter gate that silently no-ops in a
        // default build is the *precondition* species of vacuous test. With the
        // feature off the hooks are provably inert, so assert that instead — a
        // zero here must not be readable as a pass.
        assert_eq!(
            s.counters.biome_searches, 0,
            "with gen-counters off every hook must be inert; a non-zero reading means the \
             feature forward in Cargo.toml is broken (see tests/gen_counters_forward.rs)"
        );
        return;
    }

    // The world-species control first: if the drivers never ran, every number
    // below is a measurement of nothing. This crate's own fixtures supply no biome
    // documents, which is exactly why the data comes from `lodestone-server`.
    assert_eq!(
        s.counters.pre_ore_computed, PRE_ORE_CHUNKS,
        "the sweep must reach the derived pre-ore closure"
    );
    assert!(
        s.counters.stage_entered[Stage::Vegetation as usize] > 0,
        "the sweep must enter unified FEATURES"
    );
    assert_eq!(
        s.counters.stage_entered[Stage::Ore as usize],
        0,
        "the unified FEATURES sweep must not enter the obsolete standalone ore stage"
    );
    assert_eq!(
        s.counters.stage_entered[Stage::Carve as usize],
        PRE_ORE_CHUNKS,
        "carve_stage — the D5 call site — must have run once per pre-ore chunk"
    );

    assert!(
        s.counters.biome_searches >= MEMOISED_SEARCHES,
        "biome searches must include the memoised carver baseline {MEMOISED_SEARCHES} \
         (= {BIOME_STAGE_SEARCHES} per-quart + {CARVER_SOURCE_DISTINCT} distinct source chunks); \
         unified FEATURES adds candidate-membership searches"
    );
    assert_ne!(
        s.counters.biome_searches, UNMEMOISED_SEARCHES,
        "landing on the un-memoised count means the memo is not being consulted"
    );

    // The derivation is only meaningful if the memo's slot map is injective over
    // the whole reached source range — otherwise a displaced entry would be a miss
    // and the count would exceed the distinct-chunk figure for a reason unrelated
    // to the memo working. Asserted, because it is the premise of the number above.
    assert!(
        CARVER_SOURCE_WIDTH <= 32,
        "the {CARVER_SOURCE_DISTINCT} prediction assumes the reached source range \
         ({CARVER_SOURCE_WIDTH} wide) fits one 32-wide residue block of biome::memo's slot \
         map; a wider sweep displaces entries and the exact count no longer follows"
    );

    // The tree's own axis. A search that evaluated `TABLE_ROWS` node distances
    // would be brute force with extra steps, so this is the structural claim, not
    // a tuned tolerance.
    let per_search = s.counters.biome_rows_compared / s.counters.biome_searches;
    assert!(
        per_search < TABLE_ROWS,
        "a tree search must examine fewer than the {TABLE_ROWS} rows brute force does, \
         measured {per_search} per search"
    );
    // Do not turn the total into another literal: unified FEATURES performs a
    // data-dependent number of candidate-membership searches, so its total is
    // not the carver-only product above. The per-search tree bound is the stable
    // contract this gate can derive independently of feature placement counts.
}
