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
//! See `docs/worldgen-parity.md` for what the measured numbers mean and why
//! `postcarve` is expected to be far from 100% today (carvers/real
//! aquifer/features are not yet composed into `OverworldGenerator` —
//! `crates/lodestone-worldgen/src/overworld.rs:29-34`, tracked as #295).

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

/// The headline gate: the currently-composed Rust pipeline (shape + fluid
/// approximation + real biome + surface rules —
/// `crates/lodestone-worldgen/src/overworld.rs`'s doc comment) against
/// vanilla's `postsurface` stage (same subset, real biome, real aquifer).
/// This isolates exactly what's composed today from what isn't (carvers),
/// so a regression here means the *composed* stages broke, not that caves
/// are still missing.
///
/// Thresholds are measured against the committed fixture (see
/// `docs/worldgen-parity.md`), not guessed: they assert "did not get worse
/// than observed," with headroom for the property-only fluid-level gap
/// (`Mismatch::same_block_id`) which this counts separately from real
/// block-id differences.
#[test]
fn currently_composed_subset_matches_vanilla_postsurface() {
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
        // Measured (see docs/worldgen-parity.md): chunk (0,0) real=316/98304,
        // chunk (-120,-120) real=4910/98304 (the badlands-exclusion gap,
        // `usable_overworld_table`'s documented scope note in
        // `crates/lodestone-worldgen/src/biome.rs`). 5% headroom over the
        // measured value per chunk so this test does not flap on
        // insignificant noise while still catching a real regression.
        let ceiling = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 332,
            (-120, -120) => 5156,
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

/// The full-pipeline gate: currently-composed Rust vs. vanilla's `postcarve`
/// (shape + real aquifer + surface + carvers — still missing ore/vegetation
/// features and structures, see this crate's doc comment). This is the
/// "how far from a real vanilla chunk are we today" number — expected to be
/// well short of 100% because carvers/the real aquifer are not composed yet
/// (issue #295). The assertion is a **floor**, not a ceiling: it fails if
/// this regresses *below* the measured composed-subset baseline (i.e. if
/// something makes the currently-composed stages themselves worse), and
/// separately reports the full gap so it's visible without being a pass/fail
/// trap for #295's own work.
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

        // Floor, not ceiling: composing carvers (#295) should only ever
        // *shrink* this number. A regression below the measured floor means
        // the currently-composed stages (shape/fluid/biome/surface) broke,
        // not that carvers are still missing — that's expected and already
        // accounted for.
        let floor = match (f.chunk_x, f.chunk_z) {
            (0, 0) => 90_000, // measured 90100/98304
            (-120, -120) => 92_800, // measured 93053/98304
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
