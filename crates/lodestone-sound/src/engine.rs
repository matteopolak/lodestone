//! The native, device-backed audio engine: [`SoundResolver`] + a running
//! [`CpalSink`](lodestone_audio::CpalSink) sharing one [`Mixer`].
//!
//! This is the piece that finally makes the game *audible*. Everything below
//! [`AudioEngine`] is unchanged, headless, sample-driven core; this module is
//! the thin native seam — the exact `cfg(not(target_arch = "wasm32"))` boundary
//! the sink already draws — that connects that core to a real output device.
//!
//! # Why it is native-only, structurally
//!
//! The whole module is `#[cfg(not(target_arch = "wasm32"))]` and it depends on
//! [`CpalSink`](lodestone_audio::CpalSink), which is itself a `cpal` type behind
//! a `[target.'cfg(not(...))'.dependencies]` entry. `cpal` therefore cannot
//! enter the wasm build even under Cargo feature unification — the boundary is
//! `cfg`, not a feature. In the browser the sink *is* the `AudioWorklet`; it
//! calls [`Mixer::render`](lodestone_audio::Mixer::render) directly, so there is
//! no `AudioEngine` there and none is needed.
//!
//! # Realtime discipline
//!
//! The `cpal` callback runs on a high-priority audio thread and locks the shared
//! mixer once per block to [`render`](lodestone_audio::Mixer::render). The game
//! thread must never hold that lock while doing slow work, or the callback
//! starves and the device xruns (audible glitch). So [`AudioEngine::play_sound`]
//! resolves and **decodes outside the lock**, then takes the lock only for the
//! O(1) [`Mixer::play`](lodestone_audio::Mixer::play). [`set_listener`] and
//! [`set_voice_position`] likewise hold the lock for a single O(1) write.
//!
//! [`set_listener`]: AudioEngine::set_listener
//! [`set_voice_position`]: AudioEngine::set_voice_position

use std::sync::{Arc, Mutex};

use glam::Vec3;
use lodestone_assets::ResourceSource;
use lodestone_assets::sound::SoundRegistry;
use lodestone_audio::{AudioError, AudioSink, CpalSink, Listener, Mixer, PlayHandle};
use lodestone_model::event::SoundCategory as ModelCategory;

use crate::driver::{DriverError, SoundResolver, StreamingSound};

/// A running native audio engine.
///
/// Construct it once with a parsed [`SoundRegistry`] and a byte source, then
/// each frame call [`set_listener`](Self::set_listener) with the camera's
/// position/orientation and [`play_sound`](Self::play_sound) /
/// [`play_entity_sound`](Self::play_entity_sound) as sound events arrive. The
/// device pulls samples on its own thread; the caller never renders.
#[derive(Debug)]
pub struct AudioEngine {
    // Owns the running stream. Kept alive for the engine's lifetime — dropping it
    // stops the device. Never accessed after construction except to pause/resume.
    sink: CpalSink,
    mixer: Arc<Mutex<Mixer>>,
    resolver: SoundResolver,
}

impl AudioEngine {
    /// Opens the default output device and starts playback, returning a ready
    /// engine.
    ///
    /// Fails with [`AudioError::Device`] if no output device is available or the
    /// stream cannot start — a real, loud failure, never a silent no-op. Callers
    /// that want the game to run without audio should treat the error as
    /// "audio unavailable" explicitly rather than swallowing it.
    pub fn new(
        registry: SoundRegistry,
        source: Box<dyn ResourceSource>,
    ) -> Result<Self, AudioError> {
        let (sink, mixer) = CpalSink::new()?;
        // The stream is created paused on some platforms; start it now so the
        // callback begins pulling from the shared mixer.
        sink.resume()?;
        Ok(Self {
            sink,
            mixer,
            resolver: SoundResolver::new(registry, source),
        })
    }

    /// The device output sample rate in Hz (the rate the shared mixer renders
    /// at).
    pub fn sample_rate(&self) -> u32 {
        self.sink.sample_rate()
    }

    /// Number of distinct `.ogg` files decoded and cached so far. Lets a live
    /// gate prove real decode work happened rather than the device merely
    /// running.
    pub fn decoded_file_count(&self) -> usize {
        self.resolver.decoded_file_count()
    }

    /// The event's `subtitles` translation key, for the HUD's sound-subtitle
    /// captions (issue #198). See [`SoundResolver::subtitle`].
    pub fn subtitle(&self, event_name: &str) -> Option<&str> {
        self.resolver.subtitle(event_name)
    }

    /// Updates the listener transform from the camera. Call every frame with the
    /// camera's world position and forward/up basis; spatialisation of every
    /// live voice follows on the next rendered block.
    pub fn set_listener(&self, position: Vec3, forward: Vec3, up: Vec3) {
        let listener = Listener {
            position,
            forward,
            up,
        };
        self.lock_mixer().set_listener(listener);
    }

    /// Resolve a **music** track to a lazily-decoded stream, without touching the
    /// mixer.
    ///
    /// This is the music counterpart to [`Self::play_sound`], and it deliberately
    /// stops short of playing, because the last mile does not exist yet: `Mixer`
    /// has no streaming-voice API, and its `SoundInstance` takes a fully decoded
    /// `Arc<PcmBuffer>`. So a caller gets a [`StreamingSound`] it can prove it
    /// resolved, and nothing is audible until a streaming voice lands. See
    /// `docs/music-selection.md`.
    ///
    /// # Why this must not be [`SoundResolver::resolve_instance`]
    ///
    /// **All 316 music leaf entries declare `"stream": true`**, and
    /// `resolve_instance` caches decoded PCM — `the_end.ogg` alone is **304 MiB**
    /// decoded. Routing music through the caching path is a several-hundred-
    /// megabyte allocation, not a glitch, which is why this exposes only the
    /// streaming path and no accessor to the resolver itself.
    ///
    /// `Ok(None)` is the **ordinary** answer in a normal checkout, not an error:
    /// `cargo xtask fetch-sounds` excludes music by default, so 0 of 70 music
    /// objects are on disk and `resolve_streaming` reports absence rather than
    /// failing. Silence is the correct default here.
    pub fn resolve_music(
        &mut self,
        event_name: &str,
        seed: i64,
    ) -> Result<Option<StreamingSound>, DriverError> {
        self.resolver.resolve_streaming(event_name, seed)
    }

    /// Plays a positioned sound (the `SOUND` packet path).
    ///
    /// Resolution + decode run **before** the mixer lock is taken; only the O(1)
    /// enqueue is under the lock, so this never stalls the realtime callback.
    /// Returns `Ok(None)` for vanilla's silent "empty sound" (unknown event /
    /// zero weight), which is not an error.
    pub fn play_sound(
        &mut self,
        event_name: &str,
        category: ModelCategory,
        position: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> Result<Option<PlayHandle>, DriverError> {
        // Decode OUTSIDE the lock — this is the slow step (file read + Vorbis
        // decode on a cache miss) and must never be held against the audio
        // callback.
        let instance = match self
            .resolver
            .resolve_instance(event_name, category, position, volume, pitch, seed)?
        {
            Some(instance) => instance,
            None => return Ok(None),
        };
        // Now a brief O(1) lock just to enqueue the voice.
        Ok(Some(self.lock_mixer().play(instance)))
    }

    /// Plays an entity-attached sound (the `SOUND_ENTITY` packet path) at the
    /// entity's current position. Identical to [`play_sound`](Self::play_sound);
    /// the caller keeps the returned handle and pushes new positions with
    /// [`set_voice_position`](Self::set_voice_position) as the entity moves.
    pub fn play_entity_sound(
        &mut self,
        event_name: &str,
        category: ModelCategory,
        position: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> Result<Option<PlayHandle>, DriverError> {
        self.play_sound(event_name, category, position, volume, pitch, seed)
    }

    /// Moves a live voice (an entity-attached sound) to a new position. Returns
    /// `false` once the voice has finished — the signal to stop tracking that
    /// entity.
    pub fn set_voice_position(&self, handle: PlayHandle, position: Vec3) -> bool {
        self.lock_mixer().set_voice_position(handle, position)
    }

    /// Starts a **looping, head-relative** voice — the ambient biome/dimension
    /// loop (`BiomeAmbientSoundsHandler.LoopSoundInstance`).
    ///
    /// Head-relative and looping are both forced here rather than left to the
    /// caller, because they are what distinguishes a loop from every other play
    /// path: vanilla's loop instances are `RELATIVE` with no attenuation, and
    /// the crossfade below assumes the voice never ends on its own. `volume` is
    /// normally `0.0` at start; ramp it with
    /// [`set_voice_volume`](Self::set_voice_volume).
    ///
    /// `Ok(None)` means the event resolved to nothing playable (the ordinary
    /// answer when the `.ogg` corpus is not on disk), not an error.
    pub fn play_loop(
        &mut self,
        event_name: &str,
        category: ModelCategory,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> Result<Option<PlayHandle>, DriverError> {
        let mut instance = match self.resolver.resolve_instance(
            event_name,
            category,
            Vec3::ZERO,
            volume,
            pitch,
            seed,
        )? {
            Some(instance) => instance,
            None => return Ok(None),
        };
        instance.looping = true;
        instance.relative = true;
        Ok(Some(self.lock_mixer().play(instance)))
    }

    /// Re-sets a live voice's volume — the ambient loop's 40-tick crossfade.
    /// `false` once the voice is gone.
    pub fn set_voice_volume(&self, handle: PlayHandle, volume: f32) -> bool {
        self.lock_mixer().set_voice_volume(handle, volume)
    }

    /// Stops a live voice. `false` when it had already finished.
    pub fn stop_voice(&self, handle: PlayHandle) -> bool {
        self.lock_mixer().stop(handle)
    }

    /// Runs `f` with exclusive access to the shared mixer — an escape hatch for
    /// less-common operations (per-category volumes via
    /// [`volumes_mut`](lodestone_audio::Mixer::volumes_mut), inspection). Holds
    /// the realtime lock for the duration of `f`, so keep `f` short and never
    /// decode or block inside it.
    pub fn with_mixer<R>(&self, f: impl FnOnce(&mut Mixer) -> R) -> R {
        f(&mut self.lock_mixer())
    }

    /// Pushes the eleven `soundSource.*` slider values onto their mixer buses —
    /// vanilla's `Options.soundSourceVolumes`, the write side of
    /// `getFinalSoundSourceVolume`.
    ///
    /// # Why the whole set, under one lock
    ///
    /// Every pair is applied inside a single [`Self::with_mixer`], because that
    /// lock is the realtime one the audio callback also takes: a caller pushing
    /// eleven sliders one at a time would take and drop it eleven times per
    /// frame for a total of eleven `f32` stores. The set is also the natural
    /// unit — vanilla's final gain for a non-master bus is
    /// `sourceVolume * masterVolume`, so master and the bus it scales must never
    /// be observable half-applied by a render block between two pushes.
    ///
    /// Takes [`lodestone_model::event::SoundCategory`] rather than the mixer's
    /// own bus enum so callers need no dependency on `lodestone-audio`; the
    /// ordinal bridge is the same `map_category` every play path uses.
    pub fn set_category_volumes(&self, volumes: &[(ModelCategory, f32)]) {
        self.with_mixer(|mixer| {
            let buses = mixer.volumes_mut();
            for (category, volume) in volumes {
                buses.set_user(crate::driver::map_category(*category), *volume);
            }
        });
    }

    /// The slider value currently on `category`'s bus — the read-back side of
    /// [`Self::set_category_volumes`], so a caller (or a gate) can observe what
    /// actually landed rather than what it sent.
    #[must_use]
    pub fn category_volume(&self, category: ModelCategory) -> f32 {
        self.with_mixer(|mixer| mixer.volumes().user(crate::driver::map_category(category)))
    }

    /// Pauses the output device without tearing it down.
    pub fn pause(&self) -> Result<(), AudioError> {
        self.sink.pause()
    }

    /// Resumes the output device.
    pub fn resume(&self) -> Result<(), AudioError> {
        self.sink.resume()
    }

    fn lock_mixer(&self) -> std::sync::MutexGuard<'_, Mixer> {
        // A panicked audio callback poisons the mutex, but the mixer state is
        // still coherent (render is the only thing that runs there), so recover
        // rather than propagate a panic into the game thread.
        self.mixer.lock().unwrap_or_else(|p| p.into_inner())
    }
}
