//! Event counters for the ore placement engine — the instrument DESIGN.md
//! §12.143 says has to exist before the `ore` stage (38.7% of a steady-state
//! column, never profiled) can be attacked.
//!
//! # What it is
//!
//! Thread-local `u64`s bumped at the places `OreFeature.doPlace` and the driver
//! around it spend their time: source passes, emitted feature positions, blobs,
//! spheres, candidate positions, [`super::region_view::RegionView`] reads, target
//! tests, air probes and writes. Reported per interior column by
//! `tests/ore_stage_profile.rs`.
//!
//! **A count, not a duration** (DESIGN.md §12.19), and **not a back-derived unit
//! cost** (§12.143's 40× miss): the counts here go *with* an instructions-retired
//! measurement of a change, never instead of one.
//!
//! # How it works
//!
//! Every hook is `#[inline(always)]` and compiles to nothing at all unless the
//! `gen-counters` cargo feature is on — the same convention, and the same reason,
//! as [`lodestone_worldgen_core::counters`]: these sit in the innermost loop of
//! the whole generator, so a production build must not carry even a thread-local
//! increment. That crate's `Snapshot` is not extended instead because these
//! counters belong to the ore engine and live in the crate that owns it.
//!
//! Thread-local rather than atomic for the same reason: 289 concurrent generator
//! calls sharing one atomic is the cache contention `4307b59` is the scar for.
//! A probe is therefore per-thread and its consumers are single-threaded sweeps.
//!
//! # How to change it
//!
//! Add the field to [`Snapshot`], a row in [`Snapshot::rows`], a cell and `bump_*`
//! in the `gen-counters` module, and a no-op `bump_*` in the other one. The
//! [`Snapshot::rows`] row is what stops a counter being collected and never
//! printed.
//!
//! # Configuration
//!
//! `gen-counters`, default off. Nothing else.

/// One thread's ore-engine event counts since [`reset`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// `apply_one_source` calls — nine per `ore_stage`, one per source chunk.
    pub source_passes: u64,
    /// `place_ore_feature` calls: one per emitted position of every ore of every
    /// source, i.e. after the placement modifiers have run.
    pub features: u64,
    /// `place_ore_feature` calls that returned at the `proceed` heightmap probe
    /// without placing a blob.
    pub features_culled: u64,
    /// Columns visited by that `proceed` probe — a box up to 27×27 per feature.
    pub height_probes: u64,
    /// `do_place` calls — one blob each.
    pub blobs: u64,
    /// Spheres summed over every blob (`config.size` per blob).
    pub spheres: u64,
    /// Positions reaching `try_place_ore` — the innermost loop's real trip count.
    pub candidates: u64,
    /// Positions the `VisitedBox` had already seen.
    pub candidates_deduped: u64,
    /// `RegionView::get` calls out of the ore engine (`try_place_ore`'s own read
    /// plus `is_adjacent_to_air`'s six).
    pub region_reads: u64,
    /// Of those, reads answered by the write overlay rather than a source grid —
    /// the ones that cost a `StateInterner::name_of` read guard.
    pub region_reads_overlay: u64,
    /// `RuleTest` evaluations — one per target of every candidate, until one
    /// matches.
    pub target_tests: u64,
    /// Of those, the `TagMatch` ones: one `ore_tag_map` lookup plus one member-set
    /// lookup, each hashing a string.
    pub target_tests_tag: u64,
    /// `is_adjacent_to_air` calls — one per *matching* target of a candidate.
    pub air_checks: u64,
    /// Cells written through the view by ore placement.
    pub writes: u64,
}

impl Snapshot {
    /// Every counter, in report order. Named here rather than at the call site so
    /// a counter that is collected but never printed cannot exist.
    #[must_use]
    pub fn rows(&self) -> [(&'static str, u64); 14] {
        [
            ("source_passes", self.source_passes),
            ("features", self.features),
            ("features_culled", self.features_culled),
            ("height_probes", self.height_probes),
            ("blobs", self.blobs),
            ("spheres", self.spheres),
            ("candidates", self.candidates),
            ("candidates_deduped", self.candidates_deduped),
            ("region_reads", self.region_reads),
            ("region_reads_overlay", self.region_reads_overlay),
            ("target_tests", self.target_tests),
            ("target_tests_tag", self.target_tests_tag),
            ("air_checks", self.air_checks),
            ("writes", self.writes),
        ]
    }
}

#[cfg(feature = "gen-counters")]
mod imp {
    use std::cell::Cell;

    use super::Snapshot;

    thread_local! {
        static C: Cell<Snapshot> = const { Cell::new(Snapshot {
            source_passes: 0,
            features: 0,
            features_culled: 0,
            height_probes: 0,
            blobs: 0,
            spheres: 0,
            candidates: 0,
            candidates_deduped: 0,
            region_reads: 0,
            region_reads_overlay: 0,
            target_tests: 0,
            target_tests_tag: 0,
            air_checks: 0,
            writes: 0,
        }) };
    }

    /// One `Cell<Snapshot>` rather than fourteen `Cell<u64>`s: a `Snapshot` is
    /// `Copy`, so this is a read-modify-write of a 112-byte struct in TLS, which
    /// the optimiser folds to a single field update at each call site. Fourteen
    /// separate `thread_local!`s would each carry their own lazy-init check.
    macro_rules! bumps {
        ($($bump:ident => $field:ident),* $(,)?) => {
            $(
                #[inline(always)]
                pub fn $bump(n: u64) {
                    let _ = C.try_with(|c| {
                        let mut s = c.get();
                        s.$field = s.$field.wrapping_add(n);
                        c.set(s);
                    });
                }
            )*
        };
    }

    bumps! {
        bump_source_pass => source_passes,
        bump_feature => features,
        bump_feature_culled => features_culled,
        bump_height_probes => height_probes,
        bump_blob => blobs,
        bump_spheres => spheres,
        bump_candidate => candidates,
        bump_candidate_deduped => candidates_deduped,
        bump_region_read => region_reads,
        bump_region_read_overlay => region_reads_overlay,
        bump_target_test => target_tests,
        bump_target_test_tag => target_tests_tag,
        bump_air_check => air_checks,
        bump_write => writes,
    }

    /// Zeroes every counter on this thread.
    pub fn reset() {
        let _ = C.try_with(|c| c.set(Snapshot::default()));
    }

    /// This thread's counts since [`reset`].
    #[must_use]
    pub fn snapshot() -> Snapshot {
        C.try_with(Cell::get).unwrap_or_default()
    }
}

#[cfg(not(feature = "gen-counters"))]
mod imp {
    use super::Snapshot;

    macro_rules! noop {
        ($($bump:ident),* $(,)?) => {
            $(
                #[inline(always)]
                pub fn $bump(_n: u64) {}
            )*
        };
    }

    noop! {
        bump_source_pass,
        bump_feature,
        bump_feature_culled,
        bump_height_probes,
        bump_blob,
        bump_spheres,
        bump_candidate,
        bump_candidate_deduped,
        bump_region_read,
        bump_region_read_overlay,
        bump_target_test,
        bump_target_test_tag,
        bump_air_check,
        bump_write,
    }

    /// Inert without `gen-counters`.
    pub fn reset() {}

    /// All-zero without `gen-counters`. `tests/ore_stage_profile.rs` **fails** on
    /// the zero rather than reporting it, so a feature-less run cannot look like a
    /// measurement.
    #[must_use]
    pub fn snapshot() -> Snapshot {
        Snapshot::default()
    }
}

pub use imp::{
    bump_air_check, bump_blob, bump_candidate, bump_candidate_deduped, bump_feature,
    bump_feature_culled, bump_height_probes, bump_region_read, bump_region_read_overlay,
    bump_source_pass, bump_spheres, bump_target_test, bump_target_test_tag, bump_write, reset,
    snapshot,
};
