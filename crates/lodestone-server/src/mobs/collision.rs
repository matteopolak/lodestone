//! Live collision resolution for server-side entities.
//!
//! This module owns the shape-aware sweep used after AI and item/orb motion.
//! Keeping it separate makes the distinction between the pathfinding snapshot
//! and the live block-state collision oracle explicit.

use super::*;

/// How far below the world's floor an item may sink before it is discarded —
/// vanilla's own "check below world" threshold (its own min-Y-minus-64 comparison).
pub(super) const VOID_DESPAWN_DEPTH: f64 = 64.0;

/// The dropped item's hitbox — vanilla's own item-entity type
/// dimensions, `0.25 × 0.25`, with **no** auto-step.
///
/// `step_height` is `0.0` rather than the `0.6` an ordinary mob resolves from its
/// `STEP_HEIGHT` attribute: vanilla's own item entity never overrides its
/// max-up-step getter, and
/// the base entity's own default returns `0.0`. Getting this wrong would let a dropped item climb
/// a slab it slid into, which is the sort of thing that looks like a physics
/// improvement in a screenshot.
pub(super) const ITEM_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.25, 0.25, 0.0);

/// A [`CollisionView`] over a caller-supplied block-state oracle, serving the real
/// per-block-state collision shapes.
///
/// # Why this exists rather than a `Fn(i32, i32, i32) -> bool`
///
/// The item pass used to take exactly that: one boolean per cell, `!is_air_or_fluid`,
/// with an item resolved as a **point** and its rest height hardcoded to `by + 1`.
/// Read out of `lodestone_data`'s generated shape table rather than predicted, that
/// is wrong for most of the blocks a player actually drops things onto:
///
/// | block state | true collision top | the boolean's answer |
/// |---|---|---|
/// | `short_grass`, `tall_grass`, `snow[layers=1]` | **0.0** — no collision at all | solid, so the item rests a full block *above* the ground |
/// | `oak_slab[type=bottom]` | 0.5 | 1.0 |
/// | `enchanting_table` | 0.75 | 1.0 |
/// | `soul_sand`, `mud`, `chest` | 0.875 | 1.0 |
/// | `dirt_path` | 0.9375 | 1.0 |
/// | `oak_fence` | **1.5** — uncapped | 1.0, i.e. *too low* |
///
/// The grass row is the one with the visible symptom, and it is the common case:
/// almost any grassy surface has a plant on it, so almost every dropped item floated.
/// The fence row is worth keeping because it fails in the opposite direction, so a
/// gate that only ever checked "not too high" would miss it.
///
/// # Cost, stated because it is strictly more work per item
///
/// The boolean was one map lookup per cell. This is a `String` from the oracle, a
/// name→id lookup, and an O(1) rodata index — then `collide` sweeps the cells the
/// item's expanded box spans rather than probing one column. `probe_count` is
/// incremented per cell so the cost is a **counter** a gate can assert on rather
/// than a duration, and `items_settled_probe_count` exposes it.
pub(super) struct LiveBlockCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
    probe_count: std::cell::Cell<u64>,
}

impl CollisionView for LiveBlockCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        self.probe_count.set(self.probe_count.get() + 1);
        let name = (self.block_state)(x, y, z);
        // The moving-piston state has no static census box because its carried
        // shape is dynamic. During the server's discrete two-cell shove it is
        // nevertheless solid for entity support and collision, matching the
        // pathfinding adapter's deliberately equivalent full-cube fallback.
        if crate::piston::is_moving_piston(&name) {
            let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
            out.push(lodestone_physics::Aabb::new(bx, by, bz, bx + 1.0, by + 1.0, bz + 1.0));
            return;
        }
        // The shared resolver preserves every named property and supplies only a
        // block's registered default for properties the input omits. Replacing it
        // with a lowest-id fallback makes a bare oak slab a full cube rather than
        // its default bottom slab, so the item rests at the wrong height.
        let Some(state) = block_state_id(&name) else {
            return;
        };
        let shape = collision_shapes::collision_boxes(state);
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(lodestone_physics::Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }
}

/// Resolves one item's collision with the terrain after [`ItemMotion::tick`] has
/// already moved it, and records whether it is resting.
///
/// This is the "world crate's job" [`ItemMotion::tick`]'s doc comment always
/// deferred and nothing ever did.
///
/// # What it models, and what it does not
///
/// Vertical only. Vanilla resolves the item's full `0.25 × 0.25 × 0.25` AABB
/// against every intersecting shape in its own generic entity-move step; this pushes the item out of
/// a solid cell it has sunk into, zeroes a downward velocity when that happens,
/// and sets `on_ground` from the cell beneath. Horizontal collision is left out
/// deliberately rather than by oversight: a dropped item's horizontal velocity is
/// a fraction of a block per tick and decays by `ITEM_AIR_DRAG` every tick, so it
/// cannot cross a wall in the time it takes to stop — whereas gravity is
/// unbounded, which is why the vertical case was the one with a visible symptom.
/// The single-column test also means an item is treated as a point at its own
/// centre rather than a cube, so it can settle in a cell whose neighbour is where
/// vanilla's wider box would have caught it. Both are visible as an item resting
/// slightly off-centre in a corner, never as an item falling through the floor.
///
/// Per-block friction is likewise not looked up: `block_friction` keeps
/// [`lodestone_entity::item_entity::DEFAULT_BLOCK_FRICTION`], so an item slides on
/// ice exactly as it does on stone. Vanilla reads
/// `getBlockPosBelowThatAffectsMyMovement().getBlock().getFriction()`; wiring that
/// needs a per-block friction census this crate does not carry.
/// Settles one item against a solidity oracle.
///
/// # Why this takes a closure and not the sim's own `ChunkWorld`
///
/// Settling uses the live `ChunkSource` supplied to
/// [`MobSim::tick_with_terrain`], so placed and removed blocks affect collision
/// at the player's current coordinates. The plain `tick` entry point retains its
/// `ChunkWorld` snapshot for hermetic callers.
pub(super) fn settle_item(view: &dyn CollisionView, motion: &mut ItemMotion, before: Vec3) {
    settle_entity(view, ITEM_DIMENSIONS, motion, before);
}

/// [`settle_item`] with the hitbox as a parameter, so an experience orb
/// (`0.5 × 0.5`) resolves against the same swept collision an item (`0.25 × 0.25`)
/// does.
///
/// Split out rather than copied: the *geometry* differs between the two entities and
/// nothing else does, and a second copy of the restitution rules is how one of them
/// ends up with the pre-swept-collision point test again. `motion` is the
/// position/velocity/`on_ground` triple only — the caller has already applied whatever
/// per-entity gravity and drag its own `tick` uses.
fn settle_entity(
    view: &dyn CollisionView,
    dimensions: EntityDimensions,
    motion: &mut ItemMotion,
    before: Vec3,
) {
    // The movement the caller's own tick just applied by translating outright. It is
    // recovered rather than recomputed so this cannot drift from that arithmetic
    // (gravity, then translate, then drag, then the landing bounce).
    let attempted = Vec3d::new(
        motion.position.x - before.x,
        motion.position.y - before.y,
        motion.position.z - before.z,
    );

    // **The ordering is deliberately identical to what it replaced**: gravity and
    // drag still happen inside `ItemMotion::tick`, before the collision, and this
    // still runs after. Vanilla's own item-entity per-tick update collides
    // *between* them, so its
    // friction reads the post-move `onGround`. Matching that is a separate change to
    // a crate outside this one; keeping the order fixed here means the only thing
    // the implementation alters is the **geometry**, which is what makes the existing
    // settling gates still meaningful rather than merely still green.
    let bb = dimensions.bounding_box(Vec3d::new(before.x, before.y, before.z));
    let resolved = collide(view, attempted, bb, motion.on_ground, dimensions.step_height);

    motion.position = Vec3::new(
        before.x + resolved.x,
        before.y + resolved.y,
        before.z + resolved.z,
    );

    // Vanilla's own generic entity-move step's own "restitute movement after
    // collisions" helper: zero each component the
    // sweep could not fully apply. Horizontal is included now — the old point test
    // could not see a wall at all, and its doc comment argued that was safe because
    // an item's horizontal velocity decays before it can cross one. That argument
    // holds for *slow* items and not for a thrown one, and it is free to get right
    // here because `collide` resolves all three axes in one call.
    if (resolved.x - attempted.x).abs() > f64::EPSILON {
        motion.velocity.x = 0.0;
    }
    if (resolved.z - attempted.z).abs() > f64::EPSILON {
        motion.velocity.z = 0.0;
    }
    if (resolved.y - attempted.y).abs() > f64::EPSILON {
        motion.velocity.y = 0.0;
    }

    // Vanilla's own rule (its own generic "set on ground with movement" call): grounded when the sweep
    // ate downward movement. This replaces a point probe one epsilon below the
    // bottom face, which is why `ITEM_SUPPORT_EPSILON` is gone: there is no longer a
    // boundary-straddling floor() to defend against, and an item resting on a slab
    // has no block boundary under its feet to probe in the first place.
    motion.on_ground = attempted.y < 0.0 && (resolved.y - attempted.y).abs() > f64::EPSILON;
}

/// Applies the live world's swept block collision to one navigation-driven mob.
///
/// Navigation keeps a bounded terrain snapshot so a path search has stable
/// inputs, but its result is not permission to move through blocks changed or
/// loaded after that snapshot. This is the live authority: it resolves the
/// entity's real species-sized body against the current per-state shape table
/// after every navigation step, then commits the resolved delta back into the
/// navigator so the emitted entity snapshot and next physics step agree.
pub(super) fn settle_mob(
    view: &dyn CollisionView,
    mob: &mut NavigatingMob<'_>,
    before: Vec3,
    allow_live_fall: bool,
) {
    let shape = mob.shape();
    let dimensions = EntityDimensions::new(shape.width, shape.height, shape.max_up_step);
    // Commands and plugin hooks can create a body at an arbitrary exact
    // coordinate, including inside a placed block. A zero-length swept move
    // deliberately does not depenetrate, so escape upward through the real
    // live shape before treating this tick's navigation result as movement.
    // Teleports do not take this route: `NavigatingMob::teleport_to` declares
    // an intentional authority boundary by replacing `before`.
    if mob.needs_live_unembed() {
        if let Some(unembedded) = unembed_mob(view, dimensions, before) {
            let supported = mob_supported(view, dimensions, unembedded);
            mob.apply_live_collision(before, unembedded, supported, true);
            return;
        }
    }
    let mut attempted_position = mob.position();
    let mut attempted = Vec3d::new(
        attempted_position.x - before.x,
        attempted_position.y - before.y,
        attempted_position.z - before.z,
    );
    let mut resolved = collide(
        view,
        attempted,
        dimensions.bounding_box(Vec3d::new(before.x, before.y, before.z)),
        mob.is_on_ground(),
        dimensions.step_height,
    );
    let mut resolved_position = Vec3::new(
        before.x + resolved.x,
        before.y + resolved.y,
        before.z + resolved.z,
    );

    // A static navigation snapshot can still contain a block a player has
    // mined. If that held an idle mob level, navigation produced no downward
    // delta at all; begin the same gravity integration here and sweep the full
    // updated movement from the last accepted live position.
    if allow_live_fall && attempted.y >= 0.0 && !mob_supported(view, dimensions, resolved_position) {
        mob.begin_live_fall_from_unsupported_surface();
        attempted_position = mob.position();
        attempted = Vec3d::new(
            attempted_position.x - before.x,
            attempted_position.y - before.y,
            attempted_position.z - before.z,
        );
        resolved = collide(
            view,
            attempted,
            dimensions.bounding_box(Vec3d::new(before.x, before.y, before.z)),
            false,
            dimensions.step_height,
        );
        resolved_position = Vec3::new(
            before.x + resolved.x,
            before.y + resolved.y,
            before.z + resolved.z,
        );
    }
    let vertical_collision = (resolved.y - attempted.y).abs() > f64::EPSILON;
    let landed = attempted.y < 0.0 && vertical_collision;
    let supported = mob_supported(view, dimensions, resolved_position);
    mob.apply_live_collision(before, resolved_position, landed || supported, vertical_collision);
}

/// Moves an initially intersecting body to the highest live shape it overlaps.
///
/// Swept collision correctly clips motion that starts outside a collider, but
/// intentionally leaves a zero-delta overlap alone. The command and plugin
/// spawn seam permits such a starting point, so repeat the same collision-box
/// query used by a sweep until the body is flush above every overlapping shape.
/// The loaded world's vertical span is smaller than this bound; retaining a
/// bound avoids a malformed collision oracle turning a server tick into an
/// unbounded loop.
fn unembed_mob(view: &dyn CollisionView, dimensions: EntityDimensions, feet: Vec3) -> Option<Vec3> {
    const MAX_UNEMBED_STEPS: usize = 512;
    let mut candidate = feet;
    for _ in 0..MAX_UNEMBED_STEPS {
        let body = dimensions.bounding_box(Vec3d::new(candidate.x, candidate.y, candidate.z));
        let mut colliders = Vec::new();
        for x in body.min_x.floor() as i32 - 1..=body.max_x.floor() as i32 + 1 {
            for y in body.min_y.floor() as i32 - 1..=body.max_y.floor() as i32 + 1 {
                for z in body.min_z.floor() as i32 - 1..=body.max_z.floor() as i32 + 1 {
                    view.collision_boxes(x, y, z, &mut colliders);
                }
            }
        }
        let Some(top) = colliders
            .iter()
            .filter(|shape| body.intersects(shape))
            .map(|shape| shape.max_y)
            .max_by(f64::total_cmp)
        else {
            return (candidate != feet).then_some(candidate);
        };
        candidate.y = top;
    }
    // A malformed oracle cannot make normal movement unsafe: the next tick
    // retries the bounded escape instead of publishing an unchecked movement.
    Some(candidate)
}

/// Whether a body at `feet` is supported by any live collision shape directly
/// beneath it. The probe has no visible displacement; it solely keeps the
/// collision step's ground/auto-step state honest after block edits.
fn mob_supported(view: &dyn CollisionView, dimensions: EntityDimensions, feet: Vec3) -> bool {
    const SUPPORT_PROBE: f64 = 1.0e-4;
    let support = collide(
        view,
        Vec3d::new(0.0, -SUPPORT_PROBE, 0.0),
        dimensions.bounding_box(Vec3d::new(feet.x, feet.y, feet.z)),
        false,
        0.0,
    );
    support.y > -SUPPORT_PROBE + f64::EPSILON
}

#[cfg(test)]
mod live_mob_collision_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn state_with_floor(x: i32, y: i32, z: i32) -> String {
        let _ = (x, z);
        if y == 0 {
            "minecraft:stone".to_string()
        } else {
            AIR.to_string()
        }
    }

    fn settle(sim: &mut MobSim<'_>, state: &(dyn Fn(i32, i32, i32) -> String + Sync)) {
        for _ in 0..160 {
            sim.tick_with_terrain(state);
        }
    }

    /// A command-created mob can be outside the immutable pathfinding snapshot.
    /// These three bodies differ in both species and dimensions, but all must
    /// use the live floor and stop at its known top rather than falling toward
    /// the snapshot's minimum Y.
    #[test]
    fn live_sweep_lands_cow_zombie_and_warden_outside_the_navigation_snapshot() {
        let snapshot = ChunkWorld::new(-64, 384);
        let mut sim = MobSim::new(&snapshot);
        let ids: Vec<_> = ["cow", "zombie", "warden"]
            .into_iter()
            .map(|species| {
                sim.spawn_species(
                    format!("minecraft:{species}").parse().expect("valid key"),
                    Vec3::new(160.5, 5.0, 160.5),
                )
                .id()
            })
            .collect();

        settle(&mut sim, &state_with_floor);

        for id in ids {
            let mob = sim.get(id).expect("spawned mob remains live");
            assert!(
                (mob.position().y - 1.0).abs() < 1.0e-9,
                "{} must rest on the live full-block top, got {:?}",
                mob.entity_type(),
                mob.position()
            );
            assert!(mob.mob.is_on_ground(), "{} must report live support", mob.entity_type());
        }
    }

    /// Real collision shapes, rather than a solid/air guess: a bottom slab is
    /// 0.5 high, a fence is 1.5 high, and short grass has no collision at all.
    #[test]
    fn live_sweep_uses_slab_fence_and_non_colliding_plant_shapes() {
        for (state, expected_y) in [
            ("minecraft:oak_slab[type=bottom]", 0.5),
            ("minecraft:oak_fence[north=false,east=false,south=false,west=false,waterlogged=false]", 1.5),
            ("minecraft:short_grass", 0.0),
        ] {
            let snapshot = ChunkWorld::new(-64, 384);
            let mut sim = MobSim::new(&snapshot);
            let id = sim
                .spawn_species(
                    "minecraft:cow".parse().expect("valid key"),
                    Vec3::new(0.5, 4.0, 0.5),
                )
                .id();
            let live = |x: i32, y: i32, z: i32| {
                if x == 0 && y == 0 && z == 0 {
                    state.to_string()
                } else if y == -1 {
                    "minecraft:stone".to_string()
                } else {
                    AIR.to_string()
                }
            };
            settle(&mut sim, &live);
            let y = sim.get(id).expect("cow remains live").position().y;
            assert!(
                (y - expected_y).abs() < 1.0e-9,
                "{state} must resolve to its real collision top {expected_y}, got {y}"
            );
        }
    }

    /// Mining the live floor must make an idle mob fall even if that block is
    /// still present in the immutable navigation snapshot.
    #[test]
    fn removing_live_support_starts_gravity_without_waiting_for_a_path_refresh() {
        let mut snapshot = ChunkWorld::new(-64, 384);
        snapshot.set_block(0, 0, 0, "minecraft:stone");
        let mut sim = MobSim::new(&snapshot);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.5, 1.0, 0.5))
            .id();
        let floor_exists = AtomicBool::new(true);
        let live = |x: i32, y: i32, z: i32| {
            if floor_exists.load(Ordering::SeqCst) && x == 0 && y == 0 && z == 0 {
                "minecraft:stone".to_string()
            } else {
                AIR.to_string()
            }
        };
        sim.tick_with_terrain(&live);
        assert_eq!(sim.get(id).expect("cow").position().y, 1.0, "precondition: floor supports cow");

        floor_exists.store(false, Ordering::SeqCst);
        sim.tick_with_terrain(&live);
        assert!(
            sim.get(id).expect("cow").position().y < 1.0,
            "the live floor was removed, so the cow must begin falling despite the stale path snapshot"
        );
    }

    /// Support is an AABB overlap, not a single column chosen from the feet
    /// coordinate. These positions straddle the same live floor edge by less
    /// than one body-width, so a point/column approximation would give them
    /// the same answer even though only the first cow still overlaps the slab
    /// of floor.
    #[test]
    fn live_sweep_distinguishes_the_horizontal_edge_of_a_supporting_block() {
        for (x, should_stand) in [(1.44, true), (1.46, false)] {
            let snapshot = ChunkWorld::new(-64, 384);
            let mut sim = MobSim::new(&snapshot);
            let cow = sim
                .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(x, 1.0, 0.5))
                .id();
            let floor = |bx: i32, by: i32, bz: i32| {
                if (bx, by, bz) == (0, 0, 0) {
                    "minecraft:stone".to_string()
                } else {
                    AIR.to_string()
                }
            };
            sim.tick_with_terrain(&floor);
            let mob = sim.get(cow).expect("cow remains live");
            if should_stand {
                assert_eq!(mob.position().y, 1.0, "overlapping body must remain supported");
                assert!(mob.mob.is_on_ground());
            } else {
                assert!(mob.position().y < 1.0, "body beyond floor edge must start falling");
                assert!(!mob.mob.is_on_ground());
            }
        }
    }

    /// The command seam accepts an exact coordinate, not only a prevalidated
    /// empty cell. Starting with a living body embedded in a full block must
    /// escape through that full block's actual top; a zero-length sweep alone
    /// would otherwise leave it intersecting forever.
    #[test]
    fn live_sweep_unembeds_an_initially_embedded_living_mob() {
        let snapshot = ChunkWorld::new(-64, 384);
        let mut sim = MobSim::new(&snapshot);
        let cow = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.5, 0.0, 0.5))
            .id();
        sim.tick_with_terrain(&state_with_floor);
        let mob = sim.get(cow).expect("cow remains live");
        assert_eq!(mob.position().y, 1.0, "embedded cow must escape to the floor top");
        assert!(mob.mob.is_on_ground(), "escaped cow must report its live support");
    }

    /// A physical impulse is swept from the last accepted position. This covers
    /// the shared impulse route used by crowd push, leashes, combat, warden
    /// effects, and piston shoves; it is deliberately unlike a teleport, which
    /// resets the collision origin as an intentional instant relocation.
    #[test]
    fn an_impulse_and_a_piston_shove_cannot_cross_a_live_wall() {
        let snapshot = ChunkWorld::new(-64, 384);
        let mut sim = MobSim::new(&snapshot);
        let cow = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.5, 1.0, 0.5))
            .id();
        let live = |x: i32, y: i32, z: i32| {
            if y == 0 {
                "minecraft:stone".to_string()
            } else if x == 2 && y == 1 && z == 0 {
                "minecraft:stone".to_string()
            } else {
                AIR.to_string()
            }
        };
        sim.tick_with_terrain(&live);
        sim.get_mut(cow)
            .expect("cow")
            .apply_knockback(Vec3::new(3.0, 0.0, 0.0));
        sim.tick_with_terrain(&live);
        let after_impulse = sim.get(cow).expect("cow").position();
        let maximum_center_x = 2.0 - f64::from(sim.get(cow).expect("cow").shape().width) / 2.0;
        assert!(
            after_impulse.x <= maximum_center_x + 1.0e-9,
            "impulse crossed live wall: {after_impulse:?}"
        );

        let shoved = sim.shove_from_piston(
            BlockPos::new(1, 1, 0),
            BlockPos::new(2, 1, 0),
            crate::neighbor_update::Direction::East,
        );
        assert_eq!(shoved, vec![cow], "precondition: piston selected the cow");
        sim.tick_with_terrain(&live);
        let after_piston = sim.get(cow).expect("cow").position();
        assert!(
            after_piston.x <= maximum_center_x + 1.0e-9,
            "piston shove crossed live wall: {after_piston:?}"
        );
    }

    /// `push_entities` runs after every mob's first live sweep. Place a pig
    /// flush with the wall and a player inside its push range: the player
    /// impulse is created in that late phase, so only the final sweep can
    /// keep the snapshot from reporting an in-wall position this same tick.
    #[test]
    fn a_post_ai_push_is_clipped_before_the_entity_snapshot_is_published() {
        let mut snapshot = ChunkWorld::new(-64, 384);
        for x in 0..=2 {
            snapshot.set_block(x, 0, 0, "minecraft:stone");
        }
        snapshot.set_block(2, 1, 0, "minecraft:stone");
        let mut sim = MobSim::new(&snapshot);
        let pig = sim
            .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.55, 1.0, 0.5))
            .id();
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(1.0, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        let live = |x: i32, y: i32, z: i32| snapshot.block_state(x, y, z).to_owned();

        sim.tick_with_terrain(&live);

        let maximum_center_x = 2.0 - f64::from(sim.get(pig).expect("pig").shape().width) / 2.0;
        let snapshot = sim
            .snapshots()
            .into_iter()
            .find(|entity| entity.id == pig)
            .expect("pig must be published to the entity snapshot consumer");
        assert!(
            snapshot.position.x <= maximum_center_x + 1.0e-9,
            "post-AI player push crossed the live wall before streaming: {snapshot:?}"
        );
        assert_eq!(snapshot.position.y, 1.0, "wall clipping must retain live floor support");
    }
}

