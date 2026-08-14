//! Resident-set growth over a synthetic session.
//!
//! # The trap this bench is one step away from
//!
//! RSS is a process-wide counter that outlives any gate measuring it, which is
//! exactly `CLAUDE.md`'s **duration species** of vacuous test — the flaw would
//! live in the relationship between the gate's lifetime and the counter, and no
//! amount of reading the assertion would reveal it. Two consequences shape
//! everything below:
//!
//! 1. **Nothing here asserts an absolute RSS.** An absolute reading is mostly a
//!    statement about how much of the binary, the allocator's arenas and the
//!    fixture data happened to be resident, none of which is what this bench
//!    is about. Every recorded figure is a *delta* against a baseline taken
//!    after warm-up, or a cycle-over-cycle growth rate.
//! 2. **The detector is proved before it is trusted.** This bench does that
//!    explicitly: a synthetic session with a deliberately reintroduced leak (one
//!    skipped `unload`) whose growth this measurement *must* observe. That
//!    control is `leaky_arm` below, and the gate is a comparison between the two
//!    arms rather than a threshold on either — so it needs no calibrated
//!    constant and it cannot pass just because the machine happens to be quiet.
//!
//! RSS is not a timing, so unlike most of this harness these numbers are **not**
//! load-sensitive; a busy machine changes how long the bench takes, not how many
//! pages it holds. They are still recorded rather than asserted in absolute
//! terms.
//!
//! # Scope
//!
//! This is the CPU-side, chunk-and-light half of a session: load a
//! render-distance-sized area, unload it, load a *different* area, repeat. The
//! GPU-side counterpart — arena occupancy returning to exactly zero across
//! load/evict cycles — is `lodestone-render`'s `benches/render_submit.rs`
//! (`bench_arena_occupancy`) and `tests/world_mesher_bench.rs`'s
//! `gpu_world_mesher_upload_evict_roundtrip`. Entity spawn/despawn churn is not
//! covered here; it needs the shell's ECS and is tracked separately.
//!
//! Run with: `cargo bench -p lodestone-world --bench session_rss`

mod support;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_testsupport::bench_fixtures::{MODERN_SECTIONS, synthetic_overworld_column};
use lodestone_world::{ChunkPos, ColumnLight, Heightmaps, LoadedChunk, World};

/// Chunk radius per cycle. 81 columns, each carrying a full light column, is
/// enough that one cycle's footprint is megabytes rather than kilobytes — a
/// leak has to be visible above page-granularity noise for the control to mean
/// anything.
const RD: i32 = 4;
/// Cycles per arm. The first is discarded as warm-up (the allocator has not
/// reached steady state and would be charged to "growth").
const CYCLES: usize = 6;

fn rss_bytes() -> u64 {
    memory_stats::memory_stats().map_or(0, |s| s.physical_mem as u64)
}

/// One column's worth of loadable state, with real light data so the per-cycle
/// footprint is dominated by something a leak would actually retain.
fn loaded_chunk(seed: u64) -> LoadedChunk {
    let column = synthetic_overworld_column(seed);
    let mut light = ColumnLight::new(MODERN_SECTIONS);
    // Touch every section's light so the nibble arrays are really allocated; a
    // `ColumnLight::new` whose sections stayed lazily empty would make this
    // bench's footprint a fraction of a real session's and shrink the leak
    // signal the control depends on.
    for i in 0..MODERN_SECTIONS {
        light.set_sky_light(i, 0, 15);
        light.set_block_light(i, 0, 7);
    }
    LoadedChunk::new(column, light, Heightmaps::default(), Vec::new())
}

/// Loads the `RD`-radius area centred on `(centre_x, 0)` into `world`.
fn load_area(world: &mut World, centre_x: i32) -> usize {
    let mut n = 0;
    for cz in -RD..=RD {
        for cx in -RD..=RD {
            let pos = ChunkPos::new(centre_x + cx, cz);
            world.load(pos, loaded_chunk((cx.unsigned_abs() as u64) * 31 + cz.unsigned_abs() as u64));
            n += 1;
        }
    }
    n
}

fn unload_area(world: &mut World, centre_x: i32) {
    for cz in -RD..=RD {
        for cx in -RD..=RD {
            world.unload(ChunkPos::new(centre_x + cx, cz));
        }
    }
}

/// Runs `CYCLES` load/unload cycles, each over a *different* area, and returns
/// (RSS after cycle 1, RSS after the last cycle, columns per cycle).
///
/// `leak` skips the unload, which is the deliberately-reintroduced leak used
/// as the control. Everything else about the two arms is identical.
fn churn(leak: bool) -> (u64, u64, usize) {
    let mut world = World::new();
    let mut after_first = 0u64;
    let mut columns = 0usize;

    for cycle in 0..CYCLES {
        // A different area every cycle, so a leak accumulates instead of
        // overwriting the same keys (which would hide it entirely).
        let centre = 1000 * (cycle as i32 + 1);
        columns = load_area(&mut world, centre);
        black_box(world.len());
        if !leak {
            unload_area(&mut world, centre);
        }
        if cycle == 0 {
            after_first = rss_bytes();
        }
    }

    black_box(world.len());
    (after_first, rss_bytes(), columns)
}

/// Does RSS return to a plateau across load/unload churn, or ratchet upward?
///
/// The gate is the *ratio between the two arms*, not a threshold on either.
/// A healthy session's growth across cycles 2..N should be a small fraction of
/// the leaky session's, because the leaky one retains one full area per cycle.
/// Expressing it as a comparison means there is no tuned byte constant to go
/// stale, and it cannot be satisfied by a quiet machine.
fn bench_session_rss(_c: &mut Criterion) {
    // Warm-up arm, discarded: the first `World`/allocator interaction in the
    // process is not representative, and charging it to either arm would bias
    // whichever ran first.
    {
        let (_, _, _) = churn(false);
    }

    let (healthy_first, healthy_last, columns) = churn(false);
    let (leaky_first, leaky_last, _) = churn(true);

    let healthy_growth = healthy_last.saturating_sub(healthy_first);
    let leaky_growth = leaky_last.saturating_sub(leaky_first);
    let cycles_measured = CYCLES - 1;

    println!(
        "session RSS over {CYCLES} load/unload cycles of {columns} columns each (rd={RD}, \
         {MODERN_SECTIONS} sections + light per column):"
    );
    println!(
        "  healthy (unloads):   after cycle 1 {:.1}MiB -> after cycle {CYCLES} {:.1}MiB \
         = {:+.2}MiB over {cycles_measured} cycles ({:+.0}KiB/cycle)",
        healthy_first as f64 / (1 << 20) as f64,
        healthy_last as f64 / (1 << 20) as f64,
        healthy_growth as f64 / (1 << 20) as f64,
        healthy_growth as f64 / cycles_measured as f64 / 1024.0,
    );
    println!(
        "  leaky (skips unload): after cycle 1 {:.1}MiB -> after cycle {CYCLES} {:.1}MiB \
         = {:+.2}MiB over {cycles_measured} cycles ({:+.0}KiB/cycle)  <-- the control",
        leaky_first as f64 / (1 << 20) as f64,
        leaky_last as f64 / (1 << 20) as f64,
        leaky_growth as f64 / (1 << 20) as f64,
        leaky_growth as f64 / cycles_measured as f64 / 1024.0,
    );

    // Control first: the detector must be able to see a leak at all. If this
    // fails, nothing about the healthy arm's result is meaningful — a flat
    // healthy number would be indistinguishable from a measurement that cannot
    // detect anything.
    assert!(
        leaky_growth > 4 << 20,
        "CONTROL FAILED: the leaky arm retained {columns} columns x {cycles_measured} cycles and \
         RSS grew only {leaky_growth} bytes. The measurement cannot see a leak of that size, so \
         the healthy arm's result below proves nothing. Check that `loaded_chunk` really allocates \
         (a lazily-empty ColumnLight would shrink the signal) and that each cycle uses a distinct \
         area."
    );

    // The actual question: healthy churn must not ratchet the way the leak does.
    assert!(
        healthy_growth * 4 < leaky_growth,
        "healthy load/unload churn grew RSS by {healthy_growth} bytes against the leaky control's \
         {leaky_growth} — within 4x of a real leak, so the world is retaining most of what it \
         unloads across {cycles_measured} cycles"
    );

    let scene =
        format!("rd={RD} columns={columns} cycles={CYCLES} sections={MODERN_SECTIONS} light=dense");
    for (metric, value, unit) in [
        ("session_rss_growth_healthy_bytes", healthy_growth as f64, "bytes"),
        ("session_rss_growth_leaky_bytes", leaky_growth as f64, "bytes"),
        (
            "session_rss_growth_per_cycle_healthy_bytes",
            healthy_growth as f64 / cycles_measured as f64,
            "bytes",
        ),
        (
            "session_rss_leak_detection_ratio",
            leaky_growth as f64 / healthy_growth.max(1) as f64,
            "x",
        ),
    ] {
        support::record(support::Record {
            bench: "session_rss",
            metric,
            scene: &scene,
            value,
            unit,
        });
    }
}

criterion_group!(benches, bench_session_rss);
criterion_main!(benches);
