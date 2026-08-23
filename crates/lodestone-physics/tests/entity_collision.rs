//! The **hard** half of entity interaction: entity boxes participating in
//! `Entity.collide`'s sweep, and the entity term of `CollisionGetter.noCollision`.
//!
//! Separate from `golden.rs` because none of it is reachable from a golden trace:
//! `getEntityCollisions` filters on `Entity.canBeCollidedWith`, which **no player
//! and no mob overrides** in 26.2 (`Entity.java:2381` returns `false` and
//! `LivingEntity` inherits it). Only `AbstractBoat`, `Shulker` and `HappyGhast`
//! do. So the entity collider list is empty for every scenario a two-player or
//! player-plus-mob trace can express, and these gates use hand-derived
//! expectations instead — each paired with the empty-slice control that must give
//! the pre-change answer.

use lodestone_physics::PhysicsProfile;
use lodestone_physics::collision::{CollisionView, collide, collide_among_entities, no_collision};
use lodestone_physics::entity::{
    EntityDimensions, EntityMotion, MoveContext, move_entity, move_entity_among_entities,
};
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::player::{MovementInput, PlayerState, tick_among_entities};
use lodestone_physics::push::{
    NearbyEntity, PushSelf, entity_collision_boxes, no_collision_among_entities,
    no_entity_collision,
};

/// A floor of unit cubes at `y = 0` spanning `-r..=r`, and nothing else.
struct Floor(i32);

impl CollisionView for Floor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 && x.abs() <= self.0 && z.abs() <= self.0 {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
}

/// A world with a solid ceiling at `y = 2` as well as the floor, leaving a
/// one-block-tall gap the standing 1.8-tall box cannot occupy — the shape the
/// swimming-pose fit gate exists for.
struct FloorAndLowCeiling;

impl CollisionView for FloorAndLowCeiling {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if (y == 0 || y == 2) && x.abs() <= 4 && z.abs() <= 4 {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
}

/// A boat-shaped collidable entity: `1.375 × 0.5625`, `canBeCollidedWith` true
/// (`AbstractBoat.java:119`), sitting on the floor at `(x, 1.0, z)`.
fn boat(x: f64, z: f64) -> NearbyEntity {
    let half = 1.375 / 2.0;
    let mut e = NearbyEntity::living(
        Vec3d::new(x, 1.0, z),
        Aabb::new(x - half, 1.0, z - half, x + half, 1.0 + 0.5625, z + half),
    );
    e.collidable = true;
    e
}

/// A player box (`0.6 × 1.8` from [`EntityDimensions::PLAYER`]) with feet at
/// `(x, y, z)`.
fn player_box(x: f64, y: f64, z: f64) -> Aabb {
    EntityDimensions::PLAYER.bounding_box(Vec3d::new(x, y, z))
}

/// A shulker-shaped collidable entity: `1.0 × 1.0`, `canBeCollidedWith` =
/// `isAlive()` (`Shulker.java:470`) and `isPushable()` inherited as `false`. Tall
/// enough that the 0.6 auto-step cannot mount it, which is what makes it the right
/// fixture for "an entity that actually blocks you".
fn shulker(x: f64, z: f64) -> NearbyEntity {
    let mut e = NearbyEntity::living(
        Vec3d::new(x, 1.0, z),
        Aabb::new(x - 0.5, 1.0, z - 0.5, x + 0.5, 2.0, z + 0.5),
    );
    e.collidable = true;
    e.pushable = false;
    e
}

#[test]
fn a_collidable_entity_stops_horizontal_movement_and_the_control_walks_through_it() {
    // A shulker centred at x = 2.0 has its -X face at 1.5. A player at x = 0.5 has
    // a leading face at 0.5 + 0.300000011920929, so a +0.8 move clips to
    // 1.5 - 0.800000011920929. A *boat* would not do: its 0.5625 deck is inside the
    // 0.6 auto-step, so you mount it instead — see the next test.
    let view = Floor(6);
    let bb = player_box(0.5, 1.0, 0.5);
    let mut colliders = Vec::new();
    entity_collision_boxes(
        bb.expand_towards(0.8, 0.0, 0.0),
        &[shulker(2.0, 0.5)],
        &mut colliders,
    );
    assert_eq!(
        colliders.len(),
        1,
        "the shulker must be gathered as a collider"
    );

    let resolved =
        collide_among_entities(&view, Vec3d::new(0.8, 0.0, 0.0), bb, true, 0.6, &colliders);
    let expected = 1.5 - (0.5 + f64::from(0.6_f32) / 2.0);
    assert_eq!(
        resolved.x.to_bits(),
        expected.to_bits(),
        "expected the sweep to stop flush against the shulker"
    );
    assert_eq!(resolved.y, 0.0, "1.0 tall is out of auto-step reach");

    // CONTROL — the pre-change behaviour, verbatim: with no entity colliders the
    // same move goes through unimpeded. If this ever agrees with the line above,
    // the gate is measuring the floor rather than the entity.
    let through = collide(&view, Vec3d::new(0.8, 0.0, 0.0), bb, true, 0.6);
    assert_eq!(
        through.x, 0.8,
        "block-only collide must not see the shulker"
    );
}

#[test]
fn auto_step_mounts_a_collidable_entity_the_way_it_mounts_a_slab() {
    // Entity colliders are passed to `collectCandidateStepUpHeights`
    // (`Entity.java:1158-1160`), so a boat deck at 0.5625 is a step candidate under
    // the 0.6 step height — you walk up onto the boat rather than into it.
    let view = Floor(6);
    let bb = player_box(0.5, 1.0, 0.5);
    let mut colliders = Vec::new();
    let boat = boat(1.6, 0.5);
    let movement = Vec3d::new(0.4, 0.0, 0.0);
    entity_collision_boxes(
        bb.expand_towards(movement.x, movement.y, movement.z),
        &[boat],
        &mut colliders,
    );
    let resolved = collide_among_entities(&view, movement, bb, true, 0.6, &colliders);
    assert_eq!(
        resolved.x, 0.4,
        "horizontal movement is preserved by the step"
    );
    assert_eq!(
        resolved.y, 0.5625,
        "the rise must be the boat's own deck height, not a rounded 0.5"
    );

    // CONTROL: with a step height below the deck the step cannot happen and the
    // move is clipped instead — so the assertion above is about the step mechanic
    // and not about the collider being ignored.
    let blocked = collide_among_entities(&view, movement, bb, true, 0.5, &colliders);
    assert!(blocked.x < 0.4 && blocked.y == 0.0);
}

/// The direct movement core already accepted entity colliders; this is the
/// missing production seam. A whole player tick must resolve its downward move
/// against a boat deck, not merely consult the boat during the later pose gate.
#[test]
fn tick_among_entities_lands_on_a_boat_deck_and_the_noncollidable_control_falls_through() {
    let view = Floor(6);
    let profile = PhysicsProfile::mc_1_21();
    let mut deck = boat(0.5, 0.5);
    deck.pushes_players = false;

    let mut landed = PlayerState::at(Vec3d::new(0.5, 2.0, 0.5), 0.0);
    landed.velocity = Vec3d::new(0.0, -0.8, 0.0);
    tick_among_entities(
        &mut landed,
        MovementInput::NONE,
        &view,
        &profile,
        &[deck],
        PushSelf::LIVING_PLAYER,
    );
    assert_eq!(landed.position.y, 1.5625, "feet must stop on the boat's exact deck height");
    assert!(landed.on_ground, "a downward collision with the deck is ground contact");

    let mut ghost = deck;
    ghost.collidable = false;
    let mut through = PlayerState::at(Vec3d::new(0.5, 2.0, 0.5), 0.0);
    through.velocity = Vec3d::new(0.0, -0.8, 0.0);
    tick_among_entities(
        &mut through,
        MovementInput::NONE,
        &view,
        &profile,
        &[ghost],
        PushSelf::LIVING_PLAYER,
    );
    assert!(through.position.y < 1.5625, "the control must fall through when hard collision is disabled");
}

#[test]
fn move_entity_with_an_empty_collider_slice_is_bit_identical_to_move_entity() {
    // The inertness proof for every existing caller, in code rather than in prose:
    // `move_entity` *is* `move_entity_among_entities(.., &[])`, so the 29
    // pre-existing golden traces cannot move. Run over a case that exercises
    // gravity, a downward collision, restitution and the speed factor.
    let view = Floor(6);
    let profile = PhysicsProfile::mc_1_21();
    for velocity in [
        Vec3d::new(0.3, -0.4, 0.15),
        Vec3d::new(-0.12, 0.42, 0.0),
        Vec3d::ZERO,
    ] {
        let base = EntityMotion {
            velocity,
            ..EntityMotion::at(Vec3d::new(0.5, 1.2, 0.5))
        };
        let mut a = base;
        let mut b = base;
        move_entity(
            &mut a,
            EntityDimensions::PLAYER,
            &view,
            &profile,
            MoveContext::default(),
        );
        move_entity_among_entities(
            &mut b,
            EntityDimensions::PLAYER,
            &view,
            &profile,
            MoveContext::default(),
            &[],
        );
        assert_eq!(a.position.x.to_bits(), b.position.x.to_bits());
        assert_eq!(a.position.y.to_bits(), b.position.y.to_bits());
        assert_eq!(a.position.z.to_bits(), b.position.z.to_bits());
        assert_eq!(a.velocity.x.to_bits(), b.velocity.x.to_bits());
        assert_eq!(a.velocity.y.to_bits(), b.velocity.y.to_bits());
        assert_eq!(a.velocity.z.to_bits(), b.velocity.z.to_bits());
        assert_eq!(a.on_ground, b.on_ground);
        assert_eq!(a.horizontal_collision, b.horizontal_collision);
    }
}

#[test]
fn the_pose_fit_gate_needs_both_terms_and_a_mob_contributes_to_neither() {
    // `Player.canPlayerFitWithinBlocksAndEntitiesWhen` (`Player.java:373-375`) is
    // `noCollision(this, dims(pose).makeBoundingBox(position).deflate(1.0E-7))`,
    // i.e. blocks AND entities. Reproduce its exact box for both poses in a
    // one-block gap.
    let view = FloorAndLowCeiling;
    let feet = Vec3d::new(0.5, 1.0, 0.5);
    let standing = EntityDimensions::new(0.6, 1.8, 0.6)
        .bounding_box(feet)
        .inflate(-1.0E-7);
    // Pose.SWIMMING is a flat 0.6 x 0.6 box.
    let swimming = EntityDimensions::new(0.6, 0.6, 0.6)
        .bounding_box(feet)
        .inflate(-1.0E-7);

    // Drift guard: these two boxes are hand-derived from Avatar.POSES and
    // Player.java:374, and `lodestone_physics::pose` now ships the same
    // construction. If the shipped one ever changes shape, this is where it shows.
    assert_eq!(standing, lodestone_physics::Pose::Standing.fit_box(feet));
    assert_eq!(swimming, lodestone_physics::Pose::Swimming.fit_box(feet));

    assert!(
        !no_collision(&view, standing),
        "the standing box must not fit under a ceiling at y = 2"
    );
    assert!(
        no_collision(&view, swimming),
        "the swimming box must fit — this is the gap the fit gate exists for"
    );

    // A mob standing in the same cell changes nothing: it is pushable, not
    // collidable, so `getEntityCollisions` skips it and the swimmer still fits.
    // This is why the swimming-hitbox work is *not* blocked on entity collision:
    // its entity term is vacuous for every player and every mob.
    let mob = NearbyEntity::living(feet, EntityDimensions::PLAYER.bounding_box(feet));
    assert!(
        no_collision_among_entities(&view, swimming, &[mob]),
        "a mob is not a collider — the entity term of the fit gate is vacuous for it"
    );

    // A shulker in the same cell *does* block the pose, and that is the only shape
    // of case where the entity term changes the answer.
    let mut shulker = mob;
    shulker.collidable = true;
    shulker.pushable = false; // Shulker inherits Entity.isPushable() == false
    assert!(
        !no_collision_among_entities(&view, swimming, &[shulker]),
        "a shulker is a collider and must veto the pose"
    );
    // CONTROL: the block half alone still says the swimmer fits, so the line above
    // is the entity term talking.
    assert!(no_collision(&view, swimming));
    assert!(!no_entity_collision(swimming, &[shulker]));
}

#[test]
fn a_same_vehicle_passenger_is_neither_a_collider_nor_a_pusher() {
    // `Entity.canCollideWith` (`Entity.java:2377-2379`) ends in
    // `!this.isPassengerOfSameVehicle(entity)`, so two riders of one boat do not
    // clip through the boat's own occupants.
    let feet = Vec3d::new(0.5, 1.0, 0.5);
    let probe = player_box(0.5, 1.0, 0.5);
    let mut rider = NearbyEntity::living(feet, EntityDimensions::PLAYER.bounding_box(feet));
    rider.collidable = true;
    assert!(!no_entity_collision(probe, &[rider]));
    rider.same_vehicle = true;
    assert!(
        no_entity_collision(probe, &[rider]),
        "isPassengerOfSameVehicle must drop the collider"
    );
}
