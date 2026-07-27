//! Per-entity-dimension divergence tests — the §12.31 guard for the entity seam.
//!
//! The hitbox (`width`/`height`) and auto-step (`step_height`) moved out of the
//! per-version [`PhysicsProfile`] and became per-call [`EntityDimensions`], so a
//! zombie and a player in the same version can carry different boxes. The danger
//! this file exists to catch is the one §12.31 names: a wrong box that *agrees*
//! with the right one on the cases we happened to test. The old `do_move`
//! shortcut passed 16 scenarios because all 16 were **flush contacts** where two
//! different formulations coincide. Agreement is weak evidence when the cases
//! can't tell the formulations apart.
//!
//! So every scenario here is chosen so a **wrong box diverges**, not coincides:
//!
//! * `width_bridges_a_gap` — a mob wider than the player, dropped over a 1×1 hole
//!   in the floor. The player box fits *inside* the hole and falls through; the
//!   wider box spans it and is held up by the surrounding floor. Same world, same
//!   drop, same velocity — only `width` differs, and it decides a 10-block
//!   outcome. Using the player box for a wide mob would silently drop it into a
//!   pit the real mob bridges: exactly the invisible-until-the-anti-cheat class.
//!
//! * `step_height_mounts_a_ledge` — a mob with a taller `STEP_HEIGHT` than the
//!   player, walking into a full-block ledge exactly `1.0` above its feet. The
//!   player's `0.6` step can't mount it (blocked); a `1.0` step climbs it. Same
//!   world, same push — only `step_height` differs.
//!
//! Both drive the **public** [`move_entity`] entry so the test proves the
//! dimensions actually flow through the shared integrator and change the result,
//! rather than testing the geometry helper in isolation.

use lodestone_physics::collision::CollisionView;
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::{EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, move_entity};

/// The player's box, for contrast: `0.6` wide, `0.6` step.
const PLAYER: EntityDimensions = EntityDimensions::PLAYER;

/// A deliberately *wider* mob (half-width `0.7`), so its footprint overhangs a
/// 1-wide hole the `0.3`-half player box drops straight into.
const WIDE_MOB: EntityDimensions = EntityDimensions::new(1.4, 1.4, 0.6);

/// A mob with a taller auto-step (a horse-like `1.0`), same box as the player so
/// only the step height is in play.
const TALL_STEP_MOB: EntityDimensions = EntityDimensions::new(0.6, 1.8, 1.0);

/// A world with a full floor at `TOP_Y` that has a single 1×1 hole punched at
/// column `(0, *, 0)`, plus a catch floor far below at `CATCH_Y`. An entity that
/// fits inside the hole falls to the catch floor; one that overhangs it rests on
/// the top floor.
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

/// A world with a full floor at `floor_y` and one full-cube ledge block sitting
/// on top of it at `ledge` — a step whose top face is exactly `1.0` above an
/// entity resting on the floor.
struct LedgeWorld {
    floor_y: i32,
    ledge: (i32, i32, i32),
}

impl CollisionView for LedgeWorld {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let solid = y == self.floor_y || (x, y, z) == self.ledge;
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

/// Drop an entity of `dims` straight down through `world` from `start` feet and
/// return `(resting_foot_y, on_ground)` after one [`move_entity`].
fn drop_through(world: &dyn CollisionView, dims: EntityDimensions, start: Vec3d) -> (f64, bool) {
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(start);
    motion.velocity = Vec3d::new(0.0, -20.0, 0.0);
    move_entity(&mut motion, dims, world, &profile, MoveContext::default());
    (motion.position.y, motion.on_ground)
}

/// Push an entity of `dims` in `+X` while grounded and return the resting foot
/// height after one [`move_entity`] — reveals whether it stepped up the ledge.
fn walk_into_ledge(world: &dyn CollisionView, dims: EntityDimensions, feet: Vec3d) -> f64 {
    let profile = PhysicsProfile::mc_1_21();
    let mut motion = EntityMotion::at(feet);
    motion.on_ground = true;
    motion.velocity = Vec3d::new(0.5, 0.0, 0.0);
    move_entity(&mut motion, dims, world, &profile, MoveContext::default());
    motion.position.y
}

// --- Axis 1: first-principles geometry -------------------------------------
// The bridging outcome is decided by box half-width vs a 1-wide hole. Assert the
// geometry the physics *should* produce, derived from the box definition alone
// (not from the movement code), so this axis catches a misread that the seam
// axis below would agree with.

#[test]
fn box_half_widths_are_derived_in_f64() {
    // width / 2 is done in f64 *after widening the f32 width* (vanilla
    // `makeBoundingBox`). That widening is observable: `0.6f32` is not `0.6f64`,
    // so the player half-width is 0.30000001192092896, not a clean 0.3, and the
    // box edge lands at 0.19999998807907104 rather than 0.2. Asserting the naive
    // decimal here would be wrong for exactly the reason the whole crate exists —
    // encode the f32→f64 path instead.
    let player_half = f64::from(0.6f32) / 2.0;
    let player = PLAYER.bounding_box(Vec3d::new(0.5, 0.0, 0.5));
    assert_eq!(player.min_x, 0.5 - player_half);
    assert_eq!(player.max_x, 0.5 + player_half);
    assert_eq!(player.min_x, 0.199_999_988_079_071_04);

    let wide_half = f64::from(1.4f32) / 2.0;
    let wide = WIDE_MOB.bounding_box(Vec3d::new(0.5, 0.0, 0.5));
    assert_eq!(wide.min_x, 0.5 - wide_half);
    assert_eq!(wide.max_x, 0.5 + wide_half);

    // The player footprint fits strictly inside the hole span [0, 1]; the wide
    // footprint overhangs both edges. This is what decides the bridging below,
    // and it is true despite (not because of) the f32 rounding.
    assert!(player.min_x > 0.0 && player.max_x < 1.0);
    assert!(wide.min_x < 0.0 && wide.max_x > 1.0);
}

// --- Axis 2: end-to-end through the shared integrator -----------------------

#[test]
fn width_bridges_a_gap() {
    // Top floor at y=0 (top face y=1) with a 1×1 hole at column (0, *, 0); a
    // catch floor 10 blocks down at y=-10 (top face y=-9).
    let world = GapWorld {
        top_y: 0,
        catch_y: -10,
        hole: (0, 0),
    };
    let start = Vec3d::new(0.5, 5.0, 0.5); // centred over the hole

    // Player-sized box fits in the hole and falls to the catch floor.
    let (player_y, player_ground) = drop_through(&world, PLAYER, start);
    assert_eq!(player_y, -9.0, "player box should fall through the gap");
    assert!(player_ground);

    // The wider mob overhangs the hole and is held up on the top floor.
    let (wide_y, wide_ground) = drop_through(&world, WIDE_MOB, start);
    assert_eq!(wide_y, 1.0, "wide box should bridge the gap");
    assert!(wide_ground);

    // The whole point: the two boxes DIVERGE by 10 blocks on identical input.
    // A flush-contact scenario could never show this; a wrong box here is a
    // silent 10-block error, not a rounding difference.
    assert_ne!(player_y, wide_y);
    assert_eq!(player_y - wide_y, -10.0);
}

#[test]
fn using_player_box_for_a_wide_mob_is_wrong() {
    // Restates the divergence as the concrete defect it guards against: if a mob
    // simulator mistakenly fed the player box to a wide mob, it would drop the
    // mob into a pit the real mob bridges. This fails the moment `move_entity`
    // stops honouring the supplied `width`.
    let world = GapWorld {
        top_y: 0,
        catch_y: -10,
        hole: (0, 0),
    };
    let start = Vec3d::new(0.5, 5.0, 0.5);

    let correct = drop_through(&world, WIDE_MOB, start).0;
    let with_player_box = drop_through(&world, PLAYER, start).0;
    assert_eq!(correct, 1.0);
    assert_ne!(
        correct, with_player_box,
        "supplying the player box for a wide mob must not silently agree"
    );
}

#[test]
fn step_height_mounts_a_ledge() {
    // Floor at y=0 (feet rest at y=1). A full-cube ledge sits on the floor at
    // (1,1,0); its top face is y=2, exactly 1.0 above the entity's feet.
    let world = LedgeWorld {
        floor_y: 0,
        ledge: (1, 1, 0),
    };
    let feet = Vec3d::new(0.5, 1.0, 0.5);

    // The player's 0.6 step can't mount a 1.0 ledge: it stays at foot y=1.
    let player_y = walk_into_ledge(&world, PLAYER, feet);
    assert_eq!(player_y, 1.0, "0.6 step cannot climb a 1.0 ledge");

    // A 1.0-step mob climbs it: feet rise onto the ledge top at y=2.
    let mob_y = walk_into_ledge(&world, TALL_STEP_MOB, feet);
    assert_eq!(mob_y, 2.0, "1.0 step mounts the ledge");

    // Only `step_height` differs between the two runs, and it flips the outcome —
    // proving the second per-entity field also flows through `move_entity`.
    assert_ne!(player_y, mob_y);
}
