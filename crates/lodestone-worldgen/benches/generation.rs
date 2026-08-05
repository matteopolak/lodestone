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
    /// When false, only `density_function` and `noise` are answered and every
    /// other [`Resolver`] method keeps its `Value::Null` default — see
    /// [`make_shape_only_generator`].
    full: bool,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    /// Like [`Self::read`] but `Value::Null` for a missing file, matching
    /// `tests/vegetation_parity.rs`'s `try_json`. The fixture tree does not
    /// carry every id the real registries do, and a missing one must degrade to
    /// "no data" rather than panicking mid-benchmark.
    fn try_read(&self, kind: &str, id: &str) -> Value {
        if !self.full {
            return Value::Null;
        }
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

fn generator_with(seed: i64, full: bool) -> OverworldGenerator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone(), full };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    OverworldGenerator::new(seed, &settings, &resolver, "minecraft:plains", false)
}

/// The **shape-only** generator: density functions and noises resolved, and
/// nothing else.
///
/// # This was the whole file's generator, and it made two benches measure a
/// pipeline they claimed not to be measuring
///
/// `Resolver` has nine methods. This bench's `FsResolver` implemented the two
/// required ones and inherited `Value::Null` defaults for `biome_document`,
/// `configured_carver`, `configured_feature`, `placed_feature`,
/// `block_freeze_facts` and `block_tag` (`src/density/mod.rs:220-278`). Those
/// nulls resolve to empty carver lists, empty feature steps and an empty
/// replaceable-block tag, so **carvers, ore features, vegetal decoration and the
/// freeze stage were all inert** — every one of them an early return.
///
/// Measured, before the fix: over 49 chunks the carve stage totalled 185µs
/// (0.02%), ore 10µs, vegetation 9µs, top-layer 7µs. Meanwhile
/// `bench_ore_composition_sweep`'s doc comment asserted it "actually exercises
/// `OverworldGenerator::ore_stage`" and warned in terms that "a resolver with no
/// ore data at all would make `ore_stage` an early-return no-op and this bench
/// would measure nothing relevant" — while citing a `biome/plains.json` its own
/// resolver never opened. The file names the trap and then contains it, which is
/// `CLAUDE.md`'s "world" species of vacuous test exactly: the flaw is in the
/// input data, and nothing about reading the test reveals it.
///
/// This constructor is kept, because a shape-only generator is genuinely the
/// right subject for the raw noise-router throughput numbers (it samples fast
/// enough for criterion to take many samples). It is just no longer allowed to
/// masquerade as the composed pipeline: every bench using it says so, and
/// anything making a claim about carvers/ores/vegetation uses
/// [`make_full_generator`] instead.
fn make_shape_only_generator(seed: i64) -> OverworldGenerator {
    generator_with(seed, false)
}

/// The **composed** generator: every fixture the tree carries is resolved, so
/// carvers, the 3×3 ore driver, vegetal decoration and the freeze stage all
/// really run. Much slower per column, which is why benches using it work over
/// small patches.
fn make_full_generator(seed: i64) -> OverworldGenerator {
    generator_with(seed, true)
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
    // SHAPE-ONLY: this is the noise-router throughput number, deliberately not
    // the composed pipeline's. See `make_shape_only_generator`.
    let generator = make_shape_only_generator(SEED);
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

/// **Issue #85** — the per-stage cost split, now with **one bucket per stage
/// the pipeline actually has**: aquifer, noise router (shape), biome, surface,
/// materialize, carve, ore, vegetation, top-layer, intern.
///
/// # What this pass changed, and why the previous split was not #85's answer
///
/// A four-bucket split already existed here and was recorded to
/// `bench-results/generation.jsonl`. Two of its four buckets did not measure
/// what they were named, so the persisted numbers were misattributed:
///
/// * `stage_fluid_heightmap_pct` was **the biome stage**. The heightmap is
///   computed inside the shape window.
/// * `stage_intern_pct` was **materialize + carve + ore + vegetation +
///   intern**. So carvers, ore features and vegetal decoration — the three
///   things #85 explicitly says are "missing from that split entirely" — were
///   present in the total but invisible, filed under "interning".
///
/// Both are fixed by [`StageTimes`]'s new fields. **Do not compare a
/// `stage_intern_pct` from before this change with one from after**; the scene
/// string differs, which is what keeps `cargo xtask bench-compare` from pairing
/// them.
///
/// # The anti-drift control #85 asks for
///
/// #85's method note is that the previously-deleted instrumented twin was a
/// *correctness* risk because it duplicated the verified pipeline. `column_timed`
/// is not a duplicate — it calls the same private stage functions in the same
/// order — but it does bypass the two memo caches `column()` goes through, which
/// is a real difference and exactly the sort of thing that drifts silently. So
/// this bench **asserts the two produce identical output**, block for block,
/// over a fresh generator per arm.
///
/// The fresh generator per arm is load-bearing rather than tidiness: the
/// generator holds a 512-entry memo cache, so running both arms on one generator
/// would have the second arm read the first's cached result and agree with
/// itself no matter what — the trap that neutered two determinism gates once
/// already (see `chunk.rs`'s determinism test).
fn bench_stage_split(c: &mut Criterion) {
    // --- Anti-drift control: column_timed must equal column, block for block.
    {
        let g_plain = make_full_generator(SEED);
        let g_timed = make_full_generator(SEED);
        let (cx, cz) = (1, -1);
        let plain = g_plain.column(cx, cz);
        let (timed, _) = g_timed.column_timed(cx, cz);
        assert_eq!(
            (plain.min_y(), plain.height(), plain.non_air_count()),
            (timed.min_y(), timed.height(), timed.non_air_count()),
            "column_timed's column shape differs from column()'s — the timed path has drifted \
             from the verified one, which is the risk #85's method note is about"
        );
        let mut first_mismatch = None;
        let mut mismatches = 0usize;
        for lz in 0..16 {
            for lx in 0..16 {
                for y in plain.min_y()..plain.min_y() + plain.height() {
                    if plain.block_state(lx, y, lz) != timed.block_state(lx, y, lz) {
                        mismatches += 1;
                        first_mismatch.get_or_insert((lx, y, lz));
                    }
                }
            }
        }
        assert_eq!(
            mismatches, 0,
            "column_timed disagrees with column() at {mismatches} of {} cells, first at {:?} \
             (chunk {cx},{cz}) — the timed path is no longer measuring the real pipeline",
            16 * 16 * plain.height(),
            first_mismatch
        );
        // Control on the control: the comparison above must be capable of
        // seeing a difference. A neighbouring chunk must NOT match, or the
        // block_state comparison is reading something constant.
        let other = g_plain.column(cx + 1, cz);
        let differs = (plain.min_y()..plain.min_y() + plain.height())
            .any(|y| plain.block_state(3, y, 5) != other.block_state(3, y, 5));
        assert!(
            differs,
            "the block-for-block comparison found no difference between two DIFFERENT chunks, so \
             it could not have detected drift between column and column_timed either"
        );
    }

    let generator = make_full_generator(SEED);
    // 3x3 rather than 7x7: the composed pipeline is ~1000x slower per column
    // than the shape-only one this bench used to run, so the patch shrinks to
    // keep the bench runnable. Nine columns is still enough for the ore
    // driver's 3x3 neighbourhood sharing to be exercised.
    let coords: Vec<(i32, i32)> = (-1..=1).flat_map(|cz| (-1..=1).map(move |cx| (cx, cz))).collect();

    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column_timed(cx, cz));
    }

    // One accumulator per stage, in pipeline order.
    let mut stages: [(&str, Vec<f64>); 10] = [
        ("aquifer", Vec::new()),
        ("shape", Vec::new()),
        ("biome", Vec::new()),
        ("surface", Vec::new()),
        ("materialize", Vec::new()),
        ("carve", Vec::new()),
        ("ore", Vec::new()),
        ("vegetation", Vec::new()),
        ("top_layer", Vec::new()),
        ("intern", Vec::new()),
    ];
    for &(cx, cz) in &coords {
        let (col, t) = generator.column_timed(cx, cz);
        black_box(col.non_air_count());
        let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
        for (slot, value) in stages.iter_mut().zip([
            us(t.aquifer),
            us(t.shape),
            us(t.biome),
            us(t.surface),
            us(t.materialize),
            us(t.carve),
            us(t.ore),
            us(t.vegetation),
            us(t.top_layer),
            us(t.intern),
        ]) {
            slot.1.push(value);
        }
    }
    let sum = |v: &[f64]| v.iter().sum::<f64>();
    let total: f64 = stages.iter().map(|(_, v)| sum(v)).sum();
    assert!(total > 0.0, "every stage measured zero — nothing was timed");

    // The non-vacuity gate that the previous four-bucket split had no way to
    // express, and whose absence let this file measure an ore-free, carver-free
    // pipeline for as long as it existed. Each of these stages must have cost
    // *something*: a stage measuring ~0 here means the resolver stopped
    // supplying its data (a `Value::Null` default silently re-inherited, a
    // fixture file renamed) and the split has gone back to describing a
    // pipeline that is missing stages, while still looking like a clean
    // percentage table.
    //
    // The threshold is per-stage total over the whole patch, in microseconds —
    // a count-like floor, not a performance ceiling, so machine load cannot
    // move it. Carvers and ores were 185us and 10us across 49 chunks when
    // inert; a real run of either is orders of magnitude above that.
    let stage_total = |name: &str| -> f64 {
        stages.iter().find(|(n, _)| *n == name).map_or(0.0, |(_, v)| sum(v))
    };
    for name in ["aquifer", "shape", "surface", "materialize", "carve", "ore", "vegetation"] {
        let got = stage_total(name);
        assert!(
            got > 1_000.0,
            "stage {name:?} measured only {got:.1}us across {} composed columns, which is the \
             signature of an early return rather than a stage. Check that this bench's FsResolver \
             still answers the method that stage's data comes from (biome_document / \
             configured_carver / configured_feature / placed_feature / block_tag) — a \
             `Value::Null` default makes the stage a no-op and the percentage table below stays \
             perfectly plausible while describing a pipeline with stages missing.",
            coords.len()
        );
    }

    println!("worldgen stage split over {} chunks (seed={SEED}), release-or-debug per the recorded profile:", coords.len());
    for (name, v) in &stages {
        println!(
            "  {name:<12} {:>9.1}us total  {:>6.2}%",
            sum(v),
            100.0 * sum(v) / total
        );
    }
    println!(
        "  (HANDOFF.md \u{a7}4's original, deleted-instrumentation split was noise 40% / surface 51% / intern 7% at RD8/seed 3 \u{2014} different scene AND different bucket boundaries, so this is a fresh measurement, not a reproduction of that figure)"
    );
    println!(
        "  two stages are expected to read ~0 here and are excluded from the non-vacuity gate \
         above, for stated reasons rather than convenience: `biome` because this generator is \
         constructed with a single fixed biome (\"minecraft:plains\"), so there is no multi-noise \
         search to do; and `top_layer` because the fixture tree carries no \
         `block_freeze_facts` document, so `freeze_top_layer` has no predicates and early-returns. \
         `lodestone-server`'s own top_layer share assertion covers that stage against the \
         EMBEDDED data, which does carry those facts."
    );

    // Scene string names the ten-bucket split explicitly, so a reader (and
    // `bench-compare`) can never pair these with the old four-bucket numbers.
    let scene = format!("seed={SEED} patch=7x7(49 chunks) split=10stage");
    for (name, v) in &stages {
        support::record(support::Record {
            bench: "generation",
            metric: &format!("stage_{name}_pct"),
            scene: &scene,
            value: 100.0 * sum(v) / total,
            unit: "%",
        });
        support::record(support::Record {
            bench: "generation",
            metric: &format!("stage_{name}_us_per_chunk"),
            scene: &scene,
            value: sum(v) / coords.len() as f64,
            unit: "us",
        });
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
    // SHAPE-ONLY: linearity of the noise router in column count. The composed
    // pipeline's linearity is a different question (its memo caches make
    // adjacent columns cheaper) and is not what this ratio measures.
    let generator = make_shape_only_generator(SEED);
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

/// Issue #106's before/after: the ore-composition 3×3 driver's per-chunk cost
/// over a sweep large enough to exercise `pre_ore_cache` the same way a real
/// server view does (adjacent chunks share 8 of their 9 driven neighbours).
/// `tests/support/worldgen_data/biome/plains.json` carries plains' real
/// `UNDERGROUND_ORES` step (the same fixture `tests/feature_parity.rs` proves
/// matches the real JVM, per that test's own header), so this sweep actually
/// exercises `OverworldGenerator::ore_stage`/`stitch_region` — a resolver with
/// no ore data at all would make `ore_stage` an early-return no-op and this
/// bench would measure nothing relevant.
///
/// A 12×12 (144-chunk) patch specifically because `crate::overworld`'s own
/// module doc "Performance" section already has a historical number at this
/// exact size (`lodestone_server::worldgen_data::tests
/// ::served_columns_never_carry_an_unported_badlands_variant`, debug profile,
/// ~700.57s) to be comparable *in shape* against, even though that number
/// used the embedded server data (real biome variety) rather than this
/// crate's single-biome fixture and a different (debug, `cargo test`)
/// profile — see this bench's own `support::record` scene string, which
/// names both, so a reader never conflates the two numbers.
fn bench_ore_composition_sweep(c: &mut Criterion) {
    // FULL resolver, which is what makes this bench's name true. It previously
    // ran the shape-only generator, so `ore_stage` early-returned and the
    // recorded `ore_composition_column_median_us` described a pipeline with no
    // ores in it at all. Patch shrunk from 12x12 to 4x4 to stay runnable now
    // that the ore driver actually runs; 16 columns still gives interior chunks
    // whose 9 driven neighbours are mostly `pre_ore_cache` hits, which is the
    // sharing this bench exists to exercise.
    let generator = make_full_generator(SEED);
    let coords: Vec<(i32, i32)> = (0..4).flat_map(|cz| (0..4).map(move |cx| (cx, cz))).collect();

    // Warm up (first pass through the patch primes `pre_ore_cache` for the
    // interior chunks the timed pass below will find as cache hits).
    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column(cx, cz));
    }

    // Scene names the patch size AND the resolver, because both changed: the
    // pre-fix runs of this metric were 12x12 with a resolver that supplied no
    // ore data. A different scene string is what stops `bench-compare` pairing
    // the two, per `docs/benchmark-harness.md`.
    let scene = format!("seed={SEED} patch=4x4(16 chunks) resolver=full ores=real_plains_fixture");
    let n = coords.len();
    let mut per_chunk_us: Vec<f64> = Vec::with_capacity(n);
    let t_total = Instant::now();
    for &(cx, cz) in &coords {
        let t = Instant::now();
        black_box(generator.column(cx, cz));
        per_chunk_us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let total_s = t_total.elapsed().as_secs_f64();
    per_chunk_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_chunk_us[n / 2];
    println!(
        "worldgen ore-composition sweep: {n} chunks in {total_s:.3}s, median {median:.1}us/chunk (scene={scene:?})"
    );
    support::record(support::Record {
        bench: "generation",
        metric: "ore_composition_column_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });
    support::record(support::Record {
        bench: "generation",
        metric: "ore_composition_sweep_total_s",
        scene: &scene,
        value: total_s,
        unit: "s",
    });

    let mut idx = 0usize;
    c.bench_function("worldgen/ore_composition_column", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column(black_box(cx), black_box(cz)))
        })
    });
}

/// **Issue #87** — region-level generation with **peak resident-set growth**,
/// the half of #87 that was missing (its render-distance and linearity halves
/// are `bench_ore_composition_sweep` and `bench_linearity_check` above).
///
/// # Deltas, never absolutes
///
/// RSS is a process-wide counter that long outlives any one measurement, so an
/// absolute reading here would be the "duration species" of vacuous test — it
/// would mostly report how much of the binary and the fixture JSON tree happened
/// to be resident, which has nothing to do with generation. Everything recorded
/// below is therefore a **delta** from a baseline taken after the generator is
/// built and warmed, plus a per-chunk normalisation. The one number worth
/// tracking across runs is bytes/chunk.
///
/// # Why the sweep is small by default
///
/// #87 asks for RD 8/16/32. At the composed pipeline's real release cost those
/// are 289, 1089 and 4225 columns — roughly 4 minutes, 15 minutes and an hour on
/// this machine, which is not a benchmark anybody will run. So the default sweep
/// is small and the large radii are opt-in via `LODESTONE_BENCH_BIG_RD=1`. The
/// bytes/chunk and µs/chunk figures are what let a reader *project* RD 8/16/32,
/// and a projection is labelled as one rather than recorded as a measurement.
///
/// # On "how long before a joining player sees terrain"
///
/// #87 asks to cross-reference the integrated server's spawn chunk-send radius.
/// Checked: there is no constant to cite — `view_radius` is a caller-supplied
/// parameter (`lodestone_server::integrated::open_in_memory(.., view_radius)`,
/// clamped per-client in `server.rs:2080`), so the region a joining player needs
/// is configuration, not a fixed number. µs/chunk × (2r+1)² is the answer for
/// whatever radius a caller passes, which is why this bench records the
/// per-chunk figure rather than one region total.
fn bench_region_rss(_c: &mut Criterion) {
    let big = std::env::var("LODESTONE_BENCH_BIG_RD").is_ok();
    let radii: &[i32] = if big { &[2, 4, 8, 16] } else { &[2, 4] };
    if !big {
        println!(
            "worldgen region RSS: running small radii {radii:?} only; set LODESTONE_BENCH_BIG_RD=1 \
             for the RD 8/16 sweep #87 describes (RD 16 is ~1089 columns and takes minutes)"
        );
    }

    for &rd in radii {
        // FULL resolver: "how long before a joining player sees terrain" is a
        // question about the composed pipeline, not the noise router alone.
        let generator = make_full_generator(SEED);
        // Warm: build the density trees, touch the fixture JSON, let the
        // allocator reach a steady state — all of which would otherwise land in
        // the delta and be attributed to generation.
        for cx in 0..2 {
            black_box(generator.column(cx, 0));
        }

        let baseline = rss_bytes();
        let coords: Vec<(i32, i32)> =
            (-rd..=rd).flat_map(|cz| (-rd..=rd).map(move |cx| (cx, cz))).collect();
        let n = coords.len();
        let t0 = Instant::now();
        let mut peak = baseline;
        let mut non_air = 0usize;
        for &(cx, cz) in &coords {
            let col = generator.column(cx, cz);
            non_air += col.non_air_count();
            peak = peak.max(rss_bytes());
        }
        let wall_s = t0.elapsed().as_secs_f64();
        assert!(non_air > 0, "rd={rd}: generated only air — nothing measured");

        let growth = peak.saturating_sub(baseline);
        let per_chunk_us = wall_s * 1e6 / n as f64;
        println!(
            "worldgen region: rd={rd} ({n} columns) wall {wall_s:.3}s = {per_chunk_us:.0}us/chunk; \
             RSS baseline {:.1}MiB -> peak {:.1}MiB, growth {:.1}MiB = {:.1}KiB/chunk. \
             PROVISIONAL (wall-clock on a shared machine; the RSS delta is not load-sensitive). \
             Projection only: (2r+1)^2 x {per_chunk_us:.0}us => RD8 ~{:.0}s, RD16 ~{:.0}s, RD32 ~{:.0}s.",
            baseline as f64 / (1 << 20) as f64,
            peak as f64 / (1 << 20) as f64,
            growth as f64 / (1 << 20) as f64,
            growth as f64 / n as f64 / 1024.0,
            289.0 * per_chunk_us / 1e6,
            1089.0 * per_chunk_us / 1e6,
            4225.0 * per_chunk_us / 1e6,
        );

        let scene = format!("seed={SEED} rd={rd} columns={n}");
        for (metric, value, unit) in [
            ("region_rss_growth_bytes", growth as f64, "bytes"),
            ("region_rss_bytes_per_chunk", growth as f64 / n as f64, "bytes"),
            ("region_column_us", per_chunk_us, "us"),
            ("region_wall_s", wall_s, "s"),
        ] {
            support::record(support::Record {
                bench: "generation",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
    }
}

/// Current process resident set in bytes. In-process sampling rather than
/// `lodestone-allocbench`'s `/usr/bin/time -l` subprocess wrapper, because that
/// pattern can only report one figure for a whole process and #87 wants a
/// per-radius sweep inside one run. `memory-stats` reads the same OS counter
/// (`task_info` on macOS) with no subprocess and no output parsing — notably
/// *not* a shell pipeline, per `CLAUDE.md`'s rule that a number a conclusion
/// rests on must not come through one.
fn rss_bytes() -> u64 {
    memory_stats::memory_stats().map_or(0, |s| s.physical_mem as u64)
}

criterion_group!(
    benches,
    bench_column_throughput,
    bench_stage_split,
    bench_linearity_check,
    bench_ore_composition_sweep,
    bench_region_rss
);
criterion_main!(benches);
