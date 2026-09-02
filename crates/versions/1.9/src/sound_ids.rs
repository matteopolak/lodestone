//! Public legacy sound-id→name resolution for protocol 340 (Minecraft
//! 1.12.2).
//!
//! Only the numeric `sound_effect` packet needs this table —
//! `named_sound_effect` already carries its sound as a string and needs no
//! lookup at all. `vendor/minecraft-data/data/pc/1.12.2/sounds.json` lists
//! every 1.12.2 `SoundEvent` in wire-order network id (verified contiguous
//! `0..SOUND_ID_COUNT`, no gaps to silently misalign a later entry), and its
//! bare names already match the dotted `category.detail` shape the modern
//! sound-event registry key uses (e.g. `"block.anvil.break"` is
//! `minecraft:block.anvil.break` in both eras), so resolving one is a bare
//! namespace prefix rather than a rename table — unlike the block/particle
//! legacy id spaces, which genuinely renamed entries across the flattening.

pub use crate::generated_sound_ids::SOUND_ID_COUNT;
use crate::generated_sound_ids::SOUND_ID_NAMES;

/// Resolves a legacy 1.12.2 `SoundEvent` registry id to its canonical
/// `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..SOUND_ID_COUNT`, so a malformed or
/// out-of-range id surfaces as an explicit miss rather than a panic or a
/// silently wrong sound.
#[must_use]
pub fn sound_name(id: i32) -> Option<String> {
    usize::try_from(id)
        .ok()
        .and_then(|index| SOUND_ID_NAMES.get(index).copied())
        .map(|name| format!("minecraft:{name}"))
}
