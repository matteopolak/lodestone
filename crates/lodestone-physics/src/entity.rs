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

use crate::collision::{CollisionView, collide};
use crate::geometry::{Aabb, Vec3d};
use crate::mth;
use crate::player::{friction_block, mth_equal, restitute_movement_after_collisions};
use crate::profile::PhysicsProfile;

/// The per-entity movement inputs that are **not** version knowledge: the
/// collision hitbox (`width`/`height`) and the auto-step height (`step_height`).
///
/// In vanilla these are `EntityDimensions` (width/height) and the `STEP_HEIGHT`
/// attribute respectively — both keyed on entity *type*, not on game version. A
/// caller supplies the concrete values for whatever it is moving; the player path
/// supplies [`Self::PLAYER`], a mob supplies its own hitbox and step height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityDimensions {
    /// Standing bounding-box width (the box is `width` on both horizontal axes).
    pub width: f32,
    /// Standing bounding-box height.
    pub height: f32,
    /// Auto-step height — how far up a ledge the entity climbs without jumping.
    /// Vanilla's `STEP_HEIGHT` attribute (`0.6` for a player, `1.0` for e.g. a
    /// horse, `0.0` for most mobs).
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
/// A plain mob passes `MoveContext::default()` (both `false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoveContext {
    /// Slow Falling is active (reduces descent gravity to `min(gravity, 0.01)`).
    pub slow_falling: bool,
    /// The entity is suppressing bounces (a sneaking player).
    pub suppress_bounce: bool,
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
    let resolved = collide(view, move_delta, bb, motion.on_ground, dims.step_height);

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
    let bx = mth::floor(motion.position.x);
    let by = mth::floor(motion.position.y);
    let bz = mth::floor(motion.position.z);
    let speed_factor_here = f64::from(view.speed_factor(bx, by, bz));
    let block_speed_factor = if speed_factor_here == 1.0 {
        let (fx, fy, fz) = friction_block(motion.position);
        f64::from(view.speed_factor(fx, fy, fz))
    } else {
        speed_factor_here
    };
    motion.velocity = motion
        .velocity
        .multiply_each(block_speed_factor, 1.0, block_speed_factor);
}
