//! Public particle-type id→identifier resolution for protocol 776.
//!
//! `level_particles` carries the particle as a `minecraft:particle_type`
//! registry id (a VarInt) before any per-particle payload data. That id→name
//! mapping is version-specific data — ids shift as the registry grows — so it
//! lives here in the version crate, generated from Mojang's own
//! `registries.json`, never in a shared crate.

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
