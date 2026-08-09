//! Sounds, particles and level events the **server** owns (issue #530).
//!
//! # What it is
//!
//! Until this module existed the `ServerProtocol` trait had no sound encoder and
//! no particle encoder, so the integrated server emitted no `sound`, no
//! `level_event` and no `level_particles` packet, ever. Anything the client
//! cannot predict for itself was therefore silent and invisible: a mob taking
//! damage, a door opened by redstone, a composter filled by another player.
//!
//! This module is the version-free description of one such effect
//! ([`WorldEffect`]) plus the derivations that turn a world change into one —
//! nothing here writes a byte. The three
//! [`crate::ServerProtocol`] encoders do that.
//!
//! # How it works
//!
//! Every sound name is *derived* from the block or entity id and then **checked
//! against `lodestone_data::sound_events`**, the jar-derived
//! `minecraft:sound_event` registry. A derivation that names a sound 26.2 does
//! not have yields `None` and the effect is simply not sent, rather than a
//! packet the client rejects. That is what makes a per-material family
//! (`block.wooden_door.open` vs `block.bamboo_wood_door.open`) safe to derive
//! by string rather than by a census column of its own: the fallback chain is
//! validated at each step.
//!
//! # How to change it
//!
//! To add an effect, add a derivation here and call it from the publisher —
//! `crate::tick::run_tick_loop` for anything the world tick produces,
//! `crate::mobs::MobSim` for anything a mob does. **The transport is
//! [`crate::BlockTickFeed`]'s effect lane**, not a feed of its own: every
//! `serve_connection*` variant already carries that feed, and an effect is the
//! same kind of thing as the block update beside it — something the world tick
//! did that this connection must be told about. See that type's own doc.
//!
//! The gotcha is **double-triggering**: `lodestone-shell` predicts its own
//! block-break and block-place sounds locally (`docs/block-sound-types.md`), so
//! an effect the acting client would also predict must not reach *that* client.
//! Publish it through `BlockTickFeed::publish_effect_except` with the
//! acting player's uuid — vanilla's own `except` argument on
//! `Level.playSound`/`Level.levelEvent` — and every other player still hears it.
//! `publish_effect` is for effects with
//! no acting player at all.

use lodestone_model::{BlockPos, SoundCategory, Vec3, Vec3f};

/// One sound, particle burst or level event for a connection to be told about.
///
/// Mirrors the three clientbound packets one-for-one (`sound`, `level_event`,
/// `level_particles`) rather than being an abstraction over them: the mapping to
/// wire fields is the protocol implementor's job, and a lossy intermediate here
/// would just have to be un-lost there.
#[derive(Debug, Clone, PartialEq)]
pub enum WorldEffect {
    /// Vanilla `ClientboundSoundPacket` — a positioned sound from the
    /// `minecraft:sound_event` registry.
    Sound {
        /// Sound event id, e.g. `minecraft:entity.zombie.hurt`.
        sound: String,
        /// Which volume slider it obeys.
        category: SoundCategory,
        /// World position. The wire form is `(int)(block * 8)`, so this is
        /// quantised to eighths of a block by the encoder.
        pos: Vec3,
        /// Volume multiplier; also sets the audible radius (`volume * 16`).
        volume: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Vanilla's per-play `random.nextLong()`, which picks between a sound
        /// event's variants. A constant would make every play identical.
        seed: i64,
    },
    /// Vanilla `ClientboundLevelEventPacket` — one of the numbered composite
    /// effects in `LevelEvent.java`, several of which (notably
    /// [`PARTICLES_DESTROY_BLOCK`]) are a sound *and* a particle burst in a
    /// single packet.
    LevelEvent {
        /// The `LevelEvent.java` constant.
        event: i32,
        /// Block position the effect happens at.
        pos: BlockPos,
        /// Event-specific payload — a block-state id for
        /// [`PARTICLES_DESTROY_BLOCK`], `0` for most others.
        data: i32,
        /// `true` to reach every player regardless of distance (vanilla uses
        /// this for the wither spawn and the dragon death only).
        global: bool,
    },
    /// Vanilla `ClientboundLevelParticlesPacket` — a burst of one particle type
    /// with a randomised spread.
    Particles {
        /// Particle type id, e.g. `minecraft:crit`.
        particle: String,
        /// Centre of the burst.
        pos: Vec3,
        /// Per-axis Gaussian spread bound.
        offset: Vec3f,
        /// Vanilla's `maxSpeed`; for several types this is repurposed as a
        /// type-specific scalar rather than a speed.
        max_speed: f32,
        /// How many particles. `0` has a special meaning for some types
        /// (direction taken from `offset` instead of randomised).
        count: i32,
        /// Bypass the client's particle-distance limiter.
        long_distance: bool,
    },
    /// Vanilla `ClientboundBlockEntityDataPacket` — one block entity's update tag,
    /// republished for a cell whose *record* changed without the chunk being
    /// resent.
    ///
    /// The odd one out in this enum: it is not a sound or a particle, and it is
    /// here because this is the lane that is drained **after** the block-change
    /// lane. That order is load-bearing for anything whose record must land on a
    /// state the same batch established — a moving piston, whose block state alone
    /// says nothing about which block is travelling. Giving it a lane of its own
    /// would mean choosing that order again somewhere else.
    BlockEntityData {
        /// The cell the record belongs to.
        pos: BlockPos,
        /// The `minecraft:block_entity_type` registry key — **the entity's key,
        /// not the block's**. A `moving_piston` block carries a
        /// `minecraft:piston` block entity.
        block_entity_type: String,
        /// The `getUpdateTag` payload, as a nameless compound.
        nbt: lodestone_core::Nbt,
    },
}

/// `LevelEvent.PARTICLES_DESTROY_BLOCK`
/// (`.cache/mc/26.2/src/net/minecraft/world/level/block/LevelEvent.java:56`).
///
/// The one level event worth knowing by heart: its `data` is a **block-state
/// id**, and the client's handler plays the block's break sound *and* spawns its
/// destroy particles. `Level.destroyBlock` sends exactly this and no separate
/// sound packet, which is why breaking a block needs one effect rather than two.
pub const PARTICLES_DESTROY_BLOCK: i32 = 2001;

/// `LevelEvent.COMPOSTER_FILL` (`LevelEvent.java:47`). `data` is `1` when the
/// insert raised the composter's level (`ComposterBlock`'s own
/// `level.levelEvent(LevelEvent.COMPOSTER_FILL, pos, success ? 1 : 0)`).
pub const COMPOSTER_FILL: i32 = 1500;

/// `LevelEvent.SOUND_BREWING_STAND_BREW` (`LevelEvent.java:36`). `data` unused.
pub const SOUND_BREWING_STAND_BREW: i32 = 1035;

/// Strips any `[...]` property suffix, as every canonical-name comparison in
/// this crate does.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The value of `state`'s `key=` property, if it carries one.
fn property_of<'s>(state: &'s str, key: &str) -> Option<&'s str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// `true` iff `name` is a real entry in 26.2's `minecraft:sound_event` registry.
///
/// The guard every derivation in this module ends with — see the module doc.
/// Backed by a set built once from the generated table (~1,500 entries), so a
/// derivation costs a hash rather than a scan.
#[must_use]
pub fn sound_exists(name: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            (0..lodestone_data::sound_events::SOUND_EVENT_COUNT as i32)
                .filter_map(lodestone_data::sound_events::sound_event_name)
                .collect()
        })
        .contains(name)
}

/// The first name in `candidates` that 26.2 actually has, as an owned `String`.
fn first_real_sound(candidates: &[String]) -> Option<String> {
    candidates.iter().find(|name| sound_exists(name)).cloned()
}

/// The level event for a block destroyed at `pos`, or `None` if `state` does not
/// resolve to a block-state id.
///
/// One packet, not two: see [`PARTICLES_DESTROY_BLOCK`]. Publish it with the
/// breaker as the `except` player — see the module doc's double-trigger note.
#[must_use]
pub fn block_destroyed(pos: BlockPos, state: &str) -> Option<WorldEffect> {
    let id = crate::mobs::block_state_id_or_default(state)?;
    Some(WorldEffect::LevelEvent {
        event: PARTICLES_DESTROY_BLOCK,
        pos,
        data: i32::try_from(id).ok()?,
        global: false,
    })
}

/// The place sound for `state` at `pos` — vanilla's
/// `BlockItem.place`/`Block.setPlacedBy` pair, which plays
/// `soundType.getPlaceSound()` at `(volume + 1) / 2` and `pitch * 0.8`
/// (`BlockItem.java`'s `SoundType`-derived call).
#[must_use]
pub fn block_placed(pos: BlockPos, state: &str, seed: i64) -> Option<WorldEffect> {
    let id = crate::mobs::block_state_id_or_default(state)?;
    let sound = lodestone_data::sound_types::place_sound_name(id)?;
    let kind = lodestone_data::sound_types::sound_type(id)?;
    Some(WorldEffect::Sound {
        sound: sound.to_owned(),
        category: SoundCategory::Block,
        pos: block_centre(pos),
        volume: (kind.break_or_place_volume() + 1.0) / 2.0,
        pitch: kind.break_or_place_pitch() * 0.8,
        seed,
    })
}

/// The open/close sound for a door, trapdoor or fence gate whose state went
/// from `from` to `to`, or `None` if that pair is not an open/close toggle.
///
/// `DoorBlock.playSound` (`DoorBlock.java:247-248`) is a real
/// `level.playSound(…, SoundSource.BLOCKS, 1.0F, random.nextFloat() * 0.1F +
/// 0.9F)` — **not** a level event, which is the thing worth checking before
/// reaching for a `LevelEvent.SOUND_*` constant. `TrapDoorBlock.playSound`
/// (`:118-121`) and `FenceGateBlock` (`:158`) are the same shape.
///
/// The sound *name* is derived from the block id and validated (module doc):
/// `minecraft:iron_door` has its own event, the modern woods
/// (bamboo/cherry/pale/nether/copper) have theirs, and everything else falls
/// back to the generic `block.wooden_*` family. `pitch` is the caller's, since
/// vanilla draws it from the level RNG.
#[must_use]
pub fn openable_toggled(pos: BlockPos, from: &str, to: &str, pitch: f32) -> Option<WorldEffect> {
    let block = base_name(to);
    if base_name(from) != block {
        return None;
    }
    let was_open = property_of(from, "open")? == "true";
    let is_open = property_of(to, "open")? == "true";
    if was_open == is_open {
        return None;
    }
    let action = if is_open { "open" } else { "close" };

    let path = block.strip_prefix("minecraft:")?;
    let (family, generic) = if path.ends_with("_door") || path == "door" {
        ("door", "block.wooden_door")
    } else if path.ends_with("_trapdoor") || path == "trapdoor" {
        ("trapdoor", "block.wooden_trapdoor")
    } else if path.ends_with("_fence_gate") || path == "fence_gate" {
        ("fence_gate", "block.fence_gate")
    } else {
        return None;
    };
    // `block.iron_door.open` and `block.copper_door.open` are per-block;
    // `block.bamboo_wood_door.open` is per *wood type*, so the material prefix
    // is the block id minus the family suffix.
    let material = path.trim_end_matches(family).trim_end_matches('_');
    let sound = first_real_sound(&[
        format!("minecraft:block.{path}.{action}"),
        format!("minecraft:block.{material}_wood_{family}.{action}"),
        format!("minecraft:{generic}.{action}"),
    ])?;
    Some(WorldEffect::Sound {
        sound,
        category: SoundCategory::Block,
        pos: block_centre(pos),
        volume: 1.0,
        pitch,
        seed: 0,
    })
}

/// The hurt or death sound for an entity of type `entity_type`, or `None` for a
/// type 26.2 gives no such sound (every non-living entity, plus the silent mobs).
///
/// `LivingEntity.hurt` plays `getHurtSound()` and `LivingEntity.die` plays
/// `getDeathSound()`; both are per-class constants of the form
/// `entity.<path>.hurt` / `entity.<path>.death`, so deriving the name and
/// checking it against the registry (module doc) is exact for every mob that has
/// one and correctly silent for the rest.
///
/// The category is `Hostile` or `Neutral` following vanilla's own
/// `Entity.getSoundSource` split (`Monster` overrides it to `HOSTILE`).
#[must_use]
pub fn mob_vocalisation(
    entity_type: &str,
    pos: Vec3,
    died: bool,
    hostile: bool,
    pitch: f32,
    seed: i64,
) -> Option<WorldEffect> {
    let path = entity_type.strip_prefix("minecraft:")?;
    let action = if died { "death" } else { "hurt" };
    let sound = first_real_sound(&[format!("minecraft:entity.{path}.{action}")])?;
    Some(WorldEffect::Sound {
        sound,
        category: if hostile {
            SoundCategory::Hostile
        } else {
            SoundCategory::Neutral
        },
        pos,
        volume: 1.0,
        pitch,
        seed,
    })
}

/// The centre of the block at `pos` — where vanilla's
/// `Level.playSound(…, BlockPos, …)` overload puts a block sound
/// (`pos.getX() + 0.5` and so on).
fn block_centre(pos: BlockPos) -> Vec3 {
    Vec3 {
        x: f64::from(pos.x) + 0.5,
        y: f64::from(pos.y) + 0.5,
        z: f64::from(pos.z) + 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation guard actually discriminates: a real sound passes, an
    /// invented one does not. Without this every `first_real_sound` chain would
    /// silently return its first candidate.
    #[test]
    fn only_real_sound_events_pass_the_guard() {
        assert!(sound_exists("minecraft:entity.zombie.hurt"));
        assert!(sound_exists("minecraft:block.wooden_door.open"));
        assert!(!sound_exists("minecraft:entity.zombie.not_a_sound"));
        assert!(!sound_exists("minecraft:block.oak_door.open"));
    }

    /// Each openable family lands on the name 26.2 really has — and the three
    /// answers are *different*, so a chain that always took its first or last
    /// candidate would fail here.
    #[test]
    fn openable_sounds_resolve_per_material() {
        let sound = |from: &str, to: &str| match openable_toggled(BlockPos::new(1, 2, 3), from, to, 1.0) {
            Some(WorldEffect::Sound { sound, .. }) => sound,
            other => panic!("expected a sound, got {other:?}"),
        };
        // Generic wood: `block.oak_door.open` does not exist, the family one does.
        assert_eq!(
            sound("minecraft:oak_door[open=false]", "minecraft:oak_door[open=true]"),
            "minecraft:block.wooden_door.open"
        );
        // Per-block.
        assert_eq!(
            sound("minecraft:iron_door[open=true]", "minecraft:iron_door[open=false]"),
            "minecraft:block.iron_door.close"
        );
        // Per-wood-type.
        assert_eq!(
            sound("minecraft:bamboo_door[open=false]", "minecraft:bamboo_door[open=true]"),
            "minecraft:block.bamboo_wood_door.open"
        );
        assert_eq!(
            sound(
                "minecraft:oak_trapdoor[open=false]",
                "minecraft:oak_trapdoor[open=true]"
            ),
            "minecraft:block.wooden_trapdoor.open"
        );
        assert_eq!(
            sound(
                "minecraft:oak_fence_gate[open=false]",
                "minecraft:oak_fence_gate[open=true]"
            ),
            "minecraft:block.fence_gate.open"
        );

        // Not a toggle: same `open`, a different property changing, or a block
        // with no `open` at all.
        assert!(
            openable_toggled(
                BlockPos::new(1, 2, 3),
                "minecraft:oak_door[open=true,powered=false]",
                "minecraft:oak_door[open=true,powered=true]",
                1.0
            )
            .is_none()
        );
        assert!(openable_toggled(BlockPos::new(1, 2, 3), "minecraft:stone", "minecraft:dirt", 1.0).is_none());
    }

    /// A break carries the broken block's own state id, since that is what the
    /// client resolves the particle texture and break sound from.
    #[test]
    fn a_destroyed_block_carries_its_state_id() {
        let expected = crate::mobs::block_state_id_or_default("minecraft:stone").expect("stone resolves");
        assert_eq!(
            block_destroyed(BlockPos::new(4, 5, 6), "minecraft:stone"),
            Some(WorldEffect::LevelEvent {
                event: PARTICLES_DESTROY_BLOCK,
                pos: BlockPos::new(4, 5, 6),
                data: expected as i32,
                global: false,
            })
        );
    }

    /// Living mobs vocalise, non-living entities do not — and the death and hurt
    /// sounds are distinct names, not one reused.
    #[test]
    fn mobs_vocalise_and_non_mobs_do_not() {
        let pos = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
        let name = |ty: &str, died: bool| match mob_vocalisation(ty, pos, died, false, 1.0, 0) {
            Some(WorldEffect::Sound { sound, .. }) => Some(sound),
            None => None,
            other => panic!("expected a sound, got {other:?}"),
        };
        assert_eq!(name("minecraft:cow", false).as_deref(), Some("minecraft:entity.cow.hurt"));
        assert_eq!(name("minecraft:cow", true).as_deref(), Some("minecraft:entity.cow.death"));
        assert_eq!(name("minecraft:item", false), None);
    }
}
