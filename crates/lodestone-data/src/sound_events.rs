//! Public sound-event id→identifier resolution for protocol 776.
//!
//! `sound`/`sound_entity` carry the sound as a `Holder<SoundEvent>`: a positive
//! VarInt referencing the `minecraft:sound_event` registry (id minus one), or a
//! zero flag introducing an inline definition. The registry id→name mapping is
//! generated from Mojang's own `registries.json` for 26.2, the one canonical
//! internal version, so it lives here in this data crate rather than
//! in `lodestone-v26-2` — it is a game-data census, not
//! wire-format code.
//!
//! The canonical names live in one id-indexed table. Optional fixed audible
//! ranges are registry-entry metadata rather than wire fields, and live in a
//! sparse id-keyed table so the names are not stored twice.

pub use crate::generated_sound_events::SOUND_EVENT_COUNT;
use crate::generated_sound_events::{SOUND_EVENT_FIXED_RANGES, SOUND_EVENT_NAMES};
use std::collections::HashMap;
use std::sync::OnceLock;

/// A validated entry in the built-in 26.2 sound-event registry.
///
/// Decode or import a raw registry id with [`Self::new`] before asking this
/// census for a name. Inline holders keep their supplied resource key instead:
/// they are deliberately not coerced into this built-in registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SoundEventId(i32);

impl SoundEventId {
    /// Validates a raw network registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: i32) -> Option<Self> {
        if raw >= 0 && raw < SOUND_EVENT_COUNT as i32 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The registry id used by the version-specific wire codec.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

fn sound_event_from_tables<'name>(
    id: SoundEventId,
    names: &[&'name str],
    fixed_ranges: &[(u32, f32)],
) -> (&'name str, Option<f32>) {
    let raw = id.raw();
    let id = u32::try_from(raw).expect("validated sound-event ids are non-negative");
    let index = usize::try_from(raw).expect("validated sound-event ids fit usize");
    let name = names[index];
    let fixed_range = fixed_ranges
        .binary_search_by_key(&id, |&(candidate, _)| candidate)
        .ok()
        .map(|index| fixed_ranges[index].1);
    (name, fixed_range)
}

/// Resolves a validated network sound-event registry id to its canonical `minecraft:*`
/// identifier and optional fixed audible range.
///
/// Raw values enter through [`SoundEventId::new`], so this lookup is total.
#[must_use]
pub fn sound_event(id: SoundEventId) -> (&'static str, Option<f32>) {
    sound_event_from_tables(id, &SOUND_EVENT_NAMES, &SOUND_EVENT_FIXED_RANGES)
}

/// Resolves a validated network sound-event registry id to just its canonical
/// `minecraft:*` identifier, ignoring the fixed audible range.
#[must_use]
pub fn sound_event_name(id: SoundEventId) -> &'static str {
    SOUND_EVENT_NAMES[id.raw() as usize]
}

/// Resolves a canonical `minecraft:*` sound-event identifier to its validated
/// network registry id for protocol 776.
///
/// Inline custom definitions have no built-in id and remain `None`; callers
/// must preserve their resource key rather than inventing an id for them.
#[must_use]
pub fn sound_event_id(name: &str) -> Option<SoundEventId> {
    static INDEX: OnceLock<HashMap<&'static str, SoundEventId>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            SOUND_EVENT_NAMES
                .iter()
                .enumerate()
                .filter_map(|(index, &name)| {
                    let raw = i32::try_from(index).ok()?;
                    SoundEventId::new(raw).map(|id| (name, id))
                })
                .collect()
        })
        .get(name)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_id_rejects_negative_and_out_of_range_values() {
        let past_end = i32::try_from(SOUND_EVENT_COUNT).expect("sound-event count fits i32");

        assert_eq!(SoundEventId::new(-1), None);
        assert_eq!(SoundEventId::new(past_end), None);
    }

    /// Literal control from the registry report: id zero names this event.
    #[test]
    fn registry_lookup_returns_the_canonical_name_and_range() {
        let id = SoundEventId::new(0).expect("the first registry id validates");
        assert_eq!(
            sound_event(id),
            ("minecraft:entity.allay.ambient_with_item", None)
        );
        assert_eq!(sound_event_name(id), "minecraft:entity.allay.ambient_with_item");
        assert_eq!(sound_event_id("minecraft:entity.allay.ambient_with_item"), Some(id));
        assert_eq!(sound_event_id("minecraft:not_a_real_sound"), None);
    }

    #[test]
    fn every_generated_name_round_trips_through_the_cached_reverse_lookup() {
        for raw in 0..SOUND_EVENT_COUNT as i32 {
            let id = SoundEventId::new(raw).expect("generated table ids validate");
            assert_eq!(sound_event_id(sound_event_name(id)), Some(id));
        }
    }

    #[test]
    fn sparse_range_lookup_joins_by_registry_id() {
        let names = ["zero", "one", "two", "three", "four"];
        let ranges = [(3, 16.0)];

        assert_eq!(
            sound_event_from_tables(SoundEventId::new(3).unwrap(), &names, &ranges),
            ("three", Some(16.0))
        );
        assert_eq!(
            sound_event_from_tables(SoundEventId::new(2).unwrap(), &names, &ranges),
            ("two", None)
        );
    }
}
