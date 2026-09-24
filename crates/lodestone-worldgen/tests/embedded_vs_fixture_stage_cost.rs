//! Controlled A/B check for the cache-cold, fixture-backed stage split:
//! vegetation measured ~68% of total time, ore ~13%, and every other stage
//! at most ~8%.
//!
//! That figure and `bench_vegetation_walk_cost`'s tracked `embedded_stage_*`
//! baseline (`bench-results/generation.jsonl`, `lodestone-server`'s real
//! 26.2 data) disagree sharply: the tracked baseline puts **ore** first
//! (~29%), then biome (~23%), then shape (~19%), with vegetation a distant
//! fourth (~9%) — not vegetation-dominant at all. Per `CLAUDE.md`'s
//! "validate the instrument before optimizing the system", two conflicting
//! numbers are not evidence either is wrong on their own; they differ along
//! **two** uncontrolled variables at once:
//!
//! 1. **Resolver completeness.** `profile_columns_report.rs`'s `FsResolver`
//!    has no `tests/support/worldgen_data/biome_parameters/` directory at
//!    all (confirmed: the fixture tree has no such subdirectory), so
//!    `Resolver::biome_parameters` takes the trait default (empty array) and
//!    `OverworldGenerator::new`'s `dynamic_biome` is `None` — real per-column
//!    biome search never runs. The embedded resolver's `biome_parameters`
//!    returns a real 7,594-row multi-noise table.
//! 2. **Warm vs. cold neighbourhood.** The tracked `embedded_stage_*_us`
//!    baseline pre-warms a 5x5 neighbourhood via `generator.column(..)`
//!    (which uses `OverworldGenerator`'s own caches) before timing the
//!    centre chunk alone; `profile_columns_report.rs`'s 3x3 patch has no
//!    warm-up at all — every column, and every neighbour `ore`/`vegetation`
//!    walks internally, is fully cache-cold.
//!
//! This file holds variable 2 constant (both arms below profile the same
//! cache-cold 3x3 patch, no warm-up, matching `profile_columns_report.rs`'s
//! own methodology exactly) and varies only variable 1 (fixture resolver vs.
//! the real embedded 26.2 data), to find out whether resolver completeness
//! alone explains the discrepancy. It intentionally pins no specific
//! dominant stage as a pass/fail assertion — same reasoning as
//! `profile_columns_report.rs`'s own doc comment: the ranking is exactly
//! what this test exists to measure, not to assume. Run with
//! `--nocapture` to read the numbers; re-run in a quiet window
//! (`pgrep -l 'rustc|cargo'` empty) before using a specific figure for a
//! decision. The profiling guidance in `docs/tick-scheduling.md` explains
//! why a result from this scene cannot stand in for steady-state play.

use lodestone_worldgen::profile::{StageDistribution, profile_columns};

const SEED: i64 = 42;

/// Same 3x3, 9-column, cache-cold patch `profile_columns_report.rs` uses.
fn patch() -> Vec<(i32, i32)> {
    (0..3).flat_map(|cx| (0..3).map(move |cz| (cx, cz))).collect()
}

fn print_report(label: &str, distribution: &StageDistribution) {
    let total: u128 = distribution.per_stage.iter().map(|s| s.total_us).sum();
    println!("\n=== {label} (cache-cold 3x3 patch, seed={SEED}) ===");
    for stage in &distribution.per_stage {
        let pct = if total > 0 {
            100.0 * stage.total_us as f64 / total as f64
        } else {
            0.0
        };
        println!(
            "  {:<12} total_us={:>10} p50_us={:>8} max_us={:>9} share={:>5.1}%",
            stage.stage, stage.total_us, stage.p50_us, stage.max_us, pct
        );
    }
    let dominant = distribution.dominant_stage();
    println!("  dominant_stage={} total_us={} (of {total} total)", dominant.stage, dominant.total_us);
}

#[test]
fn resolver_completeness_alone_changes_the_dominant_stage() {
    let coords = patch();

    // Arm A: the fixture tree `profile_columns_report.rs` already uses —
    // reused via `lodestone-server`'s own dev-dependency graph is not
    // available here (that resolver is private to that test binary), so this
    // arm is rebuilt from the same `tests/support/worldgen_data` fixture
    // files `profile_columns_report.rs`, `overworld_gen.rs` and
    // `benches/generation.rs` all read.
    let fixture_distribution = {
        use lodestone_worldgen::density::{NoiseParams, Resolver};
        use lodestone_worldgen::overworld::OverworldGenerator;
        use serde_json::Value;
        use std::path::Path;

        struct FsResolver {
            root: std::path::PathBuf,
        }
        impl FsResolver {
            fn read(&self, kind: &str, id: &str) -> Value {
                let name = id.strip_prefix("minecraft:").unwrap_or(id);
                let path = self.root.join(kind).join(format!("{name}.json"));
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
            }
            fn try_read(&self, kind: &str, id: &str) -> Value {
                let name = id.strip_prefix("minecraft:").unwrap_or(id);
                let path = self.root.join(kind).join(format!("{name}.json"));
                match std::fs::read_to_string(&path) {
                    Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
                    Err(_) => Value::Null,
                }
            }
        }
        impl Resolver for FsResolver {
            fn density_function(&self, id: &str) -> Value {
                self.read("density_function", id)
            }
            fn noise(&self, id: &str) -> NoiseParams {
                let v = self.read("noise", id);
                NoiseParams {
                    first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
                    amplitudes: v["amplitudes"]
                        .as_array()
                        .expect("amplitudes")
                        .iter()
                        .map(|a| a.as_f64().expect("amplitude"))
                        .collect(),
                }
            }
            fn biome_document(&self, id: &str) -> Value {
                self.try_read("biome", id)
            }
            fn configured_carver(&self, id: &str) -> Value {
                self.try_read("configured_carver", id)
            }
            fn configured_feature(&self, id: &str) -> Value {
                self.try_read("configured_feature", id)
            }
            fn placed_feature(&self, id: &str) -> Value {
                self.try_read("placed_feature", id)
            }
            fn block_tag(&self, id: &str) -> Value {
                self.try_read("tags/block", id)
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        assert!(
            !root.join("biome_parameters").is_dir(),
            "fixture tree grew a biome_parameters/ directory — this test's premise \
             (real biome search is structurally absent from the fixture resolver) no \
             longer holds; re-derive the comparison rather than trusting the stale doc"
        );
        let resolver = FsResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
        )
        .unwrap();
        let generator = OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false);
        profile_columns(&generator, &coords)
    };

    // Arm B: the real embedded 26.2 data, same patch, same cold-cache
    // discipline — `lodestone_server::overworld_generator` builds a fresh
    // `OverworldGenerator` each call and `profile_columns` calls
    // `column_timed`, which bypasses this generator's own caches too, so
    // "no warm-up" holds for both arms identically.
    let embedded_distribution = {
        let generator = lodestone_server::overworld_generator(SEED);
        profile_columns(&generator, &coords)
    };

    print_report("fixture tree (no biome_parameters/, single fixed biome)", &fixture_distribution);
    print_report("embedded 26.2 data (real multi-noise biome search)", &embedded_distribution);

    let fixture_dominant = fixture_distribution.dominant_stage().stage;
    let embedded_dominant = embedded_distribution.dominant_stage().stage;
    println!(
        "\nfixture dominant stage: {fixture_dominant}   embedded dominant stage: {embedded_dominant}"
    );
    if fixture_dominant != embedded_dominant {
        println!(
            "CONFIRMED: resolver completeness alone changes which stage dominates a \
             cache-cold column — the fixture measurement's vegetation-heavy split describes \
             an inert-biome-search scene, not production behaviour. The profiling guidance \
             in docs/tick-scheduling.md explains why scenes must be named."
        );
    }

    // Both distributions must be well-formed (same shape invariant
    // `profile_columns_report.rs` checks) — this test's job is the
    // comparison, not re-proving `profile.rs`'s own percentile math.
    for distribution in [&fixture_distribution, &embedded_distribution] {
        for stage in &distribution.per_stage {
            assert_eq!(stage.sample_count, coords.len());
        }
    }
}
