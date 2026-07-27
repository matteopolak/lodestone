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
    MovementInput, PlayerState, StatusEffects, tick, tick_air, tick_elytra,
};
use lodestone_physics::{PhysicsProfile, Vec3d};

#[path = "support/golden_traces.rs"]
mod golden_traces;
use golden_traces::{
    GOLDEN_ANALOG_STRAFE, GOLDEN_BLUE_ICE_SLIDE, GOLDEN_DIAGONAL_WALK, GOLDEN_ELYTRA_CLIMB,
    GOLDEN_ELYTRA_DIAGONAL_YAW, GOLDEN_ELYTRA_DIVE, GOLDEN_ELYTRA_GLIDE_LEVEL, GOLDEN_FREE_FALL,
    GOLDEN_HONEY_JUMP, GOLDEN_ICE_SLIDE, GOLDEN_JUMP_BOOST, GOLDEN_LADDER_CLIMB,
    GOLDEN_LADDER_SNEAK_HOLD, GOLDEN_LAVA_SINK, GOLDEN_LEVITATION, GOLDEN_SLAB_STEP,
    GOLDEN_SLIME_BOUNCE, GOLDEN_SLIME_BOUNCE_SNEAK, GOLDEN_SLOW_FALLING_WATER,
    GOLDEN_SOUL_SAND_WALK, GOLDEN_SPRINT_JUMP, GOLDEN_SWIM_SPRINT, GOLDEN_WALK_FLAT,
    GOLDEN_WALK_INTO_WALL, GOLDEN_WATER_SINK, GOLDEN_WATER_CURRENT_PUSH, GoldenTick,
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
