//! `Sim`'s audio cluster: entity-sound position resolution, the audio
//! listener push, and block break/place sound playback — seam 5 of the
//! sim.rs decomposition sequence (seam 1 was the test module, `sim/tests.rs`;
//! seam 2 was placement prediction, `sim/placement.rs`; seam 3 was the
//! interaction/combat cluster, `sim/actions.rs`; seam 4 was the net-apply
//! cluster, `sim/net_apply.rs`).
//!
//! `use super::*;` for the same reason every other seam file uses it:
//! `sim::audio` is a descendant of `sim` and already has the same visibility
//! into `Sim`'s private fields and `sim.rs`'s other private helpers
//! (`is_air_state`, brought in through `sim.rs`'s own private `use
//! placement::{..}`) that `sim::actions`/`sim::net_apply`/`sim::tests` have.
//!
//! `entity_sound_position` and `play_block_break_sound`/`play_block_place_sound`
//! widen from private to `pub(crate)`: `sim/net_apply.rs`'s `poll_net` calls
//! `entity_sound_position` and `play_block_break_sound` (both across a sibling
//! boundary now), and `sim/actions.rs`'s `break_block`/`use_item_generic` call
//! `play_block_break_sound`/`play_block_place_sound` the same way. `set_audio_listener`
//! was already `pub` (called from `app.rs`); `play_block_surface_sound` and
//! `block_sound_seed` stay private — their only callers are the two sound
//! methods above, which moved here with them.

use super::*;

impl Sim {
    /// World-space origin for an entity-attached sound: the entity's live feet
    /// position raised half a block so the source sits at body centre. Falls
    /// back to the player's current position if the entity is unknown (so the
    /// sound is still heard rather than dropped) — the same "audible, not
    /// silent" preference the live gate encodes.
    pub(crate) fn entity_sound_position(&self, entity_id: i32) -> glam::Vec3 {
        if let Some(net) = &self.net
            && let Some(snap) = net
                .entity_snapshots()
                .into_iter()
                .find(|s| s.id == entity_id)
        {
            return snap.feet + glam::Vec3::new(0.0, 0.5, 0.0);
        }
        let p = self.player().position;
        glam::Vec3::new(p.x as f32, p.y as f32, p.z as f32)
    }

    /// Push the listener transform to the audio engine from the render camera.
    /// Called once per frame by [`crate::app`] with the exact interpolated
    /// camera it renders, so what the player hears matches what they see.
    pub fn set_audio_listener(&self, camera: &Camera) {
        if let Some(audio) = &self.audio {
            audio.set_listener(camera);
        }
    }

    /// Play a block's break sound at the centre of `block`, the half of vanilla's
    /// `LevelEventHandler` `case 2001` this shell used to drop on the floor.
    ///
    /// `case 2001` does *two* things with the state id the event carries
    /// (`LevelEventHandler.java:283-291`): `addDestroyBlockEffect` **and**
    /// `playLocalSound(pos, soundType.getBreakSound(), SoundSource.BLOCKS, …)`.
    /// Only the first was wired, so every block break in the game was visually
    /// right and silent — from an event already decoded, routed and handled. See
    /// `docs/sound-playback.md`.
    pub(crate) fn play_block_break_sound(&mut self, block: [i32; 3], state: u32) {
        self.play_block_surface_sound(block, state, lodestone_data::sound_types::break_sound_name);
    }

    /// Play a block's place sound at the centre of `block` — vanilla's
    /// `BlockItem.place` tail (`BlockItem.java:87`), which passes the placing
    /// player as the *excluded* entity, so on the acting client the sound is
    /// **predicted** rather than received. (`ClientLevel.playSound` inverts the
    /// exclusion: it plays only when `except == minecraft.player`.) Another
    /// player's placement arrives as an ordinary `SOUND` packet and is already
    /// audible through the [`NetUpdate::Sound`] arm.
    pub(crate) fn play_block_place_sound(&mut self, block: [i32; 3], state: u32) {
        self.play_block_surface_sound(block, state, lodestone_data::sound_types::place_sound_name);
    }

    /// The shared body of the two above: resolve the block state's `SoundType`,
    /// pick one of its sounds, and play it at the block's centre with vanilla's
    /// break/place scaling.
    ///
    /// Three things here are vanilla's, not ours, and all three come from the
    /// same two call sites (`LevelEventHandler.java:288-289` and
    /// `BlockItem.java:87`):
    ///
    /// * the position is the **block centre** — `Level.playLocalSound(BlockPos, …)`
    ///   forwards `pos.getX() + 0.5` and so on (`Level.java:472-476`);
    /// * the volume is `(soundType.getVolume() + 1.0) / 2.0` and the pitch is
    ///   `soundType.getPitch() * 0.8`, both computed by
    ///   [`lodestone_data::sound_types::BlockSoundType`] so neither multiplier is
    ///   retyped per call site;
    /// * the category is `SoundSource.BLOCKS`.
    ///
    /// The **air guard** is vanilla's too (`case 2001`'s `if (!blockState.isAir())`)
    /// and is not redundant: air has a `SoundType` in the table — `STONE`, as it
    /// happens — so without it an air-state level event would play a stone break.
    fn play_block_surface_sound(
        &mut self,
        block: [i32; 3],
        state: u32,
        pick: fn(u32) -> Option<&'static str>,
    ) {
        if is_air_state(state) {
            return;
        }
        let Some(sound) = lodestone_data::sound_types::sound_type(state) else {
            return;
        };
        // `None` also covers `minecraft:intentionally_empty`, the sentinel vanilla
        // parks in a slot it does not want to fill (water, lava and bubble columns
        // are the three blocks with no break sound at all).
        let Some(name) = pick(state) else {
            return;
        };
        let seed = self.block_sound_seed(block);
        let volume = sound.break_or_place_volume();
        let pitch = sound.break_or_place_pitch();
        let Some(audio) = &mut self.audio else {
            return;
        };
        audio.play_sound(
            name,
            lodestone_model::event::SoundCategory::Block,
            glam::Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            ),
            volume,
            pitch,
            seed,
        );
    }

    /// A variant-selection seed for a sound this client decided to play.
    ///
    /// Vanilla uses `this.random.nextLong()` for a level event
    /// (`ClientLevel.java:723-733`), i.e. the variant is *client*-chosen and needs
    /// no cross-client agreement — unlike a `SOUND` packet's seed, which must be
    /// passed through unchanged (`lodestone-audio/src/select.rs`).
    ///
    /// So this is a `splitmix64` finalizer over the block position and the fixed
    /// tick count. Two properties are deliberate:
    ///
    /// * **not `Instant::now`** — `select.rs` rules it out (it panics on wasm), and
    ///   this crate's other RNG-free paths avoid `getrandom` for the same reason;
    /// * **not the particle engine's `JavaRandom`** (`Particles`' own
    ///   `engine.rng()`), even though it is already in scope at the break site.
    ///   Drawing from it would shift every subsequent particle draw, and the
    ///   destroy-burst golden tests (`mining_destroy_burst`,
    ///   `break_particle_tint`) are written against that exact sequence.
    ///
    /// Mixing in `ticks` rather than position alone is what stops re-breaking one
    /// cell from picking the same `.ogg` variant every time.
    fn block_sound_seed(&self, block: [i32; 3]) -> i64 {
        let mut x = (block[0] as i64 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (block[1] as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (block[2] as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
            ^ self.clock().ticks;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (x ^ (x >> 31)) as i64
    }
}
