//! Light-propagation throughput for `lodestone-world`'s local/singleplayer
//! per-chunk consumer: [`compute_column_light`] and
//! [`compute_column_light_with_neighbours`] — the functions
//! `lodestone-shell/src/worldgen.rs` calls for every locally generated column.
//! `lodestone-shell/src/net.rs`'s own module doc states the split explicitly:
//! "MP consumes server light; SP computes it. Do not run `compute_column_light`
//! on live columns." This bench is the SP half; `chunk_load.rs` in this same
//! directory is the MP half, where post-decode light work is exactly zero.
//!
//! This crate's `tests/memory.rs` already put a number on this with plain
//! `Instant` timing (`measure_light_recompute_cost`,
//! `measure_neighbour_light_cost`) — worth reading first, because it explains
//! *why* neighbour light needed measuring at all: `CLAUDE.md` flags it as a
//! cost that was "surprising" enough to earn a dedicated ratio gate. This bench
//! is the criterion-instrumented companion for the harness (issue #78 epic):
//! `tests/memory.rs` keeps its own sanity-ceiling assertions (they belong in a
//! `#[test]`, gating `cargo test`); nothing here asserts a ceiling — advisory
//! only, matching every other bench in this harness. It does keep one
//! correctness-of-setup assertion (see below), which is not the same thing.
//!
//! # Why this input is not the vacuous-world trap
//!
//! An empty or uniformly-lit section propagates trivially: every cell already
//! agrees with its neighbours, so the flood-fill queue drains in O(1)
//! regardless of whether the real algorithm is correct, fast, or badly broken
//! — a healthy-looking number that proves nothing. `lighting.rs` names this
//! exact trap in its own doc comment for [`light_exercises_propagation`] ("on a
//! superflat world... sky light never spreads horizontally"), and ships the
//! function to detect it. This bench's fixture is `tests/memory.rs`'s
//! `realistic_terrain_column` shape: a solid stone floor, a *varied* surface
//! band forcing real per-cell opacity differences, open sky above — and each
//! bench function below asserts `light_exercises_propagation` on its own
//! computed output *before* any timing starts, so a future fixture regression
//! (someone "simplifying" the terrain to flat stone to speed up the bench)
//! fails loudly instead of quietly reporting a fast, meaningless number.
//!
//! # Duration species
//!
//! Neither function here needed per-iteration setup: both take `&ChunkColumn`
//! and return a fresh `ColumnLight` with no shared mutable state, so nothing
//! accumulates across criterion's repeated calls — the same reason
//! `tests/memory.rs`'s plain-`Instant` loop was already safe to run thousands
//! of times without resetting anything between calls.
//!
//! Run with: `cargo bench -p lodestone-world --bench light_propagation`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_world::{
    ChunkColumn, LightProperties, Neighbourhood, PaletteKind, compute_column_light,
    compute_column_light_with_neighbours, light_exercises_propagation,
};

const MIN_Y: i32 = -64;
const SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.

/// Same shape as `tests/memory.rs`'s `realistic_terrain_column` — duplicated
/// here rather than shared, because `tests/` is a separate compilation target
/// a `benches/` binary cannot link against. Keep the two in sync by
/// inspection if either changes; they encode the same claim ("this is what a
/// player is actually standing in"), not a shared implementation detail.
fn realistic_terrain_column() -> ChunkColumn {
    let mut col = ChunkColumn::new(
        MIN_Y,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    let stone = 1u32;
    for y in MIN_Y..40 {
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, y, z, stone);
            }
        }
    }
    for y in 40..48 {
        for z in 0..16 {
            for x in 0..16 {
                let id = 1 + ((x + z + (y as usize)) % 6) as u32;
                col.set_block(x, y, z, id);
            }
        }
    }
    col
}

/// Same opacity/emission table as `tests/memory.rs`'s `TimingProps`: anything
/// non-air is opaque, one id is glass-like transparent, one id emits like a
/// torch — so both the sky-light and block-light-source paths are exercised.
struct TimingProps;

impl LightProperties for TimingProps {
    fn opacity(&self, state: u32) -> u8 {
        match state {
            0 => 0,  // air
            7 => 0,  // a transparent non-air (glass-like)
            _ => 15, // everything else opaque
        }
    }
    fn emission(&self, state: u32) -> u8 {
        if state == 5 { 14 } else { 0 } // one emissive id in the surface band
    }
}

fn bench_single_column(c: &mut Criterion) {
    let col = realistic_terrain_column();
    let props = TimingProps;

    // Anti-vacuity control: prove the fixture actually exercises horizontal
    // propagation before timing it. This cannot be seen by reading the rest of
    // this function — only by checking the thing it was pointed at, which is
    // exactly the failure mode `light_exercises_propagation` exists to catch.
    let probe = compute_column_light(&col, &props);
    assert!(
        light_exercises_propagation(&probe),
        "fixture produces uniform light -- this bench would be measuring nothing"
    );

    for _ in 0..8 {
        black_box(compute_column_light(black_box(&col), black_box(&props)));
    }

    const ITERS: usize = 200;
    let mut best = f64::INFINITY;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let s = Instant::now();
        let light = compute_column_light(black_box(&col), black_box(&props));
        best = best.min(s.elapsed().as_secs_f64() * 1e3);
        black_box(light);
    }
    let mean_ms = t0.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    println!(
        "light_propagation single column: mean {mean_ms:.3} ms, best {best:.3} ms over {ITERS} calls"
    );

    support::record(support::Record {
        bench: "light_propagation",
        metric: "single_column_mean_ms",
        scene: "realistic 24-section terrain column, isolated (no neighbours)",
        value: mean_ms,
        unit: "ms",
    });

    c.bench_function("world/light_single_column", |b| {
        b.iter(|| black_box(compute_column_light(black_box(&col), black_box(&props))))
    });
}

fn bench_neighbourhood(c: &mut Criterion) {
    let center = realistic_terrain_column();
    let n = realistic_terrain_column();
    let props = TimingProps;
    let hood = Neighbourhood::new(&center)
        .with(-1, 0, &n)
        .with(1, 0, &n)
        .with(0, -1, &n)
        .with(0, 1, &n)
        .with(-1, -1, &n)
        .with(1, -1, &n)
        .with(-1, 1, &n)
        .with(1, 1, &n);

    assert!(
        light_exercises_propagation(&compute_column_light_with_neighbours(&hood, &props)),
        "fixture produces uniform light -- this bench would be measuring nothing"
    );

    for _ in 0..4 {
        black_box(compute_column_light(black_box(&center), black_box(&props)));
        black_box(compute_column_light_with_neighbours(black_box(&hood), black_box(&props)));
    }

    const ITERS: usize = 60;
    let mut single_best = f64::INFINITY;
    let mut hood_best = f64::INFINITY;
    for _ in 0..ITERS {
        let t0 = Instant::now();
        black_box(compute_column_light(black_box(&center), black_box(&props)));
        single_best = single_best.min(t0.elapsed().as_secs_f64() * 1e3);

        let t1 = Instant::now();
        black_box(compute_column_light_with_neighbours(black_box(&hood), black_box(&props)));
        hood_best = hood_best.min(t1.elapsed().as_secs_f64() * 1e3);
    }
    let factor = hood_best / single_best;
    println!(
        "light_propagation 3x3 neighbourhood: single best {single_best:.3} ms, hood best {hood_best:.3} ms ({factor:.2}x single)"
    );

    support::record(support::Record {
        bench: "light_propagation",
        metric: "neighbourhood_single_best_ms",
        scene: "3x3, isolated single-column baseline",
        value: single_best,
        unit: "ms",
    });
    support::record(support::Record {
        bench: "light_propagation",
        metric: "neighbourhood_hood_best_ms",
        scene: "3x3 realistic terrain neighbourhood",
        value: hood_best,
        unit: "ms",
    });
    support::record(support::Record {
        bench: "light_propagation",
        metric: "neighbourhood_factor_vs_single",
        scene: "3x3 realistic terrain neighbourhood",
        value: factor,
        unit: "x",
    });

    let mut group = c.benchmark_group("world/light_neighbourhood");
    group.bench_function("single_column_baseline", |b| {
        b.iter(|| black_box(compute_column_light(black_box(&center), black_box(&props))))
    });
    group.bench_function("3x3_neighbourhood", |b| {
        b.iter(|| black_box(compute_column_light_with_neighbours(black_box(&hood), black_box(&props))))
    });
    group.finish();
}

criterion_group!(benches, bench_single_column, bench_neighbourhood);
criterion_main!(benches);
