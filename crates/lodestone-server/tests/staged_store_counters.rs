//! Unit 6's acceptance criterion, in counters: every neighbour stage is computed
//! **exactly once** per chunk it is reached for over a 12×12 sweep.
//!
//! # Its own binary, and the measurement that forced it
//!
//! `lodestone_worldgen::counters` are **process-global atomics**, so any other
//! test in the same binary that generates terrain lands in this one's window. That
//! is not hypothetical: the first version of this gate shared a binary with the
//! byte-identity and join-burst tests (now in `staged_store_gates.rs`), ran under
//! `--test-threads=2`, and read `pre_ore_computed = 502` against a true value of
//! **256** — a 96% over-count that looks exactly like a broken store. Wrapping the
//! sweep in a `OnceLock` (as this file still does) was *not* sufficient and the
//! comment claiming it was, was wrong: the `OnceLock` serialises the tests that
//! read *it*, and does nothing about tests that call `column()` themselves.
//!
//! So the rule for this binary is stronger than "use a `OnceLock`": **nothing
//! here may generate terrain except [`sweep`]**. Adding a test that calls
//! `column()` directly silently corrupts every number below.
//!
//! # Running it
//!
//! `#[ignore]`d because it is a multi-minute release-profile sweep — 144 columns
//! of full embedded-data generation, the same scale as
//! `worldgen_data.rs`'s own badlands census. The always-on protection for the
//! store's once-only property is `overworld::store`'s own unit tests (which
//! include a negative control showing the *old* cache shape recomputing under the
//! same race); this is the end-to-end confirmation on real data.
//!
//! ```text
//! cargo test --release -p lodestone-server -p lodestone-worldgen \
//!   --features lodestone-worldgen/gen-counters \
//!   --test staged_store_counters -- --ignored --nocapture
//! ```
//!
//! Without `gen-counters` the counter arm cannot fire, so the test forks on
//! `counters::enabled()` and asserts a *different*, instrument-independent
//! property instead of skipping — see the test's own doc comment.
//!
//! # Why the data has to come from `lodestone-server`
//!
//! `lodestone-worldgen`'s own fixture resolvers supply density functions and noise
//! but **no biome documents**, so `ores_by_biome`/`vegetation_by_biome` come out
//! empty, both 3×3 drivers early-return, and a stage-computation gate written
//! against them would sweep 144 chunks, touch zero neighbours, and pass. That is
//! the *world* species of vacuous test — the flaw is in the input, invisible in the
//! test source — so [`the_sweep_actually_drives_both_neighbourhoods`] asserts the
//! drivers ran before any number here is believed.
//!
//! # The arithmetic, derived from the drivers rather than from a measurement
//!
//! For a 12×12 sweep of centres `(0..12, 0..12)`:
//!
//! * `vegetation_stage(C)` needs `post_ore_world` over `C ± 1`, so post-ore is
//!   reached across `-1..=12` — **14×14 = 196** chunks.
//! * `post_ore_world(X)` runs `ore_stage(X)`, which needs `pre_ore_stage` over
//!   `X ± 1`, so pre-ore is reached across `-2..=13` — **16×16 = 256** chunks.
//!
//! **144 × 2 = 288 is the wrong reading** of the plan's "chunks × stages": each
//! stage has its own closure radius, so the chunk count differs per stage. The
//! invariant is one computation per *distinct chunk reached at that stage*.

use lodestone_server::overworld_generator;
use lodestone_worldgen::counters::{self, Snapshot};
use std::sync::OnceLock;

/// Sweep extent — the 12×12 `docs/plans/worldgen-rewrite.md` names for Unit 6.
const SWEEP: i32 = 12;

/// Distinct chunks the sweep reaches at the pre-ore stage: `-2..=13` squared.
const PRE_ORE_CLOSURE: usize = 16 * 16;
/// Distinct chunks the sweep reaches at the post-ore stage: `-1..=12` squared.
const POST_ORE_CLOSURE: usize = 14 * 14;
/// Distinct chunks the sweep reaches at the **structure-starts** stage, and
/// therefore the store's real entry count: `-10..=21` squared.
///
/// This is the widest stage, not the pre-ore one, because `pre_ore_stage` reads
/// `structure_refs_stage`, which walks
/// [`REFS_RADIUS`](lodestone_worldgen::overworld::structures::REFS_RADIUS) = 8 —
/// so a single column closes over 21×21 = 441 chunks rather than 25. Asserting
/// `PRE_ORE_CLOSURE` here was wrong from the moment structure placement landed:
/// the store legitimately holds the *structure* closure, and reading the
/// entry count against the narrower stage made a correct store look like it had
/// aliased a key.
const STRUCTURE_CLOSURE: usize = 32 * 32;

struct SweepResult {
    counters: Snapshot,
    counters_live: bool,
    store_len: usize,
    evictions: usize,
}

/// The **only** thing in this binary that may generate terrain. See the module doc.
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
            store_len: generator.store_len(),
            evictions: generator.store_evictions(),
        };
        eprintln!(
            "[U6] {SWEEP}x{SWEEP} sweep: pre_ore computed={} hits={} | post_ore computed={} hits={} \
             | store_len={} evictions={} | counters_live={}",
            result.counters.pre_ore_computed,
            result.counters.pre_ore_hits,
            result.counters.post_ore_computed,
            result.counters.post_ore_hits,
            result.store_len,
            result.evictions,
            result.counters_live,
        );
        result
    })
}

/// **Unit 6's acceptance criterion.** Each neighbour stage computed exactly once
/// per distinct chunk reached — never recomputed, never aliased.
///
/// Forked on `counters::enabled()` rather than skipped: a counter gate that
/// silently no-ops in a default build is the *precondition* species of vacuous
/// test. The counters-off arm is not a weaker restatement — it asserts the
/// **store's own entry count**, which bounds stage computations from above through
/// an instrument-independent path, and it asserts the counters really are inert so
/// a zero could never be mistaken for a pass.
#[test]
#[ignore = "multi-minute release-profile sweep of real embedded worldgen data"]
fn each_neighbour_stage_is_computed_exactly_once_over_a_12x12_sweep() {
    let s = sweep();

    // Both arms: entry count is not feature-gated. Exactly the pre-ore closure
    // means no chunk was ever entered under a second key — the anti-aliasing half
    // of the criterion, from a different source than the counters.
    assert_eq!(
        s.store_len, STRUCTURE_CLOSURE,
        "the store should hold exactly the sweep's 32x32 structure-starts closure \
         ({STRUCTURE_CLOSURE}) — the widest stage, since pre-ore reads structure refs over \
         REFS_RADIUS = 8; more means a key aliased or a closure radius is wrong, fewer means \
         something evicted"
    );
    assert_eq!(
        s.evictions, 0,
        "eviction during the sweep would inflate every count below, and STORE_RETENTION \
         is derived specifically so it cannot happen at this scale"
    );

    if s.counters_live {
        assert_eq!(
            s.counters.pre_ore_computed as usize, PRE_ORE_CLOSURE,
            "stages 1-4 must run once per chunk in the 16x16 pre-ore closure, not once per requester"
        );
        assert_eq!(
            s.counters.post_ore_computed as usize, POST_ORE_CLOSURE,
            "the ore RNG walk must run once per chunk in the 14x14 post-ore closure"
        );
        // Non-vacuity floor for the two equalities, derived from the drivers and
        // not from this run: each of the 144 centres calls `pre_ore_stage` at
        // least once for itself plus once per source in `features_for_source`'s
        // 3x3 — 10 minimum — so the sweep performs >= 1440 pre-ore lookups
        // against 256 computations. Without this floor, "computed == 256" would
        // also hold for a sweep in which no chunk was ever *shared*, which is the
        // only case the store exists for.
        let floor = (SWEEP * SWEEP * 10) as u64 - PRE_ORE_CLOSURE as u64;
        assert!(
            s.counters.pre_ore_hits >= floor,
            "only {} pre-ore hits against {} computations (floor {floor}) — the drivers \
             cannot have asked for shared neighbours, so the equality above proves nothing",
            s.counters.pre_ore_hits,
            s.counters.pre_ore_computed
        );
        assert!(
            s.counters.post_ore_hits >= (SWEEP * SWEEP) as u64,
            "only {} post-ore hits; the 3x3 vegetation driver must reuse neighbours",
            s.counters.post_ore_hits
        );
    } else {
        assert_eq!(
            s.counters,
            Snapshot::default(),
            "built without `gen-counters`, so every hook must be provably inert — otherwise \
             a zero in the counters-on arm could not be told apart from an instrument that \
             never fires"
        );
    }
}

/// The *world* control: the embedded resolver really drives both 3×3
/// neighbourhoods.
///
/// Without it, a resolver change that emptied `ores_by_biome` or
/// `vegetation_by_biome` would make both decoration stages early-return, collapse
/// the closure to the 144 centres, and leave the once-only assertions holding —
/// passing while measuring a pipeline with its neighbour stages missing. The
/// closure size is the observable that separates the two.
#[test]
#[ignore = "reads the same multi-minute sweep"]
fn the_sweep_actually_drives_both_neighbourhoods() {
    let s = sweep();
    assert!(
        s.store_len > (SWEEP * SWEEP) as usize,
        "store holds {} entries for a {SWEEP}x{SWEEP} sweep — no neighbour was ever \
         requested, so the drivers early-returned and every number in this binary is vacuous",
        s.store_len
    );
    if s.counters_live {
        assert!(
            s.counters.post_ore_computed as usize > (SWEEP * SWEEP) as usize,
            "post-ore ran for only {} chunks; the 3x3 vegetation driver did not run",
            s.counters.post_ore_computed
        );
    }
}
