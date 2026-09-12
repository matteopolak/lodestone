//! Production Overworld admission growth curve.
//!
//! This is an ignored measurement, not a timing gate.  It drives the same
//! bundled generator and `ChunkSource` boundary used by the integrated server
//! over row-major and join-ring walks, printing every sample around the point
//! at which cache retention or dependency recomputation could become visible.
//!
//! Run on an otherwise quiet host with:
//!
//! ```text
//! cargo test --release -p lodestone-server --test overworld_growth_profile \
//!   -- --ignored --nocapture
//! ```
//!
//! The report intentionally keeps timing and cache counters together.  A
//! rising latency with no eviction is an intrinsic generation regression; a
//! latency jump coincident with `store_evictions` identifies retention-driven
//! recomputation.  The staged-store wait counters are reset before each arm so
//! a parallel caller cannot make a single-threaded curve appear blocked.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use lodestone_server::{
    overworld_chunk_source, retained_chunk_source_for_view_radius, ChunkSource,
};
use lodestone_worldgen::overworld::store::{reset_wait_stats, wait_stats};

const SEED: i64 = 42;
const GRID_SIDE: i32 = 16;

fn walk(stride: i32) -> Vec<(i32, i32)> {
    (0..GRID_SIDE)
        .flat_map(|cz| (0..GRID_SIDE).map(move |cx| (cx * stride, cz * stride)))
        .collect()
}

/// The server's join admission order: complete Chebyshev rings, with the
/// centre first. Keep this local copy in the measurement so the test remains
/// independent of the private connection scheduler while still exercising the
/// exact order it hands to the production source.
fn ring_walk(radius: i32, limit: usize) -> Vec<(i32, i32)> {
    let mut coords = Vec::with_capacity(limit);
    for ring in 0..=radius {
        for cz in -ring..=ring {
            for cx in -ring..=ring {
                if cx.abs().max(cz.abs()) == ring {
                    coords.push((cx, cz));
                    if coords.len() == limit {
                        return coords;
                    }
                }
            }
        }
    }
    coords
}

fn run_curve<S, F>(label: &str, source: &S, coords: &[(i32, i32)], observe_store: F)
where
    S: ChunkSource,
    F: Fn() -> (usize, usize),
{
    reset_wait_stats();
    let started = Instant::now();
    let mut total_block_bytes = 0usize;
    println!("OVERWORLD_GROWTH arm={label} chunks={}", coords.len());
    for (index, &(cx, cz)) in coords.iter().enumerate() {
        let sample_start = Instant::now();
        let column = source.column(cx, cz);
        total_block_bytes = total_block_bytes.saturating_add(column.blocks_heap_bytes());
        let elapsed_us = sample_start.elapsed().as_micros();
        let (store_len, evictions) = observe_store();

        // Print the full curve for the first two closure-widths, then every
        // sample around the suspected cliff.  The sparse tail keeps logs
        // readable while preserving the first causal transition.
        if index < 32 || (96..=192).contains(&index) || index % 16 == 0 {
            println!(
                "OVERWORLD_GROWTH arm={label} index={index} cx={cx} cz={cz} elapsed_us={elapsed_us} store_len={store_len} evictions={evictions}"
            );
        }
    }
    let elapsed = started.elapsed();
    let stats = wait_stats();
    println!(
        "OVERWORLD_GROWTH arm={label} total_ms={} chunks_per_s={:.3} block_bytes={} waits={} wait_ms={} computes={} compute_ms={}",
        elapsed.as_millis(),
        coords.len() as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE),
        total_block_bytes,
        stats.waits,
        stats.wait_nanos as f64 / 1_000_000.0,
        stats.computes,
        stats.compute_nanos as f64 / 1_000_000.0,
    );
    black_box(total_block_bytes);
}

#[test]
#[ignore = "release production performance measurement"]
fn reports_production_overworld_growth_curve() {
    for (walk_label, coords) in [("contiguous", walk(1)), ("spaced-2", walk(2))] {
        // The raw source is the intrinsic production generator.  Its public
        // generator handle lets the caller correlate a sample curve with
        // staged-store retention without adding a hot-path probe to generation.
        let raw = Arc::new(overworld_chunk_source(SEED));
        run_curve(&format!("raw-{walk_label}"), &raw, &coords, || {
            (
                raw.generator().store_len(),
                raw.generator().store_evictions(),
            )
        });
        println!(
            "OVERWORLD_GROWTH arm=raw-{walk_label} final_store_len={} final_store_evictions={}",
            raw.generator().store_len(),
            raw.generator().store_evictions(),
        );

        // This is the hosted retained-source wrapper used at the server
        // boundary. Keep the inner source in an Arc so its intrinsic counters
        // remain visible after the wrapper takes ownership of the source handle.
        let inner = Arc::new(overworld_chunk_source(SEED));
        let retained = retained_chunk_source_for_view_radius(Arc::clone(&inner), 32);
        run_curve(&format!("retained-r32-{walk_label}"), &retained, &coords, || {
            (
                inner.generator().store_len(),
                inner.generator().store_evictions(),
            )
        });
        println!(
            "OVERWORLD_GROWTH arm=retained-r32-{walk_label} inner_store_len={} inner_store_evictions={}",
            inner.generator().store_len(),
            inner.generator().store_evictions(),
        );
    }
}

#[test]
#[ignore = "release production admission-order measurement"]
fn reports_join_ring_growth_curve() {
    let coords = ring_walk(8, GRID_SIDE as usize * GRID_SIDE as usize);
    let raw = Arc::new(overworld_chunk_source(SEED));
    run_curve("raw-join-ring", &raw, &coords, || {
        (
            raw.generator().store_len(),
            raw.generator().store_evictions(),
        )
    });
    println!(
        "OVERWORLD_GROWTH arm=raw-join-ring final_store_len={} final_store_evictions={}",
        raw.generator().store_len(),
        raw.generator().store_evictions(),
    );
}
