//! The output-device seam: an [`AudioSink`] drives a shared [`Mixer`].
//!
//! Following the `ResourceSource`/`Transport` precedent, the device is the only
//! target-specific part of the engine and it is a *thin* consumer of the pure
//! core:
//!
//! * **Native** — [`CpalSink`] (`cfg(not(target_arch = "wasm32"))`) opens the
//!   default output device with `cpal` and, on its realtime callback, locks the
//!   shared [`Mixer`] and calls [`Mixer::render`] to fill each block. `cpal` is a
//!   `[target.'cfg(not(...))'.dependencies]` entry, so it is *structurally*
//!   absent from the wasm build — feature unification cannot drag it in.
//!
//! * **Browser** — there is no sink type here at all. The WebAudio graph in the
//!   `web/` crate registers an `AudioWorklet` whose `process` callback owns the
//!   `Mixer` and calls [`Mixer::render`] directly. That worklet *is* the sink;
//!   the seam it plugs into is [`Mixer::render`], the same method `cpal` calls.
//!   Keeping the browser device out of this crate is what lets `lodestone-audio`
//!   compile to `wasm32` with no web-sys/wasm-bindgen dependency of its own.

use crate::error::AudioError;

/// A started audio output that is being fed by a [`Mixer`].
///
/// This is the control surface for a running device (query its rate, pause and
/// resume it). The actual sample flow is pull-based: the device asks the mixer
/// to render blocks. Deliberately object-safe so a caller can hold a
/// `Box<dyn AudioSink>` without caring which backend it is.
pub trait AudioSink {
    /// The device output sample rate in Hz. A [`Mixer`] feeding this sink must
    /// be constructed with the same rate.
    fn sample_rate(&self) -> u32;

    /// Resumes (or starts) playback.
    fn resume(&self) -> Result<(), AudioError>;

    /// Pauses playback without tearing the device down.
    fn pause(&self) -> Result<(), AudioError>;
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::CpalSink;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::{Arc, Mutex};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SizedSample};

    use super::AudioSink;
    use crate::error::AudioError;
    use crate::mixer::{Mixer, OUTPUT_CHANNELS};

    /// A native audio sink backed by `cpal` (CoreAudio / WASAPI / ALSA…).
    ///
    /// It owns the running stream and a shared handle to the [`Mixer`] its
    /// realtime callback renders from. The callback takes the mixer lock once per
    /// block; the lock is held only for the render call, never across an await or
    /// allocation (the scratch buffer is reused).
    pub struct CpalSink {
        stream: cpal::Stream,
        sample_rate: u32,
        mixer: Arc<Mutex<Mixer>>,
    }

    // `cpal::Stream` is not `Send`/`Sync` on every platform; a sink is owned and
    // driven from one thread, which is all we require.
    impl std::fmt::Debug for CpalSink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CpalSink")
                .field("sample_rate", &self.sample_rate)
                .finish_non_exhaustive()
        }
    }

    impl CpalSink {
        /// Opens the default output device and returns the sink together with the
        /// shared [`Mixer`] it drives (constructed at the device's sample rate).
        ///
        /// The stream is created **paused** on platforms where that is the
        /// default; call [`AudioSink::resume`] to begin playback.
        pub fn new() -> Result<(Self, Arc<Mutex<Mixer>>), AudioError> {
            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or_else(|| AudioError::Device("no default output device".into()))?;
            let supported = device
                .default_output_config()
                .map_err(|e| AudioError::Device(format!("default output config: {e}")))?;

            let sample_rate = supported.sample_rate();
            let channels = supported.channels() as usize;
            let sample_format = supported.sample_format();
            let config: cpal::StreamConfig = supported.into();

            let mixer = Arc::new(Mutex::new(Mixer::new(sample_rate)));

            let stream = match sample_format {
                cpal::SampleFormat::F32 => {
                    Self::build::<f32>(&device, &config, channels, Arc::clone(&mixer))
                }
                cpal::SampleFormat::I16 => {
                    Self::build::<i16>(&device, &config, channels, Arc::clone(&mixer))
                }
                cpal::SampleFormat::U16 => {
                    Self::build::<u16>(&device, &config, channels, Arc::clone(&mixer))
                }
                other => Err(AudioError::Device(format!(
                    "unsupported sample format: {other:?}"
                ))),
            }?;

            Ok((
                Self {
                    stream,
                    sample_rate,
                    mixer: Arc::clone(&mixer),
                },
                mixer,
            ))
        }

        /// The shared mixer handle (also returned from [`new`](Self::new)).
        pub fn mixer(&self) -> Arc<Mutex<Mixer>> {
            Arc::clone(&self.mixer)
        }

        fn build<T>(
            device: &cpal::Device,
            config: &cpal::StreamConfig,
            channels: usize,
            mixer: Arc<Mutex<Mixer>>,
        ) -> Result<cpal::Stream, AudioError>
        where
            T: SizedSample + FromSample<f32>,
        {
            // Reused across callbacks so the realtime path never allocates.
            let mut scratch: Vec<f32> = Vec::new();
            let data_cb = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let dev_ch = channels.max(1);
                let frames = data.len() / dev_ch;
                let needed = frames * OUTPUT_CHANNELS;
                if scratch.len() < needed {
                    scratch.resize(needed, 0.0);
                }
                {
                    let mut m = mixer.lock().unwrap_or_else(|p| p.into_inner());
                    m.render(&mut scratch[..needed]);
                }
                // Map the stereo mix onto the device's channel layout, clamping
                // to the representable range (the mixer applies no limiter).
                for f in 0..frames {
                    let l = scratch[f * OUTPUT_CHANNELS].clamp(-1.0, 1.0);
                    let r = scratch[f * OUTPUT_CHANNELS + 1].clamp(-1.0, 1.0);
                    for ch in 0..dev_ch {
                        let v = match ch {
                            0 => l,
                            1 => r,
                            _ => 0.0,
                        };
                        data[f * dev_ch + ch] = T::from_sample(v);
                    }
                }
            };
            let err_cb = |e| {
                // A device-side error (e.g. hot-unplug) is logged via the error
                // channel; there is no caller thread to propagate to here.
                eprintln!("lodestone-audio: cpal stream error: {e}");
            };
            device
                .build_output_stream::<T, _, _>(*config, data_cb, err_cb, None)
                .map_err(|e| AudioError::Device(format!("build output stream: {e}")))
        }
    }

    impl AudioSink for CpalSink {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn resume(&self) -> Result<(), AudioError> {
            self.stream
                .play()
                .map_err(|e| AudioError::Device(format!("play: {e}")))
        }

        fn pause(&self) -> Result<(), AudioError> {
            self.stream
                .pause()
                .map_err(|e| AudioError::Device(format!("pause: {e}")))
        }
    }
}
