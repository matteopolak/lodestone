//! The actual parity gate: diffs `lodestone_server::overworld_generator` (the
//! generator the integrated server serves today) against the committed
//! vanilla fixture (`fixtures/composed_seed42.txt`, from
//! `scripts/worldgen-oracle/ComposedChunkOracle.java`) block-for-block, for
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
//! Issue #295 composed the real aquifer and carvers into `OverworldGenerator`
//! (`crates/lodestone-worldgen/src/overworld.rs`) — `column()`'s output is
//! now post-carve, not post-surface, which is why
//! [`composed_pipeline_vs_vanilla_postsurface_reference`] deliberately
//! expects *more* real mismatches against the `postsurface` stage than it
//! used to: every cell a carver legitimately touches now differs from the
//! pre-carve reference on purpose. `postcarve` remains the honest "how close
//! to a real vanilla chunk" number; ore/vegetation features and structures
//! are still missing from it (see `crate` doc comment).

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

/// Reference gate against vanilla's **pre-carve** `postsurface` stage.
/// Before issue #295 composed carvers, `OverworldGenerator::column()`'s
/// output *was* the post-surface subset, so this test used to isolate
/// exactly what was composed from what wasn't. Now that carve runs inside
/// `column()` too, this comparison **necessarily** shows every cell a
/// carver legitimately touched as a "real" mismatch against the pre-carve
/// reference — that is not a regression, it is carving working. The ceiling
/// below is therefore a *measured, re-assertable* value, not a target to
/// shrink: a regression here means either a carver changed what it carves
/// (investigate) or an earlier stage (shape/aquifer/biome/surface) broke.
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
        // Measured post-#295 (see docs/worldgen-parity.md): chunk (0,0)
        // real=3060/98304 (almost entirely the carved flooded-cave cells —
        // vanilla's own postsurface has stone there, pre-carve, and our
        // composed output has already carved it to water/air, matching
        // vanilla's *postcarve*, not postsurface — see the module doc);
        // chunk (-120,-120) real=5157/98304 (still overwhelmingly the
        // badlands-exclusion gap, `usable_overworld_table`'s documented scope
        // note in `crates/lodestone-worldgen/src/biome.rs`, plus a small
        // carve contribution). 5% headroom over the measured value per
        // chunk so this test does not flap on insignificant noise while
        // still catching a real regression.
        let ceiling = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 3213,
            (-120, -120) => 5415,
            other => panic!("no measured ceiling recorded for fixture chunk {other:?} — add one"),
        };
        assert!(
            real <= ceiling,
            "chunk ({},{}) real (non-property) mismatches vs vanilla postsurface grew to {real} (ceiling {ceiling}):\n{}",
            f.chunk_x,
            f.chunk_z,
            report.summary(20)
        );
    }
}

/// The full-pipeline gate: currently-composed Rust (shape + real aquifer +
/// biome + surface + carvers, issue #295) vs. vanilla's `postcarve` — still
/// missing ore/vegetation features and structures, see this crate's doc
/// comment. This is the "how far from a real vanilla chunk are we today"
/// number. The assertion is a **floor**, not a ceiling: it fails if this
/// regresses *below* the measured baseline (i.e. if any composed stage got
/// worse), and separately reports the full gap so it's visible without being
/// a pass/fail trap for the next increment's own work (ore features).
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

        // Floor, not ceiling: composing ore features (#295's next increment)
        // should only ever *shrink* this number further. A regression below
        // the measured floor means a currently-composed stage
        // (shape/aquifer/biome/surface/carve) broke.
        //
        // Measured post-#295: chunk (0,0) 94460/98304, **zero real
        // (non-property) mismatches** — the composed subset now matches
        // vanilla's postcarve exactly modulo the known fluid-`level`
        // representation gap. Chunk (-120,-120) 93608/98304, still short
        // because of the badlands-exclusion gap (unrelated to #295).
        let floor = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 94_400, // measured 94460/98304, 0 real mismatches
            (-120, -120) => 93_550, // measured 93608/98304
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

/// Issue #295's headline claim, made assertable rather than eyeballed.
/// `docs/worldgen-parity.md` named chunk (0,0)'s pre-#295 postcarve gap as
/// "dominated by `water -> stone`, 2780 positions" — vanilla's flooded caves,
/// which the (then uncomposed) carvers/aquifer could not produce. Per
/// `CLAUDE.md`'s "predict the value, do not merely assert the sign of the
/// change": this predicts the bucket lands at exactly zero once the real
/// aquifer + carvers are composed, and asserts that value — not just "fewer
/// mismatches than before," which a differently-wrong composition could also
/// satisfy.
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
    // Anti-vacuity: the fixture must still actually contain the water vanilla
    // carved into this chunk, so a diff engine that visited nothing (or a
    // fixture gone stale to all-air) couldn't pass this by accident.
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
