//! Per-stage duration **percentiles** over a batch of columns.
//!
//! # What it is
//!
//! [`overworld::OverworldGenerator::column_timed`] already splits one
//! column's generation into the ten [`overworld::StageTimes`] buckets
//! (aquifer/shape/biome/surface/materialize/carve/ore/vegetation/top_layer/
//! intern) — but it returns one sample. This module runs it over many
//! columns and reports the **tail**, not the mean, matching this
//! workspace's own rule that a mean can hide the one window that actually
//! mattered (a keep-alive timeout was once diagnosed from an average that
//! did exactly that — see `lodestone-server`'s `tick.rs`, which carries the
//! tick-loop side of this same instrumentation pass). [`profile_columns`]
//! returns [`StageDistribution`]: p50/p95/p99/max per stage, plus which
//! single stage-and-column pair was the single worst sample in the batch.
//!
//! # How it works
//!
//! [`aggregate_stage_samples`] is the pure half: given a slice of
//! `((cx, cz), StageTimes)` pairs it has no part in producing, it sorts each
//! stage's collected durations and computes a nearest-rank percentile
//! (`ceil(p * n)`-th smallest, 1-indexed). [`profile_columns`] is the thin
//! I/O half: it calls [`OverworldGenerator::column_timed`] once per
//! requested `(cx, cz)`, discards the generated column, and hands the ten
//! stage durations to `aggregate_stage_samples`. Splitting the two is what
//! lets this module's own tests exercise the percentile math with
//! hand-built `StageTimes` values and a hand-derived expected answer,
//! without needing a real generator, a `Resolver` fixture, or any wall-clock
//! timing of their own — see the `#[cfg(test)]` module below.
//!
//! Nothing here is memoised across calls — `column_timed` itself
//! deliberately bypasses [`OverworldGenerator`]'s own caches (see its own
//! doc comment for why), so every requested column pays the full,
//! cache-cold cost, on purpose: a per-stage split taken over memoised calls
//! would attribute ~0% to whichever stage happened to be warm.
//!
//! # Validating the instrument
//!
//! A duration-only instrument cannot tell "the loop only ran once" from
//! "the loop ran ten times and nine were free" — both look like one small
//! number. [`profile_columns_with_counter_check`] (behind the `gen-counters`
//! feature) cross-checks against `lodestone_worldgen_core::counters`, an
//! independent, already-tested instrument: `Stage::Intern`'s
//! `StageGuard::enter` runs from exactly one call site
//! (`OverworldGenerator::intern_from_dense`), reached exactly once per
//! top-level `column`/`column_timed` call and never from the neighbour-chunk
//! recursion inside `ore_stage`/`vegetation_stage` — so after `reset()`,
//! profiling `N` columns must leave `stage_entered[Stage::Intern]` at
//! exactly `N`. Disagreement would mean the aggregation loop skipped,
//! doubled, or deduplicated a coordinate, not that generation itself is
//! wrong. This is a real cross-instrument check rather than a
//! self-referential one: the counter and this module share no code path
//! other than `column_timed` itself.
//!
//! # How to change it
//!
//! Add a stage by widening [`WORLDGEN_STAGE_NAMES`] and [`stage_micros`]
//! together — they must stay the same length and order as
//! [`overworld::StageTimes`]'s own fields, exactly the guard
//! `lodestone-server`'s `tick.rs` `TICK_PHASE_NAMES` carries for
//! `TickPhase` and `lodestone-worldgen-core`'s `STAGE_NAMES` carries for
//! [`Stage`]: a report joining an index back to a label silently mislabels
//! every row past the first drift.
//!
//! [`Stage`]: lodestone_worldgen_core::counters::Stage
//!
//! # Configuration
//!
//! None beyond the existing `gen-counters` feature this module borrows for
//! its own cross-check — see that feature's own doc in
//! `lodestone-worldgen-core::counters` for its cost (additional atomics on
//! the hot path; off by default, and this module works without it, just
//! without the cross-check).
//!
//! # Dependencies
//!
//! [`OverworldGenerator::column_timed`], and (behind `gen-counters`)
//! `lodestone_worldgen_core::counters`. Native-only, like `column_timed`
//! itself: wall-clock timing has no meaning under wasm, and
//! `lodestone_time::Instant::now()` panics on bare `wasm32-unknown-unknown`
//! (this module does no timing of its own — it only aggregates
//! `column_timed`'s already-`#[cfg(not(target_arch = "wasm32"))]` output).
//! `crates/lodestone-worldgen/tests/profile_columns_report.rs` is a small
//! integration test that runs this over a real, fixture-backed generator
//! and prints the report with `--nocapture`. The profiling guidance in
//! `docs/tick-scheduling.md` explains why its cache-cold fixture scene must
//! be named when interpreting that output.

#![cfg(not(target_arch = "wasm32"))]

use crate::overworld::{OverworldGenerator, StageTimes};

/// [`StageTimes`]'s ten field names, in field declaration order — the same
/// order and the same first ten entries as
/// `lodestone_worldgen_core::counters::STAGE_NAMES`, kept as a separate,
/// shorter array here because `StageTimes` has no `Structure`/`Other`
/// fields to name.
pub const WORLDGEN_STAGE_NAMES: [&str; 10] =
    ["aquifer", "shape", "biome", "surface", "materialize", "carve", "ore", "vegetation", "top_layer", "intern"];

/// `StageTimes`'s ten `Duration` fields, read out in the same order as
/// [`WORLDGEN_STAGE_NAMES`], in whole microseconds.
fn stage_micros(t: &StageTimes) -> [u64; 10] {
    [
        t.aquifer.as_micros() as u64,
        t.shape.as_micros() as u64,
        t.biome.as_micros() as u64,
        t.surface.as_micros() as u64,
        t.materialize.as_micros() as u64,
        t.carve.as_micros() as u64,
        t.ore.as_micros() as u64,
        t.vegetation.as_micros() as u64,
        t.top_layer.as_micros() as u64,
        t.intern.as_micros() as u64,
    ]
}

/// The nearest-rank percentile (`ceil(p * n)`-th smallest, 1-indexed) of an
/// already-sorted slice. `p` in `[0.0, 1.0]`. Returns 0 for an empty slice —
/// a stage nothing ever profiled reads as "no data", not as a panic.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[rank]
}

/// One stage's percentile summary over an [`aggregate_stage_samples`] batch.
#[derive(Debug, Clone, Copy)]
pub struct StagePercentiles {
    pub stage: &'static str,
    pub sample_count: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    /// Sum of every sample for this stage, across the whole batch — what
    /// [`StageDistribution::dominant_stage`] ranks by. `u128` because a
    /// large batch's total is not bounded the way one sample is.
    pub total_us: u128,
}

/// The single largest stage duration observed anywhere in a batch, and
/// which stage *and which column* it was — the worldgen-side "worst
/// unserviced window, named" this instrumentation pass's brief asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorstStageWindow {
    pub stage: &'static str,
    pub micros: u64,
    pub cx: i32,
    pub cz: i32,
}

/// A batch's full report: per-stage percentiles plus the single worst
/// sample.
#[derive(Debug, Clone)]
pub struct StageDistribution {
    pub per_stage: [StagePercentiles; 10],
    pub columns_profiled: usize,
    /// `None` only if the batch was empty.
    pub worst: Option<WorstStageWindow>,
}

impl StageDistribution {
    /// The stage with the largest **cumulative** cost across the batch —
    /// "which part to improve on", ranked the way a profile normally is
    /// (by total time spent), not by any single sample's peak.
    ///
    /// # Panics
    ///
    /// If `per_stage` is somehow empty, which cannot happen through
    /// [`aggregate_stage_samples`] or [`profile_columns`] (both always
    /// produce exactly 10 entries).
    #[must_use]
    pub fn dominant_stage(&self) -> &StagePercentiles {
        self.per_stage
            .iter()
            .max_by_key(|s| s.total_us)
            .expect("StageDistribution::per_stage is always non-empty")
    }
}

/// The pure aggregation half of this module — see the module doc for why it
/// is split from [`profile_columns`]. Takes ownership of nothing it did not
/// compute: every `((cx, cz), StageTimes)` pair is opaque input, so this
/// function can be (and is, in `#[cfg(test)]` below) driven from
/// hand-constructed `StageTimes` values with a hand-derived expected
/// answer, with no generator and no `Resolver` fixture involved.
#[must_use]
pub fn aggregate_stage_samples(samples: &[((i32, i32), StageTimes)]) -> StageDistribution {
    let mut per_stage_samples: [Vec<u64>; 10] = std::array::from_fn(|_| Vec::with_capacity(samples.len()));
    let mut totals: [u128; 10] = [0; 10];
    let mut worst: Option<WorstStageWindow> = None;

    for &((cx, cz), ref times) in samples {
        let micros = stage_micros(times);
        for (i, &m) in micros.iter().enumerate() {
            per_stage_samples[i].push(m);
            totals[i] += u128::from(m);
            if worst.is_none_or(|w| m > w.micros) {
                worst = Some(WorstStageWindow { stage: WORLDGEN_STAGE_NAMES[i], micros: m, cx, cz });
            }
        }
    }

    let per_stage = std::array::from_fn(|i| {
        let mut sorted = per_stage_samples[i].clone();
        sorted.sort_unstable();
        StagePercentiles {
            stage: WORLDGEN_STAGE_NAMES[i],
            sample_count: sorted.len(),
            p50_us: percentile(&sorted, 0.50),
            p95_us: percentile(&sorted, 0.95),
            p99_us: percentile(&sorted, 0.99),
            max_us: sorted.last().copied().unwrap_or(0),
            total_us: totals[i],
        }
    });

    StageDistribution { per_stage, columns_profiled: samples.len(), worst }
}

/// Profiles every `(cx, cz)` in `coords` through
/// [`OverworldGenerator::column_timed`] and returns the per-stage
/// percentile distribution — the I/O half of this module; see its own doc
/// for what it does and does not measure (cache-cold cost, deliberately),
/// and see [`profile_columns_with_counter_check`] for validating that the
/// loop itself ran the expected number of times.
///
/// `coords` is walked in order and nothing is deduplicated: profiling the
/// same `(cx, cz)` twice profiles it twice, on purpose — a caller wanting
/// distinct columns is responsible for passing distinct coordinates.
#[must_use]
pub fn profile_columns(generator: &OverworldGenerator, coords: &[(i32, i32)]) -> StageDistribution {
    let samples: Vec<((i32, i32), StageTimes)> = coords
        .iter()
        .map(|&(cx, cz)| {
            let (_column, times) = generator.column_timed(cx, cz);
            ((cx, cz), times)
        })
        .collect();
    aggregate_stage_samples(&samples)
}

/// [`profile_columns`], plus a [`crate::counters::Snapshot`] taken around
/// the same batch after a `reset()` — the validation control described in
/// this module's own doc. Only meaningful with the `gen-counters` feature
/// on; the snapshot reads all zeros otherwise (see
/// [`crate::counters::enabled`]), which is why this is feature-gated rather
/// than silently returning zeros to a caller who forgot the feature.
#[cfg(feature = "gen-counters")]
#[must_use]
pub fn profile_columns_with_counter_check(
    generator: &OverworldGenerator,
    coords: &[(i32, i32)],
) -> (StageDistribution, crate::counters::Snapshot) {
    crate::counters::reset();
    let distribution = profile_columns(generator, coords);
    let snapshot = crate::counters::snapshot();
    (distribution, snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// `StageTimes` with every field set to the same `Duration` — a
    /// convenience for building synthetic samples below, not a claim about
    /// any real column.
    fn uniform(d: Duration) -> StageTimes {
        StageTimes {
            aquifer: d,
            shape: d,
            biome: d,
            surface: d,
            materialize: d,
            carve: d,
            ore: d,
            vegetation: d,
            top_layer: d,
            intern: d,
        }
    }

    /// A `StagePercentiles` for a stage nothing profiled reads as empty,
    /// not as a panic or a stale read of a neighbouring array slot — same
    /// control shape as `lodestone-server`'s `tick.rs`
    /// `phase_stats_reports_the_hand_derived_percentiles_for_known_samples`.
    #[test]
    fn percentile_of_empty_slice_is_zero_not_a_panic() {
        assert_eq!(percentile(&[], 0.50), 0);
        assert_eq!(percentile(&[], 0.99), 0);
    }

    /// Predicted value, hand-derived from the nearest-rank formula, not
    /// read back from the function under test: for ten ascending samples
    /// `1..=10`, `p50` is the 5th smallest (`ceil(0.50*10)=5`) and `p95`/
    /// `p99` both land on the 10th (`ceil(9.5)=10`, `ceil(9.9)=10`) because
    /// ten samples cannot resolve either percentile finer than the max.
    #[test]
    fn percentile_matches_the_hand_derived_nearest_rank_value() {
        let sorted: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&sorted, 0.50), 5);
        assert_eq!(percentile(&sorted, 0.95), 10);
        assert_eq!(percentile(&sorted, 0.99), 10);
    }

    /// [`WORLDGEN_STAGE_NAMES`] must stay the same length and order as
    /// [`StageTimes`]'s own ten fields — see [`stage_micros`], the single
    /// point that would need to change together with it.
    #[test]
    fn worldgen_stage_names_has_exactly_ten_entries() {
        assert_eq!(WORLDGEN_STAGE_NAMES.len(), 10);
        assert_eq!(WORLDGEN_STAGE_NAMES[0], "aquifer");
        assert_eq!(WORLDGEN_STAGE_NAMES[9], "intern");
    }

    /// Three hand-built columns, each stage set to a distinct constant
    /// across columns (`shape` at 1/2/3ms, say), so every percentile and
    /// the dominant-stage ranking is predictable by hand — no generator,
    /// no fixture, no timing of any kind. `shape` is given the largest
    /// values on purpose, so `dominant_stage()` has a known correct answer
    /// to be checked against, and the worst single sample is placed on
    /// `shape` at chunk `(9, -1)` so `worst` has a known correct answer too.
    #[test]
    fn aggregate_reports_hand_derived_percentiles_dominant_stage_and_worst_window() {
        let mut samples = Vec::new();
        for (i, &(cx, cz)) in [(0, 0), (1, 0), (9, -1)].iter().enumerate() {
            let base_ms = (i as u64 + 1) * 10; // 10, 20, 30
            let mut times = uniform(Duration::from_micros(base_ms));
            // `shape` dominates: 100x every other stage's value at each column.
            times.shape = Duration::from_micros(base_ms * 100);
            samples.push(((cx, cz), times));
        }
        let distribution = aggregate_stage_samples(&samples);

        assert_eq!(distribution.columns_profiled, 3);
        let shape = distribution.per_stage[WORLDGEN_STAGE_NAMES.iter().position(|&s| s == "shape").unwrap()];
        // Samples are 1000, 2000, 3000 micros; nearest-rank p50 of 3
        // ascending samples is the 2nd smallest (`ceil(0.50*3)=2`).
        assert_eq!(shape.sample_count, 3);
        assert_eq!(shape.p50_us, 2000);
        assert_eq!(shape.max_us, 3000);
        assert_eq!(shape.total_us, 1000 + 2000 + 3000);

        assert_eq!(distribution.dominant_stage().stage, "shape");

        let worst = distribution.worst.expect("non-empty batch");
        assert_eq!(worst.stage, "shape");
        assert_eq!(worst.micros, 3000);
        assert_eq!(worst.cx, 9);
        assert_eq!(worst.cz, -1);
    }

    /// A batch of size zero must not panic and must report an empty,
    /// well-formed distribution (every stage present with zero samples) —
    /// the negative-input control for the two tests above.
    #[test]
    fn aggregate_of_an_empty_batch_is_well_formed_and_empty() {
        let distribution = aggregate_stage_samples(&[]);
        assert_eq!(distribution.columns_profiled, 0);
        assert!(distribution.worst.is_none());
        for stage in &distribution.per_stage {
            assert_eq!(stage.sample_count, 0);
            assert_eq!(stage.max_us, 0);
            assert_eq!(stage.total_us, 0);
        }
    }
}
