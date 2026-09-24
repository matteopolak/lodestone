//! Lodestone's audio engine: Ogg Vorbis decode, a device-free mixer, 3-D
//! positional audio, per-category volume, and pitch/volume playback control,
//! plus a version-free seam for turning game events into sounds.
//!
//! # Design: a device-free core with an injected sink
//!
//! Every part of this crate that *computes* audio — decoding, resampling,
//! attenuation, panning, category gain, and mixing — is pure, synchronous, and
//! free of any audio hardware. It runs and unit-tests headlessly, on native and
//! on `wasm32`, with no device present. This mirrors the `ResourceSource` and
//! `Transport` precedents in the asset and net layers: the logic is testable in
//! isolation and the *device* is a thin, injected sink.
//!
//! The output device is the only target-specific piece:
//!
//! * **Native** ([`CpalSink`], `cfg(not(target_arch = "wasm32"))`) drives a real
//!   device via `cpal` and, on its realtime callback thread, pulls interleaved
//!   stereo frames out of a shared [`Mixer`].
//! * **Browser** (`wasm32`) has no linked device at all. The browser's WebAudio
//!   graph — an `AudioWorklet` living in the `web/` crate — is itself the sink:
//!   it calls [`Mixer::render`] to fill each block. Because the boundary is
//!   `cfg(target_arch)` rather than a Cargo feature, `cpal` is *structurally*
//!   absent from the wasm build and cannot be dragged in by feature unification.
//!
//! # Timekeeping is sample-driven, not wall-clock
//!
//! The mixer advances by the exact number of frames it renders, so playback
//! time is a deterministic function of samples emitted. Nothing here calls
//! [`std::time::Instant::now`] (which compiles to wasm and then panics at
//! runtime) or touches `std::fs`. Where a caller needs elapsed real time, it is
//! injected — but the core never needs it.
//!
//! # Vanilla parity
//!
//! Behavioural rules (attenuation model, range scaling, volume/pitch clamping,
//! category gain) are transcribed from the Minecraft 26.2 client and cited at
//! their call sites. Where a rule depends on an OpenAL-implementation detail we
//! cannot reproduce bit-for-bit (stereo panning geometry), it is documented as
//! an explicit approximation rather than a false claim of parity.

mod category;
mod decode;
mod error;
mod event;
mod mixer;
mod resample;
mod ring;
mod select;
mod sink;
mod spatial;
mod stream;
mod stream_voice;
mod voice;

pub use category::{CategoryVolumes, SoundCategory};
pub use decode::{PcmBuffer, decode_vorbis};
pub use error::AudioError;
pub use event::{PlayHandle, SoundInstance};
pub use mixer::{Mixer, OUTPUT_CHANNELS};
pub use resample::resample_linear;
pub use ring::SampleRing;
pub use select::{JavaRandom, select_weighted};
pub use sink::AudioSink;
pub use spatial::{Attenuation, Listener, Spatialization, attenuation_gain, panning_gains};
pub use stream::VorbisStream;
pub use stream_voice::{StreamSource, StreamVoice};
pub use voice::Voice;

#[cfg(not(target_arch = "wasm32"))]
pub use sink::CpalSink;
