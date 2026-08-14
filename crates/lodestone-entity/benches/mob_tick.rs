//! Per-tick throughput for [`NavigatingMob::tick`] — the composed goal
//! scheduler + real A* + kinematic follower, i.e. what a live `MobSim`
//! actually calls every server tick for every pathing mob
//! (`ai::navigating_mob`'s module doc: "the composition that closes the gap"
//! between the goal scheduler and the pathfinder, "drivable by a
//! `GoalSelector`"). This is the entities half of the goal-scheduler/pathfinder split, the sibling of
//! `pathfinding_search.rs`'s raw `PathFinder::find_path` number: this is what
//! a *tick loop* actually pays, which is not one number but two very
//! different ones.
//!
//! # Two regimes, not one — the duration-species trap
//!
//! A live mob's per-tick cost swings by orders of magnitude between "this
//! tick runs a fresh A* search" and "this tick just advances one kinematic
//! step along an already-computed path"
//! (`NavigatingMob::move_to`'s `same_target`/20-tick-throttle logic — see its
//! doc comment). Timing `mob.tick()` in a plain `b.iter` loop over one
//! long-lived mob would let criterion's thousands of iterations run the mob
//! to completion after the first handful of calls, so iteration 1 pays a
//! search and iteration 5000 measures an idle mob standing at its target
//! doing almost nothing — a textbook instance of `CLAUDE.md`'s duration
//! species: the state a `b.iter` closure mutates persists across iterations,
//! so late iterations measure a different regime than early ones, and
//! nothing about reading the closure would tell you that. Both benches below
//! use `iter_batched` with fresh per-batch setup for exactly this reason:
//! every timed batch starts from the same initial condition, so the number
//! reported is the *specific* regime named, not whatever the mob happened to
//! settle into by the end of the run.
//!
//! Run with: `cargo bench -p lodestone-entity --bench mob_tick`

mod support;

use std::collections::HashSet;
use std::hint::black_box;
use std::time::Instant;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use lodestone_entity::ai::goals::MeleeAttackGoal;
use lodestone_entity::ai::{GoalSelector, MobController, NavigatingMob};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

/// Same fence arena as `pathfinding_search.rs`'s `detour_fence` (duplicated —
/// each `benches/*.rs` file is its own compilation unit, and this harness's
/// convention, per `docs/benchmark-harness.md`, is duplication over a shared
/// bench-only crate). A real obstacle so `move_to` actually invokes A* rather
/// than reusing a trivial straight walk — the same shape
/// `ai::navigating_mob.rs`'s own
/// `goal_drives_pathfinder_to_detour_an_unjumpable_fence` test is built on.
struct Arena {
    walls: HashSet<(i32, i32, i32)>,
}

impl Arena {
    fn is_ground(y: i32) -> bool {
        y <= -1
    }
    fn is_wall(&self, x: i32, y: i32, z: i32) -> bool {
        self.walls.contains(&(x, y, z))
    }
    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.is_wall(x, y, z) || Self::is_ground(y)
    }
}

impl PathWorld for Arena {
    fn min_y(&self) -> i32 {
        -8
    }
    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.is_solid(x, y, z) { PathType::Blocked } else { PathType::Open }
    }
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.is_wall(x, y, z) {
            1.5
        } else if Self::is_ground(y) {
            1.0
        } else {
            0.0
        }
    }
    fn collides(&self, aabb: Aabb) -> bool {
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if self.is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }
    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

fn detour_fence() -> Arena {
    let mut walls = HashSet::new();
    for z in -25..=25 {
        walls.insert((15, -1, z));
        walls.insert((15, 0, z));
    }
    Arena { walls }
}

const START: Vec3 = Vec3::new(0.5, 0.0, 0.5);
const TARGET: Vec3 = Vec3::new(30.5, 0.0, 0.5); // ~30 blocks + the fence detour.

fn fresh_mob(world: &Arena) -> (NavigatingMob<'_>, GoalSelector) {
    let shape = MobShape::land(0.6, 1.95);
    let mut mob = NavigatingMob::new(world, shape, START, 0.25, 20_000, 0);
    mob.set_attack_target(Some(TARGET));
    let mut ai = GoalSelector::new();
    ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
    (mob, ai)
}

/// Regime 1: the tick that triggers a fresh A* search — the first tick of a
/// chase, or any tick 20+ after the last search when the mob still has not
/// arrived (vanilla's `MAX_TIME_RECOMPUTE` throttle, reproduced in
/// `NavigatingMob::move_to`). Fresh setup per batch so every timed call is
/// genuinely the search-triggering tick, never a warmed-up follow tick.
fn bench_tick_with_search(c: &mut Criterion) {
    let world = detour_fence();

    // Anti-vacuity control: confirm the timed tick really does invoke a
    // search (the composed-tick-level counterpart of the check
    // `pathfinding_search.rs` does at the raw-search level).
    {
        let (mut mob, mut ai) = fresh_mob(&world);
        mob.tick(&mut ai);
        assert!(
            mob.path_searches() >= 1,
            "first tick did not invoke A* -- this bench would be measuring an idle mob"
        );
    }

    const ITERS: usize = 300;
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (mut mob, mut ai) = fresh_mob(&world);
        let t0 = Instant::now();
        black_box(mob.tick(&mut ai));
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[ITERS / 2];
    println!("mob_tick (search-triggering tick): median {median:.2} us over {ITERS} calls");
    support::record(support::Record {
        bench: "mob_tick",
        metric: "search_tick_median_us",
        scene: "fence detour, first tick (forces exactly one A* search)",
        value: median,
        unit: "us",
    });

    c.bench_function("entity/mob_tick_with_search", |b| {
        b.iter_batched(
            || fresh_mob(&world),
            |(mut mob, mut ai)| black_box(mob.tick(&mut ai)),
            BatchSize::PerIteration,
        )
    });
}

/// Regime 2: a steady-state follow tick, well after the initial search — what
/// the overwhelming majority of ticks in a real chase actually cost (this
/// scene's chase runs roughly 100+ follow ticks per search at
/// 0.25 blocks/tick). `iter_batched`'s `setup` closure runs the search and a
/// few follow ticks *untimed* (setup is always excluded from criterion's
/// timed region — see `chunk_load.rs` in `lodestone-world` for a case where
/// getting that boundary wrong cost a 450x measurement error), then the
/// routine times a small batch of pure-follow ticks, chosen far from both the
/// target (no early completion) and the 20-tick throttle boundary (no
/// accidental second search).
fn bench_tick_steady_follow(c: &mut Criterion) {
    let world = detour_fence();
    const WARMUP_TICKS: u32 = 5;
    const FOLLOW_TICKS: u32 = 10;

    // Anti-vacuity control: after warmup, prove the timed batch genuinely
    // runs zero additional searches -- otherwise this "follow-only" number
    // would silently include search cost too.
    {
        let (mut mob, mut ai) = fresh_mob(&world);
        for _ in 0..WARMUP_TICKS {
            mob.tick(&mut ai);
        }
        let before = mob.path_searches();
        for _ in 0..FOLLOW_TICKS {
            mob.tick(&mut ai);
        }
        assert_eq!(
            mob.path_searches(),
            before,
            "a 'steady follow' tick triggered a new search -- this scene no longer isolates follow-only cost"
        );
    }

    const ITERS: usize = 300;
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (mut mob, mut ai) = fresh_mob(&world);
        for _ in 0..WARMUP_TICKS {
            mob.tick(&mut ai);
        }
        let t0 = Instant::now();
        for _ in 0..FOLLOW_TICKS {
            black_box(mob.tick(&mut ai));
        }
        samples.push(t0.elapsed().as_secs_f64() * 1e6 / f64::from(FOLLOW_TICKS));
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[ITERS / 2];
    println!(
        "mob_tick (steady follow): median {median:.3} us/tick over {ITERS} batches of {FOLLOW_TICKS}"
    );
    let scene = format!("fence detour, {WARMUP_TICKS} warmup + {FOLLOW_TICKS} follow ticks, no search");
    support::record(support::Record {
        bench: "mob_tick",
        metric: "follow_tick_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });

    c.bench_function("entity/mob_tick_steady_follow", |b| {
        b.iter_batched(
            || {
                let (mut mob, mut ai) = fresh_mob(&world);
                for _ in 0..WARMUP_TICKS {
                    mob.tick(&mut ai);
                }
                (mob, ai)
            },
            |(mut mob, mut ai)| {
                for _ in 0..FOLLOW_TICKS {
                    black_box(mob.tick(&mut ai));
                }
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group!(benches, bench_tick_with_search, bench_tick_steady_follow);
criterion_main!(benches);
