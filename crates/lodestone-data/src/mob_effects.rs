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

/// A validated entry in the built-in 26.2 `minecraft:mob_effect` registry.
///
/// The 26.2 packet and server encoders construct this value at their fixed
/// registry boundary. Version-free item components deliberately retain a raw
/// `i32`: an extension or a session whose registry differs from this census
/// must remain representable until a built-in consumer decides it can use the
/// value. [`Self::from_registry_id`] is that consumer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MobEffectId(u8);

impl MobEffectId {
    /// Validates a raw network registry id against the 26.2 built-in census.
    #[must_use]
    pub fn from_registry_id(id: i32) -> Option<Self> {
        let id = u8::try_from(id).ok()?;
        (usize::from(id) < MOB_EFFECT_NAMES.len()).then_some(Self(id))
    }

    /// The registry id emitted by the 26.2 wire codec.
    #[must_use]
    pub const fn registry_id(self) -> i32 {
        self.0 as i32
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Resolves a validated mob-effect registry id to its canonical
/// `minecraft:*` identifier.
///
/// A [`MobEffectId`] makes this lookup total. Raw wire values must be checked
/// with [`MobEffectId::from_registry_id`] before they reach this function.
#[must_use]
pub fn mob_effect_name_for(id: MobEffectId) -> &'static str {
    MOB_EFFECT_NAMES[id.index()]
}

/// Resolves a network mob-effect registry id to its canonical `minecraft:*`
/// identifier.
///
/// Returns `None` for ids outside `0..MOB_EFFECT_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong effect.
#[must_use]
pub fn mob_effect_name(id: i32) -> Option<&'static str> {
    MobEffectId::from_registry_id(id).map(mob_effect_name_for)
}

/// Resolves a canonical `minecraft:*` mob-effect identifier to its network
/// registry id for protocol 776 as a validated built-in value.
///
/// The reverse of [`mob_effect_name_for`], needed to encode the serverbound
/// `set_beacon` packet's chosen effects. A linear scan is fine: this runs once
/// per beacon confirmation, not per tick.
#[must_use]
pub fn mob_effect_id(name: &str) -> Option<MobEffectId> {
    MOB_EFFECT_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
        .and_then(MobEffectId::from_registry_id)
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
    MobEffectId::from_registry_id(id).map(mob_effect_color_for)
}

/// Returns the opaque display colour for a validated built-in mob effect.
#[must_use]
pub fn mob_effect_color_for(id: MobEffectId) -> u32 {
    crate::generated_mob_effect_colors::MOB_EFFECT_COLORS[id.index()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_id_rejects_values_outside_the_generated_census() {
        assert_eq!(MobEffectId::from_registry_id(-1), None);
        assert_eq!(MobEffectId::from_registry_id(MOB_EFFECT_COUNT as i32), None);
    }

    /// Literal controls from the generated registry report: the first and last
    /// rows prove both ends of the census rather than a merely symmetric lookup.
    #[test]
    fn registry_id_resolves_the_generated_boundary_rows() {
        let speed = MobEffectId::from_registry_id(0).expect("first generated id validates");
        let nautilus = MobEffectId::from_registry_id(39).expect("last generated id validates");

        assert_eq!(mob_effect_name_for(speed), "minecraft:speed");
        assert_eq!(mob_effect_name_for(nautilus), "minecraft:breath_of_the_nautilus");
        assert_eq!(mob_effect_id("minecraft:speed"), Some(speed));
        assert_eq!(mob_effect_id("minecraft:breath_of_the_nautilus"), Some(nautilus));
    }

    #[test]
    fn every_generated_name_round_trips_through_the_validated_id() {
        for raw in 0..MOB_EFFECT_COUNT as i32 {
            let id = MobEffectId::from_registry_id(raw).expect("generated ids validate");
            assert_eq!(mob_effect_id(mob_effect_name_for(id)), Some(id));
        }
    }
}
