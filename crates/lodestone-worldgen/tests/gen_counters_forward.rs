//! The `gen-counters` feature still reaches the hooks after Unit 16's crate split.
//!
//! `counters.rs` moved to `lodestone-worldgen-core`, so the
//! `#[cfg(feature = "gen-counters")]` that picks live hooks over inert ones is
//! now evaluated in a *different crate* from the one callers pass the flag to.
//! `lodestone-worldgen`'s `gen-counters` is a forward
//! (`= ["lodestone-worldgen-core/gen-counters"]`), and a forward that is dropped
//! or misspelled fails **silently and in the safe-looking direction**: the parent
//! compiles, the flag is accepted, `cfg!(feature = "gen-counters")` in this
//! crate's own bench still reads true, and every counter reads 0. Every
//! acceptance criterion in `docs/plans/worldgen-rewrite.md` that is expressed as
//! a counter would then be vacuous — the assertion species, passing because its
//! subject is always zero.
//!
//! So this is the control on the instrument's *plumbing*, one level below
//! `counters.rs`'s own `hooks_count_and_stage_tag_restores` (which proves the
//! hooks work when the core crate has the feature, but cannot see whether the
//! parent's flag got there). Both states are asserted, because only the pair
//! distinguishes "the forward works" from "the counter is always non-zero".
//!
//! Deliberately its own integration binary: the counters are process-global
//! atomics, so sharing a binary with anything else that generates terrain would
//! make `reset()`/`snapshot()` racy.
//!
//! Note every path below goes through `lodestone_worldgen::…`, never
//! `lodestone_worldgen_core::…`. That is the point — it exercises the re-export
//! and the feature forward together, which is exactly the pair of things the
//! split introduced.

use lodestone_worldgen::counters::{self, Snapshot, Stage};
use lodestone_worldgen::rng::{RandomSource, WorldgenRandom, XoroshiroRandomSource};

/// Draws `n` values through the production RNG funnel — `WorldgenRandom::next_bits`,
/// the single site that calls `bump_rng_draw`.
///
/// Uses a real generator rather than calling `bump_rng_draw` directly on purpose:
/// a direct `bump` would prove the feature reached `counters.rs` but not that the
/// *hook site* in the moved `rng` module still resolves to it.
fn draw(n: u32) {
    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0x5EED));
    for _ in 0..n {
        let _ = random.next_int();
    }
    assert_eq!(random.count(), n, "next_int must route through next_bits once each");
}

/// With the feature forwarded, a real RNG draw must be *counted*, and counted the
/// exact number of times it was drawn.
///
/// The expected value is predicted, not merely asserted non-zero: 7 draws inside
/// a `Vegetation` stage guard and 4 outside it must land as 4 in `Other` and 7 in
/// `Vegetation`, totalling 11. A magnitude-species test ("more than zero") would
/// pass against a hook that fired once, or against one that fired on every
/// backend primitive and double-counted.
#[cfg(feature = "gen-counters")]
#[test]
fn forwarded_feature_actually_counts_through_the_re_export() {
    assert!(
        counters::enabled(),
        "lodestone-worldgen was built with `gen-counters`, so the forward to \
         lodestone-worldgen-core must have activated it; `enabled()` reading false \
         means the feature entry is no longer forwarding"
    );

    counters::reset();
    draw(4);
    {
        let _veg = counters::StageGuard::enter(Stage::Vegetation);
        draw(7);
        assert_eq!(counters::current_stage(), Stage::Vegetation);
    }
    assert_eq!(counters::current_stage(), Stage::Other);

    let snapshot = counters::snapshot();
    assert_eq!(
        snapshot.rng_draws[Stage::Other as usize], 4,
        "draws outside a guard attribute to Other"
    );
    assert_eq!(
        snapshot.rng_draws[Stage::Vegetation as usize], 7,
        "draws inside the guard attribute to Vegetation"
    );
    assert_eq!(snapshot.rng_draws_total(), 11, "and nothing double-counted");

    counters::reset();
    assert_eq!(
        counters::snapshot(),
        Snapshot::default(),
        "reset must clear, or a later gate in the same process inherits this one's draws"
    );
}

/// Without the feature, the same draws must be *un*counted.
///
/// This is the half that makes the assertion above evidence rather than a
/// tautology: it proves the counter can read zero, so an 11 is the feature
/// working and not a constant.
#[cfg(not(feature = "gen-counters"))]
#[test]
fn hooks_stay_inert_without_the_feature() {
    assert!(
        !counters::enabled(),
        "built without `gen-counters`, so nothing may have enabled it transitively — \
         a dependency turning it on would silently slow every shipped build"
    );

    counters::reset();
    draw(4);
    {
        let _veg = counters::StageGuard::enter(Stage::Vegetation);
        draw(7);
    }

    assert_eq!(
        counters::snapshot(),
        Snapshot::default(),
        "every hook must be inert without the feature — this is what lets hook \
         sites drop their #[cfg]s"
    );
}
