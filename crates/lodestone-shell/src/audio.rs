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

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use glam::Vec3;
use lodestone_assets::sound::SoundRegistry;
use lodestone_model::event::SoundCategory;
use lodestone_render::Camera;
use lodestone_sound::AudioEngine;
use lodestone_sound::music::{Music, MusicStart};

pub(crate) mod ambient;
pub(crate) mod music;
pub(crate) mod subtitles;

/// Wall-clock milliseconds for the caption clock — vanilla's `Util.getMillis()`,
/// the same origin `gpu/glint.rs` and `app::recipe_toast_now_ms` use, so a caption
/// ages against the same clock every other timed overlay does.
fn caption_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
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
#[derive(Debug)]
pub struct ShellAudio {
    engine: AudioEngine,
    /// The sound-subtitle captions (issue #198). Fed here rather than at each
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

