//! Collision-sweep throughput against the real 26.2 shape census (issue #78
//! epic, sub-issue #120): [`collide`] swept against three scenes of
//! increasing geometric complexity — open air (no candidates), simple
//! full-cube terrain (the common case), and a dense mix of real complex
//! shapes (stairs, fences, slabs, walls — the worst *realistic* case, not a
//! pathological one).
//!
//! # Real data, not hand-picked simple shapes
//!
//! The complex-shape scene reuses `lodestone-physics/tests/support/
//! collision_shapes_jvm.txt` — the same authoritative dump
//! `tests/collision_shapes.rs` verifies the engine against, extracted from
//! the real 26.2 server via `oracle-java/ShapeOracle.java` — rather than a
//! synthetic approximation. This bench only *reads* that fixture
//! (`include_str!`); it does not modify `tests/collision_shapes.rs` or the
//! fixture itself.
//!
//! Run with: `cargo bench -p lodestone-physics --bench collision_sweep`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_physics::collision::{CollisionView, collide};
use lodestone_physics::geometry::{Aabb, Vec3d};

const REFERENCE: &str = include_str!("../tests/support/collision_shapes_jvm.txt");

/// One authoritative shape's block-local boxes, parsed the same way
/// `tests/collision_shapes.rs::parse_reference` does (copied rather than
/// imported — `tests/` and `benches/` are separate compilation units with no
/// shared lib target between them).
fn parse_shapes() -> Vec<(String, Vec<[f64; 6]>)> {
    let mut shapes = Vec::new();
    for line in REFERENCE.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let name = tok.next().unwrap().to_string();
        let _state_id: u32 = tok.next().unwrap().parse().unwrap();
        let nboxes: usize = tok.next().unwrap().parse().unwrap();
        if nboxes == 0 {
            continue; // no-collision blocks (cobweb, lava, water) irrelevant to a sweep bench.
        }
        let bits: Vec<f64> = tok.map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap())).collect();
        let boxes = bits.chunks_exact(6).map(|c| [c[0], c[1], c[2], c[3], c[4], c[5]]).collect();
        shapes.push((name, boxes));
    }
    shapes
}

struct OpenAir;
impl CollisionView for OpenAir {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// A full floor of simple cubes at `y = 0` across a wide area — the common
/// case: solid, unambiguous terrain.
struct SimpleFloor;
impl CollisionView for SimpleFloor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 {
            out.push(Aabb::new(f64::from(x), 0.0, f64::from(z), f64::from(x) + 1.0, 1.0, f64::from(z) + 1.0));
        }
    }
}

/// A floor tiled with the real complex shapes from the census, cycling
/// through every multi-box entry (stairs, fences, slabs, walls) so the swept
/// path crosses several distinct complex shapes rather than one repeated.
struct ComplexFloor {
    shapes: Vec<Vec<[f64; 6]>>,
}

impl ComplexFloor {
    fn new() -> Self {
        let shapes: Vec<Vec<[f64; 6]>> = parse_shapes().into_iter().map(|(_, boxes)| boxes).collect();
        assert!(!shapes.is_empty(), "collision_shapes_jvm.txt fixture produced no usable shapes");
        Self { shapes }
    }
}

impl CollisionView for ComplexFloor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y != 0 {
            return;
        }
        let idx = ((x.rem_euclid(1000) + z.rem_euclid(1000) * 31) as usize) % self.shapes.len();
        for b in &self.shapes[idx] {
            out.push(Aabb::new(
                b[0] + f64::from(x),
                b[1],
                b[2] + f64::from(z),
                b[3] + f64::from(x),
                b[4],
                b[5] + f64::from(z),
            ));
        }
    }
}

/// A horizontal sweep over a wide box, 20 blocks in x, entering fresh
/// candidate cells throughout — the realistic case (a moving entity, not a
/// stationary one already resolved).
fn time_sweep<V: CollisionView>(view: &V, box_height: f64) -> f64 {
    let start = Aabb::new(0.3, 1.0, 0.3, 0.9, 1.0 + box_height, 0.9);
    const ITERS: usize = 2000;
    for _ in 0..20 {
        black_box(collide(view, Vec3d::new(0.1, -0.05, 0.0), black_box(start), false, 0.6));
    }
    let t0 = Instant::now();
    for i in 0..ITERS {
        let x0 = (i % 20) as f64;
        let box_ = Aabb::new(x0, 1.0, 0.3, x0 + 0.6, 1.0 + box_height, 0.9);
        black_box(collide(black_box(view), black_box(Vec3d::new(0.1, -0.05, 0.0)), black_box(box_), false, 0.6));
    }
    t0.elapsed().as_secs_f64() * 1e9 / ITERS as f64 // ns/sweep
}

fn bench_sweep_by_complexity(c: &mut Criterion) {
    let open = OpenAir;
    let simple = SimpleFloor;
    let complex = ComplexFloor::new();

    let ns_open = time_sweep(&open, 1.8);
    let ns_simple = time_sweep(&simple, 1.8);
    let ns_complex = time_sweep(&complex, 1.8);

    println!("collision_sweep by terrain complexity (ns/sweep, player-sized box):");
    println!("  open air:            {ns_open:.1}");
    println!("  simple full-cube:    {ns_simple:.1}  ({:.2}x open air)", ns_simple / ns_open.max(1.0));
    println!("  dense complex mix:   {ns_complex:.1}  ({:.2}x simple cube)", ns_complex / ns_simple);

    for (metric, value) in [
        ("open_air_ns_per_sweep", ns_open),
        ("simple_cube_ns_per_sweep", ns_simple),
        ("complex_mix_ns_per_sweep", ns_complex),
    ] {
        support::record(support::Record {
            bench: "collision_sweep",
            metric,
            scene: "20-wide horizontal sweep, player-sized box",
            value,
            unit: "ns",
        });
    }
    support::record(support::Record {
        bench: "collision_sweep",
        metric: "complex_vs_simple_ratio",
        scene: "20-wide horizontal sweep, player-sized box",
        value: ns_complex / ns_simple,
        unit: "x",
    });

    let mut group = c.benchmark_group("physics/collision_sweep");
    group.bench_function("open_air", |b| {
        b.iter(|| black_box(collide(black_box(&open), black_box(Vec3d::new(0.1, -0.05, 0.0)), black_box(Aabb::new(0.3, 1.0, 0.3, 0.9, 2.8, 0.9)), false, 0.6)))
    });
    group.bench_function("simple_cube", |b| {
        b.iter(|| black_box(collide(black_box(&simple), black_box(Vec3d::new(0.1, -0.05, 0.0)), black_box(Aabb::new(0.3, 1.0, 0.3, 0.9, 2.8, 0.9)), false, 0.6)))
    });
    group.bench_function("complex_mix", |b| {
        b.iter(|| black_box(collide(black_box(&complex), black_box(Vec3d::new(0.1, -0.05, 0.0)), black_box(Aabb::new(0.3, 1.0, 0.3, 0.9, 2.8, 0.9)), false, 0.6)))
    });
    group.finish();
}

criterion_group!(benches, bench_sweep_by_complexity);
criterion_main!(benches);
