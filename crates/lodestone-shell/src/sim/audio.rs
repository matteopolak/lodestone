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
//!
//! **The `audio: Option<ShellAudio>` field itself later moved out of `Sim`
//! entirely**, into the [`AudioEngine`](super::AudioEngine) resource
//! (`docs/sim-dissolution.md`) — a plain field, unlike every other item this
//! module's own doc above describes, was invisible to a `GameTick` **system**,
//! which is a free function over `&mut World` rather than a `Sim` method. The
//! accessors here (`Self::audio`/`Self::audio_mut`, in `sim.rs` beside
//! `Self::mining`/`Self::terrain`) read the resource instead of the field, so
//! every method in this file kept its signature; only the two direct
//! `&self.audio`/`&mut self.audio` reads changed. [`Self::play_local_sound`]
//! is new: the public, non-networked play path both the rain-ambience
//! producer and a plugin-driven placement's sound need.

use super::*;

impl Sim {
    /// World-space origin for an entity-attached sound: the entity's live feet
    /// position raised half a block so the source sits at body centre. Falls
    /// back to the player's current position if the entity is unknown (so the
    /// sound is still heard rather than dropped) — the same "audible, not
    /// silent" preference the live gate encodes.
    ///
    /// Issue #36: there is no `NetClient::entity_snapshots()` any more — the
    /// render-side fold this now reads, [`Self::entity_draws`], is the same
    /// `fold_entities` output every entity pixel gate reads, so a missing id
    /// (no connection, or a track the fold has not spawned yet) falls through
    /// to the player position exactly as before.
    pub(crate) fn entity_sound_position(&self, entity_id: i32) -> glam::Vec3 {
        if let Some(draw) = self.entity_draws().into_iter().find(|d| d.id == entity_id) {
            return draw.feet + glam::Vec3::new(0.0, 0.5, 0.0);
        }
        let p = self.player().position;
        glam::Vec3::new(p.x as f32, p.y as f32, p.z as f32)
    }

    /// Push the listener transform to the audio engine from the render camera.
    /// Called once per frame by [`crate::app`] with the exact interpolated
    /// camera it renders, so what the player hears matches what they see.
    pub fn set_audio_listener(&self, camera: &Camera) {
        self.audio(|audio| {
            if let Some(audio) = audio {
                audio.set_listener(camera);
            }
        });
    }

    /// This frame's sound-subtitle caption rows (issue #198), already translated
    /// against the loaded language table.
    ///
    /// Empty when audio is disabled or nothing is live, so the caller can assign
    /// it unconditionally. A key with no translation falls back to the raw key —
    /// the same degradation every other translated string in this crate takes,
    /// which keeps a jar-less run showing *something* rather than silently
    /// dropping the caption and looking like the feature is broken.
    pub fn sound_subtitles(
        &mut self,
        camera: &Camera,
    ) -> Vec<crate::audio::subtitles::SubtitleCaption> {
        // Two steps rather than one closure: `translator` borrows `&self` and
        // `audio_mut` wants `&mut self`, so the keys come out first and the table
        // is consulted after the mutable borrow has ended.
        let mut captions = self.audio_mut(|audio| match audio {
            Some(audio) => audio.subtitle_captions(camera),
            None => Vec::new(),
        });
        if !captions.is_empty() {
            let translate = self.translator();
            for c in &mut captions {
                if let Some(text) = translate(&c.text) {
                    c.text = text;
                }
            }
        }
        captions
    }

    /// Play a sound with no wire origin at all — a **local** decision, not a
    /// server one, matching vanilla's own `Level.playLocalSound` (the same
    /// call [`Self::play_block_break_sound`]/[`Self::play_block_place_sound`]
    /// forward to via [`Self::play_block_surface_sound`]).
    ///
    /// Added alongside [`AudioEngine`](super::AudioEngine) so a caller that
    /// cannot reach a `NetUpdate` at all — [`crate::app::WindowApp`]'s
    /// rain-ambience cadence (`lodestone_render::RainAmbience`, ticked from
    /// the render loop, never from the wire) and a plugin-driven placement's
    /// sound (`crate::interact::drive_placement`, a `GameTick` **system**,
    /// which reads [`AudioEngine`](super::AudioEngine) directly rather than
    /// through this method — a system is a free function over `&mut World`,
    /// not a `Sim` method) — has a public, non-`Sim`-internal way to play one.
    /// `seed` is the caller's: unlike a `SOUND` packet's seed, a local
    /// decision has no cross-client agreement to preserve, so nothing here
    /// picks one for you (see [`Self::block_sound_seed`] for the block-keyed
    /// derivation the two `play_block_*_sound` methods use, if that shape
    /// fits the caller).
    pub fn play_local_sound(
        &mut self,
        name: &str,
        category: lodestone_model::event::SoundCategory,
        pos: glam::Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.audio_mut(|audio| {
            if let Some(audio) = audio {
                audio.play_sound(name, category, pos, volume, pitch, seed);
            }
        });
    }

    /// Advance the music clock by however many 20 Hz ticks have elapsed, and start
    /// a track if vanilla's `MusicManager` says to.
    ///
    /// This is the call that stopped `#135`'s selector being an island: the whole
    /// biome table, the delay constants and the streaming resolve were built,
    /// tested and reached nothing, because nothing called them.
    ///
    /// # Two resources at one instant
    ///
    /// The tick needs [`MusicState`](super::MusicState) *and*
    /// [`AudioEngine`](super::AudioEngine) mutably together, which two
    /// `World::resource_mut` borrows cannot express. So the state is moved out of
    /// its slot for the duration and put back unconditionally — see
    /// [`MusicState`](super::MusicState)'s own doc for why that `Option` is a
    /// move-out slot and not a maybe.
    ///
    /// # The situation is the caller's
    ///
    /// `situation` decides everything: whether this is menu or in-world music,
    /// which biome's three-slot record applies, and whether the player counts as
    /// creative. Building it here would mean this method guessing at screen state
    /// it cannot see. See [`crate::audio::music::menu_situation`] and
    /// [`crate::audio::music::world_situation`], and note the two traps that live
    /// on the latter: the selector's input is **not** the biome id, and `creative`
    /// is **`instabuild && mayfly`**, not a gamemode check.
    pub(crate) fn tick_music(
        &mut self,
        now: std::time::Instant,
        situation: &lodestone_sound::music::MusicSituation<'_>,
    ) {
        self.write(|w| {
            let taken = w.resource_mut::<super::MusicState>().0.take();
            let Some(mut music) = taken else {
                // Only reachable from inside another `tick_music` on the same
                // world, which cannot happen — but re-entrancy silently losing
                // the music state would be a very confusing bug, so it is a
                // documented no-op rather than an `expect`.
                return;
            };
            {
                let mut audio = w.resource_mut::<super::AudioEngine>();
                music.advance(now, situation, audio.0.as_mut());
            }
            w.resource_mut::<super::MusicState>().0 = Some(music);
        });
    }

    /// Advance the ambience clock — cave mood, the biome/dimension loop and the
    /// rain cadence — and play whatever it decided.
    ///
    /// Same two-resources-at-one-instant shape as [`Self::tick_music`], and the
    /// same move-out slot for the same reason. What differs is that the *tick* is
    /// pure ([`crate::audio::ambient::ShellAmbience::tick`] returns events) and
    /// only [`crate::audio::ambient::ShellAmbience::submit`] needs the device, so
    /// the world reads the mood probe makes happen **outside** the ECS write
    /// guard rather than inside it.
    ///
    /// `weather` is the caller's because the shell's [`crate::app::WindowApp`]
    /// owns the `WeatherTracker`, not `Sim`.
    pub(crate) fn tick_ambience(
        &mut self,
        now: std::time::Instant,
        weather: Option<&lodestone_render::WeatherState>,
    ) {
        let player = self.player();
        let eye = glam::DVec3::new(
            player.position.x,
            player.position.y + f64::from(player.eye_height),
            player.position.z,
        );
        let ambient = self.ambient_sounds();

        // The light probe, and the sky check the rain cadence rides on. Both go
        // through `crate::net::entity_light_at`, which is the only reader in the
        // shell that applies the *dimension's* absent-sky policy — reading
        // `sky_at` directly resolves missing sky data to 0 and would make cave
        // ambience accumulate in open daylight.
        let light_source = self
            .net()
            .map(|n| (n.shared_handle(), n.shared_sky_default()));
        let mut probe = |pos: glam::IVec3| -> lodestone_sound::ambient::LightSample {
            let packed = light_source.as_ref().and_then(|(handle, policy)| {
                crate::net::entity_light_at(handle, pos.x, pos.y, pos.z, policy.get())
            });
            match packed {
                Some(p) => lodestone_sound::ambient::LightSample {
                    sky: i32::from((p >> 4) & 0x0F),
                    block: i32::from(p & 0x0F),
                },
                // No sample is "the column has not streamed in". Report full sky
                // rather than darkness: guessing dark would bank moodiness while
                // the world loads and fire cave ambience on the surface.
                None => lodestone_sound::ambient::LightSample { sky: 15, block: 0 },
            }
        };
        // Rain is only audible where the sky reaches the ear. `landing` is
        // narrowed to the listener's own column rather than vanilla's random
        // `rainParticlePosition`, which means the muffled `weather.rain.above`
        // variant is never selected — reaching it needs a real `MOTION_BLOCKING`
        // heightmap read, which nothing in the shell does yet
        // (`app::weather`'s own doc records the same gap for `canSeeSky`).
        let sky_at_ear = probe(glam::IVec3::new(
            eye.x.floor() as i32,
            eye.y.floor() as i32,
            eye.z.floor() as i32,
        ));
        let landing = (sky_at_ear.sky > 0).then_some([
            eye.x.floor() as i32,
            eye.y.floor() as i32,
            eye.z.floor() as i32,
        ]);

        let taken = self.write(|w| w.resource_mut::<super::AmbienceState>().0.take());
        let Some(mut ambience) = taken else {
            return;
        };
        let events = ambience.advance(
            now,
            &crate::audio::ambient::AmbienceInput {
                eye,
                ambient: &ambient,
                weather,
                landing,
                roof_above: false,
            },
            &mut probe,
        );
        self.write(|w| {
            {
                let mut audio = w.resource_mut::<super::AudioEngine>();
                if let Some(audio) = audio.0.as_mut() {
                    ambience.submit(&events, audio);
                }
            }
            w.resource_mut::<super::AmbienceState>().0 = Some(ambience);
        });
    }

    /// The [`AmbientSounds`](lodestone_sound::ambient::AmbientSounds) in force at
    /// the player: the standing biome's attribute if it declares one, otherwise
    /// the dimension's.
    ///
    /// The biome hop is the same one [`Self::biome_sky_color`](crate::sim::Sim)
    /// makes and for the same reason — **the biome is not on the network**, it
    /// lives in the chunk section's palette, so it has to be resolved at the
    /// player every tick. Falls back to `overworld` when the dimension is
    /// unknown, which is what a pre-login frame sees.
    fn ambient_sounds(&self) -> lodestone_sound::ambient::AmbientSounds {
        let dimension = self
            .net
            .as_ref()
            .and_then(|net| net.shared_handle().get().and_then(|h| h.player().dimension))
            .map_or_else(|| "overworld".to_string(), |d| d.path().to_string());
        let biome = self.standing_biome_name().unwrap_or_default();
        lodestone_sound::biome_ambient::ambient_sounds_at(&dimension, &biome)
    }

    /// The three-slot [`BackgroundMusic`](lodestone_sound::music::BackgroundMusic)
    /// record in force at the player.
    ///
    /// The selector's input is this record, never the biome id — see
    /// [`crate::audio::music::world_situation`]. The biome only *chooses* the
    /// record, which is why the standing-biome lookup happens here and the pick
    /// happens in `BackgroundMusic::select`.
    ///
    /// The fallback is dimension-specific on purpose, which is why this does not
    /// simply call `overworld_music_for`: the Nether's biomes all set the attribute
    /// explicitly, so a Nether biome we have no row for should fall back to
    /// **nothing** rather than to the overworld's default track.
    #[must_use]
    pub(crate) fn background_music(&self) -> lodestone_sound::music::BackgroundMusic {
        let biome = self.standing_biome_name();
        if let Some(record) = biome.and_then(lodestone_sound::biome_music::biome_music) {
            return record.clone();
        }
        let overworld = self
            .net
            .as_ref()
            .and_then(|net| net.shared_handle().get().and_then(|h| h.player().dimension))
            .is_none_or(|d| d.path() == "overworld");
        if overworld {
            lodestone_sound::music::BackgroundMusic::overworld()
        } else {
            lodestone_sound::music::BackgroundMusic::EMPTY
        }
    }

    /// The standing biome's `audio/music_volume`, defaulting to the attribute's
    /// own `1.0`.
    #[must_use]
    pub(crate) fn music_volume(&self) -> f32 {
        self.standing_biome_name()
            .map_or(1.0, lodestone_sound::biome_music::biome_music_volume)
    }

    /// The name of the biome the player is standing in, or `None` when the
    /// registry has not arrived or the column has not streamed in.
    fn standing_biome_name(&self) -> Option<&'static str> {
        let net = self.net.as_ref()?;
        let names = net.shared_biome_names().snapshot();
        if names.is_empty() {
            return None;
        }
        let dims = net.world_dimensions()?;
        let p = self.player().position;
        let (block_x, block_y, block_z) =
            (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32);
        let si = block_y.div_euclid(16) - dims.min_y.div_euclid(16);
        if si < 0 || usize::try_from(si).ok()? >= dims.section_count() {
            return None;
        }
        let chunk = lodestone_client::ChunkPos {
            x: block_x.div_euclid(16),
            z: block_z.div_euclid(16),
        };
        let section = net
            .sections_at(&[(chunk, usize::try_from(si).ok()?)])
            .into_iter()
            .next()
            .flatten()?;
        let biome = section.biome_at_block(
            block_x.rem_euclid(16) as usize,
            block_y.rem_euclid(16) as usize,
            block_z.rem_euclid(16) as usize,
        );
        names.get(usize::try_from(biome).ok()?).copied()
    }

    /// Whether the player counts as **creative** for music selection.
    ///
    /// `Minecraft.java:2615` — `instabuild && mayfly`, read off `Abilities`, and
    /// deliberately **not** a `GameMode::Creative` check. The two come apart in
    /// both directions: a survival player granted both abilities hears creative
    /// music in vanilla, and a creative player whose `mayfly` was revoked does not.
    #[must_use]
    pub(crate) fn music_creative(&self) -> bool {
        self.read(|w| {
            w.get::<lodestone_ecs::session::Abilities>(self.local)
                .is_some_and(|a| a.instabuild && a.may_fly)
        })
    }

    /// Whether the player's eye is **underwater** for music selection — the top of
    /// `BackgroundMusic::select`'s precedence.
    ///
    /// `eye_in_water && in_water()`, matching `FluidState`'s own `submerged`
    /// predicate rather than the raw eye flag: the raw flag alone is true for lava
    /// too, and vanilla's underwater music slot is water-only.
    #[must_use]
    pub(crate) fn music_underwater(&self) -> bool {
        let fluid = self.fluid_state();
        fluid.eye_in_water && fluid.in_water()
    }

    /// The local player's own footstep, **predicted** rather than waited on.
    ///
    /// Called once per physics tick from [`Self::step`](crate::sim::Sim) with the
    /// position before and after this tick's movement, because vanilla accumulates
    /// `moveDist` from the movement *actually achieved* after collision — which is
    /// why walking into a wall makes no sound and a per-frame velocity read would.
    ///
    /// # Why this is predicted at all
    ///
    /// `LocalPlayer.playSound` overrides straight to `playLocalSound`
    /// (`LocalPlayer.java:540-542`), so every step the local player takes is
    /// client-side with no round trip. Swing and attack sounds are **not** — they
    /// go through the method vanilla names `playServerSideSound` — so this is
    /// deliberately steps only; see [`lodestone_sound::predict`].
    ///
    /// The step is also recorded in the echo ledger, so a server that broadcasts
    /// it back to us degrades to "correct" rather than "doubled". Nothing reachable
    /// today double-plays (`lodestone-server` sends no sound packets and a vanilla
    /// server excludes the acting player), which is why that is defence in depth.
    pub(crate) fn tick_footstep(&mut self, before: Vec3d, after: &PlayerState) {
        let moved = glam::DVec3::new(
            after.position.x - before.x,
            after.position.y - before.y,
            after.position.z - before.z,
        );
        let crossed = self.write(|w| {
            w.resource_mut::<super::AmbienceState>()
                .0
                .as_mut()
                .is_some_and(|a| a.advance_step(moved, false, !after.on_ground))
        });
        if !crossed {
            return;
        }
        // The block being stood on, one below the feet — vanilla's
        // `getBlockPosBelowThatAffectsMyMovement`, narrowed to the plain block
        // below (the honeycomb of edge cases it handles is about half-blocks at
        // section borders, none of which changes which sound plays).
        let below = [
            after.position.x.floor() as i32,
            (after.position.y - 0.2).floor() as i32,
            after.position.z.floor() as i32,
        ];
        let state = self.block_at_world(below);
        if is_air_state(state) {
            return;
        }
        let Some(sound) = lodestone_data::sound_types::sound_type(state) else {
            return;
        };
        let Some(name) = lodestone_data::sound_types::step_sound_name(state) else {
            return;
        };
        let position = glam::Vec3::new(
            after.position.x as f32,
            after.position.y as f32,
            after.position.z as f32,
        );
        let volume = crate::audio::ambient::step_volume(sound.volume);
        let pitch = sound.pitch;
        let ticks = self.clock().ticks;
        self.write(|w| {
            if let Some(ambience) = w.resource_mut::<super::AmbienceState>().0.as_mut() {
                ambience.record_step(name, position, ticks);
            }
        });
        self.play_local_sound(
            name,
            lodestone_model::event::SoundCategory::Player,
            position,
            volume,
            pitch,
            block_sound_seed(below, ticks),
        );
    }

    /// Whether an incoming server sound is an echo of one we already predicted
    /// locally, and so must be dropped rather than played twice.
    pub(crate) fn suppresses_echo(&mut self, name: &str, pos: glam::Vec3) -> bool {
        let ticks = self.clock().ticks;
        self.write(|w| {
            w.resource_mut::<super::AmbienceState>()
                .0
                .as_mut()
                .is_some_and(|a| a.should_suppress(name, pos, ticks))
        })
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
        self.play_local_sound(
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
        block_sound_seed(block, self.clock().ticks)
    }
}

/// The pure formula behind [`Sim::block_sound_seed`], parameterised over the
/// tick count instead of reading [`crate::sim::Sim::clock`] directly.
///
/// Split out for `crate::interact::drive_placement`: a plugin-driven
/// placement's sound has the identical "client-chosen, no cross-client
/// agreement to preserve" shape this seed exists for (see the method's own
/// docs), and that system has a [`lodestone_ecs::FrameClock`] resource to read
/// `ticks` from, not a `Sim` to call a method on.
#[must_use]
pub(crate) fn block_sound_seed(block: [i32; 3], ticks: u64) -> i64 {
    let mut x = (block[0] as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (block[1] as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (block[2] as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ ticks;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 31)) as i64
}
