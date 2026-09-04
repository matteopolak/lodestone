//! What one server tick costs, and how that cost scales with the population
//! it is ticking.
//!
//! # Why this bench exists
//!
//! Every other bench in this workspace measures the client: decode, meshing,
//! light, render submit. The server's own 20 Hz loop had a live instrument
//! (`crate::tick::TickClock`, with per-phase percentiles) and no tracked
//! number anywhere -- so "how much of a tick does the world simulation
//! actually take, and does it grow linearly with mobs" was answerable only by
//! attaching a profiler to a running game.
//!
//! That question is the prerequisite for deciding whether server ticking
//! should be regionised: partitioning the tick is worth its complexity only
//! if the tick is actually the cost, and only if the cost grows with
//! population rather than being dominated by a fixed per-tick overhead. Both
//! halves are measured below.
//!
//! # How it runs a real tick loop in bench time
//!
//! [`IntegratedServer::open_in_memory_with_mobs`] is the one in-memory
//! constructor that starts `crate::tick::run_tick_loop`, so it is the only one
//! whose ticks are real. Driving it at wall-clock speed would cost 50 ms per
//! tick and put a few hundred ticks out of reach of anything but a manual run.
//!
//! So the runtime is built with `start_paused(true)` and time is advanced one
//! tick period at a time: the loop's *waiting* is virtual and its *work* is
//! real, which is exactly the split a benchmark wants. The tick count is
//! therefore deterministic (N advances produce N ticks) rather than a
//! function of how fast the machine is.
//!
//! # Read the timing from outside the paused clock, never from inside it
//!
//! `TickStats::mspt_avg_ms` is derived from the runtime's own clock, and that
//! clock is the one being paused. Reading it here would report a tick that
//! costs approximately nothing -- a number that is not wrong so much as
//! meaningless, and whose meaninglessness is invisible in the value itself.
//! The instrument used instead is a plain [`std::time::Instant`] around the
//! whole advance loop, which the pause cannot reach. Both are printed, side by
//! side and labelled, precisely so the difference between them is on the
//! record rather than a trap for the next reader.
//!
//! The control that this measures anything at all is the population sweep: an
//! empty world and a populated one must not cost the same. If they do, the
//! instrument is reading something other than the simulation, and the bench
//! says so rather than recording a tidy number.
//!
//! # What is asserted versus recorded
//!
//! Per this workspace's measurement rules, a wall-clock number taken while
//! other work shares the machine is a sample, not a measurement, and a bare
//! duration ceiling is the wrong shape for a gate. So:
//!
//! * **Asserted**: the tick count advanced exactly as many times as time was
//!   advanced; the populated world does strictly more chunk-source work than
//!   the empty one; the loop forgave no overruns.
//! * **Recorded and compared against a stored baseline**: the per-tick count
//!   of `ChunkSource::column` calls, which is a property of the simulation and
//!   not of the machine.
//! * **Recorded, advisory only**: every microsecond figure.
//!
//! Run with `cargo bench -p lodestone-server --bench server_tick`.

mod support;

use std::hint::black_box;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};

use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v26_2::server_protocol::V770ServerProtocol;

/// One server tick period. Advancing the paused clock by exactly this much
/// per iteration is what makes the tick count deterministic.
const TICK_PERIOD: Duration = Duration::from_millis(50);

/// How many ticks each sweep point runs.
///
/// Chosen to clear the loop's own start-up deferral -- the first random-tick
/// pass is held back for the first 40 ticks so the world-seeding task can
/// finish before a block tick can trigger generation -- with enough ticks
/// after it that the steady state, not the warm-up, dominates the average.
const TICKS: u64 = 200;

/// Exclusive top of the fixture world's solid floor. The one place the floor's
/// extent is written down, so `column()` and `block_state()` cannot disagree
/// about what the world contains -- a disagreement that would show up as
/// mobs falling through a floor the block reads insist is there.
const FLOOR_TOP: i32 = 4;

/// A flat, cheap, deterministic world.
///
/// Deliberately not the real generator: this bench is measuring the *tick*, and
/// a generator-backed source would fold ~30 ms of column generation into
/// whichever tick first touched a cold chunk, drowning the quantity under
/// study. The counter is the point -- it turns "how much terrain work does a
/// tick do" into a count rather than a duration.
struct CountingFlatWorld {
    columns: Arc<AtomicU64>,
    block_reads: Arc<AtomicU64>,
}

impl CountingFlatWorld {
    fn build(&self) -> ChunkColumn {
        // A four-layer floor under air. Deliberately thin: the fixture's own
        // construction cost lands inside whichever tick first loads a column,
        // so a full-height fill would put fixture work into the quantity being
        // measured. Four layers is enough for mobs to stand on and for a block
        // tick to have something to read.
        let mut col = ChunkColumn::new(0, 128);
        for y in 0..FLOOR_TOP {
            for z in 0..16i32 {
                for x in 0..16i32 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col
    }
}

impl ChunkSource for CountingFlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.columns.fetch_add(1, Ordering::Relaxed);
        self.build()
    }

    fn block_state(&self, _x: i32, y: i32, _z: i32) -> String {
        self.block_reads.fetch_add(1, Ordering::Relaxed);
        if (0..FLOOR_TOP).contains(&y) {
            "minecraft:stone".to_string()
        } else {
            "minecraft:air".to_string()
        }
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage: this fixture serves a fixed world and discards edits by
        // design, so a tick that writes a block cannot change what a later
        // tick reads. That keeps the sweep points comparable to each other.
    }
}

/// One sweep point's outcome. Counts first, because they are the only figures
/// here that mean the same thing on another machine.
struct TickRun {
    ticks: u64,
    overruns: u64,
    column_calls: u64,
    block_reads: u64,
    /// Wall time of the whole advance loop, measured *outside* the paused
    /// runtime clock.
    wall: Duration,
    /// What the tick loop's own clock believed, for the side-by-side above.
    reported_mspt_avg_ms: f64,
}

fn run_ticks(mob_count: usize, view_radius: i32, area: i32) -> TickRun {
    let columns = Arc::new(AtomicU64::new(0));
    let block_reads = Arc::new(AtomicU64::new(0));
    let world = CountingFlatWorld { columns: Arc::clone(&columns), block_reads: Arc::clone(&block_reads) };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a current-thread runtime with a paused clock");

    runtime.block_on(async move {
        let mob_area: (RangeInclusive<i32>, RangeInclusive<i32>) = (-area..=area, -area..=area);
        let (server, _client_io) = IntegratedServer::open_in_memory_with_mobs(
            V770ServerProtocol,
            world,
            mob_area,
            (0, 0),
            mob_count,
            view_radius,
        );

        // A spawned task is never polled synchronously, so the tick task has
        // not yet reached its first timer when this returns from the
        // constructor. Yielding once lets it get there, which is what makes
        // "N advances produce N ticks" true rather than off by one.
        tokio::task::yield_now().await;

        let started = Instant::now();
        for _ in 0..TICKS {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;
        let wall = started.elapsed();

        let stats = server
            .tick_stats()
            .expect("open_in_memory_with_mobs starts a tick loop, so stats exist");
        let out = TickRun {
            ticks: stats.tick_count,
            overruns: stats.overrun_count,
            column_calls: columns.load(Ordering::Relaxed),
            block_reads: block_reads.load(Ordering::Relaxed),
            wall,
            reported_mspt_avg_ms: stats.mspt_avg_ms,
        };
        server.shutdown().await;
        out
    })
}

fn tick_cost(c: &mut Criterion) {
    // Two sweep points, and the pair is the measurement: an empty world and a
    // populated one. A single point would be a number with nothing to compare
    // it against, which is the shape this workspace's rules single out as
    // unfalsifiable.
    let empty = run_ticks(0, 2, 1);
    let populated = run_ticks(48, 2, 2);

    assert_eq!(
        empty.ticks, TICKS,
        "advancing the paused clock {TICKS} tick periods must produce exactly \
         {TICKS} ticks; got {}. A mismatch means the loop is not driven by the \
         clock this bench advances, and every figure below is measuring \
         something else.",
        empty.ticks
    );
    assert_eq!(populated.ticks, TICKS, "same, for the populated sweep point");
    assert_eq!(
        empty.overruns, 0,
        "a healthy loop under a paused clock never falls behind schedule"
    );

    // The control that the instrument sees the simulation at all. If a world
    // with 48 mobs over a wider area does not touch the chunk source more than
    // an empty one, the counter is wired to something that is not the tick.
    assert!(
        populated.column_calls + populated.block_reads
            > empty.column_calls + empty.block_reads,
        "a populated world must do strictly more chunk-source work than an \
         empty one; empty={}+{} populated={}+{}. Equal counts mean this bench \
         is not measuring the simulation.",
        empty.column_calls,
        empty.block_reads,
        populated.column_calls,
        populated.block_reads,
    );

    for (label, run) in [("empty", &empty), ("populated", &populated)] {
        println!(
            "[server_tick] {label}: {} ticks, {} column() calls, {} block_state() reads, \
             {:.3} ms wall for the whole loop (measured outside the paused clock), \
             loop's own mspt_avg {:.3} ms (paused clock -- not a cost figure)",
            run.ticks,
            run.column_calls,
            run.block_reads,
            run.wall.as_secs_f64() * 1e3,
            run.reported_mspt_avg_ms,
        );
    }

    let empty_scene = "flat in-memory world, mobs=0 area=3x3 view_radius=2";
    let populated_scene = "flat in-memory world, mobs=48 area=5x5 view_radius=2";

    // Counts: comparable across machines, so these are what a stored baseline
    // can hold.
    for (scene, run) in [(empty_scene, &empty), (populated_scene, &populated)] {
        support::record(support::Record {
            bench: "server_tick",
            metric: "column_calls_per_tick",
            scene,
            value: run.column_calls as f64 / run.ticks as f64,
            unit: "calls",
        });
        support::record(support::Record {
            bench: "server_tick",
            metric: "block_state_reads_per_tick",
            scene,
            value: run.block_reads as f64 / run.ticks as f64,
            unit: "calls",
        });
    }

    // Timings: advisory, same-machine only, never a baseline.
    for (scene, run) in [(empty_scene, &empty), (populated_scene, &populated)] {
        support::record(support::Record {
            bench: "server_tick",
            metric: "wall_us_per_tick",
            scene,
            value: run.wall.as_secs_f64() * 1e6 / run.ticks as f64,
            unit: "us",
        });
    }

    // The figure the regionised-ticking question actually needs: how much of a
    // tick is population-driven versus fixed overhead. A ratio of two arms
    // measured seconds apart on the same machine is still a sample rather than
    // a measurement, which is why it is recorded and not asserted -- but it is
    // the right *shape* of number to watch, unlike either arm alone.
    let populated_us = populated.wall.as_secs_f64() * 1e6 / populated.ticks as f64;
    let empty_us = empty.wall.as_secs_f64() * 1e6 / empty.ticks as f64;
    if empty_us > 0.0 {
        support::record(support::Record {
            bench: "server_tick",
            metric: "populated_vs_empty_wall_ratio",
            scene: "48 mobs over 5x5 versus an empty 3x3",
            value: populated_us / empty_us,
            unit: "x",
        });
    }

    // A criterion target so `cargo bench` reports something for this file even
    // though the real work above runs once. One tick period per iteration
    // would dominate the sample with runtime construction, so this measures
    // the cheapest honest unit instead: building the fixture world's column,
    // which is what every cold `column()` call inside a tick pays.
    let fixture = CountingFlatWorld {
        columns: Arc::new(AtomicU64::new(0)),
        block_reads: Arc::new(AtomicU64::new(0)),
    };
    c.bench_function("server/flat_column_build", |b| {
        b.iter(|| black_box(fixture.build()));
    });
}

criterion_group!(benches, tick_cost);
criterion_main!(benches);
