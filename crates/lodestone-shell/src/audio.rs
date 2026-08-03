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
//! # Asset store, and why it is opt-in by an explicit path
//!
//! `sounds.json` and every `.ogg` do **not** live in `client.jar`; they live in
//! the launcher's asset-object store, addressed by an `asset-index-*.json` that
//! maps an in-pack key (`minecraft/sounds/…​.ogg`) to
//! `objects/<sha1[0..2]>/<sha1>`. That resolution is
//! [`crate::asset_objects`] — extracted out of this module once the title-screen
//! panorama turned out to need the same store, because `client.jar` ships stubs
//! for the files it overrides. Read that module before assuming the jar is the
//! whole pack. The shell is deliberately version-free (it
//! knows only a protocol *number*), so it must not hardcode a version directory.
//! Instead the asset root is supplied explicitly via the `LODESTONE_ASSET_ROOT`
//! environment variable (a directory such as `.cache/mc/26.2`). If it is unset
//! or unusable, audio is **disabled with a logged reason** and the game runs on
//! — an honest, visible degradation, never a silent half-state.
//!
//! We deliberately do **not** guess a version by scanning `.cache/mc/*`:
//! multiple client versions coexist there, and "first match wins over a shared
//! directory" is a known cross-agent landmine (§ the fixture-selection rule).
//! One explicit path, or nothing.
//!
//! [`ClientEvent::Sound`]: lodestone_client::ClientEvent::Sound
//! [`ClientEvent::EntitySound`]: lodestone_client::ClientEvent::EntitySound
//! [`NetUpdate`]: crate::net::NetUpdate

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use glam::Vec3;
use lodestone_assets::sound::SoundRegistry;
use lodestone_model::event::SoundCategory;
use lodestone_render::Camera;
use lodestone_sound::AudioEngine;

/// Environment variable naming the Minecraft asset root (the directory that
/// contains exactly one `asset-index-*.json` and an `objects/` tree). Unset ⇒
/// audio disabled.
pub const ASSET_ROOT_ENV: &str = "LODESTONE_ASSET_ROOT";

/// The shell's live audio, wrapping a device-backed [`AudioEngine`].
///
/// Constructed once via [`ShellAudio::from_env`]; `None` means audio is disabled
/// (no asset root, load failure, or no output device) and every call site is a
/// simple `if let Some(audio)`.
#[derive(Debug)]
pub struct ShellAudio {
    engine: AudioEngine,
}

impl ShellAudio {
    /// Brings audio up from the `LODESTONE_ASSET_ROOT` asset store, or returns
    /// `None` with a logged reason.
    ///
    /// Failure is never fatal to the game: a missing asset root, an unreadable
    /// index, or an unavailable output device all log and yield `None`. The one
    /// thing this must not do is come up *silently working-but-mute*, which is
    /// why the load path reads and parses `sounds.json` eagerly — a broken store
    /// fails here, at startup, not as unexplained silence later.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let Some(root) = std::env::var_os(ASSET_ROOT_ENV).map(PathBuf::from) else {
            tracing::info!(
                target: "audio",
                "audio disabled: set {ASSET_ROOT_ENV} to a Minecraft asset dir \
                 (e.g. .cache/mc/26.2) to enable sound"
            );
            return None;
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

        let engine = AudioEngine::new(registry, Box::new(source))
            .map_err(|e| format!("opening audio device: {e}"))?;
        Ok(Self { engine })
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
            tracing::debug!(target: "audio", "sound '{name}' not played: {e}");
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
            tracing::debug!(target: "audio", "entity sound '{name}' not played: {e}");
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

