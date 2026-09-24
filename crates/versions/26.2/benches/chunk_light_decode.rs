//! Decode-throughput benchmark for `minecraft:level_chunk_with_light` — the
//! highest-volume packet in this codebase: one per loaded chunk, scaling with
//! render distance.
//!
//! # Evidence caveat — read before trusting this as a correctness oracle
//!
//! There is **no captured live server chunk-with-light payload** checked into
//! this crate's `tests/fixtures/` (unlike `tool_component_explicit.hex` etc.);
//! `tests/chunk_decode.rs` itself is hermetic, round-tripping a
//! synthetically-built packet through `LevelChunkWithLight::decode`. This bench
//! follows that same precedent — it builds a wire-format-accurate packet with
//! our own encoder (mirroring `tests/chunk_decode.rs`'s `encode_packet`, plus
//! populated light data that test's trivial "all absent" case does not
//! exercise), not against captured server bytes.
//!
//! That is a real limitation **for correctness**, per `CLAUDE.md`'s evidence
//! standard (a self-round-trip can validate a wrong shared understanding
//! rather than catch it) — but it does not weaken this as a *throughput*
//! measurement: cost is driven by container shape (bits-per-entry, palette
//! size, section count, light-array presence), which this construction
//! matches exactly, not by whether the specific IDs are real vanilla output.
//! Capturing a live `level_chunk_with_light` payload (needs a running 26.2
//! oracle, out of scope for a static check pass) would let this same harness
//! benchmark against real bytes without changing its shape.
//!
//! Run with: `cargo bench -p lodestone-v26-2 --bench chunk_light_decode`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_core::{Reader, Writer};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_world::{ColumnLight, Heightmaps, PalettedContainer};

/// Builds a realistic (but self-authored — see module docs) chunk packet: every
/// section carries an indirect-palette block distribution across several
/// distinct state ids (not a single-value uniform section, which would
/// short-circuit the container's bit-packing cost), and every light section
/// carries an explicit, spatially-varying nibble array rather than the trivial
/// `Missing`/`Uniform` forms `tests/chunk_decode.rs` uses — a resident chunk in
/// a lit world has real per-block light data on the wire, not an all-absent
/// column.
fn encode_realistic_packet(x: i32, z: i32, shape: &ChunkShape) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);

    Heightmaps::new().encode(&mut w);

    // A handful of distinct block-state ids, spatially varied, so every
    // section's container is a genuine indirect palette (a real terrain
    // section: stone/dirt/ore/air-pocket variety), not one repeated value.
    const STATE_IDS: [u32; 8] = [1, 4, 7, 12, 55, 90, 140, 310];

    let mut blob = Writer::default();
    for section in 0..shape.section_count {
        let mut values = vec![shape.air_id; 4096];
        // Bottom third of the column: solid varied terrain. Middle: sparse
        // (caves). Top: mostly air (sky). Mirrors a real column's vertical
        // profile closely enough to exercise realistic bit-packing without
        // depending on the real generator (out of this crate's scope).
        let fill_fraction = match section {
            s if s < shape.section_count / 3 => 0.95,
            s if s < 2 * shape.section_count / 3 => 0.35,
            _ => 0.05,
        };
        for (i, slot) in values.iter_mut().enumerate() {
            let threshold = (fill_fraction * 4096.0) as usize;
            if i % 4096 < threshold {
                *slot = STATE_IDS[(i + section) % STATE_IDS.len()];
            }
        }
        let block_container = PalettedContainer::from_values(shape.block_kind, &values);
        let non_air = values.iter().filter(|&&v| v != shape.air_id).count() as i16;
        blob.i16(non_air);
        blob.i16(0); // fluid count
        block_container.encode(&mut blob);
        // Biomes: two distinct biome ids per column (real overworld columns
        // straddle biome boundaries far more than a single-biome section).
        let biome_values: Vec<u32> = (0..64).map(|i| u32::from(i % 3 == 0)).collect();
        PalettedContainer::from_values(shape.biome_kind, &biome_values).encode(&mut blob);
    }
    let blob = blob.into_vec();
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);

    // Block entities: empty (chests/signs are a separate, sparser benchmark
    // concern — not this packet's dominant cost).
    w.var_i32(0);

    let mut light = ColumnLight::new(shape.section_count);
    for i in 0..light.light_section_count() {
        for index in 0..4096usize {
            // Sky light decays with depth within the section; block light is
            // sparse near a handful of "torch-like" indices. Both forms force
            // the decoder down the explicit-array path for every section.
            let sky = 15u8.saturating_sub(((index / 256) % 16) as u8);
            light.set_sky_light(i, index, sky);
            if index % 173 == 0 {
                light.set_block_light(i, index, 12);
            }
        }
    }
    light.encode(&mut w);

    w.into_vec()
}

fn bench_decode_throughput(c: &mut Criterion) {
    let shape = ChunkShape::overworld_1_21();
    let bytes = encode_realistic_packet(4, -9, &shape);

    // Prove it actually decodes cleanly before timing it — a bench that times
    // a decode error would be measuring the wrong thing.
    {
        let mut r = Reader::new(&bytes);
        let chunk = LevelChunkWithLight::decode(&mut r, &shape).expect("bench packet decodes");
        r.ensure_empty().expect("zero trailing bytes");
        black_box(&chunk);
    }

    let scene = format!("overworld_1_21 {} sections, {} bytes/packet", shape.section_count, bytes.len());

    // One-shot diagnostic, recorded with metadata.
    const ITERS: usize = 500;
    for _ in 0..20 {
        let mut r = Reader::new(&bytes);
        black_box(LevelChunkWithLight::decode(&mut r, &shape).unwrap());
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let mut r = Reader::new(black_box(&bytes));
        black_box(LevelChunkWithLight::decode(&mut r, &shape).unwrap());
    }
    let mean_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
    support::record(support::Record {
        bench: "chunk_light_decode",
        metric: "decode_mean_us",
        scene: &scene,
        value: mean_us,
        unit: "us",
    });
    println!(
        "level_chunk_with_light decode: {mean_us:.1} us/packet mean over {ITERS} iters ({} bytes/packet)",
        bytes.len()
    );

    c.bench_function("protocol/level_chunk_with_light_decode", |b| {
        b.iter(|| {
            let mut r = Reader::new(black_box(&bytes));
            black_box(LevelChunkWithLight::decode(&mut r, &shape).unwrap())
        })
    });
}

criterion_group!(benches, bench_decode_throughput);
criterion_main!(benches);
