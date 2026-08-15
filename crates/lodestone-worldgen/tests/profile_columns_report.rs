//! Real numbers for `profile.rs`'s per-stage percentile aggregator, over a
//! real fixture-backed generator — the source of the figures quoted in
//! `docs/tick-and-worldgen-profiling.md`. `src/profile.rs`'s own unit tests
//! already prove the percentile math against hand-built `StageTimes`; this
//! file's job is narrower: prove the whole pipeline (a real
//! `OverworldGenerator`, `column_timed`, `profile_columns`) produces a
//! sane, well-ordered report, and print it with `--nocapture` so a human can
//! read the actual split.
//!
//! Same `FsResolver`/fixture-tree shape as `tests/overworld_gen.rs` and
//! `benches/generation.rs`, with `full: true` (every fixture file, not just
//! density/noise) — a shape-only resolver would make carve/ore/vegetation/
//! top_layer all early-return, which is exactly the "world" species of
//! vacuous benchmark `benches/generation.rs`'s own `make_shape_only_generator`
//! doc comment records having shipped once already. This file has its own
//! copy of the resolver rather than importing one, matching the existing
//! precedent (`tests/overworld_gen.rs`, `benches/generation.rs` and
//! `chunk_parity.rs` each keep their own for the same reason: an integration
//! test binary is its own crate and cannot import from another one's
//! `tests/` file).

use std::path::Path;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use lodestone_worldgen::profile::profile_columns;
use serde_json::Value;

const SEED: i64 = 42;

struct FsResolver {
    root: std::path::PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    /// `Value::Null` for a missing file rather than panicking — the fixture
    /// tree does not carry every id the real registries do.
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

fn full_generator() -> OverworldGenerator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap())
            .unwrap();
    OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false)
}

/// Profiles a 3×3 patch (9 columns, each a fresh `column_timed` call — no
/// cache reuse across them, since `column_timed` bypasses the store) and
/// prints the per-stage percentile report. Run with `cargo test -p
/// lodestone-worldgen --test profile_columns_report -- --nocapture` to read
/// the real numbers; the assertions below only check the report is
/// well-formed (every stage has 9 samples, every ordering invariant holds,
/// a worst window was found) — they intentionally do not pin a specific
/// dominant stage or a specific percentile value, because that number is
/// exactly what this test exists to *measure*, not to assume.
#[test]
fn prints_a_real_stage_percentile_report_over_a_small_patch() {
    let generator = full_generator();
    let coords: Vec<(i32, i32)> = (0..3).flat_map(|cx| (0..3).map(move |cz| (cx, cz))).collect();
    let distribution = profile_columns(&generator, &coords);

    assert_eq!(distribution.columns_profiled, coords.len());
    for stage in &distribution.per_stage {
        assert_eq!(stage.sample_count, coords.len(), "stage {} missing samples", stage.stage);
        assert!(
            stage.p50_us <= stage.p95_us && stage.p95_us <= stage.p99_us && stage.p99_us <= stage.max_us,
            "stage {} percentiles out of order: {:?}",
            stage.stage,
            stage
        );
    }
    let worst = distribution.worst.expect("non-empty batch");

    println!("PROFILE_COLUMNS_REPORT columns={}", distribution.columns_profiled);
    for stage in &distribution.per_stage {
        println!(
            "PROFILE_COLUMNS_REPORT stage={:<11} p50_us={:>8} p95_us={:>8} p99_us={:>8} max_us={:>8} total_us={:>9}",
            stage.stage, stage.p50_us, stage.p95_us, stage.p99_us, stage.max_us, stage.total_us
        );
    }
    let dominant = distribution.dominant_stage();
    println!(
        "PROFILE_COLUMNS_REPORT dominant_stage={} total_us={}",
        dominant.stage, dominant.total_us
    );
    println!(
        "PROFILE_COLUMNS_REPORT worst_stage={} worst_us={} worst_chunk=({}, {})",
        worst.stage, worst.micros, worst.cx, worst.cz
    );
}

/// **Validation control**, per `src/profile.rs`'s own module doc: an
/// independent, already-tested instrument
/// (`lodestone_worldgen_core::counters`) must agree with `profile_columns`
/// on how many times generation actually ran. `Stage::Intern`'s
/// `StageGuard::enter` has exactly one call site
/// (`OverworldGenerator::intern_from_dense`), reached exactly once per
/// top-level `column`/`column_timed` call and never from the
/// neighbour-chunk recursion inside `ore_stage`/`vegetation_stage` — so
/// after `reset()`, profiling `N` columns must leave
/// `stage_entered[Stage::Intern]` at exactly `N`. Disagreement would mean
/// the aggregation loop skipped, doubled, or deduplicated a coordinate, not
/// that generation itself is wrong — the two instruments share no code path
/// other than `column_timed` itself, so this is a real cross-instrument
/// check, not a self-referential one. Only meaningful with `gen-counters`
/// on: `cargo test -p lodestone-worldgen --features gen-counters --test
/// profile_columns_report`.
#[cfg(feature = "gen-counters")]
#[test]
fn profiling_matches_the_gen_counters_intern_count() {
    use lodestone_worldgen::profile::profile_columns_with_counter_check;
    use lodestone_worldgen_core::counters::Stage;

    let generator = full_generator();
    let coords = [(0, 0), (1, 0), (0, 1), (5, -3), (2, 2)];
    let (distribution, snapshot) = profile_columns_with_counter_check(&generator, &coords);

    assert_eq!(distribution.columns_profiled, coords.len());
    assert_eq!(
        snapshot.stage_entered[Stage::Intern as usize],
        coords.len() as u64,
        "gen-counters saw a different number of Intern-stage entries than columns requested \
         (aggregation loop skipped, doubled, or deduplicated a coordinate)"
    );
}
