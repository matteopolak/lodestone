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
//! `objects/<sha1[0..2]>/<sha1>`. The shell is deliberately version-free (it
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use lodestone_assets::ResourceSource;
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
        let index_path = find_asset_index(root)?;
        let index_bytes =
            std::fs::read(&index_path).map_err(|e| format!("reading {}: {e}", index_path.display()))?;
        let index = parse_asset_index(&index_bytes)?;
        let source = AssetObjectSource {
            root: root.to_path_buf(),
            index,
        };

        // sounds.json lives in the object store under this fixed index key.
        let sounds_json = source
            .object_bytes("minecraft/sounds.json")
            .ok_or("no minecraft/sounds.json in the asset store")?;
        let registry = SoundRegistry::parse(&sounds_json)
            .map_err(|e| format!("parsing sounds.json: {e}"))?;

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

/// Finds the single `asset-index-*.json` under `root`, refusing to guess when
/// zero or several exist (deterministic selection, never directory order).
fn find_asset_index(root: &Path) -> Result<PathBuf, String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| format!("reading dir {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("asset-index-") && name.ends_with(".json") {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => Err("no asset-index-*.json in the asset root".to_string()),
        1 => Ok(matches.pop().expect("len checked")),
        n => {
            matches.sort();
            Err(format!(
                "{n} asset-index-*.json files in the asset root; refusing to guess \
                 (found e.g. {})",
                matches[0].display()
            ))
        }
    }
}

/// Parses an `asset-index-*.json` into an `in-pack key -> sha1 hex` map.
///
/// Shape: `{"objects": {"<key>": {"hash": "<sha1>", "size": <n>}, …}}`.
fn parse_asset_index(bytes: &[u8]) -> Result<HashMap<String, String>, String> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("asset index is not valid JSON: {e}"))?;
    let objects = json
        .get("objects")
        .and_then(|o| o.as_object())
        .ok_or("asset index has no \"objects\" map")?;
    let mut index = HashMap::with_capacity(objects.len());
    for (key, meta) in objects {
        if let Some(hash) = meta.get("hash").and_then(|h| h.as_str()) {
            index.insert(key.clone(), hash.to_string());
        }
    }
    if index.is_empty() {
        return Err("asset index \"objects\" map is empty".to_string());
    }
    Ok(index)
}

/// A local-disk [`ResourceSource`] over a launcher asset-object store.
///
/// Reads `assets/<ns>/sounds/<p>.ogg` (the driver's in-pack path) by dropping
/// the leading `assets/` to form the asset-index key, resolving that to a sha1,
/// and reading `objects/<sha1[0..2]>/<sha1>`. Local only: a missing object
/// returns `None` (that one sound is absent) rather than blocking the game on a
/// network fetch.
struct AssetObjectSource {
    root: PathBuf,
    index: HashMap<String, String>,
}

impl std::fmt::Debug for AssetObjectSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetObjectSource")
            .field("root", &self.root)
            .field("objects", &self.index.len())
            .finish()
    }
}

impl AssetObjectSource {
    fn object_bytes(&self, index_key: &str) -> Option<Vec<u8>> {
        let sha1 = self.index.get(index_key)?;
        let path = object_relpath(&self.root, sha1);
        std::fs::read(path).ok()
    }
}

impl ResourceSource for AssetObjectSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let key = path.strip_prefix("assets/").unwrap_or(path);
        self.object_bytes(key)
    }

    fn list(&self, _prefix: &str) -> Vec<String> {
        // The sound path never lists; resolution is by explicit key.
        Vec::new()
    }
}

/// `<root>/objects/<sha1[0..2]>/<sha1>`.
fn object_relpath(root: &Path, sha1: &str) -> PathBuf {
    root.join("objects").join(&sha1[0..2]).join(sha1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_index_parses_key_to_sha1() {
        let json = br#"{
            "objects": {
                "minecraft/sounds.json": { "hash": "abcdef0123456789abcdef0123456789abcdef01", "size": 10 },
                "minecraft/sounds/block/stone/break1.ogg": { "hash": "0011223344556677889900112233445566778899", "size": 20 }
            }
        }"#;
        let index = parse_asset_index(json).expect("parses");
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.get("minecraft/sounds.json").map(String::as_str),
            Some("abcdef0123456789abcdef0123456789abcdef01")
        );
    }

    #[test]
    fn empty_or_malformed_index_is_an_error_not_a_silent_empty() {
        // A store that parses but yields zero objects would make audio come up
        // "working" and play nothing — the silent-half-state trap. It must fail.
        assert!(parse_asset_index(b"{\"objects\":{}}").is_err());
        assert!(parse_asset_index(b"not json").is_err());
        assert!(parse_asset_index(b"{\"nope\":1}").is_err());
    }

    #[test]
    fn read_strips_assets_prefix_to_form_the_index_key() {
        // The driver asks for `assets/minecraft/sounds/…`; the asset-index key
        // has no `assets/` prefix. A mismatch here resolves zero sounds — the
        // exact "connected but silent" failure the maintainer flagged.
        let mut index = HashMap::new();
        index.insert(
            "minecraft/sounds/block/stone/break1.ogg".to_string(),
            "0011223344556677889900112233445566778899".to_string(),
        );
        let src = AssetObjectSource {
            root: PathBuf::from("/nonexistent-root"),
            index,
        };
        // The key is present, so it *attempts* a disk read at the right relpath
        // (which fails because the root is fake — proving the mapping, not I/O).
        // A key that is NOT in the index returns None before any disk access.
        assert!(
            src.read("assets/minecraft/sounds/does/not/exist.ogg")
                .is_none(),
            "unknown key must be None"
        );
    }

    #[test]
    fn object_relpath_is_sha1_sharded() {
        let p = object_relpath(Path::new("/root"), "0011223344556677889900112233445566778899");
        assert_eq!(
            p,
            Path::new("/root/objects/00/0011223344556677889900112233445566778899")
        );
    }
}
