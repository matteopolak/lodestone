use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick,
    tick_among_entities, NearbyEntity, PushSelf,
};

struct Wall;

impl CollisionView for Wall {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if x == 1 {
            out.push(Aabb::new(
                1.0, y as f64, z as f64, 2.0, y as f64 + 1.0, z as f64 + 1.0,
            ));
        }
    }

    fn stuck_multiplier(&self, _: i32, _: i32, _: i32) -> Option<Vec3d> {
        Some(Vec3d::new(0.25, 0.05, 0.25))
    }

    fn is_water(&self, _: i32, _: i32, _: i32) -> bool {
        true
    }
}

fn moving(spectator: bool) -> PlayerState {
    let mut player = PlayerState::at(Vec3d::new(0.5, 10.2, 0.5), 0.0)
        .with_flight(true, 0.05)
        .with_spectator(spectator);
    player.velocity = Vec3d::new(0.71, -0.37, 0.13);
    player
}

#[test]
fn spectator_crosses_real_collision_and_preserves_flight_arithmetic() {
    let profile = PhysicsProfile::mc_1_21();
    let mut creative = moving(false);
    tick(&mut creative, MovementInput::NONE, &Wall, &profile);
    assert!((creative.position.x - 0.7).abs() < 1e-6);
    assert!(creative.horizontal_collision, "the wall must stop the control");

    let mut spectator = moving(true);
    spectator.on_ground = true;
    spectator.fall_distance = 8.3;
    tick(&mut spectator, MovementInput::NONE, &Wall, &profile);
    assert!((spectator.position.x - 1.21).abs() < 1e-12);
    assert!((spectator.position.y - 9.83).abs() < 1e-12);
    assert!((spectator.position.z - 0.63).abs() < 1e-12);
    assert!((spectator.velocity.x - 0.71 * f64::from(0.91f32)).abs() < 1e-12);
    assert!((spectator.velocity.y - -0.37 * 0.6).abs() < 1e-12);
    assert_eq!(spectator.stuck_speed_multiplier, Vec3d::ZERO);
    assert_eq!(spectator.fall_distance, 0.0);
    assert!(!spectator.on_ground && !spectator.horizontal_collision);
    assert!(!spectator.swimming);
    assert!(spectator.eye_in_water, "noclip still senses real water for the camera");
}

#[test]
fn spectator_bypasses_entity_collision_and_push_and_resumes_collision_on_exit() {
    let profile = PhysicsProfile::mc_1_21();
    let mut player = moving(true);
    let mut collider = NearbyEntity::living(
        Vec3d::new(0.9, 10.2, 0.5), Aabb::new(0.9, 9.0, 0.0, 2.0, 13.0, 1.0),
    );
    collider.collidable = true;
    collider.pushable = true;
    tick_among_entities(
        &mut player, MovementInput::NONE, &Wall, &profile, &[collider], PushSelf::LIVING_PLAYER,
    );
    assert!((player.position.x - 1.21).abs() < 1e-12);
    assert!((player.velocity.x - 0.71 * f64::from(0.91f32)).abs() < 1e-12);

    let mut resumed = moving(false);
    tick(&mut resumed, MovementInput::NONE, &Wall, &profile);
    assert!((resumed.position.x - 0.7).abs() < 1e-6);
}
