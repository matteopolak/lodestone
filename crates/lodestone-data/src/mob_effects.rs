//! Public mob-effect id→identifier resolution for protocol 776.
//!
//! `update_mob_effect` and `remove_mob_effect` carry the effect as a
//! `minecraft:mob_effect` registry id (a VarInt). Unlike `minecraft:damage_type`
//! (a purely data-driven registry with no default protocol id, so its network
//! id is assigned per-connection by registry sync order this adapter does not
//! yet track), `minecraft:mob_effect` is a fixed, built-in registry: the same
//! kind of static id→name table as items, particles, and sounds, generated from
//! Mojang's own `registries.json`, never guessed.

pub use crate::generated_mob_effects::MOB_EFFECT_COUNT;
use crate::generated_mob_effects::MOB_EFFECT_NAMES;

/// Resolves a network mob-effect registry id to its canonical `minecraft:*`
/// identifier.
///
/// Returns `None` for ids outside `0..MOB_EFFECT_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong effect.
#[must_use]
pub fn mob_effect_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| MOB_EFFECT_NAMES.get(index).copied())
}

/// Resolves a canonical `minecraft:*` mob-effect identifier to its network
/// registry id for protocol 776.
///
/// The reverse of [`mob_effect_name`], needed to encode the serverbound
/// `set_beacon` packet's chosen effects. A linear scan is fine: this runs once
/// per beacon confirmation, not per tick.
#[must_use]
pub fn mob_effect_id(name: &str) -> Option<i32> {
    MOB_EFFECT_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
}

/// Vanilla's own mob-effect "get color" accessor for a network mob-effect registry id — the
/// constructor colour argument [`crate::generated_mob_effect_colors`] carries,
/// as opaque ARGB.
///
/// Exposed because it is a **sort key**, not only a tint: vanilla's own
/// mob-effect-instance comparator breaks ties on the colour, so the
/// inventory effect column's row order is not reproducible without it.
///
/// `None` for an id outside the registry, exactly like [`mob_effect_name`].
#[must_use]
pub fn mob_effect_color(id: i32) -> Option<u32> {
    usize::try_from(id)
        .ok()
        .and_then(|index| {
            crate::generated_mob_effect_colors::MOB_EFFECT_COLORS
                .get(index)
                .copied()
        })
}
