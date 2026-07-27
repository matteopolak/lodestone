//! Ogg Vorbis decoding into interleaved `f32` PCM.
//!
//! Decoding is pure computation — bytes in, samples out — with no device and no
//! `std::fs`, so it lives in the device-free core and runs identically on native
//! and `wasm32`. The decoder is [`lewton`], a pure-Rust Vorbis implementation.
//!
//! Vanilla decodes Vorbis with JOrbis to signed 16-bit PCM at the stream's
//! native sample rate and channel count (`JOrbisAudioStream`), keeping mono
//! mono and stereo stereo. We decode to the same native layout but store `f32`
//! in `[-1, 1]` for lossless mixing; the `i16 -> f32` scaling is `/ 32768.0`.

use crate::error::AudioError;

/// Decoded PCM: interleaved `f32` samples in `[-1, 1]`, at the stream's native
/// sample rate and channel count.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmBuffer {
    sample_rate: u32,
    channels: u16,
    /// Interleaved samples: `[frame0_ch0, frame0_ch1, frame1_ch0, …]`.
    samples: Vec<f32>,
}

impl PcmBuffer {
    /// Builds a buffer from interleaved `f32` samples. `samples.len()` must be a
    /// multiple of `channels`; excess trailing samples are truncated to a whole
    /// frame boundary. Returns [`AudioError::Format`] if `channels == 0`.
    pub fn from_interleaved(
        sample_rate: u32,
        channels: u16,
        mut samples: Vec<f32>,
    ) -> Result<Self, AudioError> {
        if channels == 0 {
            return Err(AudioError::Format("zero channels".into()));
        }
        let rem = samples.len() % channels as usize;
        if rem != 0 {
            samples.truncate(samples.len() - rem);
        }
        Ok(Self {
            sample_rate,
            channels,
            samples,
        })
    }

    /// The native sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The channel count (1 = mono, 2 = stereo, …).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Whether the buffer is single-channel (the only layout vanilla
    /// spatialises).
    pub fn is_mono(&self) -> bool {
        self.channels == 1
    }

    /// The number of sample frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// The interleaved sample slice.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Reads `channel` at fractional frame position `pos` with linear
    /// interpolation, using the same clamping rules as [`sample`](Self::sample).
    /// Interpolation stops at the final frame (no wrap); the mixer handles loop
    /// boundaries itself.
    pub fn read_channel_lerp(&self, pos: f64, channel: u16) -> f32 {
        let frames = self.frames();
        if frames == 0 {
            return 0.0;
        }
        if pos <= 0.0 {
            return self.sample(0, channel);
        }
        let i = pos.floor() as usize;
        if i + 1 >= frames {
            return self.sample(frames - 1, channel);
        }
        let t = (pos - i as f64) as f32;
        crate::resample::lerp(self.sample(i, channel), self.sample(i + 1, channel), t)
    }

    /// Playback duration in seconds at the native rate.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames() as f64 / self.sample_rate as f64
    }

    /// The sample for `channel` at frame `frame`, or `0.0` past the end / for an
    /// out-of-range channel. Mono buffers return their single channel for any
    /// requested channel index, so a stereo consumer reads a mono sound as
    /// centred without a separate downmix path.
    pub fn sample(&self, frame: usize, channel: u16) -> f32 {
        if frame >= self.frames() {
            return 0.0;
        }
        let ch = if self.channels == 1 {
            0
        } else if channel < self.channels {
            channel
        } else {
            return 0.0;
        };
        self.samples[frame * self.channels as usize + ch as usize]
    }
}

/// Decodes a complete Ogg Vorbis bitstream into a [`PcmBuffer`].
///
/// The whole stream is decoded eagerly; this is the "static buffer" path for
/// short sounds (the vast majority of vanilla's 4843 sound files). Long tracks
/// (music, records) are decoded incrementally instead — see
/// [`VorbisStream`](crate::stream::VorbisStream) — because decoding the largest
/// vanilla music object eagerly costs 304 MiB resident (measured; see that
/// module).
pub fn decode_vorbis(bytes: &[u8]) -> Result<PcmBuffer, AudioError> {
    use lewton::inside_ogg::OggStreamReader;
    use std::io::Cursor;

    let mut reader = OggStreamReader::new(Cursor::new(bytes))
        .map_err(|e| AudioError::Decode(format!("header: {e}")))?;

    let channels = reader.ident_hdr.audio_channels as u16;
    let sample_rate = reader.ident_hdr.audio_sample_rate;
    if channels == 0 {
        return Err(AudioError::Format("stream declares zero channels".into()));
    }

    let mut samples: Vec<f32> = Vec::new();
    loop {
        match reader.read_dec_packet_itl() {
            Ok(Some(packet)) => {
                samples.extend(packet.into_iter().map(i16_to_f32));
            }
            Ok(None) => break,
            Err(e) => return Err(AudioError::Decode(format!("packet: {e}"))),
        }
    }

    PcmBuffer::from_interleaved(sample_rate, channels, samples)
}

/// Scales a signed 16-bit sample into `[-1, 1)` by dividing by `32768`.
pub(crate) fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_interleaved_rejects_zero_channels() {
        assert!(PcmBuffer::from_interleaved(48_000, 0, vec![0.0]).is_err());
    }

    #[test]
    fn from_interleaved_truncates_partial_frame() {
        // 3 samples, stereo -> one frame kept, dangling sample dropped.
        let b = PcmBuffer::from_interleaved(48_000, 2, vec![0.1, 0.2, 0.3]).unwrap();
        assert_eq!(b.frames(), 1);
        assert_eq!(b.samples(), &[0.1, 0.2]);
    }

    #[test]
    fn mono_sample_reads_same_channel_for_any_index() {
        let b = PcmBuffer::from_interleaved(48_000, 1, vec![0.5, -0.5]).unwrap();
        assert_eq!(b.sample(0, 0), 0.5);
        assert_eq!(
            b.sample(0, 1),
            0.5,
            "mono read on ch1 returns the mono sample"
        );
        assert_eq!(b.sample(1, 0), -0.5);
        assert_eq!(b.sample(2, 0), 0.0, "past end is silence");
    }

    #[test]
    fn stereo_sample_indexing_is_correct() {
        let b = PcmBuffer::from_interleaved(48_000, 2, vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(b.frames(), 2);
        assert_eq!(b.sample(0, 0), 1.0);
        assert_eq!(b.sample(0, 1), 2.0);
        assert_eq!(b.sample(1, 0), 3.0);
        assert_eq!(b.sample(1, 1), 4.0);
        assert_eq!(b.sample(0, 2), 0.0, "out-of-range channel is silence");
    }

    #[test]
    fn duration_is_frames_over_rate() {
        let b = PcmBuffer::from_interleaved(48_000, 2, vec![0.0; 96_000]).unwrap();
        assert_eq!(b.frames(), 48_000);
        assert!((b.duration_seconds() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn i16_scaling_endpoints() {
        assert_eq!(i16_to_f32(0), 0.0);
        assert_eq!(i16_to_f32(-32768), -1.0);
        assert!((i16_to_f32(32767) - 0.999_969).abs() < 1e-5);
    }
}
