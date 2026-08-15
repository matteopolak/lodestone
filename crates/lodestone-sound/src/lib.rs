//! `lodestone-sound`: the bridge from version-free game events to the
//! device-free audio engine.
//!
//! # Why this crate exists
//!
//! [`lodestone-audio`](lodestone_audio) is a deliberately standalone core: it
//! knows how to decode Ogg Vorbis, mix voices, spatialise them and select a
//! weighted variant from a seed, but it has **never seen a sound name, a
//! protocol number, or a resource pack**. That confinement is what let it port
//! to the browser for free and unit-test with no audio hardware.
//!
//! Something still has to connect a [`ClientEvent::Sound`] arriving from the
//! network to a decoded `.ogg` playing on a bus. That glue needs three things
//! at once — the model's event carriers, the assets crate's `sounds.json`
//! resolver, and the audio engine — so it lives *here*, in its own crate,
//! rather than forcing `lodestone-audio` to grow an assets dependency or
//! `lodestone-client` to grow an audio one.
//!
//! # What it does, matched to vanilla
//!
//! Given a [`ClientEvent::Sound`] the [`SoundDriver`]:
//!
//! 1. Seeds a single [`JavaRandom`](lodestone_audio::JavaRandom) with the
//!    packet's `seed` (the server-rolled value — client-side selection would
//!    desync every client), and asks the [`SoundRegistry`] to resolve the event
//!    name to a concrete `.ogg` via its weighted selection. Because the roll
//!    closure is one RNG shared across the whole `type: event` chain, this
//!    reproduces vanilla's `WeighedSoundEvents.getSound(RandomSource)` exactly
//!    for the (constant-volume/pitch) vanilla corpus.
//! 2. Reads the resolved file's bytes from an injected [`ResourceSource`] and
//!    decodes it (cached, so a repeated footstep decodes once).
//! 3. Builds a [`SoundInstance`] whose parameters match
//!    `SoundEngine`/`AbstractSoundInstance`:
//!    * `volume = packet_volume * entry_volume` (they multiply)
//!    * `pitch  = packet_pitch  * entry_pitch`
//!    * audible range `= max(volume, 1) * entry.attenuation_distance` — the
//!      audio crate's `Spatialization::range` already computes this.
//!
//! ## An honest correction, verified against decompiled 26.2
//!
//! The packet's `fixed_range` does **not** drive client-side attenuation. Zero
//! `SoundEvent.getRange` call sites exist in the client; `SoundEngine.java`
//! computes range purely from the `sounds.json` entry's `attenuation_distance`
//! (`max(instanceVolume, 1) * sound.getAttenuationDistance()`). `fixed_range`
//! is a *server-side* culling parameter (which players receive the packet at
//! all). The driver therefore ignores `fixed_range` for attenuation and
//! documents the field as carried-but-unused on the client audio path. See
//! `SoundEngine.play` and `AbstractSoundInstance.getVolume`.

mod driver;

pub use driver::{DriverError, SoundDriver, SoundResolver, StreamingSound, map_category};

/// Situational music: *when* to play a music track and *which* one.
///
/// The driver above happily plays a music event if asked; nothing ever asked.
/// This module is the thing that decides, transcribed from `MusicManager` /
/// `Musics` / `BackgroundMusic` and — like the driver — device-free and
/// clock-free, so ticking it cannot make a sound.
pub mod music;

/// The generated biome-id → [`music::BackgroundMusic`] table, derived from the
/// biome assets already in this repository.
pub mod biome_music;

/// Ambient sound loops: the biome/dimension loop with its 40-tick crossfade, the
/// darkness-driven "mood" one-shot (cave ambience), and the flat-probability
/// "additions". Device-free and clock-free, like [`music`].
pub mod ambient;

/// The generated biome-id → [`ambient::AmbientSounds`] table, plus the
/// per-*dimension* layer that actually carries cave ambience.
pub mod biome_ambient;

/// Client-predicted local sounds: which sounds vanilla plays without waiting for the
/// server, the step-distance cadence that triggers footsteps, and the echo ledger
/// that guards against double-play.
pub mod predict;

/// The native, device-backed engine ([`AudioEngine`]) and the audio types a
/// consumer needs to drive it, re-exported so a caller depends on this one crate
/// rather than reaching into [`lodestone-audio`](lodestone_audio) directly.
///
/// Native-only by `cfg`, mirroring the `cpal` sink it wraps: the browser has no
/// `AudioEngine` (its `AudioWorklet` calls the mixer directly), so this whole
/// surface is structurally absent from the wasm build.
#[cfg(not(target_arch = "wasm32"))]
mod engine;

#[cfg(not(target_arch = "wasm32"))]
pub use engine::AudioEngine;

/// The device-open failure type. Native-only, like the [`AudioEngine`] that
/// produces it — there is no device to fail to open in a browser.
#[cfg(not(target_arch = "wasm32"))]
pub use lodestone_audio::AudioError;

/// Identifies a live voice, for ramping or stopping it.
///
/// **Deliberately *not* `cfg`-gated, unlike [`AudioEngine`] and `AudioError`
/// above.** It is a device-free identifier — an index handed back by the mixer,
/// which `lodestone-audio` compiles for wasm32 unchanged — and the browser arm
/// (`AudioWorklet` driving that same mixer) needs to name it for exactly the
/// reasons the native engine does. Gating it forced `lodestone-shell`'s browser
/// build to reach past this crate into `lodestone-audio` for one type, which is
/// the coupling this module's re-exports exist to prevent.
pub use lodestone_audio::PlayHandle;

/// The RNG [`music::MusicManager::tick`] and
/// [`ambient::AmbientAdditionsSettings::fires`] draw from.
///
/// Re-exported because those are public APIs that take it **by `&mut`**, so a
/// caller outside this crate could not name the type and therefore could not call
/// them at all — an API gap rather than a convenience. Note it is not
/// `#[cfg]`-gated like the block above: the tick/selection half of this crate is
/// deliberately device-free and works on wasm.
///
/// Not to be confused with `lodestone_particle::rng::JavaRandom`, a *different*
/// type with the same name — mixing them up desynchronises a sequence that must
/// match the jar draw for draw.
pub use lodestone_audio::JavaRandom;
