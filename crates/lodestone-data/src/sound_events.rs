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

fn sound_event_from_tables<'name>(
    id: i32,
    names: &[&'name str],
    fixed_ranges: &[(u32, f32)],
) -> Option<(&'name str, Option<f32>)> {
    let id = u32::try_from(id).ok()?;
    let index = usize::try_from(id).ok()?;
    let name = names.get(index).copied()?;
    let fixed_range = fixed_ranges
        .binary_search_by_key(&id, |&(candidate, _)| candidate)
        .ok()
        .map(|index| fixed_ranges[index].1);
    Some((name, fixed_range))
}

/// Resolves a network sound-event registry id to its canonical `minecraft:*`
/// identifier and optional fixed audible range.
///
/// Returns `None` for ids outside `0..SOUND_EVENT_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong sound.
#[must_use]
pub fn sound_event(id: i32) -> Option<(&'static str, Option<f32>)> {
    sound_event_from_tables(id, &SOUND_EVENT_NAMES, &SOUND_EVENT_FIXED_RANGES)
}

/// Resolves a network sound-event registry id to just its canonical
/// `minecraft:*` identifier, ignoring the fixed audible range.
///
/// Returns `None` for ids outside `0..SOUND_EVENT_COUNT`.
#[must_use]
pub fn sound_event_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| SOUND_EVENT_NAMES.get(index).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lookup_rejects_negative_and_out_of_range_ids() {
        let past_end = i32::try_from(SOUND_EVENT_COUNT).expect("sound-event count fits i32");

        assert_eq!(sound_event(-1), None);
        assert_eq!(sound_event_name(-1), None);
        assert_eq!(sound_event(past_end), None);
        assert_eq!(sound_event_name(past_end), None);
    }

    #[test]
    fn registry_lookup_returns_the_canonical_name_and_range() {
        assert_eq!(
            sound_event(0),
            Some(("minecraft:entity.allay.ambient_with_item", None))
        );
        assert_eq!(
            sound_event_name(0),
            Some("minecraft:entity.allay.ambient_with_item")
        );
    }

    #[test]
    fn sparse_range_lookup_joins_by_registry_id() {
        let names = ["zero", "one", "two", "three", "four"];
        let ranges = [(3, 16.0)];

        assert_eq!(
            sound_event_from_tables(3, &names, &ranges),
            Some(("three", Some(16.0)))
        );
        assert_eq!(
            sound_event_from_tables(2, &names, &ranges),
            Some(("two", None))
        );
    }
}
