//! Pose-dependent dimensions where they meet the rest of the pipeline.
//!
//! `src/pose.rs`'s own tests pin the state machine (the fallback chain, the
//! outer guard, the deflation, vanilla's own "desired pose" priority order) and
//! `tests/golden.rs`'s six pose traces pin the arithmetic bit-for-bit against the
//! Python oracle. What is left, and what this file is for, is the *seams*: the
//! places where a pose-sized box is read by something other than the collision
//! sweep, and where a pose must **not** be recomputed.
//!
//! Every gate here carries a control that must fail the same assertion.

use std::collections::HashSet;

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::Aabb;
use lodestone_physics::player::{MovementInput, PlayerState, tick, tick_air, tick_among_entities};
use lodestone_physics::pose::Pose;
use lodestone_physics::push::{NearbyEntity, PushSelf};
use lodestone_physics::{PhysicsProfile, Vec3d, compute_fluid_state};

#[derive(Default)]
struct World {
    solid: HashSet<(i32, i32, i32)>,
    water: HashSet<(i32, i32, i32)>,
}

impl World {
    /// Floor at `y = 0`, and a solid ceiling at `y = 2` for `x >= 1`: a
    /// one-block-high corridor whose mouth is the `x = 1` plane.
    fn one_block_corridor() -> Self {
        let mut w = Self::default();
        for x in -8..=24 {
            for z in -2..=2 {
                w.solid.insert((x, 0, z));
                if x >= 1 {
                    w.solid.insert((x, 2, z));
                }
            }
        }
        w
    }

    /// A deep, wide water column with a floor far below — nothing to collide with
    /// anywhere near the player.
    fn deep_water() -> Self {
        let mut w = Self::default();
        // Wide enough that a sprint-swimmer (~0.2 blocks/tick) stays inside it for
        // the length of these gates — leaving the water would end the swim pose and
        // quietly turn a gate vacuous.
        for x in -12..=12 {
            for z in -12..=12 {
                for y in 80..=100 {
                    w.water.insert((x, y, z));
                }
                w.solid.insert((x, 70, z));
            }
        }
        w
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.solid.contains(&(x, y, z)) {
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
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
}

fn profile() -> PhysicsProfile {
    PhysicsProfile::mc_1_21()
}

fn sprint_forward() -> MovementInput {
    MovementInput {
        forward: 1.0,
        sprint: true,
        ..MovementInput::NONE
    }
}

/// The box and the eye height are one `EntityDimensions` record in vanilla, and
/// this is the measurement that says why: vanilla's own fluid-interaction
/// update bounds its cell sweep by the **box**, so a `0.6`-high box with a
/// `1.62` eye can never see the eye's own cell.
#[test]
fn a_swimming_box_with_a_standing_eye_reports_dry_eyes_while_submerged() {
    let world = World::deep_water();
    let feet = Vec3d::new(0.5, 90.0, 0.5);
    let swimming_box = Pose::Swimming.dimensions().bounding_box(feet);

    let coupled = compute_fluid_state(swimming_box, feet, Pose::Swimming.eye_height(), &world);
    assert!(
        coupled.eye_in_water,
        "a fully submerged swimmer must have wet eyes"
    );
    assert!(coupled.under_water());

    // CONTROL — the bug this coupling prevents. Same box, same position, same
    // world, only the eye height left at the standing value: the sweep covers
    // y = 90 alone, the eye sits at 91.62, and the player reads *dry* while
    // twenty blocks under water. No fog, no overlay, and vanilla's own
    // swimming-state update can never re-enter the pose.
    let split = compute_fluid_state(swimming_box, feet, 1.62, &world);
    assert!(
        !split.eye_in_water,
        "if this passes the coupling is no longer load-bearing and this gate is dead"
    );
    assert!(split.in_water(), "the box half must still see the water");
}

/// A live `tick` keeps the two in step, which is the same claim one layer up.
#[test]
fn tick_keeps_the_eye_height_in_step_with_the_pose() {
    let world = World::deep_water();
    let mut s = PlayerState::at(Vec3d::new(0.5, 90.0, 0.5), 0.0);
    let p = profile();
    assert_eq!(s.pose, Pose::Standing);
    for _ in 0..5 {
        tick(&mut s, sprint_forward(), &world, &p);
    }
    assert_eq!(s.pose, Pose::Swimming);
    assert_eq!(s.eye_height, Pose::Swimming.eye_height());
    assert!(s.eye_in_water, "the eye must still be wet after the shrink");

    // Stop sprinting: vanilla's own swimming-state update drops the flag, the
    // gate grants STANDING (open water, nothing overhead) and both fields revert together.
    for _ in 0..3 {
        tick(&mut s, MovementInput::NONE, &world, &p);
    }
    assert_eq!(s.pose, Pose::Standing);
    assert_eq!(s.eye_height, Pose::Standing.eye_height());
}

/// The immunity that follows: an out-of-band write to [`PlayerState::eye_height`]
/// between ticks cannot change where the player ends up.
///
/// This is not hypothetical. `lodestone_ecs::player::player_physics` calls its own
/// `update_pose` *after* `lodestone_physics::tick`, overwriting `eye_height` from
/// `swimming`/`sneak` alone — ungated — so in the fit-forced case (pose vetoed down
/// to CROUCHING while shift is not held) it writes `1.62` over the pose's `1.27`.
/// Because `tick` reads the pose and never this field, the damage is confined to
/// the camera. See `docs/pose-dimensions.md` for the one-line spec that closes it.
#[test]
fn tick_ignores_an_out_of_band_eye_height_write() {
    let world = World::deep_water();
    let p = profile();
    let start = || PlayerState::at(Vec3d::new(0.5, 90.0, 0.5), 0.0);

    let mut clean = start();
    let mut clobbered = start();
    for _ in 0..30 {
        tick(&mut clean, sprint_forward(), &world, &p);
        tick(&mut clobbered, sprint_forward(), &world, &p);
        // The ungated pose layer's write, verbatim: standing eye height on a
        // swimming body.
        clobbered.eye_height = 1.62;
    }
    assert_eq!(
        clean.pose,
        Pose::Swimming,
        "the run must reach a small pose"
    );
    assert_ne!(
        clean.eye_height, clobbered.eye_height,
        "the clobber must actually differ, or this gate is vacuous"
    );
    assert_eq!(
        clean.position.x.to_bits(),
        clobbered.position.x.to_bits(),
        "an eye-height write must not move the player"
    );
    assert_eq!(clean.position.y.to_bits(), clobbered.position.y.to_bits());
    assert_eq!(clean.position.z.to_bits(), clobbered.position.z.to_bits());
    assert_eq!(clean.velocity.y.to_bits(), clobbered.velocity.y.to_bits());
    assert_eq!(clean.swimming, clobbered.swimming);
    assert_eq!(clean.eye_in_water, clobbered.eye_in_water);
}

/// `tick_air` is vanilla's own travel step, not its own per-tick player
/// update. It must read the pose and
/// never write it — otherwise the 19 pre-existing golden traces that replay
/// through a travel entry point would have silently acquired a pose.
#[test]
fn the_travel_entry_points_do_not_touch_the_pose() {
    let world = World::one_block_corridor();
    let p = profile();
    let sneaking = MovementInput {
        forward: 1.0,
        sneak: true,
        ..MovementInput::NONE
    };

    let mut air = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), -90.0);
    air.on_ground = true;
    for _ in 0..20 {
        tick_air(&mut air, sneaking, &world, &p);
    }
    assert_eq!(
        air.pose,
        Pose::Standing,
        "the travel step must not run vanilla's own pose-update step"
    );
    assert_eq!(air.eye_height, Pose::Standing.eye_height());

    // CONTROL: the same 20 ticks through `tick` — which *is* vanilla's own
    // per-tick player update — do adopt the crouch, so the assertion above is about where the machine lives
    // and not about an inert machine.
    let mut full = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), -90.0);
    full.on_ground = true;
    for _ in 0..20 {
        tick(&mut full, sneaking, &world, &p);
    }
    assert_eq!(full.pose, Pose::Crouching);
}

/// The pose sizes vanilla's own bounding-box accessor, and the crowd push's
/// pair test reads that box — so a neighbour that overlaps a standing
/// player's head does not overlap a crouching one. This is also why
/// `tick_among_entities` runs the push *before* the pose: vanilla's own
/// entity-push pass is the tail of its own AI step, inside its own base
/// per-tick update, while its own pose-update step is the tail of its own
/// per-tick player update.
#[test]
fn the_push_pair_test_uses_the_pose_sized_box() {
    let world = World::default();
    let p = profile();
    // A 0.2-tall neighbour floating at head height: inside a 1.8 box, above a 1.5
    // one. (Nothing in vanilla is shaped like this; it is the cleanest way to make
    // the *height* of the pair test observable at all, since width is 0.6 in every
    // player pose.)
    let feet = Vec3d::new(0.5, 1.0, 0.5);
    let perch = Vec3d::new(0.62, 2.6, 0.5);
    let neighbour = NearbyEntity::living(perch, Aabb::new(0.32, 2.6, 0.2, 0.92, 2.8, 0.8));

    let mut standing = PlayerState::at(feet, 0.0);
    standing.on_ground = true;
    tick_among_entities(
        &mut standing,
        MovementInput::NONE,
        &world,
        &p,
        &[neighbour],
        PushSelf::LIVING_PLAYER,
    );
    assert!(
        standing.velocity.x < 0.0,
        "a standing box reaches y = 2.8 and must be shoved (vx {})",
        standing.velocity.x
    );

    // CONTROL: identical everything, crouching. The 1.5 box tops out at 2.5, the
    // boxes do not intersect, and vanilla's own pushable-entities query returns nothing.
    let mut crouching = PlayerState::at(feet, 0.0).with_pose(Pose::Crouching);
    crouching.on_ground = true;
    tick_among_entities(
        &mut crouching,
        MovementInput::NONE,
        &world,
        &p,
        &[neighbour],
        PushSelf::LIVING_PLAYER,
    );
    assert_eq!(
        crouching.velocity.x, 0.0,
        "a crouch box tops out at 2.5 and must not be pushed"
    );
}

/// Vanilla has **no** recovery for a player whose box grows into a space it
/// does not fit: vanilla's own dimensions-refresh step gates its own
/// size-growth recovery on "not client-side and ... not a player", excluding
/// us twice.
/// So the correct behaviour when a pose is seeded that cannot fit is that the
/// *pose* changes and the position does not move at all.
#[test]
fn an_unfittable_seeded_pose_shrinks_and_never_displaces_the_player() {
    let world = World::one_block_corridor();
    let p = profile();
    // Seeded STANDING at x = 3.5, deep inside a corridor where 1.8 cannot fit —
    // the state a naive port produces when a swimmer surfaces under a ledge.
    let mut s = PlayerState::at(Vec3d::new(3.5, 1.0, 0.5), 0.0).with_pose(Pose::Standing);
    s.on_ground = true;
    let before = s.position;
    tick(&mut s, MovementInput::NONE, &world, &p);
    assert_eq!(
        s.pose,
        Pose::Swimming,
        "neither STANDING nor CROUCHING fits a one-block gap, so the third arm runs"
    );
    assert_eq!(
        (s.position.x, s.position.z),
        (before.x, before.z),
        "no fudge: a pose change must never move a player horizontally"
    );
    assert_eq!(s.position.y, before.y, "…nor vertically");
}
