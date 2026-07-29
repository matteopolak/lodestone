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

use crate::collision::{CollisionView, no_collision};
use crate::entity::{
    AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity, travel_in_air,
};
use crate::fluid::apply_fluid_push;
use crate::fluid_state::{FluidState, compute_fluid_state};
use crate::geometry::{Aabb, Vec3d};
use crate::mth::{self};
use crate::profile::{FluidModel, InputModel, PhysicsProfile};

/// `Avatar.DEFAULT_EYE_HEIGHT` — the player standing eye offset (`1.62F`), used
/// as the default pose [`eye height`](PlayerState::eye_height). The swimming /
/// crawling / gliding pose lowers it to `0.4`, crouching to `1.27`; a driver
/// modelling those poses sets [`PlayerState::eye_height`] accordingly.
pub const DEFAULT_EYE_HEIGHT: f32 = 1.62;

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
    /// Pose **eye height** (`getEyeHeight`), the offset from feet to eye used by
    /// [`crate::compute_fluid_state`] to decide eye-in-fluid. Standing is
    /// `1.62` (`Avatar.DEFAULT_EYE_HEIGHT`); the swimming/crawling/gliding pose is
    /// `0.4`, crouching `1.27`. It is an **input**: the pose layer sets it (see
    /// [`Self::with_eye_height`]) so the eye check tracks the pose the box does.
    /// `getEyeY()` widens it to `double` and adds `position.y`, reproduced exactly.
    pub eye_height: f32,
    /// **Output.** `isEyeInFluid(WATER)` from the last [`tick`], i.e. the eye
    /// block-column held water spanning the eye Y. Combine with in-water via
    /// [`Self::eye_in_water`] + fluid presence for `isUnderWater`; the shell reads
    /// this for submerged fog, the underwater overlay, and `ambient.underwater.*`.
    pub eye_in_water: bool,
    /// **Output.** `isEyeInFluid(LAVA)` from the last [`tick`].
    pub eye_in_lava: bool,
    /// **Output.** `Entity.isSwimming()` after the last [`tick`]'s
    /// `updateSwimming`: sprint-swimming, entered when sprinting while submerged in
    /// water and sustained while sprinting in water.
    ///
    /// The server derives this **itself**, in its own `Entity.baseTick` →
    /// `updateSwimming`, from `isSprinting()` and its own collision — there is no
    /// swimming bit anywhere on the wire (`Input` is seven booleans:
    /// forward/backward/left/right/jump/shift/sprint, `Input.java`). What a driver
    /// must transmit is the **sprint** edge, via
    /// `ServerboundPlayerCommandPacket(START_SPRINTING/STOP_SPRINTING)`
    /// (`LocalPlayer.sendIsSprintingIfNeeded`, `LocalPlayer.java:303-312`) — the
    /// `Input` packet's `sprint` flag is stored as `lastClientInput` and does *not*
    /// call `setSprinting` (`ServerGamePacketListenerImpl.java:424` vs `:1719`).
    /// Send only sprint and the server's swim pose follows; send the input packet
    /// alone and it never does.
    pub swimming: bool,
    /// `Attributes.WATER_MOVEMENT_EFFICIENCY` — the Depth Strider attribute, as an
    /// **input** from the equipment layer (like [`Self::movement_speed`]).
    ///
    /// Vanilla's `travelInWater` reads `getAttributeValue(WATER_MOVEMENT_EFFICIENCY)`
    /// (`LivingEntity.java:2509`), halves it when airborne, and then uses it to lerp
    /// the horizontal slow-down toward `0.546_000_06` and the input speed toward
    /// `getSpeed()`. Depth Strider contributes `0.33` per level via its enchantment
    /// effect, so a level-III boot is `0.99`.
    ///
    /// **Default `0.0`, because no caller in this repo can reach the value yet — but
    /// it is closer than it looks, and the gap is a missing accessor, not missing
    /// data.** `lodestone-entity`'s attribute table already knows
    /// `water_movement_efficiency` (default `0.0`, range `0..1`), `v770` has the
    /// attribute type, and `lodestone-client` folds
    /// `ClientEvent::EntityAttributesUpdated` into per-entity
    /// `EntityAttributeSnapshot`s. What is absent is (a) any route from the shell to
    /// the **local player's** attribute set — the shell's `EntitySnapshot` drops the
    /// `attributes` field, and there is no `NetClient` accessor for it — and (b) the
    /// three-stage `calculateValue()` fold from base + modifiers to an effective
    /// value, which the shell also does not do for `MOVEMENT_SPEED` (it recomputes
    /// that itself instead).
    ///
    /// The arithmetic lives in [`tick_water`] so that the value is the *only* missing
    /// piece rather than the whole branch, and so nothing can silently substitute a
    /// plausible number for it.
    pub water_movement_efficiency: f32,
    /// `Entity.fallDistance` (`Entity.java:245` — a `double`, not a `float`, since
    /// 26.2), as an **input** from the driver, exactly like
    /// [`Self::water_movement_efficiency`].
    ///
    /// **Why it is suddenly load-bearing.** [`move_entity`] is documented as
    /// modelling only "the parts of `Entity.move` that affect an entity's reported
    /// position", and fall distance used to be squarely outside that: it drives
    /// fall *damage*, which the server owns. `Player.maybeBackOffFromEdge` changes
    /// that — `isAboveGround` consults `fallDistance`, and the back-off moves you,
    /// so fall distance is now a position input.
    ///
    /// **This crate does not maintain it, deliberately.** Vanilla's accounting is
    /// spread over at least six sites: the `-= (float) ya` accumulation and the
    /// grounded reset in `Entity.checkFallDamage` (`Entity.java:1564-1582`, and note
    /// the `float` cast of a `double` argument into a `double` field), the
    /// `movementLength >= 1.0` clip-through reset inside `move` itself
    /// (`Entity.java:747-754`), the `*= 0.5` lava halving in `baseTick`
    /// (`Entity.java:555-557`), `checkFallDistanceAccumulation`'s clamp to `1.0`
    /// (`Entity.java:2904-2908`), `LivingEntity`'s override (`LivingEntity.java:363`),
    /// and the water/vehicle/teleport resets. A partial model feeding a
    /// position-affecting gate is worse than an explicit input, so this is an
    /// input.
    ///
    /// **The default `0.0` is exact for the grounded case and errs in a known
    /// direction otherwise**: `Player.isAboveGround`'s airborne branch probes
    /// `maxDownStep - fallDistance`, so a `0.0` stand-in probes the *full* step
    /// height, a strictly weaker `canFallAtLeast`, and the gate opens *more* often
    /// than vanilla's rather than less. Every scenario that
    /// motivated the rule (bridging, sneak-placing, walking a ledge) is grounded.
    pub fall_distance: f64,
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
            eye_height: DEFAULT_EYE_HEIGHT,
            eye_in_water: false,
            eye_in_lava: false,
            swimming: false,
            water_movement_efficiency: 0.0,
            fall_distance: 0.0,
        }
    }

    /// Returns a copy of this state with [`Entity.fallDistance`](Self::fall_distance)
    /// set. Only the airborne branch of `Player.isAboveGround` reads it.
    #[must_use]
    pub fn with_fall_distance(mut self, value: f64) -> Self {
        self.fall_distance = value;
        self
    }

    /// Returns a copy of this state with the
    /// [`WATER_MOVEMENT_EFFICIENCY`](Self::water_movement_efficiency) attribute
    /// value (Depth Strider) injected.
    #[must_use]
    pub fn with_water_movement_efficiency(mut self, value: f32) -> Self {
        self.water_movement_efficiency = value;
        self
    }

    /// Returns a copy of this state with the pose [`eye height`](Self::eye_height)
    /// set (e.g. `0.4` for the swimming/crawling pose, `1.62` standing).
    #[must_use]
    pub fn with_eye_height(mut self, eye_height: f32) -> Self {
        self.eye_height = eye_height;
        self
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

/// `Entity.getInputVector(relativeInput, speed, yRot)` — the yaw-rotated,
/// speed-scaled acceleration that `moveRelative` adds to velocity.
///
/// This is entity-agnostic and public so a mob loop can produce its per-tick
/// velocity *contribution* the same way the player pipeline does — vanilla drives
/// both players and mobs through `moveRelative`, and re-deriving this yaw rotation
/// by hand is a divergence surface (the `Mth.sin`/`Mth.cos` LUT and the exact
/// `f32`/`f64` widths must match). Feed the result (plus gravity) into
/// [`crate::entity::move_entity`] as `motion.velocity`.
///
/// Scope: `strafe`/`forward` are the horizontal relative-movement axes (vanilla's
/// `xxa`/`zza`); the vertical relative component is fixed at `0`, which covers
/// walking mobs. A fully-general `moveRelative(Vec3)` with a vertical input (a
/// swimming or flying mob) can be added when one is wired.
pub fn input_vector(strafe: f32, forward: f32, speed: f32, yaw: f32) -> Vec3d {
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
///
/// `suppress_bounce` (`isSuppressingBounce()`) and `staying_on_ground_surface`
/// (`isStayingOnGroundSurface()`) are **both** `isShiftKeyDown()` in vanilla, but
/// they are separate parameters here on purpose: they are separate virtual methods
/// serving unrelated rules, our elytra path already disagrees about the first
/// (passing `false`), and collapsing them would make the edge back-off silently
/// inherit whatever that path decided about bouncing.
fn do_move(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
    staying_on_ground_surface: bool,
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
        edge_back_off: EdgeBackOff::Player {
            staying_on_ground_surface,
            fall_distance: state.fall_distance,
        },
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
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
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
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
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

/// Which `maybeBackOffFromEdge` override the entity being moved has.
///
/// `Entity.maybeBackOffFromEdge(Vec3, MoverType)` (`Entity.java:1099-1101`) is a
/// virtual hook whose **base implementation is the identity** — `return delta`.
/// `Player` (`Player.java:880-927`) is the only override in the tree: it is the
/// sneak-at-a-ledge back-off that stops you walking off a drop while shift is
/// held. Modelling the override as a variant rather than a bare `bool` makes a
/// mob *structurally* unable to acquire player-only behaviour, which is the same
/// property vanilla gets from the class hierarchy.
///
/// [`Self::Entity`] is the [`Default`], so [`crate::MoveContext::default()`] — what
/// a mob or a dropped item passes — is inert by construction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum EdgeBackOff {
    /// `Entity.maybeBackOffFromEdge` — the base implementation, `return delta`.
    /// Every non-player entity.
    #[default]
    Entity,
    /// `Player.maybeBackOffFromEdge` — the sneak-at-a-ledge back-off.
    Player {
        /// `Player.isStayingOnGroundSurface()` (`Player.java:300-302`), which is
        /// exactly `isShiftKeyDown()` — the raw shift key, not the crouch *pose*
        /// and not `isCrouching()`.
        ///
        /// The two ends of the wire read the same boolean from different places
        /// and it matters that they agree: the client uses
        /// `LocalPlayer.isShiftKeyDown()`, overridden to return
        /// `this.input.keyPresses.shift()` directly
        /// (`client-src/.../LocalPlayer.java:674-676`), while the server reads the
        /// `SHIFT_KEY_DOWN` shared flag set from `ServerboundPlayerInputPacket`
        /// (`ServerGamePacketListenerImpl.java:427`). A driver that sneaks
        /// *locally* without sending the input packet manufactures the very
        /// disagreement this rule exists to prevent, only inverted — see
        /// `docs/edge-back-off.md`.
        staying_on_ground_surface: bool,
        /// `Entity.fallDistance` (`Entity.java:245`, a `double` since 26.2).
        ///
        /// **An input this crate does not maintain**, like
        /// [`PlayerState::water_movement_efficiency`]. It is read by exactly one
        /// place — `Player.isAboveGround`'s airborne branch — and only when
        /// `on_ground` is `false`.
        fall_distance: f64,
    },
}

/// `Player.maybeBackOffFromEdge(Vec3, MoverType)` (`Player.java:880-927`) — the
/// sneak-at-a-ledge back-off, called from inside `Entity.move` on the *candidate*
/// delta before collision resolution.
///
/// # Why this is a desync rule and not a feel rule
///
/// `ServerGamePacketListenerImpl.handleMovePlayer` replays the movement we claim
/// through `this.player.move(MoverType.PLAYER, …)`
/// (`ServerGamePacketListenerImpl.java:1134`) — and `MoverType.PLAYER` is one of
/// the two mover types this rule's own gate admits. It then compares the replay's
/// result against the position we claimed and teleports us back if
/// `movedDist > 0.0625` (`:1146-1153`), i.e. **0.25 blocks in a single packet with
/// no accumulator**. Because the intervening `yDist` clamp zeroes Y
/// unconditionally (`:1137-1139`), that comparison is **purely horizontal** — and
/// this rule modifies exactly and only the horizontal components. A client that
/// sneaks near a ledge without modelling it claims a position past where the
/// server's own replay puts it, and gets corrected.
///
/// # The gate, transcribed
///
/// ```text
/// !this.abilities.flying
///   && !(delta.y > 0.0)
///   && (moverType == MoverType.SELF || moverType == MoverType.PLAYER)
///   && this.isStayingOnGroundSurface()
///   && this.isAboveGround(maxDownStep)
/// ```
///
/// Two of the five are satisfied by construction here and so are not parameters:
///
/// * **`!abilities.flying`** — this crate does not model creative flight at all
///   (`travelInAir` applies gravity unconditionally), so a flying driver does not
///   route through [`tick`]. This is the same standing argument
///   [`update_swimming`] makes for `Player.updateSwimming`'s flying override.
/// * **the mover type** — [`crate::move_entity`] *is* `move(MoverType.SELF, …)`.
///   The excluded types are `PISTON` (which this crate has no equivalent of) and
///   the mover types used for vehicle/knockback pushes.
///
/// `maxDownStep` is `this.maxUpStep()`, i.e. the resolved **`STEP_HEIGHT`
/// attribute** (`LivingEntity.java:3975`), whose `RangedAttribute` default is
/// `0.6` (`Attributes.java:98-100`) — *not* a literal. It arrives as
/// [`EntityDimensions::step_height`], which is documented as the post-modifier
/// value, so a step-height modifier is honoured for free. Hard-coding `0.6` would
/// agree today and silently diverge the moment anything modifies the attribute.
///
/// # The stepping loop
///
/// Vanilla does **not** clamp the delta once; it walks it toward zero in `0.05`
/// increments, re-probing after each step, and stops at the first candidate the
/// probe says will not fall. There are **three** loops, and X and Z are
/// *independent first, then joint*:
///
/// 1. X alone, probing `canFallAtLeast(deltaX, 0.0, …)`.
/// 2. Z alone, probing `canFallAtLeast(0.0, deltaZ, …)`, starting from the
///    original `delta.z` (**not** from anything loop 1 produced).
/// 3. X and Z **together**, probing `canFallAtLeast(deltaX, deltaZ, …)` and
///    stepping both, starting from whatever loops 1 and 2 left behind.
///
/// The third loop is what handles an outside corner, where neither the pure-X nor
/// the pure-Z move leaves the ledge but the diagonal does. It also differs
/// structurally from the first two: those `break` the instant a component is
/// zeroed, whereas the diagonal loop zeroes one component and keeps stepping the
/// other in the same iteration, exiting only when the `!= 0.0` guards fail.
///
/// The step magnitude is fixed at `Math.signum(delta) * 0.05` **computed once**,
/// before any stepping, so it never changes sign mid-loop. `Y` is passed through
/// untouched.
///
/// # What the caller must know
///
/// This rewrites the *local* candidate delta only. Vanilla never calls
/// `setDeltaMovement` here, so `getDeltaMovement()` — the vector
/// `restituteMovementAfterCollisions` later reads — keeps its **un-backed-off**
/// value. A player pressed against a ledge by the back-off therefore keeps
/// accumulating horizontal velocity: the back-off is invisible to
/// `xCollision`/`zCollision` too, because those compare against the *backed-off*
/// delta (`Entity.java:766-767`), so a fully-cancelled component reads as "no
/// collision" and is never zeroed. That is vanilla behaviour and it is observable
/// the moment you release shift.
#[must_use]
pub(crate) fn maybe_back_off_from_edge(
    delta: Vec3d,
    back_off: EdgeBackOff,
    bounding_box: Aabb,
    on_ground: bool,
    step_height: f32,
    view: &dyn CollisionView,
) -> Vec3d {
    let EdgeBackOff::Player {
        staying_on_ground_surface,
        fall_distance,
    } = back_off
    else {
        return delta;
    };

    // `float maxDownStep = this.maxUpStep();` — kept at `f32` width, and widened
    // at each use exactly where vanilla's implicit promotion happens.
    let max_down_step = step_height;

    // Vanilla's gate, in vanilla's order and shape: the whole body sits inside one
    // `if`, and the method falls through to `return delta` when it does not hold.
    // `!(delta.y > 0.0)` is kept as written rather than folded to `delta.y <= 0.0`
    // so a NaN Y takes the same branch it does in Java.
    #[allow(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "transcribed from `!(delta.y > 0.0)`; the NaN branch differs from `<=`"
    )]
    if !(delta.y > 0.0)
        && staying_on_ground_surface
        && is_above_ground(bounding_box, on_ground, fall_distance, max_down_step, view)
    {
        let mut delta_x = delta.x;
        let mut delta_z = delta.z;
        // `Math.signum(…) * 0.05`, computed **once** so the step cannot change
        // sign mid-loop. Java's `signum`, not Rust's — see [`mth::java_signum`].
        let step_x = mth::java_signum(delta_x) * 0.05;
        let step_z = mth::java_signum(delta_z) * 0.05;
        let min_height = f64::from(max_down_step);

        // Loop 1: X alone.
        while delta_x != 0.0 && can_fall_at_least(bounding_box, delta_x, 0.0, min_height, view) {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
                break;
            }
            delta_x -= step_x;
        }

        // Loop 2: Z alone, from the *original* `delta.z`.
        while delta_z != 0.0 && can_fall_at_least(bounding_box, 0.0, delta_z, min_height, view) {
            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
                break;
            }
            delta_z -= step_z;
        }

        // Loop 3: both together — the outside-corner case. No `break`: one
        // component may be zeroed while the other keeps stepping in the same
        // iteration, and the `!= 0.0` guards are what end it.
        while delta_x != 0.0
            && delta_z != 0.0
            && can_fall_at_least(bounding_box, delta_x, delta_z, min_height, view)
        {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
            } else {
                delta_x -= step_x;
            }

            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
            } else {
                delta_z -= step_z;
            }
        }

        return Vec3d::new(delta_x, delta.y, delta_z);
    }

    delta
}

/// `Player.isAboveGround(float)` (`Player.java:931-933`).
///
/// ```text
/// this.onGround() || this.fallDistance < maxDownStep
///                    && !this.canFallAtLeast(0.0, 0.0, maxDownStep - this.fallDistance)
/// ```
///
/// Note the **shrinking** probe depth in the airborne branch: the further you have
/// already fallen, the less additional drop is required to disqualify the
/// back-off. So `fall_distance` errs in a known direction — supplying `0.0` for a
/// genuinely-falling entity probes the *full* step height, which is a strictly
/// weaker `canFallAtLeast`, so the gate opens *more* often than vanilla's. That
/// only reaches the airborne branch: while `on_ground` is set the value is
/// unread, and the server resets `fallDistance` to `0.0` on every grounded tick
/// (`Entity.checkFallDamage`, `Entity.java:1569-1581`), so the grounded case — the
/// entire bridging / sneak-placing use case — is exact with the default.
#[must_use]
fn is_above_ground(
    bounding_box: Aabb,
    on_ground: bool,
    fall_distance: f64,
    max_down_step: f32,
    view: &dyn CollisionView,
) -> bool {
    on_ground
        || fall_distance < f64::from(max_down_step)
            && !can_fall_at_least(
                bounding_box,
                0.0,
                0.0,
                f64::from(max_down_step) - fall_distance,
                view,
            )
}

/// `Player.canFallAtLeast(double, double, double)` (`Player.java:935-950`) —
/// whether a box offset by `(dx, dz)` and hanging `min_height` below the feet
/// would meet nothing.
///
/// The probe box is **not** uniformly shrunk, and getting this wrong is the
/// difference between stopping at a ledge and stopping a whole box-width early:
///
/// ```text
/// minX + 1.0E-7 + deltaX  ..  maxX - 1.0E-7 + deltaX   // inset on both sides
/// minY - minHeight - 1.0E-7  ..  minY                  // grown *downward*, top at the feet
/// minZ + 1.0E-7 + deltaZ  ..  maxZ - 1.0E-7 + deltaZ   // inset on both sides
/// ```
///
/// Horizontally it is inset (so merely *touching* a neighbouring column does not
/// count); vertically the bottom is pushed `1e-7` further **down** — an expansion,
/// not a shrink — while the top sits exactly at the feet plane. Because overlap is
/// the strict `min < max` test, the block you are standing on still registers
/// (its top face is at `minY`, and `minY - h - 1e-7 < minY` holds), which is what
/// makes standing on solid ground report "cannot fall".
///
/// The consequence for the caller is that this is a **whole-box** test: it only
/// reports true once the entire inset footprint clears every collider. A sneaking
/// player therefore walks until their box is flush with the ledge and their
/// footprint has left the supporting block, not until their centre crosses it.
///
/// **Scope.** Vanilla calls `level.noCollision(this, box)`, which is
/// `noBlockCollision && noEntityCollision && noBorderCollision`
/// (`CollisionGetter.java:51-53`). [`no_collision`] is the **block half only** —
/// this crate has no entity list and no world border — so a box that vanilla would
/// consider blocked by another entity or by the border reads as free here. Same
/// documented limitation as the fluid hop-out check.
#[must_use]
fn can_fall_at_least(
    bounding_box: Aabb,
    delta_x: f64,
    delta_z: f64,
    min_height: f64,
    view: &dyn CollisionView,
) -> bool {
    // Left-to-right association preserved: `(min + 1e-7) + delta`, and
    // `(minY - minHeight) - 1e-7`.
    no_collision(
        view,
        Aabb::new(
            bounding_box.min_x + 1.0e-7 + delta_x,
            bounding_box.min_y - min_height - 1.0e-7,
            bounding_box.min_z + 1.0e-7 + delta_z,
            bounding_box.max_x - 1.0e-7 + delta_x,
            bounding_box.min_y,
            bounding_box.max_z - 1.0e-7 + delta_z,
        ),
    )
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

/// `LivingEntity.aiStep`'s `noJumpDelay` countdown, run at the top of every
/// travel path before the velocity snap-to-zero.
fn decrement_no_jump_delay(state: &mut PlayerState) {
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
}

/// `aiStep`'s velocity snap-to-zero prologue: the horizontal components
/// collapse to zero below `9.0e-6` (a squared distance) and the vertical
/// component collapses below `0.003`. Byte-identical across every travel path
/// (air, water, lava, elytra), so it is factored out once rather than
/// reproduced per path.
fn snap_small_velocity(v: Vec3d) -> Vec3d {
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
    Vec3d::new(dx, dy, dz)
}

/// The sprint-flag write plus the client-side input transform shared by the
/// air, water and lava travel paths: `state.sprinting = input.sprint` then
/// `LocalPlayer.modifyInput`.
fn set_sprint_and_modify_input(
    state: &mut PlayerState,
    input: MovementInput,
    profile: &PhysicsProfile,
) -> (f32, f32) {
    state.sprinting = input.sprint;
    modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.sneak,
        profile.sneaking_speed,
    )
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
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    // --- input transformation (client-side) -----------------------------------
    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- jump -----------------------------------------------------------------
    if input.jump && state.on_ground && state.no_jump_delay == 0 {
        jump_from_ground(state, view, profile);
        state.no_jump_delay = 10;
    } else if !input.jump {
        state.no_jump_delay = 0;
    }

    // --- travelInAir ----------------------------------------------------------
    // The gravity + drag + collision core is the entity-agnostic `travel_in_air`
    // seam (shared with mobs); the player supplies only the transformed input,
    // `getSpeed()`, and its per-situation flags. Thread the player's motion state
    // through `EntityMotion` and back so the arithmetic is byte-identical.
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = AirTravelContext {
        yaw: state.yaw,
        jumping: input.jump,
        levitation: state.effects.levitation,
        slow_falling: state.effects.slow_falling,
        suppress_ladder_slide: input.sneak,
        suppress_bounce: input.sneak,
        omnidirectional_air_mover: false,
        discard_friction: false,
        // `Player.maybeBackOffFromEdge` — a player always has the override; the
        // shift key and the fall distance are what decide whether it does anything.
        edge_back_off: EdgeBackOff::Player {
            staying_on_ground_surface: input.sneak,
            fall_distance: state.fall_distance,
        },
    };
    travel_in_air(
        &mut motion,
        EntityDimensions::PLAYER,
        (xxa, zza),
        effective_speed(profile, state),
        ctx,
        view,
        profile,
    );
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;
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
pub(crate) fn handle_on_climbable(delta: Vec3d, sneaking: bool) -> Vec3d {
    let bound = f64::from(0.15f32);
    let xd = mth::clamp_f64(delta.x, -bound, bound);
    let zd = mth::clamp_f64(delta.z, -bound, bound);
    let mut yd = delta.y.max(-bound);
    if yd < 0.0 && sneaking {
        yd = 0.0;
    }
    Vec3d::new(xd, yd, zd)
}

/// `LivingEntity.getEffectiveGravity()` — Slow Falling reduces gravity to
/// `min(gravity, 0.01)` while descending; otherwise it is the base gravity.
///
/// The `0.01` is a `double` literal and `min` uses the pre-move delta-Y sign
/// (`getDeltaMovement().y <= 0.0`). In fluids this is what makes the `-0.003`
/// clamp reachable: it shifts `baseGravity/16` off the `0.005` that makes the
/// clamp dead at default gravity.
#[must_use]
pub(crate) fn effective_gravity(base_gravity: f64, falling: bool, slow_falling: bool) -> f64 {
    if falling && slow_falling {
        base_gravity.min(0.01)
    } else {
        base_gravity
    }
}

/// `LivingEntity.getFluidFallingAdjustedMovement(gravity, falling, movement)`.
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

/// `Entity.getFluidJumpThreshold()` (`Entity.java:3692-3694`) —
/// `getEyeHeight() < 0.4 ? 0.0 : 0.4`.
///
/// The pose feeds back into movement here: the swimming pose's eye height is
/// **exactly** `0.4` (`Avatar.java:28`), and `0.4 < 0.4` is false, so a swimming
/// player keeps the `0.4` threshold. Only a pose shorter than that (no vanilla
/// player pose is) collapses the threshold to zero.
#[must_use]
fn fluid_jump_threshold(eye_height: f32) -> f64 {
    if eye_height < 0.4 { 0.0 } else { 0.4 }
}

/// `LivingEntity.onClimbable()` reduced to the block test this engine models: the
/// CLIMBABLE tag on the block at the feet block position.
fn on_climbable(state: &PlayerState, view: &dyn CollisionView) -> bool {
    view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    )
}

/// `LivingEntity.aiStep`'s **jump** block for an entity standing in fluid
/// (`LivingEntity.java:3088-3113`).
///
/// This is the sinking-vs-swimming decision, and it is *not* "jump means `+0.04`
/// in water". Vanilla compares the fluid's **height** against
/// [`fluid_jump_threshold`]:
///
/// * shallow enough (`onGround && !(height > threshold)`, or not in water at all)
///   ⇒ an ordinary `jumpFromGround()` — you jump out of a puddle normally;
/// * otherwise ⇒ `jumpInLiquid()`, the `+0.04F` swim-up impulse.
///
/// Modelling this needs a real fluid height, which is why the summary is passed
/// in rather than re-derived from the coarse presence booleans: with a
/// [`CollisionView::fluid_at`]-capable world the height is exact, and without one
/// a present cell reads as full (`1.0`), which is above the `0.4` threshold and so
/// lands on the swim-up branch — the pre-existing behaviour.
fn apply_fluid_jump(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !input.jump {
        state.no_jump_delay = 0;
        return;
    }
    // `isInLava() ? getFluidHeight(LAVA) : getFluidHeight(WATER)`.
    let in_lava = fluid.in_lava();
    let fluid_height = if in_lava {
        fluid.lava_height
    } else {
        fluid.water_height
    };
    let in_water_and_has_height = fluid.in_water() && fluid_height > 0.0;
    let threshold = fluid_jump_threshold(state.eye_height);
    // The outer test is vanilla's `!(fluidHeight > threshold)` and the two inner
    // ones are its `<=` — transcribed as written rather than normalised, because the
    // two forms differ on NaN and the source is the specification here.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let not_above_threshold = !(fluid_height > threshold);
    // `isInShallowFluid(LAVA)`.
    let shallow_lava = fluid.lava_height <= threshold;
    let jump_in_liquid = |state: &mut PlayerState| {
        state.velocity = state.velocity.add(Vec3d::new(0.0, f64::from(0.04f32), 0.0));
    };

    if !in_water_and_has_height || (state.on_ground && not_above_threshold) {
        if in_lava && !(state.on_ground && shallow_lava) {
            jump_in_liquid(state);
        } else if (state.on_ground || (in_water_and_has_height && fluid_height <= threshold))
            && state.no_jump_delay == 0
        {
            jump_from_ground(state, view, profile);
            state.no_jump_delay = 10;
        }
    } else {
        jump_in_liquid(state);
    }
}

/// `LivingEntity.jumpOutOfFluid(oldY)` (`LivingEntity.java:2556-2561`) — the hop
/// that carries a swimmer *out* of the water onto the ledge they just swam into.
///
/// Runs at the end of both fluid travel branches. When the tick's move collided
/// horizontally and the box would be **free of blocks and of liquid** if lifted to
/// `movement.y + 0.6 - y + oldY`, vertical velocity is replaced by a flat `0.3F`.
/// Without it a player pressed against a shoreline swims into the wall forever;
/// this is the single most visible piece of water movement after buoyancy.
///
/// `isFree` is `noCollision(box) && !containsAnyLiquid(box)` (`Entity.java:664-670`)
/// — the liquid half is what stops it firing repeatedly while still submerged.
fn jump_out_of_fluid(
    state: &mut PlayerState,
    old_y: f64,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !state.horizontal_collision {
        return;
    }
    let movement = state.velocity;
    let lift = movement.y + f64::from(0.6f32) - state.position.y + old_y;
    let probe = state
        .bounding_box(profile)
        .moved(movement.x, lift, movement.z);
    let free = crate::collision::no_collision(view, probe)
        && !crate::collision::contains_any_liquid(view, probe);
    if free {
        state.velocity = Vec3d::new(movement.x, f64::from(0.3f32), movement.z);
    }
}

/// One tick of in-water movement (`travel` → `travelInFluid` → `travelInWater`,
/// `LivingEntity.java:2494-2530`), plus the in-water parts of `aiStep` that
/// precede it.
///
/// In order: the `baseTick` flow-current push, `LocalPlayer.aiStep`'s
/// sneak-to-sink (`goDownInWater`, `-0.04F`), the velocity snap-to-zero prologue,
/// the shallow-vs-deep jump decision ([`apply_fluid_jump`]), the
/// slow-down/input-speed terms (sprint, Depth Strider, Dolphin's Grace), the
/// collision move, the ladder clamp, the `multiply(slowDown, 0.8F, slowDown)`
/// drag, buoyancy ([`fluid_falling_adjusted_movement`]) and finally
/// [`jump_out_of_fluid`].
///
/// # Not modelled
///
/// * The **swimming hitbox**. Vanilla's `Pose.SWIMMING` is `0.6 × 0.6` with eye
///   `0.4` (`Avatar.java:28`); this engine keeps [`EntityDimensions::PLAYER`] for
///   every pose, so a swimmer cannot squeeze through a one-block gap. The *eye*
///   height is modelled, because it is an explicit input
///   ([`PlayerState::eye_height`]) that the pose layer sets, and it is what
///   `getFluidJumpThreshold` and the eye-in-fluid tests read.
/// * Bubble columns (`BubbleColumnBlock`'s own up/down impulses).
/// * `WATER_MOVEMENT_EFFICIENCY` has no source in this repo — see
///   [`PlayerState::water_movement_efficiency`]. The arithmetic is here; the value
///   is `0.0`.
pub fn tick_water(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
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
    // --- LocalPlayer.aiStep: sneak to sink (`goDownInWater`, -0.04F) -----------
    // `LocalPlayer.java:855-857` runs this *before* `super.aiStep()`, so it lands
    // ahead of the snap-to-zero prologue below. (The placement is numerically
    // inert at this magnitude — `0.04` clears the `0.003` collapse from either
    // side — but it is where vanilla puts it, and a later `goDownInWater` variant
    // with a smaller impulse would not be inert.) This is the deliberate-sink half
    // of "sinking versus swimming": without it the only way down is to release
    // jump and wait for buoyancy.
    if input.sneak {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -f64::from(0.04f32), 0.0));
    }
    // --- aiStep prologue: velocity snap-to-zero (identical to the air path) ----
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- aiStep jump: shallow water jumps, deep water swims up -----------------
    apply_fluid_jump(state, input, fluid, view, profile);

    // --- travelInFluid / travelInWater ----------------------------------------
    // `isFalling` and `oldY` are read at the top of `travelInFluid`, i.e. *after*
    // the jump block above has already altered velocity.
    let is_falling = state.velocity.y <= 0.0;
    let old_y = state.position.y;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );

    // `LivingEntity.travelInWater`, in vanilla's order: the sprint/walk base, then
    // the Depth Strider lerp, then Dolphin's Grace *overriding* the result. The
    // order is observable — Grace wins outright, but the Strider term still moves
    // `speed` even when Grace has flattened `slowDown`.
    let mut slow_down = if state.sprinting {
        profile.water_sprint_slow_down
    } else {
        profile.water_slow_down
    };
    let mut speed = profile.fluid_input_speed;
    let mut water_walker = state.water_movement_efficiency;
    if !state.on_ground {
        water_walker *= 0.5f32;
    }
    if water_walker > 0.0 {
        slow_down += (0.546_000_06f32 - slow_down) * water_walker;
        speed += (effective_speed(profile, state) - speed) * water_walker;
    }
    if state.effects.dolphins_grace {
        slow_down = 0.96f32;
    }

    let accel = input_vector(xxa, zza, speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak);

    // `if (horizontalCollision && onClimbable()) movement = (x, 0.2, z)` — a ladder
    // still lifts you while submerged, and it does so *before* the water drag.
    let mut movement = state.velocity;
    if state.horizontal_collision && on_climbable(state, view) {
        movement = Vec3d::new(movement.x, 0.2, movement.z);
    }

    let movement = movement.multiply_each(
        f64::from(slow_down),
        f64::from(0.8f32),
        f64::from(slow_down),
    );
    state.velocity =
        fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
    jump_out_of_fluid(state, old_y, view, profile);
}

/// One tick of movement while submerged in **deep lava** (`travelInLava`).
///
/// Lava is a *different branch* from water, not a retuned one: input speed is a
/// flat `0.02F`, the post-move velocity is scaled by `0.5` (deep) rather than by
/// the water slow-down, and gravity is applied as an extra `-baseGravity/4`
/// term.
///
/// The shallow-lava branch (`isInShallowFluid(LAVA)` ⇒ `multiply(0.5, 0.8, 0.5)` +
/// `getFluidFallingAdjustedMovement`, `LivingEntity.java:2538-2547`) is still
/// **not** modelled: only the deep `scale(0.5)` arm is. Note that the old reason
/// given here — "the coarse `CollisionView` hook does not expose the fluid height"
/// — no longer holds: [`FluidState::lava_height`] is now passed in and
/// [`apply_fluid_jump`] already uses it for the jump decision. What remains is
/// simply unported work, not a missing input.
pub fn tick_lava(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
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
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // aiStep's jump block: in *shallow* lava while on the ground you jump out
    // normally; only deep lava gets `jumpInLiquid`'s +0.04 (see `apply_fluid_jump`).
    apply_fluid_jump(state, input, fluid, view, profile);

    let base_gravity = f64::from(profile.gravity);
    let old_y = state.position.y;

    // moveRelative(0.02) → move → scale(0.5) [deep] → -baseGravity/4.
    let accel = input_vector(xxa, zza, profile.fluid_input_speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak);
    state.velocity = state.velocity.scale(0.5);
    if base_gravity != 0.0 {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -base_gravity / 4.0, 0.0));
    }
    jump_out_of_fluid(state, old_y, view, profile);
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

    decrement_no_jump_delay(state);

    // aiStep velocity collapse (players use the horizontal-distance test).
    let collapsed = snap_small_velocity(state.velocity);

    state.velocity = update_fall_flying_movement(state, profile, collapsed);
    do_move(state, view, profile, false, input.sneak);
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
    // `Entity.baseTick` computes the fluid summary from the *pre-move* box, before
    // `travel` reads `isInWater`/`isInLava`. Do the same: one source of truth for
    // eye/box submersion, recorded on the state for the swimming pose and for the
    // shell's fog / overlay / ambient-sound consumers.
    let fluid = compute_fluid_state(
        state.bounding_box(profile),
        state.position,
        state.eye_height,
        view,
    );
    state.eye_in_water = fluid.eye_in_water;
    state.eye_in_lava = fluid.eye_in_lava;
    state.swimming = update_swimming(
        state.swimming,
        state.sprinting,
        &fluid,
        view,
        state.position,
    );

    if fluid.in_water() {
        tick_water(state, input, &fluid, view, profile);
    } else if fluid.in_lava() {
        tick_lava(state, input, &fluid, view, profile);
    } else if state.fall_flying {
        tick_elytra(state, input, view, profile);
    } else {
        tick_air(state, input, view, profile);
    }
    update_stuck_multiplier(state, view, profile);
}

/// [`tick`] followed by one pass of `LivingEntity.pushEntities` against `nearby`.
///
/// This is the whole of `LivingEntity.aiStep`'s ordering for entity interaction:
/// `travel` first (`LivingEntity.java:3130`), the crowd push last (`:3163`). So the
/// impulse a neighbour delivers this tick is integrated on the **next** one, and
/// this tick's collision sweep never sees it.
///
/// Passing an empty `nearby` is bit-for-bit [`tick`] — [`crate::push::apply_entity_push`]
/// returns immediately, and the hard-collision half is not reached at all (see the
/// note below). That is what makes this addition provably inert for existing
/// callers.
///
/// **What this does not do, and cannot do without a producer.** The *hard*
/// collision half — `Entity.collide`'s entity colliders — is not threaded through
/// here. Doing so means widening `tick`/`tick_air`/`tick_water`/`tick_lava`/
/// `tick_elytra`/`travel_in_air`/`move_entity`, all of which are public and called
/// from crates this change may not touch, for a case that currently has **no**
/// producer: `getEntityCollisions` filters on `canBeCollidedWith`, which only boats,
/// shulkers and happy ghasts satisfy. The capability exists and is tested as
/// [`crate::collision::collide_among_entities`] /
/// [`crate::entity::move_entity_among_entities`]; wiring it into the player
/// pipeline is a signature change to make when a caller can supply boats.
pub fn tick_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
    self_flags: crate::push::PushSelf,
) {
    tick(state, input, view, profile);
    crate::push::apply_entity_push(state, view, profile, nearby, self_flags);
}

/// `Entity.updateSwimming()` (`Entity.java:1644-1652`) — the sprint-swimming pose
/// state machine.
///
/// Entering requires being **under water** (eye submerged) *and* the block at the
/// feet holding water; once swimming, it is sustained merely by sprinting while
/// **in** water (box touching water), so you keep swimming as you break the
/// surface. Passenger/vehicle state is not modelled here (this engine has none),
/// matching the `!isPassenger()` guard being vacuously true.
///
/// `Player.updateSwimming` (`Player.java:1433-1439`) adds one override: a *flying*
/// player is never swimming. This engine has no flight, so a driver with a
/// free-fly/creative-flight mode must clear [`PlayerState::swimming`] itself while
/// flying rather than relying on this function — it is only reached from [`tick`],
/// which a flying driver does not call.
fn update_swimming(
    swimming: bool,
    sprinting: bool,
    fluid: &FluidState,
    view: &dyn CollisionView,
    position: Vec3d,
) -> bool {
    if swimming {
        sprinting && fluid.in_water()
    } else {
        sprinting && fluid.under_water() && water_at_block(view, position)
    }
}

/// `level.getFluidState(blockPosition).is(WATER)` — water at the entity's own
/// block position (`floor` of each coordinate), fine-then-coarse like the rest of
/// the fluid state.
fn water_at_block(view: &dyn CollisionView, position: Vec3d) -> bool {
    let bx = mth::floor(position.x);
    let by = mth::floor(position.y);
    let bz = mth::floor(position.z);
    match view.fluid_at(bx, by, bz) {
        Some(cell) => cell.kind == crate::fluid::FluidKind::Water,
        None => view.is_water(bx, by, bz),
    }
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

/// Player-shaped wrapper over [`friction_influenced_speed_value`] retained for
/// the speed unit tests, which assert against a `PlayerState`.
#[cfg(test)]
fn friction_influenced_speed(
    profile: &PhysicsProfile,
    state: &PlayerState,
    block_friction: f32,
) -> f32 {
    friction_influenced_speed_value(
        effective_speed(profile, state),
        block_friction,
        state.on_ground,
        profile,
    )
}

/// `LivingEntity.getFrictionInfluencedSpeed(float)` (LivingEntity.java:2710),
/// entity-agnostic core. `speed` is the caller's `getSpeed()` — the player's
/// movement-speed attribute or a mob's AI-supplied speed. On the ground the
/// `0.21600002F / friction^3` factor rescales it (all in `float`); airborne it
/// is discarded for `getFlyingSpeed()` (`profile.flying_speed`, `0.02F` for a
/// non-ridden entity). `speed` therefore only reaches the result on the ground,
/// exactly as vanilla.
#[must_use]
pub(crate) fn friction_influenced_speed_value(
    speed: f32,
    block_friction: f32,
    on_ground: bool,
    profile: &PhysicsProfile,
) -> f32 {
    if on_ground {
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

    /// A hand-built world: explicit solid cells, explicit water cells with a real
    /// `getAmount()`, so the fluid **height** branches (jump threshold, hop-out)
    /// are exercised rather than the coarse full-cell fallback.
    #[derive(Default)]
    struct Pool {
        solid: std::collections::HashSet<(i32, i32, i32)>,
        water: std::collections::HashMap<(i32, i32, i32), u8>,
    }

    impl Pool {
        fn solid(&mut self, x: i32, y: i32, z: i32) -> &mut Self {
            self.solid.insert((x, y, z));
            self
        }
        fn water(&mut self, x: i32, y: i32, z: i32, amount: u8) -> &mut Self {
            self.water.insert((x, y, z), amount);
            self
        }
        fn floor(&mut self, y: i32) -> &mut Self {
            for x in -2..=2 {
                for z in -2..=2 {
                    self.solid.insert((x, y, z));
                }
            }
            self
        }
    }

    impl CollisionView for Pool {
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
            self.water.contains_key(&(x, y, z))
        }
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<crate::fluid::FluidCell> {
            self.water
                .get(&(x, y, z))
                .map(|&amount| crate::fluid::FluidCell {
                    kind: crate::fluid::FluidKind::Water,
                    amount,
                    falling: false,
                })
        }
        fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
            self.solid.contains(&(x, y, z))
        }
    }

    #[test]
    fn fluid_jump_threshold_boundary_is_the_swimming_eye_height() {
        // `Entity.getFluidJumpThreshold()` = `getEyeHeight() < 0.4 ? 0.0 : 0.4`
        // (Entity.java:3692-3694). The swimming pose's eye height is *exactly*
        // 0.4 (Avatar.java:28), so it sits on the false side of a strict `<` and
        // keeps the 0.4 threshold. Coding this as `<=` would collapse a swimmer's
        // threshold to zero and turn every swim-up into a standing jump.
        assert_eq!(fluid_jump_threshold(DEFAULT_EYE_HEIGHT), 0.4);
        assert_eq!(fluid_jump_threshold(0.4), 0.4);
        assert_eq!(fluid_jump_threshold(0.399), 0.0);
    }

    #[test]
    fn shallow_water_jumps_but_deep_water_swims_up() {
        // The sinking-versus-swimming decision (`LivingEntity.aiStep`, the jump
        // block at LivingEntity.java:3088-3113). Expected magnitudes come from
        // vanilla constants, not from this port:
        //   * shallow  -> jumpFromGround, JUMP_STRENGTH = 0.42F, then the water
        //     tick's own `* 0.8F` vertical drag and `- gravity/16` buoyancy step
        //     => 0.42*0.8 - 0.005 = 0.331
        //   * deep     -> jumpInLiquid, +0.04F  => 0.04*0.8 - 0.005 = 0.027
        // A single order of magnitude apart, so the branch cannot be mistaken.
        let p = PhysicsProfile::mc_1_21();

        // amount 3 => own height 3/9 = 0.333 < the 0.4 threshold: a puddle.
        let mut shallow = Pool::default();
        shallow.floor(0).water(0, 1, 0, 3);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        let jump = MovementInput {
            jump: true,
            ..MovementInput::NONE
        };
        tick(&mut s, jump, &shallow, &p);
        assert!(
            (s.velocity.y - (0.42 * 0.8 - 0.005)).abs() < 1.0e-6,
            "shallow water must produce a real jump, got vy = {}",
            s.velocity.y
        );

        // amount 8 => 8/9 = 0.888 > 0.4: deep enough to swim in.
        let mut deep = Pool::default();
        deep.floor(0).water(0, 1, 0, 8).water(0, 2, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        tick(&mut s, jump, &deep, &p);
        assert!(
            (s.velocity.y - (0.04 * 0.8 - 0.005)).abs() < 1.0e-6,
            "deep water must swim up, not jump, got vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn sneaking_sinks_and_not_sneaking_barely_does() {
        // `LocalPlayer.aiStep` -> `goDownInWater()` (LivingEntity.java:2395-2397):
        // shift while in water adds -0.04F. Expected values from vanilla constants:
        // the tick's vertical drag is 0.8F and buoyancy is -gravity/16 = -0.005, so
        //   no shift: 0.0  * 0.8 - 0.005 = -0.005
        //   shift:   -0.04 * 0.8 - 0.005 = -0.037
        // The pair is the point: without `goDownInWater` both read -0.005 and the
        // only way down is to release jump and wait.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;

        let mut idle = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(&mut idle, MovementInput::NONE, &view, &p);
        assert!(
            (idle.velocity.y - (-0.005)).abs() < 1.0e-8,
            "idle sink vy = {}",
            idle.velocity.y
        );

        let mut sinking = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(
            &mut sinking,
            MovementInput {
                sneak: true,
                ..MovementInput::NONE
            },
            &view,
            &p,
        );
        assert!(
            (sinking.velocity.y - (-0.04 * 0.8 - 0.005)).abs() < 1.0e-8,
            "shift-sink vy = {}",
            sinking.velocity.y
        );
        assert!(
            sinking.position.y < idle.position.y,
            "shift must actually move the player down further"
        );
    }

    #[test]
    fn swimming_into_a_ledge_hops_out_of_the_water() {
        // `LivingEntity.jumpOutOfFluid` (LivingEntity.java:2556-2561): a horizontal
        // collision plus a lifted box that is free of blocks *and* of liquid
        // replaces vertical velocity with a flat 0.3F. The expected value is that
        // literal, straight from the source.
        //
        // Geometry: a one-deep pool (water only at y = 1) with a shore block at
        // z = 1, and the player floating with its feet near the surface so the box
        // still overlaps the shore block. Swimming +Z (yaw 0 faces +Z) presses into
        // the shore.
        let p = PhysicsProfile::mc_1_21();
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let mut pool = Pool::default();
        pool.floor(0).water(0, 1, 0, 8).solid(0, 1, 1);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        let mut hopped = false;
        for _ in 0..60 {
            tick(&mut s, forward, &pool, &p);
            if (s.velocity.y - f64::from(0.3f32)).abs() < 1.0e-12 {
                hopped = true;
                break;
            }
        }
        assert!(hopped, "never hopped out; final state {s:?}");

        // Control: the identical pool with no shore block. `jumpOutOfFluid` is
        // gated on `horizontalCollision`, so with nothing to swim into the same
        // detector must never fire — proving the assertion above is not just
        // "0.3 appears sometimes".
        let mut open = Pool::default();
        open.floor(0).water(0, 1, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        for _ in 0..60 {
            tick(&mut s, forward, &open, &p);
            assert!(
                (s.velocity.y - f64::from(0.3f32)).abs() > 1.0e-12,
                "open water must never hop: vy = {}",
                s.velocity.y
            );
        }
    }

    #[test]
    fn depth_strider_attribute_speeds_up_swimming() {
        // `travelInWater` lerps both the horizontal slow-down (toward 0.546_000_06)
        // and the input speed (toward `getSpeed()`) by
        // `getAttributeValue(WATER_MOVEMENT_EFFICIENCY)` (LivingEntity.java:2507-2517).
        // No caller can reach that attribute value yet (see the field docs on
        // `PlayerState::water_movement_efficiency` for exactly what is missing), so
        // this drives it directly: the *arithmetic* is what is under test, and the
        // direction it must move in (faster) is fixed by the source, not by this port.
        //
        // Note the halving when airborne (`if (!onGround()) waterWalker *= 0.5F`):
        // a swimmer is airborne, so a level-III boot (0.99) acts as 0.495.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let travel = |efficiency: f32| {
            let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0)
                .with_water_movement_efficiency(efficiency);
            for _ in 0..40 {
                tick(&mut s, forward, &view, &p);
            }
            s.position.z - 0.5
        };

        let bare = travel(0.0);
        let strider = travel(0.99);
        assert!(
            strider > bare * 1.5,
            "Depth Strider must materially speed up swimming: {strider} vs {bare}"
        );
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
    fn tick_sets_swimming_and_eye_in_water_when_sprinting_submerged() {
        // The real consumer of the eye-in-fluid state inside physics:
        // `updateSwimming`. A sprinting player fully under water enters the
        // sprint-swimming pose, and the eye/box submersion flags are recorded on
        // the state for the shell (fog / overlay / ambient sound) to read.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(s.eye_in_water, "eye is submerged");
        assert!(s.swimming, "sprinting + underwater => swimming pose");
    }

    #[test]
    fn tick_does_not_swim_when_sprinting_in_air() {
        // Negative control: sprinting with no water must not set the swim pose,
        // and must not spuriously report the eye in water.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &Air, &p);
        assert!(!s.eye_in_water && !s.swimming);
    }

    #[test]
    fn swimming_is_sustained_while_sprinting_in_water_even_when_eye_surfaces() {
        // Once swimming, the pose persists on `sprinting && isInWater()` alone —
        // you keep swimming as you break the surface (eye leaves the water) until
        // you stop sprinting or leave the water. Uses a one-block-deep pool so the
        // box is in water but the eye (feet + 1.62) is above it.
        struct ShallowPool;
        impl CollisionView for ShallowPool {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
            fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
                y == 94
            }
        }
        let p = PhysicsProfile::mc_1_21();
        let view = ShallowPool;
        let mut s = PlayerState::at(Vec3d::new(0.5, 94.0, 0.5), 0.0);
        s.sprinting = true;
        s.swimming = true; // already swimming from a prior submerged tick
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(!s.eye_in_water, "eye is above the one-deep pool");
        assert!(s.swimming, "swim pose sustained by sprinting-in-water");
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
        tick_water(&mut s, MovementInput::NONE, &FluidState::NONE, &view, &p);
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
