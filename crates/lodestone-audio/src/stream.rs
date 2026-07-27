//! Incremental ("streaming") Ogg Vorbis decode for long tracks.
//!
//! # Why this exists — a measured memory decision, not a guess
//!
//! Music and record tracks are minutes long. Decoding one eagerly to `f32` PCM
//! is not tidy — it is expensive. The largest vanilla 26.2 music object,
//! `the_end.ogg`, is **10.76 MiB compressed** but **904.53 s of 44.1 kHz stereo**,
//! which is `39_889_699` frames → `79_779_398` `f32` samples =
//! **304.33 MiB resident** if fully decoded (a `28.3×` expansion). At `f32`
//! stereo 44.1 kHz, decoded PCM costs `~0.336 MiB/s`, so the eight largest
//! music/record objects alone would sit at `130–300 MiB` each. Against the
//! world layer's measured 77.6 MiB budget that is not viable, so long tracks are
//! **streamed**: only the compressed bytes (resident, ~11 MiB) plus a small
//! decoded window are ever held.
//!
//! # What is device-free here, and what is not
//!
//! [`VorbisStream`] is pure computation: compressed bytes in, `f32` packets out,
//! no device, no `std::fs`, no wall clock. It decodes **lazily, one Ogg packet at
//! a time**, so its working set is a single packet (a few thousand samples), not
//! the whole track. It is the producer-side primitive: a native producer thread
//! or a browser worker pulls packets from it and pushes them into a
//! [`crate::ring::SampleRing`]; the realtime `render` callback only ever touches
//! the ring's non-allocating read side. Who *runs* the producer is the
//! platform's job (see the crate report); the decode itself is portable and is
//! unit-tested headlessly against the eager [`decode_vorbis`] path.
//!
//! [`decode_vorbis`]: crate::decode::decode_vorbis

use crate::decode::i16_to_f32;
use crate::error::AudioError;
use lewton::inside_ogg::OggStreamReader;
use std::io::Cursor;

/// A lazily-decoding Ogg Vorbis stream.
///
/// Owns the compressed bitstream (kept resident so decode never needs I/O) and
/// yields interleaved `f32` frames one Ogg packet at a time via
/// [`next_packet`](Self::next_packet). The decoded working set is bounded by a
/// single packet regardless of track length.
///
/// This is the exact same decoder ([`lewton`]) and the exact same `i16 → f32`
/// scaling used by the eager [`decode_vorbis`](crate::decode::decode_vorbis)
/// path, so concatenating every packet a stream yields is *bit-identical* to
/// decoding the whole file eagerly — a property the tests assert against the
/// committed fixture.
pub struct VorbisStream {
    reader: OggStreamReader<Cursor<Vec<u8>>>,
    sample_rate: u32,
    channels: u16,
    finished: bool,
}

impl std::fmt::Debug for VorbisStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `OggStreamReader` is not `Debug`; surface the stream's observable
        // shape instead of its internal decoder state.
        f.debug_struct("VorbisStream")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl VorbisStream {
    /// Opens a stream over an in-memory Ogg Vorbis bitstream, reading only the
    /// identification/setup headers up front. The compressed `bytes` are moved
    /// in and held for the life of the stream.
    ///
    /// Returns [`AudioError::Decode`] if the headers are malformed and
    /// [`AudioError::Format`] if the stream declares zero channels.
    pub fn new(bytes: Vec<u8>) -> Result<Self, AudioError> {
        let reader = OggStreamReader::new(Cursor::new(bytes))
            .map_err(|e| AudioError::Decode(format!("header: {e}")))?;
        let channels = reader.ident_hdr.audio_channels as u16;
        let sample_rate = reader.ident_hdr.audio_sample_rate;
        if channels == 0 {
            return Err(AudioError::Format("stream declares zero channels".into()));
        }
        Ok(Self {
            reader,
            sample_rate,
            channels,
            finished: false,
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

    /// Whether the stream has reached end-of-bitstream. Once `true`,
    /// [`next_packet`](Self::next_packet) always returns `Ok(None)`.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Decodes and returns the next Ogg packet as interleaved `f32` samples in
    /// `[-1, 1)`, or `Ok(None)` at end-of-stream.
    ///
    /// Each call decodes exactly one packet, so the caller controls how far
    /// ahead of playback it decodes — the natural knob for keeping a ring buffer
    /// fed without decoding the whole track. Allocates a `Vec` per packet, which
    /// is fine on a producer thread but **must not** be called from the realtime
    /// `render` callback (that pulls from the ring instead).
    ///
    /// A packet may legitimately be empty (some encoders emit zero-length
    /// packets); an empty `Vec` is returned in that case, distinct from the
    /// `None` that signals end-of-stream.
    pub fn next_packet(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        if self.finished {
            return Ok(None);
        }
        match self.reader.read_dec_packet_itl() {
            Ok(Some(packet)) => Ok(Some(packet.into_iter().map(i16_to_f32).collect())),
            Ok(None) => {
                self.finished = true;
                Ok(None)
            }
            Err(e) => Err(AudioError::Decode(format!("packet: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_vorbis;

    const CHIRP: &[u8] = include_bytes!("../tests/fixtures/chirp_stereo_44100.ogg");

    #[test]
    fn header_matches_eager_decode() {
        let eager = decode_vorbis(CHIRP).unwrap();
        let stream = VorbisStream::new(CHIRP.to_vec()).unwrap();
        assert_eq!(stream.sample_rate(), eager.sample_rate());
        assert_eq!(stream.channels(), eager.channels());
    }

    #[test]
    fn incremental_concatenation_is_bit_identical_to_eager() {
        // The load-bearing streaming invariant: decoding packet-by-packet and
        // concatenating must equal decoding the whole file at once, sample for
        // sample. If lazy decode drifted from eager decode, streamed music would
        // sound subtly wrong while the eager path stayed correct.
        let eager = decode_vorbis(CHIRP).unwrap();
        let mut stream = VorbisStream::new(CHIRP.to_vec()).unwrap();

        let mut acc: Vec<f32> = Vec::new();
        let mut packets = 0usize;
        let mut max_packet = 0usize;
        while let Some(p) = stream.next_packet().unwrap() {
            max_packet = max_packet.max(p.len());
            packets += 1;
            acc.extend(p);
        }

        assert_eq!(acc.len(), eager.samples().len());
        assert_eq!(&acc, eager.samples());

        // Prove the working set is genuinely bounded, not "one giant packet":
        // the largest single packet is a small fraction of the whole track.
        assert!(packets > 1, "fixture should decode as multiple packets");
        assert!(
            max_packet * 4 < acc.len(),
            "largest packet {max_packet} was not a small fraction of {} total samples",
            acc.len()
        );
    }

    #[test]
    fn next_packet_is_none_after_finish() {
        let mut stream = VorbisStream::new(CHIRP.to_vec()).unwrap();
        while stream.next_packet().unwrap().is_some() {}
        assert!(stream.is_finished());
        assert!(stream.next_packet().unwrap().is_none());
        assert!(stream.next_packet().unwrap().is_none());
    }

    #[test]
    fn rejects_garbage_header() {
        let err = VorbisStream::new(vec![0u8; 64]).unwrap_err();
        assert!(matches!(err, AudioError::Decode(_)));
    }
}
