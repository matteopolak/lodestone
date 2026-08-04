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
use lodestone_entity::{AttributeMap, DamageFlags, Defenses};
use lodestone_model::ResourceKey;
use lodestone_model::Vec3;
use lodestone_model::action::ClientAction;
use lodestone_model::adapter::{
    AdapterError, ConnectionState, Directive, EntityBaseDimensions, LoginProfile, ServerAddress,
    VersionAdapter, WorldSink,
};
use lodestone_server::{
    ChunkWorld, EntitySnapshot, EntitySource, MobSim, WorldgenChunkSource, resolve_mob_shape,
};
use lodestone_worldgen::density::Density;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

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
    assert!(
        world.is_solid(4, -1, 4),
        "expected solid floor block at y=-1"
    );
    assert!(
        !world.is_solid(4, 0, 4),
        "expected air at y=0 (floor surface)"
    );

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
    assert!(
        world.is_solid(0, 0, 3) && world.is_solid(0, 1, 3),
        "wall not two-tall"
    );
    assert!(
        !world.is_solid(5, 0, 3) && !world.is_solid(5, 1, 3),
        "no gap past wall end"
    );

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

/// The identity/motion accessors `bulk-encoders` consumes to build entity-spawn
/// and move packets must expose **real derived values**, not placeholders that
/// happen to compile. This drives a mob east over real terrain and asserts:
///   * `uuid` is unique per mob and stable across ticks (needed verbatim in the
///     spawn packet);
///   * `entity_type` defaults to a valid key and is overridable;
///   * `velocity` is in **blocks/tick** (≈ the 0.15 step, not ×20 blocks/sec —
///     the exact scale bug the wire packing would hide);
///   * `rotation`/`head_yaw` face the movement direction (due-east ⇒ yaw ≈ −90).
#[test]
fn identity_and_motion_accessors_expose_real_derived_state() {
    let source = WorldgenChunkSource::new(floor_density(), -64, 128);
    let world = ChunkWorld::from_source(&source, -1..=1, -1..=1);

    let mut sim = MobSim::new(&world);
    let start = Vec3::new(0.5, 0.0, 0.5);
    let target = Vec3::new(8.5, 0.0, 0.5); // due east of the start
    let (id_a, uuid_a) = {
        let m = sim.spawn(start, MobShape::land(0.6, 1.95), 0.15, 400);
        m.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        m.set_attack_target(Some(target));
        // Default entity_type is a valid, namespaced key.
        assert_eq!(m.entity_type().to_string(), "minecraft:zombie");
        (m.id(), m.uuid())
    };
    // A second mob gets a distinct UUID and an overridable type.
    let (id_b, uuid_b) = {
        let m = sim.spawn(Vec3::new(0.5, 0.0, 4.5), MobShape::land(0.9, 0.9), 0.25, 400);
        m.set_entity_type("minecraft:pig".parse().unwrap());
        assert_eq!(m.entity_type().to_string(), "minecraft:pig");
        (m.id(), m.uuid())
    };
    assert_ne!(uuid_a, uuid_b, "distinct mobs must get distinct UUIDs");

    // Let the first mob get up to cruising speed, still far from its target.
    sim.tick_for(10);

    let a = sim.get(id_a).unwrap();
    // UUID is stable across ticks.
    assert_eq!(a.uuid(), uuid_a, "UUID changed across ticks");

    // The version-free snapshot the encode seam consumes carries the same
    // derived state as the accessors — a real lowering, not defaults.
    let snap = a.snapshot();
    assert_eq!(snap.id, id_a);
    assert_eq!(snap.uuid, uuid_a);
    assert_eq!(snap.entity_type.to_string(), "minecraft:zombie");
    assert_eq!(snap.position, a.position());
    assert_eq!(snap.rotation, a.rotation());
    assert_eq!(snap.head_yaw, a.head_yaw());
    assert_eq!(snap.velocity, a.velocity());

    // Velocity is blocks/tick: horizontal speed near the 0.15 step, decisively
    // NOT ~3.0 (the ×20 blocks/sec scale bug bulk-encoders warned about).
    let v = a.velocity();
    let speed = (v.x * v.x + v.z * v.z).sqrt();
    assert!(
        (0.05..=0.16).contains(&speed),
        "velocity not in blocks/tick: speed = {speed:.3} (expected ~0.15, not ~3.0)"
    );
    assert!(v.x > 0.1, "mob heading toward +X target should have vx>0: {v:?}");
    assert!(v.z.abs() < 0.05, "straight-east path should have ~0 vz: {v:?}");

    // Body rotation faces the motion: due-east (+X) is yaw −90 in MC convention,
    // pitch level for a ground mob.
    let rot = a.rotation();
    assert!(
        (rot.yaw - (-90.0)).abs() < 30.0,
        "body yaw not facing east: yaw = {} (expected ~-90)",
        rot.yaw
    );
    assert_eq!(rot.pitch, 0.0, "ground mob body pitch should be level");
    assert!(a.head_yaw().is_finite(), "head yaw must be a real angle");

    // The second mob, never ticked toward a target, is idle: zero velocity.
    let b = sim.get(id_b).unwrap();
    let bv = b.velocity();
    assert!(
        bv.x.abs() < 1e-9 && bv.z.abs() < 1e-9,
        "idle mob should have zero velocity: {bv:?}"
    );
}

/// The exact wrapper the integrated server will hand to a connection task: the
/// shared, ticking simulation viewed as an [`EntitySource`]. Because
/// `EntitySource: Send + Sync`, this `impl` only compiles if `MobSim` is `Send`
/// — which it is *only* because `Goal: Send` and `PathWorld: Send + Sync`. So
/// this type is itself a standing proof of that seam, and its `snapshots()`
/// lowers each live mob through the same `SimMob::snapshot()` the encoders read.
struct SimSource<'w>(Arc<Mutex<MobSim<'w>>>);

impl EntitySource for SimSource<'_> {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0.lock().unwrap().iter().map(|m| m.snapshot()).collect()
    }
}

/// Closes the "a real ticking mob is actually visible" gap: a real `MobSim`
/// behind the `Arc<Mutex<…>>` the integrated server uses, viewed as an
/// `EntitySource`, yields snapshots that track the mob's real movement across
/// ticks — spawn appears, then position advances toward the target. bulk-encoders
/// proved the transport/client half with a stand-in source; this proves the
/// other half of the same seam with a *real* sim (no stand-in), so the two meet
/// in the middle.
#[test]
fn real_mobsim_behind_arc_mutex_is_an_entity_source_that_tracks_movement() {
    let source = WorldgenChunkSource::new(floor_density(), -64, 128);
    let world = ChunkWorld::from_source(&source, -1..=1, -1..=1);

    let sim = Arc::new(Mutex::new(MobSim::new(&world)));
    let start = Vec3::new(0.5, 0.0, 0.5);
    let target = Vec3::new(8.5, 0.0, 0.5);
    let id = {
        let mut guard = sim.lock().unwrap();
        let m = guard.spawn(start, MobShape::land(0.6, 1.95), 0.15, 400);
        m.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        m.set_attack_target(Some(target));
        m.id()
    };

    let entities = SimSource(sim.clone());

    // Before any tick: exactly one snapshot, our mob, at the spawn position.
    let before = entities.snapshots();
    assert_eq!(before.len(), 1, "one spawned mob should yield one snapshot");
    assert_eq!(before[0].id, id);
    assert_eq!(before[0].position, start, "pre-tick snapshot is the spawn point");

    // Drive the shared sim (the integrated server's sim loop does this), then
    // read the source again — the same seam a connection task reads each pass.
    sim.lock().unwrap().tick_for(60);
    let after = entities.snapshots();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, id, "same entity id across ticks");

    // The snapshot moved toward +X with the real sim — not frozen at spawn, and
    // not teleported past the target.
    assert!(
        after[0].position.x > before[0].position.x + 1.0,
        "snapshot did not track the mob's real eastward movement: {} -> {}",
        before[0].position.x,
        after[0].position.x
    );
    assert!(
        after[0].position.x <= target.x + 0.5,
        "mob overshot the target: {:?}",
        after[0].position
    );
    // Grounded the whole way (the snapshot y stays on the floor surface).
    assert!(
        after[0].position.y.abs() < 0.6,
        "mob left the floor surface: y = {}",
        after[0].position.y
    );
}

/// Builds a 1-wide walled corridor along +z with a **two-block-high tunnel** over
/// its middle: floor at y=-1, side walls 3 tall at x=±1 (so a mob cannot detour
/// laterally), and a solid ceiling at y=2 over z=4..=6 leaving exactly two blocks
/// of headroom (air at y=0 and y=1). A mob that occupies two vertical cells fits;
/// one that needs a third does not.
fn tunnel_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for z in -2..=12 {
        for x in -1..=1 {
            world.set_solid(x, -1, z, true); // floor, surface at y=0
        }
        for y in 0..=2 {
            world.set_solid(-1, y, z, true); // side walls, 3 tall
            world.set_solid(1, y, z, true);
        }
    }
    for z in 4..=6 {
        world.set_solid(0, 2, z, true); // low ceiling: 2-high tunnel
    }
    world
}

/// A minimal [`VersionAdapter`] answering `entity_dimensions` from a fixed table
/// (real census numbers) and panicking on everything else — the census consumer
/// only ever calls `entity_dimensions`, so a real registry adapter slots in
/// unchanged. This proves the seam end-to-end (census → fold → shape → path)
/// without naming a version crate.
#[derive(Debug)]
struct CensusStub(std::collections::HashMap<i32, EntityBaseDimensions>);

impl CensusStub {
    fn with(pairs: &[(i32, f32, f32)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|&(id, width, height)| (id, EntityBaseDimensions { width, height }))
                .collect(),
        )
    }
}

impl VersionAdapter for CensusStub {
    fn protocol_version(&self) -> i32 {
        0
    }
    fn minecraft_versions(&self) -> &'static [&'static str] {
        &[]
    }
    fn supports(&self, _protocol: i32) -> bool {
        false
    }
    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        unimplemented!("census stub")
    }
    fn handle_packet(
        &self,
        _world: &mut dyn WorldSink,
        _state: ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        unimplemented!("census stub")
    }
    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        unimplemented!("census stub")
    }
    fn entity_dimensions(&self, entity_type_id: i32) -> Option<EntityBaseDimensions> {
        self.0.get(&entity_type_id).copied()
    }
}

/// The census consumer, gated on *consequence*: a mob's real hitbox height,
/// folded through [`resolve_mob_shape`], decides whether it can path a two-high
/// tunnel. A 1.95-tall zombie fits; a 2.9-tall enderman does not — and swapping
/// the enderman's census height for a wrong 1.8 flips the outcome, proving the
/// value (not the world geometry) is what bites.
#[test]
fn census_height_decides_whether_a_mob_fits_a_two_high_tunnel() {
    let world = tunnel_world();

    // Ground truth: the tunnel is exactly two blocks of headroom, the approach is
    // open above it, and the side walls really enclose the corridor — so the only
    // route to the far side runs *through* the tunnel.
    assert!(
        !world.is_solid(0, 0, 5) && !world.is_solid(0, 1, 5),
        "tunnel is not open at y=0/y=1"
    );
    assert!(world.is_solid(0, 2, 5), "tunnel has no ceiling at y=2");
    assert!(
        !world.is_solid(0, 2, 2),
        "approach should have open headroom at y=2"
    );
    assert!(
        world.is_solid(-1, 1, 5) && world.is_solid(1, 1, 5),
        "corridor is not walled at x=±1"
    );

    // Real census geometry, folded through the actual consumer with a default
    // attribute map (scale 1.0, step_height 0.6 from the registry).
    let adapter = CensusStub::with(&[
        (151, 0.6, 1.95), // minecraft:zombie
        (41, 0.6, 2.9),   // minecraft:enderman
    ]);
    let attrs = AttributeMap::new();
    let zombie = resolve_mob_shape(&adapter, 151, &attrs).expect("zombie census");
    let enderman = resolve_mob_shape(&adapter, 41, &attrs).expect("enderman census");
    // The mechanism: the 2.9-tall enderman needs a third vertical cell.
    assert_eq!(zombie.cell_height(), 2, "zombie should occupy 2 cells");
    assert_eq!(enderman.cell_height(), 3, "enderman should occupy 3 cells");

    let start = Vec3::new(0.5, 0.0, 0.5);
    let target = Vec3::new(0.5, 0.0, 9.5); // past the tunnel

    let run = |shape: MobShape| -> f64 {
        let mut sim = MobSim::new(&world);
        let id = {
            let m = sim.spawn(start, shape, 0.15, 600);
            m.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
            m.set_attack_target(Some(target));
            m.id()
        };
        sim.tick_for(1500);
        sim.position(id).expect("mob present").z
    };

    // The 1.95-tall zombie clears the tunnel and reaches the far side...
    let zombie_z = run(zombie);
    assert!(
        zombie_z > 8.0,
        "zombie failed to clear a 2-high tunnel it fits: z = {zombie_z:.2}"
    );
    // ...while the real 2.9-tall enderman is stopped short of the ceiling.
    let enderman_z = run(enderman);
    assert!(
        enderman_z < 4.0,
        "2.9-tall enderman wrongly cleared a 2-high tunnel: z = {enderman_z:.2}"
    );

    // Bite / non-vacuity: feed the *same* enderman a wrong 1.8 census height
    // (cell_height 2) and it now clears the tunnel — the real 2.9 is what blocks
    // it, not the world.
    let wrong = CensusStub::with(&[(41, 0.6, 1.8)]);
    let enderman_wrong = resolve_mob_shape(&wrong, 41, &attrs).expect("wrong census");
    assert_eq!(enderman_wrong.cell_height(), 2);
    let wrong_z = run(enderman_wrong);
    assert!(
        wrong_z > 8.0,
        "a wrong 1.8-height enderman should clear the tunnel (bite check): z = {wrong_z:.2}"
    );
}

/// Closes the `damage.rs` island: before this, `MeleeAttackGoal` calling
/// `mob.attack(target)` only pushed to a `Vec` for test assertions — no health
/// value anywhere ever changed. A freshly spawned mob's stats must now be the
/// *real* per-type attributes (`Zombie.createAttributes()`: `max_health` 20,
/// `attack_damage` 3, `armor` 2 — `lodestone_entity::attribute`'s own
/// hand-verified template, not a number invented for this test), proving the
/// wiring reads real data rather than a hardcoded placeholder.
#[test]
fn spawned_mob_combat_stats_are_the_real_zombie_attributes() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);
    let m = sim.spawn(Vec3::new(0.5, 0.0, 0.5), MobShape::land(0.6, 1.95), 0.15, 100);
    assert_eq!(m.health(), 20.0, "zombie max_health is 20");
    assert_eq!(m.attack_damage(), 3.0, "zombie attack_damage override is 3.0");
    assert_eq!(m.defenses().armor, 2.0, "zombie armor override is 2.0");
}

/// The load-bearing acceptance gate for the damage pipeline's real consumer.
///
/// Two mobs: an attacker chasing a defender's position *and* id (the identity
/// a goal's `Vec3`-only seam cannot carry, which is why `SimMob` now tracks
/// `attack_target_id` separately), and a third, untouched bystander as the
/// control. The defender's health is staged at exactly the attacker's raw
/// damage with zero armor, so **one** connecting hit is exactly lethal — an
/// unambiguous, closed-form expected value, not "health went down some".
#[test]
fn melee_attack_reduces_target_health_and_a_lethal_hit_removes_the_mob() {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -4..=12 {
        for z in -4..=4 {
            world.set_solid(x, -1, z, true);
        }
    }

    let mut sim = MobSim::new(&world);
    let attacker_pos = Vec3::new(0.5, 0.0, 0.5);
    let defender_pos = Vec3::new(4.5, 0.0, 0.5);

    let defender_id = {
        let d = sim.spawn(defender_pos, MobShape::land(0.6, 1.95), 0.0, 100);
        // Zero armor and health exactly equal to the attacker's raw damage:
        // the first connecting hit must be exactly lethal, nothing more.
        d.set_defenses(Defenses::default());
        d.set_health(3.0);
        d.id()
    };
    let bystander_id = {
        // Never targeted by anything — the control proving health changes are
        // per-mob, not some global decay this mechanism could hide behind.
        let b = sim.spawn(Vec3::new(0.5, 0.0, 8.5), MobShape::land(0.6, 1.95), 0.0, 100);
        b.id()
    };
    let attacker_id = {
        let a = sim.spawn(attacker_pos, MobShape::land(0.6, 1.95), 0.2, 400);
        a.set_attack_damage(3.0);
        a.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        a.set_attack_target(Some(defender_pos));
        a.set_attack_target_id(Some(defender_id));
        a.id()
    };

    assert_eq!(
        sim.get(defender_id).unwrap().health(),
        3.0,
        "defender starts at the staged 3.0 health"
    );

    let mut ticks_run = 0;
    for _ in 0..2000 {
        sim.tick();
        ticks_run += 1;
        if sim.get(defender_id).is_none() {
            break;
        }
    }

    assert!(
        sim.get(defender_id).is_none(),
        "a lethal hit must remove the defender from the sim (ran {ticks_run} ticks)"
    );
    assert!(
        sim.get(attacker_id).is_some(),
        "the attacker is untouched and must still be present"
    );
    // Control: the bystander was never in anyone's attack_target_id and must
    // be exactly as healthy as it spawned — the mechanism is per-target, not a
    // blanket health decay that would kill it too.
    assert_eq!(
        sim.get(bystander_id).unwrap().health(),
        20.0,
        "an untargeted mob's health must be untouched"
    );
    assert_eq!(sim.len(), 2, "exactly the defender was removed");
}

/// The i-frame gate must actually gate: two attackers hitting the *same*
/// defender the same tick apply only one full hit's worth of damage, not two —
/// the control that the mechanism (not just the arithmetic) is wired. Without
/// `HurtCooldown` in the loop, `apply_damage` would be called twice and the
/// defender would take double damage.
#[test]
fn two_attackers_hitting_the_same_tick_only_land_one_full_hit() {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -4..=4 {
        for z in -4..=4 {
            world.set_solid(x, -1, z, true);
        }
    }
    let mut sim = MobSim::new(&world);
    let defender_pos = Vec3::new(2.5, 0.0, 0.5);
    let defender_id = {
        let d = sim.spawn(defender_pos, MobShape::land(0.6, 1.95), 0.0, 100);
        d.set_defenses(Defenses::default());
        d.set_health(100.0);
        d.id()
    };
    // Both attackers start already adjacent, so the very first tick both
    // strike (no travel needed to desync who lands first).
    for start in [Vec3::new(2.5, 0.0, 1.4), Vec3::new(2.5, 0.0, -0.4)] {
        let a = sim.spawn(start, MobShape::land(0.6, 1.95), 0.0, 100);
        a.set_attack_damage(10.0);
        a.add_goal(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        a.set_attack_target(Some(defender_pos));
        a.set_attack_target_id(Some(defender_id));
    }

    sim.tick();

    let health = sim.get(defender_id).unwrap().health();
    assert_eq!(
        health, 90.0,
        "the i-frame gate must cap the same tick to one full hit's damage, got health={health}"
    );
}

/// Closes the `explosion.rs` island: before this, `seen_percent` /
/// `entity_damage` had no consumer anywhere in the tree — only their own
/// hermetic unit tests exercised them (against `OpenAir` or a hand-rolled
/// blocked stub, never a real terrain query). [`MobSim::explode`] is the real
/// consumer: it samples exposure against the sim's own [`ChunkWorld`] and
/// lands the result through the same [`SimMob::apply_damage`] pipeline a
/// melee hit uses.
///
/// A point-blank mob (staged at low health) must die, and a mob roughly twice
/// as far — but still within the blast's `2 * radius` — must take strictly
/// less damage and survive. Both start with zero armor so the only thing
/// separating them is distance/exposure, not defense.
#[test]
fn explosion_damages_exposed_mobs_more_up_close_and_kills_at_ground_zero() {
    // Deliberately blank terrain: every ray is unobstructed, isolating the
    // distance/exposure falloff from any wall effects (that is the next test).
    let world = ChunkWorld::new(-8, 32);
    let mut sim = MobSim::new(&world);
    let centre = Vec3::new(0.0, 0.0, 0.0);

    let near_id = {
        let m = sim.spawn(Vec3::new(0.5, 0.0, 0.0), MobShape::land(0.6, 1.95), 0.0, 10);
        m.set_defenses(Defenses::default());
        m.set_health(5.0);
        m.id()
    };
    let far_id = {
        let m = sim.spawn(Vec3::new(6.0, 0.0, 0.0), MobShape::land(0.6, 1.95), 0.0, 10);
        m.set_defenses(Defenses::default());
        m.id()
    };

    let dealt = sim.explode(centre, 4.0, DamageFlags::default());

    let near_dealt = dealt
        .iter()
        .find(|(id, _)| *id == near_id)
        .map(|(_, d)| *d)
        .expect("the point-blank mob must be in the damaged set");
    let far_dealt = dealt
        .iter()
        .find(|(id, _)| *id == far_id)
        .map(|(_, d)| *d)
        .unwrap_or(0.0);

    assert!(
        sim.get(near_id).is_none(),
        "a point-blank TNT-scale blast (46+ raw damage) must kill a 5-health mob"
    );
    assert!(
        near_dealt > far_dealt,
        "the nearer mob must take strictly more damage: near={near_dealt} far={far_dealt}"
    );
    assert!(far_dealt > 0.0, "the far mob is still within 2*radius and must take some damage");
    let far_health = sim.get(far_id).unwrap().health();
    assert!(
        far_health < 20.0 && far_health > 0.0,
        "the far mob should be hurt but survive: health={far_health}"
    );
}

/// The control for the previous test: exposure is *ray-sampled*, not a bare
/// distance falloff. A solid wall placed only in the +x plane between the
/// blast and one mob must fully shield it, while a second mob at the exact
/// same distance but on the +z axis (where there is no wall) takes real
/// damage — proving `ChunkWorld`'s new [`RayView`] impl is what the exposure
/// model actually reads, not a stand-in that always reports clear.
#[test]
fn explosion_exposure_is_ray_sampled_a_wall_fully_shields_a_mob() {
    let mut world = ChunkWorld::new(-8, 32);
    for y in -1..=3 {
        for z in -3..=3 {
            world.set_solid(1, y, z, true);
        }
    }
    let mut sim = MobSim::new(&world);
    let centre = Vec3::new(0.0, 0.0, 0.0);

    let shielded_id = {
        let m = sim.spawn(Vec3::new(3.0, 0.0, 0.0), MobShape::land(0.6, 1.95), 0.0, 10);
        m.set_defenses(Defenses::default());
        m.id()
    };
    let exposed_id = {
        let m = sim.spawn(Vec3::new(0.0, 0.0, 3.0), MobShape::land(0.6, 1.95), 0.0, 10);
        m.set_defenses(Defenses::default());
        m.id()
    };

    let dealt = sim.explode(centre, 4.0, DamageFlags::default());

    assert!(
        dealt.iter().all(|(id, _)| *id != shielded_id),
        "the wall must fully block exposure, so the shielded mob takes no damage"
    );
    assert_eq!(
        sim.get(shielded_id).unwrap().health(),
        20.0,
        "shielded mob's health must be exactly untouched"
    );
    let exposed_dealt = dealt.iter().find(|(id, _)| *id == exposed_id).map(|(_, d)| *d);
    assert!(
        exposed_dealt.is_some_and(|d| d > 0.0),
        "the unshielded mob at the same distance must take real damage: {exposed_dealt:?}"
    );
}

// -- `MobSim::spawn_species`: the #205 driver --------------------------
//
// Before this, `SimMob::entity_type` defaulted to `minecraft:zombie`
// unconditionally and `MobSim::spawn`'s `GoalSelector` started empty, so
// every spawned mob — whatever species a caller thought it was placing —
// was behaviourally and nominally identical. These gates drive
// `spawn_species` (never hand-building a `SimMob` with a manually forced
// entity_type/goal set) and assert real, jar-verified per-species numbers
// (`lodestone_entity::attribute`'s own hand-verified `type_spec` table),
// not invented test constants.

fn rk(name: &str) -> ResourceKey {
    ResourceKey::from_str(name).expect("valid resource key")
}

/// `type_spec`'s zombie override (`attribute.rs`: `follow_range` 35,
/// `movement_speed` 0.23, `attack_damage` 3.0, `armor` 2.0) and pig override
/// (`max_health` 10, `movement_speed` 0.25) must actually reach a spawned
/// mob's real fields — shape from the 26.2 dimension census
/// (`entity_dimensions.rs`: zombie `(0.6, 1.95)`, pig `(0.9, 0.9)`), not the
/// old universal zombie placeholder box.
#[test]
fn spawn_species_resolves_real_per_species_shape_speed_and_combat_stats() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);

    let zombie_height = {
        let zombie = sim.spawn_species(rk("minecraft:zombie"), Vec3::new(0.5, 0.0, 0.5));
        assert_eq!(*zombie.entity_type(), rk("minecraft:zombie"));
        assert_eq!(zombie.health(), 20.0, "zombie max_health");
        assert_eq!(zombie.attack_damage(), 3.0, "zombie attack_damage override");
        assert_eq!(zombie.defenses().armor, 2.0, "zombie armor override");
        assert_eq!(zombie.shape().width, 0.6, "zombie census width");
        assert_eq!(zombie.shape().height, 1.95, "zombie census height");
        zombie.shape().height
    };

    let pig_height = {
        let pig = sim.spawn_species(rk("minecraft:pig"), Vec3::new(5.0, 0.0, 0.5));
        assert_eq!(*pig.entity_type(), rk("minecraft:pig"));
        assert_eq!(pig.health(), 10.0, "pig max_health override");
        assert_eq!(pig.shape().width, 0.9, "pig census width");
        assert_eq!(pig.shape().height, 0.9, "pig census height");
        pig.shape().height
    };

    // Species differ observably, not just internally: two different
    // entity_types must not collapse to the same body or stats.
    assert_ne!(zombie_height, pig_height);
}

/// The behavioural half of #205's own worked example: "a `minecraft:pig`
/// never acquires a melee target, a `minecraft:zombie` does." A pig's
/// `spawn_species` goal set has no `MeleeAttackGoal` at all, so setting an
/// attack target on it (exactly as the zombie below is given one) can never
/// produce a connecting hit — structurally, not by chance/timing. The zombie
/// closes the same distance and its own real `attack_damage` (3.0, not a
/// hand-set test constant) is exactly lethal against a pig staged at 3.0
/// health, mirroring this file's existing `melee_attack_reduces_target_
/// health…` pattern but through the species-aware entry point.
#[test]
fn spawn_species_only_the_hostile_species_can_ever_land_a_melee_hit() {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -4..=12 {
        for z in -4..=4 {
            world.set_solid(x, -1, z, true);
        }
    }
    let mut sim = MobSim::new(&world);

    let zombie_pos = Vec3::new(0.5, 0.0, 0.5);
    let target_pos = Vec3::new(4.5, 0.0, 0.5);

    let victim_id = {
        let pig = sim.spawn_species(rk("minecraft:pig"), target_pos);
        pig.set_defenses(Defenses::default());
        pig.set_health(3.0); // exactly the zombie's real attack_damage
        pig.id()
    };
    let zombie_id = {
        let zombie = sim.spawn_species(rk("minecraft:zombie"), zombie_pos);
        assert_eq!(zombie.attack_damage(), 3.0, "real species attribute, not staged");
        zombie.set_attack_target(Some(target_pos));
        zombie.set_attack_target_id(Some(victim_id));
        zombie.id()
    };

    // Control, run first: the pig is given an attack target/id pointing at
    // the zombie too, exactly like the zombie's own setup below — proving
    // any difference in outcome comes from the goal set `spawn_species` gave
    // each species, not from one of them simply never being told to attack.
    let control_zombie_health = {
        let control_world = ChunkWorld::new(-4, 24);
        let mut control_sim = MobSim::new(&control_world);
        let z = control_sim.spawn_species(rk("minecraft:zombie"), zombie_pos);
        let z_id = z.id();
        let p = control_sim.spawn_species(rk("minecraft:pig"), target_pos);
        p.set_attack_target(Some(zombie_pos));
        p.set_attack_target_id(Some(z_id));
        for _ in 0..500 {
            control_sim.tick();
        }
        control_sim.get(z_id).map(|m| m.health())
    };
    assert_eq!(
        control_zombie_health,
        Some(20.0),
        "a pig given an attack target must never actually connect: no MeleeAttackGoal exists to act on it"
    );

    let mut ticks_run = 0;
    for _ in 0..2000 {
        sim.tick();
        ticks_run += 1;
        if sim.get(victim_id).is_none() {
            break;
        }
    }
    assert!(
        sim.get(victim_id).is_none(),
        "the zombie's real attack_damage must land and be exactly lethal (ran {ticks_run} ticks)"
    );
    assert!(sim.get(zombie_id).is_some(), "the zombie is untouched");
}

// -- Creeper swell/detonate: the "creepers never prime" fix -------------
//
// Before this, `MobSim::explode` had correct exposure/damage maths (issue
// #213) and exactly two callers anywhere in the tree — both direct calls from
// this file's own explosion tests above. Nothing ever *decided* an explosion
// should happen: `SwellGoal` did not exist, no species got it, and
// `NavigatingMob` had no fuse state at all. These gates drive the whole
// chain through `MobSim::tick` — the same production entry point
// `run_mob_tick_loop` calls every server tick — with no test-only shortcut
// to the detonation trigger.

/// The `ignite()` path: vanilla's `readAdditionalSaveData` calls this when a
/// summoned creeper carries NBT `ignited:1b`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/monster/Creeper.java:120-122`),
/// and `Creeper.java:129-131` then forces `swellDir = 1` every tick regardless
/// of proximity. Predicts the exact tick-29 value (not merely "increased" —
/// see CLAUDE.md's *magnitude* vacuous-test species) and the exact tick the
/// blast reaches a mob standing one block away.
#[test]
fn ignited_creeper_climbs_by_exactly_one_per_tick_and_detonates_at_tick_30() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);

    let creeper_id = {
        let creeper = sim.spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0));
        creeper.ignite();
        creeper.id()
    };
    let victim_id = {
        let victim = sim.spawn(Vec3::new(1.0, 0.0, 0.0), MobShape::land(0.6, 1.95), 0.0, 10);
        victim.set_defenses(Defenses::default());
        victim.set_health(20.0);
        victim.id()
    };

    for expected in 1..lodestone_entity::ai::MAX_SWELL {
        sim.tick();
        let creeper = sim.get(creeper_id).unwrap_or_else(|| {
            panic!("creeper must not detonate before tick {}, but is already gone at tick {expected}", lodestone_entity::ai::MAX_SWELL)
        });
        assert_eq!(creeper.swell(), expected, "swell must climb by exactly 1/tick while ignited");
    }
    assert_eq!(
        sim.get(creeper_id).unwrap().swell(),
        lodestone_entity::ai::MAX_SWELL - 1,
        "predicted tick-29 value"
    );
    assert_eq!(sim.get(victim_id).unwrap().health(), 20.0, "no blast yet — the victim must be untouched");

    sim.tick(); // the 30th tick: swell reaches MAX_SWELL

    assert!(
        sim.get(creeper_id).is_none(),
        "MobSim::tick must call MobSim::explode and discard the creeper on the tick its fuse completes"
    );
    let victim_health = sim.get(victim_id).map_or(0.0, |m| m.health());
    assert!(
        victim_health < 20.0,
        "the production tick path must land real explosion damage on a mob one block away, got {victim_health}"
    );
}

/// A creeper given a stationary attack target within `SwellGoal`'s 3-block
/// start range (`SwellGoal.java:20`) must prime from proximity alone, with no
/// `ignite()` call — the actual bug report ("creepers never prime near a
/// player"). Same exact-tick prediction as the ignited case.
#[test]
fn creeper_with_a_close_stationary_target_primes_from_proximity_alone() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);

    let creeper_id = {
        let creeper = sim.spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0));
        creeper.set_attack_target(Some(Vec3::new(1.0, 0.0, 0.0))); // distSqr 1 < 9
        assert!(!creeper.is_ignited(), "this path must not need ignition");
        creeper.id()
    };

    let mut detonated_at = None;
    for t in 1..=lodestone_entity::ai::MAX_SWELL {
        sim.tick();
        if sim.get(creeper_id).is_none() {
            detonated_at = Some(t);
            break;
        }
    }
    assert_eq!(
        detonated_at,
        Some(lodestone_entity::ai::MAX_SWELL),
        "a stationary target within 3 blocks must prime and detonate in exactly MAX_SWELL ticks"
    );
}

/// The negative control: a creeper that is never ignited and never given an
/// attack target must sit inert through hundreds of production ticks —
/// proving the previous two tests' detonations come from the fix, not from
/// `MobSim::tick`/`explode` firing unconditionally for every creeper.
#[test]
fn creeper_with_no_target_and_never_ignited_never_primes_or_detonates() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);

    let creeper_id = {
        let creeper = sim.spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0));
        creeper.id()
    };

    for _ in 0..300 {
        sim.tick();
    }

    let creeper = sim
        .get(creeper_id)
        .expect("an inert creeper must never detonate itself away");
    assert_eq!(creeper.swell(), 0, "swell must stay at 0 with no direction ever set positive");
    assert_eq!(creeper.swell_dir(), -1, "swell_dir must stay at vanilla's own default");
}
