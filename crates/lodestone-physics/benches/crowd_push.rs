//! Crowd-push pair-test cost at scale:
//! [`entity_push_impulse`] (the pure pair-accumulation core
//! [`apply_entity_push`] calls), which `docs/entity-push.md` documents as an
//! O(pairs) mechanism by construction — every entity checks every nearby
//! entity, with no distance falloff despite looking like it does.
//!
//! Benchmarks the pair test at increasing crowd density (N = 10/50/200/1000
//! nearby entities, all within push range) and reports both wall time *and*
//! the ratio against N, so a future broad-phase (a narrower
//! `NEARBY_ENTITY_RADIUS`) that changes how many entities actually reach this
//! call can be judged against this baseline — if this function's own cost
//! per call ever stops being ~linear in `nearby.len()`, that is a regression
//! independent of any broad-phase change.
//!
//! Run with: `cargo bench -p lodestone-physics --bench crowd_push`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::push::{NearbyEntity, PushSelf, entity_push_impulse};

fn body(cx: f64, cy: f64, cz: f64) -> Aabb {
    Aabb::new(cx - 0.3, cy, cz - 0.3, cx + 0.3, cy + 1.8, cz + 0.3)
}

/// `n` mobs packed into a bounded pen (a mob-farm/spawn-pen shape, per the
/// issue's own framing — not a sparse world), each overlapping our own box so
/// every pair actually contributes a push, not a distance-vetoed no-op.
fn crowd(n: usize) -> Vec<NearbyEntity> {
    (0..n)
        .map(|i| {
            let angle = (i as f64) * std::f64::consts::TAU / (n.max(1) as f64);
            let r = 0.4; // well within the ~1.0-ish pair-push interaction range.
            let x = 0.5 + r * angle.cos();
            let z = 0.5 + r * angle.sin();
            NearbyEntity::living(Vec3d::new(x, 1.0, z), body(x, 1.0, z))
        })
        .collect()
}

fn bench_crowd_density(c: &mut Criterion) {
    let us = Vec3d::new(0.5, 1.0, 0.5);
    let self_box = body(0.5, 1.0, 0.5);

    println!("crowd_push pair-test scaling:");
    let mut baseline_ns_per_pusher: Option<f64> = None;
    for &n in &[10usize, 50, 200, 1000] {
        let nearby = crowd(n);
        const ITERS: usize = 200;
        for _ in 0..10 {
            black_box(entity_push_impulse(us, self_box, PushSelf::LIVING_PLAYER, true, &nearby));
        }
        let t0 = Instant::now();
        for _ in 0..ITERS {
            black_box(entity_push_impulse(black_box(us), black_box(self_box), PushSelf::LIVING_PLAYER, true, black_box(&nearby)));
        }
        let ns = t0.elapsed().as_secs_f64() * 1e9 / ITERS as f64;
        let ns_per_pusher = ns / n as f64;
        if baseline_ns_per_pusher.is_none() {
            baseline_ns_per_pusher = Some(ns_per_pusher);
        }
        let ratio_vs_linear = ns_per_pusher / baseline_ns_per_pusher.unwrap();
        println!(
            "  n={n:<5} total={ns:>10.1} ns/call  {ns_per_pusher:.2} ns/pusher  (ratio vs n=10's per-pusher cost: {ratio_vs_linear:.2}x)"
        );
        let scene = format!("n={n} nearby entities, all overlapping");
        support::record(support::Record {
            bench: "crowd_push",
            metric: "pair_test_ns_per_pusher",
            scene: &scene,
            value: ns_per_pusher,
            unit: "ns",
        });
        support::record(support::Record {
            bench: "crowd_push",
            metric: "pair_test_total_ns",
            scene: &scene,
            value: ns,
            unit: "ns",
        });
    }

    let mut group = c.benchmark_group("physics/crowd_push");
    for &n in &[10usize, 50, 200, 1000] {
        let nearby = crowd(n);
        group.bench_function(format!("n_{n}"), |b| {
            b.iter(|| black_box(entity_push_impulse(black_box(us), black_box(self_box), PushSelf::LIVING_PLAYER, true, black_box(&nearby))))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_crowd_density);
criterion_main!(benches);
