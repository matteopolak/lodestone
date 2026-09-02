//! `travel_in_air` seam tests — proving the gravity + drag + input-assembly core
//! is genuinely entity-agnostic and is the *same* integrator the player runs.
//!
//! Vanilla's own airborne travel step used to live inline in the player's
//! `tick_air`. It was extracted into the public [`travel_in_air`] seam so a mob loop can call it
//! without reimplementing gravity and drag — the "second copy of vanilla motion"
//! failure this crate exists to prevent. Two things must hold, and this file
//! asserts both rather than assuming them:
//!
//! * **One integrator.** The player path must be *expressible through* the seam,
//!   not merely similar to it. `player_free_fall_routes_through_the_seam` drives
//!   the public `tick_air` and an independent `travel_in_air` call over the same
//!   empty world and asserts the velocity traces are **bit-identical** — if the
//!   player ever grew a private gravity/drag copy, they would drift.
//!
//! * **The box still decides collision through the seam.** The §12.31 guard, now
//!   aimed at the new entry: `width_still_bridges_a_gap_through_the_seam` drops a
//!   wide mob and a player through `travel_in_air` (gravity + collision together,
//!   not `move_entity` in isolation) over a 1×1 hole. A wrong box diverges by ten
//!   blocks; a flush case could not tell the two apart.

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::{
    AirTravelContext, EntityDimensions, EntityMotion, MovementInput, PhysicsProfile, PlayerState,
    tick_air, travel_in_air,
};

/// A zombie-shaped box: same `0.6` width as the player but taller (`1.95`) and
/// the same `0.6` step — used to show the seam runs for a non-player entity.
const ZOMBIE: EntityDimensions = EntityDimensions::new(0.6, 1.95, 0.6);

/// A deliberately wider mob (half-width `0.7`) that overhangs a 1-wide hole the
/// `0.3`-half player box drops straight into.
const WIDE_MOB: EntityDimensions = EntityDimensions::new(1.4, 1.4, 0.6);

/// Empty air everywhere — nothing to collide with, so `travel_in_air` reduces to
/// the pure gravity + drag update.
struct Void;

impl CollisionView for Void {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

/// A full floor at `top_y` with a single 1×1 hole at column `(0, *, 0)` and a
/// catch floor at `catch_y`: a box that fits the hole falls through, one that
/// overhangs it rests on top.
struct GapWorld {
    top_y: i32,
    catch_y: i32,
    hole: (i32, i32),
}

impl CollisionView for GapWorld {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let solid = (y == self.top_y && (x, z) != self.hole) || y == self.catch_y;
        if solid {
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

/// A resting-air context: no jump, no effects, no ladder — the free-fall case.
fn falling_ctx(yaw: f32) -> AirTravelContext {
    AirTravelContext {
        yaw,
        ..AirTravelContext::default()
    }
}

#[test]
fn player_free_fall_routes_through_the_seam() {
    // Drive the *public* player tick and an independent entity-shaped seam call
    // over the same empty world with zero input, and assert the falls are
    // bit-identical. This is the actual test of whether the seam is the player's
    // integrator or merely a lookalike: if `tick_air` had kept a private
    // gravity/drag copy, one ULP of divergence would surface within a few ticks.
    let profile = PhysicsProfile::mc_1_21();
    let world = Void;

    let mut player = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    let mut mob = EntityMotion::at(Vec3d::new(0.5, 100.0, 0.5));

    let mut saw_real_fall = false;
    for _ in 0..40 {
        tick_air(&mut player, MovementInput::NONE, &world, &profile);
        // The player's own speed accessor at rest is its walk speed; with zero input the
        // rotated acceleration is zero, so any non-zero speed cannot leak in — the
        // fall is pure gravity + drag, exactly what the mob call below sees.
        travel_in_air(
            &mut mob,
            EntityDimensions::PLAYER,
            (0.0, 0.0),
            0.1,
            falling_ctx(0.0),
            &world,
            &profile,
        );

        assert_eq!(
            player.velocity.y.to_bits(),
            mob.velocity.y.to_bits(),
            "player and seam vertical velocity must match bit-for-bit"
        );
        assert_eq!(
            player.position.y.to_bits(),
            mob.position.y.to_bits(),
            "player and seam vertical position must match bit-for-bit"
        );
        if player.velocity.y < -0.05 {
            saw_real_fall = true;
        }
    }
    // Guard against a vacuous pass (both stuck at zero): the trace must be a real
    // accelerating fall, not a pair of motionless entities agreeing trivially.
    assert!(saw_real_fall, "the comparison must exercise a genuine fall");
    assert!(player.position.y < 90.0, "the entity should have fallen");
}

#[test]
fn seam_runs_for_a_non_player_entity() {
    // A zombie box (taller than the player) free-falls through the seam and obeys
    // the same vanilla constants: each airborne tick subtracts gravity then scales
    // Y by the vertical air drag. The drag is *not* raw `0.98F`: vanilla runs it
    // through `computeModifiedFriction(0.98F, AIR_DRAG_MODIFIER)` = `1 - (1-0.98)*1`
    // in `float`, which does not round-trip back to `0.98F`. And gravity is the
    // `float` literal `0.08F`, whose widening to `double` is `0.0799999982...`,
    // *not* a clean `0.08` — asserting either as a tidy decimal would be wrong for
    // the exact reason this crate exists. Transcribe both independently (not via
    // the crate helpers) and put the f32→f64 widen where vanilla does.
    let vertical_drag = f64::from((1.0f32 - (1.0f32 - 0.98f32) * 1.0f32).clamp(0.0, 1.0));
    let gravity = f64::from(0.08f32);
    let profile = PhysicsProfile::mc_1_21();
    let world = Void;
    let mut mob = EntityMotion::at(Vec3d::new(0.5, 100.0, 0.5));

    travel_in_air(
        &mut mob,
        ZOMBIE,
        (0.0, 0.0),
        0.0,
        falling_ctx(0.0),
        &world,
        &profile,
    );
    // v1 = (0 - gravity) * verticalDrag
    assert_eq!(mob.velocity.y, (0.0 - gravity) * vertical_drag);

    let v1 = mob.velocity.y;
    travel_in_air(
        &mut mob,
        ZOMBIE,
        (0.0, 0.0),
        0.0,
        falling_ctx(0.0),
        &world,
        &profile,
    );
    // v2 = (v1 - gravity) * verticalDrag — gravity subtracted *before* drag.
    assert_eq!(mob.velocity.y, (v1 - gravity) * vertical_drag);
    assert!(!mob.on_ground, "still airborne over the void");
}

#[test]
fn width_still_bridges_a_gap_through_the_seam() {
    // The §12.31 guard aimed at the new entry point: the box must decide collision
    // when the sweep runs *inside* `travel_in_air` (gravity applied same tick),
    // not only when `move_entity` is called directly.
    let world = GapWorld {
        top_y: 0,
        catch_y: -10,
        hole: (0, 0),
    };
    let profile = PhysicsProfile::mc_1_21();

    let mut player = EntityMotion::at(Vec3d::new(0.5, 5.0, 0.5));
    player.velocity = Vec3d::new(0.0, -20.0, 0.0);
    travel_in_air(
        &mut player,
        EntityDimensions::PLAYER,
        (0.0, 0.0),
        0.0,
        falling_ctx(0.0),
        &world,
        &profile,
    );

    let mut wide = EntityMotion::at(Vec3d::new(0.5, 5.0, 0.5));
    wide.velocity = Vec3d::new(0.0, -20.0, 0.0);
    travel_in_air(
        &mut wide,
        WIDE_MOB,
        (0.0, 0.0),
        0.0,
        falling_ctx(0.0),
        &world,
        &profile,
    );

    assert_eq!(player.position.y, -9.0, "player box falls through the hole");
    assert!(player.on_ground);
    assert_eq!(wide.position.y, 1.0, "wide box bridges the hole");
    assert!(wide.on_ground);
    // Same world, same drop, only `width` differs — a 10-block divergence the
    // seam must honour, invisible to any flush-contact case.
    assert_eq!(player.position.y - wide.position.y, -10.0);
}
