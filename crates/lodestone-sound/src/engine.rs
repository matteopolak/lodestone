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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glam::Vec3;
use lodestone_assets::ResourceSource;
use lodestone_assets::sound::SoundRegistry;
use lodestone_audio::{
    AudioError, AudioSink, CpalSink, Listener, Mixer, PlayHandle, SampleRing,
    SoundCategory as AudioCategory, StreamSource, VorbisStream,
};
use lodestone_model::event::SoundCategory as ModelCategory;

use crate::driver::{DriverError, SoundResolver, StreamingSound};
use crate::music::MusicStart;

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
    /// Stop flags for live streaming-voice producer threads
    /// ([`spawn_stream_producer`]), keyed by the voice's [`PlayHandle`]. The
    /// mixer's own [`Mixer::stop`](lodestone_audio::Mixer::stop) removes the
    /// voice but has no idea a thread is feeding it — this is what lets
    /// [`stop_music`](Self::stop_music) also ask that thread to exit rather
    /// than leaking it.
    stream_producers: Mutex<HashMap<PlayHandle, Arc<AtomicBool>>>,
    /// The one music voice [`start_music`](Self::start_music) may have live
    /// at a time — vanilla's `MusicManager` only ever tracks a single
    /// `currentMusic`, so [`stop_music`](Self::stop_music)/
    /// [`is_music_active`](Self::is_music_active) need no handle argument.
    current_music: Mutex<Option<PlayHandle>>,
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
            stream_producers: Mutex::new(HashMap::new()),
            current_music: Mutex::new(None),
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
    /// captions. See [`SoundResolver::subtitle`].
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

    /// Starts a music/record track playing for real: resolves it exactly like
    /// [`resolve_music`](Self::resolve_music), then spins up a producer
    /// thread that decodes it packet by packet into a fresh
    /// [`SampleRing`](lodestone_audio::SampleRing) and hands that ring to the
    /// mixer as a new streaming voice. This is the "last mile"
    /// [`resolve_music`](Self::resolve_music) always documented as missing.
    ///
    /// # Threading and realtime discipline
    ///
    /// - **The producer thread** (spawned here, native-only — see this
    ///   module's header) owns the decoded-so-far [`VorbisStream`] and does
    ///   all the Vorbis decode work. It never touches the mixer lock; it only
    ///   pushes finished packets into the ring with
    ///   [`SampleRing::write`](lodestone_audio::SampleRing::write), backing
    ///   off with a short native sleep when the ring is full rather than
    ///   busy-spinning.
    /// - **The `cpal` realtime callback** (this engine's existing sink) only
    ///   ever reads from that ring inside
    ///   [`Mixer::render`](lodestone_audio::Mixer::render) — wait-free,
    ///   non-allocating, and unaffected by how far behind the producer is.
    /// - **This call's own (game) thread** does the one-time setup (resolve,
    ///   spawn) plus an O(1)
    ///   [`Mixer::play_stream`](lodestone_audio::Mixer::play_stream) enqueue
    ///   under the same brief lock every other `play_*` method here takes.
    ///
    /// If the producer ever falls behind, the realtime callback sees an empty
    /// ring and the streaming voice contributes silence for the rest of that
    /// block rather than blocking or panicking — see
    /// [`lodestone_audio::StreamVoice`]'s doc for the exact contract.
    ///
    /// Only one music voice may be live at a time (matching vanilla's single
    /// `currentMusic`): starting a new one first stops whatever this engine
    /// was previously tracking, so a caller that forgets to call
    /// [`stop_music`](Self::stop_music) between tracks cannot leak a producer
    /// thread.
    ///
    /// Returns [`MusicStart::Silent`] — not an error — for vanilla's ordinary
    /// "nothing resolved" case (unknown event, or the `.ogg` bytes are absent
    /// from a checkout that has not run `fetch-sounds --all`), matching
    /// [`resolve_music`](Self::resolve_music)'s own contract.
    pub fn start_music(&mut self, event_name: &str, seed: i64) -> Result<MusicStart, DriverError> {
        // Defensive cleanup, not the ordinary path: `MusicManager` only ever
        // calls `start` once the previous track is already known inactive,
        // but a caller that skips `stop_music` between tracks must not leak
        // the old producer thread.
        self.stop_music();

        let Some(streaming) = self.resolver.resolve_streaming(event_name, seed)? else {
            return Ok(MusicStart::Silent);
        };

        let source_rate = streaming.stream.sample_rate();
        let channels = streaming.stream.channels();
        let ring = Arc::new(SampleRing::with_min_capacity(stream_ring_capacity(
            source_rate,
            channels,
        )));
        let ended = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        spawn_stream_producer(
            streaming.stream,
            Arc::clone(&ring),
            Arc::clone(&ended),
            Arc::clone(&stop),
        );

        let source = StreamSource {
            ring,
            source_channels: channels,
            source_rate,
            category: crate::driver::map_category(ModelCategory::Music),
            volume: streaming.volume,
            pitch: streaming.pitch,
            ended,
        };
        let handle = self.lock_mixer().play_stream(source);
        *self.lock_current_music() = Some(handle);
        self.lock_stream_producers().insert(handle, stop);

        Ok(MusicStart::Started)
    }

    /// Stops the current music voice, if any, removing it from the mixer
    /// **and** signalling its producer thread to exit — the half
    /// [`Mixer::stop`](lodestone_audio::Mixer::stop) alone cannot do, since
    /// the mixer knows nothing about the thread feeding it. A no-op when no
    /// music is playing.
    pub fn stop_music(&self) {
        let Some(handle) = self.lock_current_music().take() else {
            return;
        };
        self.lock_mixer().stop(handle);
        if let Some(stop) = self.lock_stream_producers().remove(&handle) {
            stop.store(true, Ordering::Release);
        }
    }

    /// Whether the track [`start_music`](Self::start_music) most recently
    /// started is still sounding — the `MusicSink::is_active` a caller needs
    /// to drive [`lodestone_sound::music::MusicManager::tick`].
    ///
    /// Also the cleanup point for a track that finished **on its own** (the
    /// mixer reaped the voice once the stream ended, without anyone calling
    /// [`stop_music`](Self::stop_music)): a `false` answer here also drops
    /// the now-stale producer-registry entry, so a long session that plays
    /// many tracks does not accumulate one entry per track forever. No
    /// stop-flag store is needed in that path — the producer thread already
    /// set `ended` and returned on its own once the stream ran out, which is
    /// exactly why the mixer reaped the voice in the first place.
    pub fn is_music_active(&self) -> bool {
        let Some(handle) = *self.lock_current_music() else {
            return false;
        };
        let active = self.with_mixer(|m| m.is_active(handle));
        if !active {
            *self.lock_current_music() = None;
            self.lock_stream_producers().remove(&handle);
        }
        active
    }

    /// Sets the `Music` bus's runtime gain — the primitive
    /// vanilla's own fade-playing routine's crossfade drives
    /// (its own category-volume update for the music bus), applied here via
    /// [`CategoryVolumes::set_runtime_gain`](lodestone_audio::CategoryVolumes::set_runtime_gain)
    /// rather than a per-voice field — see [`StreamSource`]'s own doc for why
    /// the fade is a bus property, not an instance one.
    pub fn set_music_gain(&self, gain: f32) {
        self.with_mixer(|m| {
            m.volumes_mut().set_runtime_gain(AudioCategory::Music, gain);
        });
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

    /// Plays a **head-relative, one-shot** sound — vanilla's own
    /// UI-sound-instance shape (no attenuation, head-relative): no
    /// distance falloff and no stereo panning, so it sounds identical no matter
    /// where the listener stands or faces. This is [`play_loop`](Self::play_loop)
    /// minus the forced `looping = true` — the two share the same
    /// `Vec3::ZERO`-position-is-ignored contract because `relative` is what makes
    /// the position irrelevant, not the zero itself.
    ///
    /// The UI button-click sound is the motivating caller
    /// (`crate::app::WindowApp` in `lodestone-shell`), but this is the general
    /// "vanilla marked this `RELATIVE`" primitive — not UI-specific — should
    /// another head-relative one-shot event need it later.
    ///
    /// `Ok(None)` means the event resolved to nothing playable (the ordinary
    /// answer when the `.ogg` corpus is not on disk), not an error.
    pub fn play_relative_sound(
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
        instance.relative = true;
        Ok(Some(self.lock_mixer().play(instance)))
    }

    /// Starts a **looping, head-relative** voice — the ambient biome/dimension
    /// loop (vanilla's own loop-sound-instance type).
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
    /// vanilla's own sound-source-volumes map, the write side of
    /// its own final-source-volume formula.
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

    fn lock_current_music(&self) -> std::sync::MutexGuard<'_, Option<PlayHandle>> {
        self.current_music.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_stream_producers(&self) -> std::sync::MutexGuard<'_, HashMap<PlayHandle, Arc<AtomicBool>>> {
        self.stream_producers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}

/// Ring capacity for one streaming voice, in samples (interleaved across
/// `channels`): about half a second of the source's own audio. Generous
/// enough to absorb real producer-thread scheduling jitter without the
/// realtime callback ever starving, small enough that even the largest
/// vanilla music asset (stereo, decoded natively at its own rate) costs a
/// few hundred KiB rather than the hundreds of megabytes eager decode would
/// (see `crate::driver::SoundResolver::resolve_streaming`'s own doc for that
/// figure). Deliberately phrased as a sample count derived from the stream's
/// own reported rate/channels rather than any wall-clock read — this crate's
/// own `Instant::now()` ban (enforced by `cargo xtask wasm-check`) means nothing
/// here may consult a clock, and there is no need to: the ring is sized once,
/// at construction, from data the stream already carries.
fn stream_ring_capacity(source_rate: u32, channels: u16) -> usize {
    let half_second = (source_rate as usize / 2) * usize::from(channels.max(1));
    half_second.max(2)
}

/// Spawns the native producer thread for one streaming voice: decodes
/// `stream` packet by packet and pushes every sample into `ring`, backing off
/// with a short sleep when the ring is full rather than busy-spinning, until
/// either the stream ends (sets `ended` and returns), a decode error occurs
/// (same), or `stop` is set (the caller asked this voice to stop — see
/// [`AudioEngine::stop_music`]).
///
/// # Why this is safe here and would not be on wasm32
///
/// `std::thread::spawn` and `std::thread::sleep` both **trap** on wasm32 —
/// this is exactly the hazard CLAUDE.md's rendering-constraints section
/// documents. But this function is reached only from [`AudioEngine`], and
/// that whole type lives in a module that is `#[cfg(not(target_arch =
/// "wasm32"))]` (see this module's header) — so this call site is
/// structurally absent from any wasm32 build, not merely untested there. The
/// browser's own producer (in `lodestone-shell`'s wasm arm of `ShellAudio`)
/// uses a different mechanism entirely: it pumps decode from the same main
/// thread the `ScriptProcessorNode` callback already runs on, once per
/// callback, because wasm32 without the `atomics` target feature has no
/// second thread to spawn one onto.
fn spawn_stream_producer(
    mut stream: VorbisStream,
    ring: Arc<SampleRing>,
    ended: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            match stream.next_packet() {
                Ok(Some(packet)) => {
                    let mut offset = 0;
                    while offset < packet.len() {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        let written = ring.write(&packet[offset..]);
                        offset += written;
                        if written == 0 {
                            // The realtime consumer has not caught up; back off
                            // briefly. This is the producer thread, never the
                            // render callback, so sleeping here cannot stall
                            // playback of anything already buffered.
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                }
                Ok(None) => {
                    ended.store(true, Ordering::Release);
                    return;
                }
                Err(_) => {
                    // A mid-stream decode error: stop producing and mark ended
                    // so the voice finishes cleanly (once the ring drains)
                    // rather than the mixer waiting forever for samples that
                    // will never arrive.
                    ended.store(true, Ordering::Release);
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_audio::{Mixer, decode_vorbis};

    /// The same real (synthetic, non-silent) 0.5 s stereo chirp used
    /// throughout this crate's decode validation. A real ogg, not silence —
    /// two silent buffers agree perfectly, so a silent fixture would make
    /// every "did real audio arrive" assertion below vacuous.
    const CHIRP_OGG: &[u8] = include_bytes!("../tests/fixtures/chirp_stereo_44100.ogg");

    fn poll_until(max_polls: u32, mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..max_polls {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        done()
    }

    #[test]
    fn stream_ring_capacity_is_half_a_second_of_stereo_44100() {
        // Predicted value, not a sign check: 44100 Hz / 2 * 2 channels.
        assert_eq!(stream_ring_capacity(44_100, 2), 44_100);
        // Mono halves it.
        assert_eq!(stream_ring_capacity(44_100, 1), 22_050);
        // A degenerate zero-channel report (should never happen — a real
        // header always declares >=1) still yields a usable, non-zero ring
        // rather than dividing by zero or returning 0.
        assert_eq!(stream_ring_capacity(44_100, 0), 22_050);
    }

    /// The producer thread this crate actually spawns in production
    /// (`AudioEngine::start_music`), exercised directly against a real
    /// `SampleRing` and a real `Mixer::render` — no `cpal`, no output device,
    /// so this runs in the ordinary (non-`#[ignore]`d) suite. This is the
    /// strongest evidence short of the live-device gate: it proves the exact
    /// function production calls, decoding on a genuine spawned OS thread,
    /// produces amplitudes the eager decode path agrees with.
    #[test]
    fn the_real_producer_thread_feeds_the_ring_and_the_mixer_measures_a_predicted_peak() {
        let stream = VorbisStream::new(CHIRP_OGG.to_vec()).expect("chirp header parses");
        let source_rate = stream.sample_rate();
        let channels = stream.channels();
        let ring = Arc::new(SampleRing::with_min_capacity(stream_ring_capacity(
            source_rate,
            channels,
        )));
        let ended = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        spawn_stream_producer(stream, Arc::clone(&ring), Arc::clone(&ended), Arc::clone(&stop));

        // The producer is a real, independently-scheduled OS thread; give it
        // a bounded window to publish the first samples rather than asserting
        // instantaneously (which would be a race, not a test).
        assert!(
            poll_until(100, || ring.len() > 0),
            "the producer thread never wrote a single sample within 500ms"
        );

        let mut mixer = Mixer::new(source_rate);
        let handle = mixer.play_stream(StreamSource {
            ring: Arc::clone(&ring),
            source_channels: channels,
            source_rate,
            category: AudioCategory::Music,
            volume: 1.0,
            pitch: 1.0,
            ended: Arc::clone(&ended),
        });
        assert!(mixer.is_active(handle));

        // Predict the peak from the SAME fixture decoded eagerly (the path
        // every other test in this crate already trusts), not a round
        // number: the two decode paths (`decode_vorbis` vs `VorbisStream` +
        // streaming voice) must agree on what the loudest sample is, because
        // `stream.rs`'s own tests already prove they are bit-identical.
        let eager = decode_vorbis(CHIRP_OGG).expect("eager decode of the same bytes");
        let expected_peak = eager
            .samples()
            .iter()
            .fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(
            expected_peak > 0.1,
            "fixture sanity: the chirp itself must be non-trivially loud, got {expected_peak}"
        );

        // Render in small blocks, draining the producer's output, until the
        // whole (short) track has been decoded, streamed and mixed — proving
        // the full path: thread -> ring -> StreamVoice -> Mixer::render.
        let mut peak = 0.0_f32;
        let mut out = [0.0f32; 1024];
        let drained = poll_until(400, || {
            mixer.render(&mut out);
            peak = peak.max(out.iter().fold(0.0_f32, |m, &s| m.max(s.abs())));
            !mixer.is_active(handle)
        });
        assert!(
            drained,
            "the streaming voice never finished within the poll budget — either the \
             producer thread stalled or Mixer::render never reaped it"
        );
        assert!(ring.is_empty(), "ring must be drained once the voice finishes");
        assert!(ended.load(Ordering::Acquire), "producer must have signalled ended");
        // Bracket the measured peak against the eagerly-decoded one: real
        // playback through the ring must land close to the source's own
        // loudest sample (equal-power centre pan is 1/sqrt(2) for a mono
        // source, but this fixture is stereo and plays flat/ungained at
        // bus_gain=1, so the two should agree closely).
        assert!(
            (peak - expected_peak).abs() < 0.05,
            "measured peak {peak} not close to the eagerly-decoded peak {expected_peak} — \
             streaming and eager decode disagree on loudness"
        );
        assert!(peak > 0.3, "measured peak {peak} too quiet to be the real chirp");
    }

    /// Control proving the amplitude assertion above has teeth: silence
    /// (an all-zero ring) must NOT satisfy the same peak floor.
    #[test]
    fn control_a_silent_stream_never_reaches_the_peak_floor() {
        let ring = Arc::new(SampleRing::with_min_capacity(4096));
        assert_eq!(ring.write(&vec![0.0f32; 2048]), 2048);
        let mut mixer = Mixer::new(48_000);
        mixer.play_stream(StreamSource {
            ring,
            source_channels: 2,
            source_rate: 48_000,
            category: AudioCategory::Music,
            volume: 1.0,
            pitch: 1.0,
            ended: Arc::new(AtomicBool::new(true)),
        });
        let mut out = [0.0f32; 1024];
        mixer.render(&mut out);
        let peak = out.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert_eq!(peak, 0.0, "an all-zero source must mix to exact silence");
        assert!(
            peak <= 0.3,
            "the silence control must not itself satisfy the >0.3 floor the real test uses"
        );
    }
}
