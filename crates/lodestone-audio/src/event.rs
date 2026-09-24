//! The version-free event seam: what a caller hands the mixer to play a sound.
//!
//! # This is the seam, and it is deliberately empty of version knowledge
//!
//! Game *events* — a `sound_effect` packet, an `entity_sound_effect` packet, or a
//! client-side prediction like a footstep or block-break — are turned into
//! sounds elsewhere. That translation is version-specific (sound *names*,
//! category *ids*, and packet layouts all drift across protocol 47…776) and so
//! lives in the version crates and the client adapter, exactly like
//! `PhysicsProfile`. This crate never sees a protocol number or a sound name.
//!
//! What crosses the seam into the mixer is a fully-resolved, version-neutral
//! [`SoundInstance`]: decoded PCM plus the playback parameters. Everything in it
//! is a plain number or an audio-domain enum.
//!
//! ## The adapter hook the client must provide
//!
//! To wire this up, the client-side adapter is expected to:
//!
//! 1. Resolve the event to a concrete `.ogg` and playback parameters. The asset
//!    layer already does this (`SoundRegistry::resolve` in `lodestone-assets`
//!    yields a `ResolvedSound { file, volume, pitch, stream, attenuation_distance }`).
//! 2. Read the `.ogg` bytes via a `ResourceSource` and decode them with
//!    [`crate::decode_vorbis`] (cache the resulting [`crate::PcmBuffer`] in an
//!    `Arc` — sounds repeat).
//! 3. Map the wire/category id to a [`crate::SoundCategory`] (version data) and
//!    the entity/world position to a [`glam::Vec3`].
//! 4. Build a [`SoundInstance`] and call `Mixer::play`.
//!
//! The one cross-crate request this implies (relayed to the maintainer): the
//! client needs a place to call the adapter for both packet-driven and
//! prediction-driven sounds — a single `play_sound(SoundInstance)` entry point on
//! the client audio subsystem that both the net packet handlers and the
//! prediction code can reach.

use std::sync::Arc;

use crate::category::SoundCategory;
use crate::decode::PcmBuffer;
use crate::spatial::{Attenuation, Spatialization};

pub use crate::voice::PlayHandle;

/// A fully-resolved, version-neutral request to play one sound.
///
/// Construct it from the asset layer's resolved sound plus a decoded
/// [`PcmBuffer`]; it carries no notion of *which* Minecraft version or event
/// produced it.
#[derive(Debug, Clone)]
pub struct SoundInstance {
    /// Decoded PCM, shared so the same sound can play many times without
    /// re-decoding.
    pub pcm: Arc<PcmBuffer>,
    /// The mixer bus this sound belongs to.
    pub category: SoundCategory,
    /// Per-instance volume (the resolved sound's volume). Affects both gain
    /// (clamped to 1) and audible range (`max(volume, 1)` scaling).
    pub volume: f32,
    /// Per-instance pitch / playback-rate multiplier (clamped to `[0.5, 2.0]`
    /// when the voice starts).
    pub pitch: f32,
    /// World-space position (ignored when `relative`).
    pub position: glam::Vec3,
    /// Attenuation mode.
    pub attenuation: Attenuation,
    /// Raw attenuation distance in blocks (vanilla default 16).
    pub attenuation_distance: f32,
    /// Head-relative (UI/music): no attenuation, no panning.
    pub relative: bool,
    /// Whether the sound loops.
    pub looping: bool,
}

impl SoundInstance {
    /// A minimal positional sound: linear attenuation at the vanilla default
    /// distance (16), full volume and pitch, non-looping.
    pub fn positional(pcm: Arc<PcmBuffer>, category: SoundCategory, position: glam::Vec3) -> Self {
        Self {
            pcm,
            category,
            volume: 1.0,
            pitch: 1.0,
            position,
            attenuation: Attenuation::Linear,
            attenuation_distance: 16.0,
            relative: false,
            looping: false,
        }
    }

    /// A head-relative sound (UI click, music): no attenuation or panning.
    pub fn relative(pcm: Arc<PcmBuffer>, category: SoundCategory) -> Self {
        Self {
            pcm,
            category,
            volume: 1.0,
            pitch: 1.0,
            position: glam::Vec3::ZERO,
            attenuation: Attenuation::None,
            attenuation_distance: 16.0,
            relative: true,
            looping: false,
        }
    }

    /// The [`Spatialization`] implied by this instance.
    pub(crate) fn spatialization(&self) -> Spatialization {
        Spatialization {
            position: self.position,
            attenuation: self.attenuation,
            attenuation_distance: self.attenuation_distance,
            instance_volume: self.volume,
            relative: self.relative,
        }
    }
}
