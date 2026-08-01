//! `Entity.fallDistance` — accumulation and every reset this crate maintains.
//!
//! # Why this file exists
//!
//! `fall_distance` used to be a permanent `0.0`: read by
//! `Player.maybeBackOffFromEdge`'s airborne branch, written by nothing. See
//! [`lodestone_physics::player::PlayerState::fall_distance`]'s doc for the full
//! jar citation of every site now reproduced: the `checkFallDamage`
//! accumulation and grounded reset, the water reset, the lava halving, the
//! climbable reset, the Slow Falling/Levitation reset, the elytra
//! `checkFallDistanceAccumulation` clamp, and the stuck-in-block reset.
//!
//! # The vacuity risk this file is written against
//!
//! A test whose player never leaves the ground cannot distinguish real
//! accumulation from the old permanent-`0.0`. Every test below either drives
//! genuine airborne ticks through the public `tick_air`/`tick_water`/
//! `tick_lava`/`tick_elytra`/`tick` entry points (never hand-setting
//! `fall_distance` to the value under test), or — where a precondition must be
//! hand-set to reach an otherwise-unreachable regime in a bounded number of
//! ticks (an initial velocity, exactly as `tests/edge_back_off.rs`'s
//! `scenario_sneak_edge_diagonal`-style tests already do) — says so explicitly
//! and keeps the *quantity under test* (the accumulation/reset arithmetic)
//! driven by a real move, not by writing to `fall_distance` itself.
//!
//! Several tests derive their expected value from the tick's **measured
//! position delta** (`state.position.y` before/after, read from the public
//! field) rather than from a hand-typed number, and check that
//! `state.fall_distance` matches what `checkFallDamage`'s formula
//! (`fall_distance -= (float) ya`) predicts from that measurement — so the
//! assertion is cross-checking two independently-observed quantities, not
//! replaying the implementation's own arithmetic back at it.
//!
//! The flagship test, `real_accumulated_fall_distance_opens_the_edge_back_off_gate`,
//! goes further: it proves the accumulation is not just bookkeeping but changes
//! *committed position* — the one behaviour this whole field exists for.

use std::collections::{HashMap, HashSet};

use lodestone_physics::{
    Aabb, CollisionView, EdgeBackOff, EntityDimensions, EntityMotion, FluidState, MoveContext,
    MovementInput, PhysicsProfile, PlayerState, Vec3d, move_entity, tick, tick_air, tick_lava,
};

/// A minimal synthetic world: solid blocks, plus optional water/lava/climbable/
/// stuck cells, each used by only the tests that need them.
#[derive(Default)]
struct World {
    solid: HashSet<(i32, i32, i32)>,
    water: HashSet<(i32, i32, i32)>,
    lava: HashSet<(i32, i32, i32)>,
    climbable: HashSet<(i32, i32, i32)>,
    stuck: HashMap<(i32, i32, i32), Vec3d>,
}

impl World {
    /// A flat floor at block-`y` = `y`, covering `-r..=r` on both horizontal
    /// axes (so the top surface is at world-Y `y + 1`).
    fn flat_floor(r: i32, y: i32) -> Self {
        let mut w = Self::default();
        for x in -r..=r {
            for z in -r..=r {
                w.solid.insert((x, y, z));
            }
        }
        w
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.solid.contains(&(x, y, z)) {
            let (fx, fy, fz) = (f64::from(x), f64::from(y), f64::from(z));
            out.push(Aabb::new(fx, fy, fz, fx + 1.0, fy + 1.0, fz + 1.0));
        }
    }
    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.water.contains(&(x, y, z))
    }
    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        self.lava.contains(&(x, y, z))
    }
    fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
        self.climbable.contains(&(x, y, z))
    }
    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        self.stuck.get(&(x, y, z)).copied()
    }
}

// ---------------------------------------------------------------------------
// Accumulation
// ---------------------------------------------------------------------------

#[test]
fn free_fall_accumulation_matches_the_measured_position_delta() {
    // Real airborne ticks, open sky (no blocks at all) so nothing can land or
    // reset this early. `MovementInput::NONE` -- no horizontal drift, isolating
    // the vertical accumulation.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
    state.on_ground = false;

    let mut expected = 0.0_f64;
    for tick_index in 0..10 {
        let old_y = state.position.y;
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
        assert!(
            !state.on_ground,
            "tick {tick_index}: landed in open sky — fixture is broken"
        );
        let ya = state.position.y - old_y;
        // `checkFallDamage`'s own guard: only a real downward move accumulates.
        if ya < 0.0 {
            expected -= f64::from(ya as f32);
        }
        assert_eq!(
            state.fall_distance.to_bits(),
            expected.to_bits(),
            "tick {tick_index}: fall_distance {} != checkFallDamage(ya={ya}) prediction {}",
            state.fall_distance,
            expected
        );
    }
    assert!(
        expected > 0.6,
        "fixture must clear maxDownStep (0.6) for the gate tests below to be \
         meaningful — only accumulated {expected} in 10 ticks"
    );
}

#[test]
fn landing_resets_fall_distance_to_exactly_zero() {
    // Real fall onto a real floor -- fall_distance must be strictly positive the
    // tick before landing and exactly zero the tick landing happens (and after).
    let world = World::flat_floor(6, 0);
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 5.0, 0.5), 0.0);
    state.on_ground = false;

    let mut saw_airborne_nonzero = false;
    let mut landed = false;
    for _ in 0..100 {
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
        if !state.on_ground {
            if state.fall_distance > 0.0 {
                saw_airborne_nonzero = true;
            }
            continue;
        }
        landed = true;
        assert_eq!(
            state.fall_distance, 0.0,
            "on_ground became true but fall_distance was not reset"
        );
        break;
    }
    assert!(landed, "fixture never landed — nothing to test");
    assert!(
        saw_airborne_nonzero,
        "fixture never observed a positive fall_distance while airborne — the \
         reset assertion above would be vacuously true for a player that never \
         accumulated anything"
    );

    // And it stays zero on a subsequent grounded tick.
    tick_air(&mut state, MovementInput::NONE, &world, &profile);
    assert_eq!(state.fall_distance, 0.0);
}

#[test]
fn entering_water_resets_fall_distance_to_exactly_zero() {
    // Real fall through open air, then into a water column with no floor under
    // it -- water_reset must fire on the very first tick `tick` (the top-level
    // dispatcher, which computes the per-tick fluid summary) sees the box
    // touching water, while fall_distance was still genuinely positive from the
    // real fall the tick before.
    let mut world = World::default();
    for x in -2..=2 {
        for z in -2..=2 {
            for y in -10..=0 {
                world.water.insert((x, y, z));
            }
        }
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 5.0, 0.5), 0.0);
    state.on_ground = false;

    // `travel_and_check_inside_blocks` dispatches on the fluid summary of the
    // *pre-move* box (`compute_fluid_state`, exposed publicly), computed once at
    // the top of the tick — exactly reproduce that here so "was this the tick
    // `tick_water` ran" is judged the same way the crate itself judges it,
    // rather than by an after-the-fact guess at the post-move position.
    let mut saw_positive_before_water = false;
    let mut saw_water_tick = false;
    for _ in 0..30 {
        let pre_fluid = lodestone_physics::compute_fluid_state(
            state.bounding_box(&profile),
            state.position,
            state.pose.eye_height(),
            &world,
        );
        if !pre_fluid.in_water() && state.fall_distance > 0.0 {
            saw_positive_before_water = true;
        }
        tick(&mut state, MovementInput::NONE, &world, &profile);
        if pre_fluid.in_water() {
            // This tick's dispatch was `tick_water`, whose first act is the
            // reset — and nothing in this fixture (no floor, no obstruction)
            // gives it a reason to re-accumulate before returning.
            assert_eq!(
                state.fall_distance, 0.0,
                "fall_distance was not reset on entering water"
            );
            saw_water_tick = true;
            break;
        }
    }
    assert!(
        saw_positive_before_water,
        "fixture never accumulated real fall_distance before reaching water"
    );
    assert!(
        saw_water_tick,
        "fixture never reached the water column — nothing to test"
    );
}

/// Pins the **one named divergence** in this subsystem: the water reset that
/// vanilla reaches from *inside* `move()`, which this crate does not model.
///
/// Hand-derived from the jar, not from this crate. `updateFluidInteraction` has
/// **two** call sites, not one:
///
/// * `Entity.baseTick` (`Entity.java:537`), before `travel()` — the pre-move
///   evaluation this crate reproduces as `tick`'s dispatch summary; and
/// * `LivingEntity.checkFallDamage` (`LivingEntity.java:365`),
///   `if (!this.isInWater()) { this.updateFluidInteraction(); }`, which runs
///   *inside* `move()` against the **post-move** position.
///
/// `Entity.isInWater()` is `return this.wasTouchingWater` (`Entity.java:1605-1607`)
/// — a *cached* flag that `updateFluidInteraction` itself rewrites
/// (`Entity.java:1657-1666`). So on the tick a fall first enters water, vanilla
/// re-evaluates mid-`move`, hits `if (inWater) resetFallDistance()`
/// (`Entity.java:1658-1659`), and the `super.checkFallDamage` accumulation that
/// follows is then skipped because `!isInWater()` is now false
/// (`Entity.java:1565`). Vanilla therefore ends the entry tick at **exactly 0.0**.
///
/// This crate freezes the fluid summary for the whole tick, so the entry tick is
/// still dispatched to `tick_air`, which accumulates that tick's descent.
///
/// **Why this is recorded rather than fixed**: modelling it costs a second
/// `compute_fluid_state` on every air tick (a box-cell walk, on the path a
/// pathfinder calls thousands of times) and **cannot change committed position**.
/// The accumulation runs at the *end* of the move, after the edge-back-off gate
/// has already read the old value, and the next tick's dispatch re-derives the
/// fluid summary from the same post-move position vanilla used — so `tick_water`
/// resets it before that tick's gate reads anything. The divergence is a
/// single-tick transient in the field, visible only to an external reader
/// between ticks (a future fall-damage predictor), never to the gate. The second
/// half of this test is what establishes that bound.
#[test]
fn water_entry_tick_is_the_one_known_divergence_and_it_lasts_exactly_one_tick() {
    // Same fixture as the test above: a deep water column with no floor.
    let mut world = World::default();
    for x in -2..=2 {
        for z in -2..=2 {
            for y in -10..=0 {
                world.water.insert((x, y, z));
            }
        }
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 5.0, 0.5), 0.0);
    state.on_ground = false;

    let in_water_now = |s: &PlayerState| {
        lodestone_physics::compute_fluid_state(
            s.bounding_box(&profile),
            s.position,
            s.pose.eye_height(),
            &world,
        )
        .in_water()
    };

    // Find the entry tick: pre-move box out of water, post-move box in it.
    let mut entry = None;
    for _ in 0..30 {
        let pre_in_water = in_water_now(&state);
        tick(&mut state, MovementInput::NONE, &world, &profile);
        if !pre_in_water && in_water_now(&state) {
            entry = Some(state.fall_distance);
            break;
        }
    }
    let entry_fall_distance =
        entry.expect("fixture never produced a tick that *entered* water — nothing to test");

    // The divergence itself. Vanilla: exactly 0.0, per the citations above.
    // This crate: strictly positive, because `tick_air` accumulated the descent.
    assert!(
        entry_fall_distance > 0.0,
        "expected this crate's known divergence (a positive fall_distance on the \
         water-entry tick, where vanilla resets to 0.0 inside move()); got \
         {entry_fall_distance}. If this now reads 0.0 the gap has been CLOSED — \
         that is an improvement, but update `PlayerState::fall_distance`'s \
         \"Not modelled\" list and docs/edge-back-off.md, which both still \
         document it as open."
    );

    // The bound that makes it harmless: the very next tick is dispatched to
    // `tick_water`, whose first act is the reset. One tick, then converged.
    assert!(
        in_water_now(&state),
        "precondition for the bound: entry tick must have left the box in water"
    );
    tick(&mut state, MovementInput::NONE, &world, &profile);
    assert_eq!(
        state.fall_distance, 0.0,
        "the divergence must not survive past the entry tick — the next tick's \
         `tick_water` reset is what keeps it invisible to the edge-back-off gate"
    );
}

#[test]
fn lava_halves_fall_distance() {
    // Isolated: velocity zero and no floor under the lava column, so this
    // tick's own move contributes ya == 0 exactly (no re-accumulation to
    // confound the halving), and on_ground stays false (no grounded reset
    // either). fall_distance itself is a legitimate hand-set precondition here
    // (a scenario input, like `PlayerState::with_fall_distance` is documented
    // for), because the quantity under test is the *halving arithmetic*, not
    // how the value got there — that is covered by the accumulation test above.
    let mut world = World::default();
    for x in -2..=2 {
        for z in -2..=2 {
            world.lava.insert((x, 5, z));
        }
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 5.5, 0.5), 0.0);
    state.on_ground = false;
    state.fall_distance = 2.0;
    let fluid = FluidState {
        lava_height: 1.0,
        water_height: 0.0,
        eye_in_water: false,
        eye_in_lava: false,
    };

    tick_lava(&mut state, MovementInput::NONE, &fluid, &world, &profile);

    assert!(!state.on_ground);
    assert_eq!(
        state.fall_distance, 1.0,
        "baseTick's `fallDistance *= 0.5` did not fire (or something else also touched it)"
    );
}

#[test]
fn climbable_resets_fall_distance_to_exactly_zero() {
    // Real fall down an open shaft, then into a climbable (ladder) column.
    // Sneaking clamps the climb velocity's Y to exactly 0 while descending
    // (`handle_on_climbable`), so this tick's own move contributes ya == 0 and
    // cannot re-accumulate anything -- isolating the climbable reset itself.
    let mut world = World::default();
    for y in -10..=5 {
        world.climbable.insert((0, y, 0));
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 10.0, 0.5), 0.0);
    state.on_ground = false;

    let mut saw_positive = false;
    for _ in 0..20 {
        let on_climbable_before =
            world.is_climbable(0, lodestone_physics::mth::floor(state.position.y), 0);
        let sneak = on_climbable_before; // hold shift once on the ladder
        tick_air(
            &mut state,
            MovementInput {
                sneak,
                ..MovementInput::NONE
            },
            &world,
            &profile,
        );
        if !on_climbable_before && state.fall_distance > 0.0 {
            saw_positive = true;
        }
        if on_climbable_before {
            assert_eq!(
                state.fall_distance, 0.0,
                "climbable reset did not fire (or something re-accumulated)"
            );
            assert!(
                saw_positive,
                "fixture never accumulated real fall_distance before the ladder — nothing to reset"
            );
            return;
        }
    }
    panic!("fixture never reached the climbable column — nothing to test");
}

#[test]
fn slow_falling_resets_fall_distance_before_travel() {
    // Real fall builds a genuine fall_distance; velocity is then hand-set to
    // zero (a legitimate scenario precondition -- see `lava_halves_fall_distance`)
    // purely so this tick's own elytra-free travel contributes a small, exactly
    // *measured* ya rather than an unmeasured one, letting the assertion below
    // isolate "was the pre-travel reset applied" instead of eyeballing a
    // tolerance.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
    state.on_ground = false;
    for _ in 0..8 {
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
    }
    assert!(
        state.fall_distance > 0.6,
        "fixture did not accumulate real fall_distance before the reset — got {}",
        state.fall_distance
    );

    state.velocity = Vec3d::ZERO;
    state.effects.slow_falling = true;
    let old_y = state.position.y;
    tick(&mut state, MovementInput::NONE, &world, &profile);
    let ya = state.position.y - old_y;
    let expected = if ya < 0.0 { -f64::from(ya as f32) } else { 0.0 };
    assert_eq!(
        state.fall_distance.to_bits(),
        expected.to_bits(),
        "expected the pre-travel Slow Falling reset (0.0) plus this tick's own \
         measured accumulation ({expected}), got {}",
        state.fall_distance
    );
}

#[test]
fn levitation_resets_fall_distance_before_travel() {
    // Same construction as the Slow Falling test, for the other half of the
    // `hasEffect(SLOW_FALLING) || hasEffect(LEVITATION)` disjunction.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
    state.on_ground = false;
    for _ in 0..8 {
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
    }
    assert!(state.fall_distance > 0.6);

    state.velocity = Vec3d::ZERO;
    state.effects.levitation = Some(0);
    tick(&mut state, MovementInput::NONE, &world, &profile);
    // Levitation replaces gravity with an upward pull toward 0.05, so this
    // tick's own move is upward (ya > 0) and contributes no further
    // accumulation at all -- the reset is the whole story here.
    assert_eq!(state.fall_distance, 0.0);
}

#[test]
fn elytra_clamps_fall_distance_to_one_before_travel() {
    // Real fall past the clamp threshold (1.0, not just maxDownStep), then
    // start gliding. `checkFallDistanceAccumulation` clamps to 1.0 *before*
    // travel(), so this tick's own elytra move (whatever small further
    // accumulation it contributes, measured the same way as the other
    // isolation tests) must land on top of exactly 1.0, not the pre-clamp
    // value.
    let world = World::default();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
    state.on_ground = false;
    for _ in 0..12 {
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
    }
    assert!(
        state.fall_distance > 1.0,
        "fixture did not accumulate past the elytra clamp threshold — got {}",
        state.fall_distance
    );
    let pre_clamp = state.fall_distance;

    state.fall_flying = true;
    state.velocity = Vec3d::ZERO; // > -0.5, satisfying checkFallDistanceAccumulation's guard
    let old_y = state.position.y;
    tick(&mut state, MovementInput::NONE, &world, &profile);
    let ya = state.position.y - old_y;
    let expected = 1.0 - if ya < 0.0 { f64::from(ya as f32) } else { 0.0 };
    assert_ne!(
        state.fall_distance, pre_clamp,
        "the clamp did not run at all (value unchanged from the pre-glide fall)"
    );
    assert_eq!(
        state.fall_distance.to_bits(),
        expected.to_bits(),
        "expected the clamp (1.0) plus this tick's own measured accumulation, got {}",
        state.fall_distance
    );
}

#[test]
fn stuck_in_block_resets_fall_distance() {
    // Real fall into a cobweb column with no floor beneath it.
    let mut world = World::default();
    for y in -10..=5 {
        world.stuck.insert(
            (0, y, 0),
            Vec3d {
                x: 0.25,
                y: 0.05,
                z: 0.25,
            },
        );
    }
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 10.0, 0.5), 0.0);
    state.on_ground = false;

    let mut saw_positive = false;
    for i in 0..20 {
        tick(&mut state, MovementInput::NONE, &world, &profile);
        if state.fall_distance > 0.0 {
            saw_positive = true;
        }
        if state.stuck_speed_multiplier != Vec3d::ZERO {
            // The multiplier set THIS tick is consumed at the top of NEXT
            // tick's move (the documented one-tick lag) — that is the tick the
            // reset fires on too, since both are written by the same
            // `update_stuck_multiplier` call.
            assert_eq!(
                state.fall_distance, 0.0,
                "tick {i}: stuck-in-block reset did not fire"
            );
            assert!(
                saw_positive,
                "fixture never accumulated real fall_distance before the web"
            );
            return;
        }
    }
    panic!("fixture never registered a stuck multiplier — nothing to test");
}

// ---------------------------------------------------------------------------
// The consequence: the edge back-off gate
// ---------------------------------------------------------------------------

/// A floor at `y = 0` solid for `x <= 0` only (matches
/// `tests/edge_back_off.rs`'s `ledge_at_x1`), so the top surface is at world-Y
/// `1.0` and there is nothing at all for `x >= 1`.
fn ledge_at_x1() -> World {
    let mut w = World::default();
    for x in -6..=0 {
        for z in -6..=6 {
            w.solid.insert((x, 0, z));
        }
    }
    w
}

#[test]
fn real_accumulated_fall_distance_opens_the_edge_back_off_gate_that_zero_would_close() {
    // Phase 1: a REAL fall, driven entirely by `tick_air` with no horizontal
    // input, from directly above solid ground (x = 0.5, well clear of the x = 1
    // edge) down toward the ledge platform. `fall_distance` here is 100% the
    // product of the accumulation this change adds -- never hand-set.
    let world = ledge_at_x1();
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(Vec3d::new(0.5, 2.0, 0.5), -90.0);
    state.on_ground = false;

    let mut mid_air = None;
    for i in 0..40 {
        tick_air(&mut state, MovementInput::NONE, &world, &profile);
        // Still airborne, feet within maxDownStep (0.6) of the floor top (1.0),
        // and already past maxDownStep in accumulated fall_distance: the exact
        // regime `Player.isAboveGround`'s airborne branch exists for.
        let height_above_floor = state.position.y - 1.0;
        if !state.on_ground && (0.0..0.6).contains(&height_above_floor) && state.fall_distance > 0.6
        {
            mid_air = Some(state);
            break;
        }
        assert!(
            !state.on_ground,
            "tick {i}: landed before ever entering the target window — fixture \
             needs a taller drop"
        );
    }
    let mid_air = mid_air.expect(
        "never reached an airborne tick with real fall_distance > 0.6 AND the \
         floor within maxDownStep — fixture is broken, or gravity/drag jumped \
         straight over the window",
    );
    assert!(
        mid_air.fall_distance > 0.6,
        "sanity: window condition above should already guarantee this"
    );
    assert!(!mid_air.on_ground);

    // Phase 2: from this exact real state, a single large sneaking horizontal
    // move toward the edge (a "launch", not a walk -- ordinary sneak-walk speed
    // cannot cross a ledge in one tick; `tests/edge_back_off.rs`'s
    // `scenario_sneak_edge_diagonal` and `fall_distance_gates_the_airborne_branch`
    // isolate the mechanism the same way). Run it twice through the bare
    // `move_entity` primitive -- identical delta, identical world, identical
    // position -- varying only which `fall_distance` the gate sees. This is the
    // same "pure control" shape as `back_off_is_the_only_difference`.
    let profile = PhysicsProfile::mc_1_21();
    let delta = Vec3d::new(0.8, mid_air.velocity.y, 0.0);

    let run = |fall_distance: f64| -> f64 {
        let mut motion = EntityMotion::at(mid_air.position);
        motion.velocity = delta;
        motion.on_ground = false;
        move_entity(
            &mut motion,
            EntityDimensions::PLAYER,
            &world,
            &profile,
            MoveContext {
                edge_back_off: EdgeBackOff::Player {
                    staying_on_ground_surface: true,
                    fall_distance,
                },
                ..MoveContext::default()
            },
        );
        motion.position.x
    };

    let with_real_fall_distance = run(mid_air.fall_distance);
    let with_zero_fall_distance = run(0.0);

    // The control: a permanent 0.0 (the old, pre-accumulation behaviour) closes
    // `canFallAtLeast` (the floor is within reach) and so backs the player off
    // the ledge -- it must NOT reach 0.5 + 0.8.
    assert!(
        (with_zero_fall_distance - (0.5 + 0.8)).abs() > 1e-9,
        "control did not back off with fall_distance = 0.0 (got x = \
         {with_zero_fall_distance}) — the fixture cannot express the divergence \
         this test exists to show"
    );
    // The real behaviour: fall_distance > maxDownStep closes the airborne
    // branch outright (`fall_distance < maxDownStep` is false), so the gate
    // never engages and the player reaches the full delta.
    assert_eq!(
        with_real_fall_distance.to_bits(),
        (0.5_f64 + 0.8).to_bits(),
        "real accumulated fall_distance ({}) should have closed the airborne \
         branch and let the move through unbacked-off, got x = {with_real_fall_distance}",
        mid_air.fall_distance
    );
    assert_ne!(
        with_real_fall_distance.to_bits(),
        with_zero_fall_distance.to_bits(),
        "real accumulation made no difference at all vs. the old permanent 0.0 \
         — this is the exact divergence issue #194 exists to close"
    );
}
