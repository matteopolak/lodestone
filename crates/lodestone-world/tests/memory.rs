//! Measures the real heap footprint of representative chunk columns.
//!
//! Run with `cargo test -p lodestone-world --test memory -- --nocapture` to see
//! the printed numbers. The assertions guard the qualitative claims: a
//! flat-world column costs a tiny fraction of a fully dense one, and a dense
//! column stays within the paletted-storage budget rather than the naive
//! `2 bytes * 98304` layout.

use lodestone_world::{
    ChunkColumn, ColumnLight, LightData, LightProperties, NibbleArray, Neighbourhood, PaletteKind,
    compute_column_light, compute_column_light_with_neighbours,
};
use std::hint::black_box;
use std::time::Instant;

const MODERN_MIN_Y: i32 = -64;
const MODERN_SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.

fn modern_column() -> ChunkColumn {
    ChunkColumn::new(
        MODERN_MIN_Y,
        MODERN_SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    )
}

/// Naive lower bound for the block data alone: one u16 per block.
const NAIVE_BLOCK_BYTES: usize = MODERN_SECTIONS * 4096 * 2;

#[test]
fn measure_flatworld_like_column() {
    // Flat-world profile: bedrock, three dirt layers, a grass layer, then air.
    // Only the lowest section carries blocks; everything above is elided air.
    let mut col = modern_column();
    let bedrock = 1u32;
    let dirt = 10u32;
    let grass = 9u32;

    for x in 0..16 {
        for z in 0..16 {
            col.set_block(x, -64, z, bedrock);
            for y in -63..-60 {
                col.set_block(x, y, z, dirt);
            }
            col.set_block(x, -60, z, grass);
        }
    }

    let bytes = col.heap_bytes();
    println!(
        "flatworld-like column heap: {bytes} bytes ({} allocated sections)",
        col.allocated_sections()
    );
    println!("  naive block-only baseline: {NAIVE_BLOCK_BYTES} bytes");

    // Only one section is populated; the rest are None.
    assert_eq!(col.allocated_sections(), 1);
    // Comfortably under 10 KiB, versus ~192 KiB naive.
    assert!(
        bytes < 10 * 1024,
        "flatworld column unexpectedly large: {bytes}"
    );
    assert!(
        bytes * 15 < NAIVE_BLOCK_BYTES,
        "no meaningful saving vs naive"
    );
}

#[test]
fn measure_dense_varied_column() {
    // Worst-ish case: every section filled with enough variety to force direct
    // storage in the block container. This is the upper bound our layout admits.
    let mut col = modern_column();
    for s in 0..MODERN_SECTIONS {
        let base_y = MODERN_MIN_Y + (s as i32) * 16;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    // A varied but deterministic id well above any palette floor.
                    let id = ((x + z * 16 + y * 256 + s * 4096) % 4000 + 1) as u32;
                    col.set_block(x, base_y + y as i32, z, id);
                }
            }
        }
    }

    let bytes = col.heap_bytes();
    let per_column_kib = bytes as f64 / 1024.0;
    println!("dense/varied column heap: {bytes} bytes ({per_column_kib:.1} KiB)");
    // Project a render distance of 32 (4225 columns) of block data.
    let rd32 = bytes * 4225;
    println!(
        "  projected at render distance 32 (4225 columns): {:.1} MiB",
        rd32 as f64 / (1024.0 * 1024.0)
    );

    assert_eq!(col.allocated_sections(), MODERN_SECTIONS);
    // Direct block storage is 15 bits => 1024 longs => 8192 bytes per section.
    // 24 sections plus biomes and the section vector: keep it near that budget.
    assert!(
        bytes < 220 * 1024,
        "dense column exceeded direct-storage budget: {bytes}"
    );
}

#[test]
fn measure_realistic_terrain_column() {
    // A more life-like column: solid stone below the surface (single-valued or
    // tiny palette per section), a shallow varied surface band, air above.
    let mut col = modern_column();
    let stone = 1u32;

    // Fill y = -64..40 with stone (full sections become single-valued).
    for y in -64..40 {
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, y, z, stone);
            }
        }
    }
    // A varied surface band y = 40..48 with a handful of block types.
    for y in 40..48 {
        for z in 0..16 {
            for x in 0..16 {
                let id = 1 + ((x + z + (y as usize)) % 6) as u32;
                col.set_block(x, y, z, id);
            }
        }
    }

    let bytes = col.heap_bytes();
    println!(
        "realistic terrain column heap: {bytes} bytes ({} allocated sections)",
        col.allocated_sections()
    );
    let rd32 = bytes * 4225;
    println!(
        "  projected at render distance 32 (4225 columns): {:.1} MiB",
        rd32 as f64 / (1024.0 * 1024.0)
    );

    // Solid-stone sections are single-valued (0 heap); only the surface band and
    // the partially filled boundary sections allocate.
    assert!(
        bytes < 32 * 1024,
        "realistic column unexpectedly large: {bytes}"
    );
}

const RD32_COLUMNS: usize = 4225;

/// Naive light lower bound: 2048 bytes per section, two light types, all sections
/// materialised. For a 24-section column that is 24 * 2 * 2048 = 98304 bytes.
const NAIVE_LIGHT_BYTES: usize = MODERN_SECTIONS * 2 * 2048;

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[test]
fn measure_realistic_light_column() {
    // Realistic terrain lighting: block light is uniform 0 (dark) everywhere
    // except a couple of surface sections; sky light is uniform 15 (full) above
    // the surface and uniform 0 below it, with a thin varied band at the terrain
    // boundary. This is the overwhelmingly common shape and should elide to a
    // handful of arrays.
    let mut light = ColumnLight::new(MODERN_SECTIONS);
    let n = light.light_section_count();

    for i in 0..n {
        // Section index 0 is below the world; surface is roughly light section 8.
        *light.block_mut(i) = LightData::Uniform(0);
        *light.sky_mut(i) = if i >= 9 {
            LightData::Uniform(15)
        } else {
            LightData::Uniform(0)
        };
    }
    // Two varied surface sections carry real arrays for each light type.
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

    let bytes = light.heap_bytes();
    println!("realistic light column heap: {bytes} bytes");
    println!("  naive all-materialised baseline: {NAIVE_LIGHT_BYTES} bytes");
    println!(
        "  projected at render distance 32 ({RD32_COLUMNS} columns): {:.1} MiB (naive {:.1} MiB)",
        mib(bytes * RD32_COLUMNS),
        mib(NAIVE_LIGHT_BYTES * RD32_COLUMNS)
    );

    // Only four arrays allocate (sky+block across two sections); the rest are
    // one-byte tags. Comfortably under 10 KiB versus ~96 KiB naive.
    assert!(
        bytes < 10 * 1024,
        "realistic light unexpectedly large: {bytes}"
    );
}

#[test]
fn measure_dense_light_column() {
    // Worst case: every light section, both types, holds a genuinely varied
    // array. This is the upper bound and matches the naive footprint.
    let mut light = ColumnLight::new(MODERN_SECTIONS);
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

    let bytes = light.heap_bytes();
    println!("dense light column heap: {bytes} bytes");
    println!(
        "  projected at render distance 32 ({RD32_COLUMNS} columns): {:.1} MiB",
        mib(bytes * RD32_COLUMNS)
    );

    // 26 light sections * 2 types * 2048 bytes = 106496 bytes of arrays.
    assert!(
        bytes >= n * 2 * 2048,
        "dense light smaller than array total"
    );
    assert!(
        bytes < n * 2 * 2048 + 4096,
        "dense light has unexpected overhead"
    );
}

/// Opacity/emission for the recompute timing: anything non-air is fully opaque,
/// air is transparent, one id emits like a torch so the block-light seed path is
/// exercised too. Mirrors the injected-provider seam the real engine uses.
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

/// A life-like full-height column: stone below the surface, a varied surface
/// band, air above — the same shape as `measure_realistic_terrain_column`, which
/// is what a player standing in the world actually holds.
fn realistic_terrain_column() -> ChunkColumn {
    let mut col = modern_column();
    let stone = 1u32;
    for y in -64..40 {
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

/// Puts a real number on the from-zero light recompute that a single block
/// update currently triggers. Correct-by-construction removal recomputes the
/// whole column; this measures whether that is cheap enough to leave deferred or
/// whether incremental removal needs to move up the list — a deferral with a
/// number is a decision, one without is a worry.
#[test]
fn measure_light_recompute_cost() {
    let col = realistic_terrain_column();
    let props = TimingProps;

    // Warm up (allocator, caches) then time a batch and report per-call.
    for _ in 0..8 {
        black_box(compute_column_light(black_box(&col), black_box(&props)));
    }

    const ITERS: usize = 200;
    let mut best = f64::INFINITY;
    let start = Instant::now();
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let light = compute_column_light(black_box(&col), black_box(&props));
        let dt = t0.elapsed().as_secs_f64() * 1e3; // ms
        best = best.min(dt);
        black_box(light);
    }
    let mean_ms = start.elapsed().as_secs_f64() * 1e3 / ITERS as f64;

    println!("light recompute (from zero) over a realistic 24-section column:");
    println!("  mean {mean_ms:.3} ms/call, best {best:.3} ms/call over {ITERS} calls");
    println!(
        "  a player mining ~1 block/several ticks (~5/s) costs ~{:.3} ms/s of light recompute",
        mean_ms * 5.0
    );

    // Sanity ceiling only — timing is machine- and contention-dependent, so this
    // catches a pathological regression (a full recompute creeping into tens of
    // ms), not a microbenchmark target. The printed number is the deliverable.
    assert!(
        mean_ms < 50.0,
        "column light recompute unexpectedly slow: {mean_ms:.3} ms"
    );
}

/// Puts a number on the neighbour-aware compute that closes the cross-chunk seam.
/// It floods over a 3×3 field (9× the cells), so it should cost several times a
/// single column — the figure that decides whether an incremental seam
/// re-propagation (touching only the two columns at a changed boundary) is worth
/// building, or whether a full neighbourhood relight is cheap enough to leave
/// deferred behind the same interface.
#[test]
fn measure_neighbour_light_cost() {
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

    println!("cross-chunk (3×3 neighbourhood) light over a realistic 24-section centre:");
    println!("  single column best {single_best:.3} ms/call");
    println!(
        "  3×3 neighbourhood best {hood_best:.3} ms/call  ({:.1}× single)",
        hood_best / single_best
    );

    // Sanity ceiling only; the printed factor is the deliverable — so the ceiling
    // is expressed against that factor rather than as an absolute millisecond
    // count.
    //
    // It used to be `hood_best < 200.0`, which was **inconsistent with the
    // deliverable it guards**. A 3x3 neighbourhood is nine columns and the
    // measured factor is ~8.7x, i.e. essentially exactly linear — so a 200 ms
    // ceiling silently asserts `single_best < 22.2 ms`, an undocumented
    // constraint on an absolute machine-speed number that nothing in this test
    // is about. Measured here at 25.083 ms single / 218.564 ms hood: a perfectly
    // healthy 8.7x that failed the ceiling purely because the machine was busy.
    //
    // This is the **duration** species from `CLAUDE.md` — test lifetime measured
    // against wall-clock, unjudgeable by reading the assert. A ratio is the
    // load-robust form: it still catches the thing worth catching (neighbour
    // light going *superlinear* in column count, e.g. re-walking the centre per
    // neighbour), and it cannot be reddened by a busy CPU, because both halves
    // are measured in the same conditions microseconds apart.
    let factor = hood_best / single_best;
    assert!(
        factor < 12.0,
        "neighbourhood light is superlinear in column count: {factor:.1}x single \
         for a 9-column neighbourhood ({hood_best:.3} ms vs {single_best:.3} ms)"
    );
}
