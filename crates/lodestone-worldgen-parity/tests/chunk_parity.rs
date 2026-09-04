//! The actual parity gate: diffs `lodestone_server::overworld_generator` (the
//! generator the integrated server serves today) against the committed
//! reference fixture (`fixtures/composed_seed42.txt`) block-for-block, for
//! every fixture chunk, at both pipeline stages.
//!
//! Regenerate the fixture after a data/seed/coordinate change with:
//! ```text
//! cargo run -p lodestone-worldgen-parity --bin regen
//! ```
//! and re-run this test — a stale fixture and a real regression look
//! identical from outside, so treat any failure here as "check whether the
//! fixture is stale" *and* "check whether generation broke," in that order
//! (`CLAUDE.md`'s "re-verify before routing around" — the fixture itself can
//! be the stale thing).
//!
//! See `docs/worldgen-parity.md` for what the measured numbers mean.
//! `column()` includes aquifer and carver application, so its output is
//! post-carve rather than post-surface. Consequently,
//! [`composed_pipeline_vs_vanilla_postsurface_reference`] counts every cell
//! legitimately touched by a carver as a real mismatch against the pre-carve
//! reference. `postcarve` remains the honest "how close to a real vanilla
//! chunk" number; ore/vegetation features and structures are still missing
//! from it (see the crate doc comment).

use lodestone_worldgen_parity::{ChunkFixture, diff_field, parse_compact};

fn fixtures() -> Vec<ChunkFixture> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest_dir}/fixtures/composed_seed42.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let fixtures = parse_compact(&text);
    assert_eq!(fixtures.len(), 2, "expected exactly the 2 committed fixture chunks");
    fixtures
}

/// Anti-vacuity floor: the fixture itself must contain real, varied terrain —
/// not (accidentally) an all-air or all-one-block dump, which would let every
/// comparison below pass for the wrong reason.
#[test]
fn fixtures_are_non_vacuous() {
    for f in &fixtures() {
        let total = 16 * 16 * f.height as usize;
        assert!(
            f.postsurface.non_air_count() > total / 4,
            "chunk ({},{}) postsurface is suspiciously empty ({} non-air of {total})",
            f.chunk_x,
            f.chunk_z,
            f.postsurface.non_air_count()
        );
        assert!(
            f.postcarve.non_air_count() > total / 4,
            "chunk ({},{}) postcarve is suspiciously empty",
            f.chunk_x,
            f.chunk_z
        );
        assert!(
            f.postfeatures.non_air_count() > total / 4,
            "chunk ({},{}) postfeatures is suspiciously empty",
            f.chunk_x,
            f.chunk_z
        );
        // At least 2 distinct biome quarts across the pair of fixture chunks
        // combined would be a weak floor; per-chunk each fixture on its own
        // must show real biome resolution (a non-placeholder id), not every
        // quart silently defaulting to the same unresolved string.
        let distinct: std::collections::BTreeSet<&str> =
            f.biome_quarts.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            !distinct.is_empty() && distinct.iter().all(|id| id.starts_with("minecraft:")),
            "chunk ({},{}) biome quarts look unresolved: {distinct:?}",
            f.chunk_x,
            f.chunk_z
        );
    }
}

/// Control: diffing a fixture against itself must be exactly zero mismatches
/// over the whole chunk — proves [`diff_field`] visits every cell and that
/// "zero mismatches" is a meaningful signal here, not a loop that silently
/// compared nothing.
#[test]
fn control_self_diff_is_exact() {
    for f in &fixtures() {
        let report = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
        );
        assert_eq!(report.total, 16 * 16 * f.height as usize, "self-diff must visit every cell");
        assert_eq!(
            report.mismatches.len(),
            0,
            "chunk ({},{}) self-diff found mismatches — diff_field is broken:\n{}",
            f.chunk_x,
            f.chunk_z,
            report.summary(5)
        );
    }
}

/// Control: deliberately mutate exactly one block in a clone of the vanilla
/// fixture and confirm the harness catches it — at the exact location, and
/// nowhere else. Per `CLAUDE.md`'s evidence standards ("a control that
/// deliberately mutates one block and observes the harness catch it — run
/// it, do not describe it"), this *runs* the mutation and asserts on its
/// result rather than asserting the harness's own machinery works by
/// inspection.
#[test]
fn control_mutate_one_block_is_caught() {
    let f = &fixtures()[0];
    let mutated_lx = 7i32;
    let mutated_y = f.min_y + 10;
    let mutated_lz = 9i32;
    let original = f.postcarve.get(mutated_lx, mutated_y, mutated_lz).to_string();
    // Pick a value guaranteed different from whatever's really there.
    let bogus = if original == "minecraft:__parity_control_probe__" {
        "minecraft:__parity_control_probe_2__".to_string()
    } else {
        "minecraft:__parity_control_probe__".to_string()
    };

    let report = diff_field(
        f.min_y,
        f.height,
        |lx, y, lz| {
            if lx == mutated_lx && y == mutated_y && lz == mutated_lz {
                bogus.clone()
            } else {
                f.postcarve.get(lx, y, lz).to_string()
            }
        },
        |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
    );

    assert_eq!(
        report.mismatches.len(),
        1,
        "mutating exactly one block must produce exactly one mismatch, got {}:\n{}",
        report.mismatches.len(),
        report.summary(10)
    );
    let m = &report.mismatches[0];
    assert_eq!((m.lx, m.y, m.lz), (mutated_lx, mutated_y, mutated_lz), "mismatch must localise to the mutated cell");
    assert_eq!(m.got, bogus);
    assert_eq!(m.expected, original);
    let (min, max) = report.bounding_box().expect("bbox must exist");
    assert_eq!(min, (mutated_lx, mutated_y, mutated_lz));
    assert_eq!(max, (mutated_lx, mutated_y, mutated_lz));
}

/// Same two controls as [`control_self_diff_is_exact`]/
/// [`control_mutate_one_block_is_caught`], run again against the
/// `postfeatures` stage specifically — `diff_field` is stage-agnostic, but
/// per `CLAUDE.md`'s evidence standards a control is "run, not described" for
/// the stage it is actually gating, not inferred from a different field
/// happening to share the same comparator.
#[test]
fn control_postfeatures_self_diff_is_exact_and_mutation_is_caught() {
    let f = &fixtures()[0];

    let self_report = diff_field(
        f.min_y,
        f.height,
        |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
        |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
    );
    assert_eq!(self_report.total, 16 * 16 * f.height as usize);
    assert_eq!(
        self_report.mismatches.len(),
        0,
        "chunk ({},{}) postfeatures self-diff found mismatches:\n{}",
        f.chunk_x,
        f.chunk_z,
        self_report.summary(5)
    );

    let mutated_lx = 4i32;
    let mutated_y = f.min_y + 20;
    let mutated_lz = 11i32;
    let original = f.postfeatures.get(mutated_lx, mutated_y, mutated_lz).to_string();
    let bogus = if original == "minecraft:__parity_control_probe__" {
        "minecraft:__parity_control_probe_2__".to_string()
    } else {
        "minecraft:__parity_control_probe__".to_string()
    };
    let mutate_report = diff_field(
        f.min_y,
        f.height,
        |lx, y, lz| {
            if lx == mutated_lx && y == mutated_y && lz == mutated_lz {
                bogus.clone()
            } else {
                f.postfeatures.get(lx, y, lz).to_string()
            }
        },
        |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
    );
    assert_eq!(mutate_report.mismatches.len(), 1);
    let m = &mutate_report.mismatches[0];
    assert_eq!((m.lx, m.y, m.lz), (mutated_lx, mutated_y, mutated_lz));
    assert_eq!(m.got, bogus);
    assert_eq!(m.expected, original);
}

/// Anti-vacuity for the `postfeatures` stage itself: it must actually differ
/// from `postcarve` (the oracle's ore step wrote *something*), or every
/// comparison against it would trivially "pass" by measuring the carve stage
/// twice under a different name.
#[test]
fn postfeatures_actually_differs_from_postcarve() {
    for f in &fixtures() {
        let report = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
        );
        let real = report.real_mismatches().len();
        assert!(
            real > 0,
            "chunk ({},{}) postfeatures is byte-identical to postcarve — the oracle's ore \
             step did not write anything, so this fixture cannot be used as an ore-composition \
             gap measurement",
            f.chunk_x,
            f.chunk_z
        );
        eprintln!(
            "[parity] chunk ({}, {}): postfeatures vs postcarve real mismatches = {real} \
             (blocks the centre chunk's own ore step actually placed)",
            f.chunk_x,
            f.chunk_z
        );
    }
}

/// The residual ore-composition gap is measured and reported rather than
/// guessed. `OverworldGenerator::column` composes the real 3×3 ore driver,
/// while this fixture stage records only a single source chunk. Neighbour
/// spill therefore produces an expected non-zero residual; this test puts a
/// number on that residual instead of treating zero as the success condition.
/// The only hard assertion is a floor based on the measured fixture baseline;
/// the gap itself is reported, not asserted away.
#[test]
fn ore_composition_gap_is_measured_and_reported() {
    for f in &fixtures() {
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);
        let report = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
        );
        assert_eq!(report.total, 16 * 16 * f.height as usize);

        // Floor: `postfeatures` records only the centre chunk's decoration
        // pass, while `apply_ore_step_3x3_per_source` includes neighbour spill.
        // A faithful 3×3 composition therefore differs wherever an ore vein
        // from a neighbouring source lands in the centre. The debug-only
        // single-source toggle (`LODESTONE_ORE_SINGLE_SOURCE_DEBUG=1`) matches
        // the fixture's narrower scope and measures 563/98304 at (0,0),
        // confirming that most of the full-3×3 residual is a scope difference.
        //
        // The (-120,-120) fixture samples badlands. Its biome-specific ore
        // table is preserved by `crate::biome::usable_overworld_table`, so
        // the bonus gold entry is included in the measured 96429/98304 match
        // floor below. Floors are measured values with headroom.
        let floor = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 92_000,       // measured 92223/98304
            (-120, -120) => 96_300, // measured 96429/98304
            other => panic!("no measured floor recorded for fixture chunk {other:?} — add one"),
        };
        assert!(
            report.match_count() >= floor,
            "chunk ({},{}) match count vs vanilla postfeatures dropped to {} (floor {floor})",
            f.chunk_x,
            f.chunk_z,
            report.match_count()
        );

        eprintln!(
            "[parity] chunk ({}, {}): {}/{} match ({:.2}%) vs vanilla postfeatures (issue #295 \
             ore composition landed, real 3×3 spill vs a single-source-only oracle); {} real \
             mismatches (the residual ore gap), {} property-only",
            f.chunk_x,
            f.chunk_z,
            report.match_count(),
            report.total,
            report.match_fraction() * 100.0,
            report.real_mismatches().len(),
            report.representation_only_mismatches().len(),
        );
    }
}

/// Per-ore-type count bands (`CLAUDE.md`'s evidence standard: "an exact-match
/// ore comparison can fail purely from RNG stream ordering, so a position
/// diff alone cannot distinguish 'wrong placement' from 'same ores, different
/// draw order'" — `DESIGN.md:1957`). Buckets every cell that changed from
/// `postcarve` by its placed block's base id, for both vanilla's own
/// single-source `postfeatures` stage and Rust's now-composed `column()`, and
/// prints both tables side by side. Anti-vacuity floor only (not exact-match,
/// since real 3×3 spill legitimately shifts counts vs a single-source
/// oracle — see [`ore_composition_gap_is_measured_and_reported`]'s doc
/// comment): every ore type vanilla placed at least 10 of must appear at
/// least once in Rust's output, so a systematically-broken ore *type*
/// (wrong tag resolution, wrong target list) can't hide behind an
/// aggregate match percentage.
#[test]
fn ore_counts_by_type_are_predicted_and_measured() {
    fn counts_by_type(
        min_y: i32,
        height: i32,
        base: impl Fn(i32, i32, i32) -> String,
        other: impl Fn(i32, i32, i32) -> String,
    ) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for y in min_y..min_y + height {
                    let b = base(lx, y, lz);
                    let o = other(lx, y, lz);
                    if b != o {
                        let id = o.split('[').next().unwrap_or(&o).to_string();
                        *counts.entry(id).or_insert(0) += 1;
                    }
                }
            }
        }
        counts
    }

    for f in &fixtures() {
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);

        let vanilla_counts = counts_by_type(
            f.min_y,
            f.height,
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
            |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
        );
        let rust_counts = counts_by_type(
            f.min_y,
            f.height,
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
        );

        eprintln!(
            "[ore-counts] chunk ({}, {}) — block: vanilla(single-source postfeatures) / rust(composed column())",
            f.chunk_x, f.chunk_z
        );
        let all_ids: std::collections::BTreeSet<&String> =
            vanilla_counts.keys().chain(rust_counts.keys()).collect();
        for id in &all_ids {
            let v = vanilla_counts.get(*id).copied().unwrap_or(0);
            let r = rust_counts.get(*id).copied().unwrap_or(0);
            eprintln!("  {id}: vanilla={v} rust={r}");
        }

        for (id, &v) in &vanilla_counts {
            if v < 10 {
                continue;
            }
            // The badlands sample has a biome-specific `UNDERGROUND_ORES`
            // entry for the bonus `minecraft:ore_gold_extra` vein. Keep its
            // positive-gold check and dedicated log separate from the generic
            // per-type assertion so a missing biome-specific entry remains
            // visible in both normal output and a hard failure.
            if (f.chunk_x, f.chunk_z) == (-120, -120) && id == "minecraft:gold_ore" {
                let r = rust_counts.get(id).copied().unwrap_or(0);
                eprintln!(
                    "[ore-counts] chunk (-120,-120): minecraft:gold_ore vanilla={v} rust={r} — \
                     badlands' ore_gold_extra is reachable now that usable_overworld_table is a \
                     pass-through (Job 3 closed)"
                );
                assert!(
                    r > 0,
                    "chunk (-120,-120): vanilla placed {v} of minecraft:gold_ore but rust's \
                     composed column() placed NONE — badlands' bonus gold vein \
                     (ore_gold_extra) should be reachable now that usable_overworld_table is a \
                     pass-through; this is a regression, not the pre-existing Job 3 gap"
                );
                continue;
            }
            let r = rust_counts.get(id).copied().unwrap_or(0);
            assert!(
                r > 0,
                "chunk ({},{}): vanilla placed {v} of {id} but rust's composed column() placed \
                 NONE at all — a whole ore type appears to be missing, not merely miscounted",
                f.chunk_x,
                f.chunk_z
            );
        }
    }
}

/// Reference gate against vanilla's **pre-carve** `postsurface` stage.
/// `OverworldGenerator::column()` applies carving before this comparison, so
/// every cell legitimately touched by a carver appears as a real mismatch
/// against the pre-carve reference. The ceiling is a measured,
/// re-assertable bound: exceeding it indicates a change in carving or an
/// earlier shape, aquifer, biome, or surface stage.
///
/// Thresholds are measured against the committed fixture (see
/// `docs/worldgen-parity.md`), not guessed: they assert "did not get worse
/// than observed," with headroom for the property-only fluid-level gap
/// (`Mismatch::same_block_id`) which this counts separately from real
/// block-id differences.
#[test]
fn composed_pipeline_vs_vanilla_postsurface_reference() {
    for f in &fixtures() {
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);
        let report = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postsurface.get(lx, y, lz).to_string(),
        );
        assert_eq!(report.total, 16 * 16 * f.height as usize, "diff must visit every cell");

        let real = report.real_mismatches().len();
        // Current fixture measurements: chunk (0,0) has 8787 real mismatches
        // out of 98304. Ore placement contributes to this count because the
        // generated column includes decoration while `postsurface` is
        // pre-carve and pre-feature.
        //
        // The badlands sample at (-120,-120) has 6704 real mismatches out of
        // 98304. Its biome-specific banded-terracotta surface rule tracks the
        // reference closely; the independent ore-composition residual is
        // covered by `ore_composition_gap_is_measured_and_reported`.
        //
        // 5% headroom over the measured value per chunk so this test does
        // not flap on insignificant noise while still catching a real
        // regression.
        let ceiling = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 9_230,      // measured 8787
            (-120, -120) => 7_040, // measured 6704
            other => panic!("no measured ceiling recorded for fixture chunk {other:?} — add one"),
        };
        assert!(
            real <= ceiling,
            "chunk ({},{}) real (non-property) mismatches vs vanilla postsurface grew to {real} (ceiling {ceiling}):\n{}",
            f.chunk_x,
            f.chunk_z,
            report.summary(20)
        );
        eprintln!(
            "[parity] chunk ({}, {}): {real} real mismatches vs vanilla postsurface (ceiling {ceiling})",
            f.chunk_x,
            f.chunk_z
        );
    }
}

/// The full-pipeline gate compares currently-composed Rust (shape + real
/// aquifer + biome + surface + carvers + ores) with vanilla's `postcarve`.
/// That reference has no ore features, so generated ore blocks necessarily
/// widen this gap; vegetation and structures are also absent, as described in
/// the crate documentation. The assertion is a **floor**, not a ceiling: it
/// fails when the match count falls below the measured baseline and reports
/// the full gap for the stages outside this fixture's scope.
#[test]
fn full_vanilla_pipeline_gap_is_measured_and_reported() {
    for f in &fixtures() {
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);
        let report = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
        );
        assert_eq!(report.total, 16 * 16 * f.height as usize);

        // Floor, not ceiling: `postcarve` has no ore features, so generated
        // ore blocks reduce the match count even though ore composition is
        // functioning. A result below the floor indicates a regression in an
        // already-composed shape, aquifer, biome, surface, or carver stage.
        //
        // Current fixture measurements are 88733/98304 matches and 5727 real
        // mismatches for (0,0), and 92061/98304 matches and 6237 real
        // mismatches for (-120,-120).
        let floor = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 88_700,       // measured 88733
            (-120, -120) => 92_000, // measured 92061
            other => panic!("no measured floor recorded for fixture chunk {other:?} — add one"),
        };
        assert!(
            report.match_count() >= floor,
            "chunk ({},{}) match count vs full vanilla postcarve dropped to {} (floor {floor}) — \
             a composed-stage regression, not the known carver/aquifer/feature gap:\n{}",
            f.chunk_x,
            f.chunk_z,
            report.match_count(),
            report.summary(20)
        );

        eprintln!(
            "[parity] chunk ({}, {}): {}/{} match ({:.2}%) vs full vanilla (minus features/structures); {} real mismatches, {} property-only",
            f.chunk_x,
            f.chunk_z,
            report.match_count(),
            report.total,
            report.match_fraction() * 100.0,
            report.real_mismatches().len(),
            report.representation_only_mismatches().len(),
        );
    }
}

/// Specific mismatch gate for chunk (0,0). The committed `postcarve`
/// reference contains water in flooded caves, so the generated column should
/// reproduce those cells rather than leaving stone in their positions. The
/// zero target is exact: checking the bucket count prevents a differently
/// wrong composition from passing merely because it produces fewer total
/// mismatches.
#[test]
fn water_to_stone_bucket_is_resolved_for_chunk_0_0() {
    let f = fixtures()
        .into_iter()
        .find(|f| (f.chunk_x, f.chunk_z) == (0, 0))
        .expect("chunk (0,0) fixture must be present");
    let generator = lodestone_server::overworld_generator(f.seed);
    let generated = generator.column(f.chunk_x, f.chunk_z);
    let report = diff_field(
        f.min_y,
        f.height,
        |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
        |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
    );

    let water_to_stone = report
        .real_mismatches()
        .iter()
        .filter(|m| {
            m.expected.split('[').next() == Some("minecraft:water")
                && m.got.split('[').next() == Some("minecraft:stone")
        })
        .count();
    assert_eq!(
        water_to_stone, 0,
        "the water->stone bucket (vanilla's flooded caves) should be fully resolved by \
         composing the real aquifer + carvers — got {water_to_stone} remaining"
    );
    // Anti-vacuity: the fixture must contain enough reference water in this
    // chunk for the bucket to be exercised, so an empty traversal or stale
    // all-air fixture cannot pass the gate by accident.
    let water_cells = (0..16)
        .flat_map(|lx| (0..16).map(move |lz| (lx, lz)))
        .flat_map(|(lx, lz)| (f.min_y..f.min_y + f.height).map(move |y| (lx, y, lz)))
        .filter(|&(lx, y, lz)| f.postcarve.get(lx, y, lz).split('[').next() == Some("minecraft:water"))
        .count();
    assert!(
        water_cells > 1000,
        "fixture chunk (0,0) postcarve has suspiciously little water ({water_cells}) — \
         the bucket this test checks may not actually be exercised"
    );
}
