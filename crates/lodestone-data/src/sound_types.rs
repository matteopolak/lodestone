//! Per-block-state `SoundType` (break / step / place / hit / fall sounds, plus
//! the type's volume and pitch) for protocol 776 (Minecraft 26.2).
//!
//! This is the data half of every block-surface sound. Without it a client can
//! decode and route `LEVEL_EVENT` 2001 perfectly and still be silent, because
//! the packet carries a **block-state id and nothing else**: vanilla's
//! `LevelEventHandler.levelEvent` `case 2001` looks the sound up locally from
//! `Block.stateById(data).getSoundType()`.
//! That is exactly the state lodestone was in — see `docs/sound-playback.md`.
//!
//! # Data source: interrogate the real jar
//!
//! A `SoundType` is seven values assigned per block in
//! `BlockBehaviour.Properties.sound(..)` — i.e. in **code**. `blocks.json`
//! carries block *properties* only and has no sound field at all, and
//! `vendor/minecraft-data` has no 26.x data. So, like [`crate::hardness`] and
//! [`crate::collision_shapes`], the table comes from booting the real 26.2
//! server headlessly and asking every one of the 32,366 `BlockState`s in
//! `Block.BLOCK_STATE_REGISTRY` for `getSoundType()` — see
//! `oracle-java/SoundTypeOracle.java` and `tests/sound_types.rs`.
//!
//! **Transcribing `SoundType.java` by hand would have been wrong.** The dump
//! measures 126 distinct sound types in use against 127 `public static final
//! SoundType` constants in that file, and the odd one out is
//! `SoundType.TWISTING_VINES` — the only static carrying `pitch = 0.5F`, and
//! assigned to **no block**: `Blocks.TWISTING_VINES`,
//! `TWISTING_VINES_PLANT` and their kin all pass
//! `SoundType.WEEPING_VINES` instead. A reader
//! pairing constants to blocks by name would have shipped a 0.5 pitch on
//! twisting vines. Similarly `HARD_CROP` reuses `WOOD`'s four sounds with
//! `CROP_PLANTED` for placement, and `GLOW_LICHEN` reuses `GRASS`'s four with
//! `VINE_STEP`.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id, the same shape as [`crate::hardness`].
//! Measured on the dump: the 32,366 states carry only **126** distinct
//! seven-tuples (and 126 distinct `SoundType` *objects*, so value-dedup collapses
//! nothing), while 124 of those 126 are `volume = 1.0, pitch = 1.0` — only
//! `ANVIL` (`volume = 0.3`) and `METAL` (`pitch = 1.5`) differ. So:
//!
//! | representation | bytes |
//! |---|---|
//! | per-state seven-tuple (`f32,f32,5×u16`, 20 B padded) | 647,320 |
//! | 126-entry table + per-state `u8` index | 34,634 |
//!
//! ~19× smaller, and the `u8` is safe with room to spare because 126 ≤ 255 — the
//! generator *panics* rather than truncating if a version bump ever pushes past
//! 256, and [`ENTRY_COUNT`] is asserted in `tests/sound_types.rs` so a new sound
//! type fails loudly instead of being rounded onto a neighbour.
//!
//! The five sound columns are `minecraft:sound_event` **registry ids**, which is
//! the same id space [`crate::sound_events`] is indexed by — so no sound name is
//! duplicated into this table, and the test suite cross-checks two
//! independently generated tables (this one from the live registry, that one from
//! Mojang's `registries.json`) against each other.
//!
//! # Gotchas
//!
//! * **Air has a `SoundType`** (`STONE`, as it happens), so "the table answered"
//!   is not "there is a sound to play". Vanilla's `case 2001` guards with
//!   `if (!blockState.isAir())`; a consumer must do the same or an air-state
//!   level event plays a stone break.
//! * **`minecraft:intentionally_empty` is a real registry entry** (id for
//!   `SoundEvents.EMPTY`) and appears in this table — e.g. `CACTUS_FLOWER`'s step
//!   and `DRIED_GHAST`'s place. It resolves to no sample, so playing it is a
//!   silent no-op plus a `debug` log line. [`BlockSoundType::is_empty_sound`]
//!   lets a caller skip it instead.
//! * **The volume and pitch here are the `SoundType`'s own**, not what vanilla
//!   passes to the sound manager. Both the break path
//!   (`LevelEventHandler.levelEvent`) and the placement path
//!   (`BlockItem.place`) scale them: see
//!   [`BlockSoundType::break_or_place_volume`].
//! * Only **one** block is per-state rather than per-block:
//!   `minecraft:decorated_pot`, whose `CRACKED` states swap the break sound for
//!   `block.decorated_pot.shatter` (`DecoratedPotBlock` is the sole
//!   `getSoundType(BlockState)` override in the game). The table is therefore
//!   state-keyed, not block-keyed.

use crate::generated_sound_types as table;
use crate::sound_events;

pub use table::{ENTRY_COUNT, STATE_COUNT};

/// The registry name of `SoundEvents.EMPTY`, the "there is no sound here"
/// sentinel vanilla stores in a `SoundType` slot it does not want to fill.
///
/// It *is* a real `minecraft:sound_event` registry entry, so it round-trips
/// through [`crate::sound_events`] like any other — it simply has no
/// `sounds.json` entry and therefore no sample.
pub const EMPTY_SOUND: &str = "minecraft:intentionally_empty";

/// The five surface sounds and the volume/pitch pair vanilla's `SoundType`
/// carries, for one block state.
///
/// The sound fields are `minecraft:sound_event` registry ids; resolve them with
/// the `*_sound_name` accessors, which go through [`crate::sound_events`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockSoundType {
    /// `SoundType.getVolume()`. `1.0` for all but `ANVIL` (`0.3`).
    pub volume: f32,
    /// `SoundType.getPitch()`. `1.0` for all but `METAL` (`1.5`).
    pub pitch: f32,
    /// `SoundType.getBreakSound()` — `LEVEL_EVENT` 2001's sound.
    pub break_sound: u16,
    /// `SoundType.getStepSound()` — footsteps.
    pub step_sound: u16,
    /// `SoundType.getPlaceSound()` — `BlockItem.place`'s sound.
    pub place_sound: u16,
    /// `SoundType.getHitSound()` — the tick-rate sound while mining.
    pub hit_sound: u16,
    /// `SoundType.getFallSound()` — landing on the surface.
    pub fall_sound: u16,
}

impl BlockSoundType {
    /// Vanilla's volume for a break or place sound: `(volume + 1.0) / 2.0`.
    ///
    /// The identical expression appears in **both** vanilla call sites —
    /// `LevelEventHandler.levelEvent` (`case 2001`, the break) and
    /// `BlockItem.place` (placement) — so it belongs here rather than being
    /// retyped at each consumer. For the 124 sound types with `volume = 1.0` this
    /// is `1.0`; for `ANVIL` it is `0.65`.
    #[must_use]
    pub fn break_or_place_volume(self) -> f32 {
        (self.volume + 1.0) / 2.0
    }

    /// Vanilla's pitch for a break or place sound: `pitch * 0.8`.
    ///
    /// Same two call sites as [`Self::break_or_place_volume`]. `0.8` for the 125
    /// sound types with `pitch = 1.0`; `1.2` for `METAL`.
    #[must_use]
    pub fn break_or_place_pitch(self) -> f32 {
        self.pitch * 0.8
    }

    /// `minecraft:*` identifier of [`Self::break_sound`], or `None` if the id is
    /// outside the sound-event registry (which the generated table makes
    /// impossible — the oracle refuses to emit an unregistered id).
    #[must_use]
    pub fn break_sound_name(self) -> Option<&'static str> {
        sound_events::sound_event_name(i32::from(self.break_sound))
    }

    /// `minecraft:*` identifier of [`Self::step_sound`].
    #[must_use]
    pub fn step_sound_name(self) -> Option<&'static str> {
        sound_events::sound_event_name(i32::from(self.step_sound))
    }

    /// `minecraft:*` identifier of [`Self::place_sound`].
    #[must_use]
    pub fn place_sound_name(self) -> Option<&'static str> {
        sound_events::sound_event_name(i32::from(self.place_sound))
    }

    /// `minecraft:*` identifier of [`Self::hit_sound`].
    #[must_use]
    pub fn hit_sound_name(self) -> Option<&'static str> {
        sound_events::sound_event_name(i32::from(self.hit_sound))
    }

    /// `minecraft:*` identifier of [`Self::fall_sound`].
    #[must_use]
    pub fn fall_sound_name(self) -> Option<&'static str> {
        sound_events::sound_event_name(i32::from(self.fall_sound))
    }

    /// Whether `name` is the [`EMPTY_SOUND`] sentinel, i.e. a slot vanilla
    /// deliberately left unfilled. Playing it is harmless but pointless.
    #[must_use]
    pub fn is_empty_sound(name: &str) -> bool {
        name == EMPTY_SOUND
    }
}

/// The `SoundType` for block-state `id`, or `None` if `id` is not in
/// `0..`[`STATE_COUNT`].
///
/// Zero-heap: two rodata reads, no search. Note that this answers for **air**
/// too (see the module gotchas) — the caller decides whether a sound is
/// appropriate.
#[must_use]
pub fn sound_type(id: u32) -> Option<BlockSoundType> {
    let &entry = table::STATE_ENTRY.get(id as usize)?;
    let (volume, pitch, break_sound, step_sound, place_sound, hit_sound, fall_sound) =
        table::ENTRIES[entry as usize];
    Some(BlockSoundType {
        volume,
        pitch,
        break_sound,
        step_sound,
        place_sound,
        hit_sound,
        fall_sound,
    })
}

/// The break sound name for block-state `id`, ready to hand to a sound engine.
///
/// `None` when `id` is out of range **or** the sound is the
/// [`EMPTY_SOUND`] sentinel — the two cases a caller would otherwise have to
/// distinguish by hand before every play, and both mean "nothing to play".
#[must_use]
pub fn break_sound_name(id: u32) -> Option<&'static str> {
    let name = sound_type(id)?.break_sound_name()?;
    (!BlockSoundType::is_empty_sound(name)).then_some(name)
}

/// The place sound name for block-state `id`. Same `None` contract as
/// [`break_sound_name`].
#[must_use]
pub fn place_sound_name(id: u32) -> Option<&'static str> {
    let name = sound_type(id)?.place_sound_name()?;
    (!BlockSoundType::is_empty_sound(name)).then_some(name)
}

/// The step sound name for block-state `id`. Same `None` contract as
/// [`break_sound_name`].
///
/// Nothing in the tree plays footsteps yet — they are per-tick, per-surface and
/// distance-gated, and the *producer* is the missing half (see
/// `docs/sound-playback.md`). This accessor exists because the data half is now
/// free, not because a caller is waiting.
#[must_use]
pub fn step_sound_name(id: u32) -> Option<&'static str> {
    let name = sound_type(id)?.step_sound_name()?;
    (!BlockSoundType::is_empty_sound(name)).then_some(name)
}
