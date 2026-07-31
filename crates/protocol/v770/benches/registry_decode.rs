//! Block-state registry resolution benchmark (issue #78 epic, sub-issue #146).
//!
//! Not a wire-packet decode — this crate has no runtime `registry_data`
//! configuration-packet decoder to benchmark (26.2's per-block-state census is
//! baked in at compile time from the real server dump, per `CLAUDE.md`'s data
//! sources; see `src/generated/block_states.rs`'s header). The real per-chunk
//! hot path this data feeds is turning a wire block-state id into a name +
//! properties — `crate::block_states`'s [`block_name`]/[`properties`]
//! zero-heap accessors and the heap-owning [`BlockStateTable`] the
//! version-free [`lodestone_model::BlockStateRegistry`] seam needs. Every
//! non-air block in every decoded chunk goes through one of these paths on its
//! way to the asset baker / mesher, so this is a real, already-measured-never
//! per-block cost, not a synthetic stand-in.
//!
//! Run with: `cargo bench -p lodestone-v770 --bench registry_decode`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_model::BlockStateRegistry;
use lodestone_data::block_states::{BlockStateTable, STATE_COUNT, block_name, properties};

/// Every state id `0..STATE_COUNT`, the same population a full chunk decode's
/// worth of distinct-block-type resolution would touch over time (a chunk
/// itself touches far fewer distinct ids, but the registry's total lookup
/// surface is what a session accumulates across many chunks).
fn all_state_ids() -> Vec<u32> {
    (0..STATE_COUNT).collect()
}

/// Zero-heap accessor throughput: `block_name`/`properties`, the mesher's hot
/// path (no `BlockStateTable` construction, no heap at all).
fn bench_zero_heap_lookup(c: &mut Criterion) {
    let ids = all_state_ids();
    let scene = format!("all {STATE_COUNT} states, zero-heap accessors");

    for _ in 0..5 {
        for &id in &ids {
            black_box(block_name(id));
            black_box(properties(id));
        }
    }

    const ROUNDS: usize = 200;
    let t0 = Instant::now();
    for _ in 0..ROUNDS {
        for &id in &ids {
            black_box(block_name(black_box(id)));
            black_box(properties(black_box(id)));
        }
    }
    let total_lookups = ROUNDS * ids.len();
    let mean_ns = t0.elapsed().as_secs_f64() * 1e9 / total_lookups as f64;
    support::record(support::Record {
        bench: "registry_decode",
        metric: "zero_heap_lookup_mean_ns",
        scene: &scene,
        value: mean_ns,
        unit: "ns",
    });
    println!("zero-heap block_name+properties: {mean_ns:.2} ns/lookup mean over {total_lookups} lookups");

    c.bench_function("protocol/block_states_zero_heap_lookup", |b| {
        b.iter(|| {
            for &id in &ids {
                black_box(block_name(black_box(id)));
                black_box(properties(black_box(id)));
            }
        })
    });
}

/// `BlockStateRegistry::resolve` throughput via `BlockStateTable` — the owned,
/// heap-backed path the asset baker actually calls through the version-free
/// trait seam. Table construction is timed separately (a one-time, per-bake
/// cost per the module's own doc comment) from steady-state `resolve` calls.
fn bench_table_resolve(c: &mut Criterion) {
    let ids = all_state_ids();

    // Table construction: one-shot, recorded separately since callers are
    // told to build once and drop after baking, never per-lookup.
    const BUILD_ROUNDS: usize = 20;
    for _ in 0..3 {
        black_box(BlockStateTable::new());
    }
    let t0 = Instant::now();
    for _ in 0..BUILD_ROUNDS {
        black_box(BlockStateTable::new());
    }
    let build_mean_us = t0.elapsed().as_secs_f64() * 1e6 / BUILD_ROUNDS as f64;
    support::record(support::Record {
        bench: "registry_decode",
        metric: "table_construction_mean_us",
        scene: "BlockStateTable::new() (1,196 identifiers + 6,454 property maps)",
        value: build_mean_us,
        unit: "us",
    });
    println!("BlockStateTable::new(): {build_mean_us:.1} us/build mean over {BUILD_ROUNDS} builds");

    let table = BlockStateTable::new();
    println!(
        "BlockStateTable heap footprint: {} bytes for {STATE_COUNT} states",
        table.heap_bytes()
    );

    for _ in 0..5 {
        for &id in &ids {
            black_box(table.resolve(id));
        }
    }
    const ROUNDS: usize = 200;
    let t0 = Instant::now();
    for _ in 0..ROUNDS {
        for &id in &ids {
            black_box(table.resolve(black_box(id)));
        }
    }
    let total_lookups = ROUNDS * ids.len();
    let mean_ns = t0.elapsed().as_secs_f64() * 1e9 / total_lookups as f64;
    let scene = format!("all {STATE_COUNT} states via BlockStateRegistry::resolve");
    support::record(support::Record {
        bench: "registry_decode",
        metric: "resolve_mean_ns",
        scene: &scene,
        value: mean_ns,
        unit: "ns",
    });
    println!("BlockStateRegistry::resolve: {mean_ns:.2} ns/lookup mean over {total_lookups} lookups");

    c.bench_function("protocol/block_states_table_construction", |b| {
        b.iter(|| black_box(BlockStateTable::new()))
    });
    c.bench_function("protocol/block_states_resolve", |b| {
        b.iter(|| {
            for &id in &ids {
                black_box(table.resolve(black_box(id)));
            }
        })
    });
}

criterion_group!(benches, bench_zero_heap_lookup, bench_table_resolve);
criterion_main!(benches);
