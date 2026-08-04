//! Tracked-baseline layer for `lodestone-world`'s per-chunk/per-section
//! memory footprint (issue #78 epic, sub-issue #155).
//!
//! `tests/memory.rs` already has five real, well-designed heap-byte
//! measurements — `measure_flatworld_like_column`, `measure_dense_varied_
//! column`, `measure_realistic_terrain_column`, `measure_realistic_light_
//! column`, `measure_dense_light_column` — each gated only by a loose sanity
//! ceiling (e.g. "under 10 KiB", "15x smaller than the naive baseline").
//! Those ceilings are deliberately generous and stay exactly as they are:
//! this file adds the second, *tracked* layer #155 asks for — the same five
//! fixture shapes, rebuilt here with the identical public constructors
//! (`ChunkColumn`/`ColumnLight`, `PaletteKind`, `synthetic_overworld_column`),
//! feeding every byte count into `support::record` so a future PR is diffed
//! against a number, not just checked against headroom.
//!
//! This is a criterion "bench" in name only — there is nothing to time, only
//! a heap-byte count to read once per fixture, so the `Criterion` handle is
//! used only to keep this file discoverable the same way as every other
//! bench in the harness (`cargo bench -p lodestone-world --bench
//! memory_footprint`) and to get the free `--baseline` machinery for nothing
//! extra. Not a duration measurement, so none of the duration-species traps
//! this harness documents elsewhere apply.
//!
//! Run with: `cargo bench -p lodestone-world --bench memory_footprint`

mod support;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_testsupport::bench_fixtures::synthetic_overworld_column;
use lodestone_world::{ChunkColumn, ColumnLight, LightData, NibbleArray, PaletteKind};

const MIN_Y: i32 = -64;
const SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.
const RD32_COLUMNS: f64 = 4225.0;

fn modern_column() -> ChunkColumn {
    ChunkColumn::new(MIN_Y, SECTIONS, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0)
}

/// Identical shape to `tests/memory.rs::measure_flatworld_like_column`:
/// bedrock, three dirt layers, a grass layer, then air — only the lowest
/// section allocates.
fn flatworld_like_column() -> ChunkColumn {
    let mut col = modern_column();
    for x in 0..16 {
        for z in 0..16 {
            col.set_block(x, -64, z, 1); // bedrock
            for y in -63..-60 {
                col.set_block(x, y, z, 10); // dirt
            }
            col.set_block(x, -60, z, 9); // grass
        }
    }
    col
}

/// Identical shape to `tests/memory.rs::measure_dense_varied_column`: every
/// section filled with enough variety to force direct storage.
fn dense_varied_column() -> ChunkColumn {
    let mut col = modern_column();
    for s in 0..SECTIONS {
        let base_y = MIN_Y + (s as i32) * 16;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let id = ((x + z * 16 + y * 256 + s * 4096) % 4000 + 1) as u32;
                    col.set_block(x, base_y + y as i32, z, id);
                }
            }
        }
    }
    col
}

/// Identical shape to `tests/memory.rs::measure_realistic_light_column`.
fn realistic_light_column() -> ColumnLight {
    let mut light = ColumnLight::new(SECTIONS);
    let n = light.light_section_count();
    for i in 0..n {
        *light.block_mut(i) = LightData::Uniform(0);
        *light.sky_mut(i) = if i >= 9 { LightData::Uniform(15) } else { LightData::Uniform(0) };
    }
    for i in 8..=9 {
        let mut sky = NibbleArray::filled(8);
        let mut block = NibbleArray::filled(0);
        for idx in (0..NibbleArray::LEN).step_by(37) {
            sky.set(idx, (idx % 16) as u8);
            block.set(idx, (idx % 5) as u8);
        }
        *light.sky_mut(i) = LightData::Values(sky);
        *light.block_mut(i) = LightData::Values(block);
    }
    light
}

/// Identical shape to `tests/memory.rs::measure_dense_light_column`: every
/// light section, both types, holds a genuinely varied array.
fn dense_light_column() -> ColumnLight {
    let mut light = ColumnLight::new(SECTIONS);
    let n = light.light_section_count();
    for i in 0..n {
        let mut sky = NibbleArray::filled(0);
        let mut block = NibbleArray::filled(0);
        for idx in 0..NibbleArray::LEN {
            sky.set(idx, (idx % 16) as u8);
            block.set(idx, ((idx / 3) % 16) as u8);
        }
        *light.sky_mut(i) = LightData::Values(sky);
        *light.block_mut(i) = LightData::Values(block);
    }
    light
}

fn record_bytes(metric: &str, scene: &str, bytes: usize) {
    println!("memory_footprint {metric} ({scene}): {bytes} bytes, {:.1} MiB at RD32", bytes as f64 * RD32_COLUMNS / (1024.0 * 1024.0));
    support::record(support::Record {
        bench: "memory_footprint",
        metric,
        scene,
        value: bytes as f64,
        unit: "bytes",
    });
}

fn bench_column_footprints(c: &mut Criterion) {
    record_bytes("flatworld_like_column_heap_bytes", "bedrock/dirt/grass, one allocated section", flatworld_like_column().heap_bytes());
    record_bytes("dense_varied_column_heap_bytes", "every section direct-storage varied", dense_varied_column().heap_bytes());
    record_bytes(
        "realistic_terrain_column_heap_bytes",
        "issue #80 shared Tier 2 fixture (seed=0)",
        synthetic_overworld_column(0).heap_bytes(),
    );
    record_bytes("realistic_light_column_heap_bytes", "lit surface band, uniform elsewhere", realistic_light_column().heap_bytes());
    record_bytes("dense_light_column_heap_bytes", "every light section genuinely varied", dense_light_column().heap_bytes());

    // Criterion needs at least one `bench_function` registration to produce a
    // report; the measurement above already happened and was recorded, so
    // this just re-reads the already-built fixture's byte count (a field
    // read, not a computation) to give criterion something nominal to time.
    let col = dense_varied_column();
    c.bench_function("world/memory_footprint_heap_bytes_read", |b| {
        b.iter(|| std::hint::black_box(col.heap_bytes()))
    });
}

criterion_group!(benches, bench_column_footprints);
criterion_main!(benches);
