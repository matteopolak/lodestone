//! Pose-fit-gate cost (issue #78 epic, sub-issue #124): the swim/crouch
//! box-fit veto, [`can_player_fit_within_blocks_when`] — a real collision-
//! shape query against the world, run every pose transition attempt and,
//! per `docs/pose-dimensions.md`, potentially every tick while a player is
//! near a transition boundary (treading water at a low ceiling).
//!
//! The question this bench exists to answer: is the *failing* case (a
//! transition that keeps failing, tick after tick — the low-ceiling scenario)
//! meaningfully more expensive than the *succeeding* one? A veto that
//! re-queries collision geometry every tick with no memoisation would show up
//! here as a real, per-tick cost that only pays for players stuck at a
//! boundary — exactly the scenario likely to hit it repeatedly.
//!
//! Run with: `cargo bench -p lodestone-physics --bench pose_fit_gate`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::pose::{Pose, can_player_fit_within_blocks_when};

/// Open water column: no blocks anywhere, so every pose fits — the
/// succeeding-transition scene (a swimmer with headroom).
struct OpenWater;
impl CollisionView for OpenWater {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// A solid ceiling at `y = 2`: standing (1.8 tall) fits below it from feet at
/// `y = 0`, but the moment a surfacing swimmer's *standing* box would poke
/// through, the gate must veto every single tick it keeps trying — the
/// failing-transition scene `docs/pose-dimensions.md` calls out as the one
/// case vanilla has no recovery for.
struct LowCeiling;
impl CollisionView for LowCeiling {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 2 {
            out.push(Aabb::new(f64::from(x), 2.0, f64::from(z), f64::from(x) + 1.0, 3.0, f64::from(z) + 1.0));
        }
    }
}

fn bench_succeeding_transition(c: &mut Criterion) {
    let pos = Vec3d::new(0.5, 0.0, 0.5);
    const ITERS: usize = 20_000;
    for _ in 0..20 {
        black_box(can_player_fit_within_blocks_when(&OpenWater, pos, Pose::Standing));
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        black_box(can_player_fit_within_blocks_when(black_box(&OpenWater), black_box(pos), black_box(Pose::Standing)));
    }
    let ns = t0.elapsed().as_secs_f64() * 1e9 / ITERS as f64;
    println!("pose_fit_gate succeeding transition (open water): {ns:.1} ns/call over {ITERS} calls");
    support::record(support::Record {
        bench: "pose_fit_gate",
        metric: "succeeding_transition_ns",
        scene: "open water, standing pose, no obstruction",
        value: ns,
        unit: "ns",
    });

    c.bench_function("physics/pose_fit_gate_succeeding", |b| {
        b.iter(|| black_box(can_player_fit_within_blocks_when(black_box(&OpenWater), black_box(pos), black_box(Pose::Standing))))
    });
}

fn bench_failing_transition(c: &mut Criterion) {
    // Standing is 1.8 tall; feet at y=0.3 puts the box top at 2.1, clearing
    // into the y=2 ceiling block (which occupies world y=2..3) — feet at 0.0
    // would leave the box top at 1.8, entirely clear of the ceiling and not
    // exercising the veto at all.
    let pos = Vec3d::new(0.5, 0.3, 0.5);
    const ITERS: usize = 20_000;
    // Confirm the fixture actually exercises the veto before timing it —
    // otherwise this would be measuring the same vacuous "always true" case
    // as the succeeding scene above.
    assert!(
        !can_player_fit_within_blocks_when(&LowCeiling, pos, Pose::Standing),
        "fixture ceiling does not actually block standing -- this bench would measure the wrong branch"
    );
    for _ in 0..20 {
        black_box(can_player_fit_within_blocks_when(&LowCeiling, pos, Pose::Standing));
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        black_box(can_player_fit_within_blocks_when(black_box(&LowCeiling), black_box(pos), black_box(Pose::Standing)));
    }
    let ns = t0.elapsed().as_secs_f64() * 1e9 / ITERS as f64;
    println!("pose_fit_gate failing transition (low ceiling, repeated veto): {ns:.1} ns/call over {ITERS} calls");
    support::record(support::Record {
        bench: "pose_fit_gate",
        metric: "failing_transition_ns",
        scene: "low ceiling, standing pose repeatedly vetoed",
        value: ns,
        unit: "ns",
    });

    c.bench_function("physics/pose_fit_gate_failing", |b| {
        b.iter(|| black_box(can_player_fit_within_blocks_when(black_box(&LowCeiling), black_box(pos), black_box(Pose::Standing))))
    });
}

criterion_group!(benches, bench_succeeding_transition, bench_failing_transition);
criterion_main!(benches);
