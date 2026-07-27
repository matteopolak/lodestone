//! Public sound-event id→identifier resolution for protocol 776.
//!
//! `sound`/`sound_entity` carry the sound as a `Holder<SoundEvent>`: a positive
//! VarInt referencing the `minecraft:sound_event` registry (id minus one), or a
//! zero flag introducing an inline definition. The registry id→name mapping is
//! version-specific data — ids shift as the registry grows — so it lives here in
//! the version crate, generated from Mojang's own `registries.json`, never in a
//! shared crate.
//!
//! Each entry pairs the identifier with the sound's optional fixed audible
//! range, which is a property of the registry entry rather than the wire and so
//! must travel with the name for registry-referenced sounds.

pub use crate::generated_sound_events::SOUND_EVENT_COUNT;
use crate::generated_sound_events::{SOUND_EVENT_ENTRIES, SOUND_EVENT_NAMES};

/// Resolves a network sound-event registry id to its canonical `minecraft:*`
/// identifier and optional fixed audible range.
///
/// Returns `None` for ids outside `0..SOUND_EVENT_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong sound.
#[must_use]
pub fn sound_event(id: i32) -> Option<(&'static str, Option<f32>)> {
    usize::try_from(id)
        .ok()
        .and_then(|index| SOUND_EVENT_ENTRIES.get(index).copied())
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
