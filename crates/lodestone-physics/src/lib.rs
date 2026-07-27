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
pub mod mth;
pub mod player;
pub mod profile;

pub use collision::CollisionView;
pub use effect::{DirectEffect, MovementEffect, classify, movement_speed_modifier};
pub use entity::{
    AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity, travel_in_air,
};
pub use fluid::{FluidCell, FluidKind, HorizontalDir, apply_fluid_push, get_flow};
pub use fluid_state::{FluidState, compute_fluid_state};
pub use geometry::{Aabb, Axis, Vec3d};
pub use player::{
    MovementInput, PlayerState, StatusEffects, input_vector, tick, tick_air, tick_elytra,
    tick_lava, tick_water,
};
pub use profile::{FluidModel, InputModel, PhysicsProfile};
