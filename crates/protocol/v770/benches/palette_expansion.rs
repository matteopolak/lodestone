//! Palette-expansion throughput (issue #78 epic, sub-issue #88's middle
//! stage): the cost of resolving a decoded [`PalettedContainer`]'s local
//! indices through its palette to raw block-state ids —
//! [`PalettedContainer::iter`]/`get`, called once per cell by any consumer
//! that needs concrete ids rather than the compact wire form.
//!
//! `PalettedContainer::decode` (already benchmarked end-to-end by
//! `chunk_light_decode.rs` in this crate) does **not** eagerly expand: `get`/
//! `iter` resolve `local index -> global id` lazily on every call (confirmed
//! by reading `container.rs`'s `Storage::Indirect` arm — a palette lookup per
//! cell, not a precomputed dense array). So "decode" and "expand to raw ids"
//! are genuinely two separate costs paid at different times, which is exactly
//! the seam #88 asks to be measured in isolation from `chunk_load.rs`'s
//! `World::load` (which, per its own module doc, only ever sees *already
//! expanded* content — it moves a `ChunkColumn`, it does not walk a
//! container).
//!
//! Two variety shapes, matching `lodestone-world/tests/memory.rs`'s
//! `measure_flatworld_like_column`/`measure_dense_varied_column` split (the
//! same two shapes #88 explicitly asks to reuse): a low-variety section
//! (mostly one value, `Storage::Single` — the cheap case, no palette lookup
//! at all) and a high-variety section (many distinct values, forcing
//! `Storage::Indirect` — a real palette lookup per cell).
//!
//! Run with: `cargo bench -p lodestone-v770 --bench palette_expansion`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_world::{PaletteKind, PalettedContainer};

const ENTRIES: usize = 4096; // one section's worth of cells (16x16x16).

fn low_variety_container() -> PalettedContainer {
    // Mostly air with a handful of distinct non-air ids -- a flat-world-like
    // section, `Storage::Single` or a tiny `Storage::Indirect` palette.
    let mut values = vec![0u32; ENTRIES];
    for (i, slot) in values.iter_mut().enumerate() {
        if i % 256 < 16 {
            *slot = 1 + (i % 4) as u32; // bedrock/dirt/dirt/grass-ish variety.
        }
    }
    PalettedContainer::from_values(PaletteKind::block_states(), &values)
}

fn high_variety_container() -> PalettedContainer {
    // Every cell a distinct-ish id -- forces `Storage::Indirect` (or
    // `Storage::Direct` past the palette-size threshold) with a real
    // per-cell palette lookup.
    let values: Vec<u32> = (0..ENTRIES).map(|i| 1 + (i % 400) as u32).collect();
    PalettedContainer::from_values(PaletteKind::block_states(), &values)
}

fn bench_expansion(c: &mut Criterion) {
    for (name, container) in [
        ("low_variety", low_variety_container()),
        ("high_variety", high_variety_container()),
    ] {
        const ITERS: usize = 2000;
        for _ in 0..20 {
            black_box(container.iter().collect::<Vec<u32>>());
        }
        let t0 = Instant::now();
        for _ in 0..ITERS {
            black_box(black_box(&container).iter().collect::<Vec<u32>>());
        }
        let mean_us = t0.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
        println!(
            "palette_expansion {name}: {mean_us:.3} us/section ({} cells) mean over {ITERS} iters"
        , ENTRIES);
        support::record(support::Record {
            bench: "palette_expansion",
            metric: "expand_section_mean_us",
            scene: &format!("{name} section, {ENTRIES} cells"),
            value: mean_us,
            unit: "us",
        });
    }

    let low = low_variety_container();
    let high = high_variety_container();
    let mut group = c.benchmark_group("protocol/palette_expansion");
    group.bench_function("low_variety", |b| {
        b.iter(|| black_box(black_box(&low).iter().collect::<Vec<u32>>()))
    });
    group.bench_function("high_variety", |b| {
        b.iter(|| black_box(black_box(&high).iter().collect::<Vec<u32>>()))
    });
    group.finish();
}

criterion_group!(benches, bench_expansion);
criterion_main!(benches);
