//! `LivingEntity.pushEntities` at the pipeline level: where in the tick the push
//! lands, and the proof that an empty neighbour slice is the pre-change behaviour
//! bit for bit.
//!
//! The rule's own arithmetic (the `sqrt(absMax)` normaliser, the widened `0.01f`
//! floor and `0.05f` scale, the near-contact clamp, the team truth table, the
//! vehicle/ladder/spectator vetoes) is unit-tested inside
//! `lodestone_physics::push`. The bit-exact trajectory comparison against the
//! independent Python oracle is `golden.rs`'s `entity_push_*`. This file covers the
//! two things neither can see.

use std::collections::HashSet;

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::player::{MovementInput, PlayerState, tick, tick_among_entities};
use lodestone_physics::push::{NearbyEntity, PushSelf};
use lodestone_physics::{EntityDimensions, PhysicsProfile};

struct Floor(HashSet<(i32, i32, i32)>);

impl Floor {
    fn flat(r: i32) -> Self {
        let mut s = HashSet::new();
        for x in -r..=r {
            for z in -r..=r {
                s.insert((x, 0, z));
            }
        }
        Self(s)
    }
}

impl CollisionView for Floor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.0.contains(&(x, y, z)) {
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

/// A ladder in every cell — `LivingEntity.isPushable()` vetoes on `onClimbable()`.
struct Ladders(Floor);

impl CollisionView for Ladders {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        self.0.collision_boxes(x, y, z, out);
    }
    fn is_climbable(&self, _x: i32, _y: i32, _z: i32) -> bool {
        true
    }
}

fn grounded(x: f64, y: f64, z: f64) -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(x, y, z), 0.0);
    s.on_ground = true;
    s
}

fn neighbour_at(x: f64, y: f64, z: f64) -> NearbyEntity {
    NearbyEntity::living(
        Vec3d::new(x, y, z),
        EntityDimensions::PLAYER.bounding_box(Vec3d::new(x, y, z)),
    )
}

#[test]
fn the_push_lands_on_velocity_after_the_move_not_on_this_tick_s_position() {
    // Vanilla runs `pushEntities` at the end of `aiStep` (`LivingEntity.java:3163`),
    // *after* `travel` (`:3130`). So on the tick a neighbour first overlaps, the
    // position must be exactly what an unpushed tick would give, and only the
    // velocity differs. A "clamp/nudge the position" or "push before travel" port
    // gets tick 1 wrong and then agrees from tick 2 — a one-tick divergence that
    // looks fine on screen.
    let view = Floor::flat(6);
    let profile = PhysicsProfile::mc_1_21();
    let nearby = [neighbour_at(0.65, 1.0, 0.58)];

    let mut pushed = grounded(0.5, 1.0, 0.5);
    let mut plain = grounded(0.5, 1.0, 0.5);
    tick_among_entities(
        &mut pushed,
        MovementInput::NONE,
        &view,
        &profile,
        &nearby,
        PushSelf::LIVING_PLAYER,
    );
    tick(&mut plain, MovementInput::NONE, &view, &profile);

    assert_eq!(
        pushed.position.x.to_bits(),
        plain.position.x.to_bits(),
        "the first pushed tick must not have moved yet"
    );
    assert_eq!(pushed.position.z.to_bits(), plain.position.z.to_bits());
    assert_eq!(pushed.position.y.to_bits(), plain.position.y.to_bits());
    assert!(
        pushed.velocity.x < plain.velocity.x,
        "…but the velocity must already carry the impulse"
    );
    assert!(pushed.velocity.z < plain.velocity.z);
    assert_eq!(
        pushed.velocity.y.to_bits(),
        plain.velocity.y.to_bits(),
        "the push is horizontal-only: Y must be untouched"
    );

    // Second tick: now the position diverges, which is what makes the first-tick
    // equality above an ordering assertion rather than a claim that nothing happens.
    tick_among_entities(
        &mut pushed,
        MovementInput::NONE,
        &view,
        &profile,
        &nearby,
        PushSelf::LIVING_PLAYER,
    );
    tick(&mut plain, MovementInput::NONE, &view, &profile);
    assert!(pushed.position.x < plain.position.x);
}

#[test]
fn tick_among_entities_with_no_neighbours_is_tick_bit_for_bit() {
    // The inertness proof for the whole change at the pipeline level. Run a mix of
    // grounded, airborne and colliding starts so this is not a single lucky case.
    let view = Floor::flat(6);
    let profile = PhysicsProfile::mc_1_21();
    let starts = [
        (grounded(0.5, 1.0, 0.5), MovementInput::NONE),
        (
            grounded(0.5, 1.0, 0.5),
            MovementInput {
                forward: 1.0,
                sprint: true,
                jump: true,
                ..MovementInput::NONE
            },
        ),
        (
            PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 33.0),
            MovementInput {
                sneak: true,
                ..MovementInput::NONE
            },
        ),
    ];
    for (start, input) in starts {
        let mut a = start;
        let mut b = start;
        for _ in 0..40 {
            tick(&mut a, input, &view, &profile);
            tick_among_entities(&mut b, input, &view, &profile, &[], PushSelf::LIVING_PLAYER);
        }
        assert_eq!(a.position.x.to_bits(), b.position.x.to_bits());
        assert_eq!(a.position.y.to_bits(), b.position.y.to_bits());
        assert_eq!(a.position.z.to_bits(), b.position.z.to_bits());
        assert_eq!(a.velocity.x.to_bits(), b.velocity.x.to_bits());
        assert_eq!(a.velocity.y.to_bits(), b.velocity.y.to_bits());
        assert_eq!(a.velocity.z.to_bits(), b.velocity.z.to_bits());
        assert_eq!(a.on_ground, b.on_ground);
    }
}

#[test]
fn a_ladder_holds_you_against_a_crowd_and_the_control_shows_the_crowd_would_move_you() {
    // `LivingEntity.isPushable()` = `isAlive() && !isSpectator() && !onClimbable()`,
    // so the same crowd that shoves a standing player cannot budge one on a ladder.
    // Both halves run here because "no motion" alone is satisfied by a broken push.
    let profile = PhysicsProfile::mc_1_21();
    let crowd: Vec<NearbyEntity> = [(0.65, 0.5), (0.35, 0.5), (0.5, 0.65), (0.5, 0.35)]
        .into_iter()
        .map(|(x, z)| neighbour_at(x, 1.0, z))
        .collect();

    let floor = Floor::flat(6);
    let mut standing = grounded(0.5, 1.0, 0.5);
    // Deliberately asymmetric: three of the four are cancelled in pairs, so any
    // motion here comes from the fourth and cannot be an artefact of summing order.
    let asymmetric = &crowd[..3];
    for _ in 0..5 {
        tick_among_entities(
            &mut standing,
            MovementInput::NONE,
            &floor,
            &profile,
            asymmetric,
            PushSelf::LIVING_PLAYER,
        );
    }
    assert_ne!(
        standing.velocity.z, 0.0,
        "CONTROL: the crowd must move an ordinary player, or the test below is vacuous"
    );

    let ladders = Ladders(Floor::flat(6));
    let mut climbing = grounded(0.5, 1.0, 0.5);
    for _ in 0..5 {
        tick_among_entities(
            &mut climbing,
            MovementInput::NONE,
            &ladders,
            &profile,
            asymmetric,
            PushSelf::LIVING_PLAYER,
        );
    }
    assert_eq!(
        climbing.position.x, 0.5,
        "onClimbable must veto the push entirely"
    );
    assert_eq!(climbing.position.z, 0.5);
}
