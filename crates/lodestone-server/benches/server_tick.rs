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
//!   advanced; the populated world retains a strictly larger live roster
//!   sample than the empty one; the loop forgave no overruns.
//! * **Recorded as diagnostics**: the normalized count of cold-load
//!   `ChunkSource::column` calls, which is a property of fixture setup rather
//!   than a wall-clock measurement.
//! * **Recorded as diagnostics**: each phase's sample window, percentile and
//!   budget summary, plus the largest phase window and its phase label.
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

use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, PhaseStats, TickPhase, WorstPhaseWindow,
    TICK_HISTORY_LEN,
};
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

/// The non-empty arm's exact population. Keeping this separate from its
/// description and assertion makes removing fixture seeding observable.
const POPULATED_MOBS: usize = 48;

/// Cooperative polls allowed for the constructor's off-thread reseed before
/// the fixture uses the live mob handle. The clock stays paused throughout.
const RESEED_POLLS: usize = 100_000;

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
    roster: usize,
    /// Sum of the live mob roster observed after each driven tick. Unlike the
    /// underlying source counters, this crosses the retaining store and sees
    /// the simulation's resident `ChunkWorld` state directly.
    mob_roster_samples: u64,
    column_calls: u64,
    block_reads: u64,
    /// Wall time of the whole advance loop, measured *outside* the paused
    /// runtime clock.
    wall: Duration,
    /// What the tick loop's own clock believed, for the side-by-side above.
    reported_mspt_avg_ms: f64,
    /// The three summaries captured by the live tick instrument, in execution
    /// order. Keeping the complete snapshots here prevents the recorder from
    /// silently dropping percentile and budget information at the bench edge.
    phase_stats: [PhaseStats; 3],
    /// Longest phase interval observed during this sweep point.
    worst_phase: WorstPhaseWindow,
}

#[derive(Debug, PartialEq)]
struct PhaseMetric {
    name: String,
    value: f64,
    unit: &'static str,
}

fn phase_metrics(stats: PhaseStats) -> [PhaseMetric; 7] {
    let prefix = match stats.phase {
        TickPhase::MobsAndItems => "mobs_and_items",
        TickPhase::WeatherAndSleep => "weather_and_sleep",
        TickPhase::ScheduledAndPhysics => "scheduled_and_physics",
    };
    [
        ("rolling_samples", stats.sample_count as f64, "samples"),
        ("total_samples", stats.total_sample_count as f64, "samples"),
        ("p50_us", stats.p50_ms * 1_000.0, "us"),
        ("p95_us", stats.p95_ms * 1_000.0, "us"),
        ("p99_us", stats.p99_ms * 1_000.0, "us"),
        ("max_us", stats.max_ms * 1_000.0, "us"),
        ("over_budget_count", stats.over_budget_count as f64, "ticks"),
    ]
    .map(|(suffix, value, unit)| PhaseMetric {
        name: format!("{prefix}_{suffix}"),
        value,
        unit,
    })
}

fn phase_has_samples(stats: PhaseStats) -> bool {
    stats.sample_count > 0 && stats.total_sample_count > 0
}

/// A synthetic zero summary is the negative control for the recorder. If the
/// projection ever stops carrying a phase's values, this check fails before a
/// real sweep can publish an apparently healthy but neutered phase.
fn assert_phase_metric_control() {
    let active = PhaseStats {
        phase: TickPhase::WeatherAndSleep,
        sample_count: 4,
        total_sample_count: 9,
        p50_ms: 1.25,
        p95_ms: 2.5,
        p99_ms: 2.75,
        max_ms: 3.0,
        over_budget_count: 0,
    };
    let neutered = PhaseStats {
        phase: TickPhase::WeatherAndSleep,
        sample_count: 0,
        total_sample_count: 0,
        p50_ms: 0.0,
        p95_ms: 0.0,
        p99_ms: 0.0,
        max_ms: 0.0,
        over_budget_count: 0,
    };
    assert!(phase_has_samples(active));
    assert!(!phase_has_samples(neutered));
    let active_metrics = phase_metrics(active);
    let neutered_metrics = phase_metrics(neutered);
    assert_ne!(active_metrics, neutered_metrics);
    assert_eq!(active_metrics[2].name, "weather_and_sleep_p50_us");
    assert_eq!(active_metrics[2].value, 1_250.0);
    assert_eq!(active_metrics[3].value, 2_500.0);
    assert_eq!(active_metrics[6].value, 0.0);
}

/// Waits until the in-memory world's asynchronous reseed has installed the
/// real simulation, then inserts this sweep point's exact population through
/// the public server surface.
///
/// The constructor's configured demo population is deliberately zero: it is
/// environment-gated, while a benchmark fixture must be self-contained.
async fn seed_fixture_mobs(server: &IntegratedServer, mob_count: usize) -> usize {
    let mobs = server
        .mobs()
        .expect("open_in_memory_with_mobs exposes its live mob handle");
    let mut reseeded = false;
    for _ in 0..RESEED_POLLS {
        if mobs.with(|sim| sim.next_id()) >= 1000 {
            reseeded = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        reseeded,
        "the asynchronous fixture reseed did not finish after {RESEED_POLLS} cooperative polls"
    );

    let species: ResourceKey = "minecraft:pig"
        .parse()
        .expect("the fixture's species key is valid");
    for index in 0..mob_count {
        let x = (index % 8) as f64 + 0.5;
        let z = (index / 8) as f64 + 0.5;
        server
            .spawn_mob(species.clone(), Vec3::new(x, FLOOR_TOP as f64, z))
            .expect("open_in_memory_with_mobs accepts a spawned fixture mob");
    }

    let roster = mobs.with(|sim| sim.iter().count());
    assert_eq!(
        roster, mob_count,
        "fixture seeding must leave exactly the requested roster after reseed"
    );
    roster
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
            0,
            view_radius,
        );

        let roster = seed_fixture_mobs(&server, mob_count).await;
        let mobs = server
            .mobs()
            .expect("open_in_memory_with_mobs exposes its live mob handle");

        // The asynchronous install fetches the fixture's complete 5x5 area.
        // That setup work establishes a live simulation but is not work done
        // by the driven ticks, so discard it before taking per-tick counts.
        columns.store(0, Ordering::Relaxed);
        block_reads.store(0, Ordering::Relaxed);
        assert_eq!(
            columns.load(Ordering::Relaxed),
            0,
            "the measured column counter must start after fixture setup"
        );
        assert_eq!(
            block_reads.load(Ordering::Relaxed),
            0,
            "the measured block-read counter must start after fixture setup"
        );

        // A spawned task is never polled synchronously, so the tick task has
        // not yet reached its first timer when this returns from the
        // constructor. Yielding once lets it get there, which is what makes
        // "N advances produce N ticks" true rather than off by one.
        tokio::task::yield_now().await;

        let started = Instant::now();
        let mut mob_roster_samples = 0;
        for _ in 0..TICKS {
            tokio::time::advance(TICK_PERIOD).await;
            // `advance` wakes the tick task, but the current task can keep
            // running until it yields. Sampling after this yield makes each
            // observation correspond to one completed driven tick rather
            // than repeatedly seeing the pre-tick state.
            tokio::task::yield_now().await;
            mob_roster_samples += mobs.with(|sim| sim.iter().count() as u64);
        }
        tokio::task::yield_now().await;
        let wall = started.elapsed();

        let stats = server
            .tick_stats()
            .expect("open_in_memory_with_mobs starts a tick loop, so stats exist");
        let out = TickRun {
            ticks: stats.tick_count,
            overruns: stats.overrun_count,
            roster,
            mob_roster_samples,
            column_calls: columns.load(Ordering::Relaxed),
            block_reads: block_reads.load(Ordering::Relaxed),
            wall,
            reported_mspt_avg_ms: stats.mspt_avg_ms,
            phase_stats: [stats.mobs_and_items, stats.weather_and_sleep, stats.scheduled_and_physics],
            worst_phase: stats
                .worst_phase_window
                .expect("every completed tick records every phase, so the worst window exists"),
        };
        server.shutdown().await;
        out
    })
}

fn tick_cost(c: &mut Criterion) {
    assert_phase_metric_control();
    // Two sweep points, and the pair is the measurement: an empty world and a
    // populated one. A single point would be a number with nothing to compare
    // it against, which is the shape this workspace's rules single out as
    // unfalsifiable.
    let empty = run_ticks(0, 2, 2);
    let populated = run_ticks(POPULATED_MOBS, 2, 2);

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
    assert_eq!(empty.roster, 0, "the empty sweep point's roster");
    assert_eq!(
        populated.roster, POPULATED_MOBS,
        "the populated sweep point must retain its exact seeded roster"
    );
    assert_ne!(
        populated.roster, 0,
        "the populated sweep point must not silently become empty"
    );

    for (label, run) in [("empty", &empty), ("populated", &populated)] {
        let expected_rolling_samples = TICKS.min(TICK_HISTORY_LEN as u64);
        for stats in run.phase_stats {
            assert!(
                phase_has_samples(stats),
                "{label}: {:?} must have a rolling and cumulative sample",
                stats.phase
            );
            assert_eq!(stats.total_sample_count, TICKS, "{label}: {:?} total samples", stats.phase);
            assert_eq!(
                stats.sample_count, expected_rolling_samples,
                "{label}: {:?} rolling samples", stats.phase
            );
        }
        assert_eq!(
            run.worst_phase.phase,
            TickPhase::MobsAndItems,
            "{label}: the paused clock gives every phase a zero duration, so the first recorded phase owns the tied worst window"
        );
    }

    // The control that the instrument sees the simulation at all. The source
    // counters intentionally measure only cold loads: the constructor's
    // retaining store fills the mob area before the driven ticks, so those
    // counters can legitimately remain zero during the measured loop. The
    // live roster sample crosses that cache boundary and is the population
    // distinction this benchmark can observe without changing production code.
    assert!(
        populated.mob_roster_samples > empty.mob_roster_samples,
        "a populated world must retain a strictly larger live roster sample \
         than an empty one; empty={} populated={}. Equal counts mean this \
         bench is not observing the simulation.",
        empty.mob_roster_samples,
        populated.mob_roster_samples,
    );

    for (label, run) in [("empty", &empty), ("populated", &populated)] {
        println!(
            "[server_tick] {label}: {} ticks, roster={}, {} column() calls, {} block_state() reads, \
             {:.3} ms wall for the whole loop (measured outside the paused clock), \
             loop's own mspt_avg {:.3} ms (paused clock -- not a cost figure), \
             phase samples rolling={}/{}/{} total={}/{}/{}, worst={}us ({:?})",
            run.ticks,
            run.roster,
            run.column_calls,
            run.block_reads,
            run.wall.as_secs_f64() * 1e3,
            run.reported_mspt_avg_ms,
            run.phase_stats[0].sample_count,
            run.phase_stats[1].sample_count,
            run.phase_stats[2].sample_count,
            run.phase_stats[0].total_sample_count,
            run.phase_stats[1].total_sample_count,
            run.phase_stats[2].total_sample_count,
            run.worst_phase.micros,
            run.worst_phase.phase,
        );
    }

    let empty_scene = "flat in-memory world, mobs=0 area=5x5 view_radius=2";
    let populated_scene = "flat in-memory world, mobs=48 area=5x5 view_radius=2";

    // Deterministic fixture counts: comparable across machines and useful for
    // checking that the named sweep point still ran the intended scenario.
    for (scene, run) in [(empty_scene, &empty), (populated_scene, &populated)] {
        // Keep the fixture identity beside its work counters. These are
        // deterministic integrity metrics: a changed tick count or roster
        // means the sweep no longer represents the scenario named by `scene`.
        support::record(support::Record {
            bench: "server_tick",
            metric: "ticks",
            scene,
            value: run.ticks as f64,
            unit: "ticks",
        });
        support::record(support::Record {
            bench: "server_tick",
            metric: "roster",
            scene,
            value: run.roster as f64,
            unit: "mobs",
        });
        support::record(support::Record {
            bench: "server_tick",
            metric: "mob_roster_samples_per_tick",
            scene,
            value: run.mob_roster_samples as f64 / run.ticks as f64,
            unit: "mobs",
        });
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
        for stats in run.phase_stats {
            for metric in phase_metrics(stats) {
                support::record(support::Record {
                    bench: "server_tick",
                    metric: &metric.name,
                    scene,
                    value: metric.value,
                    unit: metric.unit,
                });
            }
        }
        let worst_phase_metric = match run.worst_phase.phase {
            TickPhase::MobsAndItems => "worst_phase_mobs_and_items_us",
            TickPhase::WeatherAndSleep => "worst_phase_weather_and_sleep_us",
            TickPhase::ScheduledAndPhysics => "worst_phase_scheduled_and_physics_us",
        };
        support::record(support::Record {
            bench: "server_tick",
            metric: worst_phase_metric,
            scene,
            value: run.worst_phase.micros as f64,
            unit: "us",
        });
        support::record(support::Record {
            bench: "server_tick",
            metric: "worst_phase_us",
            scene,
            value: run.worst_phase.micros as f64,
            unit: "us",
        });
        support::record(support::Record {
            bench: "server_tick",
            metric: "worst_phase_tick",
            scene,
            value: run.worst_phase.tick_count as f64,
            unit: "tick",
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
            scene: "48 mobs over 5x5 versus an empty 5x5",
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
