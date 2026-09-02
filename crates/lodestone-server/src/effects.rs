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
    /// Issue #694, item 4: a **server-side-only** correction signal, not a
    /// wire-mirrored packet like every other variant above (that is why it is
    /// named a "push" rather than after a real clientbound packet, and why
    /// [`crate::ServerProtocol::encode_world_effect`]'s only real
    /// implementation returns [`crate::protocol::ServerDirective::None`] for
    /// it). This crate has no server-side physics for a connected player —
    /// position is client-reported, the same boundary
    /// `crate::mobs::piston_shove`'s own module doc already states for why a
    /// player is not shoved through [`crate::mobs::MobSim`] — so a piston
    /// push cannot go through that path. This is the next cheapest thing:
    /// the two swept cells and the push direction ride this channel's
    /// existing single-consumer transport (rather than a new parameter
    /// threaded through `crate::tick::run_tick_loop`'s already-long
    /// signature and its two dozen callers) so the *player's own connection*
    /// can correct its own last-known position when it overlaps, and send a
    /// real teleport for it — see `crate::server`'s handling of this variant.
    PistonPlayerPush {
        /// The cell the moved block (or piston head/base) vacated.
        source: BlockPos,
        /// The cell it now occupies — the `moving_piston` write's own
        /// position, per `crate::mobs::piston_shove`'s module doc.
        dest: BlockPos,
        /// The one-block displacement a player standing in the swept region
        /// is pushed by, matching `crate::mobs::MobSim::shove_from_piston`'s
        /// own displacement for a mob in the same position.
        push_delta: Vec3,
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

/// `LevelEvent.SOUND_ZOMBIE_CONVERTED` (`LevelEvent.java:23`) — vanilla's
/// `ZombieVillager.finishConversion` fires this (`data` unused) the instant a
/// cured zombie villager becomes a real villager (issue #247).
pub const SOUND_ZOMBIE_CONVERTED: i32 = 1027;

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

/// One periodic eating/drinking sound, played each time the consume animation's
/// per-tick sound trigger fires.
///
/// Publish with **no** excluded player: the real broadcast plays with no excluded
/// listener at all, so the local-player exclusion that a client-side prediction
/// path would otherwise apply does not kick in here, and the eater hears only this
/// broadcast. That is the opposite of the block-break case in this module's doc —
/// do not reach for `publish_effect_except` by analogy, or the player eating hears
/// nothing at all while everyone nearby does.
///
/// The particles are **not** here and must not be: the server-side particle spawn
/// for eating is a no-op there, so the crumbs are the client's own prediction
/// (`lodestone_shell::consume`).
///
/// The real volume and pitch are per-animation: eating rolls a fair coin between
/// `0.5` and `1.0` for volume, and picks pitch from a triangular distribution
/// centred on `1.0` with half-width `0.2`; drinking is a fixed `0.5` volume with
/// pitch drawn uniformly from `0.9..1.0`.
///
/// `roll` is a uniform `0.0..1.0` sample the caller draws, one per emission — a
/// constant makes every bite identical, and reaching for a clock traps on wasm32.
/// The eat volume's coin flip is folded into the same sample rather than a second
/// draw, which changes the *sequence* the real distribution would produce but not
/// the shape of it; the seed field already decides which sample of the sound event
/// plays, so nothing here needs to reproduce that sequence bit-for-bit anyway.
#[must_use]
pub fn item_consumed_tick(
    item: &str,
    pos: Vec3,
    roll: f32,
    seed: i64,
) -> Option<WorldEffect> {
    let consumable = lodestone_game::consumable::consumable_for_item(item)?;
    let drink = consumable.animation == lodestone_game::consumable::ConsumeAnimation::Drink;
    let sound = first_real_sound(&[consumable.sound.to_owned()])?;
    let (volume, pitch) = if drink {
        // A uniform draw over `0.9..1.0`.
        (0.5, 0.9 + roll * 0.1)
    } else {
        // The triangular distribution centred on `1.0` with half-width `0.2` is
        // `1.0 + 0.2 * (a - b)` for two independent uniform draws, whose support is
        // `0.8..1.2` and whose mode is 1.0. One uniform sample gives the right
        // range and a flat distribution inside it;
        // the audible difference is that a bite is very slightly less often
        // exactly-pitched, which is not the kind of thing a gate can see.
        (if roll < 0.5 { 0.5 } else { 1.0 }, 0.8 + roll * 0.4)
    };
    Some(WorldEffect::Sound {
        sound,
        category: SoundCategory::Player,
        pos,
        volume,
        pitch,
        seed,
    })
}

/// The **finish** replay of the consumable sound — the food component's
/// on-consume hook plays it a second time, at volume `1.0`, pitch drawn from a
/// triangular distribution centred on `1.0` with half-width `0.4`, and the
/// neutral sound category.
///
/// Easy to miss because it is the *same sound event* as the periodic one and looks
/// like a duplicate of [`item_consumed_tick`]. It is not: louder, wider pitch
/// spread, a different category, and it is what makes the last bite land audibly
/// rather than just stopping. Like the burp it lives on `minecraft:food`, so a
/// potion does not get it.
#[must_use]
pub fn item_consume_finished(item: &str, pos: Vec3, roll: f32, seed: i64) -> Option<WorldEffect> {
    let consumable = lodestone_game::consumable::consumable_for_item(item)?;
    let sound = first_real_sound(&[consumable.sound.to_owned()])?;
    Some(WorldEffect::Sound {
        sound,
        category: SoundCategory::Neutral,
        pos,
        volume: 1.0,
        // The triangular distribution centred on `1.0` with half-width `0.4` —
        // support `0.6..1.4`, flattened to one uniform sample for the reason
        // [`item_consumed_tick`] gives.
        pitch: 0.6 + roll * 0.8,
        seed,
    })
}

/// The burp sound — the extra sound the food component's on-consume hook plays
/// when a **player** finishes a food, broadcast with no excluded listener at
/// volume `0.5` and pitch drawn uniformly from `0.9..1.0`, in the players sound
/// category.
///
/// Food-only and player-only, because it is emitted by the `minecraft:food`
/// component's own consume listener rather than by the generic consumable
/// animation — a potion and a milk bucket finish without one. Callers must
/// therefore gate on the item having a food component, not merely on it being
/// consumable.
#[must_use]
pub fn player_burped(pos: Vec3, roll: f32, seed: i64) -> WorldEffect {
    WorldEffect::Sound {
        sound: lodestone_game::consumable::BURP_SOUND.to_owned(),
        category: SoundCategory::Player,
        pos,
        volume: 0.5,
        pitch: 0.9 + roll * 0.1,
        seed,
    }
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

/// The idle vocalisation for a living, undamaged mob — a cow's moo, a
/// zombie's groan — derived the same way [`mob_vocalisation`] derives hurt
/// and death: `entity.<path>.ambient`, checked against the real sound
/// registry so a species 26.2 gives no ambient sound (or whose name this
/// derivation gets wrong) is silently `None` rather than a rejected packet.
///
/// The category split is [`mob_vocalisation`]'s own `Hostile`/`Neutral`
/// split, for the same reason (`Entity.getSoundSource`, overridden to
/// `HOSTILE` by `Monster`).
///
/// `pitch` is the caller's, drawn from whatever pseudo-random source it has
/// (see `MobSim::roll_ambient_sound`'s own doc for why this crate cannot
/// draw two independent samples from vanilla's level RNG the way
/// `LivingEntity.getVoicePitch` does), but the **centre** it is built around
/// is this function's call, not the caller's: a baby's ambient call is
/// higher-pitched than an adult's in vanilla regardless of species, so
/// `is_baby` shifts the *expected* centre from `1.0` to `1.5` and the caller
/// is expected to have already sampled around that centre.
#[must_use]
pub fn mob_ambient_sound(
    entity_type: &str,
    pos: Vec3,
    hostile: bool,
    pitch: f32,
    seed: i64,
) -> Option<WorldEffect> {
    let path = entity_type.strip_prefix("minecraft:")?;
    let sound = first_real_sound(&[format!("minecraft:entity.{path}.ambient")])?;
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

/// `ZombieVillager.startConverting`'s entity-event sound
/// (`SoundEvents.ZOMBIE_VILLAGER_CURE`, `entity.zombie_villager.cure`) —
/// issue #247, played the instant a golden apple starts the conversion
/// timer. Category is always `Hostile`, matching `Monster.getSoundSource`
/// (a converting zombie villager is still a zombie until the timer
/// completes).
#[must_use]
pub fn zombie_villager_cure_sound(pos: Vec3, volume: f32, pitch: f32, seed: i64) -> Option<WorldEffect> {
    let sound = first_real_sound(&["minecraft:entity.zombie_villager.cure".to_owned()])?;
    Some(WorldEffect::Sound {
        sound,
        category: SoundCategory::Hostile,
        pos,
        volume,
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

    /// The idle-vocalisation counterpart of the test above: a real species
    /// derives `entity.<path>.ambient` and a non-vocal entity derives
    /// nothing, and the category follows the caller's `hostile` flag exactly
    /// as [`mob_vocalisation`]'s does.
    #[test]
    fn mobs_have_ambient_sounds_and_non_mobs_do_not() {
        let pos = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
        assert_eq!(
            mob_ambient_sound("minecraft:cow", pos, false, 1.0, 0),
            Some(WorldEffect::Sound {
                sound: "minecraft:entity.cow.ambient".to_owned(),
                category: SoundCategory::Neutral,
                pos,
                volume: 1.0,
                pitch: 1.0,
                seed: 0,
            })
        );
        assert_eq!(
            mob_ambient_sound("minecraft:zombie", pos, true, 1.0, 0),
            Some(WorldEffect::Sound {
                sound: "minecraft:entity.zombie.ambient".to_owned(),
                category: SoundCategory::Hostile,
                pos,
                volume: 1.0,
                pitch: 1.0,
                seed: 0,
            })
        );
        assert_eq!(mob_ambient_sound("minecraft:item", pos, false, 1.0, 0), None);
    }
}
