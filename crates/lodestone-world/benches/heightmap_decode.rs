//! Heightmap-decode throughput for the real per-chunk consumer:
//! [`Heightmaps::decode`], called directly by
//! `protocol/v26-2/src/packets/chunk.rs`'s `decode_heightmaps` (the
//! `#[mc(decode_with = "decode_heightmaps")]` field on `LevelChunkWithLight`)
//! for every `level_chunk_with_light` packet. This is the "heightmap work"
//! half of client-side chunk loading the benchmark epic asks for, living in
//! `lodestone-world` rather than the version crate — the typed-list/packed-long
//! decode is version-free; only the *framing* choice (this form vs. the legacy
//! NBT-keyed form) is version-conditional, and a version crate has already made
//! that choice by the time it calls this function.
//!
//! # Why this one is not vulnerable to the "vacuous world" trap
//!
//! Unlike light or terrain data, [`Heightmaps::decode`]'s cost is **not** a
//! function of its content. It walks exactly `count` maps and, for each,
//! unpacks exactly `PackedArray::long_count(height_bits(world_height), 256)`
//! fixed-width longs — a straight-line loop whose iteration count is set by
//! `count` and `world_height` alone. An all-zero heightmap and a genuinely
//! varied-terrain one cost the identical number of reads: no early exit, no
//! branch on value, no allocation sized by content. So — unlike this crate's
//! own `light_propagation` bench, which must actively guard against a
//! flat/uniform fixture measuring nothing — realistic-looking heights below are
//! for the fixture's honesty, not because a degenerate one would report a
//! different (and misleadingly good) number. Nobody has to take that argument
//! on faith while reading the printed numbers, though: real terrain heights are
//! used regardless.
//!
//! # Evidence caveat
//!
//! No captured live-server heightmap bytes exist in this repo — the same gap
//! `lodestone-v26-2`'s `chunk_light_decode`/`nbt_decode` document — so this
//! builds a wire-accurate payload with [`Heightmaps::encode`] itself. That is a
//! real limitation for *correctness* (a self-round-trip can validate a shared
//! wrong understanding, per `CLAUDE.md`'s evidence standard) but not for
//! *throughput*, which is driven by container shape, not by which encoder
//! produced the bytes.
//!
//! Run with: `cargo bench -p lodestone-world --bench heightmap_decode`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_core::{Reader, Writer};
use lodestone_world::{Heightmap, Heightmaps};

const WORLD_HEIGHT: u32 = 384; // 1.18+ overworld (24 sections, y = -64..320).

/// MOTION_BLOCKING + WORLD_SURFACE — vanilla's two heightmap types that are
/// always present — each carrying genuinely varied per-column heights (not a
/// flat plane), matching a real terrain column's surface.
fn realistic_heightmaps() -> Heightmaps {
    let mut maps = Heightmaps::new();
    let mut motion = Heightmap::new(WORLD_HEIGHT);
    let mut surface = Heightmap::new(WORLD_HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            let h = 104 + (x * 7 + z * 3) % 12; // varied, ~104..115
            motion.set(x, z, h as u32);
            surface.set(x, z, (h + 1) as u32);
        }
    }
    maps.insert(0, motion); // registry ids are opaque at this layer
    maps.insert(4, surface);
    maps
}

fn bench_decode_throughput(c: &mut Criterion) {
    let maps = realistic_heightmaps();
    let mut w = Writer::default();
    maps.encode(&mut w);
    let bytes = w.into_vec();

    // Prove it actually decodes cleanly, byte-exact, before timing it — a
    // bench that times a decode error would be measuring the wrong thing.
    {
        let mut r = Reader::new(&bytes);
        let decoded = Heightmaps::decode(WORLD_HEIGHT, &mut r).expect("bench payload decodes");
        r.ensure_empty().expect("zero trailing bytes");
        assert_eq!(decoded, maps, "decode must round-trip the encoded fixture exactly");
        black_box(&decoded);
    }

    let scene = format!("world_height={WORLD_HEIGHT} maps=2 {} bytes/payload", bytes.len());

    // One-shot diagnostic, recorded with metadata.
    const ITERS: usize = 2000;
    for _ in 0..50 {
        let mut r = Reader::new(&bytes);
        black_box(Heightmaps::decode(WORLD_HEIGHT, &mut r).unwrap());
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let mut r = Reader::new(black_box(&bytes));
        black_box(Heightmaps::decode(WORLD_HEIGHT, &mut r).unwrap());
    }
    let mean_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
    support::record(support::Record {
        bench: "heightmap_decode",
        metric: "decode_mean_us",
        scene: &scene,
        value: mean_us,
        unit: "us",
    });
    println!(
        "Heightmaps::decode: {mean_us:.3} us/payload mean over {ITERS} iters ({} bytes/payload)",
        bytes.len()
    );

    c.bench_function("world/heightmaps_decode", |b| {
        b.iter(|| {
            let mut r = Reader::new(black_box(&bytes));
            black_box(Heightmaps::decode(WORLD_HEIGHT, &mut r).unwrap())
        })
    });
}

criterion_group!(benches, bench_decode_throughput);
criterion_main!(benches);
