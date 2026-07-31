//! Chunk-generation benchmarks for the real, JVM-verified overworld pipeline
//! (issue #78 epic, sub-issues #84/#85). Builds the *real*
//! [`OverworldGenerator`] the same way `tests/overworld_gen.rs` and
//! `tests/*_parity.rs` do — an `FsResolver` reading the checked-in
//! `tests/support/worldgen_data` JSON tree — rather than a synthetic stand-in,
//! per the epic's evidence standard: chunk generation is "stable enough to
//! benchmark meaningfully" specifically because it is the one subsystem
//! verified bit-exact against JVM oracles (`HANDOFF.md` §4).
//!
//! This crate has no `[[bin]]`/example target of its own akin to
//! `lodestone-server/examples/bench_worldgen.rs`; that example lives in
//! `lodestone-server` (out of this pass's scope) and benches the *embedded*
//! production data. This bench is deliberately independent of it — it proves
//! the same generator, driven from data this crate already owns for its own
//! parity tests, so no `lodestone-server` dependency is needed to benchmark
//! `lodestone-worldgen` itself.
//!
//! Run with: `cargo bench -p lodestone-worldgen --bench generation`

mod support;

use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

/// Same shape as `tests/overworld_gen.rs`'s `FsResolver` / `chunk_parity.rs`'s
/// resolver: reads density functions and noises straight off disk under
/// `tests/support/worldgen_data`, the fixture tree the parity suite already
/// keeps current against the JVM oracle.
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
}

fn make_generator(seed: i64) -> OverworldGenerator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    OverworldGenerator::new(seed, &settings, &resolver, "minecraft:plains", false)
}

/// Seed 42, chunk (0,0): the exact fixture `overworld_gen.rs`'s
/// `composed_shape_matches_verified_density_field` anchors to a checked-in JVM
/// density dump, so this bench exercises the same terrain those correctness
/// tests already pinned down (an oceanic column: shape + fluid + surface all
/// exercised, not a vacuous stone slab).
const SEED: i64 = 42;

/// Headline throughput: real `column()` generation over a modest patch, timed
/// both by criterion (statistically robust, many samples) and — once, outside
/// criterion's loop — recorded to `bench-results/generation.jsonl` with scene
/// metadata so it is comparable across runs on this machine.
fn bench_column_throughput(c: &mut Criterion) {
    let generator = make_generator(SEED);
    // A small patch so criterion's sampling stays fast; #84/#87 (region-scale
    // throughput) are the place for a full render-distance sweep.
    let coords: Vec<(i32, i32)> = (-2..=2).flat_map(|cz| (-2..=2).map(move |cx| (cx, cz))).collect();

    // Warm up (touch caches / branch predictor) before either measurement.
    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column(cx, cz));
    }

    // One-shot diagnostic measurement, recorded with metadata — independent of
    // criterion's own iteration count/loop, matching `bench_worldgen.rs`'s
    // percentile shape.
    let scene = format!("seed={SEED} patch=5x5(25 chunks)");
    let n = coords.len();
    let mut per_chunk_us: Vec<f64> = Vec::with_capacity(n);
    for &(cx, cz) in &coords {
        let t = Instant::now();
        black_box(generator.column(cx, cz));
        per_chunk_us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    per_chunk_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_chunk_us[n / 2];
    support::record(support::Record {
        bench: "generation",
        metric: "column_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });

    // Criterion's own headline number, for local `--baseline`/`--save-baseline`
    // before/after comparisons.
    let mut idx = 0usize;
    c.bench_function("worldgen/column_real_generator", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column(black_box(cx), black_box(cz)))
        })
    });
}

/// Per-stage cost split (shape / fluid+heightmap / surface / intern),
/// re-deriving the number `HANDOFF.md` §4 recorded once (noise 40% / surface
/// 51% / intern 7%) via an instrumented twin of `column()` that was then
/// deleted specifically because it duplicated a correctness-verified pipeline.
///
/// This does **not** recreate that risk: `OverworldGenerator::column_timed`
/// (added alongside this bench, in `src/overworld.rs`) calls the exact same
/// private stage functions `column()` itself calls — the only thing added is
/// an `Instant::now()` at each boundary. There is one pipeline, not two.
fn bench_stage_split(c: &mut Criterion) {
    let generator = make_generator(SEED);
    let coords: Vec<(i32, i32)> = (-3..=3).flat_map(|cz| (-3..=3).map(move |cx| (cx, cz))).collect();

    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column_timed(cx, cz));
    }

    let mut shape_us = Vec::with_capacity(coords.len());
    let mut fluid_us = Vec::with_capacity(coords.len());
    let mut surface_us = Vec::with_capacity(coords.len());
    let mut intern_us = Vec::with_capacity(coords.len());
    for &(cx, cz) in &coords {
        let (col, t) = generator.column_timed(cx, cz);
        black_box(col.non_air_count());
        shape_us.push(t.shape.as_secs_f64() * 1e6);
        fluid_us.push(t.fluid_heightmap.as_secs_f64() * 1e6);
        surface_us.push(t.surface.as_secs_f64() * 1e6);
        intern_us.push(t.intern.as_secs_f64() * 1e6);
    }
    let sum = |v: &[f64]| v.iter().sum::<f64>();
    let total = sum(&shape_us) + sum(&fluid_us) + sum(&surface_us) + sum(&intern_us);
    let pct = |v: &[f64]| 100.0 * sum(v) / total;

    println!(
        "worldgen stage split over {} chunks (seed={SEED}): shape {:.1}% fluid+heightmap {:.1}% surface {:.1}% intern {:.1}%",
        coords.len(),
        pct(&shape_us),
        pct(&fluid_us),
        pct(&surface_us),
        pct(&intern_us),
    );
    println!(
        "  (HANDOFF.md \u{a7}4's original, deleted-instrumentation split was noise 40% / surface 51% / intern 7% at RD8/seed 3 \u{2014} different scene, so treat this as a fresh measurement, not a reproduction of that exact figure)"
    );

    let scene = format!("seed={SEED} patch=7x7(49 chunks)");
    for (metric, pct_val) in [
        ("stage_shape_pct", pct(&shape_us)),
        ("stage_fluid_heightmap_pct", pct(&fluid_us)),
        ("stage_surface_pct", pct(&surface_us)),
        ("stage_intern_pct", pct(&intern_us)),
    ] {
        support::record(support::Record { bench: "generation", metric, scene: &scene, value: pct_val, unit: "%" });
    }

    // Also give criterion a crack at the whole `column_timed` call so its
    // overhead vs. plain `column()` is visible (should be ~identical; a
    // divergence would mean the timing wrapper itself is not free).
    let mut idx = 0usize;
    c.bench_function("worldgen/column_timed_overhead", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column_timed(black_box(cx), black_box(cz)))
        })
    });
}

/// Sanity: does generation scale roughly linearly with chunk count? A ratio
/// against a paired same-run measurement, per the epic's method rules — not a
/// wall-clock ceiling. Superlinear growth here would mean a per-call cost is
/// leaking global state (e.g. an unbounded cache) across chunks.
fn bench_linearity_check(c: &mut Criterion) {
    let generator = make_generator(SEED);
    let small: Vec<(i32, i32)> = (0..3).map(|i| (i, 0)).collect(); // 3 chunks
    let large: Vec<(i32, i32)> = (0..12).map(|i| (i, 0)).collect(); // 12 chunks (4x)

    for &(cx, cz) in &small {
        black_box(generator.column(cx, cz));
    }

    let time_batch = |coords: &[(i32, i32)]| -> f64 {
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t0 = Instant::now();
            for &(cx, cz) in coords {
                black_box(generator.column(cx, cz));
            }
            best = best.min(t0.elapsed().as_secs_f64());
        }
        best
    };

    let t_small = time_batch(&small);
    let t_large = time_batch(&large);
    let ratio = t_large / t_small;
    let chunk_ratio = large.len() as f64 / small.len() as f64;
    println!(
        "worldgen linearity: {}x chunks -> {:.2}x time (best-of-5, seed={SEED}) \u{2014} expect close to {:.1}x for linear cost",
        chunk_ratio, ratio, chunk_ratio
    );

    let scene = format!("seed={SEED} small={} large={}", small.len(), large.len());
    support::record(support::Record {
        bench: "generation",
        metric: "linearity_ratio_vs_expected",
        scene: &scene,
        value: ratio / chunk_ratio,
        unit: "x",
    });

    let mut group = c.benchmark_group("worldgen/linearity");
    group.bench_function("small_3_chunks", |b| {
        b.iter(|| {
            for &(cx, cz) in &small {
                black_box(generator.column(black_box(cx), black_box(cz)));
            }
        })
    });
    group.bench_function("large_12_chunks", |b| {
        b.iter(|| {
            for &(cx, cz) in &large {
                black_box(generator.column(black_box(cx), black_box(cz)));
            }
        })
    });
    group.finish();
}

criterion_group!(benches, bench_column_throughput, bench_stage_split, bench_linearity_check);
criterion_main!(benches);
