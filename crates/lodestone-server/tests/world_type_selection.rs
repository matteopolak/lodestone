//! `world_type` was a hardcoded `OnceLock`, so `Amplified` and
//! `LargeBiomes` — whose `noise_settings` and density functions are already
//! bundled and byte-identical to the jar — were unreachable. This gate proves
//! [`WorldType`] is a real, effective parameter rather than a decoration:
//! selecting a different variant at the **same seed and the same column**
//! must produce terrain the default arm cannot, and the default arm itself
//! must still behave exactly as it did before the parameterisation.
//!
//! # Why these particular statistics, not "terrain differs"
//!
//! CLAUDE.md's *magnitude* species: "terrain differs" is satisfied by any bug
//! that also differs, so both gates below assert a specific, re-derived
//! number rather than an inequality, and both name what the default arm
//! actually produced at the same input so the assertion demonstrably rejects
//! it (not merely "is different from an unstated baseline").
//!
//! * **Amplified** changes height by construction (`noise_settings/
//!   amplified.json`'s `final_density` uses a `0.64` multiplier and its own
//!   `overworld_amplified/depth` density function in place of `overworld/
//!   depth`). Measured at seed 4242, chunk `(0, 0)`, local `(0, 0)`:
//!   [`overworld_generator`] (plain overworld) yields
//!   [`NORMAL_TOP_Y`] == 64 — sea-level, unremarkable. Re-running the
//!   *identical* seed and column through
//!   [`overworld_generator_of_type`]`(seed, WorldType::Amplified)` yields
//!   [`AMPLIFIED_TOP_Y`] == 130, a 66-block jump at a column the plain
//!   generator called flat. No plausible normal-overworld noise at this
//!   exact seed and column produces 130; the two calls differ only in which
//!   `WorldType` was passed.
//! * **LargeBiomes** changes biome *patch size*, not height (its
//!   `final_density` differs only slightly; the router's `temperature`/
//!   `vegetation` entries point at `noise/temperature_large`/
//!   `noise/vegetation_large` instead, zooming the climate noise). A single
//!   column's biome id is not a useful statistic — both arms could coincide
//!   by chance — so this gate instead counts biome *transitions* along a
//!   120-chunk strip at the same seed: [`NORMAL_BIOME_CHANGES`]-worth of
//!   changes (measured 20, across 12 distinct biomes) versus
//!   [`LARGE_BIOME_CHANGES`]-worth (measured 1, across 2 distinct biomes) —
//!   the same strip, the same seed, an order of magnitude apart. That is the
//!   statistic large-biomes worlds exist to produce and the plain generator
//!   structurally cannot: its own climate noise changes roughly six times as
//!   often over the identical distance.
//!
//! Both figures were re-derived by running the actual generator (see the
//! module history for the probe), not predicted, per CLAUDE.md's "do not
//! predict a plausible round number" — 130 and 64 are exactly what the two
//! calls emit, and 20/1 are exactly what the strip walk counted.
//!
//! # The overworld arm's own control
//!
//! [`overworld_generator`]/[`overworld_chunk_source`] still call
//! [`WorldType::Overworld`] internally and take no new parameter, so every
//! pre-existing gate over them (`chunk_memory.rs`, `overworld_gen.rs` in
//! `lodestone-worldgen`, the structure corpus, …) is itself the "byte-identical
//! before and after" detector CLAUDE.md's evidence standard asks for: any of
//! them going red would mean the refactor touched the default path, and none
//! did.

use lodestone_server::{WorldType, overworld_generator, overworld_generator_of_type};
use std::collections::HashSet;

/// Seed shared by both gates below — arbitrary, fixed so the measured
/// constants stay reproducible.
const SEED: i64 = 4242;

const NORMAL_TOP_Y: i32 = 64;
const AMPLIFIED_TOP_Y: i32 = 130;

const NORMAL_BIOME_CHANGES: usize = 20;
const NORMAL_BIOME_DISTINCT: usize = 12;
const LARGE_BIOME_CHANGES: usize = 1;
const LARGE_BIOME_DISTINCT: usize = 2;

/// Amplified must reach a height the plain overworld does not, at the exact
/// same seed and column — the discriminating statistic named in this file's
/// module doc.
#[test]
fn amplified_reaches_a_height_the_plain_overworld_does_not_at_the_same_column() {
    let normal = overworld_generator(SEED);
    let amplified = overworld_generator_of_type(SEED, WorldType::Amplified);

    let normal_y = normal.column(0, 0).top_non_air_y(0, 0);
    let amplified_y = amplified.column(0, 0).top_non_air_y(0, 0);

    assert_eq!(
        normal_y, NORMAL_TOP_Y,
        "plain overworld's own height at this column moved — the baseline this \
         gate compares against is stale, re-derive it rather than editing the \
         amplified assertion to match"
    );
    assert_eq!(
        amplified_y, AMPLIFIED_TOP_Y,
        "amplified's height at the same seed and column moved; if it now equals \
         {NORMAL_TOP_Y} (the plain overworld's own height), WorldType::Amplified \
         silently produced ordinary terrain — the exact failure mode this gate \
         exists to catch"
    );
    // The load-bearing assertion: amplified must clear the plain overworld's
    // own answer by a wide margin (66 blocks measured), not merely differ by
    // one block of floating-point noise.
    assert!(
        amplified_y - normal_y > 40,
        "amplified ({amplified_y}) is not decisively taller than plain overworld \
         ({normal_y}) at the same column — WorldType may not be reaching the \
         generator"
    );
}

/// Large-biomes' climate noise must change far less often than the plain
/// overworld's over an identical strip of columns at the same seed — the
/// biome-patch-size statistic large_biomes exists to produce, which a
/// height-only check cannot see (its `final_density` differs only slightly
/// from plain overworld's).
#[test]
fn large_biomes_biome_boundaries_are_far_sparser_than_plain_overworld_over_the_same_strip() {
    let normal = overworld_generator(SEED);
    let large = overworld_generator_of_type(SEED, WorldType::LargeBiomes);

    let (normal_changes, normal_distinct) = biome_transitions(&normal);
    let (large_changes, large_distinct) = biome_transitions(&large);

    assert_eq!(
        normal_changes, NORMAL_BIOME_CHANGES,
        "plain overworld's own biome-transition count over this strip moved — \
         re-derive the baseline rather than editing the large_biomes assertion"
    );
    assert_eq!(normal_distinct, NORMAL_BIOME_DISTINCT);
    assert_eq!(
        large_changes, LARGE_BIOME_CHANGES,
        "large_biomes' transition count over the same strip moved; if it now \
         matches plain overworld's {NORMAL_BIOME_CHANGES}, WorldType::LargeBiomes \
         silently produced ordinary-sized biomes — the exact failure mode this \
         gate exists to catch"
    );
    assert_eq!(large_distinct, LARGE_BIOME_DISTINCT);
    // The load-bearing assertion: large_biomes must be a real order of
    // magnitude sparser, not merely numerically different.
    assert!(
        normal_changes >= large_changes * 5,
        "large_biomes ({large_changes} changes) is not decisively sparser than \
         plain overworld ({normal_changes} changes) over the same 120-chunk \
         strip at the same seed"
    );
}

/// Walks `cx in 0..120` at `cz == 0`, sampling `biome_state(0, 0)` per chunk,
/// and returns `(transition count, distinct biome count)`.
fn biome_transitions(
    generator: &lodestone_server::OverworldGenerator,
) -> (usize, usize) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut changes = 0usize;
    let mut prev: Option<String> = None;
    for cx in 0..120 {
        let biome = generator.column(cx, 0).biome_state(0, 0).to_string();
        seen.insert(biome.clone());
        if let Some(p) = &prev {
            if *p != biome {
                changes += 1;
            }
        }
        prev = Some(biome);
    }
    (changes, seen.len())
}
