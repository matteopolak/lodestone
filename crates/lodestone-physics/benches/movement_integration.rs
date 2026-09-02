//! Per-tick movement integration cost:
//! [`lodestone_physics::player::tick`], the real function every entity runs
//! once per tick, 20 Hz, with zero measured baseline before this bench.
//!
//! Benchmarks the walking-on-ground and falling-in-air branches directly
//! (both real, already-ported code paths — `travel_and_check_inside_blocks`
//! dispatches to `travel_in_air` for both, differing only in whether the
//! floor stops the fall) as *different* scenes rather than one blended
//! number, since these are treated as distinct costs.
//! Swimming (`docs/swimming.md`'s fixed-tick timing) needs a water-tagged
//! `CollisionView`, which this bench's minimal fixture does not model —
//! documented as a gap below rather than faked with a `CollisionView` that
//! reports water without a real block backing it.
//!
//! Run with: `cargo bench -p lodestone-physics --bench movement_integration`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::player::{MovementInput, PlayerState, tick};
use lodestone_physics::profile::PhysicsProfile;

/// Empty air everywhere — the falling-in-air scene.
struct Void;
impl CollisionView for Void {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// A full floor at `y = 0` and nothing else — the walking-on-ground scene
/// (the overwhelmingly common per-tick case: a player standing/moving on
/// solid terrain, not falling).
struct FlatFloor;
impl CollisionView for FlatFloor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 {
            out.push(Aabb::new(f64::from(x), 0.0, f64::from(z), f64::from(x) + 1.0, 1.0, f64::from(z) + 1.0));
        }
    }
}

fn time_ticks<V: CollisionView>(view: &V, input: MovementInput, iters: usize) -> f64 {
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    // Settle a few ticks first so the measured region is steady-state
    // (on-ground/falling already resolved) rather than the first-tick
    // transient.
    for _ in 0..10 {
        tick(&mut state, input, black_box(view as &dyn CollisionView), &profile);
    }
    let t0 = Instant::now();
    for _ in 0..iters {
        tick(black_box(&mut state), black_box(input), black_box(view as &dyn CollisionView), black_box(&profile));
    }
    t0.elapsed().as_secs_f64() * 1e9 / iters as f64 // ns/tick
}

fn bench_walking_on_ground(c: &mut Criterion) {
    const ITERS: usize = 20_000;
    let ns_per_tick = time_ticks(&FlatFloor, MovementInput { forward: 1.0, ..MovementInput::NONE }, ITERS);
    println!("movement_integration walking-on-ground: {ns_per_tick:.1} ns/tick over {ITERS} ticks");
    support::record(support::Record {
        bench: "movement_integration",
        metric: "walking_on_ground_ns_per_tick",
        scene: "single player, flat floor, forward input",
        value: ns_per_tick,
        unit: "ns",
    });

    let profile = PhysicsProfile::mc_1_21();
    let input = MovementInput { forward: 1.0, ..MovementInput::NONE };
    c.bench_function("physics/tick_walking_on_ground", |b| {
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        b.iter(|| tick(black_box(&mut state), black_box(input), black_box(&FlatFloor as &dyn CollisionView), black_box(&profile)))
    });
}

fn bench_falling_in_air(c: &mut Criterion) {
    const ITERS: usize = 20_000;
    let ns_per_tick = time_ticks(&Void, MovementInput::NONE, ITERS);
    println!("movement_integration falling-in-air: {ns_per_tick:.1} ns/tick over {ITERS} ticks");
    support::record(support::Record {
        bench: "movement_integration",
        metric: "falling_in_air_ns_per_tick",
        scene: "single player, open air, no input",
        value: ns_per_tick,
        unit: "ns",
    });

    let profile = PhysicsProfile::mc_1_21();
    c.bench_function("physics/tick_falling_in_air", |b| {
        let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
        b.iter(|| tick(black_box(&mut state), black_box(MovementInput::NONE), black_box(&Void as &dyn CollisionView), black_box(&profile)))
    });
}

criterion_group!(benches, bench_walking_on_ground, bench_falling_in_air);
criterion_main!(benches);
