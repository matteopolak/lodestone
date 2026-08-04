//! Bit-exact re-implementation of Minecraft Java Edition player physics.
//!
//! Lodestone connects to real vanilla servers, whose movement anti-cheat
//! compares reported positions against what it believes is possible. Any
//! floating-point divergence accumulates into rubber-banding and kicks, so this
//! crate reproduces vanilla's arithmetic exactly — the same `f32`/`f64` widths,
//! the same operation order, and the same `Mth` lookup-table trigonometry — down
//! to the bit.
//!
//! # Layout
//!
//! * [`mth`] — `net.minecraft.util.Mth` helpers (sine LUT, `floor`, `clamp`,
//!   `lerp`, `wrapDegrees`, …).
//! * [`geometry`] — `Vec3`/`AABB` mirrors keeping vanilla's expression order.
//! * [`collision`] — swept-AABB collision with the auto-step mechanic, over the
//!   [`collision::CollisionView`] trait (so physics stays decoupled from the
//!   world crate and testable against synthetic worlds).
//! * [`profile`] — [`profile::PhysicsProfile`], the version-parameterised knobs.
//! * [`entity`] — the entity-agnostic move core ([`entity::move_entity`]) and the
//!   gravity + drag + input-assembly seam ([`entity::travel_in_air`]), both shared
//!   by players and mobs and parameterised by [`entity::EntityDimensions`].
//! * [`player`] — the per-tick player movement pipeline (a thin caller of the
//!   entity core).
//! * [`pose`] — [`pose::Pose`], the per-pose hitbox/eye height, and vanilla's
//!   fit-gated `Player.updatePlayerPose` state machine, which is what makes a
//!   swimmer `0.6` tall without ever clipping them into a ceiling.
//! * [`push`] — entity-versus-entity interaction: the soft crowd push
//!   (`Entity.push(Entity)`) and the entity half of `noCollision`. Deliberately
//!   *not* on [`collision::CollisionView`]: that trait answers block geometry, and
//!   entity data is a caller-owned per-tick snapshot rather than a repeatable
//!   spatial query.
//! * [`knockback`] — [`knockback::knockback_impulse`], the melee attack
//!   knockback velocity mechanic (`LivingEntity.knockback`). Distinct from
//!   [`push`]'s always-on crowd nudge and from an explosion's radial knockback
//!   (`lodestone_entity::explosion`) — three different formulas, none shared.
//!
//! The crate has no runtime dependencies; the sine table is generated once and
//! checked in as [`sin_table`].

mod sin_table;

pub mod collision;
pub mod effect;
pub mod entity;
pub mod fluid;
pub mod fluid_state;
pub mod geometry;
pub mod knockback;
pub mod mth;
pub mod player;
pub mod pose;
pub mod profile;
pub mod push;

pub use collision::CollisionView;
pub use effect::{DirectEffect, MovementEffect, classify, movement_speed_modifier};
pub use entity::{
    AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity,
    move_entity_among_entities, travel_in_air,
};
pub use fluid::{FluidCell, FluidKind, HorizontalDir, apply_fluid_push, get_flow};
pub use fluid_state::{FluidState, compute_fluid_state};
pub use geometry::{Aabb, Axis, Vec3d};
pub use knockback::{attack_direction, knockback_impulse};
pub use player::{
    EdgeBackOff, MovementInput, PlayerState, StatusEffects, apply_firework_boost, apply_riptide,
    input_vector, player_flying_speed, tick, tick_air, tick_among_entities, tick_elytra,
    tick_lava, tick_water,
};
pub use pose::{
    Pose, can_player_fit_within_blocks_and_entities_when, can_player_fit_within_blocks_when,
    desired_pose, update_player_pose,
};
pub use profile::{FluidModel, InputModel, PhysicsProfile};
pub use push::{
    CollisionRule, NearbyEntity, PushSelf, apply_entity_push, entity_collision_boxes,
    entity_push_impulse, no_collision_among_entities, no_entity_collision, pair_push_vector,
    reciprocal_push_impulse, self_is_pushable, team_allows_push,
};
