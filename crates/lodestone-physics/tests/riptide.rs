//! The riptide-trident impulse and spin-attack pose — issue #208.
//!
//! `TridentItem.releaseUsing` (`TridentItem.java:88-104`):
//!
//! ```text
//! xd = -sin(yRot * pi/180) * cos(xRot * pi/180)
//! yd = -sin(xRot * pi/180)
//! zd = cos(yRot * pi/180) * cos(xRot * pi/180)
//! dist = sqrt(xd*xd + yd*yd + zd*zd)
//! push(xd * strength/dist, yd * strength/dist, zd * strength/dist)
//! startAutoSpinAttack(20, 8.0F, itemStack)
//! if (onGround) move(SELF, (0, 1.1999999, 0))
//! ```
//!
//! `pose.rs` documents vanilla's `getDesiredPose` order as `SLEEPING >
//! SWIMMING > FALL_FLYING > SPIN_ATTACK > CROUCHING/STANDING`; these tests also
//! pin `apply_riptide`'s pose side effect against that order.

use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, Pose, PlayerState, Vec3d, apply_riptide,
    tick,
};

struct Empty;
impl CollisionView for Empty {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

#[test]
fn facing_due_south_level_pushes_purely_along_z_by_the_strength() {
    // yaw = 0 (south, `+Z`), pitch = 0 (level): the trig collapses exactly —
    // sin(0) = 0, cos(0) = 1 — so xd = 0, yd = 0, zd = 1, dist = 1, and the
    // impulse is exactly `(0, 0, strength)`. This is pure trigonometric
    // identity, not a value read from this crate.
    let world = Empty;
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    s.pitch = 0.0;
    s.velocity = Vec3d::ZERO;

    apply_riptide(&mut s, &world, &profile, 3.0);

    assert!((s.velocity.x).abs() < 1e-9, "x must be ~0, got {}", s.velocity.x);
    assert!((s.velocity.y).abs() < 1e-9, "y must be ~0, got {}", s.velocity.y);
    assert!(
        (s.velocity.z - 3.0).abs() < 1e-6,
        "z must be exactly the strength, got {}",
        s.velocity.z
    );
}

#[test]
fn the_impulse_is_additive_not_a_replacement() {
    // `Entity.push` adds to the existing velocity (`Entity.java:1919-1924`); a
    // player already moving must have that motion carried forward, not
    // overwritten.
    let world = Empty;
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    s.pitch = 0.0;
    s.velocity = Vec3d::new(0.2, 0.0, -0.1);

    apply_riptide(&mut s, &world, &profile, 3.0);

    assert!(
        (s.velocity.x - 0.2).abs() < 1e-9,
        "pre-existing x velocity must survive the push, got {}",
        s.velocity.x
    );
    assert!(
        (s.velocity.z - (3.0 - 0.1)).abs() < 1e-6,
        "z must be the pre-existing velocity PLUS the impulse, got {}",
        s.velocity.z
    );
}

#[test]
fn starts_a_twenty_tick_spin_attack_that_counts_down_and_ends() {
    let world = Empty;
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);

    apply_riptide(&mut s, &world, &profile, 1.0);
    assert_eq!(s.auto_spin_attack_ticks, 20);
    assert!(s.is_auto_spin_attack());

    for expected in (0..20).rev() {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        assert_eq!(s.auto_spin_attack_ticks, expected);
    }
    assert!(!s.is_auto_spin_attack(), "must have ended after 20 ticks");

    // Control: it does not go negative or wrap on further ticks.
    tick(&mut s, MovementInput::NONE, &world, &profile);
    assert_eq!(s.auto_spin_attack_ticks, 0);
}

#[test]
fn the_spin_attack_pose_wins_over_standing_and_crouching_but_not_fall_flying() {
    let world = Empty;
    let profile = PhysicsProfile::mc_1_21();

    // Below FALL_FLYING/SWIMMING in vanilla's `getDesiredPose` priority, but
    // above CROUCHING/STANDING: a sneaking, non-gliding, mid-spin player must
    // show SPIN_ATTACK, not CROUCHING.
    let mut spinning = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    apply_riptide(&mut spinning, &world, &profile, 1.0);
    tick(
        &mut spinning,
        MovementInput {
            sneak: true,
            ..MovementInput::NONE
        },
        &world,
        &profile,
    );
    assert_eq!(spinning.pose, Pose::SpinAttack);

    // Control: the same sneak input with no spin attack pending is CROUCHING —
    // proving the pose above came from the spin-attack branch, not from the
    // fit gate defaulting somewhere unexpected.
    let mut just_sneaking = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    tick(
        &mut just_sneaking,
        MovementInput {
            sneak: true,
            ..MovementInput::NONE
        },
        &world,
        &profile,
    );
    assert_eq!(just_sneaking.pose, Pose::Crouching);

    // Gliding still wins over an active spin attack, matching vanilla's order.
    let mut gliding_and_spinning = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    gliding_and_spinning.fall_flying = true;
    apply_riptide(&mut gliding_and_spinning, &world, &profile, 1.0);
    tick(
        &mut gliding_and_spinning,
        MovementInput::NONE,
        &world,
        &profile,
    );
    assert_eq!(gliding_and_spinning.pose, Pose::FallFlying);
}

#[test]
fn on_ground_the_launch_also_pops_the_player_up_by_1_2_blocks() {
    // `if (onGround) player.move(SELF, (0, 1.1999999F, 0))` — a real,
    // collision-resolving move (open world here, so it goes through in full).
    let world = Empty;
    let profile = PhysicsProfile::mc_1_21();
    let mut grounded = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    grounded.on_ground = true;
    let start_y = grounded.position.y;

    apply_riptide(&mut grounded, &world, &profile, 1.0);

    assert!(
        (grounded.position.y - (start_y + f64::from(1.199_999_9f32))).abs() < 1e-6,
        "on-ground launch must pop the player up by 1.1999999, got delta {}",
        grounded.position.y - start_y
    );

    // Control: an airborne player (on_ground = false) gets no such pop-up —
    // only the velocity impulse.
    let mut airborne = PlayerState::at(Vec3d::new(0.5, 60.0, 0.5), 0.0);
    airborne.on_ground = false;
    let start_y2 = airborne.position.y;
    apply_riptide(&mut airborne, &world, &profile, 1.0);
    assert_eq!(
        airborne.position.y, start_y2,
        "control: airborne must not get the on-ground pop-up"
    );
}
