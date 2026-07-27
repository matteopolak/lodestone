//! Player movement core, mirroring vanilla's `LivingEntity`/`Player` tick.
//!
//! The per-tick pipeline reproduced here is (for a non-fluid, non-elytra
//! player), in order:
//!
//! 1. `aiStep` velocity snap-to-zero (`< 9.0E-6` horizontal, `< 0.003` vertical).
//! 2. Jump handling (`jumpFromGround`, including the sprint boost).
//! 3. `travel` → `travelInAir`:
//!    - `moveRelative` adds the friction-influenced input acceleration,
//!    - `move` resolves collision and applies the block speed factor,
//!    - gravity is subtracted from the post-move Y,
//!    - horizontal drag (`blockFriction * 0.91`) and vertical drag (`0.98`) are
//!      applied last.
//!
//! Every width (`f32` vs `f64`) and every operation order matches the reference
//! source, because the server validates the resulting positions.

use crate::collision::CollisionView;
use crate::entity::{EntityDimensions, EntityMotion, MoveContext, move_entity};
use crate::fluid::apply_fluid_push;
use crate::geometry::{Aabb, Vec3d};
use crate::mth::{self};
use crate::profile::{FluidModel, InputModel, PhysicsProfile};

/// Raw player intent for one tick, before any client-side transformation.
///
/// `forward`/`strafe` are the digital movement axes (typically `-1.0`, `0.0` or
/// `1.0`), matching `Input.getMoveVector()` (`y` = forward, `x` = strafe).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementInput {
    /// Forward (`+`) / backward (`-`) intent.
    pub forward: f32,
    /// Left (`+`) / right (`-`) strafe intent.
    pub strafe: f32,
    /// Jump key held.
    pub jump: bool,
    /// Sneak (shift) key held.
    pub sneak: bool,
    /// Sprint active this tick.
    pub sprint: bool,
}

impl MovementInput {
    /// A no-input tick (standing still).
    pub const NONE: Self = Self {
        forward: 0.0,
        strafe: 0.0,
        jump: false,
        sneak: false,
        sprint: false,
    };
}

/// Active status effects that influence the movement integration.
///
/// Only the effects that change the *physics* (not just stats) live here.
/// Speed/Slowness are deliberately **absent**: they are attribute modifiers on
/// `MOVEMENT_SPEED`, so they arrive pre-folded into the effective movement speed
/// via the attribute pipeline (see the crate docs' "attribute seam"), not as a
/// physics flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusEffects {
    /// `MobEffects.LEVITATION` amplifier (0-based) if active. In `travelInAir`
    /// this *replaces* gravity with `y += (0.05*(amp+1) - y) * 0.2`.
    pub levitation: Option<u32>,
    /// `MobEffects.SLOW_FALLING`. Reduces `getEffectiveGravity()` to
    /// `min(gravity, 0.01)` **while falling** — which, in fluids, is precisely
    /// what revives the otherwise-dead `-0.003` slow-descent clamp.
    pub slow_falling: bool,
    /// `MobEffects.DOLPHINS_GRACE`. Forces the in-water horizontal slow-down to
    /// `0.96F` regardless of sprint state.
    pub dolphins_grace: bool,
    /// `MobEffects.JUMP_BOOST` amplifier (0-based) if active. Per the ruling that
    /// Jump Boost is **not** a `MOVEMENT_SPEED` modifier, it rides its own field:
    /// `getJumpBoostPower()` adds `0.1F * (amp + 1)` to the jump velocity in
    /// `getJumpPower`, *after* the `JUMP_STRENGTH * blockJumpFactor` product.
    pub jump_boost: Option<u32>,
}

/// Mutable player physics state carried across ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// World position (feet centre), `Vec3` in vanilla.
    pub position: Vec3d,
    /// Delta movement (velocity), `Vec3` in vanilla.
    pub velocity: Vec3d,
    /// Yaw in degrees; `0` faces `+Z` (south).
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground, i.e. this tick's move collided
    /// **downward** (`verticalCollisionBelow` in `Entity.move`). This is the flag
    /// the client **transmits** to the server on every movement packet
    /// (`ServerboundMovePlayerPacket`'s `onGround`).
    ///
    /// It is a *distinct decision* from the collision result the server re-runs
    /// from our reported position: if the server ever believes we are unsupported
    /// and not descending in open air, it counts `aboveGroundTickCount` and
    /// disconnects with `multiplayer.disconnect.flying` at `getMaximumFlyingTicks`
    /// (80 ticks at default gravity). Because our position is bit-exact, the
    /// server's own downward collision stays aligned with this flag, so the two
    /// never diverge — but a driver must transmit *this* value unmodified rather
    /// than re-deriving one.
    ///
    /// Vanilla computes it identically in **every** movement mode (walking,
    /// swimming, climbing, falling); there is no bespoke "supported" notion for
    /// swimming or climbing. The **sole override** is `Player.tick`, which forces
    /// `onGround = false` while a **spectator or passenger** (riding a
    /// boat/minecart/horse). This engine has no riding state, so a driver that
    /// adds vehicles must apply that override itself — see the
    /// `spectator_or_passenger_note` contract test in `tests/on_ground.rs`.
    ///
    /// Note: a player starting from rest reports airborne for exactly one settle
    /// tick, because a tick runs `move()` before applying gravity — matching the
    /// server's own first-tick computation.
    pub on_ground: bool,
    /// Whether the player collided horizontally last tick.
    pub horizontal_collision: bool,
    /// `noJumpDelay` countdown that gates repeated jumps.
    pub no_jump_delay: i32,
    /// Whether the player is currently sprinting (affects movement speed).
    pub sprinting: bool,
    /// Whether the player is gliding with an elytra (`isFallFlying()`). When set,
    /// [`tick`] routes to [`tick_elytra`] instead of [`tick_air`] (fluid still
    /// takes precedence, matching vanilla's `travel()` dispatch order).
    pub fall_flying: bool,
    /// Active physics-affecting status effects.
    pub effects: StatusEffects,
    /// Effective `MOVEMENT_SPEED` attribute value handed in by the entity layer
    /// (`lodestone-entity`'s `AttributeInstance.value()`), or `None` to let
    /// physics compute the standalone base+sprint value itself.
    ///
    /// **Reconciled attribute seam.** Vanilla `Player` does, every tick,
    /// `setSpeed((float) getAttributeValue(MOVEMENT_SPEED))` and movement reads
    /// that float via `getSpeed()`. That attribute value already folds in the
    /// transient **sprint** modifier (`AddMultipliedTotal 0.3`) *and* any
    /// Speed/Slowness/Depth-Strider modifiers, computed by the three-stage
    /// `calculateValue()` (AddValue → AddMultipliedBase → AddMultipliedTotal).
    /// Physics must **not** reimplement that maths or re-apply sprint: when this
    /// is `Some(v)`, [`friction_influenced_speed`] uses `v as f32` directly
    /// (reproducing vanilla's `(float)` cast at the same place) and ignores the
    /// `sprinting` flag, so there is no double-count. Pass the raw `f64` — never
    /// a pre-cast `f32` — so the double→float rounding stays inside physics.
    pub movement_speed: Option<f64>,
    /// Pending **"stuck in block" speed multiplier** (`Entity.stuckSpeedMultiplier`),
    /// set last tick by the block we were inside and consumed at the top of the
    /// next move (see [`CollisionView::stuck_multiplier`]). `ZERO` means "not
    /// stuck"; vanilla treats `lengthSqr <= 1.0E-7` as unset. Cobweb, powder snow
    /// and sweet berry bush write this; consumption multiplies the tick's
    /// movement component-wise and then zeroes velocity, exactly as vanilla — the
    /// one-tick delay between entering the block and being slowed is observable
    /// and reproduced.
    pub stuck_speed_multiplier: Vec3d,
}

impl PlayerState {
    /// Constructs a state standing at `position` facing `yaw`.
    #[must_use]
    pub fn at(position: Vec3d, yaw: f32) -> Self {
        Self {
            position,
            velocity: Vec3d::ZERO,
            yaw,
            pitch: 0.0,
            on_ground: false,
            horizontal_collision: false,
            no_jump_delay: 0,
            sprinting: false,
            fall_flying: false,
            effects: StatusEffects::default(),
            movement_speed: None,
            stuck_speed_multiplier: Vec3d::ZERO,
        }
    }

    /// Returns a copy of this state with the given status effects applied.
    #[must_use]
    pub fn with_effects(mut self, effects: StatusEffects) -> Self {
        self.effects = effects;
        self
    }

    /// Returns a copy of this state with the entity layer's effective
    /// `MOVEMENT_SPEED` attribute value injected (see [`Self::movement_speed`]).
    #[must_use]
    pub fn with_movement_speed(mut self, value: f64) -> Self {
        self.movement_speed = Some(value);
        self
    }

    /// The player's bounding box at its current position.
    ///
    /// The player's `0.6 × 1.8` hitbox is per-entity data ([`EntityDimensions`]),
    /// not version data, so it no longer comes from the profile. The `profile`
    /// parameter is retained (as `_profile`) purely for source compatibility with
    /// existing callers; it is unused, and a caller may drop the argument once its
    /// call sites are updated.
    #[must_use]
    pub fn bounding_box(&self, _profile: &PhysicsProfile) -> Aabb {
        EntityDimensions::PLAYER.bounding_box(self.position)
    }
}

/// `getSpeed()` for a player: the `MOVEMENT_SPEED` attribute cast to `float`.
///
/// Walking is the base `0.1F` (widened to `double` when stored, then cast back
/// to `float`); sprinting applies the `+0.3` `ADD_MULTIPLIED_TOTAL` modifier in
/// `double` before the final `float` cast. Reproduced exactly here.
#[must_use]
fn player_speed(profile: &PhysicsProfile, sprinting: bool) -> f32 {
    let base = f64::from(profile.base_movement_speed); // 0.1F widened
    if sprinting {
        (base * (1.0 + f64::from(profile.sprint_speed_modifier))) as f32
    } else {
        base as f32
    }
}

/// Client-side `LocalPlayer.modifyInput` for the modern (1.21+) input pipeline.
///
/// **Version note:** the square-movement normalization
/// (`modifyInputSpeedForSquareMovement`) is a *structural* difference between
/// modern and legacy clients, not a scalar — see [`PhysicsProfile`] docs. This
/// implements the modern form; a 1.8 client would use a different mapping.
#[must_use]
fn modify_input(
    model: InputModel,
    strafe: f32,
    forward: f32,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    match model {
        InputModel::UnitSquareProjection => {
            modify_input_unit_square(strafe, forward, sneak, sneak_factor)
        }
        // Structural seam: 1.8 used `moveFlying` (normalise by max(1, magnitude),
        // no unit-square projection). Deliberately not modelled yet — failing
        // loudly here is correct, because silently running the modern transform
        // would produce wrong-but-plausible 1.8 movement. Blocked on the 1.8
        // client restructure + a 1.8 JVM oracle.
        InputModel::LegacyMoveFlying => {
            unimplemented!("1.8 moveFlying input pipeline is not implemented yet")
        }
    }
}

fn modify_input_unit_square(
    strafe: f32,
    forward: f32,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    if strafe * strafe + forward * forward == 0.0 {
        return (strafe, forward);
    }
    let mut sx = strafe * 0.98;
    let mut sy = forward * 0.98;
    if sneak {
        sx *= sneak_factor;
        sy *= sneak_factor;
    }
    // modifyInputSpeedForSquareMovement
    let length = (sx * sx + sy * sy).sqrt();
    if length <= 0.0 {
        return (sx, sy);
    }
    let dir_x = sx / length;
    let dir_y = sy / length;
    let ax = dir_x.abs();
    let ay = dir_y.abs();
    let tan = if ay > ax { ax / ay } else { ay / ax };
    let dist_to_unit_square = (1.0 + tan * tan).sqrt();
    let modified_length = (length * dist_to_unit_square).min(1.0);
    (dir_x * modified_length, dir_y * modified_length)
}

/// `Entity.getInputVector(input, speed, yRot)` — yaw-rotated, speed-scaled input.
fn input_vector(strafe: f32, forward: f32, speed: f32, yaw: f32) -> Vec3d {
    let input = Vec3d::new(f64::from(strafe), 0.0, f64::from(forward));
    let length_sqr = input.length_sqr();
    if length_sqr < 1.0E-7 {
        return Vec3d::ZERO;
    }
    let scaled = if length_sqr > 1.0 {
        input.normalize()
    } else {
        input
    }
    .scale(f64::from(speed));
    let rad = yaw * (core::f32::consts::PI / 180.0);
    let sin = f64::from(mth::sin(f64::from(rad)));
    let cos = f64::from(mth::cos(f64::from(rad)));
    Vec3d::new(
        scaled.x * cos - scaled.z * sin,
        scaled.y,
        scaled.z * cos + scaled.x * sin,
    )
}

/// `Entity.getBlockPosBelowThatAffectsMyMovement()` → `getOnPos(0.500001F)`.
///
/// For the common case (no fence/wall special-casing) this is the block at
/// `(floor(x), floor(y - 0.500001), floor(z))`.
pub(crate) fn friction_block(position: Vec3d) -> (i32, i32, i32) {
    let x = mth::floor(position.x);
    let y = mth::floor(position.y - f64::from(0.500001f32));
    let z = mth::floor(position.z);
    (x, y, z)
}

/// The player's per-tick call into the shared entity move core
/// ([`move_entity`]). Restricted, as vanilla's `Entity.move(MoverType.SELF, …)`
/// is, to the parts that affect a player's reported position: collide, commit
/// position, update collision flags, run `restituteMovementAfterCollisions`, and
/// apply the block speed factor.
///
/// This is a thin wrapper: it lifts the player's motion into an [`EntityMotion`],
/// supplies the player's [`EntityDimensions::PLAYER`] hitbox/step height and a
/// [`MoveContext`] (Slow Falling, and `suppress_bounce` = the player sneaking,
/// which both zeroes the base entity restitution and vetoes the block-bounce
/// branch), runs the shared core, and writes the result back. A mob loop would
/// call [`move_entity`] directly with its own dimensions and velocity — the
/// arithmetic is identical, which is the whole point of the shared core.
fn do_move(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
) {
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = MoveContext {
        slow_falling: state.effects.slow_falling,
        suppress_bounce,
    };
    move_entity(&mut motion, EntityDimensions::PLAYER, view, profile, ctx);
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;
}

/// `Entity.restituteMovementAfterCollisions` — the post-collision velocity
/// rewrite that zeroes blocked axes and produces slime/bed bounces.
///
/// `current` is the pre-collision velocity (`deltaMovement`, still `== delta`
/// here); `resolved` is the movement actually achieved. A `LivingEntity` has
/// zero base bounciness, so horizontal wall bounces never happen for a player;
/// the only live branch is the vertical land-bounce off a bouncy block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn restitute_movement_after_collisions(
    current: Vec3d,
    resolved: Vec3d,
    x_collision: bool,
    z_collision: bool,
    vertical_collision: bool,
    vertical_collision_below: bool,
    position: Vec3d,
    slow_falling: bool,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
) -> Vec3d {
    // restitution starts at getEntityBounciness() (0.0 for a player), or 0 while
    // sneaking.
    let mut restitution: f64 = 0.0;
    let mut vx = current.x;
    let mut vy = current.y;
    let mut vz = current.z;
    if x_collision {
        vx = -current.x * restitution;
    }
    if z_collision {
        vz = -current.z * restitution;
    }

    if vertical_collision {
        if vertical_collision_below {
            // Block at getOnPosLegacy() == getOnPos(0.2), from the post-move pos.
            let ex = mth::floor(position.x);
            let ey = mth::floor(position.y - f64::from(0.2f32));
            let ez = mth::floor(position.z);
            let block_bounciness = f64::from(view.bounce_restitution(ex, ey, ez));
            let effective_gravity = effective_gravity(
                f64::from(profile.gravity),
                current.y <= 0.0,
                slow_falling,
            );
            // `!(-current.y < effGravity)`: only a fast-enough landing bounces (a
            // resting entity does not jitter). Kept as vanilla's negated `<` rather
            // than `>=` so the NaN edge matches its float expression exactly.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            let fast_enough = !(-current.y < effective_gravity);
            restitution = if fast_enough && !suppress_bounce {
                restitution.max(block_bounciness)
            } else {
                0.0
            };
        }

        let (gravity_compensation, effective_drag) = if restitution > 0.0 {
            let portion_with_movement = resolved.y / current.y;
            let effective_gravity = effective_gravity(
                f64::from(profile.gravity),
                current.y <= 0.0,
                slow_falling,
            );
            (
                portion_with_movement * effective_gravity,
                mth::lerp_f64(portion_with_movement, 1.0, f64::from(0.98f32)),
            )
        } else {
            (0.0, 1.0)
        };
        vy = (gravity_compensation - current.y) * effective_drag * restitution;
    }

    Vec3d::new(vx, vy, vz)
}

/// `Mth.equal(a, b)` → `Math.abs(b - a) < 1.0E-5F`.
pub(crate) fn mth_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < f64::from(1.0e-5f32)
}

/// `LivingEntity.jumpFromGround()` including the sprint boost.
fn jump_from_ground(state: &mut PlayerState, view: &dyn CollisionView, profile: &PhysicsProfile) {
    // getJumpPower(): JUMP_STRENGTH * multiplier(1) * getBlockJumpFactor() + getJumpBoostPower().
    // The block-jump-factor product and the boost are separate terms in one
    // float expression; honey reduces the former (0.5), Jump Boost adds the latter.
    let block_jump_factor = block_jump_factor(state.position, view);
    let jump_power =
        profile.jump_power * block_jump_factor + jump_boost_power(state.effects.jump_boost);
    if jump_power <= 1.0e-5 {
        return;
    }
    let v = state.velocity;
    state.velocity = Vec3d::new(v.x, f64::from(jump_power).max(v.y), v.z);
    if state.sprinting {
        let angle = state.yaw * (core::f32::consts::PI / 180.0);
        let boost = profile.sprint_jump_boost;
        state.velocity = state.velocity.add(Vec3d::new(
            f64::from(-mth::sin(f64::from(angle))) * boost,
            0.0,
            f64::from(mth::cos(f64::from(angle))) * boost,
        ));
    }
}

/// `LivingEntity.getJumpBoostPower()` — `0.1F * (amp + 1)` as a `float`, or `0`.
fn jump_boost_power(jump_boost: Option<u32>) -> f32 {
    match jump_boost {
        Some(amp) => 0.1f32 * (amp as f32 + 1.0f32),
        None => 0.0f32,
    }
}

/// `Entity.getBlockJumpFactor()`: the jump factor of the block at the feet, or
/// the block below when the feet block is neutral (`== 1.0`). Honey is `0.5`.
fn block_jump_factor(position: Vec3d, view: &dyn CollisionView) -> f32 {
    let here_x = mth::floor(position.x);
    let here_y = mth::floor(position.y);
    let here_z = mth::floor(position.z);
    let here = view.jump_factor(here_x, here_y, here_z);
    if here == 1.0 {
        let (bx, by, bz) = friction_block(position);
        view.jump_factor(bx, by, bz)
    } else {
        here
    }
}

/// Advances the player by exactly one tick of on-land (non-fluid) movement.
///
/// Fluid, ladder, and elytra handling live in dedicated entry points; this is
/// the common walking/sprinting/jumping/falling path that dominates real play.
pub fn tick_air(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    // --- aiStep prologue: velocity snap-to-zero -------------------------------
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
    let mut v = state.velocity;
    let mut dx = v.x;
    let mut dy = v.y;
    let mut dz = v.z;
    if v.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if v.y.abs() < 0.003 {
        dy = 0.0;
    }
    v = Vec3d::new(dx, dy, dz);
    state.velocity = v;

    // --- input transformation (client-side) -----------------------------------
    state.sprinting = input.sprint;
    let (xxa, zza) = modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.sneak,
        profile.sneaking_speed,
    );

    // --- jump -----------------------------------------------------------------
    if input.jump && state.on_ground && state.no_jump_delay == 0 {
        jump_from_ground(state, view, profile);
        state.no_jump_delay = 10;
    } else if !input.jump {
        state.no_jump_delay = 0;
    }

    // --- travelInAir ----------------------------------------------------------
    let block_friction = if state.on_ground {
        let (fx, fy, fz) = friction_block(state.position);
        mth::compute_modified_friction(view.friction(fx, fy, fz), profile.friction_modifier)
    } else {
        1.0
    };

    // handleRelativeFrictionAndCalculateMovement
    let speed = friction_influenced_speed(profile, state, block_friction);
    let accel = input_vector(xxa, zza, speed, state.yaw);
    state.velocity = state.velocity.add(accel);

    // Ladder/vine handling: clamp horizontal + downward speed before the move,
    // and force a steady climb-up after it if pushing into (or jumping against)
    // the climbable. `onClimbable` tests the block at the feet block position.
    let climbing = view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    );
    if climbing {
        state.velocity = handle_on_climbable(state.velocity, input.sneak);
    }
    do_move(state, view, profile, input.sneak);
    let mut movement = state.velocity;
    if (state.horizontal_collision || input.jump) && climbing {
        movement = Vec3d::new(movement.x, 0.2, movement.z);
    }

    // gravity on the post-move Y.
    //
    // `travelInAir` chooses one of two mutually-exclusive vertical updates:
    // Levitation *replaces* gravity with a pull toward `0.05*(amp+1)`; otherwise
    // it subtracts `getEffectiveGravity()`, which Slow Falling reduces while
    // descending. `falling` is read from the post-move Y (== `movement.y`).
    let movement_y = if let Some(amp) = state.effects.levitation {
        movement.y + (0.05 * f64::from(amp + 1) - movement.y) * 0.2
    } else {
        let falling = movement.y <= 0.0;
        movement.y
            - effective_gravity(
                f64::from(profile.gravity),
                falling,
                state.effects.slow_falling,
            )
    };

    // drag applied last, horizontal by blockFriction * 0.91, vertical by 0.98
    let air_drag = mth::compute_modified_friction(profile.air_drag, profile.air_drag_modifier);
    let friction = block_friction * air_drag;
    let vertical_friction =
        mth::compute_modified_friction(profile.vertical_air_drag, profile.air_drag_modifier);
    state.velocity = Vec3d::new(
        movement.x * f64::from(friction),
        movement_y * f64::from(vertical_friction),
        movement.z * f64::from(friction),
    );
}

/// `LivingEntity.handleOnClimbable(Vec3)` — the pre-move clamp applied while on
/// a ladder/vine.
///
/// The clamp bounds are the **`float`** literals `-0.15F`/`0.15F`, promoted to
/// `double` for `Mth.clamp(double, double, double)`. `(double)0.15F` is
/// `0.15000000596046448`, *not* `0.15`, so the widened bound is observable at
/// the last ULP — we widen through `f32` exactly like vanilla rather than
/// writing `0.15_f64`. The sneak-hold (`yd = 0` when descending) applies to
/// ladders/vines but not scaffolding.
fn handle_on_climbable(delta: Vec3d, sneaking: bool) -> Vec3d {
    let bound = f64::from(0.15f32);
    let xd = mth::clamp_f64(delta.x, -bound, bound);
    let zd = mth::clamp_f64(delta.z, -bound, bound);
    let mut yd = delta.y.max(-bound);
    if yd < 0.0 && sneaking {
        yd = 0.0;
    }
    Vec3d::new(xd, yd, zd)
}
///
/// Vanilla derives this from `updateFluidHeightAndDoFluidPushing` over the whole
/// (deflated) AABB and a per-block fluid *height*. We approximate it as "the
/// block cells the bounding box occupies contain water", which is exact for a
/// player fully inside a water volume — the case this water path is built for.
#[must_use]
fn is_in_water(state: &PlayerState, view: &dyn CollisionView, profile: &PhysicsProfile) -> bool {
    let bb = state.bounding_box(profile);
    // Deflate like vanilla (`0.001`) before sampling, then test each occupied
    // block cell for water.
    let min_x = mth::floor(bb.min_x + 0.001);
    let max_x = mth::floor(bb.max_x - 0.001);
    let min_y = mth::floor(bb.min_y + 0.001);
    let max_y = mth::floor(bb.max_y - 0.001);
    let min_z = mth::floor(bb.min_z + 0.001);
    let max_z = mth::floor(bb.max_z - 0.001);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if view.is_water(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}

/// `LivingEntity.getEffectiveGravity()` — Slow Falling reduces gravity to
/// `min(gravity, 0.01)` while descending; otherwise it is the base gravity.
///
/// The `0.01` is a `double` literal and `min` uses the pre-move delta-Y sign
/// (`getDeltaMovement().y <= 0.0`). In fluids this is what makes the `-0.003`
/// clamp reachable: it shifts `baseGravity/16` off the `0.005` that makes the
/// clamp dead at default gravity.
#[must_use]
fn effective_gravity(base_gravity: f64, falling: bool, slow_falling: bool) -> f64 {
    if falling && slow_falling {
        base_gravity.min(0.01)
    } else {
        base_gravity
    }
}

/// `Entity.isInLava()` — coarse deep-lava analogue of [`is_in_water`].
fn is_in_lava(state: &PlayerState, view: &dyn CollisionView, profile: &PhysicsProfile) -> bool {
    let bb = state.bounding_box(profile);
    let min_x = mth::floor(bb.min_x + 0.001);
    let max_x = mth::floor(bb.max_x - 0.001);
    let min_y = mth::floor(bb.min_y + 0.001);
    let max_y = mth::floor(bb.max_y - 0.001);
    let min_z = mth::floor(bb.min_z + 0.001);
    let max_z = mth::floor(bb.max_z - 0.001);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if view.is_lava(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}
///
/// Applies the buoyant slow-descent: normally `y - baseGravity/16`, but when
/// already sinking near terminal it clamps to `-0.003` (the famous slow-sink).
/// When sprinting, gravity is not applied at all (vanilla returns `movement`).
#[must_use]
fn fluid_falling_adjusted_movement(
    base_gravity: f64,
    is_falling: bool,
    sprinting: bool,
    movement: Vec3d,
) -> Vec3d {
    if base_gravity != 0.0 && !sprinting {
        let gravity_step = base_gravity / 16.0;
        let yd = if is_falling
            && (movement.y - 0.005).abs() >= 0.003
            && (movement.y - gravity_step).abs() < 0.003
        {
            -0.003
        } else {
            movement.y - gravity_step
        };
        Vec3d::new(movement.x, yd, movement.z)
    } else {
        movement
    }
}

/// One tick of in-water movement (`travel` → `travelInFluid` → `travelInWater`).
///
/// Covers the common submerged cases: sinking, swimming under input, and holding
/// jump to rise (`jumpInLiquid`, `+0.04`). It does **not** model the
/// partial-submersion transition, fluid-push currents, bubble columns, depth
/// strider (`WATER_MOVEMENT_EFFICIENCY`), or dolphin's grace — those need real
/// per-block fluid height, which the [`CollisionView`] hook deliberately omits.
pub fn tick_water(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        // Structural seam: 1.8 fluid handling is a different branch (no swimming
        // pose, no falling-adjusted clamp). Not modelled yet — fail loudly rather
        // than run modern water math for a 1.8 profile.
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // --- baseTick: fluid current push (`updateFluidInteraction`) ---------------
    // Vanilla applies the flow current in `baseTick`, before `aiStep`/`travel`
    // within the same tick, so it lands here (ahead of the snap-to-zero prologue)
    // and its result is what the prologue and the accel step then see.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Water,
        profile.water_push_scale,
        profile,
    );
    // --- aiStep prologue: velocity snap-to-zero (identical to the air path) ----
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
    let mut dx = state.velocity.x;
    let mut dy = state.velocity.y;
    let mut dz = state.velocity.z;
    if state.velocity.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if state.velocity.y.abs() < 0.003 {
        dy = 0.0;
    }
    state.velocity = Vec3d::new(dx, dy, dz);

    state.sprinting = input.sprint;
    let (xxa, zza) = modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.sneak,
        profile.sneaking_speed,
    );

    // --- water jump: hold jump to rise (`jumpInLiquid`, +0.04) -----------------
    if input.jump {
        state.velocity = state.velocity.add(Vec3d::new(0.0, f64::from(0.04f32), 0.0));
    } else {
        state.no_jump_delay = 0;
    }

    // --- travelInFluid / travelInWater ----------------------------------------
    let is_falling = state.velocity.y <= 0.0;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );

    let slow_down = if state.effects.dolphins_grace {
        // Dolphin's Grace overrides the sprint/walk slow-down entirely.
        0.96f32
    } else if state.sprinting {
        profile.water_sprint_slow_down
    } else {
        profile.water_slow_down
    };
    // waterWalker (depth strider) == 0 in the common case, so its slowDown/speed
    // adjustment block is skipped.
    let speed = profile.fluid_input_speed;

    let accel = input_vector(xxa, zza, speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak);

    let movement = state.velocity.multiply_each(
        f64::from(slow_down),
        f64::from(0.8f32),
        f64::from(slow_down),
    );
    state.velocity =
        fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
}

/// One tick of movement while submerged in **deep lava** (`travelInLava`).
///
/// Lava is a *different branch* from water, not a retuned one: input speed is a
/// flat `0.02F`, the post-move velocity is scaled by `0.5` (deep) rather than by
/// the water slow-down, and gravity is applied as an extra `-baseGravity/4`
/// term. The shallow-lava branch (`multiply(0.5, 0.8, 0.5)` +
/// `getFluidFallingAdjustedMovement`) needs the fluid's height, which the coarse
/// [`CollisionView`] hook does not expose, so this models the fully-submerged
/// case — consistent with [`tick_water`]'s scope.
pub fn tick_lava(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // baseTick fluid current push (see `tick_water`); lava uses its own scale.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Lava,
        profile.lava_push_scale,
        profile,
    );
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
    let mut dx = state.velocity.x;
    let mut dy = state.velocity.y;
    let mut dz = state.velocity.z;
    if state.velocity.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if state.velocity.y.abs() < 0.003 {
        dy = 0.0;
    }
    state.velocity = Vec3d::new(dx, dy, dz);

    state.sprinting = input.sprint;
    let (xxa, zza) = modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.sneak,
        profile.sneaking_speed,
    );

    // aiStep jumpInLiquid (+0.04) applies in lava too.
    if input.jump {
        state.velocity = state.velocity.add(Vec3d::new(0.0, f64::from(0.04f32), 0.0));
    } else {
        state.no_jump_delay = 0;
    }

    let base_gravity = f64::from(profile.gravity);

    // moveRelative(0.02) → move → scale(0.5) [deep] → -baseGravity/4.
    let accel = input_vector(xxa, zza, profile.fluid_input_speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak);
    state.velocity = state.velocity.scale(0.5);
    if base_gravity != 0.0 {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -base_gravity / 4.0, 0.0));
    }
}

/// Advances the player by one tick, dispatching to the water, lava, or air path
/// exactly as `travel` → `shouldTravelInFluid`/`travelInFluid` does: water takes
/// precedence over lava, and both over air.
/// `Entity.calculateViewVector(xRot, yRot)` — the look-direction unit vector.
///
/// The trig comes from the **`Mth` LUT** (`float`), and each component is a
/// `float` product widened to `double` by the `Vec3` constructor. The
/// degrees→radians factor is `(float)(Math.PI / 180.0)` — the division happens
/// in `double` *then* narrows to `float`, which is a different bit pattern from
/// the input path's `(float)Math.PI / 180.0F`; we mirror the exact form.
fn calculate_view_vector(pitch: f32, yaw: f32) -> Vec3d {
    let deg_to_rad = (core::f64::consts::PI / 180.0) as f32;
    let real_x_rot = pitch * deg_to_rad;
    let real_y_rot = -yaw * deg_to_rad;
    let y_cos = mth::cos(f64::from(real_y_rot));
    let y_sin = mth::sin(f64::from(real_y_rot));
    let x_cos = mth::cos(f64::from(real_x_rot));
    let x_sin = mth::sin(f64::from(real_x_rot));
    Vec3d::new(
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// `LivingEntity.updateFallFlyingMovement(Vec3)` — the elytra glide update.
///
/// Preserves vanilla's exact operation order and its two distinct trig sources:
/// the look vector uses the `Mth` LUT (`float`), while `liftForce` and the
/// nose-up lift use `java.lang.Math` (`double`) `cos`/`sin`. The final drag is
/// `multiply(0.99F, 0.98F, 0.99F)` with each `float` widened to `double`.
fn update_fall_flying_movement(
    state: &PlayerState,
    profile: &PhysicsProfile,
    movement: Vec3d,
) -> Vec3d {
    let look = calculate_view_vector(state.pitch, state.yaw);
    let lean_angle = state.pitch * ((core::f64::consts::PI / 180.0) as f32);
    let look_hor_len = (look.x * look.x + look.z * look.z).sqrt();
    let move_hor_len = (movement.x * movement.x + movement.z * movement.z).sqrt();
    let gravity = effective_gravity(
        f64::from(profile.gravity),
        movement.y <= 0.0,
        state.effects.slow_falling,
    );
    // liftForce = Mth.square(Math.cos(leanAngle)) — real double cos, not the LUT.
    let cos_lean = f64::from(lean_angle).cos();
    let lift_force = mth::square_f64(cos_lean);

    let mut mx = movement.x;
    let mut my = movement.y;
    let mut mz = movement.z;

    my += gravity * (-1.0 + lift_force * 0.75);

    if my < 0.0 && look_hor_len > 0.0 {
        let convert = my * -0.1 * lift_force;
        mx += look.x * convert / look_hor_len;
        my += convert;
        mz += look.z * convert / look_hor_len;
    }

    if lean_angle < 0.0 && look_hor_len > 0.0 {
        // -Mth.sin(leanAngle): the LUT sine again, negated and widened.
        let convert = move_hor_len * f64::from(-mth::sin(f64::from(lean_angle))) * 0.04;
        mx += -look.x * convert / look_hor_len;
        my += convert * 3.2;
        mz += -look.z * convert / look_hor_len;
    }

    if look_hor_len > 0.0 {
        mx += (look.x / look_hor_len * move_hor_len - mx) * 0.1;
        mz += (look.z / look_hor_len * move_hor_len - mz) * 0.1;
    }

    Vec3d::new(
        mx * f64::from(0.99f32),
        my * f64::from(0.98f32),
        mz * f64::from(0.99f32),
    )
}

/// `LivingEntity.travelFallFlying` (client path) — one tick of elytra flight.
///
/// Direction comes purely from the look angle; WASD `input` is ignored while
/// gliding (except that landing on a climbable hands control back to
/// [`tick_air`] and ends the glide, mirroring vanilla). The `aiStep` small-
/// velocity collapse runs first, exactly as for the other travel modes.
pub fn tick_elytra(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    // onClimbable: vanilla stops fall-flying and reverts to the walking path.
    if view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    ) {
        state.fall_flying = false;
        tick_air(state, input, view, profile);
        return;
    }

    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }

    // aiStep velocity collapse (players use the horizontal-distance test).
    let v = state.velocity;
    let mut dx = v.x;
    let mut dy = v.y;
    let mut dz = v.z;
    if v.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if v.y.abs() < 0.003 {
        dy = 0.0;
    }
    let collapsed = Vec3d::new(dx, dy, dz);

    state.velocity = update_fall_flying_movement(state, profile, collapsed);
    do_move(state, view, profile, false);
}

/// `Entity.checkInsideBlocks` → `Block.entityInside` → `makeStuckInBlock`: after
/// the tick's movement, record the stuck-speed multiplier of whatever block the
/// (deflated) bounding box is now inside, for the *next* move to consume. This is
/// what produces the observable one-tick lag between entering a cobweb and being
/// grabbed by it.
///
/// Vanilla walks the swept movement segment with the target bounding box deflated
/// by `1.0E-5`; we sample that resting overlap at the final position, which is
/// exact for the stationary/slow case (standing in, or walking into, a web) — the
/// same coarse approximation the water/lava hooks document, and the common case
/// for cobweb (mineshaft corridors) and powder snow. Blocks are *assigned* in
/// vanilla, not accumulated, so the last intersected block wins; for the uniform
/// volumes these blocks form, iteration order is immaterial.
fn update_stuck_multiplier(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    let bb = state.bounding_box(profile);
    let min_x = mth::floor(bb.min_x + 1.0e-5);
    let max_x = mth::floor(bb.max_x - 1.0e-5);
    let min_y = mth::floor(bb.min_y + 1.0e-5);
    let max_y = mth::floor(bb.max_y - 1.0e-5);
    let min_z = mth::floor(bb.min_z + 1.0e-5);
    let max_z = mth::floor(bb.max_z - 1.0e-5);
    let mut found = Vec3d::ZERO;
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if let Some(m) = view.stuck_multiplier(x, y, z) {
                    found = m;
                }
            }
        }
    }
    state.stuck_speed_multiplier = found;
}

/// Advances the player one tick: dispatches to the fluid/elytra/air travel path
/// exactly as vanilla's `LivingEntity.travel()`, then records any stuck-in-block
/// multiplier for the next tick to consume (`Entity.checkInsideBlocks`).
pub fn tick(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if is_in_water(state, view, profile) {
        tick_water(state, input, view, profile);
    } else if is_in_lava(state, view, profile) {
        tick_lava(state, input, view, profile);
    } else if state.fall_flying {
        tick_elytra(state, input, view, profile);
    } else {
        tick_air(state, input, view, profile);
    }
    update_stuck_multiplier(state, view, profile);
}

/// `LivingEntity.getFrictionInfluencedSpeed(blockFriction)`.
/// The player's effective walk speed for `getFrictionInfluencedSpeed`, i.e.
/// vanilla's `getSpeed()`. Uses the injected attribute value when present
/// (sprint + Speed/Slowness already folded in by the entity layer), reproducing
/// the `(float)` cast; otherwise computes the standalone base+sprint value.
fn effective_speed(profile: &PhysicsProfile, state: &PlayerState) -> f32 {
    match state.movement_speed {
        Some(v) => v as f32,
        None => player_speed(profile, state.sprinting),
    }
}

fn friction_influenced_speed(
    profile: &PhysicsProfile,
    state: &PlayerState,
    block_friction: f32,
) -> f32 {
    if state.on_ground {
        let speed = effective_speed(profile, state);
        if block_friction > 0.6 {
            let cubed = block_friction * block_friction * block_friction;
            speed * (profile.ground_accel / cubed)
        } else {
            speed
        }
    } else {
        // getFlyingSpeed(): 0.02 for a player not riding.
        profile.flying_speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_speed_walk_and_sprint_bits() {
        let p = PhysicsProfile::mc_1_21();
        assert_eq!(player_speed(&p, false), 0.1f32);
        // Sprint speed derived via the attribute math: 0.13000001f (0x3e051eb9).
        assert_eq!(player_speed(&p, true).to_bits(), 0x3e05_1eb9);
    }

    #[test]
    fn friction_influenced_speed_default_ground_is_getspeed() {
        // On default 0.6 friction, the 0.216.../f^3 factor is exactly 1.0f.
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        let bf = mth::compute_modified_friction(0.6, 1.0);
        assert_eq!(friction_influenced_speed(&p, &s, bf), 0.1f32);
    }

    #[test]
    fn injected_attribute_speed_replaces_not_stacks_with_sprint() {
        // Reconciled attribute seam. The entity layer's MOVEMENT_SPEED value
        // already folds sprint in, so a Some(v) override must be used verbatim
        // (as f32) even while `sprinting` is true — never re-multiplied here.
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);

        // base 0.1 + sprint (AddMultipliedTotal 0.3) + Speed I (AddMultipliedTotal
        // 0.2), all one class => 0.1 * (1+0.3) * (1+0.2), per calculateValue().
        let attr = 0.1_f64 * (1.0 + 0.3) * (1.0 + 0.2);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0).with_movement_speed(attr);
        s.on_ground = true;
        s.sprinting = true; // must be ignored while the override is present

        assert_eq!(friction_influenced_speed(&p, &s, bf), attr as f32);
        // Guard against the folding failure: it is NOT the sprint-stacked value.
        assert_ne!(friction_influenced_speed(&p, &s, bf), (attr * 1.3) as f32);
    }

    #[test]
    fn no_override_falls_back_to_standalone_sprint() {
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        s.sprinting = true;
        assert_eq!(
            friction_influenced_speed(&p, &s, bf).to_bits(),
            player_speed(&p, true).to_bits()
        );
    }

    struct WaterEverywhere;
    impl CollisionView for WaterEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    struct LavaEverywhere;
    impl CollisionView for LavaEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_lava(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    #[test]
    fn lava_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): the deep-lava step is
        // `vy = 0.5*vy - baseGravity/4`, so terminal solves `0.5*vy = -0.02`,
        // i.e. vy = -0.04. Different from water's -0.025 — a different branch.
        let p = PhysicsProfile::mc_1_21();
        let view = LavaEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.04)).abs() < 1.0e-9,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn levitation_makes_player_rise() {
        // First-principles anchor: Levitation replaces gravity with a pull toward
        // 0.05*(amp+1) > 0, so with no other input the player must gain height.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0).with_effects(StatusEffects {
            levitation: Some(0),
            ..StatusEffects::default()
        });
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &Air, &p);
        }
        assert!(s.position.y > 100.5, "levitation y = {}", s.position.y);
        assert!(s.velocity.y > 0.0, "levitation vy = {}", s.velocity.y);
    }

    #[test]
    fn slow_falling_revives_the_dead_water_clamp() {
        // The satisfying test: at default gravity the -0.003 fluid clamp is dead
        // (proven by `fluid_clamp_is_dead_at_default_gravity`). Slow Falling drops
        // effective gravity to 0.01 while descending, moving baseGravity/16 off
        // 0.005 so the clamp becomes reachable. Confirm effective_gravity and that
        // the clamp fires at least once during a slow-falling submerged sink.
        assert_eq!(effective_gravity(0.08, true, true), 0.01);
        assert_eq!(effective_gravity(0.08, false, true), 0.08); // not falling: base
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0).with_effects(StatusEffects {
            slow_falling: true,
            ..StatusEffects::default()
        });
        let mut clamp_fired = false;
        for _ in 0..120 {
            tick(&mut s, MovementInput::NONE, &view, &p);
            if s.velocity.y == -0.003 {
                clamp_fired = true;
            }
        }
        assert!(clamp_fired, "slow-falling never revived the -0.003 clamp");
    }

    #[test]
    fn water_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): steady state solves
        // vy = 0.8*vy - baseGravity/16, i.e. vy = -0.005 / 0.2 = -0.025.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.025)).abs() < 1.0e-6,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn fluid_clamp_is_dead_at_default_gravity() {
        // With baseGravity/16 == 0.005, the two clamp conditions
        // (|y-0.005| >= 0.003 AND |y-0.005| < 0.003) are mutually exclusive, so
        // the -0.003 slow-sink never fires. Verify it degrades to y - 0.005.
        let g = f64::from(PhysicsProfile::mc_1_21().gravity);
        for &y in &[-0.01, -0.005, 0.0, 0.005, 0.02] {
            let out = fluid_falling_adjusted_movement(g, true, false, Vec3d::new(0.0, y, 0.0));
            assert_eq!(out.y, y - g / 16.0, "y = {y}");
        }
    }

    #[test]
    fn fluid_clamp_fires_under_reduced_gravity() {
        // Under slow-falling-style reduced gravity, baseGravity/16 != 0.005, so
        // the clamp can engage near terminal. Pick movement.y so both hold.
        let base = 0.01_f64; // baseGravity/16 = 0.000625
        let y = 0.001_f64; // |y-0.005|=0.004 >= 0.003, |y-0.000625|=0.000375 < 0.003
        let out = fluid_falling_adjusted_movement(base, true, false, Vec3d::new(0.0, y, 0.0));
        assert_eq!(out.y, -0.003);
    }

    #[test]
    fn profile_selects_structural_input_model_per_version() {
        // The 1.8-vs-modern difference is a *branch*, not a scalar: the profiles
        // must declare different `InputModel`s even though their numbers match.
        assert_eq!(
            PhysicsProfile::mc_1_21().input_model,
            InputModel::UnitSquareProjection
        );
        assert_eq!(
            PhysicsProfile::mc_1_8().input_model,
            InputModel::LegacyMoveFlying
        );
        assert_eq!(PhysicsProfile::mc_1_21().fluid_model, FluidModel::Modern);
        assert_eq!(PhysicsProfile::mc_1_8().fluid_model, FluidModel::Legacy1_8);
    }

    #[test]
    fn modern_input_path_is_selected_and_pure() {
        // The validated modern arm must be reachable through the enum dispatch and
        // produce the same result as the underlying unit-square function.
        let via_enum = modify_input(InputModel::UnitSquareProjection, 1.0, 1.0, false, 0.3);
        let direct = modify_input_unit_square(1.0, 1.0, false, 0.3);
        assert_eq!(via_enum.0.to_bits(), direct.0.to_bits());
        assert_eq!(via_enum.1.to_bits(), direct.1.to_bits());
    }

    #[test]
    #[should_panic(expected = "1.8 moveFlying input pipeline")]
    fn legacy_input_fails_loudly_not_silently() {
        // The whole point of the seam: a 1.8 profile must NOT silently run modern
        // math. Until the 1.8 pipeline is modelled and JVM-validated, it panics.
        let _ = modify_input(InputModel::LegacyMoveFlying, 1.0, 1.0, false, 0.3);
    }

    #[test]
    #[should_panic(expected = "1.8 fluid movement")]
    fn legacy_fluid_fails_loudly_not_silently() {
        let p = PhysicsProfile::mc_1_8();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick_water(&mut s, MovementInput::NONE, &view, &p);
    }

    #[test]
    fn jump_boost_power_is_tenth_per_level_as_float() {
        // getJumpBoostPower() = 0.1F*(amp+1) in float. Amp 0 (Jump Boost I) => 0.1F;
        // amp 1 (Jump Boost II) => 0.2F. The float literal matters (0.1 is inexact).
        assert_eq!(jump_boost_power(None), 0.0f32);
        assert_eq!(jump_boost_power(Some(0)).to_bits(), 0.1f32.to_bits());
        assert_eq!(
            jump_boost_power(Some(1)).to_bits(),
            (0.1f32 * 2.0).to_bits()
        );
    }

    #[test]
    fn slime_reverses_downward_velocity_and_sneak_cancels_it() {
        // First-principles anchor (not the oracle): a full slime cube has
        // bounce_restitution 1.0, so a player landing on it leaves with upward
        // velocity; the same fall while sneaking rests instead (vy path -> ~0).
        struct SlimeFloor;
        impl CollisionView for SlimeFloor {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if y == 0 {
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
            fn bounce_restitution(&self, _x: i32, y: i32, _z: i32) -> f32 {
                if y == 0 { 1.0 } else { 0.0 }
            }
        }
        let p = PhysicsProfile::mc_1_21();

        let mut bounced = false;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &SlimeFloor, &p);
            if s.velocity.y > 0.05 {
                bounced = true;
                break;
            }
        }
        assert!(bounced, "player never bounced off slime");

        let mut peak: f64 = 1.0;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        let sneak = MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: true,
            sprint: false,
        };
        for _ in 0..80 {
            tick(&mut s, sneak, &SlimeFloor, &p);
            assert!(
                s.velocity.y <= 0.05,
                "sneak failed to cancel the bounce: vy = {}",
                s.velocity.y
            );
            peak = peak.max(s.position.y);
        }
        // Never launched back above the drop height once landed.
        assert!(peak <= 6.0, "sneaking player gained height: {peak}");
    }
}
