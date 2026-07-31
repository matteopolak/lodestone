//! Public particle-type id→identifier resolution for protocol 776.
//!
//! `level_particles` carries the particle as a `minecraft:particle_type`
//! registry id (a VarInt) before any per-particle payload data. The id→name
//! mapping is generated from Mojang's own `registries.json` for 26.2, the one
//! canonical internal version (#343), so it lives here in this data crate
//! rather than in `lodestone-v770` (issue #361) — it is a game-data census,
//! not wire-format code.

pub use crate::generated_particle_types::PARTICLE_TYPE_COUNT;
use crate::generated_particle_types::PARTICLE_TYPE_NAMES;

/// Resolves a network particle-type id to its canonical `minecraft:*`
/// identifier.
///
/// Returns `None` for ids outside `0..PARTICLE_TYPE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong particle.
#[must_use]
pub fn particle_type_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| PARTICLE_TYPE_NAMES.get(index).copied())
}
