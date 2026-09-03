//! `crate::audio` — the shell's audio composition root.
//!
//! This is the last hop that makes Lodestone *audible*: it turns the sound
//! events the client already delivers ([`ClientEvent::Sound`] /
//! [`ClientEvent::EntitySound`], surfaced here as [`NetUpdate`] variants) into
//! real Ogg Vorbis playing on the machine's default output device, with the
//! listener driven by the camera each frame.
//!
//! # Where the pieces live (and why the orchestration is *here*)
//!
//! * [`lodestone_audio`] is the device-free core (decode, mix, spatialise) — no
//!   sound names, no packs, no device.
//! * [`lodestone_sound`] is the version-free bridge: event → `sounds.json`
//!   resolution → weighted selection → decode → mixer. Its native
//!   [`AudioEngine`](lodestone_sound::AudioEngine) adds a real `cpal` device.
//! * Neither knows *where the bytes come from*. That is a composition decision,
//!   so it lives in the shell — the only layer that gets to say "read oggs from
//!   the Minecraft asset store on this disk."
//!
//! # Asset store, and how the root is found
//!
//! `sounds.json` and every `.ogg` do **not** live in `client.jar`; they live in
//! the launcher's asset-object store, addressed by an `asset-index-*.json` that
//! maps an in-pack key (`minecraft/sounds/…​.ogg`) to
//! `objects/<sha1[0..2]>/<sha1>`. That resolution is
//! [`crate::asset_objects`] — extracted out of this module once the title-screen
//! panorama turned out to need the same store, because `client.jar` ships stubs
//! for the files it overrides. Read that module before assuming the jar is the
//! whole pack. The shell is deliberately version-free (it knows only a protocol
//! *number*), so it must not hardcode a version directory.
//!
//! Finding the root is [`crate::asset_objects::discover_store_root`], shared with every
//! other consumer. This module used to demand its **own** environment variable,
//! `LODESTONE_ASSET_ROOT`, and return `None` without it — while the rest of the
//! shell resolved the very same directory from `LODESTONE_ASSETS` or an ancestor
//! walk. So a plain `cargo run --release` rendered vanilla textures and a real
//! panorama with audio *switched off*, and setting the documented
//! `LODESTONE_ASSETS` did not help because nothing here read it. That variable is
//! still honoured, first, as the explicit override; the fallbacks are what changed.
//! See `discover_store_root` for the ordering and why an explicitly-set variable
//! is never silently skipped in favour of the scan.
//!
//! # The silence this module has to make visible
//!
//! `sounds.json` being present is not the same as the samples being present, and
//! the difference is invisible: the engine comes up, resolves an event, finds no
//! object, and plays nothing. A fresh `fetch-assets` checkout has 11 of 4871
//! `.ogg` objects, so *every* sound is silent while every log line says "audio
//! enabled". Two things guard that here: a startup census
//! ([`AssetObjectStore::present_count`](crate::asset_objects::AssetObjectStore::present_count))
//! that prints the ratio and warns when it is zero, and a one-shot `warn` the
//! first time a sound cannot be played. Both name
//! `cargo run -p xtask -- fetch-sounds`. Per-event failures stay at `debug` after
//! the first, because one unresolvable sound must not flood the log.
//!
//! [`ClientEvent::Sound`]: lodestone_client::ClientEvent::Sound
//! [`ClientEvent::EntitySound`]: lodestone_client::ClientEvent::EntitySound
//! [`NetUpdate`]: crate::net::NetUpdate

//! ## Browser (`wasm32`) arm — a real `ShellAudio`, not a stub
//!
//! This module used to be `#![cfg(not(target_arch = "wasm32"))]` in its entirety,
//! which made `crate::audio` vanish on wasm and took thirteen call sites in
//! `hud.rs`, `sim.rs`, `sim/audio.rs`, `sim/build.rs`, `app/menus.rs` and
//! `app/redraw.rs` down with it. Then [`ShellAudio`] became a `cfg` fork whose
//! browser arm was an **uninhabited enum** — a deliberate compile-time "this
//! cannot even appear to work" while nobody had built the device sink yet.
//!
//! That device sink now exists. [`lodestone_sound::AudioEngine`] (native) wraps
//! `cpal`, which is `cfg`-gated out of the wasm build at its own crate — so the
//! two targets could never share that type — but everything `AudioEngine`
//! wraps *around* `cpal` is device-free and already wasm-clean:
//! [`lodestone_sound::SoundResolver`] (event → weighted pick → decode) and
//! [`lodestone_audio::Mixer`] (spatialise → sum → interleaved stereo `f32`)
//! carry no `cfg` gate at all. The browser arm below is exactly what this
//! module's previous revision predicted: "an `AudioWorklet` feeding
//! `lodestone-audio`'s mixer directly" — except the sink is a `ScriptProcessorNode`
//! rather than an `AudioWorklet`. That substitution is deliberate, not a
//! shortcut: an `AudioWorklet` processor runs its own wasm module instance on a
//! **separate** audio-rendering thread, which needs a JS loader shim living
//! beside the page (`web/`, off limits to this file) plus a
//! `SharedArrayBuffer`/`Atomics` handoff across it — a second build artifact and
//! a cross-origin-isolation header this crate cannot add on its own.
//! `ScriptProcessorNode` runs its callback on the **main** thread instead, so it
//! needs none of that: `web_sys::AudioContext` and a `Closure` are enough, both
//! already ordinary dependencies of this crate's wasm32 target. The cost is
//! real (main-thread audio callbacks can glitch under load, and the API is
//! formally deprecated though universally implemented) and is exactly the "one
//! turn, real but partial" trade this module's doc explicitly allows; migrating
//! the callback to a genuine `AudioWorklet` later changes nothing about
//! [`SoundResolver`](lodestone_sound::SoundResolver) or [`Mixer`] — only the
//! sink.
//!
//! ### What still has to come from outside this file
//!
//! The mixer and the resolver both need bytes: `sounds.json` and the `.ogg`
//! corpus it indexes. A browser has no filesystem, so those bytes have to be
//! fetched and staged by `web/` and handed in through
//! [`crate::platform::assets::Bundle`]'s `sounds_json`/`sound_objects` fields —
//! the same seam [`crate::menu::panorama`] already uses for the title-screen
//! faces, extended here rather than duplicated. **Empty is the honest default**,
//! exactly as an empty `panorama` bundle falls back to `client.jar`'s grey
//! stub: `ShellAudio::from_env` degrades to `None` with a logged reason, never a
//! stub that silently swallows sound. See that struct's field docs for the
//! staging shape a `web/`-side change needs to provide, and this module's own
//! `from_env` for the exact log lines a missing bundle, an empty `sounds.json`,
//! or a zero-object corpus each produce.
//!
//! ### The autoplay gate
//!
//! A browser refuses to start an `AudioContext` outside a real user gesture — an
//! eagerly-created one begins `suspended` and stays that way with **no error**,
//! the same failure shape this repo already measured for a gesture-less
//! `request_pointer_lock()`. So [`ShellAudio::from_env`] never calls `resume()`;
//! [`ShellAudio::resume_on_gesture`] is a separate, explicit call a real input
//! handler makes. Wiring `resume_on_gesture` to the shell's actual
//! click/keydown handling is outside this file's owned paths (`app/**`); see
//! the brokered hunk recorded for that hand-off.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc};

use glam::Vec3;
// Both targets parse `sounds.json` the same way now: native reads it off the
// asset-object store, wasm32 reads it out of `platform::assets::Bundle`.
use lodestone_assets::sound::SoundRegistry;
use lodestone_model::event::SoundCategory;
use lodestone_render::Camera;
#[cfg(not(target_arch = "wasm32"))]
use lodestone_sound::AudioEngine;
use lodestone_sound::music::{Music, MusicStart};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, closure::Closure};

pub(crate) mod ambient;
pub(crate) mod music;
pub(crate) mod subtitles;

/// Wall-clock milliseconds for the caption clock — vanilla's own epoch-millis clock,
/// the same origin `gpu/glint.rs` and `app::recipe_toast_now_ms` use, so a caption
/// ages against the same clock every other timed overlay does.
///
/// Shared between both targets: `crate::platform::epoch_duration` is
/// `lodestone_time::epoch_duration`, already portable (it is what closed the
/// last of the five wasm32 clock traps), so this needs no `cfg` fork of its
/// own — only the code that used to call `std::time::SystemTime::now()`
/// directly did.
fn caption_now_ms() -> u64 {
    crate::platform::epoch_duration().as_millis() as u64
}

/// Environment variable naming the Minecraft asset root directly. Re-exported
/// from [`crate::asset_objects`], which owns the whole resolution order — it is
/// no longer the *only* way to point audio at a store, just the highest-priority
/// one.
pub use crate::asset_objects::ASSET_ROOT_ENV;

/// The shell's live audio, wrapping a device-backed [`AudioEngine`].
///
/// Constructed once via [`ShellAudio::from_env`]; `None` means audio is disabled
/// (no asset store found, load failure, or no output device) and every call site
/// is a simple `if let Some(audio)`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct ShellAudio {
    engine: AudioEngine,
    /// The sound-subtitle captions. Fed here rather than at each
    /// caller because this struct's two `play_*` methods are the single choke
    /// point every sound in the client passes through — captions cannot drift out
    /// of sync with what is audible if they are recorded where playing happens.
    subtitles: subtitles::SubtitleQueue,
    /// Whether the "a sound could not be played" warning has already fired. The
    /// first failure is the one worth a `warn` — it names the missing corpus and
    /// the command that fixes it — and every one after it is the same story, so
    /// they drop to `debug` rather than flooding a log at 20 events a second.
    reported_failure: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl ShellAudio {
    /// Brings audio up from the discovered asset store, or returns `None` with a
    /// logged reason.
    ///
    /// Failure is never fatal to the game: no store, an unreadable index, or an
    /// unavailable output device all log and yield `None`. The one thing this must
    /// not do is come up *silently working-but-mute*, which is why the load path
    /// reads and parses `sounds.json` eagerly **and** censuses the `.ogg` objects
    /// — a store with the registry and none of the samples is the state that looks
    /// exactly like working audio, and it now says so at startup.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let root = match crate::asset_objects::discover_store_root() {
            Ok(root) => root,
            Err(reason) => {
                tracing::info!(target: "audio", "audio disabled: {reason}");
                return None;
            }
        };
        match Self::load_from_root(&root) {
            Ok(audio) => {
                tracing::info!(
                    target: "audio",
                    "audio enabled from {} (device @ {} Hz)",
                    root.display(),
                    audio.engine.sample_rate()
                );
                Some(audio)
            }
            Err(reason) => {
                tracing::warn!(
                    target: "audio",
                    "audio disabled: {reason} (root: {})",
                    root.display()
                );
                None
            }
        }
    }

    fn load_from_root(root: &Path) -> Result<Self, String> {
        // The index reader and the `objects/<hash[0..2]>/<hash>` resolution used to
        // live here privately. They are now `crate::asset_objects`, shared with the
        // title-screen panorama, which needs the same store for the same reason —
        // see that module for why `client.jar` is not the whole pack.
        let source = crate::asset_objects::AssetObjectStore::open(root)?;

        // sounds.json lives in the object store under this fixed index key.
        let sounds_json = source
            .object_bytes("minecraft/sounds.json")
            .ok_or_else(|| {
                format!(
                    "no minecraft/sounds.json object on disk (the index declares {} \
                     objects); run: cargo run -p xtask -- fetch-assets",
                    source.len()
                )
            })?;
        let registry =
            SoundRegistry::parse(&sounds_json).map_err(|e| format!("parsing sounds.json: {e}"))?;

        // The census. A registry with no samples resolves every event and plays
        // nothing, and nothing else in the pipeline can tell you that: the engine
        // reports a device, the registry reports 1968 events, and the speakers stay
        // quiet. Zero is a warning naming the fix, not a debug line.
        let (present, declared) = source.present_count(|name| name.ends_with(".ogg"));
        if present == 0 {
            tracing::warn!(
                target: "audio",
                declared,
                events = registry.len(),
                "audio is enabled but NO sound samples are on disk, so every sound will be \
                 silent. Run: cargo run -p xtask -- fetch-sounds --version <version>"
            );
        } else {
            tracing::info!(
                target: "audio",
                present,
                declared,
                events = registry.len(),
                "sound samples on disk"
            );
        }

        let engine = AudioEngine::new(registry, Box::new(source))
            .map_err(|e| format!("opening audio device: {e}"))?;
        Ok(Self {
            engine,
            reported_failure: false,
            subtitles: subtitles::SubtitleQueue::default(),
        })
    }

    /// Log one play failure: loud and actionable the first time, `debug` after.
    ///
    /// The overwhelmingly common cause is a missing `.ogg` object, which the
    /// engine reports as a resolution failure indistinguishable from a genuinely
    /// unknown event name — so the message names both possibilities rather than
    /// asserting the likely one.
    fn report_failure(&mut self, name: &str, error: &impl std::fmt::Display) {
        if self.reported_failure {
            tracing::debug!(target: "audio", "sound '{name}' not played: {error}");
            return;
        }
        self.reported_failure = true;
        tracing::warn!(
            target: "audio",
            "sound '{name}' not played: {error}. If sounds are silent, the .ogg corpus is \
             probably not on disk: cargo run -p xtask -- fetch-sounds --version <version>. \
             Further failures log at debug."
        );
    }

    /// Updates the listener from the render camera. Call once per frame.
    ///
    /// `up` is world `+Y`, matching the camera's own view matrix (a non-rolling
    /// FPS camera). Panning is documented in [`lodestone_audio`] as an
    /// equal-power *approximation*, not parity — vanilla delegates stereo
    /// placement to OpenAL-Soft's HRTF, which we do not reproduce — and using
    /// world-up here does not change that grading.
    pub fn set_listener(&self, camera: &Camera) {
        self.engine
            .set_listener(camera.position, camera.forward(), Vec3::Y);
    }

    /// Pushes the eleven `soundSource.*` slider values onto their mixer buses.
    ///
    /// A thin forward to
    /// [`AudioEngine::set_category_volumes`](lodestone_sound::AudioEngine::set_category_volumes),
    /// which exists because `ShellAudio::engine` is private and every other
    /// module reaches the engine through methods here. The whole set travels
    /// together under one mixer lock; see that method for why.
    pub fn set_category_volumes(&self, volumes: &[(SoundCategory, f32)]) {
        self.engine.set_category_volumes(volumes);
    }

    /// The slider value currently on `category`'s bus — the read-back half of
    /// [`Self::set_category_volumes`].
    #[must_use]
    pub fn category_volume(&self, category: SoundCategory) -> f32 {
        self.engine.category_volume(category)
    }

    /// Plays a positioned sound (the `SOUND` packet path). Resolution/decode
    /// failures are logged and swallowed — one bad sound must not stall the game.
    pub fn play_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.record_caption(name, pos);
        if let Err(e) = self
            .engine
            .play_sound(name, category, pos, volume, pitch, seed)
        {
            self.report_failure(name, &e);
        }
    }

    /// Record this sound's caption, if its event declares a `subtitles` key.
    ///
    /// Called before the engine, not after: a resolve failure (a missing `.ogg`)
    /// still means the event fired, and vanilla's own listener hook likewise runs
    /// off the *submission* rather than off successful decode.
    fn record_caption(&mut self, name: &str, pos: Vec3) {
        if let Some(key) = self.engine.subtitle(name) {
            let key = key.to_string();
            self.subtitles.push(&key, pos, caption_now_ms());
        }
    }

    /// Plays a **head-relative** one-shot sound — vanilla's own
    /// UI-sound shape (`ui.button.click` and anything else
    /// vanilla marks relative with no attenuation): no distance falloff, no
    /// panning, audible identically everywhere. No position argument at all,
    /// unlike [`play_sound`](Self::play_sound) — see
    /// [`lodestone_sound::AudioEngine::play_relative_sound`] for why a position
    /// would be ignored anyway. Captioned at the listener's own origin
    /// (`Vec3::ZERO`), which reads as "no arrow" — correct for a sound with no
    /// world source.
    pub fn play_relative_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.record_caption(name, Vec3::ZERO);
        if let Err(e) = self.engine.play_relative_sound(name, category, volume, pitch, seed) {
            self.report_failure(name, &e);
        }
    }

    /// This frame's drawable caption rows against the listener basis, or an empty
    /// vector when nothing is live. `forward` is the camera's own forward; `right`
    /// is derived from it here so every caller shares one basis.
    pub fn subtitle_captions(
        &mut self,
        camera: &Camera,
    ) -> Vec<subtitles::SubtitleCaption> {
        if self.subtitles.is_empty() {
            return Vec::new();
        }
        let forward = camera.forward();
        // `forward x up`, which for a non-rolling FPS camera is the listener's
        // right — the same basis `set_listener` hands the mixer.
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        self.subtitles
            .views(camera.position, forward, right, caption_now_ms())
    }

    /// Ask the engine to start a music track playing for real, reporting
    /// whether it produced anything.
    ///
    /// Returns [`MusicStart::Started`] only when the track genuinely resolved
    /// and a streaming voice is now live. In an ordinary checkout it returns
    /// [`MusicStart::Silent`], and that is **correct rather than a failure**:
    /// `cargo xtask fetch-sounds` excludes music by default, so 0 of 70 music
    /// objects are on disk and
    /// [`AudioEngine::start_music`](lodestone_sound::AudioEngine::start_music)
    /// reports a plain absence. `--all` adds 92 objects / 293 MB. One real
    /// 26.2 quirk to expect even with the full corpus:
    /// `music.nether.warped_forest` ships an **empty `sounds` array**, so
    /// that biome is silent by data.
    ///
    /// See [`AudioEngine::start_music`](lodestone_sound::AudioEngine::start_music)
    /// for the producer-thread/ring/mixer wiring that makes this audible —
    /// the "resolved but dropped" gap this call site used to have is closed
    /// there, not here.
    pub fn start_music(&mut self, music: &Music) -> MusicStart {
        // Seed 0: vanilla's own music-sound trigger takes no seed and so
        // leaves the weighted pick on its default.
        match self.engine.start_music(music.sound(), 0) {
            Ok(start) => start,
            Err(e) => {
                self.report_failure(music.sound(), &e);
                MusicStart::Silent
            }
        }
    }

    /// Stops the current music voice, if any. The `MusicSink::stop` half of
    /// [`MusicManager`](lodestone_sound::music::MusicManager)'s contract — see
    /// [`AudioEngine::stop_music`](lodestone_sound::AudioEngine::stop_music).
    pub fn stop_music(&self) {
        self.engine.stop_music();
    }

    /// Whether the track [`Self::start_music`] most recently started is still
    /// sounding — the `MusicSink::is_active` half.
    #[must_use]
    pub fn is_music_active(&self) -> bool {
        self.engine.is_music_active()
    }

    /// Sets the `Music` bus's runtime gain — the `MusicSink::set_music_gain`
    /// half, driving vanilla's own music-manager crossfade.
    pub fn set_music_gain(&self, gain: f32) {
        self.engine.set_music_gain(gain);
    }

    /// Start an ambient **loop** voice at `volume`, returning its handle.
    ///
    /// `None` means nothing playable resolved — the ordinary answer with no
    /// `.ogg` corpus on disk. The caller keeps the handle to ramp the volume
    /// ([`Self::set_loop_volume`]) and to stop it ([`Self::stop_loop`]); a loop
    /// never finishes on its own.
    pub fn start_loop(&mut self, name: &str, volume: f32) -> Option<lodestone_sound::PlayHandle> {
        match self
            .engine
            .play_loop(name, SoundCategory::Ambient, volume, 1.0, 0)
        {
            Ok(handle) => handle,
            Err(e) => {
                self.report_failure(name, &e);
                None
            }
        }
    }

    /// Push a live loop's crossfade volume.
    pub fn set_loop_volume(&self, handle: lodestone_sound::PlayHandle, volume: f32) {
        self.engine.set_voice_volume(handle, volume);
    }

    /// Stop a live loop.
    pub fn stop_loop(&self, handle: lodestone_sound::PlayHandle) {
        self.engine.stop_voice(handle);
    }

    /// Plays an entity-attached sound (the `SOUND_ENTITY` packet path) at the
    /// entity's current position.
    ///
    /// The caller resolves `pos` from the entity's live position at play time;
    /// this is a *snapshot*, not a follow. Per-frame position tracking for a
    /// moving entity is a documented enhancement (the engine already exposes
    /// [`AudioEngine::set_voice_position`](lodestone_sound::AudioEngine::set_voice_position)
    /// for it), but a snapshot is correct for the short SFX that dominate the
    /// entity-sound path.
    pub fn play_entity_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.record_caption(name, pos);
        if let Err(e) = self
            .engine
            .play_entity_sound(name, category, pos, volume, pitch, seed)
        {
            self.report_failure(name, &e);
        }
    }
}

// The asset-index parsing, `objects/<hash[0..2]>/<hash>` resolution and
// `assets/`-prefix-stripping tests that used to live here moved with the code into
// `crate::asset_objects`, which is now the single reader of the index (the
// title-screen panorama needs the same store). The equivalents there are
// `the_index_parses_name_to_hash_and_size`,
// `an_empty_or_malformed_index_is_an_error_not_a_silent_empty`,
// `the_object_path_is_the_two_character_fanout`, and the prefix-strip assertion
// inside `a_short_read_is_absence_not_a_truncated_asset` — which also gained a
// control proving the strip is what made the lookup hit, and a length check this
// copy never had.


// ---------------------------------------------------------------------------
// Browser (`wasm32`) arm — see this module's docs for the architecture.
// ---------------------------------------------------------------------------

/// `ScriptProcessorNode` frames per `onaudioprocess` callback. A power of two
/// in the WHATWG-mandated `[256, 16384]` range; `2048` at a typical `44100`/
/// `48000` Hz context is ~43-46 ms of buffering — small enough that the
/// listener/category-volume writes each frame (`Sim::set_audio_listener`,
/// `Sim::set_sound_volumes`) reach the mixer promptly, large enough that the
/// main-thread callback (JS event-loop cadence, not a realtime audio thread)
/// is not woken absurdly often.
#[cfg(target_arch = "wasm32")]
const SCRIPT_PROCESSOR_BUFFER_FRAMES: u32 = 2048;

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// The browser session's live audio, or `None` before
    /// [`ShellAudio::from_env`] installs one.
    ///
    /// A `thread_local`, not a field on [`ShellAudio`] — see that type's doc
    /// for why: `bevy_ecs::Resource` (via `Component`) requires `Send + Sync`
    /// unconditionally in this workspace's `bevy_ecs` build, `web_sys`/
    /// `wasm_bindgen::closure::Closure` types are `!Send`/`!Sync` by design,
    /// and this crate `deny`s `unsafe_code`, which rules out asserting
    /// `Send`/`Sync` on a wrapper by hand. `wasm32-unknown-unknown` without
    /// the `atomics` target feature is genuinely single-threaded, so a
    /// `thread_local!` holding the real state, reached through a zero-sized,
    /// trivially-`Send`-`Sync` `ShellAudio` handle, is sound without any
    /// `unsafe` and needs no change to how `sim.rs` stores the `AudioEngine`
    /// resource.
    static AUDIO_STATE: RefCell<Option<AudioState>> = const { RefCell::new(None) };
}

/// Ring capacity for one streaming voice, in samples (interleaved across
/// `channels`): about half a second of the source's own audio. Same
/// reasoning and same figure as native's `lodestone_sound::engine`-private
/// `stream_ring_capacity` — kept as a small independent copy here rather than
/// a shared export, since the two targets' producers (a thread vs. a
/// per-callback pump) are different enough that sharing the constant alone
/// would not remove any real duplication.
#[cfg(target_arch = "wasm32")]
fn stream_ring_capacity(source_rate: u32, channels: u16) -> usize {
    let half_second = (source_rate as usize / 2) * usize::from(channels.max(1));
    half_second.max(2)
}

/// The browser's music producer: a lazily-decoding [`VorbisStream`] plus the
/// ring it feeds, pumped from the **main thread** rather than a spawned one.
///
/// # Why not a thread, the way native does it
///
/// `lodestone_sound::AudioEngine::start_music` (native) spawns a real OS
/// thread to decode ahead of the ring. `std::thread::spawn` **traps** on
/// wasm32 (see `CLAUDE.md`'s rendering-constraints/browser-shell section),
/// and wasm32-unknown-unknown without the `atomics` target feature has no
/// second thread to put one on regardless. So instead of a producer thread,
/// [`pump_music_producer`] is called from inside the very same
/// `onaudioprocess` closure that already runs [`Mixer::render`] — a
/// `ScriptProcessorNode`'s callback runs on the **main** thread (this
/// module's own doc explains why that is the sink used here rather than an
/// `AudioWorklet`), so "decode a little, then render" in one callback is
/// exactly as safe as "render" alone was: no second thread is created or
/// needed. Vorbis packet decode is cheap enough (a few thousand samples) that
/// pumping a bounded handful of packets per callback keeps the ring topped up
/// without making any one callback noticeably slower.
///
/// [`VorbisStream`]: lodestone_audio::VorbisStream
#[cfg(target_arch = "wasm32")]
struct MusicProducer {
    stream: lodestone_audio::VorbisStream,
    ring: std::sync::Arc<lodestone_audio::SampleRing>,
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Decodes up to a handful of packets into `producer.ring`, stopping early
/// once the ring has no more free space (so a callback with a well-fed ring
/// does negligible extra work) or once the stream ends or errors (in which
/// case [`MusicProducer::ended`] is set, exactly like native's producer
/// thread does on its own end/error path).
///
/// Called from two places: once synchronously when
/// [`ShellAudio::start_music`] first resolves a track (so the very first
/// render block already has something to play instead of guaranteed silence
/// for one callback), and once per `onaudioprocess` callback thereafter.
#[cfg(target_arch = "wasm32")]
fn pump_music_producer(producer: &mut MusicProducer) {
    /// Bounds the decode work done inside one audio callback. A real vanilla
    /// music packet decodes to a few thousand samples; even a generous
    /// multiple of that per callback is a small fraction of a
    /// `SCRIPT_PROCESSOR_BUFFER_FRAMES`-sized callback's own budget.
    const MAX_PACKETS_PER_PUMP: u32 = 8;
    for _ in 0..MAX_PACKETS_PER_PUMP {
        if producer.ring.free() == 0 {
            return;
        }
        match producer.stream.next_packet() {
            Ok(Some(packet)) => {
                let mut offset = 0;
                while offset < packet.len() {
                    let written = producer.ring.write(&packet[offset..]);
                    if written == 0 {
                        // Ring is full; resume from here next callback rather
                        // than looping — there is no second thread to back
                        // off on here, and the caller (the render callback
                        // itself) must return promptly either way.
                        return;
                    }
                    offset += written;
                }
            }
            Ok(None) => {
                producer.ended.store(true, std::sync::atomic::Ordering::Release);
                return;
            }
            Err(_) => {
                // A mid-stream decode error: stop producing and mark ended so
                // the voice finishes cleanly once the ring drains, rather
                // than waiting forever for samples that will never arrive.
                producer.ended.store(true, std::sync::atomic::Ordering::Release);
                return;
            }
        }
    }
}

/// The real state behind the browser's audio: [`SoundResolver`] (resolve +
/// decode) plus a device-free [`Mixer`] a `ScriptProcessorNode` pulls from
/// every callback. See this module's docs for why a `ScriptProcessorNode`
/// rather than an `AudioWorklet`, and for what `web/` still has to stage
/// before this produces anything but silence.
///
/// [`SoundResolver`]: lodestone_sound::SoundResolver
/// [`Mixer`]: lodestone_audio::Mixer
#[cfg(target_arch = "wasm32")]
struct AudioState {
    ctx: web_sys::AudioContext,
    // `Rc<RefCell<_>>`, not a plain field: the `onaudioprocess` closure below
    // needs its own handle to the *same* mixer the game-thread methods write
    // into. Wasm is single-threaded (the closure runs on the same main thread
    // as everything else, just at a different point in the event loop), so a
    // `RefCell` — not a `Mutex` — is the honest tool; there is no second thread
    // to race with.
    mixer: Rc<RefCell<lodestone_audio::Mixer>>,
    resolver: lodestone_sound::SoundResolver,
    subtitles: subtitles::SubtitleQueue,
    reported_failure: bool,
    /// The one music voice's producer, if a track is currently playing —
    /// shared with the `onaudioprocess` closure exactly like `mixer` is, so
    /// both the game-thread `start_music` call and the per-callback pump can
    /// reach it. `None` means no music voice is live.
    music_producer: Rc<RefCell<Option<MusicProducer>>>,
    /// The mixer handle for the current music voice, if any — this struct's
    /// own half of the same single-slot contract native's `AudioEngine`
    /// keeps in its `current_music` field.
    current_music: Option<lodestone_audio::PlayHandle>,
    // Kept alive for the state's lifetime (config-scoped, effectively the
    // whole session): dropping either would disconnect the callback, and a
    // `ScriptProcessorNode` whose JS side still holds the closure reference
    // but whose Rust closure has been freed is a wasm-bindgen use-after-free,
    // not a graceful no-op.
    _node: web_sys::ScriptProcessorNode,
    _on_audio_process: Closure<dyn FnMut(web_sys::AudioProcessingEvent)>,
}

#[cfg(target_arch = "wasm32")]
impl AudioState {
    /// Log one play failure: loud and actionable the first time, `debug`
    /// after. Mirrors native's [`ShellAudio::report_failure`] — same method
    /// name, same reasoning, different `cfg`.
    fn report_failure(&mut self, name: &str, error: &impl std::fmt::Display) {
        if self.reported_failure {
            tracing::debug!(target: "audio", "sound '{name}' not played: {error}");
            return;
        }
        self.reported_failure = true;
        tracing::warn!(
            target: "audio",
            "sound '{name}' not played: {error}. If sounds are silent, the staged .ogg \
             subset probably does not cover this event — see \
             platform::assets::Bundle::sound_objects. Further failures log at debug."
        );
    }

    fn record_caption(&mut self, name: &str, pos: Vec3) {
        if let Some(key) = self.resolver.subtitle(name) {
            let key = key.to_string();
            self.subtitles.push(&key, pos, caption_now_ms());
        }
    }

    /// Stops the current music voice, if any, and drops its producer so the
    /// `onaudioprocess` closure stops pumping it. Mirrors native's
    /// `AudioEngine::stop_music` — the mixer half here (`Mixer::stop`) plus
    /// the "who feeds it" half (dropping `music_producer`, this target's
    /// equivalent of signalling a producer thread to exit).
    fn stop_music(&mut self) {
        if let Some(handle) = self.current_music.take() {
            self.mixer.borrow_mut().stop(handle);
        }
        *self.music_producer.borrow_mut() = None;
    }

    fn play(
        &mut self,
        name: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.record_caption(name, pos);
        match self
            .resolver
            .resolve_instance(name, category, pos, volume, pitch, seed)
        {
            Ok(Some(instance)) => {
                self.mixer.borrow_mut().play(instance);
            }
            // Vanilla's silent "empty sound" (unknown event / zero weight) —
            // not an error, matching `SoundResolver`'s own contract.
            Ok(None) => {}
            Err(e) => self.report_failure(name, &e),
        }
    }

    /// Head-relative one-shot: `Self::play` minus the position, forcing
    /// `instance.relative` after resolve — the same shape native's
    /// `AudioEngine::play_relative_sound` forces, since `SoundResolver` here
    /// carries no separate "relative" resolve path of its own.
    fn play_relative(
        &mut self,
        name: &str,
        category: SoundCategory,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.record_caption(name, Vec3::ZERO);
        match self
            .resolver
            .resolve_instance(name, category, Vec3::ZERO, volume, pitch, seed)
        {
            Ok(Some(mut instance)) => {
                instance.relative = true;
                self.mixer.borrow_mut().play(instance);
            }
            Ok(None) => {}
            Err(e) => self.report_failure(name, &e),
        }
    }
}

/// The shell's handle to the browser's audio — see [`AUDIO_STATE`] for why
/// this carries no fields of its own. Constructed once via
/// [`ShellAudio::from_env`]; `None` means audio is disabled (no staged bundle,
/// no `sounds.json`, or `AudioContext` creation failed) and every call site is
/// a simple `if let Some(audio)` — the same contract native's arm makes.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy)]
pub struct ShellAudio;

#[cfg(target_arch = "wasm32")]
impl ShellAudio {
    /// Brings audio up from `web/`'s staged [`platform::assets::Bundle`], or
    /// returns `None` with a logged reason — the same three-tier degrade
    /// native's `from_env` makes (no bytes / bad bytes / device failure), just
    /// with "bytes" meaning "staged in the process-wide bundle" instead of
    /// "found on disk".
    ///
    /// [`platform::assets::Bundle`]: crate::platform::assets::Bundle
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let Some(bundle) = crate::platform::assets::bundle() else {
            tracing::info!(
                target: "audio",
                "audio disabled: no asset bundle installed yet (web/ has not called \
                 platform::assets::install)"
            );
            return None;
        };
        if bundle.sounds_json.is_empty() {
            tracing::info!(
                target: "audio",
                "audio disabled: no sounds.json staged for this session (web/'s build did \
                 not fetch minecraft/sounds.json into the bundle)"
            );
            return None;
        }
        let registry = match SoundRegistry::parse(&bundle.sounds_json) {
            Ok(registry) => registry,
            Err(e) => {
                tracing::warn!(target: "audio", "audio disabled: parsing sounds.json: {e}");
                return None;
            }
        };

        // Same "silence describes itself" census native's `load_from_root`
        // does — a registry with no samples resolves every event and plays
        // nothing, and nothing else in the pipeline can tell you that.
        let mut source = lodestone_assets::MemorySource::new("wasm-sound-bundle");
        for (key, bytes) in &bundle.sound_objects {
            // `ResolvedSound::file_path` already produces the
            // `assets/<ns>/sounds/<path>.ogg` form `MemorySource` keys on; the
            // staged bundle carries the bare asset-index name (no `assets/`
            // prefix, matching `panorama`'s convention), so this is the one
            // translation point.
            source.insert(format!("assets/{key}"), bytes.clone());
        }
        let present = bundle.sound_objects.len();
        if present == 0 {
            tracing::warn!(
                target: "audio",
                events = registry.len(),
                "audio is enabled but the browser build staged NO sound samples, so every \
                 sound is silent until web/ stages a curated .ogg subset into \
                 platform::assets::Bundle::sound_objects"
            );
        } else {
            tracing::info!(
                target: "audio",
                present,
                events = registry.len(),
                "sound samples staged for the browser build"
            );
        }

        let ctx = match web_sys::AudioContext::new() {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::warn!(target: "audio", "audio disabled: AudioContext::new() failed: {e:?}");
                return None;
            }
        };
        // Rounds rather than truncates: `sample_rate()` is a real device rate
        // (44100/48000 typically) reported as `f32`, and a mixer built one Hz
        // low is an audible, permanent pitch error for the whole session.
        let sample_rate = ctx.sample_rate().round().max(1.0) as u32;
        let mixer = Rc::new(RefCell::new(lodestone_audio::Mixer::new(sample_rate)));
        let music_producer: Rc<RefCell<Option<MusicProducer>>> = Rc::new(RefCell::new(None));

        let node = match ctx
            .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(
                SCRIPT_PROCESSOR_BUFFER_FRAMES,
                0,
                lodestone_audio::OUTPUT_CHANNELS as u32,
            ) {
            Ok(node) => node,
            Err(e) => {
                tracing::warn!(target: "audio", "audio disabled: createScriptProcessor failed: {e:?}");
                return None;
            }
        };

        let on_audio_process = {
            let mixer = Rc::clone(&mixer);
            let music_producer = Rc::clone(&music_producer);
            // Reused across every callback rather than allocated per call:
            // `onaudioprocess` fires at real-time cadence (~20-40 Hz at this
            // buffer size), and a fresh `Vec` on every one of those is
            // needless per-callback churn on the same thread driving the rest
            // of the game.
            let mut interleaved: Vec<f32> = Vec::new();
            let mut left: Vec<f32> = Vec::new();
            let mut right: Vec<f32> = Vec::new();
            Closure::<dyn FnMut(web_sys::AudioProcessingEvent)>::new(
                move |event: web_sys::AudioProcessingEvent| {
                    let Ok(output) = event.output_buffer() else {
                        return;
                    };
                    // Decode-ahead happens HERE, on the main thread, before
                    // rendering this block — see `MusicProducer`'s doc for why
                    // this replaces a producer thread on wasm32. Bounded (at
                    // most `MAX_PACKETS_PER_PUMP` packets), so a well-fed ring
                    // costs one `free() == 0` check and returns immediately.
                    if let Some(producer) = music_producer.borrow_mut().as_mut() {
                        pump_music_producer(producer);
                    }
                    let frames = output.length() as usize;
                    let needed = frames * lodestone_audio::OUTPUT_CHANNELS;
                    if interleaved.len() != needed {
                        interleaved.resize(needed, 0.0);
                        left.resize(frames, 0.0);
                        right.resize(frames, 0.0);
                    }
                    mixer.borrow_mut().render(&mut interleaved);
                    // `Mixer::render` always produces interleaved stereo
                    // (`OUTPUT_CHANNELS == 2`) regardless of how many output
                    // channels this node was created with; `AudioBuffer`
                    // channels are planar, so split it into the two per-channel
                    // arrays WebAudio wants. A mono `ScriptProcessorNode` is not
                    // requested above for exactly this reason — de-interleaving
                    // down to one channel would need to *mix* L+R, which the
                    // mixer already decided not to do.
                    for i in 0..frames {
                        left[i] = interleaved[i * lodestone_audio::OUTPUT_CHANNELS];
                        right[i] = interleaved[i * lodestone_audio::OUTPUT_CHANNELS + 1];
                    }
                    // A failed `copyToChannel` (e.g. the buffer shrank between
                    // the two calls) leaves that block silent rather than
                    // panicking the callback — one glitched block must not be
                    // worse than a decode failure elsewhere in this file.
                    let _ = output.copy_to_channel(&left, 0);
                    let _ = output.copy_to_channel(&right, 1);
                },
            )
        };
        node.set_onaudioprocess(Some(on_audio_process.as_ref().unchecked_ref()));

        if let Err(e) = node.connect_with_audio_node(&ctx.destination()) {
            tracing::warn!(
                target: "audio",
                "audio disabled: connecting the mixer node to the audio destination failed: {e:?}"
            );
            return None;
        }

        tracing::info!(
            target: "audio",
            "audio enabled (browser WebAudio, device @ {sample_rate} Hz, AudioContext {:?} \
             — call ShellAudio::resume_on_gesture() from a real input event to start it)",
            ctx.state(),
        );

        // Replaces whatever was here, matching native: a fresh `Sim` (a new
        // singleplayer/multiplayer session in the same tab) calls `from_env`
        // again, and the old device — like the old `cpal` stream native
        // drops — must not linger and keep rendering into a destination
        // nothing references any more.
        AUDIO_STATE.with(|cell| {
            *cell.borrow_mut() = Some(AudioState {
                ctx,
                mixer,
                resolver: lodestone_sound::SoundResolver::new(registry, Box::new(source)),
                subtitles: subtitles::SubtitleQueue::default(),
                reported_failure: false,
                music_producer,
                current_music: None,
                _node: node,
                _on_audio_process: on_audio_process,
            });
        });
        Some(Self)
    }

    /// Resumes the audio context. Call this from an input-event handler
    /// (click/keydown) — never eagerly — so the browser's autoplay gate
    /// actually lifts. `resume()` returns a `Promise`; there is nothing this
    /// call needs to await (there is no "started" callback anyone here is
    /// waiting on), so this is fire-and-forget.
    pub fn resume_on_gesture(&self) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                // Read before the call, not after: `resume()` returns a `Promise`
                // and this is deliberately fire-and-forget (see this method's own
                // doc), so `ctx.state()` immediately afterward would still often
                // read `suspended` even on a call that is about to succeed — that
                // would make a *correct* resume log as a failure. The pre-call
                // read is what makes the log line mean "a resume was actually
                // requested", not "the browser confirmed it synchronously" —
                // which the API structurally cannot promise.
                let was_suspended = state.ctx.state() == web_sys::AudioContextState::Suspended;
                if let Err(e) = state.ctx.resume() {
                    tracing::warn!(target: "audio", "AudioContext::resume() failed: {e:?}");
                } else if was_suspended {
                    // The one observable signal this call has: it was asked to
                    // lift the autoplay gate at least once.
                    tracing::info!(
                        target: "audio",
                        "AudioContext::resume() requested from a real user gesture \
                         (was suspended)"
                    );
                }
            }
        });
    }

    /// Updates the listener from the render camera. Call once per frame — see
    /// native's [`set_listener`](Self::set_listener) doc for the basis
    /// convention, identical here.
    pub fn set_listener(&self, camera: &Camera) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state.mixer.borrow_mut().set_listener(lodestone_audio::Listener {
                    position: camera.position,
                    forward: camera.forward(),
                    up: Vec3::Y,
                });
            }
        });
    }

    /// Pushes the eleven `soundSource.*` slider values onto their mixer buses.
    /// Identical contract to native's
    /// [`set_category_volumes`](Self::set_category_volumes): a thin forward,
    /// through the same [`lodestone_sound::map_category`] ordinal bridge
    /// native's `AudioEngine` uses, so the two targets cannot drift onto
    /// different buses for the same slider.
    pub fn set_category_volumes(&self, volumes: &[(SoundCategory, f32)]) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                let mut mixer = state.mixer.borrow_mut();
                let buses = mixer.volumes_mut();
                for (category, volume) in volumes {
                    buses.set_user(lodestone_sound::map_category(*category), *volume);
                }
            }
        });
    }

    /// The slider value currently on `category`'s bus. `1.0` (full volume,
    /// the mixer's own default) when audio is disabled — the read-back must
    /// never itself be the thing that reveals a silenced session.
    #[must_use]
    pub fn category_volume(&self, category: SoundCategory) -> f32 {
        AUDIO_STATE.with(|cell| {
            cell.borrow().as_ref().map_or(1.0, |state| {
                state
                    .mixer
                    .borrow()
                    .volumes()
                    .user(lodestone_sound::map_category(category))
            })
        })
    }

    /// Plays a positioned sound (the `SOUND` packet path). Resolution/decode
    /// failures are logged and swallowed, matching native.
    pub fn play_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.play(name, category, pos, volume, pitch, seed);
            }
        });
    }

    /// Plays a head-relative one-shot sound (`ui.button.click` and anything
    /// else vanilla marks `RELATIVE`) — identical contract to native's
    /// [`play_relative_sound`](Self::play_relative_sound).
    pub fn play_relative_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.play_relative(name, category, volume, pitch, seed);
            }
        });
    }

    /// This frame's drawable caption rows — identical to native's
    /// [`subtitle_captions`](Self::subtitle_captions).
    pub fn subtitle_captions(&mut self, camera: &Camera) -> Vec<subtitles::SubtitleCaption> {
        AUDIO_STATE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return Vec::new();
            };
            if state.subtitles.is_empty() {
                return Vec::new();
            }
            let forward = camera.forward();
            let right = forward.cross(Vec3::Y).normalize_or_zero();
            state
                .subtitles
                .views(camera.position, forward, right, caption_now_ms())
        })
    }

    /// Starts a music track playing for real: resolves it exactly like
    /// native's `resolve_music` used to, then builds a [`MusicProducer`] and
    /// a fresh [`SampleRing`](lodestone_audio::SampleRing), pumps it once
    /// synchronously (so the very first render block already has something
    /// to play), and hands the ring to the mixer as a new streaming voice.
    /// See [`MusicProducer`]'s doc for why decode happens on the main thread
    /// here instead of a spawned one, and this module's header for the
    /// realtime discipline the `onaudioprocess` closure keeps either way.
    ///
    /// Only one music voice may be live at a time (matching vanilla's single
    /// `currentMusic`); starting a new one first stops whatever this state
    /// was previously tracking, exactly like native's `start_music`.
    pub fn start_music(&mut self, music: &Music) -> MusicStart {
        AUDIO_STATE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return MusicStart::Silent;
            };
            state.stop_music();
            match state.resolver.resolve_streaming(music.sound(), 0) {
                Ok(Some(streaming)) => {
                    let source_rate = streaming.stream.sample_rate();
                    let channels = streaming.stream.channels();
                    let ring = std::sync::Arc::new(lodestone_audio::SampleRing::with_min_capacity(
                        stream_ring_capacity(source_rate, channels),
                    ));
                    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let mut producer = MusicProducer {
                        stream: streaming.stream,
                        ring: std::sync::Arc::clone(&ring),
                        ended: std::sync::Arc::clone(&ended),
                    };
                    pump_music_producer(&mut producer);

                    let source = lodestone_audio::StreamSource {
                        ring,
                        source_channels: channels,
                        source_rate,
                        category: lodestone_sound::map_category(
                            lodestone_model::event::SoundCategory::Music,
                        ),
                        volume: streaming.volume,
                        pitch: streaming.pitch,
                        ended,
                    };
                    let handle = state.mixer.borrow_mut().play_stream(source);
                    state.current_music = Some(handle);
                    *state.music_producer.borrow_mut() = Some(producer);
                    MusicStart::Started
                }
                Ok(None) => MusicStart::Silent,
                Err(e) => {
                    state.report_failure(music.sound(), &e);
                    MusicStart::Silent
                }
            }
        })
    }

    /// Stops the current music voice, if any. The `MusicSink::stop` half of
    /// [`MusicManager`](lodestone_sound::music::MusicManager)'s contract.
    pub fn stop_music(&self) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.stop_music();
            }
        });
    }

    /// Whether the track [`Self::start_music`] most recently started is
    /// still sounding — the `MusicSink::is_active` half. Also the cleanup
    /// point for a track that finished on its own: a `false` answer here
    /// also drops the stale producer, mirroring native's
    /// `AudioEngine::is_music_active`.
    #[must_use]
    pub fn is_music_active(&self) -> bool {
        AUDIO_STATE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return false;
            };
            let Some(handle) = state.current_music else {
                return false;
            };
            let active = state.mixer.borrow().is_active(handle);
            if !active {
                state.current_music = None;
                *state.music_producer.borrow_mut() = None;
            }
            active
        })
    }

    /// Sets the `Music` bus's runtime gain — the `MusicSink::set_music_gain`
    /// half, driving vanilla's own music-manager crossfade.
    pub fn set_music_gain(&self, gain: f32) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state
                    .mixer
                    .borrow_mut()
                    .volumes_mut()
                    .set_runtime_gain(lodestone_audio::SoundCategory::Music, gain);
            }
        });
    }

    /// Start an ambient **loop** voice at `volume`, returning its handle —
    /// identical contract to native's [`start_loop`](Self::start_loop):
    /// forces `looping`/`relative`, same as vanilla's own
    /// biome ambient-sounds loop-sound instance.
    pub fn start_loop(&mut self, name: &str, volume: f32) -> Option<lodestone_sound::PlayHandle> {
        AUDIO_STATE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let state = guard.as_mut()?;
            match state
                .resolver
                .resolve_instance(name, SoundCategory::Ambient, Vec3::ZERO, volume, 1.0, 0)
            {
                Ok(Some(mut instance)) => {
                    instance.looping = true;
                    instance.relative = true;
                    Some(state.mixer.borrow_mut().play(instance))
                }
                Ok(None) => None,
                Err(e) => {
                    state.report_failure(name, &e);
                    None
                }
            }
        })
    }

    /// Push a live loop's crossfade volume.
    pub fn set_loop_volume(&self, handle: lodestone_sound::PlayHandle, volume: f32) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state.mixer.borrow_mut().set_voice_volume(handle, volume);
            }
        });
    }

    /// Stop a live loop.
    pub fn stop_loop(&self, handle: lodestone_sound::PlayHandle) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state.mixer.borrow_mut().stop(handle);
            }
        });
    }

    /// Plays an entity-attached sound (the `SOUND_ENTITY` packet path).
    /// Identical to [`Self::play_sound`]; see native's
    /// [`play_entity_sound`](Self::play_entity_sound) doc for the
    /// snapshot-not-follow caveat, which applies here too.
    pub fn play_entity_sound(
        &mut self,
        name: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        AUDIO_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.play(name, category, pos, volume, pitch, seed);
            }
        });
    }
}
