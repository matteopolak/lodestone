//! Acceptance gate for the server-side mob simulation.
//!
//! These drive [`MobSim`] / [`ChunkWorld`] through the crate's **public** API —
//! the point of the whole exercise is that mob AI now has a *real* consumer, not
//! another `#[cfg(test)]` fake. Two things are proven:
//!
//! 1. The sim ticks a goal-driven mob over the server's own worldgen terrain and
//!    the mob walks to its target while staying grounded.
//! 2. Over a long run against a two-tall wall the mob *detours* (it cannot jump
//!    two blocks) rather than wedging or walking through — and the recompute
//!    throttle holds for the whole run, the duration-sensitive property a short
//!    test cannot see.
//!
//! Both are hermetic and deterministic, so they always run (a gate that cannot
//! run must fail, never skip — there is no skip path here).

use lodestone_entity::ai::goals::MeleeAttackGoal;
use lodestone_entity::pathfinding::MobShape;
use lodestone_model::Vec3;
use lodestone_server::{ChunkWorld, MobSim, WorldgenChunkSource};
use lodestone_worldgen::density::Density;

/// A `y_clamped_gradient` that is positive below y=0 and negative above: a flat
/// solid floor with its surface at y=0, built from the *same* density-function
/// terrain source the server streams to clients — not a bespoke test double.
fn floor_density() -> Density {
    Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    }
}

#[test]
fn goal_driven_mob_walks_to_its_target_over_real_worldgen_terrain() {
    // The server's real terrain source (density-function noise router), snapshot
    // into a `ChunkWorld` the pathfinder can query.
    let source = WorldgenChunkSource::new(floor_density(), -64, 128);
    let world = ChunkWorld::from_source(&source, -1..=1, -1..=1);

    // Ground truth first: the snapshot really is a floor at y=0 (solid below,
    // air above). If this is wrong the mob would fall or fly and the movement
    // assertion below would be meaningless.
    assert!(world.is_solid(4, -1, 4), "expected solid floor block at y=-1");
    assert!(!world.is_solid(4, 0, 4), "expected air at y=0 (floor surface)");

    let mut sim = MobSim::new(&world);
    let start = Vec3::new(0.5, 0.0, 0.5);
    let target = Vec3::new(8.5, 0.0, 0.5);
    let id = {
        let m = sim.spawn(start, MobShape::land(0.6, 1.95), 0.15, 400);
        m.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        m.set_attack_target(Some(target));
        m.id()
    };

    sim.tick_for(600);

    let end = sim.position(id).expect("mob still present");
    // Reached: within melee range of the target on the far side.
    let dx = end.x - target.x;
    let dz = end.z - target.z;
    assert!(
        (dx * dx + dz * dz).sqrt() < 1.5,
        "mob did not reach target: ended at {end:?}, target {target:?}"
    );
    // Advanced most of the 8-block gap (not just twitching in place).
    assert!(end.x > 7.0, "mob barely moved along x: ended at {end:?}");
    // Stayed grounded on the y=0 surface the whole way — did not fall through the
    // floor or levitate.
    assert!(
        end.y.abs() < 0.6,
        "mob left the floor surface: y = {}",
        end.y
    );
    // The pathfinder actually ran (a stubbed `move_to` would never search).
    let searches = sim.get(id).unwrap().path_searches();
    assert!(searches >= 1, "no A* search ran — AI was not driven");
}

/// Builds a floor plus a two-tall solid wall from `x=-4..=4` at `z=3`, with open
/// ground on either end. A mob cannot jump two full blocks (vanilla jump ≈1.25),
/// so it must walk around a wall end to cross — the same detour invariant a live
/// zombie shows against a fence, here over the server's own block terrain.
fn walled_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -8..=8 {
        for z in -2..=12 {
            world.set_solid(x, -1, z, true); // floor, surface at y=0
        }
    }
    for x in -4..=4 {
        world.set_solid(x, 0, 3, true); // wall, two blocks tall
        world.set_solid(x, 1, 3, true);
    }
    world
}

#[test]
fn mob_detours_a_two_tall_wall_and_holds_the_recompute_throttle() {
    let world = walled_world();

    // Ground truth: the wall really is two tall and there is open ground past its
    // end at x=5 (so a detour genuinely exists and the wall genuinely blocks the
    // straight line). Without this the "detour" assertion could pass vacuously on
    // a world with no wall at all.
    assert!(world.is_solid(0, 0, 3) && world.is_solid(0, 1, 3), "wall not two-tall");
    assert!(!world.is_solid(5, 0, 3) && !world.is_solid(5, 1, 3), "no gap past wall end");

    let mut sim = MobSim::new(&world);
    let start = Vec3::new(0.5, 0.0, 0.5);
    let target = Vec3::new(0.5, 0.0, 8.5); // directly across the wall
    let id = {
        let m = sim.spawn(start, MobShape::land(0.6, 1.95), 0.15, 600);
        m.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        m.set_attack_target(Some(target));
        m.id()
    };

    const TICKS: u64 = 2000;
    let mut max_abs_x = 0.0_f64;
    for _ in 0..TICKS {
        sim.tick();
        let p = sim.position(id).unwrap();
        max_abs_x = max_abs_x.max(p.x.abs());
    }

    let end = sim.position(id).unwrap();
    // Reached the far side despite the wall between start and target.
    assert!(
        (end.z - target.z).abs() < 1.5 && (end.x - target.x).abs() < 1.5,
        "mob did not reach the far side: ended at {end:?}"
    );
    // It got there by going *around* the wall end (x=4), not through it: some
    // point on the route pushed |x| past the wall's edge.
    assert!(
        max_abs_x > 4.0,
        "mob never detoured around the wall end (max |x| = {max_abs_x:.2}) — did it walk through?"
    );
    // Never phased into a wall cell at any observed step (checked implicitly by
    // reaching without teleporting): assert it stayed grounded at the end too.
    assert!(end.y.abs() < 0.6, "mob left the floor: y = {}", end.y);

    // Duration-sensitive end-state: the 20-tick recompute throttle held for the
    // whole 2000-tick run. A regression to per-tick A* would push this toward
    // ~2000 searches; the throttle caps it near TICKS/20. This is the "navigator
    // that quietly hammers A* over time" detector — invisible at 200 ticks.
    let searches = sim.get(id).unwrap().path_searches();
    assert!(
        searches < (TICKS as u32) / 15,
        "recompute throttle regressed: {searches} A* searches over {TICKS} ticks"
    );
}
