//! Stage 1 of `docs/plans/progressive-chunk-generation.md`: the generator seam
//! itself. Nothing here is production code — these are the two gates the plan
//! specifies for `OverworldGenerator::column_shaped`.
//!
//! # What it is
//!
//! Two gates, each with its own executed control (this repo's evidence
//! standard: "run the control and observe it fail; do not describe what it
//! would do"):
//!
//! 1. **Byte identity** (`stage1_shaped_then_full_is_byte_identical_to_cold_full`):
//!    `column_shaped(cx, cz)` then `column(cx, cz)` on one generator must produce
//!    a `Full` column byte-identical to `column(cx, cz)` cold on an independent
//!    generator. This is the property an "upgrade" rests on — `column_shaped` is
//!    a pure prefix of `column`, not a second pipeline. Its control mutates one
//!    block id in a clone of the matching side and asserts the comparison stops
//!    agreeing, proving the equality check can actually detect a diff rather
//!    than passing by construction.
//! 2. **Stage-touch counters** (`stage1_shaped_sweep_touches_only_pre_ore_and_structure_stages`,
//!    `--features gen-counters` only): a shaped sweep bumps `pre_ore_computed`
//!    and `structure_starts_computed` and leaves `post_ore_computed` and the
//!    vegetation stage-entry counter at zero. Its control
//!    (`stage1_control_full_sweep_trips_the_post_ore_and_vegetation_counters`)
//!    runs the identical sweep shape through plain `column()` and asserts those
//!    same two counters are **not** zero — the counter gate's hypothesis,
//!    observed failing against the thing it exists to distinguish `column_shaped`
//!    from.
//!
//! A third, non-gate check (`stage1_shaped_column_contains_a_real_structure`)
//! answers the report question the plan's Stage 1 write-up asks for directly:
//! whether a Shaped column really does contain structure pieces (it should —
//! `structure_place_stage` runs inside `pre_ore_stage_uncached`'s carve step,
//! strictly before `column_shaped`'s cutoff). Its own control checks a nearby
//! chunk with no structure start placed in it and confirms the same
//! structure-telltale block set is absent there, so the detector is shown to
//! discriminate rather than being a permissive tautology.
//!
//! # How it works
//!
//! Terrain/structure selection follows `stage0_shaped_vs_full_cost.rs`'s own
//! pattern (read that file first — its module doc explains why): a cheap
//! `biome_at_quart`/`structure_starts_placed_in` search over a widening area,
//! then a cross-check against the real materialized result before any timed or
//! asserted measurement trusts the coordinate. Every test in this file keeps its
//! own copy of the small helpers that pattern needs (`find_columns`,
//! `is_forest`, `is_mountain`) rather than importing from another test binary —
//! an integration test file is its own crate and cannot import from a sibling
//! one, the same constraint that file's own module doc names.
//!
//! # Configuration
//!
//! `SEED = 42`, matching every other `lodestone-worldgen` integration test that
//! runs against the embedded production generator
//! (`lodestone_server::overworld_generator`). Every test here generates real
//! terrain (including, for the counter and structure gates, a real structure
//! closure — `docs/plans/progressive-chunk-generation.md`'s own `What exists
//! today` section names a fresh generator's first column near unexplored
//! territory as the expensive case, tens of billions of instructions in
//! release), so every test is `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p lodestone-worldgen --features gen-counters \
//!     --test stage1_shaped_seam -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! # Dependencies
//!
//! `lodestone_server::overworld_generator` (the same dev-dependency cycle
//! `stage0_shaped_vs_full_cost.rs` already established and justified) and
//! `lodestone_worldgen::{overworld, counters}`.

use lodestone_server::overworld_generator;
use lodestone_worldgen::overworld::{GenStage, OverworldGenerator};

const SEED: i64 = 42;

// ===========================================================================
// Terrain census — copied from `stage0_shaped_vs_full_cost.rs`'s own
// predicates/search, per that file's own "How to change it" precedent that
// each measurement file keeps its own copy.
// ===========================================================================

fn is_forest(biome: &str) -> bool {
    biome.contains("forest") && !biome.contains("windswept")
}

fn is_mountain(biome: &str) -> bool {
    biome.contains("peaks") || biome.contains("windswept_hills") || biome.contains("windswept_gravelly")
}

fn find_columns(generator: &OverworldGenerator, matches: impl Fn(&str) -> bool, count: usize, radius: i32) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            let biome = generator.biome_at_quart(cx * 4 + 2, 15, cz * 4 + 2);
            if matches(&biome) {
                found.push((cx, cz));
                if found.len() >= count {
                    return found;
                }
            }
        }
    }
    found
}

fn assert_census_match(terrain: &str, pred: impl Fn(&str) -> bool, cx: i32, cz: i32, biome: &str) {
    assert!(
        pred(biome),
        "STAGE1 census control failed: column ({cx},{cz}) picked for {terrain} via \
         biome_at_quart has real biome {biome:?} after full generation — the cheap climate \
         lookup and the materialized biome disagree here"
    );
}

// ===========================================================================
// Gate 1: byte identity
// ===========================================================================

/// `column_shaped` then `column` for the same chunk must be byte-identical to
/// `column` cold — the property [`OverworldGenerator::column_shaped`]'s own doc
/// calls the "pure prefix" property. Run over two discriminating terrains
/// (forest for the vegetation diff, mountains for real ore density and exposed
/// stone) — never ocean, per this repo's own "an input where both hypotheses
/// coincide is not a test" rule (ocean scores near-zero on every axis a shaped
/// vs. full comparison could fail to notice).
///
/// The control lives at the end of each terrain's iteration: a clone of the
/// matching (upgraded) side has one block id flipped and must then disagree
/// with the cold side, proving `assert_eq!` on `into_raw()`'s tuple can
/// actually see a difference rather than vacuously agreeing (e.g. two empty
/// `Vec`s).
#[test]
#[ignore = "measurement-shaped correctness gate, real generation with a structure closure; \
            run with `cargo test --release -p lodestone-worldgen --test stage1_shaped_seam \
            -- --ignored --test-threads=1 --nocapture \
            stage1_shaped_then_full_is_byte_identical_to_cold_full`"]
fn stage1_shaped_then_full_is_byte_identical_to_cold_full() {
    const SEARCH_RADIUS: i32 = 64;

    let terrains: [(&str, fn(&str) -> bool); 2] =
        [("forest", is_forest as fn(&str) -> bool), ("mountains", is_mountain)];

    for &(name, pred) in &terrains {
        let search_gen = overworld_generator(SEED);
        let coords = find_columns(&search_gen, pred, 1, SEARCH_RADIUS);
        assert!(
            !coords.is_empty(),
            "STAGE1 census failure: found no {name} column within radius {SEARCH_RADIUS} of \
             seed {SEED} — this gate's fixture guard; widen SEARCH_RADIUS or re-derive the \
             predicate rather than trusting a partial sample"
        );
        let (cx, cz) = coords[0];

        // Cross-check against the real materialized biome, on a throwaway
        // generator so it does not warm either arm below.
        let verify_gen = overworld_generator(SEED);
        let biome = verify_gen.column(cx, cz).biome_state(8, 8).to_string();
        assert_census_match(name, pred, cx, cz, &biome);

        // Arm A: shaped, then upgraded to Full on the SAME generator — the
        // real "column already served at reduced stage, now upgraded" path.
        let arm_a = overworld_generator(SEED);
        let shaped = arm_a.column_shaped(cx, cz);
        assert_eq!(
            shaped.stage(),
            GenStage::Shaped,
            "STAGE1: column_shaped's own result is not tagged GenStage::Shaped"
        );
        let upgraded = arm_a.column(cx, cz);
        assert_eq!(
            upgraded.stage(),
            GenStage::Full,
            "STAGE1: column's own result is not tagged GenStage::Full"
        );

        // Arm B: an independent generator, cold Full — never touched by
        // column_shaped at all.
        let arm_b = overworld_generator(SEED);
        let cold = arm_b.column(cx, cz);

        let upgraded_raw = upgraded.into_raw();
        let cold_raw = cold.into_raw();

        println!(
            "STAGE1_BYTE_IDENTITY terrain={name:<9} chunk=({cx},{cz}) palette_len={} blocks_len={} \
             non_air_upgraded={}",
            upgraded_raw.2.len(),
            upgraded_raw.3.len(),
            upgraded_raw.3.iter().filter(|&&b| b != 0).count()
        );

        assert_eq!(
            upgraded_raw, cold_raw,
            "STAGE1: shaped-then-upgraded {name} column ({cx},{cz}) is NOT byte-identical to a \
             cold full column — column_shaped is not a pure prefix of column"
        );

        // Control: a deliberately corrupted clone of the cold side must stop
        // agreeing with the upgraded side. Proves the comparison above can
        // fail, not merely pass by construction.
        let mut corrupted = cold_raw.clone();
        assert!(!corrupted.3.is_empty(), "STAGE1 control setup: block array unexpectedly empty");
        corrupted.3[0] = corrupted.3[0].wrapping_add(1);
        assert_ne!(
            corrupted, upgraded_raw,
            "STAGE1 control failed: flipping one block id in a clone of the cold {name} column \
             did not make the byte-identity comparison disagree — the comparison is vacuous"
        );
    }
}

// ===========================================================================
// Gate 2: stage-touch counters (gen-counters only)
// ===========================================================================

#[cfg(feature = "gen-counters")]
#[test]
fn stage1_counters_require_the_gen_counters_feature() {
    assert!(
        lodestone_worldgen::counters::enabled(),
        "the gen-counters feature is on for this build but counters::enabled() reads false — \
         see lodestone-worldgen's Cargo.toml for the forward into lodestone-worldgen-core"
    );
}

/// A shaped sweep must compute `pre_ore`/structure-start work and must never
/// touch `post_ore` or the vegetation stage. Small sweep (3×3) — this gate is
/// about *which* counters move, not about scale (that is Stage 0's job).
#[cfg(feature = "gen-counters")]
#[test]
#[ignore = "measurement-shaped correctness gate, gen-counters build, real generation; run with \
            `cargo test --release -p lodestone-worldgen --features gen-counters \
            --test stage1_shaped_seam -- --ignored --test-threads=1 --nocapture \
            stage1_shaped_sweep_touches_only_pre_ore_and_structure_stages`"]
fn stage1_shaped_sweep_touches_only_pre_ore_and_structure_stages() {
    use lodestone_worldgen::counters::{self, Stage};

    assert!(counters::enabled(), "gen-counters feature is off for this build");

    const RADIUS: i32 = 1;
    let generator = overworld_generator(SEED);
    counters::reset();
    let mut non_air = 0usize;
    for cz in -RADIUS..=RADIUS {
        for cx in -RADIUS..=RADIUS {
            non_air += generator.column_shaped(cx, cz).non_air_count();
        }
    }
    assert!(non_air > 0, "STAGE1: shaped sweep generated only air — nothing measured");

    let snap = counters::snapshot();
    println!(
        "STAGE1_COUNTER pre_ore_computed={} structure_starts_computed={} post_ore_computed={} \
         vegetation_stage_entered={}",
        snap.pre_ore_computed,
        snap.structure_starts_computed,
        snap.post_ore_computed,
        snap.stage_entered[Stage::Vegetation as usize]
    );

    assert!(snap.pre_ore_computed > 0, "STAGE1: shaped sweep never computed pre_ore — vacuous");
    assert!(
        snap.structure_starts_computed > 0,
        "STAGE1: shaped sweep never computed structure starts — vacuous"
    );
    assert_eq!(
        snap.post_ore_computed, 0,
        "STAGE1: shaped sweep computed post_ore {} times — column_shaped is not a pure prefix",
        snap.post_ore_computed
    );
    assert_eq!(
        snap.stage_entered[Stage::Vegetation as usize], 0,
        "STAGE1: shaped sweep entered the vegetation stage {} times",
        snap.stage_entered[Stage::Vegetation as usize]
    );
}

/// The control for the gate above: the identical sweep shape through plain
/// `column()` must trip the two counters the shaped gate asserts are zero —
/// observed failing, not described. A generator independent from the shaped
/// gate's, so the two tests never share store state.
#[cfg(feature = "gen-counters")]
#[test]
#[ignore = "measurement-shaped correctness gate, gen-counters build, real generation; run with \
            `cargo test --release -p lodestone-worldgen --features gen-counters \
            --test stage1_shaped_seam -- --ignored --test-threads=1 --nocapture \
            stage1_control_full_sweep_trips_the_post_ore_and_vegetation_counters`"]
fn stage1_control_full_sweep_trips_the_post_ore_and_vegetation_counters() {
    use lodestone_worldgen::counters::{self, Stage};

    assert!(counters::enabled(), "gen-counters feature is off for this build");

    const RADIUS: i32 = 1;
    let generator = overworld_generator(SEED);
    counters::reset();
    let mut non_air = 0usize;
    for cz in -RADIUS..=RADIUS {
        for cx in -RADIUS..=RADIUS {
            non_air += generator.column(cx, cz).non_air_count();
        }
    }
    assert!(non_air > 0, "STAGE1 control: full sweep generated only air — nothing measured");

    let snap = counters::snapshot();
    println!(
        "STAGE1_CONTROL post_ore_computed={} vegetation_stage_entered={}",
        snap.post_ore_computed, snap.stage_entered[Stage::Vegetation as usize]
    );

    assert!(
        snap.post_ore_computed > 0,
        "STAGE1 control failed: a full sweep never computed post_ore — either the counter is not \
         wired, or column() stopped calling post_ore_world"
    );
    assert!(
        snap.stage_entered[Stage::Vegetation as usize] > 0,
        "STAGE1 control failed: a full sweep never entered the vegetation stage"
    );
}

// ===========================================================================
// Structure presence — answers the plan's own report question directly
// ===========================================================================

/// Block-state prefixes that only ever come from a mineshaft's own placed
/// pieces (`crate::structure::mineshaft`'s real state strings: planks, fence,
/// rail, cobweb, wall torch, iron chain) — never from fill, surface,
/// materialize or carve, the stages a Shaped column actually runs.
/// `starts_with` because the palette stores properties (`"minecraft:rail[shape=…]"`).
const MINESHAFT_TELLTALE_PREFIXES: [&str; 8] = [
    "minecraft:rail",
    "minecraft:cobweb",
    "minecraft:oak_fence",
    "minecraft:oak_planks",
    "minecraft:dark_oak_planks",
    "minecraft:dark_oak_fence",
    "minecraft:wall_torch",
    "minecraft:iron_chain",
];

fn palette_has_structure_telltale(palette: &[String]) -> bool {
    palette
        .iter()
        .any(|state| MINESHAFT_TELLTALE_PREFIXES.iter().any(|prefix| state.starts_with(prefix)))
}

/// Confirms `docs/plans/progressive-chunk-generation.md`'s own claim: "a shaped
/// column already contains villages, mineshafts, monuments — a distant
/// *generated* structure is visible at the reduced stage for free", by finding
/// a real mineshaft start via `structure_starts_placed_in` (mineshafts have
/// `spacing = 1` in `assets/worldgen/structure_set/mineshafts.json`, so they are
/// the densest structure set to search for) and checking its own
/// `column_shaped` output for real placed structure blocks.
///
/// **Checks the room piece's own chunk, not an arbitrary chunk the start merely
/// touches.** First cut of this gate picked the first `(cx, cz)`
/// `structure_starts_placed_in` returned non-empty for and found a real
/// mineshaft there with **no** telltale block in that chunk's own
/// `column_shaped` output — not a bug: a mineshaft corridor's own bounding box
/// can cross a chunk seam while its actual planks/rails sit on the other side
/// (rails in particular are placed at intervals, not throughout a corridor).
/// `mineshaft.rs`'s own test (`the room is added first`) establishes
/// `pieces[0]` is always the room, which — unlike a corridor stretch — is
/// guaranteed to carry the telltale materials, so this gate reads that piece's
/// own bounding box centre rather than trusting whichever chunk the raster
/// scan happened to land on.
///
/// The control: a chunk with **no** structure start placed in it must NOT show
/// the same telltale blocks — proving the detector discriminates rather than
/// matching everything.
#[test]
#[ignore = "measurement-shaped correctness gate, real generation with a structure closure; \
            run with `cargo test --release -p lodestone-worldgen --test stage1_shaped_seam \
            -- --ignored --test-threads=1 --nocapture \
            stage1_shaped_column_contains_a_real_structure`"]
fn stage1_shaped_column_contains_a_real_structure() {
    const SEARCH_RADIUS: i32 = 24;

    let search_gen = overworld_generator(SEED);
    let mut found: Option<(i32, i32, String)> = None;
    let mut checked_empty: Option<(i32, i32)> = None;
    'search: for cz in -SEARCH_RADIUS..=SEARCH_RADIUS {
        for cx in -SEARCH_RADIUS..=SEARCH_RADIUS {
            let starts = search_gen.structure_starts_placed_in(cx, cz);
            if let Some(start) = starts.first() {
                let room = start.pieces.first().unwrap_or_else(|| {
                    panic!(
                        "STAGE1 census failure: {} start at ({cx},{cz}) has pieces_complete=true \
                         but an empty pieces list",
                        start.structure
                    )
                });
                let mid_x = (room.bounding_box.min[0] + room.bounding_box.max[0]) / 2;
                let mid_z = (room.bounding_box.min[2] + room.bounding_box.max[2]) / 2;
                let (room_cx, room_cz) = (mid_x >> 4, mid_z >> 4);
                found = Some((room_cx, room_cz, start.structure.clone()));
                break 'search;
            }
            if checked_empty.is_none() {
                checked_empty = Some((cx, cz));
            }
        }
    }

    let (cx, cz, structure_id) = found.unwrap_or_else(|| {
        panic!(
            "STAGE1 census failure: found no structure start placed within radius \
             {SEARCH_RADIUS} of seed {SEED} — this gate's fixture guard; mineshafts alone have \
             spacing=1 in assets/worldgen/structure_set/mineshafts.json, so widen SEARCH_RADIUS \
             rather than trusting a partial sample"
        )
    });
    assert!(
        !search_gen.structure_starts_placed_in(cx, cz).is_empty(),
        "STAGE1 census failure: the room piece's own chunk ({cx},{cz}) reports no structure \
         start placed in it at all — the bounding-box-to-chunk derivation is wrong"
    );

    let shaped = search_gen.column_shaped(cx, cz);
    let (_, _, palette, _, _) = shaped.into_raw();
    println!(
        "STAGE1_STRUCTURE chunk=({cx},{cz}) structure={structure_id} shaped_palette_len={} \
         has_telltale={}",
        palette.len(),
        palette_has_structure_telltale(&palette)
    );
    assert!(
        palette_has_structure_telltale(&palette),
        "STAGE1: chunk ({cx},{cz})'s structure_starts_placed_in reports a real placed \
         {structure_id} start, but its column_shaped output carries none of \
         {MINESHAFT_TELLTALE_PREFIXES:?} — a shaped column does NOT actually contain the \
         structure"
    );

    // Control: a chunk with no structure start placed in it must not show the
    // same telltale blocks — the detector must discriminate, not match
    // everything.
    let (empty_cx, empty_cz) =
        checked_empty.expect("STAGE1 control setup: no structure-free chunk found before the hit");
    assert!(
        search_gen.structure_starts_placed_in(empty_cx, empty_cz).is_empty(),
        "STAGE1 control setup: chunk ({empty_cx},{empty_cz}) unexpectedly has a structure start \
         placed in it"
    );
    let empty_shaped = search_gen.column_shaped(empty_cx, empty_cz);
    let (_, _, empty_palette, _, _) = empty_shaped.into_raw();
    println!(
        "STAGE1_STRUCTURE_CONTROL chunk=({empty_cx},{empty_cz}) has_telltale={}",
        palette_has_structure_telltale(&empty_palette)
    );
    assert!(
        !palette_has_structure_telltale(&empty_palette),
        "STAGE1 control failed: chunk ({empty_cx},{empty_cz}) has no structure start placed in \
         it, but its shaped palette contains a structure telltale block anyway — the detector \
         does not discriminate"
    );
}
