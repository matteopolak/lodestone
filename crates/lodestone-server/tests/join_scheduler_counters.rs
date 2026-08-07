//! Unit 10's acceptance criterion in **stage counters**: driving the 289-column
//! join burst through `join_scheduler`'s window, with the per-ring barrier gone,
//! must compute each neighbour stage exactly once — the same 441/361 Unit 6
//! measured, exactly, with no run-to-run variance.
//!
//! # Why this cannot be measured serially, and why that matters here specifically
//!
//! Unit 6's central methodological finding: a **serial** sweep cannot distinguish
//! the old FIFO memo caches from the staged store, because serially a cache never
//! has a racing miss. Both read identical stage counts. The defect only appeared
//! under a 289-column burst:
//!
//! | arm | pre-ore (true 441) | post-ore (true 361) |
//! |---|---|---|
//! | old cache (`4be59556`) | 452, 452, 448 *(varying)* | 380, 383, 372 |
//! | new store (`34202a21`) | **441, 441, 441** | **361, 361, 361** |
//!
//! That is exactly the reasoning trap that produced `4307b59`: measure barrier
//! removal serially and it looks free. So this gate's subject arm is the concurrent
//! burst, driven through the production scheduler rather than through hand-rolled
//! threads — `tests/staged_store_gates.rs` already covers the hand-rolled shape,
//! and the question here is whether the *scheduler* preserves the store's
//! once-only property once nothing waits at a ring boundary.
//!
//! # Its own binary, and the rule that goes with it
//!
//! `lodestone_worldgen::counters` are **process-global atomics**. The first version
//! of Unit 6's counter gate shared a binary with generating tests and read
//! `pre_ore_computed = 502` against a true 256 — a 96% over-count that looks
//! exactly like a broken store. So, as in `staged_store_counters.rs`: **nothing in
//! this binary may generate terrain except [`measure`]**, which runs all three arms
//! sequentially inside one `OnceLock` so the counters can be reset between them. A
//! `OnceLock` around each arm separately would not be enough — it serialises
//! readers, not generators.
//!
//! Counters-on inflates this burst ~3× (Unit 6 measured 130–149 s against
//! 40–55 s), so this is **not** a timing gate and nothing here reports a duration.
//!
//! # Running it
//!
//! ```text
//! cargo test --release -p lodestone-server -p lodestone-worldgen \
//!   --features lodestone-worldgen/gen-counters \
//!   --test join_scheduler_counters -- --ignored --nocapture
//! ```
//!
//! Without `gen-counters` the counter arm cannot fire, so every test forks on
//! `counters::enabled()` and asserts the store's own entry count instead of
//! skipping — an instrument-independent upper bound on stage computations, plus a
//! check that the hooks really are inert so a zero cannot be mistaken for a pass.

use std::sync::{Arc, OnceLock};

use lodestone_server::join_scheduler::{ColumnPipeline, generation_window};
use lodestone_server::overworld_chunk_source;
use lodestone_worldgen::counters::{self, Snapshot};

/// The burst named in `4307b59` — *"cache contention with 289 concurrent generator
/// calls"*. 17×17 at `view_radius = 8`.
const BURST_RADIUS: i32 = 8;
const BURST_COLUMNS: usize = 289;

/// Derived from the drivers, not from a measurement. `post_ore_world(X)` is needed
/// over `C ± 1` for each of the 289 centres, so post-ore is reached across a 19×19;
/// `pre_ore_stage` is needed over `X ± 1` of each of those, so pre-ore is reached
/// across a 21×21. `STORE_RETENTION = 512` is derived from this 441.
const BURST_PRE_ORE_CLOSURE: usize = 21 * 21;
const BURST_POST_ORE_CLOSURE: usize = 19 * 19;

/// The control's view: 3×3, so the two arms differ by a factor rather than by a
/// handful and the comparison cannot be mistaken for noise.
const CONTROL_RADIUS: i32 = 1;
const CONTROL_COLUMNS: usize = 9;
/// Same derivation as above, one radius in: 7×7 and 5×5.
const CONTROL_PRE_ORE_CLOSURE: usize = 7 * 7;
const CONTROL_POST_ORE_CLOSURE: usize = 5 * 5;
/// **The control's expectation.** With a fresh generator per column no store entry
/// is ever shared, so every column pays its own full 5×5 pre-ore closure and 3×3
/// post-ore closure. `9 × 25` and `9 × 9`.
const UNSHARED_PRE_ORE: usize = CONTROL_COLUMNS * 25;
const UNSHARED_POST_ORE: usize = CONTROL_COLUMNS * 9;

/// The wire order for a view of `radius` centred on `(ox, oz)` — rings outward,
/// `dz`-outer/`dx`-inner within a ring, mirroring `server.rs`'s private
/// `join_view_rings`. The scheduler is handed exactly this.
fn wire_order(radius: i32, ox: i32, oz: i32) -> Vec<(i32, i32)> {
    (0..=radius)
        .flat_map(|r| {
            let mut ring = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        ring.push((dx + ox, dz + oz));
                    }
                }
            }
            ring
        })
        .collect()
}

struct ArmCounters {
    counters: Snapshot,
    store_len: usize,
    evictions: usize,
    emitted_in_order: bool,
    columns: usize,
}

struct Measurement {
    counters_live: bool,
    window: usize,
    /// The subject: 289 columns, one shared generator, through the scheduler.
    burst: ArmCounters,
    /// A 3×3 through the scheduler over one shared generator — the control's
    /// like-for-like partner, so the two numbers differ only in whether the store
    /// is shared.
    shared_small: ArmCounters,
    /// **The negative control**: the same 3×3, but a *fresh generator per column*,
    /// so the store's dependency edges are severed.
    unshared_small: ArmCounters,
}

/// Drives one arm through the production scheduler over **one shared generator**,
/// and snapshots the counters.
///
/// One generator is the whole point: the store is a field on it, so sharing it is
/// what makes the dependency edges exist at all. [`drive_unshared`] is the same
/// view with that sharing removed.
fn drive_shared(
    runtime: &tokio::runtime::Runtime,
    coords: Vec<(i32, i32)>,
    window: usize,
) -> ArmCounters {
    let source = Arc::new(overworld_chunk_source(42));
    counters::reset();
    let expected = coords.clone();
    let emitted = runtime.block_on(async {
        let mut pipeline = ColumnPipeline::with_window(Arc::clone(&source), coords, window);
        let mut emitted = Vec::new();
        while let Some((pos, _column)) = pipeline.next().await {
            emitted.push(pos);
        }
        emitted
    });
    ArmCounters {
        counters: counters::snapshot(),
        store_len: source.generator().store_len(),
        evictions: source.generator().store_evictions(),
        emitted_in_order: emitted == expected,
        columns: emitted.len(),
    }
}

/// The severed-edge arm: a fresh generator per column, so nothing is memoised
/// across columns and every shared neighbour is recomputed.
fn drive_unshared(coords: &[(i32, i32)]) -> ArmCounters {
    counters::reset();
    let mut last_len = 0usize;
    let mut evictions = 0usize;
    for &(cx, cz) in coords {
        let source = overworld_chunk_source(42);
        let _column = lodestone_server::ChunkSource::column(&source, cx, cz);
        last_len = source.generator().store_len();
        evictions += source.generator().store_evictions();
    }
    ArmCounters {
        counters: counters::snapshot(),
        store_len: last_len,
        evictions,
        emitted_in_order: true,
        columns: coords.len(),
    }
}

/// The **only** thing in this binary that may generate terrain. See the module doc.
fn measure() -> &'static Measurement {
    static ONCE: OnceLock<Measurement> = OnceLock::new();
    ONCE.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            // Two worker threads, not the default one-per-core: generation happens
            // on the blocking pool, and four sibling agents share this machine.
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a multi-thread runtime, the flavour production's blocking pool needs");
        let window = generation_window();

        // Three views, none overlapping, so the arms stay independent even though
        // they run in one process. 289 at (700, 700), the controls far away.
        let burst = drive_shared(&runtime, wire_order(BURST_RADIUS, 700, 700), window);
        let small = wire_order(CONTROL_RADIUS, -900, 1300);
        let shared_small = drive_shared(&runtime, small.clone(), window);
        let unshared_small = drive_unshared(&small);

        let m = Measurement {
            counters_live: counters::enabled(),
            window,
            burst,
            shared_small,
            unshared_small,
        };
        eprintln!(
            "[U10] window={} counters_live={}\n\
             [U10] burst   {} cols: pre_ore computed={} hits={} | post_ore computed={} hits={} \
             | store_len={} evictions={}\n\
             [U10] shared  {} cols: pre_ore computed={} | post_ore computed={}\n\
             [U10] unshared(control) {} cols: pre_ore computed={} | post_ore computed={}",
            m.window,
            m.counters_live,
            m.burst.columns,
            m.burst.counters.pre_ore_computed,
            m.burst.counters.pre_ore_hits,
            m.burst.counters.post_ore_computed,
            m.burst.counters.post_ore_hits,
            m.burst.store_len,
            m.burst.evictions,
            m.shared_small.columns,
            m.shared_small.counters.pre_ore_computed,
            m.shared_small.counters.post_ore_computed,
            m.unshared_small.columns,
            m.unshared_small.counters.pre_ore_computed,
            m.unshared_small.counters.post_ore_computed,
        );
        m
    })
}

/// **Unit 10's acceptance criterion.** With the per-ring barrier deleted, the
/// 289-column burst still computes each neighbour stage exactly once — Unit 6's
/// 441/361, exactly, not approximately.
///
/// A regression here is a *rollback*, not a tolerance to widen: the old cache's
/// signature was over-computation **plus run-to-run variance** (452/452/448), so
/// any number other than 441 means the scheduler reintroduced a racing miss the
/// store cannot absorb.
#[test]
#[ignore = "289 columns of real embedded-data generation with counters on; minutes in release"]
fn the_barrier_free_289_column_burst_computes_each_stage_exactly_once() {
    let m = measure();

    assert_eq!(m.burst.columns, BURST_COLUMNS);
    assert!(
        m.burst.emitted_in_order,
        "the scheduler emitted out of wire order, which no counter below would notice"
    );

    // Instrument-independent, both arms: the store holds exactly the pre-ore
    // closure. More means a key aliased or a closure radius is wrong; fewer means
    // something was evicted and every count below is inflated.
    assert_eq!(
        m.burst.store_len, BURST_PRE_ORE_CLOSURE,
        "the store should hold exactly the burst's 21x21 pre-ore closure \
         ({BURST_PRE_ORE_CLOSURE})"
    );
    assert_eq!(
        m.burst.evictions, 0,
        "STORE_RETENTION (512) is derived from this burst's 441-chunk closure precisely so \
         nothing can be evicted mid-burst; an eviction would inflate every count below"
    );

    if m.counters_live {
        assert_eq!(
            m.burst.counters.pre_ore_computed as usize, BURST_PRE_ORE_CLOSURE,
            "pre-ore must run once per chunk in the 21x21 closure. Unit 6 measured 441 exactly, \
             3 of 3, and the old FIFO cache measured 452/452/448 — anything but 441 here is the \
             racing miss back, and blocks the landing"
        );
        assert_eq!(
            m.burst.counters.post_ore_computed as usize, BURST_POST_ORE_CLOSURE,
            "the ore RNG walk must run once per chunk in the 19x19 closure (Unit 6: 361 exactly, \
             3 of 3; old cache 380/383/372)"
        );

        // Non-vacuity floor, derived from the drivers rather than from this run:
        // each of the 289 centres asks `pre_ore_stage` for itself plus each source
        // in its 3x3 — 10 minimum — so the burst performs >= 2,890 lookups against
        // 441 computations. Without this, "computed == 441" would also hold for a
        // burst in which no chunk was ever shared, which is the only case the
        // store, and this scheduler, exist for.
        let floor = (BURST_COLUMNS * 10) as u64 - BURST_PRE_ORE_CLOSURE as u64;
        assert!(
            m.burst.counters.pre_ore_hits >= floor,
            "only {} pre-ore hits against {} computations (floor {floor}) — no neighbour was \
             ever shared, so the equality above proves nothing about the dependency edges",
            m.burst.counters.pre_ore_hits,
            m.burst.counters.pre_ore_computed
        );
    } else {
        assert_eq!(
            m.burst.counters,
            Snapshot::default(),
            "built without `gen-counters`, so every hook must be provably inert — otherwise a \
             zero in the counters-on arm could not be told apart from an instrument that never \
             fires"
        );
    }
}

/// **The negative control for the dependency edge, and it must fail the assertion
/// above's shape.**
///
/// The scheduler's correctness under a deleted barrier rests entirely on one
/// property: two workers that need the same chunk's same stage **join on one
/// computation** through the store's per-entry `OnceLock`, rather than each running
/// it. That is the dependency edge, and it is the thing the barrier used to stand
/// in for.
///
/// Severing it — a fresh generator per column, so no entry is ever shared — must
/// make the stage counters blow up. Measured at a 3×3, the shared arm computes the
/// 7×7 pre-ore closure (49) and the severed arm computes 9 × 25 = 225: a 4.6×
/// over-computation. If this control ever reports the *same* number as the shared
/// arm, then the counters cannot see a scheduler that stopped honouring the edges,
/// and the acceptance criterion above is vacuous.
///
/// The two arms share a view size and a seed, so nothing but store sharing differs.
#[test]
#[ignore = "reads the same multi-minute measurement"]
fn control_severing_the_store_dependency_edge_over_computes_every_stage() {
    let m = measure();
    assert_eq!(m.shared_small.columns, CONTROL_COLUMNS);
    assert_eq!(m.unshared_small.columns, CONTROL_COLUMNS);

    // Instrument-independent half: the shared arm's store holds the whole closure,
    // the severed arm's last generator holds only one column's own 5x5.
    assert_eq!(
        m.shared_small.store_len, CONTROL_PRE_ORE_CLOSURE,
        "the shared 3x3 must reach a 7x7 pre-ore closure"
    );
    assert_eq!(
        m.unshared_small.store_len, 25,
        "a per-column generator can only ever hold that column's own 5x5 pre-ore closure; \
         reading anything else means the arms are not actually differing in store sharing"
    );

    if m.counters_live {
        assert_eq!(
            m.shared_small.counters.pre_ore_computed as usize, CONTROL_PRE_ORE_CLOSURE,
            "the shared arm is the same once-only property as the burst, one radius in"
        );
        assert_eq!(
            m.shared_small.counters.post_ore_computed as usize, CONTROL_POST_ORE_CLOSURE
        );
        assert_eq!(
            m.unshared_small.counters.pre_ore_computed as usize, UNSHARED_PRE_ORE,
            "with the dependency edges severed each of the {CONTROL_COLUMNS} columns must pay \
             its own full 5x5 pre-ore closure ({UNSHARED_PRE_ORE} total). Reading \
             {CONTROL_PRE_ORE_CLOSURE} instead would mean the arms are indistinguishable and \
             the acceptance gate cannot see the regression it exists to catch"
        );
        assert_eq!(
            m.unshared_small.counters.post_ore_computed as usize, UNSHARED_POST_ORE
        );
        assert!(
            m.unshared_small.counters.pre_ore_computed
                > m.shared_small.counters.pre_ore_computed * 4,
            "the control must over-compute by a factor, not by a handful: {} against {}",
            m.unshared_small.counters.pre_ore_computed,
            m.shared_small.counters.pre_ore_computed
        );
    }
}

/// The *world* control: the embedded resolver really drives both 3×3
/// neighbourhoods, so the numbers above are measuring the pipeline this unit
/// scheduled rather than a pipeline whose neighbour stages early-returned.
///
/// `lodestone-worldgen`'s own fixture resolvers supply no biome documents, so
/// `ores_by_biome`/`vegetation_by_biome` come out empty, both 3×3 drivers
/// early-return, and a gate written against them would generate 289 columns, touch
/// zero neighbours, and pass every equality above with the closure collapsed to
/// 289. The closure size is the observable that separates the two — which is why
/// the data has to come from `lodestone-server`'s embedded set.
#[test]
#[ignore = "reads the same multi-minute measurement"]
fn the_burst_actually_drives_both_neighbourhoods() {
    let m = measure();
    assert!(
        m.burst.store_len > BURST_COLUMNS,
        "the store holds {} entries for a {BURST_COLUMNS}-column burst — no neighbour was ever \
         requested, so the drivers early-returned and every number in this binary is vacuous",
        m.burst.store_len
    );
    if m.counters_live {
        assert!(
            m.burst.counters.post_ore_computed as usize > BURST_COLUMNS,
            "post-ore ran for only {} chunks; the 3x3 vegetation driver did not run",
            m.burst.counters.post_ore_computed
        );
    }
}

/// The window really was open during the burst — otherwise every count above is
/// the *serial* measurement Unit 6 proved cannot distinguish anything, and this
/// whole binary would be the reasoning that produced `4307b59`.
#[test]
#[ignore = "reads the same multi-minute measurement"]
fn the_burst_was_actually_concurrent() {
    let m = measure();
    assert!(
        m.window >= 2,
        "a window of {} is serial, and a serial burst cannot distinguish a racing miss from \
         its absence — the defect 4307b59 was reverted for is invisible at width 1",
        m.window
    );
    assert!(
        m.window < BURST_COLUMNS,
        "a window of {} spans the whole {BURST_COLUMNS}-column view, which is 5104adf's \
         unbounded fan-out rather than a scheduler",
        m.window
    );
}
