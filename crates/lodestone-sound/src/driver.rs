//! The event → sound driver. Pure and device-free: it owns a [`Mixer`] and a
//! [`SoundRegistry`], resolves events against an injected [`ResourceSource`],
//! and never touches an audio device, the clock, the filesystem, or the
//! network. The device sink and the event source are both the caller's job.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use lodestone_assets::sound::SoundRegistry;
use lodestone_assets::{ResourceSource, SoundError as ResolveError};
use lodestone_audio::{
    AudioError, JavaRandom, Mixer, PcmBuffer, PlayHandle, SoundCategory as AudioCategory,
    SoundInstance, VorbisStream, decode_vorbis,
};
use lodestone_model::event::SoundCategory as ModelCategory;

/// A resolved but **undecoded** long sound (a music track or a jukebox record),
/// produced by [`SoundResolver::resolve_streaming`].
///
/// Holds the compressed bitstream plus the playback parameters the eager path
/// would have applied, so a music sink can feed the mixer incrementally instead of
/// materialising a few hundred megabytes of PCM.
#[derive(Debug)]
pub struct StreamingSound {
    /// The lazily decoding bitstream. Compressed bytes are resident (~11 MiB for
    /// the largest track); decoded PCM is not.
    pub stream: VorbisStream,
    /// The in-pack path the event resolved to, for logging.
    pub path: String,
    /// The `sounds.json` entry's volume, to multiply into the instance volume.
    pub volume: f32,
    /// The `sounds.json` entry's pitch.
    pub pitch: f32,
    /// The entry's attenuation distance. Music is head-relative, so this is
    /// carried for completeness rather than used.
    pub attenuation_distance: f32,
    /// Whether the entry declared `"stream": true`. Every music entry in real 26.2
    /// data does; a `false` here means the caller is streaming something vanilla
    /// would have decoded eagerly, which is wasteful but not wrong.
    pub declares_stream: bool,
}

/// Why a sound failed to play.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// The `sounds.json` resolver rejected the event (e.g. a reference cycle in
    /// a malicious pack — vanilla ships no cycle guard, the assets crate does).
    #[error("resolving sound event: {0}")]
    Resolve(#[from] ResolveError),
    /// The resolver chose a file, but the injected source has no bytes for it.
    /// This is a real error (a broken pack or a mis-wired source), not the
    /// "empty sound" case — that resolves to `Ok(None)`.
    #[error("resolved sound file not present in source: {0}")]
    MissingFile(String),
    /// The bytes were present but did not decode as Ogg Vorbis.
    #[error("decoding sound file: {0}")]
    Decode(#[from] AudioError),
}

/// The device-free resolve + decode core, shared by the headless
/// [`SoundDriver`] and the device-backed [`AudioEngine`](crate::AudioEngine).
///
/// It owns the parsed `sounds.json`, the injected byte source, and a
/// decoded-PCM cache, and turns an event into a ready-to-play
/// [`SoundInstance`] — but it holds **no** mixer, device, clock, filesystem, or
/// network. That is what lets the exact same resolution/decode path feed either
/// an owned [`Mixer`] (tests) or a real output device's shared mixer (the game)
/// with one implementation.
#[derive(Debug)]
pub struct SoundResolver {
    registry: SoundRegistry,
    source: Box<dyn ResourceSource>,
    /// Decoded-PCM cache keyed by in-pack file path, so a repeated sound (a
    /// footstep, a block break) decodes exactly once. `Arc` is shared into
    /// every [`SoundInstance`], so playing does not clone the samples.
    cache: HashMap<String, Arc<PcmBuffer>>,
}

impl SoundResolver {
    /// Builds a resolver over a parsed registry and a byte source.
    pub fn new(registry: SoundRegistry, source: Box<dyn ResourceSource>) -> Self {
        Self {
            registry,
            source,
            cache: HashMap::new(),
        }
    }

    /// Number of `.ogg` files decoded and cached so far.
    pub fn decoded_file_count(&self) -> usize {
        self.cache.len()
    }

    /// The event's `subtitles` translation key, for vanilla's sound-subtitle
    /// captions. `None` for an unknown event or one that declares no
    /// subtitle — vanilla shows no caption in either case.
    ///
    /// Reads the event *before* weighted selection, deliberately: the subtitle is
    /// a property of the event, not of the chosen entry, so this must not go
    /// through [`resolve`](SoundRegistry::resolve) and consume an RNG roll.
    pub fn subtitle(&self, event_name: &str) -> Option<&str> {
        self.registry.event(event_name)?.subtitle.as_deref()
    }

    /// Resolves an event to a ready-to-play [`SoundInstance`] without touching
    /// any mixer. Decoding happens here, so callers holding a realtime mixer
    /// lock must call this *outside* the lock. Returns `Ok(None)` for vanilla's
    /// silent "empty sound" (unknown event or zero total weight).
    ///
    /// Applies vanilla's observable rules:
    /// * `instanceVolume = packetVolume * entryVolume`,
    /// * `instancePitch  = packetPitch  * entryPitch`
    ///   (`AbstractSoundInstance.getVolume/getPitch`);
    /// * client attenuation uses the `sounds.json` entry distance, **not** the
    ///   packet's `fixed_range` (that is server-side culling only);
    ///   [`Spatialization::range`](lodestone_audio) then applies the
    ///   `max(volume, 1)` scaling, matching `SoundEngine`.
    pub fn resolve_instance(
        &mut self,
        event_name: &str,
        category: ModelCategory,
        position: Vec3,
        packet_volume: f32,
        packet_pitch: f32,
        seed: i64,
    ) -> Result<Option<SoundInstance>, DriverError> {
        // One RNG for the whole resolution chain, seeded with the server's
        // value. `resolve` calls the closure once per weighted level (including
        // `type: event` recursion), exactly as vanilla's single `RandomSource`
        // is threaded through `WeighedSoundEvents.getSound`.
        let mut rng = JavaRandom::new(seed);
        let resolved = self
            .registry
            .resolve(event_name, &mut |bound| rng.roll(bound))?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };

        let path = resolved.file_path();
        let pcm = self.decode_cached(&path)?;

        let mut instance = SoundInstance::positional(pcm, map_category(category), position);
        instance.volume = packet_volume * resolved.volume;
        instance.pitch = packet_pitch * resolved.pitch;
        instance.attenuation_distance = resolved.attenuation_distance as f32;
        Ok(Some(instance))
    }

    /// Resolves a **music-length** event without decoding it, yielding a lazily
    /// decoding [`VorbisStream`] plus the same volume/pitch/attenuation the eager
    /// path would produce. Returns `Ok(None)` for vanilla's silent empty sound, and
    /// `Ok(None)` — *not* an error — when the resolved file's bytes are absent from
    /// the source.
    ///
    /// # Why music must not go through [`SoundResolver::resolve_instance`]
    ///
    /// That method calls [`decode_vorbis`] and **caches the result forever**, which
    /// is right for a footstep and catastrophic for a track. Measured in
    /// `lodestone-audio`'s `stream` module: `music/game/end/the_end.ogg` is
    /// 10.76 MiB compressed and **304.33 MiB resident** decoded, a 28.3x expansion,
    /// and the eight largest music/record objects are each 130–300 MiB. The world
    /// layer's whole measured budget is 77.6 MiB. Caching two tracks would exceed
    /// the entire client's memory footprint.
    ///
    /// This is not a hypothetical distinction that might be safe to ignore:
    /// **every** music entry in the real 26.2 `sounds.json` carries
    /// `"stream": true` (checked by `tests/music_assets.rs` against the on-disk
    /// file), which is vanilla saying exactly this. `ResolvedSound::stream` has
    /// been parsed by `lodestone-assets` all along and ignored here; this is the
    /// method that honours it.
    ///
    /// # The missing-file asymmetry, and why it is deliberate
    ///
    /// [`SoundResolver::resolve_instance`] reports [`DriverError::MissingFile`] for
    /// absent bytes, because for an ordinary event that means a broken pack or a
    /// mis-wired source. For music it means neither: `cargo xtask fetch-sounds`
    /// **excludes music by default** (70 tracks + 22 records, 293 MB, only with
    /// `--all`), so absent bytes are the *normal* state of a checkout. Reporting an
    /// error would make every music tick log a failure. `Ok(None)` here is
    /// therefore the honest answer, and it is what lets
    /// [`MusicStart::Silent`](crate::music::MusicStart::Silent) be an ordinary
    /// outcome rather than an error path.
    pub fn resolve_streaming(
        &mut self,
        event_name: &str,
        seed: i64,
    ) -> Result<Option<StreamingSound>, DriverError> {
        let mut rng = JavaRandom::new(seed);
        let resolved = self
            .registry
            .resolve(event_name, &mut |bound| rng.roll(bound))?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };

        let path = resolved.file_path();
        // Absent bytes are silence, not an error — see above.
        let Some(bytes) = self.source.read(&path) else {
            return Ok(None);
        };
        let stream = VorbisStream::new(bytes)?;
        Ok(Some(StreamingSound {
            stream,
            path,
            volume: resolved.volume,
            pitch: resolved.pitch,
            attenuation_distance: resolved.attenuation_distance as f32,
            declares_stream: resolved.stream,
        }))
    }

    fn decode_cached(&mut self, path: &str) -> Result<Arc<PcmBuffer>, DriverError> {
        if let Some(pcm) = self.cache.get(path) {
            return Ok(pcm.clone());
        }
        let bytes = self
            .source
            .read(path)
            .ok_or_else(|| DriverError::MissingFile(path.to_string()))?;
        let pcm = Arc::new(decode_vorbis(&bytes)?);
        self.cache.insert(path.to_string(), pcm.clone());
        Ok(pcm)
    }
}

/// Bridges [`ClientEvent`](lodestone_model::ClientEvent) sound events to a
/// [`Mixer`]. Construct with a parsed [`SoundRegistry`] and a byte source that
/// can read `assets/<ns>/sounds/<path>.ogg`, then feed it events.
///
/// This is the headless, device-free driver: it owns its mixer and the caller
/// renders it (tests, the browser worklet). For a native output device, see
/// [`AudioEngine`](crate::AudioEngine), which shares its mixer with a `cpal`
/// stream but uses the same [`SoundResolver`].
#[derive(Debug)]
pub struct SoundDriver {
    resolver: SoundResolver,
    mixer: Mixer,
}

impl SoundDriver {
    /// Builds a driver rendering at `output_sample_rate` Hz.
    ///
    /// `registry` is the stacked `sounds.json` (pack overrides already merged by
    /// the assets layer). `source` reads `.ogg` bytes by in-pack path; on native
    /// it is typically backed by the asset-object store, in the browser by an
    /// in-memory source — the driver neither knows nor cares which.
    pub fn new(
        output_sample_rate: u32,
        registry: SoundRegistry,
        source: Box<dyn ResourceSource>,
    ) -> Self {
        Self {
            resolver: SoundResolver::new(registry, source),
            mixer: Mixer::new(output_sample_rate),
        }
    }

    /// Read access to the mixer (for `render`, listener/volume inspection).
    pub fn mixer(&self) -> &Mixer {
        &self.mixer
    }

    /// Mutable access to the mixer. The caller renders through this, updates the
    /// [`Listener`](lodestone_audio::Listener), sets per-category volumes, and —
    /// for entity-attached sounds — pushes new positions each frame via
    /// [`Mixer::set_voice_position`].
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Number of `.ogg` files decoded and cached so far. Useful for asserting a
    /// live path actually decoded something rather than mixing zeros.
    pub fn decoded_file_count(&self) -> usize {
        self.resolver.decoded_file_count()
    }

    /// Plays a positioned sound (the `SOUND` packet path).
    ///
    /// `event_name` is the version-free event key's path (e.g.
    /// `"block.stone.break"` — the `sounds.json` key, namespace stripped).
    /// Returns `Ok(None)` when the event resolves to nothing (unknown event or
    /// zero total weight — vanilla's silent "empty sound"), which is *not* an
    /// error. Otherwise returns the playing voice's handle.
    pub fn play_sound(
        &mut self,
        event_name: &str,
        category: ModelCategory,
        position: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> Result<Option<PlayHandle>, DriverError> {
        match self
            .resolver
            .resolve_instance(event_name, category, position, volume, pitch, seed)?
        {
            Some(instance) => Ok(Some(self.mixer.play(instance))),
            None => Ok(None),
        }
    }

    /// Plays an entity-attached sound (the `SOUND_ENTITY` packet path) at the
    /// entity's current position.
    ///
    /// The driver holds only a position *snapshot*; it has no entity store and
    /// must not gain one. To make the sound follow the entity, the caller
    /// re-reads the entity's position each frame and calls
    /// [`Mixer::set_voice_position`] with the returned handle — which returns
    /// `false` once the voice finishes, the signal to stop tracking.
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
}

/// Maps a model [`SoundCategory`](ModelCategory) to the audio engine's bus by
/// ordinal. The two enums share vanilla's `SoundSource` order exactly (the
/// names differ only in pluralisation), so the ordinal is the safe bridge — a
/// name match would be fragile. Both have 11 buses ending in `Ui`.
///
/// `pub`, not `pub(crate)`: the native [`AudioEngine`](crate::AudioEngine) was
/// its only caller until the browser's own mixer-driving `ShellAudio` needed
/// the identical bridge (`lodestone-shell/src/audio.rs`'s wasm32 arm) — reusing
/// this rather than hand-rolling a second ordinal table is what keeps the two
/// targets from being able to drift apart on which bus a category lands on.
pub fn map_category(category: ModelCategory) -> AudioCategory {
    AudioCategory::ALL[category.ordinal() as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::MemorySource;

    /// A real (synthetic, not copyrighted) Ogg Vorbis stream: a stereo log-sweep
    /// that peaks near 0.5. Reused from the audio crate's three-implementation
    /// decode validation. Using a *real* ogg — not silence — is deliberate: two
    /// silent buffers agree perfectly, so a silent fixture would make every
    /// "a sound played" assertion vacuously pass.
    const CHIRP_OGG: &[u8] = include_bytes!("../tests/fixtures/chirp_stereo_44100.ogg");

    /// A minimal `sounds.json` with one event, one file entry, plus a second
    /// event that references the first via `type: event`.
    const SOUNDS_JSON: &str = r#"{
        "block.stone.break": {
            "sounds": [ { "name": "block/stone/break1" } ]
        },
        "ref.event": {
            "sounds": [ { "name": "block.stone.break", "type": "event" } ]
        }
    }"#;

    /// Builds a driver whose event `block.stone.break` resolves to the chirp
    /// ogg, and whose `ref.event` chains to it via `type: event`.
    fn driver() -> SoundDriver {
        let registry = SoundRegistry::parse(SOUNDS_JSON.as_bytes()).expect("parse sounds.json");
        let mut source = MemorySource::new("test");
        source.insert(
            "assets/minecraft/sounds/block/stone/break1.ogg",
            CHIRP_OGG.to_vec(),
        );
        SoundDriver::new(48_000, registry, Box::new(source))
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0_f32, |m, &s| m.max(s.abs()))
    }

    #[test]
    fn a_resolved_event_decodes_and_mixes_non_silent_audio() {
        // The whole chain: event name -> weighted select -> read ogg -> decode
        // -> play -> render. The load-bearing guard is peak > 0.3: a path that
        // resolved nothing, failed to decode, or mixed zeros would leave the
        // buffer silent and fail here. Plus decoded_file_count proves real
        // decode work happened rather than the buffer being coincidentally
        // non-zero.
        let mut d = driver();
        assert_eq!(d.decoded_file_count(), 0);

        let handle = d
            .play_sound(
                "block.stone.break",
                ModelCategory::Block,
                Vec3::ZERO, // at the listener -> full, un-attenuated
                1.0,
                1.0,
                42,
            )
            .expect("play must not error")
            .expect("event resolves to a file");
        assert!(handle.0 > 0);
        assert_eq!(d.decoded_file_count(), 1, "exactly one ogg decoded");

        let mut out = vec![0.0f32; 2048];
        d.mixer_mut().render(&mut out);
        let p = peak(&out);
        assert!(p > 0.3, "mixed output too quiet ({p}) — silent/degenerate?");
    }

    #[test]
    fn a_repeated_sound_decodes_only_once() {
        // The decode cache: a footstep spammed 5x must decode a single time.
        let mut d = driver();
        for _ in 0..5 {
            d.play_sound(
                "block.stone.break",
                ModelCategory::Block,
                Vec3::ZERO,
                1.0,
                1.0,
                7,
            )
            .unwrap()
            .unwrap();
        }
        assert_eq!(d.decoded_file_count(), 1);
        assert_eq!(d.mixer().voice_count(), 5, "all five voices are live");
    }

    #[test]
    fn an_unknown_event_is_silent_not_an_error() {
        // Vanilla's "empty sound": an unresolved event plays nothing. This must
        // be Ok(None), distinct from a decode/missing-file error, and must NOT
        // spawn a voice.
        let mut d = driver();
        let r = d
            .play_sound(
                "no.such.event",
                ModelCategory::Master,
                Vec3::ZERO,
                1.0,
                1.0,
                1,
            )
            .expect("unknown event is not an error");
        assert!(r.is_none());
        assert_eq!(d.mixer().voice_count(), 0);
    }

    #[test]
    fn a_type_event_reference_resolves_through_to_the_file() {
        // The `type: event` case: `ref.event` contributes `block.stone.break`'s
        // total weight and resolves through to its file. A reimplementation that
        // treated the reference as a literal filename would fail to decode.
        let mut d = driver();
        let r = d
            .play_sound("ref.event", ModelCategory::Block, Vec3::ZERO, 1.0, 1.0, 99)
            .expect("chained event must play")
            .expect("chain resolves to a file");
        assert!(r.0 > 0);
        assert_eq!(d.decoded_file_count(), 1);
    }

    #[test]
    fn category_maps_to_the_matching_audio_bus_by_ordinal() {
        // The model->audio category bridge is by ordinal, and both enums are
        // vanilla's SoundSource order. Spot-check every bus.
        for m in ModelCategory::ALL {
            let a = map_category(m);
            assert_eq!(
                a as usize,
                m.ordinal() as usize,
                "ordinal must be preserved for {m:?}"
            );
        }
        // And the endpoints by name, to catch an accidental reorder of ALL.
        assert_eq!(map_category(ModelCategory::Master), AudioCategory::Master);
        assert_eq!(map_category(ModelCategory::Ui), AudioCategory::Ui);
    }

    #[test]
    fn packet_and_entry_volume_pitch_multiply() {
        // Vanilla multiplies packet volume/pitch by the sounds.json entry's.
        // With an entry volume of 0.5 and packet volume 0.5, the instance volume
        // is 0.25; render at the listener and confirm the mixed peak reflects
        // the product, not either factor alone. The chirp peaks ~0.5, so
        // 0.5 * 0.5 * 0.5(chirp) = ~0.125.
        let json = r#"{ "e": { "sounds": [ { "name": "block/stone/break1", "volume": 0.5 } ] } }"#;
        let registry = SoundRegistry::parse(json.as_bytes()).unwrap();
        let mut source = MemorySource::new("t");
        source.insert(
            "assets/minecraft/sounds/block/stone/break1.ogg",
            CHIRP_OGG.to_vec(),
        );
        let mut d = SoundDriver::new(48_000, registry, Box::new(source));
        d.play_sound("e", ModelCategory::Master, Vec3::ZERO, 0.5, 1.0, 3)
            .unwrap()
            .unwrap();
        let mut out = vec![0.0f32; 4096];
        d.mixer_mut().render(&mut out);
        let p = peak(&out);
        // Product 0.25 * chirp-peak(~0.5) ≈ 0.125. Bracket it so neither
        // "forgot to multiply" (≈0.25) nor "silent" (0) passes.
        assert!(
            p > 0.05 && p < 0.2,
            "peak {p} not consistent with 0.5*0.5 product"
        );
    }
}
