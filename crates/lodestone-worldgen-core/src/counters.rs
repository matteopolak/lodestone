//! Structural counters for world generation: exact operation counts that a test
//! can predict and gate on, compiled out unless the `gen-counters` feature is on.
//!
//! # What it is
//!
//! A flat set of process-global relaxed atomics, incremented from a handful of
//! named hook points, **compiled out entirely unless the `gen-counters` cargo
//! feature is on**. Every hook is an `#[inline(always)]` function with an empty
//! body in the default build, so a build without the feature contains no counter
//! code and no `#[cfg]` appears at any call site.
//!
//! # Why counts rather than timings
//!
//! A counter is reproducible under machine load, predicts an exact value, and can
//! therefore *gate*; a duration on a shared machine is a sample. The rule that
//! follows: **a counter that cannot predict is a counter that cannot gate.** Every
//! counter here has at least one test that asserts a hand-derived value:
//!
//! | counter | gate |
//! |---|---|
//! | `rng_draws` | `lodestone-worldgen/tests/gen_counters_forward.rs` |
//! | `noise_*` | `tests/simd_kernel_counter.rs` |
//! | `full_column_*` | `lodestone-worldgen/tests/gen_counters_forward.rs` |
//! | `raw_window_*`, `heightmap_scan_cells` | `lodestone-server`'s `chunk` raw-window tests |
//! | `structure_candidate_cell_probes` | `lodestone-worldgen/tests/structure_origin_index.rs` |
//!
//! # How it works
//!
//! Counts are `AtomicU64`s with `Relaxed` ordering: they are only read after the
//! measured work has been joined, so no other memory is synchronised through
//! them. [`snapshot`] returns a plain-`u64` [`Snapshot`]; [`reset`] zeroes
//! everything. A measurement is `reset(); work(); snapshot()`.
//!
//! # How to change it
//!
//! Adding a counter is three edits: a name in the `counters!` invocation below, a
//! `bump_*` hook, and its call site — **plus a test asserting its predicted
//! value**. A counter nothing reads is dead code; delete it.
//!
//! Gotchas:
//!
//! * **Reset before reading.** These are process globals; an absolute reading
//!   mostly reports whatever warm-up happened to run. Each gate lives in its own
//!   test binary (or serialises its measurements) for the same reason.
//! * **Never wrap a hook call in `#[cfg(feature)]`.** Call it unconditionally;
//!   it is empty when the feature is off. A `#[cfg]` at the call site is how a
//!   hook silently stops being called.
//!
//! # Configuration
//!
//! One cargo feature, `gen-counters`, default **off**. It is additive, so it
//! cannot change generated terrain, but it is slower, so a timing and a counter
//! measurement are two different runs.

macro_rules! counters {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        /// A point-in-time copy of every counter.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct Snapshot {
            $($(#[$doc])* pub $name: u64,)*
        }

        #[cfg(feature = "gen-counters")]
        mod cells {
            use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

            pub(super) struct Cells {
                $(pub(super) $name: AtomicU64,)*
            }

            pub(super) static C: Cells = Cells {
                $($name: AtomicU64::new(0),)*
            };

            pub(super) fn reset() {
                $(C.$name.store(0, Relaxed);)*
            }

            pub(super) fn snapshot() -> super::Snapshot {
                super::Snapshot {
                    $($name: C.$name.load(Relaxed),)*
                }
            }
        }
    };
}

counters! {
    /// Draws through `WorldgenRandom::next_bits`, the single RNG funnel.
    rng_draws,
    /// Eight-corner SIMD batches evaluated by improved noise.
    noise_corner_batches,
    /// Octave levels a Perlin sample visited.
    noise_active_visits,
    /// Octave levels a Perlin sample skipped because their amplitude is zero.
    noise_skipped_visits,
    /// Whole-column conversions into compact storage.
    full_column_conversions,
    /// Cells those conversions walked.
    full_column_conversion_cells,
    /// Source cells a raw-window column build read.
    raw_window_source_cells,
    /// Cells a raw-window build walked to derive heightmap and ticking summaries.
    raw_window_summary_cells,
    /// Sections a raw-window build packed in bulk.
    raw_window_bulk_sections,
    /// Cells a heightmap derivation scanned.
    heightmap_scan_cells,
    /// Cells a structure-start search probed for a candidate origin.
    structure_candidate_cell_probes,
}

/// Whether the hooks are live (the `gen-counters` feature is on).
#[inline(always)]
#[must_use]
pub const fn enabled() -> bool {
    cfg!(feature = "gen-counters")
}

/// Zeroes every counter. A no-op without the feature.
#[inline]
pub fn reset() {
    #[cfg(feature = "gen-counters")]
    cells::reset();
}

/// Reads every counter. All zero without the feature.
#[inline]
#[must_use]
pub fn snapshot() -> Snapshot {
    #[cfg(feature = "gen-counters")]
    return cells::snapshot();
    #[cfg(not(feature = "gen-counters"))]
    Snapshot::default()
}

macro_rules! hook {
    ($(#[$doc:meta])* fn $fn_name:ident($($arg:ident),*) { $($field:ident += $amount:expr;)* }) => {
        $(#[$doc])*
        #[inline(always)]
        #[allow(unused_variables)]
        pub fn $fn_name($($arg: u64),*) {
            #[cfg(feature = "gen-counters")]
            {
                use std::sync::atomic::Ordering::Relaxed;
                $(cells::C.$field.fetch_add($amount, Relaxed);)*
            }
        }
    };
}

hook! {
    /// One draw through the RNG funnel.
    fn bump_rng_draw() { rng_draws += 1; }
}
hook! {
    /// One eight-corner improved-noise batch.
    fn bump_noise_corner_batch() { noise_corner_batches += 1; }
}
hook! {
    /// `n` Perlin octave levels visited.
    fn bump_noise_active_visits(n) { noise_active_visits += n; }
}
hook! {
    /// `n` Perlin octave levels skipped.
    fn bump_noise_skipped_visits(n) { noise_skipped_visits += n; }
}
hook! {
    /// One whole-column conversion over `cells` cells.
    fn bump_full_column_conversion(cells) {
        full_column_conversions += 1;
        full_column_conversion_cells += cells;
    }
}
hook! {
    /// One raw-window column build.
    fn bump_raw_window(source_cells, summary_cells, sections) {
        raw_window_source_cells += source_cells;
        raw_window_summary_cells += summary_cells;
        raw_window_bulk_sections += sections;
    }
}
hook! {
    /// `cells` cells scanned deriving a heightmap.
    fn bump_heightmap_scan(cells) { heightmap_scan_cells += cells; }
}
hook! {
    /// One candidate cell probed by a structure-start search.
    fn bump_structure_candidate_cell_probe() { structure_candidate_cell_probes += 1; }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each hook lands on its own field and nowhere else, and `reset` clears.
    /// Without the feature the same calls must leave everything at zero.
    ///
    /// Only hooks nothing else in this crate calls are used, so tests running in
    /// parallel in this binary (RNG and noise) cannot perturb the reading.
    #[test]
    fn hooks_land_on_their_own_fields() {
        reset();
        bump_structure_candidate_cell_probe();
        bump_structure_candidate_cell_probe();
        bump_heightmap_scan(9);
        bump_full_column_conversion(256);
        bump_raw_window(3, 5, 7);
        let expected = if enabled() {
            Snapshot {
                structure_candidate_cell_probes: 2,
                heightmap_scan_cells: 9,
                full_column_conversions: 1,
                full_column_conversion_cells: 256,
                raw_window_source_cells: 3,
                raw_window_summary_cells: 5,
                raw_window_bulk_sections: 7,
                ..Snapshot::default()
            }
        } else {
            Snapshot::default()
        };
        assert_eq!(snapshot(), expected);
        reset();
        assert_eq!(snapshot(), Snapshot::default());
    }
}
