//! The entity-agnostic movement core: vanilla's `Entity.move()` shared by every
//! moving thing, players and mobs alike.
//!
//! # Why this module exists
//!
//! Vanilla has exactly **one** `Entity.move()`. Players and mobs reach it through
//! the same call (`LivingEntity.travel()` → `move(MoverType.SELF, …)`); they
//! differ only in the *velocity they hand it* and in two per-entity values — the
//! collision hitbox and the auto-step height. There is no second copy of the
//! collision/restitution arithmetic to drift on ice, slabs, water or fences.
//!
//! Lodestone must keep that single-integrator property or reintroduce exactly the
//! divergence the anti-cheat punishes: a mob loop with its *own* `move()` would
//! agree with the player path on flat ground and diverge on precisely the terrain
//! a pathfinding mob spends its life on. So [`move_entity`] is the one shared
//! primitive, and the player tick pipeline in [`crate::player`] is a thin caller
//! that supplies player velocity and [`EntityDimensions::PLAYER`].
//!
//! # The category error this fixes
//!
//! The hitbox and step height previously lived on [`PhysicsProfile`], which is
//! *per version*. But a zombie and a player share a version and **not** a hitbox:
//! width/height are `EntityDimensions` in vanilla and step height is the
//! `STEP_HEIGHT` attribute — both **per entity type**. Housing them on the
//! version profile is the same mistake as [`crate::profile`] §"what cannot be a
//! scalar" in reverse: there, per-version *behaviour* was smuggled into a scalar;
//! here, per-*entity* data was smuggled into the per-version struct. They now live
//! in [`EntityDimensions`], a per-call input, and the profile keeps only what
//! genuinely varies by version (gravity, drag, friction curves, the input model).

use crate::collision::{CollisionView, collide_among_entities};
use crate::geometry::{Aabb, Vec3d};
use crate::mth;
use crate::player::{
    EdgeBackOff, effective_gravity, friction_block, friction_influenced_speed_value,
    handle_on_climbable, input_vector, maybe_back_off_from_edge, mth_equal,
    restitute_movement_after_collisions,
};
use crate::profile::PhysicsProfile;
use crate::push::{NearbyEntity, entity_collision_boxes};

/// The per-entity movement inputs that are **not** version knowledge: the
/// collision hitbox (`width`/`height`) and the auto-step height (`step_height`).
///
/// In vanilla these are `EntityDimensions` (width/height) and the `STEP_HEIGHT`
/// attribute respectively — both keyed on entity *type*, not on game version. A
/// caller supplies the concrete values for whatever it is moving; the player path
/// supplies [`Self::PLAYER`], a mob supplies its own hitbox and step height.
///
/// **Sourcing (settled with `impl-entity`).** These three fields come from *two*
/// different origins, and a caller must not collapse them into one table:
/// * `width`/`height` are the entity type's **base** `EntityDimensions`. Any
///   `SCALE`-attribute fold is applied by the caller *before* constructing this
///   struct — the geometry table holds base dims, never scaled ones.
/// * `step_height` is the **resolved** `STEP_HEIGHT` attribute value *after* the
///   modifier fold (vanilla `Entity.maxUpStep()` = `(float) getAttributeValue(
///   STEP_HEIGHT)`), not a static per-type constant. Populate it from the entity's
///   attribute map at spawn so a step-height modifier is honoured; sourcing it
///   from the static geometry census would silently disagree the moment one exists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityDimensions {
    /// Standing bounding-box width (the box is `width` on both horizontal axes).
    pub width: f32,
    /// Standing bounding-box height.
    pub height: f32,
    /// Auto-step height — how far up a ledge the entity climbs without jumping.
    /// The **resolved** `STEP_HEIGHT` attribute, narrowed to `f32` exactly as
    /// vanilla's `maxUpStep()` `(float)` cast does. The `RangedAttribute` default
    /// is `0.6` (so an ordinary mob steps like a player, not `0.0`); some types
    /// raise it (a horse is `1.0`), and a modifier can shift it further — which is
    /// why this is sourced from the attribute map, not a static table.
    pub step_height: f32,
}

impl EntityDimensions {
    /// The player's dimensions: a `0.6 × 1.8` hitbox and a `0.6` step height.
    /// These are constant across every version Lodestone targets, which is why
    /// they are a per-entity constant here rather than a profile scalar.
    pub const PLAYER: Self = Self {
        width: 0.6,
        height: 1.8,
        step_height: 0.6,
    };

    /// Constructs dimensions for an arbitrary entity type.
    #[must_use]
    pub const fn new(width: f32, height: f32, step_height: f32) -> Self {
        Self {
            width,
            height,
            step_height,
        }
    }

    /// The axis-aligned bounding box for an entity whose feet (box centre on the
    /// horizontal axes, box *bottom* on Y) are at `feet`.
    ///
    /// This mirrors vanilla's `makeBoundingBox`: `half = width / 2`, spanning
    /// `feet.y ..= feet.y + height`. The `width / 2` division is done in `f64`
    /// after widening the `f32` width, exactly as the player path did before this
    /// value moved out of the profile.
    #[must_use]
    pub fn bounding_box(&self, feet: Vec3d) -> Aabb {
        let half = f64::from(self.width) / 2.0;
        Aabb::new(
            feet.x - half,
            feet.y,
            feet.z - half,
            feet.x + half,
            feet.y + f64::from(self.height),
            feet.z + half,
        )
    }
}

/// The mutable motion state that vanilla's `Entity.move()` reads and writes:
/// world position, velocity (`deltaMovement`), the ground/collision flags, and
/// the pending stuck-speed multiplier consumed at the top of a move.
///
/// This is the entity-agnostic slice of state — no input model, no effects, no
/// pose. A caller (the player pipeline, or a future mob loop) owns the richer
/// per-entity state and threads this motion through [`move_entity`] each tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityMotion {
    /// World position of the entity's feet.
    pub position: Vec3d,
    /// Velocity (`deltaMovement`). [`move_entity`] moves the entity by this
    /// vector, then rewrites it via the collision restitution — exactly as
    /// vanilla's `travel()` calls `move(SELF, getDeltaMovement())`.
    pub velocity: Vec3d,
    /// Whether the entity is supported from below (`verticalCollisionBelow`). This
    /// is the flag the client transmits; see [`crate::player::PlayerState`].
    pub on_ground: bool,
    /// Whether the entity hit a wall this move (`horizontalCollision`).
    pub horizontal_collision: bool,
    /// A pending stuck-in-block multiplier (cobweb, powder snow, sweet berry
    /// bush) recorded on the *previous* tick and consumed at the top of the next
    /// move. `ZERO` means "not stuck"; see [`CollisionView::stuck_multiplier`].
    pub stuck_speed_multiplier: Vec3d,
}

impl EntityMotion {
    /// A resting entity at `position`: zero velocity, airborne until the first
    /// move settles it, no wall contact, no stuck multiplier.
    #[must_use]
    pub fn at(position: Vec3d) -> Self {
        Self {
            position,
            velocity: Vec3d::ZERO,
            on_ground: false,
            horizontal_collision: false,
            stuck_speed_multiplier: Vec3d::ZERO,
        }
    }
}

/// Per-move flags that come from higher-level entity state rather than from the
/// motion itself.
///
/// `slow_falling` is the Slow Falling status effect, which lowers descent gravity
/// in `getEffectiveGravity` and so affects the land-bounce branch of restitution.
/// `suppress_bounce` is `isSuppressingBounce()` (true for a sneaking player),
/// which both zeroes the base entity restitution and vetoes the slime/bed bounce.
/// A plain mob passes `MoveContext::default()` (all inert).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MoveContext {
    /// Slow Falling is active (reduces descent gravity to `min(gravity, 0.01)`).
    pub slow_falling: bool,
    /// The entity is suppressing bounces (a sneaking player).
    pub suppress_bounce: bool,
    /// Which `maybeBackOffFromEdge` override this entity has. Defaults to
    /// [`EdgeBackOff::Entity`], the identity base implementation, so a mob or a
    /// dropped item is inert by construction.
    pub edge_back_off: EdgeBackOff,
    /// `getBlockSpeedFactor()` returns a flat `1.0F` instead of consulting the
    /// block underfoot — i.e. no soul-sand / honey slowdown.
    ///
    /// `Player.getBlockSpeedFactor()` (`Player.java:1855`) is
    /// `!abilities.flying && !isFallFlying() ? super.getBlockSpeedFactor() : 1.0F`,
    /// so a player suppresses it while **either** creative-flying or gliding. The
    /// whole disjunction is modelled rather than just the flight half: a partial
    /// model of one vanilla method is the failure `docs/edge-back-off.md` calls
    /// out as worse than an explicit stand-in, and the elytra half was already
    /// wrong here before flight existed (a gliding player was being slowed by
    /// soul sand that vanilla ignores).
    ///
    /// `false` for every mob and item — `Entity.getBlockSpeedFactor` has no such
    /// gate — so [`Default`] leaves existing callers bit-identical.
    pub suppress_block_speed_factor: bool,
}

/// `Entity.move(MoverType.SELF, deltaMovement)` restricted to the parts that
/// affect an entity's reported position: consume any pending stuck multiplier,
/// collide (with the auto-step mechanic), commit position, update the collision
/// flags, run `restituteMovementAfterCollisions`, and apply the block speed
/// factor to the resulting velocity.
///
/// This is the **single shared integrator** — the player pipeline and any mob
/// loop must both route through it so they cannot diverge on slabs, ice, water or
/// fences. The entity's hitbox and step height come in via `dims`; its velocity
/// is read from (and written back to) `motion.velocity`, mirroring vanilla's
/// `move(SELF, getDeltaMovement())`.
///
/// `profile` supplies the genuinely version-parameterised gravity used by the
/// land-bounce branch; it no longer carries the hitbox or step height.
pub fn move_entity(
    motion: &mut EntityMotion,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    ctx: MoveContext,
) {
    move_entity_among_entities(motion, dims, view, profile, ctx, &[]);
}

/// [`move_entity`] with the entity half of `Entity.collide` wired: the sweep sees
/// `entity_colliders` in addition to block geometry.
///
/// `entity_colliders` comes from [`crate::push::entity_collision_boxes`] over
/// `dims.bounding_box(motion.position).expand_towards(velocity)`. Passing `&[]` is
/// exactly [`move_entity`], bit for bit — see [`collide_among_entities`].
///
/// **This is a second entry point rather than a field on [`MoveContext`] on
/// purpose.** `MoveContext` is a `Copy` value type of plain scalars, threaded
/// through [`travel_in_air`] and constructed by callers outside this crate; a
/// borrowed slice would give it a lifetime parameter and take `Copy` with it. It
/// also already lost its `Eq` when it gained an `f64` — the bar for adding to it is
/// high, and "a per-tick world snapshot" is not the kind of thing it holds.
///
/// The soft **push** is *not* here. It is not part of `Entity.move` at all;
/// vanilla applies it at the end of `aiStep`, after the move, via
/// [`crate::push::apply_entity_push`].
pub fn move_entity_among_entities(
    motion: &mut EntityMotion,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    ctx: MoveContext,
    entity_colliders: &[Aabb],
) {
    // `deltaMovement` at the top of `Entity.move`.
    let delta = motion.velocity;

    // Consume a pending stuck-speed multiplier (cobweb, powder snow, sweet berry
    // bush) *before* collision. Vanilla scales the local movement `delta` in
    // place, then calls `setDeltaMovement(Vec3.ZERO)` — so the scaled vector
    // drives this tick's sweep, but the velocity that
    // `restituteMovementAfterCollisions` later reads is **zero**, not the scaled
    // value. Keep those two roles as distinct locals so the arithmetic matches
    // rather than reusing `delta` for both. When not stuck (the default), both
    // equal `delta` and the result is byte-identical to the un-stuck path.
    let mut move_delta = delta;
    let mut pre_collision_velocity = delta;
    if motion.stuck_speed_multiplier.length_sqr() > 1.0E-7 {
        move_delta = move_delta.multiply_each(
            motion.stuck_speed_multiplier.x,
            motion.stuck_speed_multiplier.y,
            motion.stuck_speed_multiplier.z,
        );
        motion.stuck_speed_multiplier = Vec3d::ZERO;
        pre_collision_velocity = Vec3d::ZERO;
    }

    let bb = dims.bounding_box(motion.position);

    // `delta = this.maybeBackOffFromEdge(delta, moverType);` (`Entity.java:743`) —
    // **inside** the move, after the stuck multiplier is consumed and before
    // `collide`. The position is unchanged at this point, so `bb` is vanilla's
    // `getBoundingBox()`.
    //
    // Order is observable in two ways, both of which a "clamp the velocity before
    // the tick" shortcut would get wrong. It runs *after* the stuck multiplier, so
    // a cobweb-slowed delta is what gets probed and stepped. And it rewrites only
    // this local candidate: `pre_collision_velocity` is deliberately left alone,
    // because vanilla never calls `setDeltaMovement` here, so the velocity that
    // `restituteMovementAfterCollisions` reads keeps its un-backed-off value.
    move_delta = maybe_back_off_from_edge(
        move_delta,
        ctx.edge_back_off,
        bb,
        motion.on_ground,
        dims.step_height,
        view,
    );

    let resolved = collide_among_entities(
        view,
        move_delta,
        bb,
        motion.on_ground,
        dims.step_height,
        entity_colliders,
    );

    let x_collision = !mth_equal(move_delta.x, resolved.x);
    let z_collision = !mth_equal(move_delta.z, resolved.z);
    motion.horizontal_collision = x_collision || z_collision;

    let moved_vertically = move_delta.y.abs() > 0.0;
    let vertical_collision = move_delta.y != resolved.y;
    let vertical_collision_below = vertical_collision && move_delta.y < 0.0;
    motion.on_ground = vertical_collision_below;

    // Commit position (vanilla guards on a tiny-movement threshold, but adding
    // the resolved movement is equivalent for non-degenerate deltas).
    if resolved.length_sqr() > 1.0E-7 || move_delta.length_sqr() - resolved.length_sqr() < 1.0E-7 {
        motion.position = motion.position.add(resolved);
    }

    // Vanilla keeps `deltaMovement == delta` through `collide`, then only
    // `restituteMovementAfterCollisions` rewrites it — and *that* is what zeroes
    // a blocked axis (restitution 0) or reverses it into a bounce (slime). Read
    // `pre_collision_velocity` (zeroed above when stuck) as vanilla's
    // `getDeltaMovement()`, and query the post-move position for the bounce block.
    let mut velocity = pre_collision_velocity;
    if (moved_vertically && vertical_collision) || motion.horizontal_collision {
        velocity = restitute_movement_after_collisions(
            pre_collision_velocity,
            resolved,
            x_collision,
            z_collision,
            vertical_collision,
            vertical_collision_below,
            motion.position,
            ctx.slow_falling,
            view,
            profile,
            ctx.suppress_bounce,
        );
    }
    motion.velocity = velocity;

    // Block speed factor (soul sand 0.4, honey 0.4), applied last as in `move`.
    // `getBlockSpeedFactor`: query `blockPosition()` (floor of the feet) first,
    // and only if that is 1.0 fall through to the block below that affects
    // movement (`getOnPos(0.500001)`).
    let block_speed_factor = if ctx.suppress_block_speed_factor {
        // `Player.getBlockSpeedFactor()` short-circuits to `1.0F` while flying or
        // gliding — the block is never queried at all.
        1.0
    } else {
        let bx = mth::floor(motion.position.x);
        let by = mth::floor(motion.position.y);
        let bz = mth::floor(motion.position.z);
        let speed_factor_here = f64::from(view.speed_factor(bx, by, bz));
        if speed_factor_here == 1.0 {
            let (fx, fy, fz) = friction_block(motion.position);
            f64::from(view.speed_factor(fx, fy, fz))
        } else {
            speed_factor_here
        }
    };
    motion.velocity = motion
        .velocity
        .multiply_each(block_speed_factor, 1.0, block_speed_factor);
}

/// [`move_entity`] with hard colliders selected from a mixed nearby-entity
/// snapshot at the point the movement sweep begins.
///
/// The query uses the current hitbox expanded toward the current velocity, just
/// like vanilla's `getEntityCollisions`. A pending stuck multiplier can only
/// shrink that velocity inside [`move_entity_among_entities`], so querying with
/// the unscaled vector is a conservative superset and cannot introduce a false
/// collision: the axis sweep still rejects every box the actual move does not
/// reach.
pub(crate) fn move_entity_with_nearby(
    motion: &mut EntityMotion,
    dims: EntityDimensions,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    ctx: MoveContext,
    nearby: &[NearbyEntity],
) {
    if nearby.is_empty() {
        move_entity(motion, dims, view, profile, ctx);
        return;
    }
    let query = dims
        .bounding_box(motion.position)
        .expand_towards(motion.velocity.x, motion.velocity.y, motion.velocity.z);
    let mut colliders = Vec::new();
    entity_collision_boxes(query, nearby, &mut colliders);
    move_entity_among_entities(motion, dims, view, profile, ctx, &colliders);
}

/// Per-entity / per-situation inputs to [`travel_in_air`] that are not part of
/// the core [`EntityMotion`] — the flags and effects vanilla's `travelInAir`
/// branches on. A plain falling mob passes `AirTravelContext { yaw, ..default }`;
/// the player pipeline fills in the sneak- and effect-driven fields.
///
/// Everything here is a plain scalar/bool so the seam stays free of any
/// `lodestone-entity` dependency: the caller resolves effects/attributes and
/// hands across only numbers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AirTravelContext {
    /// Body yaw in degrees, used to rotate the relative input into world space
    /// (`moveRelative`). Mobs supply their AI-driven yaw; the player its facing.
    pub yaw: f32,
    /// The jump key is held / the mob is jumping this tick. Feeds the climbable
    /// "steady climb-up" branch (`(horizontalCollision || jumping) && onClimbable`).
    pub jumping: bool,
    /// Levitation amplitude (`Some(amp)` = effect active). Levitation *replaces*
    /// gravity with a pull toward `0.05 * (amp + 1)`.
    pub levitation: Option<u32>,
    /// Slow Falling reduces descent gravity to `min(gravity, 0.01)`.
    pub slow_falling: bool,
    /// Suppress the ladder slide-down (`isSuppressingSlidingDownLadder()`), which
    /// vanilla gates on `this instanceof Player`; a mob always passes `false`.
    ///
    /// This is the *sneak* input alone, not the full vanilla conjunct: the
    /// scaffolding exception (`!getInBlockState().is(Blocks.SCAFFOLDING)`,
    /// issue #210) is applied inside [`travel_in_air`] against the same
    /// in-block position `is_climbable` already queries, because it needs a
    /// [`CollisionView`] call this context struct has none of — see that
    /// function's climbing block.
    pub suppress_ladder_slide: bool,
    /// `isSuppressingBounce()` (a sneaking player) — vetoes slime/bed bounce.
    pub suppress_bounce: bool,
    /// `omnidirectionalAirMover()` — a handful of entities drag their vertical
    /// velocity by the *horizontal* air drag (`0.91`) instead of `0.98`. False
    /// for players and ordinary mobs.
    pub omnidirectional_air_mover: bool,
    /// `shouldDiscardFriction()` — when true vanilla skips the drag multiply
    /// entirely for this tick. False for players and ordinary mobs.
    pub discard_friction: bool,
    /// Which `maybeBackOffFromEdge` override this entity has, forwarded to
    /// [`MoveContext`]. Defaults to the inert [`EdgeBackOff::Entity`].
    pub edge_back_off: EdgeBackOff,
    /// This entity's `getFlyingSpeed()` — the value
    /// `getFrictionInfluencedSpeed` substitutes for `getSpeed()` **whenever it is
    /// airborne** (`LivingEntity.java:2710-2716`).
    ///
    /// `None` means "use [`PhysicsProfile::flying_speed`]", i.e.
    /// `LivingEntity.getFlyingSpeed()`'s unridden `0.02F`. That is the
    /// [`Default`], so every existing mob caller is bit-identical to before this
    /// field existed — the field is inert by construction rather than by
    /// convention.
    ///
    /// A player passes `Some(...)` from
    /// [`crate::player::player_flying_speed`], which is sprint- **and**
    /// flight-dependent in vanilla and therefore cannot be a profile constant.
    pub flying_speed: Option<f32>,
    /// Forwarded to [`MoveContext::suppress_block_speed_factor`].
    pub suppress_block_speed_factor: bool,
    /// Force `onClimbable()` to `false`, detaching the entity from ladders and
    /// vines: no pre-move velocity clamp and no steady climb-up.
    ///
    /// `Player.onClimbable()` is `abilities.flying ? false : super.onClimbable()`
    /// (`Player.java:2025`). `false` for mobs, whose `onClimbable` has no such
    /// override, so [`Default`] is inert.
    pub suppress_climbable: bool,
}

/// `LivingEntity.travelInAir(Vec3)` (LivingEntity.java:2460) — the shared,
/// entity-agnostic gravity + drag + input-assembly seam that both the player
/// pipeline and any mob loop route through, so the two cannot grow divergent
/// copies of vanilla motion.
///
/// The caller supplies the *already-transformed* relative move amounts
/// (`input = (xxa, zza)`, vanilla's `moveRelative` input, produced by the
/// player's keyboard transform or a mob's AI vector) and `getSpeed()` as
/// `speed`. The friction-influenced rescale (`getFrictionInfluencedSpeed`),
/// climbable clamp, the shared [`move_entity`] collision sweep, gravity and drag
/// all live *inside* the seam so their order — which is observable — is fixed in
/// one place.
///
/// Order (each cited against `LivingEntity.java`):
/// 1. `blockFriction` from the block below (`getBlockPosBelowThatAffectsMyMovement`
///    + `FRICTION_MODIFIER`), or `1.0F` when airborne (:2461).
/// 2. `moveRelative(getFrictionInfluencedSpeed(bf), input)` then the ladder clamp
///    (`handleOnClimbable`) — both before the move (:2666).
/// 3. [`move_entity`] (vanilla `move(SELF, deltaMovement)`), then the climbable
///    "steady climb" override forcing `y = 0.2` (:2673).
/// 4. gravity **after** the move, levitation replacing it (:2604).
/// 5. drag **last**: horizontal `blockFriction * 0.91`, vertical `0.98` (both
///    `float` literals widened to `double` in the multiply) (:2618).
pub fn travel_in_air(
    motion: &mut EntityMotion,
    dims: EntityDimensions,
    input: (f32, f32),
    speed: f32,
    ctx: AirTravelContext,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    travel_in_air_among_entities(motion, dims, input, speed, ctx, view, profile, &[]);
}

/// The player-facing variant of [`travel_in_air`] that includes hard entity
/// colliders. Kept crate-private so the public entity movement seam does not
/// acquire a second world-snapshot API.
#[allow(clippy::too_many_arguments)]
pub(crate) fn travel_in_air_among_entities(
    motion: &mut EntityMotion,
    dims: EntityDimensions,
    input: (f32, f32),
    speed: f32,
    ctx: AirTravelContext,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[NearbyEntity],
) {
    let block_friction = if motion.on_ground {
        let (fx, fy, fz) = friction_block(motion.position);
        mth::compute_modified_friction(view.friction(fx, fy, fz), profile.friction_modifier)
    } else {
        1.0
    };

    // handleRelativeFrictionAndCalculateMovement
    let friction_speed = friction_influenced_speed_value(
        speed,
        block_friction,
        motion.on_ground,
        ctx.flying_speed.unwrap_or(profile.flying_speed),
        profile,
    );
    let accel = input_vector(input.0, input.1, friction_speed, ctx.yaw);
    motion.velocity = motion.velocity.add(accel);

    // Ladder/vine handling: clamp horizontal + downward speed before the move,
    // and force a steady climb-up after it if pushing into (or jumping against)
    // the climbable. `onClimbable` tests the block at the feet block position;
    // we evaluate it once (pre-move) and reuse it, as the current player path
    // does, rather than re-querying the post-move position.
    let climb_x = mth::floor(motion.position.x);
    let climb_y = mth::floor(motion.position.y);
    let climb_z = mth::floor(motion.position.z);
    let climbing = !ctx.suppress_climbable && view.is_climbable(climb_x, climb_y, climb_z);
    if climbing {
        // Issue #210. `LivingEntity.handleOnClimbable`'s sneak-to-hold clamp
        // carries one extra conjunct beyond "is this a climbable block":
        // `!getInBlockState().is(Blocks.SCAFFOLDING)` (`LivingEntity.java:2700`),
        // read at the **same** in-block position `climbing` above already
        // queried. On a ladder or vine, sneaking while moving down clamps `yd`
        // to `0.0` and you hang in place; on scaffolding that conjunct is
        // `false`, so the clamp never engages and sneaking keeps descending at
        // the ordinary `-0.15` cap — scaffolding does not offer a ladder's
        // edge-hold.
        let on_scaffolding = view.is_scaffolding(climb_x, climb_y, climb_z);
        motion.velocity = handle_on_climbable(
            motion.velocity,
            ctx.suppress_ladder_slide && !on_scaffolding,
        );
    }

    let move_ctx = MoveContext {
        slow_falling: ctx.slow_falling,
        suppress_bounce: ctx.suppress_bounce,
        edge_back_off: ctx.edge_back_off,
        suppress_block_speed_factor: ctx.suppress_block_speed_factor,
    };
    move_entity_with_nearby(motion, dims, view, profile, move_ctx, nearby);

    let mut movement = motion.velocity;
    if (motion.horizontal_collision || ctx.jumping) && climbing {
        movement = Vec3d::new(movement.x, 0.2, movement.z);
    }

    // gravity on the post-move Y.
    //
    // `travelInAir` chooses one of two mutually-exclusive vertical updates:
    // Levitation *replaces* gravity with a pull toward `0.05*(amp+1)`; otherwise
    // it subtracts `getEffectiveGravity()`, which Slow Falling reduces while
    // descending. `falling` is read from the post-move Y (== `movement.y`).
    let movement_y = if let Some(amp) = ctx.levitation {
        movement.y + (0.05 * f64::from(amp + 1) - movement.y) * 0.2
    } else {
        let falling = movement.y <= 0.0;
        movement.y - effective_gravity(f64::from(profile.gravity), falling, ctx.slow_falling)
    };

    if ctx.discard_friction {
        // shouldDiscardFriction(): keep the moved velocity, no drag this tick.
        motion.velocity = Vec3d::new(movement.x, movement_y, movement.z);
    } else {
        // drag applied last, horizontal by blockFriction * 0.91, vertical by 0.98
        // (unless this is an omnidirectional air mover, which drags Y by 0.91 too).
        let air_drag = mth::compute_modified_friction(profile.air_drag, profile.air_drag_modifier);
        let friction = block_friction * air_drag;
        let vertical_friction = if ctx.omnidirectional_air_mover {
            air_drag
        } else {
            mth::compute_modified_friction(profile.vertical_air_drag, profile.air_drag_modifier)
        };
        motion.velocity = Vec3d::new(
            movement.x * f64::from(friction),
            movement_y * f64::from(vertical_friction),
            movement.z * f64::from(friction),
        );
    }
}
