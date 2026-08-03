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
        if let Err(e) = self
            .engine
            .play_sound(name, category, pos, volume, pitch, seed)
        {
            self.report_failure(name, &e);
        }
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

