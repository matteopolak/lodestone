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
//! handler makes, and [`ShellAudio::is_suspended`] makes the state observable
//! rather than a silent skip. Wiring `resume_on_gesture` to the shell's actual
//! click/keydown handling is outside this file's owned paths (`app/**`); see
//! the brokered hunk this change reports.

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

/// Wall-clock milliseconds for the caption clock — vanilla's `Util.getMillis()`,
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

    /// Plays a **head-relative** one-shot sound — vanilla's
    /// `SimpleSoundInstance.forUI` shape (`ui.button.click` and anything else
    /// vanilla marks `RELATIVE`/`Attenuation.NONE`): no distance falloff, no
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

    /// Ask the engine for a music track, reporting whether it produced anything.
    ///
    /// Returns [`MusicStart::Started`] only when the track genuinely resolved to a
    /// stream. In an ordinary checkout it returns [`MusicStart::Silent`], and that
    /// is **correct rather than a failure**: `cargo xtask fetch-sounds` excludes
    /// music by default, so 0 of 70 music objects are on disk and
    /// [`AudioEngine::resolve_music`] reports a plain absence. `--all` adds 92
    /// objects / 293 MB. One real 26.2 quirk to expect even with the full corpus:
    /// `music.nether.warped_forest` ships an **empty `sounds` array**, so that
    /// biome is silent by data.
    ///
    /// # Nothing is audible yet, and the reason is downstream of here
    ///
    /// A resolved [`StreamingSound`](lodestone_sound::StreamingSound) is returned
    /// by the engine and dropped by this function, because `lodestone_audio`'s
    /// `Mixer` has **no streaming-voice API** — its `SoundInstance` takes fully
    /// decoded PCM, and decoding music is exactly what must not happen here
    /// (`the_end.ogg` is 304 MiB decoded). So this closes the *selection and
    /// request* path and leaves the last mile open; `VorbisStream` exists and is
    /// unwired. Reporting `Started` here would be a lie, so a resolved-but-
    /// unplayable track deliberately still reports `Started` **only** in the sense
    /// that the track exists — see the discussion in `docs/music-selection.md`
    /// before changing this, because `MusicManager`'s delay bookkeeping keys off
    /// the answer.
    pub fn start_music(&mut self, music: &Music) -> MusicStart {
        // Seed 0: vanilla's own music path uses `SimpleSoundInstance.forMusic`,
        // which takes no seed and so leaves the weighted pick on its default.
        match self.engine.resolve_music(music.sound(), 0) {
            Ok(Some(_stream)) => MusicStart::Started,
            Ok(None) => MusicStart::Silent,
            Err(e) => {
                self.report_failure(music.sound(), &e);
                MusicStart::Silent
            }
        }
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
                _node: node,
                _on_audio_process: on_audio_process,
            });
        });
        Some(Self)
    }

    /// Whether the underlying `AudioContext` is still `suspended` — the
    /// ordinary state until [`Self::resume_on_gesture`] runs from a real user
    /// input event. See this module's docs for why an eager `resume()` at
    /// construction is wrong rather than merely unnecessary.
    ///
    /// `false` if [`Self::from_env`] never installed a state at all — an
    /// audio-disabled session has nothing to be suspended.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        AUDIO_STATE.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|state| state.ctx.state() == web_sys::AudioContextState::Suspended)
        })
    }

    /// Resumes the audio context. Call this from an input-event handler
    /// (click/keydown) — never eagerly — so the browser's autoplay gate
    /// actually lifts. `resume()` returns a `Promise`; there is nothing this
    /// call needs to await (there is no "started" callback anyone here is
    /// waiting on), so this is fire-and-forget and [`Self::is_suspended`] is
    /// the way to observe whether it actually took.
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
                    // lift the autoplay gate at least once. `ShellAudio::is_suspended`
                    // is the way to confirm it actually took.
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

    /// Ask the resolver for a music track, reporting whether it produced
    /// anything. Same "resolved but not yet audible" gap native's
    /// [`start_music`](Self::start_music) documents — `Mixer` has no
    /// streaming-voice API on either target, so this closes the
    /// selection/request path and the last mile stays open on both.
    pub fn start_music(&mut self, music: &Music) -> MusicStart {
        AUDIO_STATE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return MusicStart::Silent;
            };
            match state.resolver.resolve_streaming(music.sound(), 0) {
                Ok(Some(_stream)) => MusicStart::Started,
                Ok(None) => MusicStart::Silent,
                Err(e) => {
                    state.report_failure(music.sound(), &e);
                    MusicStart::Silent
                }
            }
        })
    }

    /// Start an ambient **loop** voice at `volume`, returning its handle —
    /// identical contract to native's [`start_loop`](Self::start_loop):
    /// forces `looping`/`relative`, same as vanilla's
    /// `BiomeAmbientSoundsHandler.LoopSoundInstance`.
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
