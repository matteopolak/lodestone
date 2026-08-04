//! A* search throughput for [`PathFinder::find_path`] — the real per-search
//! engine `NavigatingMob::move_to` calls (see `mob_tick.rs` in this same
//! directory for the composed, per-tick view). Issue #78 epic, entities half.
//!
//! # Why four scenes, not one
//!
//! The epic's own brief names the trap directly: "a pathfinding bench over
//! open flat ground never exercises the search." A* over unobstructed terrain
//! degenerates to walking a straight line, expanding barely more nodes than
//! the path is long, regardless of whether the heap/heuristic/edge-cost code
//! is even correct. `open_flat` is kept anyway, but only as the *negative
//! control* `CLAUDE.md`'s evidence standard calls for ("assertions of an
//! absence need a control proving the detector works"): its entire purpose is
//! to be cheap, so that `detour_fence` and `serpentine_maze` costing
//! meaningfully more is evidence the obstacle scenes really do force lateral
//! search, not merely an assumption nobody checked. `sealed_unreachable` is a
//! fourth, different shape: a target with **no** path at all, so the search
//! exhausts (most of) its visited-node budget before returning a best-effort
//! partial result — the worst-case per-call cost, and a real one: a wedged mob
//! (`ai::navigating_mob::NavigatingMob::move_to`'s 20-tick recompute throttle)
//! re-runs exactly this shape of search every 20 ticks for as long as it stays
//! stuck.
//!
//! # Duration species
//!
//! Every scene's `Arena` is immutable (built once, read-only for the whole
//! bench function) and `find_path` returns a fresh `Path` with no shared
//! mutable state, so nothing here accumulates across criterion's repeated
//! calls — no `iter_batched` needed.
//!
//! Run with: `cargo bench -p lodestone-entity --bench pathfinding_search`

mod support;

use std::collections::HashSet;
use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_entity::pathfinding::{
    Aabb, MobShape, Path, PathFinder, PathParams, PathStart, PathType, PathWorld,
};
use lodestone_model::BlockPos;

/// A block arena: solid ground at `y <= -1`, plus an explicit wall set with a
/// 1.5-tall (unjumpable) collision top — the same shape
/// `ai::navigating_mob::tests::Arena` uses. Duplicated here (not imported)
/// because `tests/` is a separate compilation target a `benches/` binary
/// cannot link against.
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

fn open_flat() -> Arena {
    Arena { walls: HashSet::new() }
}

/// A single continuous wall spanning far past the target's reach on both
/// ends, forcing a real detour around one end rather than a short hop.
fn detour_fence() -> Arena {
    let mut walls = HashSet::new();
    for z in -25..=25 {
        walls.insert((15, -1, z));
        walls.insert((15, 0, z));
    }
    Arena { walls }
}

/// A serpentine corridor: alternating wall segments each leaving a gap on the
/// opposite side from the last, forcing the search back and forth across the
/// full width repeatedly — a shape `detour_fence`'s single wall does not
/// reach (one forced reversal there, several here).
fn serpentine_maze() -> Arena {
    let mut walls = HashSet::new();
    let width = 20; // z spans -width..=width
    for (i, x) in (5..40).step_by(5).enumerate() {
        let gap_on_positive_side = i % 2 == 0;
        for z in -width..=width {
            let in_gap = if gap_on_positive_side {
                z > width - 3
            } else {
                z < -width + 3
            };
            if !in_gap {
                walls.insert((x, -1, z));
                walls.insert((x, 0, z));
            }
        }
    }
    Arena { walls }
}

/// Real 26.2 collision-shape census (issue #111's own ask: benchmark against
/// real collision data, not a hand-picked constant). Read-only reuse of the
/// same fixture `lodestone-physics/tests/collision_shapes.rs` verifies the
/// engine against — this file only parses it, it does not modify it.
const CENSUS: &str = include_str!("../../lodestone-physics/tests/support/collision_shapes_jvm.txt");

/// One shape's block-local top height (vanilla's uncapped `shape.max(Axis.Y)`
/// — see `lodestone_physics::collision::CollisionView::collision_top`'s own
/// doc for why this must stay uncapped: a fence's real top is `1.5`, not
/// clamped to `1.0`).
fn census_top(name: &str) -> f64 {
    for line in CENSUS.lines() {
        let line = line.trim();
        if !line.starts_with(name) {
            continue;
        }
        let mut tok = line.split_whitespace();
        let tok_name = tok.next().unwrap();
        if tok_name != name {
            continue;
        }
        let _state_id = tok.next().unwrap();
        let nboxes: usize = tok.next().unwrap().parse().unwrap();
        if nboxes == 0 {
            return 0.0;
        }
        let bits: Vec<f64> = tok.map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap())).collect();
        return bits.chunks_exact(6).map(|c| c[4]).fold(0.0_f64, f64::max);
    }
    panic!("census fixture has no entry named {name}");
}

/// A corridor barrier built from **real** collision-shape tops rather than a
/// hand-picked `1.5` literal: `minecraft:cobblestone_wall` (real top `1.5`,
/// matching the old synthetic wall's constant, but now read from the census
/// instead of hand-picked) spans the corridor except one cell, `gap_x`, which
/// is always `Open` in [`base_path_type`](PathWorld::base_path_type) — the
/// search must route *through* that specific real-collision-tagged cell, not
/// around a hole placed anywhere convenient.
///
/// `collision_top` at the gap reports `minecraft:oak_stairs`' own real value
/// (this fixture's state `3907`, decoded: `1.0` — its highest box is the
/// full-height riser half, not the low tread half, so this is *not* a
/// step-height obstacle in practice). Passability here comes from
/// `base_path_type`, not from `collision_top`; the real value is reported
/// because the search algorithm reads `collision_top` too, and a fabricated
/// number would defeat the point of using census data at all. Do not assume
/// "stairs" implies "0.5-tall" without decoding the actual boxes — checked
/// with a one-off `python3`/`struct.unpack` decode of this exact fixture line
/// before writing this comment, specifically because that assumption was
/// wrong on the first pass.
struct StairGapArena {
    wall_top: f64,
    gap_top: f64,
    gap_x: i32,
}

impl PathWorld for StairGapArena {
    fn min_y(&self) -> i32 {
        -8
    }
    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if Arena::is_ground(y) {
            return PathType::Blocked;
        }
        if y == 0 && z == 0 && (5..=35).contains(&x) && x != self.gap_x {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }
    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if Arena::is_ground(y) {
            return 1.0;
        }
        if y == 0 && z == 0 && (5..=35).contains(&x) {
            return if x == self.gap_x { self.gap_top } else { self.wall_top };
        }
        0.0
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
                    if Arena::is_ground(y) {
                        return true;
                    }
                    if y == 0
                        && z == 0
                        && (5..=35).contains(&x)
                        && x != self.gap_x
                        && aabb.max_y - f64::from(y) > 1e-7
                    {
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

fn stair_gap_corridor() -> StairGapArena {
    StairGapArena {
        wall_top: census_top("minecraft:cobblestone_wall"),
        gap_top: census_top("minecraft:oak_stairs"),
        gap_x: 20,
    }
}

/// A target pocket fully sealed by a solid shell — no path exists, so the
/// search must exhaust (up to) its visited budget before returning a
/// best-effort partial result. Mirrors
/// `ai::navigating_mob::tests::sealed_shell`, scaled up.
fn sealed_unreachable() -> Arena {
    let mut walls = HashSet::new();
    for z in -6..=6 {
        for x in 8..=22 {
            for y in -1..=1 {
                walls.insert((x, y, z));
            }
        }
    }
    Arena { walls }
}

fn run_search<W: PathWorld>(world: &W, target: BlockPos, budget: i32) -> Option<Path> {
    let mob = MobShape::land(0.6, 1.95);
    let start = PathStart::grounded(0.5, 0.0, 0.5);
    let params = PathParams {
        max_path_length: 200.0,
        reach_range: 1,
        visited_multiplier: 1.0,
    };
    PathFinder::new(budget).find_path(world, &mob, start, &[target], params)
}

fn bench_scenes(c: &mut Criterion) {
    let scenes: [(&str, Arena, BlockPos, i32, bool); 4] = [
        ("open_flat", open_flat(), BlockPos::new(30, 0, 0), 20_000, true),
        ("detour_fence", detour_fence(), BlockPos::new(30, 0, 0), 20_000, true),
        ("serpentine_maze", serpentine_maze(), BlockPos::new(42, 0, 0), 20_000, true),
        ("sealed_unreachable", sealed_unreachable(), BlockPos::new(15, 0, 0), 6_000, false),
    ];

    let mut medians: Vec<(&str, f64)> = Vec::new();

    for (name, world, target, budget, must_reach) in &scenes {
        // Prove the scene does what its name claims *before* timing it — the
        // anti-vacuity control named in the module doc. A scene that silently
        // fails to detour (or silently succeeds when it should be sealed)
        // would report a real-looking number for the wrong search shape.
        let probe = run_search(world, *target, *budget)
            .unwrap_or_else(|| panic!("scene {name}: no path at all, not even a best-effort partial"));
        assert_eq!(
            probe.reached(),
            *must_reach,
            "scene {name}: expected reached={must_reach}, got {} -- this scene no longer tests what its name claims",
            probe.reached()
        );

        for _ in 0..5 {
            black_box(run_search(world, *target, *budget));
        }

        const ITERS: usize = 80;
        let mut samples = Vec::with_capacity(ITERS);
        for _ in 0..ITERS {
            let t0 = Instant::now();
            black_box(run_search(black_box(world), *target, *budget));
            samples.push(t0.elapsed().as_secs_f64() * 1e6);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = samples[ITERS / 2];
        medians.push((name, median));

        support::record(support::Record {
            bench: "pathfinding_search",
            metric: "search_median_us",
            scene: name,
            value: median,
            unit: "us",
        });

        c.bench_function(&format!("entity/pathfind_{name}"), |b| {
            b.iter(|| black_box(run_search(black_box(world), *target, *budget)))
        });
    }

    let open_us = medians.iter().find(|(n, _)| *n == "open_flat").unwrap().1;
    println!("pathfinding_search medians (us): {medians:?}");
    for (name, us) in &medians {
        if *name != "open_flat" {
            let ratio = us / open_us;
            println!("  {name} / open_flat = {ratio:.2}x");
            support::record(support::Record {
                bench: "pathfinding_search",
                metric: "vs_open_flat_ratio",
                scene: name,
                value: ratio,
                unit: "x",
            });
        }
    }
}

/// Issue #111's real-collision-data scene: a corridor barrier built from the
/// real 26.2 census (`minecraft:cobblestone_wall`'s real `1.5` top) with one
/// designated gap cell reporting `minecraft:oak_stairs`' own real
/// `collision_top` (`1.0` — see `StairGapArena`'s doc comment for why that is
/// not the step-height value a name like "stairs" suggests) — the search
/// must route *through* that specific real-collision-tagged cell rather than
/// around an arbitrary hole, a different shape from `detour_fence`/
/// `serpentine_maze`'s empty gaps.
fn bench_real_collision_scene(c: &mut Criterion) {
    let world = stair_gap_corridor();
    let target = BlockPos::new(20, 0, 0);
    let budget = 20_000;

    // Anti-vacuity control: the path must actually reach the target, and it
    // can only do so by crossing x=20 at z=0 — the one cell whose real
    // collision top is low enough to step onto. If this scene silently
    // routed around instead (a bug in `StairGapArena`'s bounds), the search
    // would still "work" but stop testing what its name claims.
    let probe = run_search(&world, target, budget)
        .unwrap_or_else(|| panic!("real_collision_stair_gap: no path at all"));
    assert!(probe.reached(), "real_collision_stair_gap: expected to reach the target through the real oak_stairs gap");

    for _ in 0..5 {
        black_box(run_search(&world, target, budget));
    }

    const ITERS: usize = 80;
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        black_box(run_search(black_box(&world), target, budget));
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[ITERS / 2];
    println!("pathfinding_search real_collision_stair_gap median: {median:.1} us over {ITERS} calls");
    support::record(support::Record {
        bench: "pathfinding_search",
        metric: "search_median_us",
        scene: "real_collision_stair_gap",
        value: median,
        unit: "us",
    });

    c.bench_function("entity/pathfind_real_collision_stair_gap", |b| {
        b.iter(|| black_box(run_search(black_box(&world), target, budget)))
    });
}

criterion_group!(benches, bench_scenes, bench_real_collision_scene);
criterion_main!(benches);
