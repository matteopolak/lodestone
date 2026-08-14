//! Client-side light-*application* throughput: the cost of
//! [`World::merge_light`], the `light_update` seam that
//! writes a server-computed [`LightPatch`] onto an already-loaded
//! [`ChunkColumn`]/[`ColumnLight`] — a different code path from
//! `light_propagation.rs`'s [`compute_column_light`], which *derives* an
//! answer rather than applying one. `lodestone-shell/src/net.rs`'s module doc
//! draws this split explicitly: "MP consumes server light; SP computes it."
//! This bench is the MP half's light-specific consumer, complementing
//! `chunk_load.rs`'s block-and-metadata consumer in this same directory.
//!
//! # Why this is expected to be cheap, and the bench proves it rather than
//! assuming it
//!
//! `merge_light`'s own doc comment describes it as a sparse overwrite: it
//! looks up the target chunk (one `HashMap` lookup), then replaces only the
//! light sections the patch names — no scan of other chunks, no
//! recomputation, no allocation beyond what `LightData` already owns. That
//! shape rules out the "accidentally quadratic in loaded columns" bug
//! previously found in camera uniforms *by construction* (each call touches exactly one
//! chunk's `HashMap` entry), but the epic's method rule is to measure, not
//! assert, so this bench times a full render-distance-scale batch alongside
//! the single-column number and reports the ratio against chunk count,
//! rather than describing the shape and stopping there.
//!
//! Run with: `cargo bench -p lodestone-world --bench light_application`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, LightData, LightPatch, LoadedChunk, NibbleArray,
    PaletteKind, World,
};

const MIN_Y: i32 = -64;
const SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.

/// A minimal but non-trivial resident column: this bench is about applying
/// light, not building terrain, so the column shape only needs to be real
/// enough that `World::load` accepts it.
fn minimal_column() -> ChunkColumn {
    let mut col = ChunkColumn::new(MIN_Y, SECTIONS, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0);
    for z in 0..16 {
        for x in 0..16 {
            col.set_block(x, MIN_Y, z, 1);
        }
    }
    col
}

fn empty_light() -> ColumnLight {
    ColumnLight::new(SECTIONS)
}

fn realistic_chunk() -> LoadedChunk {
    LoadedChunk::new(minimal_column(), empty_light(), lodestone_world::Heightmaps::new(), Vec::new())
}

/// A realistic `light_update`: a handful of sections carry a genuinely varied
/// array (the boundary at the terrain surface, which is where light actually
/// varies cell-to-cell) and the rest are `Uniform`, matching real wire
/// payloads far more than an all-`Values` or all-absent patch would — see
/// `chunk_light_decode.rs`'s fixture for the sibling wire-decode benchmark
/// making the same choice.
fn realistic_patch() -> LightPatch {
    let mut patch = LightPatch::new();
    let light_sections = SECTIONS + 2; // boundary sections above/below the world.
    for i in 0..light_sections {
        if i >= 10 {
            patch.set_sky(i, LightData::Uniform(15));
        } else {
            patch.set_sky(i, LightData::Uniform(0));
        }
    }
    // Two boundary sections carry real per-cell variation, matching a lit
    // surface band rather than a flat tag everywhere.
    for i in 8..=9 {
        let mut sky = NibbleArray::filled(8);
        let mut block = NibbleArray::filled(0);
        for idx in (0..NibbleArray::LEN).step_by(37) {
            sky.set(idx, (idx % 16) as u8);
            block.set(idx, (idx % 5) as u8);
        }
        patch.set_sky(i, LightData::Values(sky));
        patch.set_block(i, LightData::Values(block));
    }
    patch
}

fn bench_single_column(c: &mut Criterion) {
    let mut world = World::new();
    let pos = ChunkPos::new(0, 0);
    world.load(pos, realistic_chunk());

    const ITERS: usize = 2000;
    for _ in 0..20 {
        black_box(world.merge_light(pos, realistic_patch()));
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        black_box(world.merge_light(black_box(pos), black_box(realistic_patch())));
    }
    let mean_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
    println!("light_application single column: {mean_us:.3} us/call mean over {ITERS} calls");
    support::record(support::Record {
        bench: "light_application",
        metric: "single_column_mean_us",
        scene: "one already-loaded column, realistic sparse light_update patch",
        value: mean_us,
        unit: "us",
    });

    c.bench_function("world/light_application_single_column", |b| {
        b.iter(|| black_box(world.merge_light(black_box(pos), black_box(realistic_patch()))))
    });
}

/// Applies the same shape of patch across a whole render-distance-scale
/// loaded set, at two sizes, so a per-call cost that scales with the number
/// of *other* loaded columns (rather than staying flat) shows up as a
/// superlinear ratio, which is exactly the batch-scale question this bench
/// exists to answer.
fn bench_batch_scaling(c: &mut Criterion) {
    let small: Vec<ChunkPos> = (-2..=2)
        .flat_map(|cz| (-2..=2).map(move |cx| ChunkPos::new(cx, cz)))
        .collect(); // 5x5 = 25 columns.
    let large: Vec<ChunkPos> = (-8..=8)
        .flat_map(|cz| (-8..=8).map(move |cx| ChunkPos::new(cx, cz)))
        .collect(); // 17x17 = 289 columns (render distance 8).

    let time_batch = |positions: &[ChunkPos]| -> f64 {
        let mut world = World::new();
        for &pos in positions {
            world.load(pos, realistic_chunk());
        }
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t0 = Instant::now();
            for &pos in positions {
                black_box(world.merge_light(pos, realistic_patch()));
            }
            best = best.min(t0.elapsed().as_secs_f64());
        }
        best
    };

    let t_small = time_batch(&small);
    let t_large = time_batch(&large);
    let ratio = t_large / t_small;
    let column_ratio = large.len() as f64 / small.len() as f64;
    println!(
        "light_application batch: {}x columns ({} -> {}) -> {:.2}x time (best-of-5) — expect close to {:.1}x for linear cost",
        column_ratio, small.len(), large.len(), ratio, column_ratio
    );
    println!(
        "  render-distance-8 batch (289 columns): {:.1} ms total, {:.2} us/column",
        t_large * 1e3,
        t_large * 1e6 / large.len() as f64
    );

    let scene = format!("small={} large={}", small.len(), large.len());
    support::record(support::Record {
        bench: "light_application",
        metric: "batch_linearity_ratio_vs_expected",
        scene: &scene,
        value: ratio / column_ratio,
        unit: "x",
    });
    support::record(support::Record {
        bench: "light_application",
        metric: "rd8_batch_us_per_column",
        scene: "render distance 8 (289 columns)",
        value: t_large * 1e6 / large.len() as f64,
        unit: "us",
    });

    let mut world_small = World::new();
    for &pos in &small {
        world_small.load(pos, realistic_chunk());
    }
    let mut world_large = World::new();
    for &pos in &large {
        world_large.load(pos, realistic_chunk());
    }

    let mut group = c.benchmark_group("world/light_application_batch");
    group.bench_function("25_columns", |b| {
        b.iter(|| {
            for &pos in &small {
                black_box(world_small.merge_light(pos, realistic_patch()));
            }
        })
    });
    group.bench_function("289_columns", |b| {
        b.iter(|| {
            for &pos in &large {
                black_box(world_large.merge_light(pos, realistic_patch()));
            }
        })
    });
    group.finish();
}

criterion_group!(benches, bench_single_column, bench_batch_scaling);
criterion_main!(benches);
