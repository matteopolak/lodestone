//! Golden-trace validation: replay each scenario through the crate and assert
//! **bit-for-bit** equality (no tolerance) against the independent Python oracle
//! in `gen_golden.py`. A tolerance would hide exactly the sub-tick drift that
//! gets a client kicked, so every `f64` is compared by its raw bits.
//!
//! The worlds built here must match `gen_golden.py`'s scenarios exactly.

use std::collections::{HashMap, HashSet};

use lodestone_physics::collision::CollisionView;
use lodestone_physics::fluid::{FluidCell, FluidKind};
use lodestone_physics::geometry::Aabb;
use lodestone_physics::player::{
    MovementInput, PlayerState, StatusEffects, tick, tick_air, tick_among_entities, tick_elytra,
};
use lodestone_physics::pose::Pose;
use lodestone_physics::push::{NearbyEntity, PushSelf};
use lodestone_physics::{PhysicsProfile, Vec3d};

#[path = "support/golden_traces.rs"]
mod golden_traces;
use golden_traces::{
    GOLDEN_ANALOG_STRAFE, GOLDEN_BLUE_ICE_SLIDE, GOLDEN_CROUCH_LOW_CORRIDOR,
    GOLDEN_CROUCH_RELEASE_STAYS_CROUCHED, GOLDEN_DIAGONAL_WALK, GOLDEN_ELYTRA_CLIMB,
    GOLDEN_ELYTRA_DIAGONAL_YAW, GOLDEN_ELYTRA_DIVE, GOLDEN_ELYTRA_GAP_GLIDE,
    GOLDEN_ELYTRA_GLIDE_LEVEL, GOLDEN_ENTITY_PUSH_FLUSH_CONTROL, GOLDEN_ENTITY_PUSH_SHOVE,
    GOLDEN_ENTITY_PUSH_WIDE_PLATEAU, GOLDEN_FREE_FALL, GOLDEN_HONEY_JUMP, GOLDEN_ICE_SLIDE,
    GOLDEN_JUMP_BOOST, GOLDEN_LADDER_CLIMB, GOLDEN_LADDER_SNEAK_HOLD, GOLDEN_LAVA_SINK,
    GOLDEN_LEVITATION, GOLDEN_SLAB_STEP, GOLDEN_SLIME_BOUNCE, GOLDEN_SLIME_BOUNCE_SNEAK,
    GOLDEN_SLOW_FALLING_WATER, GOLDEN_SNEAK_EDGE_DIAGONAL, GOLDEN_SNEAK_EDGE_STOP,
    GOLDEN_SNEAK_EDGE_WALK_OFF, GOLDEN_SOUL_SAND_WALK, GOLDEN_SPRINT_JUMP,
    GOLDEN_STAND_LOW_CORRIDOR_CONTROL, GOLDEN_SWIM_GAP_BLOCKED_CONTROL, GOLDEN_SWIM_GAP_TUNNEL,
    GOLDEN_SWIM_LOOK_DOWN_DIVES, GOLDEN_SWIM_SPRINT, GOLDEN_SWIM_SURFACE_LOOK_DOWN_CONTROL,
    GOLDEN_SWIM_SURFACE_LOOK_UP_NO_PULLDOWN, GOLDEN_WALK_FLAT, GOLDEN_WALK_INTO_WALL,
    GOLDEN_WATER_CURRENT_PUSH, GOLDEN_WATER_SINK, GoldenTick,
};

#[derive(Default)]
struct World {
    solid: HashSet<(i32, i32, i32)>,
    boxes: HashMap<(i32, i32, i32), Vec<Aabb>>,
    friction: HashMap<(i32, i32, i32), f32>,
    water: HashSet<(i32, i32, i32)>,
    climbable: HashSet<(i32, i32, i32)>,
    lava: HashSet<(i32, i32, i32)>,
    slime: HashSet<(i32, i32, i32)>,
    jump_factor: HashMap<(i32, i32, i32), f32>,
    speed_factor: HashMap<(i32, i32, i32), f32>,
    fluids: HashMap<(i32, i32, i32), FluidCell>,
}

impl World {
    fn solid(&mut self, x: i32, y: i32, z: i32) {
        self.solid.insert((x, y, z));
    }
    fn boxed(&mut self, x: i32, y: i32, z: i32, local: Aabb) {
        let world = Aabb::new(
            local.min_x + f64::from(x),
            local.min_y + f64::from(y),
            local.min_z + f64::from(z),
            local.max_x + f64::from(x),
            local.max_y + f64::from(y),
            local.max_z + f64::from(z),
        );
        self.boxes.entry((x, y, z)).or_default().push(world);
    }
    fn set_friction(&mut self, x: i32, y: i32, z: i32, f: f32) {
        self.friction.insert((x, y, z), f);
    }
    fn add_water(&mut self, x: i32, y: i32, z: i32) {
        self.water.insert((x, y, z));
    }
    fn add_water_cell(&mut self, x: i32, y: i32, z: i32, amount: u8) {
        self.water.insert((x, y, z));
        self.fluids.insert(
            (x, y, z),
            FluidCell {
                kind: FluidKind::Water,
                amount,
                falling: false,
            },
        );
    }
    fn add_climbable(&mut self, x: i32, y: i32, z: i32) {
        self.climbable.insert((x, y, z));
    }
    fn add_lava(&mut self, x: i32, y: i32, z: i32) {
        self.lava.insert((x, y, z));
    }
    fn add_slime(&mut self, x: i32, y: i32, z: i32) {
        self.slime.insert((x, y, z));
    }
    fn set_jump_factor(&mut self, x: i32, y: i32, z: i32, f: f32) {
        self.jump_factor.insert((x, y, z), f);
    }
    fn set_speed_factor(&mut self, x: i32, y: i32, z: i32, f: f32) {
        self.speed_factor.insert((x, y, z), f);
    }
    fn flat_floor(r: i32) -> Self {
        let mut w = World::default();
        for x in -r..=r {
            for z in -r..=r {
                w.solid(x, 0, z);
            }
        }
        w
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if let Some(bs) = self.boxes.get(&(x, y, z)) {
            out.extend_from_slice(bs);
        } else if self.solid.contains(&(x, y, z)) {
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
    fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
        *self.friction.get(&(x, y, z)).unwrap_or(&0.6)
    }
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
    fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
        self.climbable.contains(&(x, y, z))
    }
    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        self.lava.contains(&(x, y, z))
    }
    fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
        if self.slime.contains(&(x, y, z)) {
            1.0
        } else {
            0.0
        }
    }
    fn jump_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        *self.jump_factor.get(&(x, y, z)).unwrap_or(&1.0)
    }
    fn speed_factor(&self, x: i32, y: i32, z: i32) -> f32 {
        *self.speed_factor.get(&(x, y, z)).unwrap_or(&1.0)
    }
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        self.fluids.get(&(x, y, z)).copied()
    }
    fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
        self.solid.contains(&(x, y, z))
    }
}

/// Replays `ticks` through the crate and asserts each position/velocity matches
/// the golden trace bit-for-bit.
fn assert_trace(
    name: &str,
    world: &World,
    mut state: PlayerState,
    golden: &[GoldenTick],
    input: impl Fn(usize) -> MovementInput,
) {
    let profile = PhysicsProfile::mc_1_21();
    for (tick, expected) in golden.iter().enumerate() {
        tick_air(&mut state, input(tick), world, &profile);
        check(name, tick, "pos.x", state.position.x, expected.pos[0]);
        check(name, tick, "pos.y", state.position.y, expected.pos[1]);
        check(name, tick, "pos.z", state.position.z, expected.pos[2]);
        check(name, tick, "vel.x", state.velocity.x, expected.vel[0]);
        check(name, tick, "vel.y", state.velocity.y, expected.vel[1]);
        check(name, tick, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

fn check(name: &str, tick: usize, field: &str, actual: f64, expected_bits: u64) {
    let expected = f64::from_bits(expected_bits);
    assert_eq!(
        actual.to_bits(),
        expected_bits,
        "{name} tick {tick} {field}: got {actual} ({:#018x}) expected {expected} ({expected_bits:#018x})",
        actual.to_bits(),
    );
}

fn grounded(x: f64, y: f64, z: f64) -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(x, y, z), 0.0);
    s.on_ground = true;
    s
}

#[test]
fn free_fall_matches_golden() {
    let world = World::default();
    let state = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
    assert_trace("free_fall", &world, state, &GOLDEN_FREE_FALL, |_| {
        MovementInput::NONE
    });
}

#[test]
fn walk_flat_matches_golden() {
    let world = World::flat_floor(4);
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace("walk_flat", &world, state, &GOLDEN_WALK_FLAT, |_| {
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        }
    });
}

#[test]
fn sprint_jump_matches_golden() {
    let world = World::flat_floor(4);
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace("sprint_jump", &world, state, &GOLDEN_SPRINT_JUMP, |_| {
        MovementInput {
            forward: 1.0,
            jump: true,
            sprint: true,
            ..MovementInput::NONE
        }
    });
}

#[test]
fn ice_slide_matches_golden() {
    let mut world = World::flat_floor(4);
    for x in -4..=4 {
        for z in -4..=4 {
            world.set_friction(x, 0, z, 0.98);
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace("ice_slide", &world, state, &GOLDEN_ICE_SLIDE, |t| {
        MovementInput {
            forward: if t < 40 { 1.0 } else { 0.0 },
            ..MovementInput::NONE
        }
    });
}

#[test]
fn walk_into_wall_matches_golden() {
    let mut world = World::flat_floor(4);
    for y in 1..3 {
        for z in -2..=2 {
            world.solid(1, y, z);
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace(
        "walk_into_wall",
        &world,
        state,
        &GOLDEN_WALK_INTO_WALL,
        |_| MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );
}

#[test]
fn slab_step_matches_golden() {
    let mut world = World::flat_floor(6);
    for x in [1, 2, 3] {
        for z in -2..=2 {
            world.boxed(x, 1, z, Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0));
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace("slab_step", &world, state, &GOLDEN_SLAB_STEP, |_| {
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        }
    });
}

#[test]
fn diagonal_walk_matches_golden() {
    let world = World::flat_floor(8);
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace(
        "diagonal_walk",
        &world,
        state,
        &GOLDEN_DIAGONAL_WALK,
        |_| MovementInput {
            forward: 1.0,
            strafe: 1.0,
            ..MovementInput::NONE
        },
    );
}

#[test]
fn analog_strafe_matches_golden() {
    // Asymmetric analog input exercises modifyInput's two-term length; the
    // golden is validated bit-for-bit against the real JVM (oracle-java).
    let world = World::flat_floor(8);
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace(
        "analog_strafe",
        &world,
        state,
        &GOLDEN_ANALOG_STRAFE,
        |_| MovementInput {
            forward: 0.5,
            strafe: 1.0,
            ..MovementInput::NONE
        },
    );
}

#[test]
fn ladder_climb_matches_golden() {
    let mut world = World::default();
    for y in 0..16 {
        world.add_climbable(0, y, 0);
    }
    let state = PlayerState::at(Vec3d::new(0.5, 2.0, 0.5), 0.0);
    assert_trace("ladder_climb", &world, state, &GOLDEN_LADDER_CLIMB, |_| {
        MovementInput {
            jump: true,
            ..MovementInput::NONE
        }
    });
}

#[test]
fn ladder_sneak_hold_matches_golden() {
    let mut world = World::default();
    for y in 0..16 {
        world.add_climbable(0, y, 0);
    }
    let mut state = PlayerState::at(Vec3d::new(0.5, 8.0, 0.5), 0.0);
    state.velocity = Vec3d::new(0.0, -0.2, 0.0);
    assert_trace(
        "ladder_sneak_hold",
        &world,
        state,
        &GOLDEN_LADDER_SNEAK_HOLD,
        |_| MovementInput {
            sneak: true,
            ..MovementInput::NONE
        },
    );
}

#[test]
fn blue_ice_slide_matches_golden() {
    let mut world = World::flat_floor(4);
    for x in -4..=4 {
        for z in -4..=4 {
            world.set_friction(x, 0, z, 0.989);
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_trace(
        "blue_ice_slide",
        &world,
        state,
        &GOLDEN_BLUE_ICE_SLIDE,
        |t| MovementInput {
            forward: if t < 40 { 1.0 } else { 0.0 },
            ..MovementInput::NONE
        },
    );
}

#[test]
fn water_sink_matches_golden() {
    // Deep water column with a distant floor; player starts submerged and sinks.
    // Replayed through the `tick` dispatcher so the in-water branch is exercised.
    let mut world = World::default();
    for y in 80..=100 {
        world.add_water(0, y, 0);
    }
    world.solid(0, 78, 0);
    let mut state = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
    let profile = PhysicsProfile::mc_1_21();
    for (t, expected) in GOLDEN_WATER_SINK.iter().enumerate() {
        tick(&mut state, MovementInput::NONE, &world, &profile);
        check("water_sink", t, "pos.x", state.position.x, expected.pos[0]);
        check("water_sink", t, "pos.y", state.position.y, expected.pos[1]);
        check("water_sink", t, "pos.z", state.position.z, expected.pos[2]);
        check("water_sink", t, "vel.x", state.velocity.x, expected.vel[0]);
        check("water_sink", t, "vel.y", state.velocity.y, expected.vel[1]);
        check("water_sink", t, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

/// Replays a `tick`-dispatched scenario (fluid/effects) and asserts bit-exact.
fn assert_tick_trace(
    name: &str,
    world: &World,
    mut state: PlayerState,
    golden: &[GoldenTick],
    input: MovementInput,
) {
    let profile = PhysicsProfile::mc_1_21();
    for (t, expected) in golden.iter().enumerate() {
        tick(&mut state, input, world, &profile);
        check(name, t, "pos.x", state.position.x, expected.pos[0]);
        check(name, t, "pos.y", state.position.y, expected.pos[1]);
        check(name, t, "pos.z", state.position.z, expected.pos[2]);
        check(name, t, "vel.x", state.velocity.x, expected.vel[0]);
        check(name, t, "vel.y", state.velocity.y, expected.vel[1]);
        check(name, t, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

#[test]
fn lava_sink_matches_golden() {
    // Deep lava column: exercises the `tick_lava` branch (scale 0.5, -g/4), which
    // is structurally different from water — not a scaled water path.
    let mut world = World::default();
    for y in 80..=100 {
        world.add_lava(0, y, 0);
    }
    world.solid(0, 78, 0);
    let state = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
    assert_tick_trace(
        "lava_sink",
        &world,
        state,
        &GOLDEN_LAVA_SINK,
        MovementInput::NONE,
    );
}

#[test]
fn levitation_matches_golden() {
    // Levitation replaces gravity in the air path with a pull toward 0.05*(amp+1),
    // so the player rises instead of falling.
    let world = World::default();
    let state = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0).with_effects(StatusEffects {
        levitation: Some(0),
        ..StatusEffects::default()
    });
    assert_tick_trace(
        "levitation",
        &world,
        state,
        &GOLDEN_LEVITATION,
        MovementInput::NONE,
    );
}

#[test]
fn slow_falling_water_matches_golden() {
    // Slow Falling reduces effective gravity to 0.01, shifting baseGravity/16 off
    // 0.005 and making the otherwise-dead `-0.003` fluid clamp fire. This is the
    // "dead branch comes alive" test.
    let mut world = World::default();
    for y in 80..=100 {
        world.add_water(0, y, 0);
    }
    world.solid(0, 78, 0);
    let state = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0).with_effects(StatusEffects {
        slow_falling: true,
        ..StatusEffects::default()
    });
    assert_tick_trace(
        "slow_falling_water",
        &world,
        state,
        &GOLDEN_SLOW_FALLING_WATER,
        MovementInput::NONE,
    );
    // Guard the intent: at least one tick must actually hit the clamp.
    let fired = GOLDEN_SLOW_FALLING_WATER
        .iter()
        .any(|t| f64::from_bits(t.vel[1]) == -0.003);
    assert!(fired, "slow_falling_water never exercised the -0.003 clamp");
}

#[test]
fn swim_sprint_matches_golden() {
    // Sprint-swimming with forward input: the swimming branch of travelInWater
    // (slowDown = 0.9F), which the vertical-only water_sink never exercised.
    let mut world = World::default();
    for y in 80..=100 {
        for x in -2..=2 {
            for z in -2..=2 {
                world.add_water(x, y, z);
            }
        }
    }
    let state = PlayerState::at(Vec3d::new(0.5, 90.0, 0.5), 0.0);
    assert_tick_trace(
        "swim_sprint",
        &world,
        state,
        &GOLDEN_SWIM_SPRINT,
        MovementInput {
            forward: 1.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: true,
        },
    );
}

#[test]
fn swim_look_down_dives_matches_golden() {
    // Identical fixture and input to `swim_sprint_matches_golden`, but pitch =
    // 60 (looking steeply down: lookAngleY = -sin(60 deg) ~= -0.866, past the
    // -0.2 threshold, so the steeper 0.085 multiplier applies, `Player.java:
    // 1408`). Issue #59: looking down while swimming did not make the player
    // descend, because `Player.travel`'s look-descent term
    // (`Player.java:1401-1415`) was never ported into `tick_water`. This test
    // is the regression signal for that fix.
    let mut world = World::default();
    for y in 80..=100 {
        for x in -2..=2 {
            for z in -2..=2 {
                world.add_water(x, y, z);
            }
        }
    }
    let mut state = PlayerState::at(Vec3d::new(0.5, 90.0, 0.5), 0.0);
    state.pitch = 60.0;
    assert_tick_trace(
        "swim_look_down_dives",
        &world,
        state,
        &GOLDEN_SWIM_LOOK_DOWN_DIVES,
        MovementInput {
            forward: 1.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: true,
        },
    );
    // Tick 0 is still STANDING in both traces (`updateSwimming` reads the
    // *previous* tick's `isSprinting`, so the swim pose cannot appear before
    // tick 2's dispatch, i.e. golden index 1) -- so the first tick where the
    // pitch-0 control (`GOLDEN_SWIM_SPRINT`) and this one could possibly
    // differ is index 1, and that is exactly where they must.
    assert_eq!(
        f64::from_bits(GOLDEN_SWIM_SPRINT[0].vel[1]),
        0.0,
        "control: no vertical motion before the swim pose exists"
    );
    assert_eq!(
        f64::from_bits(GOLDEN_SWIM_LOOK_DOWN_DIVES[0].vel[1]),
        0.0,
        "looking down changes nothing before the swim pose exists either"
    );
    assert_eq!(
        f64::from_bits(GOLDEN_SWIM_SPRINT[1].vel[1]),
        0.0,
        "control: pitch 0 is the blend's own fixed point (lookAngleY == vy == 0), so sprint- \
         swimming straight ahead never drifts vertically at all"
    );
    let looked_down_vy = f64::from_bits(GOLDEN_SWIM_LOOK_DOWN_DIVES[1].vel[1]);
    assert!(
        looked_down_vy < -0.01,
        "looking down must pull the swimmer down from the first swimming tick, got {looked_down_vy}"
    );
}

#[test]
fn swim_surface_look_up_no_pulldown_matches_golden() {
    // At the surface (a 10-block pool, world y 80.0..90.0): the swim box
    // (feet 89.5, height 0.6) sits inside the topmost water block, but
    // `BlockPos.containing(x, y + 1.0 - 0.1, z)` = floor(89.5 + 0.9) = 90,
    // which is air -- so `headSubmerged` is false. Looking up (pitch = -60,
    // lookAngleY ~= +0.866) makes every term of `lookAngleY <= 0.0 ||
    // jumping || headSubmerged` false, so the descent term never fires. Since
    // sprint-swimming also suppresses buoyancy
    // (`getFluidFallingAdjustedMovement`'s `!sprinting` guard) and there is no
    // vertical input, nothing moves the player vertically at all: a flat line
    // at y = 89.5, vy = 0.0, for all 30 ticks.
    //
    // The pose is seeded directly (swimming + sprinting both `true` from tick
    // 0), exactly like `elytra_gap_glide_matches_golden` seeds `FallFlying`:
    // naturally *entering* swimming here would need the eye submerged under
    // the STANDING 1.62 eye height, which this 10-block-deep pool cannot
    // provide, and `updateSwimming` reads the *previous* tick's `sprinting`,
    // so that must be seeded too or tick 1 would immediately undo the pose.
    let mut world = World::default();
    for y in 80..90 {
        for x in -2..=2 {
            for z in -2..=2 {
                world.add_water(x, y, z);
            }
        }
    }
    world.solid(0, 40, 0);
    let mut state = PlayerState::at(Vec3d::new(0.5, 89.5, 0.5), 0.0).with_pose(Pose::Swimming);
    state.pitch = -60.0;
    state.swimming = true;
    state.sprinting = true;
    assert_tick_trace(
        "swim_surface_look_up_no_pulldown",
        &world,
        state,
        &GOLDEN_SWIM_SURFACE_LOOK_UP_NO_PULLDOWN,
        MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: true,
        },
    );
    for (t, tick) in GOLDEN_SWIM_SURFACE_LOOK_UP_NO_PULLDOWN.iter().enumerate() {
        assert_eq!(
            f64::from_bits(tick.pos[1]),
            89.5,
            "tick {t}: no vertical drift while looking up"
        );
        assert_eq!(
            f64::from_bits(tick.vel[1]),
            0.0,
            "tick {t}: vy stays exactly zero"
        );
    }
}

#[test]
fn swim_surface_look_down_control_matches_golden() {
    // WORLD CONTROL for `swim_surface_look_up_no_pulldown_matches_golden`:
    // identical fixture and seed, pitch flipped to +60 so `lookAngleY <= 0.0`
    // is true and the descent term fires every tick with the steep 0.085
    // multiplier. If this trace matched the look-up one, the gate that test
    // exercises would not be doing anything -- proving it is the look angle,
    // and not some other difference between the two setups, driving the flat
    // line there.
    let mut world = World::default();
    for y in 80..90 {
        for x in -2..=2 {
            for z in -2..=2 {
                world.add_water(x, y, z);
            }
        }
    }
    world.solid(0, 40, 0);
    let mut state = PlayerState::at(Vec3d::new(0.5, 89.5, 0.5), 0.0).with_pose(Pose::Swimming);
    state.pitch = 60.0;
    state.swimming = true;
    state.sprinting = true;
    assert_tick_trace(
        "swim_surface_look_down_control",
        &world,
        state,
        &GOLDEN_SWIM_SURFACE_LOOK_DOWN_CONTROL,
        MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: true,
        },
    );
    let final_y = f64::from_bits(
        GOLDEN_SWIM_SURFACE_LOOK_DOWN_CONTROL
            .last()
            .unwrap()
            .pos[1],
    );
    assert!(
        final_y < 89.5 - 5.0,
        "looking down at the surface must pull the swimmer down noticeably: y = {final_y}"
    );
}

#[test]
fn water_current_push_matches_golden() {
    // Fluid flow-current push (`Entity.updateFluidInteraction` /
    // `FlowingFluid.getFlow`). The player is submerged in a horizontal water
    // gradient — source columns (amount 8) at x<=0, shallower flowing water
    // (amount 5) at x>0 — producing a steady eastward current. Sustained over
    // 100 ticks so a regression in the accumulation drifts visibly, matching the
    // second Python oracle bit-for-bit.
    let mut world = World::default();
    for x in -2..=11 {
        for z in -1..=1 {
            world.solid(x, 0, z);
            for y in [1, 2] {
                world.add_water_cell(x, y, z, if x <= 0 { 8 } else { 5 });
            }
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_tick_trace(
        "water_current_push",
        &world,
        state,
        &GOLDEN_WATER_CURRENT_PUSH,
        MovementInput::NONE,
    );
    // Guard the intent: the current must actually push the player east. Without
    // this a silently-zero push (e.g. `fluid_at` returning `None`) would pass.
    let last = GOLDEN_WATER_CURRENT_PUSH.last().unwrap();
    let final_x = f64::from_bits(last.pos[0]);
    assert!(
        final_x > 1.0,
        "expected sustained eastward push (final x > 1.0), got {final_x}"
    );
    let pushed = GOLDEN_WATER_CURRENT_PUSH
        .iter()
        .any(|t| f64::from_bits(t.vel[0]) > 0.0);
    assert!(pushed, "water_current_push never produced a +X velocity");
}

#[test]
fn soul_sand_walk_matches_golden() {
    // Full collision cube carrying block speed factor 0.4. The player rests at
    // y=1.0 so blockPosition() is air (1.0) and getBlockSpeedFactor falls through
    // to the block below — the here==1.0 fallback branch no other scenario hits.
    let mut world = World::flat_floor(8);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_speed_factor(x, 0, z, 0.4);
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_tick_trace(
        "soul_sand_walk",
        &world,
        state,
        &GOLDEN_SOUL_SAND_WALK,
        MovementInput {
            forward: 1.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: false,
        },
    );
}

#[test]
fn jump_boost_matches_golden() {
    // Jump Boost II (amplifier 1): getJumpBoostPower() = 0.1F*(1+1) = 0.2F added
    // to the jump velocity as a separate float term, not a MOVEMENT_SPEED modifier.
    let world = World::flat_floor(4);
    let state = grounded(0.5, 1.0, 0.5).with_effects(StatusEffects {
        jump_boost: Some(1),
        ..StatusEffects::default()
    });
    assert_tick_trace(
        "jump_boost",
        &world,
        state,
        &GOLDEN_JUMP_BOOST,
        MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: true,
            sneak: false,
            sprint: false,
        },
    );
}

#[test]
fn honey_jump_matches_golden() {
    // Block jump factor 0.5 scales jump velocity to 0.42*0.5 = 0.21F. Verifies the
    // block-jump-factor term in getJumpPower reduces jump height (honey behaviour).
    let mut world = World::flat_floor(4);
    for x in -4..=4 {
        for z in -4..=4 {
            world.set_jump_factor(x, 0, z, 0.5);
        }
    }
    let state = grounded(0.5, 1.0, 0.5);
    assert_tick_trace(
        "honey_jump",
        &world,
        state,
        &GOLDEN_HONEY_JUMP,
        MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: true,
            sneak: false,
            sprint: false,
        },
    );
    // Guard the intent: the honey jump must be lower than an un-reduced jump.
    let first_vy = f64::from_bits(GOLDEN_HONEY_JUMP[0].vel[1]);
    let boosted_first_vy = f64::from_bits(GOLDEN_JUMP_BOOST[0].vel[1]);
    assert!(
        first_vy < boosted_first_vy,
        "honey jump should be lower than a boosted jump"
    );
}

#[test]
fn slime_bounce_matches_golden() {
    // Free-fall onto slime (bounce_restitution 1.0): restituteMovementAfterCollisions
    // reverses vy through the block-bounciness branch instead of zeroing it.
    let mut world = World::flat_floor(4);
    for x in -4..=4 {
        for z in -4..=4 {
            world.add_slime(x, 0, z);
        }
    }
    let state = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
    assert_tick_trace(
        "slime_bounce",
        &world,
        state,
        &GOLDEN_SLIME_BOUNCE,
        MovementInput::NONE,
    );
    // Guard the intent: some tick must reverse from a fast descent to an ascent.
    let bounced = GOLDEN_SLIME_BOUNCE
        .windows(2)
        .any(|w| f64::from_bits(w[0].vel[1]) < -0.1 && f64::from_bits(w[1].vel[1]) > 0.05);
    assert!(bounced, "slime_bounce never reversed velocity");
}

#[test]
fn slime_bounce_sneak_matches_golden() {
    // Holding sneak (isSuppressingBounce) vetoes the block-bounce branch, so the
    // player lands and rests (vy -> 0 path) instead of bouncing.
    let mut world = World::flat_floor(4);
    for x in -4..=4 {
        for z in -4..=4 {
            world.add_slime(x, 0, z);
        }
    }
    let state = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
    assert_tick_trace(
        "slime_bounce_sneak",
        &world,
        state,
        &GOLDEN_SLIME_BOUNCE_SNEAK,
        MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: true,
            sprint: false,
        },
    );
    // Guard the intent: sneaking must never produce an upward bounce.
    let bounced = GOLDEN_SLIME_BOUNCE_SNEAK
        .iter()
        .any(|t| f64::from_bits(t.vel[1]) > 0.05);
    assert!(!bounced, "sneak failed to cancel the slime bounce");
}

/// Replays an elytra-glide scenario through [`tick_elytra`] and asserts each
/// tick matches the golden trace bit-for-bit. `input` is ignored by the glide
/// path but passed through for the climbable-fallback branch, mirroring vanilla.
fn assert_elytra_trace(name: &str, world: &World, mut state: PlayerState, golden: &[GoldenTick]) {
    let profile = PhysicsProfile::mc_1_21();
    for (t, expected) in golden.iter().enumerate() {
        tick_elytra(&mut state, MovementInput::NONE, world, &profile);
        check(name, t, "pos.x", state.position.x, expected.pos[0]);
        check(name, t, "pos.y", state.position.y, expected.pos[1]);
        check(name, t, "pos.z", state.position.z, expected.pos[2]);
        check(name, t, "vel.x", state.velocity.x, expected.vel[0]);
        check(name, t, "vel.y", state.velocity.y, expected.vel[1]);
        check(name, t, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

fn elytra_state(y: f64, yaw: f32, pitch: f32, vel: Vec3d) -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(0.5, y, 0.5), yaw);
    s.pitch = pitch;
    s.fall_flying = true;
    s.velocity = vel;
    s
}

#[test]
fn elytra_glide_level_matches_golden() {
    // Pitch 0 / yaw 0: level launch with forward speed. liftForce = 1, so the
    // vertical term is gravity*(-1 + 0.75) and the player sinks gently while the
    // steer term realigns horizontal speed onto +Z.
    let world = World::default();
    let state = elytra_state(100.0, 0.0, 0.0, Vec3d::new(0.0, 0.0, 1.0));
    assert_elytra_trace(
        "elytra_glide_level",
        &world,
        state,
        &GOLDEN_ELYTRA_GLIDE_LEVEL,
    );
    // Anti-vacuity: the glide must actually travel forward and lose altitude,
    // and never gain net height on a level launch.
    let first = &GOLDEN_ELYTRA_GLIDE_LEVEL[0];
    let last = &GOLDEN_ELYTRA_GLIDE_LEVEL[GOLDEN_ELYTRA_GLIDE_LEVEL.len() - 1];
    assert!(
        f64::from_bits(last.pos[2]) - f64::from_bits(first.pos[2]) > 5.0,
        "elytra glide did not travel forward"
    );
    assert!(
        f64::from_bits(last.pos[1]) < f64::from_bits(first.pos[1]) - 5.0,
        "elytra glide did not descend"
    );
}

#[test]
fn elytra_dive_matches_golden() {
    // Pitch +37 deg (nose-down, off-grid angle). Exercises the `my < 0` convert
    // branch that trades altitude for horizontal speed, and forces the LUT-cos
    // vs Math.cos difference to matter (a nice angle could hide it).
    let world = World::default();
    let state = elytra_state(200.0, 0.0, 37.0, Vec3d::new(0.0, 0.0, 0.8));
    assert_elytra_trace("elytra_dive", &world, state, &GOLDEN_ELYTRA_DIVE);
    // Anti-vacuity: a dive must build horizontal speed well past the launch 0.8.
    let peak_vz = GOLDEN_ELYTRA_DIVE
        .iter()
        .map(|t| f64::from_bits(t.vel[2]))
        .fold(f64::MIN, f64::max);
    assert!(peak_vz > 1.2, "dive never accelerated (peak vz {peak_vz})");
}

#[test]
fn elytra_climb_matches_golden() {
    // Pitch -23 deg (nose-up): exercises the `leanAngle < 0` branch, where
    // -Mth.sin(leanAngle) > 0 adds convert*3.2 lift plus a backward horizontal
    // term — the pump-up-then-stall arc.
    let world = World::default();
    let state = elytra_state(100.0, 0.0, -23.0, Vec3d::new(0.0, 0.0, 1.4));
    assert_elytra_trace("elytra_climb", &world, state, &GOLDEN_ELYTRA_CLIMB);
    // Anti-vacuity: the nose-up lift must produce at least one rising tick.
    let rose = GOLDEN_ELYTRA_CLIMB
        .iter()
        .any(|t| f64::from_bits(t.vel[1]) > 0.0);
    assert!(rose, "nose-up elytra never gained upward velocity");
}

#[test]
fn elytra_diagonal_yaw_matches_golden() {
    // Yaw 33 deg / pitch +11 deg: look.x and look.z both nonzero and unequal, so
    // the steer term redistributes speed onto both axes with different per-axis
    // rounding — an asymmetric case a pure +Z glide cannot reach.
    let world = World::default();
    let state = elytra_state(150.0, 33.0, 11.0, Vec3d::new(0.3, 0.0, 0.9));
    assert_elytra_trace(
        "elytra_diagonal_yaw",
        &world,
        state,
        &GOLDEN_ELYTRA_DIAGONAL_YAW,
    );
    // Anti-vacuity: motion must develop on BOTH horizontal axes (a +Z-only
    // formulation would leave x ~unchanged), and they must differ in magnitude.
    // Yaw 33 deg steers toward -X/+Z, so check magnitudes rather than sign.
    let last = &GOLDEN_ELYTRA_DIAGONAL_YAW[GOLDEN_ELYTRA_DIAGONAL_YAW.len() - 1];
    let dx = (f64::from_bits(last.pos[0]) - 0.5).abs();
    let dz = (f64::from_bits(last.pos[2]) - 0.5).abs();
    assert!(
        dx > 1.0 && dz > 1.0,
        "diagonal glide missing an axis (dx {dx}, dz {dz})"
    );
    assert!((dx - dz).abs() > 1.0, "diagonal glide was not asymmetric");
}

// ---------------------------------------------------------------------------
// `Player.maybeBackOffFromEdge` — the sneak-at-a-ledge back-off.
//
// The three traces below come from the same independent Python oracle as every
// other trace in this file, and are replayed bit-for-bit. See
// `docs/edge-back-off.md` for the rule and `tests/edge_back_off.rs` for the
// pure-rule control (the same delta run with the rule on and off).
// ---------------------------------------------------------------------------

/// A floor whose eastern edge is the `x = 1` plane: solid for `x <= 0` only.
fn ledge_at_x1(r: i32) -> World {
    let mut world = World::default();
    for x in -r..=0 {
        for z in -r..=r {
            world.solid(x, 0, z);
        }
    }
    world
}

#[test]
fn sneak_edge_stop_matches_golden() {
    let world = ledge_at_x1(6);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_trace(
        "sneak_edge_stop",
        &world,
        state,
        &GOLDEN_SNEAK_EDGE_STOP,
        |_| MovementInput {
            forward: 1.0,
            sneak: true,
            ..MovementInput::NONE
        },
    );

    // Anti-vacuity, and the assertion the rule exists to make: the player must
    // never leave y = 1.0. A single tick of descent means the back-off failed.
    for (t, tick) in GOLDEN_SNEAK_EDGE_STOP.iter().enumerate() {
        let y = f64::from_bits(tick.pos[1]);
        assert_eq!(y, 1.0, "sneak_edge_stop fell at tick {t} (y = {y})");
    }
    // ... and they must actually have walked *up to* the edge, not stalled at the
    // start. Support ends at x = 1.0 and the box half-width is 0.3, so the last
    // standable centre is just under x = 1.3.
    let last_x = f64::from_bits(GOLDEN_SNEAK_EDGE_STOP[GOLDEN_SNEAK_EDGE_STOP.len() - 1].pos[0]);
    assert!(
        last_x > 1.2 && last_x < 1.3,
        "sneak_edge_stop did not reach the ledge (x = {last_x})"
    );
}

#[test]
fn sneak_edge_walk_off_is_the_world_control() {
    // WORLD CONTROL. Same fixture, same inputs, shift released. This is the
    // negative control that makes the test above non-vacuous: it proves the
    // fixture really does have an edge you fall off, so `sneak_edge_stop`'s
    // "y never changes" is a consequence of the rule and not of the geometry.
    let world = ledge_at_x1(6);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_trace(
        "sneak_edge_walk_off",
        &world,
        state,
        &GOLDEN_SNEAK_EDGE_WALK_OFF,
        |_| MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );

    let last = &GOLDEN_SNEAK_EDGE_WALK_OFF[GOLDEN_SNEAK_EDGE_WALK_OFF.len() - 1];
    let y = f64::from_bits(last.pos[1]);
    let x = f64::from_bits(last.pos[0]);
    assert!(
        y < 1.0 && x > 1.3,
        "control never left the ledge (x = {x}, y = {y}) — the fixture has no edge \
         and `sneak_edge_stop` proves nothing"
    );
}

#[test]
fn sneak_edge_diagonal_matches_golden() {
    // An outside corner: the floor is missing wherever x >= 1 AND z >= 1, so
    // neither the pure-X nor the pure-Z probe clears the support but the joint one
    // does. This is the only scenario that enters the third loop.
    let mut world = World::default();
    for x in -6..=6 {
        for z in -6..=6 {
            if x >= 1 && z >= 1 {
                continue;
            }
            world.solid(x, 0, z);
        }
    }
    let mut state = grounded_facing(0.5, 1.0, 0.5, 0.0);
    state.velocity = Vec3d::new(0.8, 0.0, 0.8);
    assert_trace(
        "sneak_edge_diagonal",
        &world,
        state,
        &GOLDEN_SNEAK_EDGE_DIAGONAL,
        |_| MovementInput {
            sneak: true,
            ..MovementInput::NONE
        },
    );
}

/// A grounded state facing `yaw` (yaw `-90` faces `+X`).
fn grounded_facing(x: f64, y: f64, z: f64, yaw: f32) -> PlayerState {
    let mut s = PlayerState::at(Vec3d::new(x, y, z), yaw);
    s.on_ground = true;
    s
}

// --- entity push -----------------------------------------------------------
//
// The only three traces in this file with a **second entity in the world**. Every
// other one runs against an empty neighbour slice, which is why they are all
// byte-identical across this change: `tick_among_entities` with an empty slice is
// `tick`, `apply_entity_push` returns before reading anything, and
// `collide_among_entities` with an empty collider slice prepends nothing to the
// block colliders. Regenerating `golden_traces.rs` after adding these produced
// 1612 insertions and zero deletions.

/// The neighbour construction the Python oracle's `Neighbour` uses: box centred on
/// `(x, z)` horizontally, feet at `y`, spanning `width` on both horizontal axes.
///
/// Note `width / 2.0` in `f64` — the *neighbour* box is built from a plain `f64`
/// width, whereas the *player* box comes from `EntityDimensions::PLAYER`'s `f32`
/// `0.6` widened (half-width `0.300000011920929`). The two are deliberately not
/// unified: the asymmetry is what made the first draft of the flush control
/// overlap by 1.2e-8 and push.
fn neighbour(x: f64, y: f64, z: f64, width: f64, height: f64) -> NearbyEntity {
    let half = width / 2.0;
    NearbyEntity::living(
        Vec3d::new(x, y, z),
        Aabb::new(x - half, y, z - half, x + half, y + height, z + half),
    )
}

/// Replays a trace through [`tick_among_entities`] against a fixed set of
/// stationary neighbours.
fn assert_push_trace(
    name: &str,
    world: &World,
    mut state: PlayerState,
    golden: &[GoldenTick],
    nearby: &[NearbyEntity],
) {
    let profile = PhysicsProfile::mc_1_21();
    for (t, expected) in golden.iter().enumerate() {
        tick_among_entities(
            &mut state,
            MovementInput::NONE,
            world,
            &profile,
            nearby,
            PushSelf::LIVING_PLAYER,
        );
        check(name, t, "pos.x", state.position.x, expected.pos[0]);
        check(name, t, "pos.y", state.position.y, expected.pos[1]);
        check(name, t, "pos.z", state.position.z, expected.pos[2]);
        check(name, t, "vel.x", state.velocity.x, expected.vel[0]);
        check(name, t, "vel.y", state.velocity.y, expected.vel[1]);
        check(name, t, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

#[test]
fn entity_push_shove_matches_golden() {
    // One stationary pushable neighbour overlapping the player, offset on both
    // horizontal axes (dx = 0.15, dz = 0.08) so the `sqrt(absMax)` normaliser is
    // observable: normalising by the vector length instead would put the first
    // tick's velocity ~6% off on both axes, not in the last bits.
    let world = World::flat_floor(4);
    let state = grounded(0.5, 1.0, 0.5);
    let nearby = [neighbour(0.65, 1.0, 0.58, 0.6, 1.8)];
    assert_push_trace(
        "entity_push_shove",
        &world,
        state,
        &GOLDEN_ENTITY_PUSH_SHOVE,
        &nearby,
    );

    // Guard the intent, the way `water_current_push` does: a silently-zero push
    // would otherwise agree with a stationary golden trace.
    let last = GOLDEN_ENTITY_PUSH_SHOVE.last().unwrap();
    let (fx, fz) = (f64::from_bits(last.pos[0]), f64::from_bits(last.pos[2]));
    assert!(
        fx < 0.4 && fz < 0.45,
        "expected to be shoved away from the neighbour (-x, -z), ended at ({fx}, {fz})"
    );
    // And the gate must *close* again: once the boxes separate the push stops and
    // friction brings the player to rest. A push that never cut out would leave a
    // non-zero horizontal velocity at the end.
    assert_eq!(
        f64::from_bits(last.vel[0]),
        0.0,
        "the push must stop once the boxes no longer overlap"
    );
    assert_eq!(f64::from_bits(last.vel[2]), 0.0);
}

#[test]
fn entity_push_wide_plateau_matches_golden() {
    // The un-clamped `pow = 1.0 / sqrt(absMax)` branch. Two 0.6-wide bodies can
    // never reach it — they stop overlapping at dx = 0.6 and every absMax below 1.0
    // has `pow` clamped to 1.0 — so this needs a wide neighbour (4.0 × 4.0, a happy
    // ghast) with dx = 1.05 and a deep overlap.
    let world = World::flat_floor(6);
    let state = grounded(1.0, 1.0, 1.0);
    let nearby = [neighbour(2.05, 1.0, 1.4, 4.0, 4.0)];
    assert_push_trace(
        "entity_push_wide_plateau",
        &world,
        state,
        &GOLDEN_ENTITY_PUSH_WIDE_PLATEAU,
        &nearby,
    );

    // The branch guard: assert the fixture actually reaches `pow < 1.0`, so this
    // test cannot silently degrade into a second copy of `entity_push_shove`.
    let dx: f64 = 2.05 - 1.0;
    assert!(
        1.0 / dx.sqrt() < 1.0,
        "fixture no longer reaches the un-clamped branch (absMax = {dx})"
    );
    // In this branch the magnitude is a flat `0.05f` on the dominant axis — no
    // distance falloff. Assert the first tick's X impulse against the widened
    // literal, which is the sharpest available statement of "the sqrt terms cancel".
    let first = &GOLDEN_ENTITY_PUSH_WIDE_PLATEAU[0];
    assert_eq!(
        f64::from_bits(first.vel[0]).to_bits(),
        (-f64::from(0.05_f32)).to_bits(),
        "the un-clamped branch must deliver exactly -0.05f on the dominant axis"
    );
    let last = GOLDEN_ENTITY_PUSH_WIDE_PLATEAU.last().unwrap();
    assert!(f64::from_bits(last.pos[0]) < 0.0, "must be shoved clear");
}

#[test]
fn entity_push_flush_control_is_inert() {
    // WORLD CONTROL for both traces above: identical fixture, the neighbour placed
    // so its -X face sits *exactly* on the player's +X face. `AABB.intersects` is
    // strict `min < max`, so a flush contact is not an overlap and no push happens.
    //
    // This is the control that proves the two positive traces measure the push
    // rather than something else in the fixture, and it is also the only assertion
    // in the file that would catch the push pair test acquiring the `1.0E-7`
    // inflation that belongs to `getEntityCollisions` alone.
    let world = World::flat_floor(4);
    let state = grounded(0.5, 1.0, 0.5);
    let flush_x = 0.5 + f64::from(0.6_f32) / 2.0 + 0.6 / 2.0;
    let nearby = [neighbour(flush_x, 1.0, 0.5, 0.6, 1.8)];
    assert_push_trace(
        "entity_push_flush_control",
        &world,
        state,
        &GOLDEN_ENTITY_PUSH_FLUSH_CONTROL,
        &nearby,
    );

    // The control's own assertion: nothing moved horizontally, ever.
    for (t, tick) in GOLDEN_ENTITY_PUSH_FLUSH_CONTROL.iter().enumerate() {
        assert_eq!(
            f64::from_bits(tick.pos[0]),
            0.5,
            "flush contact pushed on x at tick {t}"
        );
        assert_eq!(
            f64::from_bits(tick.pos[2]),
            0.5,
            "flush contact pushed on z at tick {t}"
        );
    }
    // …and the fixture is one ulp from being live, so "nothing moved" is not
    // because the neighbour was nowhere near. Moving it a single ulp closer makes
    // the boxes overlap and the push fire.
    let live_x = f64::from_bits(flush_x.to_bits() - 1);
    let live = [neighbour(live_x, 1.0, 0.5, 0.6, 1.8)];
    let impulse = lodestone_physics::entity_push_impulse(
        Vec3d::new(0.5, 1.0, 0.5),
        lodestone_physics::EntityDimensions::PLAYER.bounding_box(Vec3d::new(0.5, 1.0, 0.5)),
        PushSelf::LIVING_PLAYER,
        true,
        &live,
    );
    assert!(
        impulse.x < 0.0,
        "one ulp closer must push — otherwise the control is vacuous"
    );
}

// ---------------------------------------------------------------------------
// `Player.updatePlayerPose` — pose-dependent dimensions and the fit gate.
//
// Counted, not assumed (the generator was instrumented to record every pose it
// committed, per scenario). Of the 32 pre-existing traces: 19 never run the machine
// at all, because `tick_air`/`tick_water`/`tick_elytra` are `travel`, not
// `Player.tick`; 11 run it and only ever hold STANDING; and exactly two hold a
// smaller box — `slime_bounce_sneak` crouches on a flat slime floor with nothing
// above head height, and `swim_sprint` swims in a 5×5×21 water shaft with no solid
// blocks at all, so in both the shorter top face intersects the same empty set of
// cells. Regenerating `support/golden_traces.rs` after adding the machine and these
// six scenarios produced 2664 insertions and 0 deletions — and the control for that
// claim (regenerating the unmodified generator, which produced an empty diff) was
// run first.
//
// See `docs/pose-dimensions.md`.
// ---------------------------------------------------------------------------

/// An open pool (`x <= 0`, three deep) opening into a **one-block-high** water
/// tunnel (`x >= 1`): floor top `y = 1.0`, tunnel ceiling underside `y = 2.0`.
fn water_tunnel(x_end: i32, r: i32) -> World {
    let mut w = World::default();
    for x in -8..=x_end {
        for z in -r..=r {
            w.solid(x, 0, z);
            if x <= 0 {
                for y in 1..=3 {
                    w.add_water(x, y, z);
                }
            } else {
                w.add_water(x, 1, z);
                w.solid(x, 2, z);
            }
        }
    }
    w
}

/// A corridor with **1.5** blocks of headroom: a top slab (`2.5 ..= 3.0`) at
/// `y = 2` for `x >= 1`, over a floor whose top face is `y = 1.0`.
fn low_corridor(x_end: i32, r: i32) -> World {
    let mut w = World::default();
    for x in -8..=x_end {
        for z in -r..=r {
            w.solid(x, 0, z);
            if x >= 1 {
                w.boxed(x, 2, z, Aabb::new(0.0, 0.5, 0.0, 1.0, 1.0, 1.0));
            }
        }
    }
    w
}

/// [`assert_tick_trace`] with a per-tick input, for scenarios that change what
/// the player is holding partway through.
fn assert_tick_trace_inputs(
    name: &str,
    world: &World,
    mut state: PlayerState,
    golden: &[GoldenTick],
    input: impl Fn(usize) -> MovementInput,
) {
    let profile = PhysicsProfile::mc_1_21();
    for (t, expected) in golden.iter().enumerate() {
        tick(&mut state, input(t), world, &profile);
        check(name, t, "pos.x", state.position.x, expected.pos[0]);
        check(name, t, "pos.y", state.position.y, expected.pos[1]);
        check(name, t, "pos.z", state.position.z, expected.pos[2]);
        check(name, t, "vel.x", state.velocity.x, expected.vel[0]);
        check(name, t, "vel.y", state.velocity.y, expected.vel[1]);
        check(name, t, "vel.z", state.velocity.z, expected.vel[2]);
    }
}

/// The x a 0.6-wide box comes to rest at when its `+X` face is flush against the
/// `x = 1` plane. Derived, not written down: the half-width is `f32(0.6)/2 =
/// 0.300000011920929`, so the "obvious" `0.7` is wrong in the 8th place.
fn flush_against_x1() -> f64 {
    1.0 - f64::from(0.6_f32) / 2.0
}

#[test]
fn swim_gap_tunnel_matches_golden() {
    // The defect this work exists for: a sprint-swimmer must fit a one-block gap.
    let world = water_tunnel(30, 2);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_tick_trace(
        "swim_gap_tunnel",
        &world,
        state,
        &GOLDEN_SWIM_GAP_TUNNEL,
        MovementInput {
            forward: 1.0,
            sprint: true,
            ..MovementInput::NONE
        },
    );

    // The pose actually reached the collision sweep: the player is deep inside a
    // tunnel whose ceiling a 1.8-high box cannot pass, and never left y = 1.0.
    let last = GOLDEN_SWIM_GAP_TUNNEL.last().unwrap();
    let x = f64::from_bits(last.pos[0]);
    assert!(
        x > 10.0,
        "the swimmer did not get down the tunnel (x = {x})"
    );
    for (t, tk) in GOLDEN_SWIM_GAP_TUNNEL.iter().enumerate() {
        assert_eq!(
            f64::from_bits(tk.pos[1]),
            1.0,
            "swim_gap_tunnel left the tunnel floor at tick {t}"
        );
    }
    // …and the state machine is what did it: replay to the end and read the pose.
    let profile = PhysicsProfile::mc_1_21();
    let mut s = grounded_facing(0.5, 1.0, 0.5, -90.0);
    for _ in 0..GOLDEN_SWIM_GAP_TUNNEL.len() {
        tick(
            &mut s,
            MovementInput {
                forward: 1.0,
                sprint: true,
                ..MovementInput::NONE
            },
            &world,
            &profile,
        );
    }
    assert_eq!(s.pose, Pose::Swimming);
    assert_eq!(s.eye_height, 0.4, "the eye must follow the box");
    assert!(s.swimming);
}

#[test]
fn swim_gap_blocked_control_is_the_world_control() {
    // WORLD CONTROL for the trace above: identical fixture, sprint released, so
    // `updateSwimming` never fires, the pose stays STANDING and the 1.8-high box
    // jams on the tunnel ceiling. Without this, "the swimmer got through" could
    // just as well mean the fixture has no ceiling.
    let world = water_tunnel(30, 2);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_tick_trace(
        "swim_gap_blocked_control",
        &world,
        state,
        &GOLDEN_SWIM_GAP_BLOCKED_CONTROL,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );

    let last = GOLDEN_SWIM_GAP_BLOCKED_CONTROL.last().unwrap();
    assert_eq!(
        f64::from_bits(last.pos[0]).to_bits(),
        flush_against_x1().to_bits(),
        "the standing box must come to rest flush against the tunnel mouth"
    );
}

#[test]
fn crouch_low_corridor_matches_golden() {
    // Sneak-walking into 1.5 blocks of headroom. The crouch box's top is *exactly*
    // the top slab's underside, so this is also the flush case for the collision
    // sweep's `1.0E-7` perpendicular epsilon.
    let world = low_corridor(40, 2);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_tick_trace(
        "crouch_low_corridor",
        &world,
        state,
        &GOLDEN_CROUCH_LOW_CORRIDOR,
        MovementInput {
            forward: 1.0,
            sneak: true,
            ..MovementInput::NONE
        },
    );

    let x = f64::from_bits(GOLDEN_CROUCH_LOW_CORRIDOR.last().unwrap().pos[0]);
    assert!(x > 5.0, "the crouch never got into the corridor (x = {x})");
}

#[test]
fn stand_low_corridor_control_is_the_world_control() {
    // WORLD CONTROL for the trace above: identical fixture, shift released. The
    // desired pose is STANDING and it *fits* out here at x = 0.5, so the gate
    // grants it and the 1.8-high box then jams on the slab. This is what makes
    // "the crouch walked in" a statement about the box height.
    let world = low_corridor(40, 2);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    assert_tick_trace(
        "stand_low_corridor_control",
        &world,
        state,
        &GOLDEN_STAND_LOW_CORRIDOR_CONTROL,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );

    let last = GOLDEN_STAND_LOW_CORRIDOR_CONTROL.last().unwrap();
    assert_eq!(
        f64::from_bits(last.pos[0]).to_bits(),
        flush_against_x1().to_bits(),
        "the standing box must come to rest flush against the corridor mouth"
    );
}

#[test]
fn crouch_release_stays_crouched_matches_golden() {
    // THE FIT-GATE FALLBACK, end to end. Shift is released on tick 60, deep inside
    // the corridor: `getDesiredPose()` says STANDING, the gate refuses it, and the
    // second arm keeps CROUCHING while the walk speed goes to full.
    //
    // A naive `pose = sneak ? CROUCHING : STANDING` port grows the box into the
    // slab on tick 60 and jams — vanilla has no recovery for a *player* whose box
    // grows (`Entity.refreshDimensions` excludes both clients and `Player`), so
    // the gate is the only thing standing between this and a clipped player.
    let world = low_corridor(40, 2);
    let state = grounded_facing(0.5, 1.0, 0.5, -90.0);
    let inputs = |t: usize| MovementInput {
        forward: 1.0,
        sneak: t < 60,
        ..MovementInput::NONE
    };
    assert_tick_trace_inputs(
        "crouch_release_stays_crouched",
        &world,
        state,
        &GOLDEN_CROUCH_RELEASE_STAYS_CROUCHED,
        inputs,
    );

    // It must both keep moving and speed up — a jam would show as neither.
    let n = GOLDEN_CROUCH_RELEASE_STAYS_CROUCHED.len();
    let at = |i: usize| f64::from_bits(GOLDEN_CROUCH_RELEASE_STAYS_CROUCHED[i].pos[0]);
    let sneak_rate = (at(59) - at(50)) / 9.0;
    let walk_rate = (at(n - 1) - at(n - 10)) / 9.0;
    assert!(
        walk_rate > sneak_rate * 2.0,
        "releasing shift must speed the player up (sneak {sneak_rate}, walk {walk_rate})"
    );

    // And the pose is still CROUCHING at the end, with the crouch eye height.
    let profile = PhysicsProfile::mc_1_21();
    let mut s = grounded_facing(0.5, 1.0, 0.5, -90.0);
    for t in 0..n {
        tick(&mut s, inputs(t), &world, &profile);
    }
    assert_eq!(s.pose, Pose::Crouching, "the gate must veto STANDING");
    assert_eq!(s.eye_height, 1.27);
    // CONTROL: the identical replay with the slab ceiling removed *does* revert to
    // STANDING, so the assertion above is the fit gate and not a stuck flag.
    let mut open = World::default();
    for x in -8..=40 {
        for z in -2..=2 {
            open.solid(x, 0, z);
        }
    }
    let mut o = grounded_facing(0.5, 1.0, 0.5, -90.0);
    for t in 0..n {
        tick(&mut o, inputs(t), &open, &profile);
    }
    assert_eq!(o.pose, Pose::Standing);
}

#[test]
fn elytra_gap_glide_matches_golden() {
    // `Pose.FALL_FLYING` is the same `0.6 × 0.6` record as `Pose.SWIMMING`
    // (`Avatar.java:27-28`), so a glider fits a one-block gap too. The pose is
    // *seeded*: a glider arriving at a tunnel has been fall-flying for many ticks,
    // and starting it STANDING at 0.9 blocks/tick would jam it on the ceiling
    // before the first `updatePlayerPose` could run.
    let mut world = World::default();
    for x in -8..=80 {
        for z in -2..=2 {
            world.solid(x, 0, z);
            if x >= 1 {
                world.solid(x, 2, z);
            }
        }
    }
    let mut state = grounded_facing(0.5, 1.0, 0.5, -90.0).with_pose(Pose::FallFlying);
    state.fall_flying = true;
    state.velocity = Vec3d::new(0.9, 0.0, 0.0);
    assert_tick_trace(
        "elytra_gap_glide",
        &world,
        state,
        &GOLDEN_ELYTRA_GAP_GLIDE,
        MovementInput::NONE,
    );

    let x = f64::from_bits(GOLDEN_ELYTRA_GAP_GLIDE.last().unwrap().pos[0]);
    assert!(x > 30.0, "the glider did not get down the tunnel (x = {x})");
    // The dry tunnel is what makes this FALL_FLYING and not SWIMMING: swimming
    // comes first in `getDesiredPose`, so a wet fixture would prove nothing about
    // the fall-flying arm.
    let profile = PhysicsProfile::mc_1_21();
    let mut s = grounded_facing(0.5, 1.0, 0.5, -90.0).with_pose(Pose::FallFlying);
    s.fall_flying = true;
    s.velocity = Vec3d::new(0.9, 0.0, 0.0);
    for _ in 0..GOLDEN_ELYTRA_GAP_GLIDE.len() {
        tick(&mut s, MovementInput::NONE, &world, &profile);
    }
    assert_eq!(s.pose, Pose::FallFlying);
    assert!(!s.swimming);
}
