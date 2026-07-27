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

use crate::driver::{DriverError, SoundResolver};

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
    pub fn new(registry: SoundRegistry, source: Box<dyn ResourceSource>) -> Result<Self, AudioError> {
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
        let instance =
            match self
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

    /// Runs `f` with exclusive access to the shared mixer — an escape hatch for
    /// less-common operations (per-category volumes via
    /// [`volumes_mut`](lodestone_audio::Mixer::volumes_mut), inspection). Holds
    /// the realtime lock for the duration of `f`, so keep `f` short and never
    /// decode or block inside it.
    pub fn with_mixer<R>(&self, f: impl FnOnce(&mut Mixer) -> R) -> R {
        f(&mut self.lock_mixer())
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
