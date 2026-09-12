//! Player movement core, mirroring vanilla's own per-tick player movement.
//!
//! The per-tick pipeline reproduced here is (for a non-fluid, non-elytra
//! player), in order:
//!
//! 1. Velocity snap-to-zero (`< 9.0E-6` horizontal, `< 0.003` vertical).
//! 2. Jump handling (leaving the ground, including the sprint boost).
//! 3. Travel resolution:
//!    - input acceleration, scaled by friction, is added,
//!    - collision is resolved and the block speed factor applied,
//!    - gravity is subtracted from the post-move Y,
//!    - horizontal drag (`blockFriction * 0.91`) and vertical drag (`0.98`) are
//!      applied last.
//!
//! Every width (`f32` vs `f64`) and every operation order matches the reference
//! source, because the server validates the resulting positions.

use crate::collision::{CollisionView, no_collision};
use crate::entity::{
    AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity,
    move_entity_with_nearby, travel_in_air_among_entities,
};
use crate::fluid::apply_fluid_push;
use crate::fluid_state::{FluidState, compute_fluid_state};
use crate::geometry::{Aabb, Vec3d};
use crate::mth::{self};
use crate::pose::{Pose, update_player_pose};
use crate::profile::{FluidModel, InputModel, PhysicsProfile};

include!("player/types.rs");
include!("player/movement.rs");
include!("player/edge_backoff.rs");
include!("player/special.rs");
include!("player/air.rs");
include!("player/fluid.rs");
include!("player/elytra.rs");
include!("player/tick.rs");
#[cfg(test)]
include!("player/tests.rs");
