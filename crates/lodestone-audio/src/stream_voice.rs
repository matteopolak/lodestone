//! A [`StreamVoice`]: one playing streaming (music/record) source inside the
//! mixer, fed through a [`SampleRing`] instead of a decoded `PcmBuffer`.
//!
//! # Why this exists, and why it is not a [`Voice`](crate::voice::Voice)
//!
//! [`Voice`](crate::voice::Voice) plays a fully decoded `Arc<PcmBuffer>` —
//! correct for the vast majority of vanilla's sound corpus, and catastrophic
//! for music: `music/game/end/the_end.ogg` alone decodes to 304 MiB (see
//! [`crate::stream`]'s module doc). So a streaming voice never holds decoded
//! PCM at all; it holds an [`Arc<SampleRing>`] and pulls from it here, on the
//! realtime render path, without ever allocating or blocking.
//!
//! # Who fills the ring
//!
//! This module knows nothing about threads, decode, or Vorbis — it is exactly
//! as device-free as [`Voice`](crate::voice::Voice). A producer (a native
//! thread for `lodestone-sound`'s `AudioEngine`, or the browser's main thread,
//! since a `ScriptProcessorNode` callback already runs there) owns a
//! [`VorbisStream`](crate::stream::VorbisStream) and pushes decoded packets
//! into the same [`SampleRing`] with [`SampleRing::write`]. [`ended`] is how
//! the producer says "no more data is coming" — checked only once every
//! buffered sample has actually been played, so already-buffered audio always
//! finishes first.
//!
//! # A bounded window, not a two-tap resampler
//!
//! [`render_into`](StreamVoice::render_into) bulk-reads whatever the ring
//! currently holds into a small owned `window: Vec<f32>` at the top of every
//! call, then interpolates against that window with the exact clamp-at-tail
//! rule [`PcmBuffer::read_channel_lerp`](crate::decode::PcmBuffer::read_channel_lerp)
//! uses (`i + 1 >= frames` returns the last frame outright, no extrapolation).
//! A naive two-frame-lookahead streaming resampler was tried first and
//! rejected: it needs the sample *after* the last one to confirm it has
//! reached the true last sample, so it silently drops exactly one frame at
//! the end of every track. Reusing `PcmBuffer`'s own tail rule sidesteps that
//! by construction, at the cost of an occasional bounded `Vec::drain`
//! (a memmove, not an allocation) to keep `window` from growing forever —
//! see [`trim_consumed`](StreamVoice::trim_consumed).
//!
//! # Underrun is silence, never a stall
//!
//! If the window runs dry (nothing left to read at the current position) and
//! the producer has not yet marked [`ended`](StreamSource::ended),
//! `render_into` stops mixing for the *rest of that block* and returns.
//! [`Mixer::render`](crate::mixer::Mixer::render) already zeroed the output
//! buffer before calling any voice, so the unfilled tail is silence, not
//! garbage and not a panic; playback resumes from exactly where it left off
//! on the next call once the producer has caught up. Only once the window is
//! dry **and** the producer has signalled `ended` does the voice finish.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::category::SoundCategory;
use crate::resample::lerp;
use crate::ring::SampleRing;
use crate::voice::PlayHandle;

/// The maximum source channel count a [`StreamVoice`] will accept. Every real
/// vanilla music/record asset is mono or stereo (`Voice`'s own mono/stereo
/// split is the whole spatialisation model this crate has); this is
/// defensive headroom against a corrupt/malicious header, not an expected
/// value. A source declaring more is rejected at construction (see
/// [`StreamVoice::new`]) rather than reading it and silently mixing garbage.
const MAX_SOURCE_CHANNELS: usize = 8;

/// What [`Mixer::play_stream`](crate::mixer::Mixer::play_stream) needs to
/// start a streaming voice: the shared ring plus the playback parameters a
/// [`SoundInstance`](crate::event::SoundInstance) would otherwise carry.
///
/// Music is always head-relative (vanilla's own music-sound constructor
/// forces this), so unlike [`SoundInstance`](crate::event::SoundInstance)
/// there is no position/attenuation here — every field a streaming voice
/// needs is present. The runtime volume *fade* (the music-bus crossfade)
/// applies through [`crate::CategoryVolumes::set_runtime_gain`], not a
/// per-voice field — matching vanilla's own music-fade routine, which
/// updates the music bus's category volume directly rather than touching the
/// individual sound instance.
#[derive(Debug, Clone)]
pub struct StreamSource {
    /// The producer-fed sample ring, interleaved at `source_channels`.
    pub ring: Arc<SampleRing>,
    /// The source's channel count (from the Vorbis header).
    pub source_channels: u16,
    /// The source's native sample rate in Hz.
    pub source_rate: u32,
    /// The mixer bus this voice plays on (`Music` or `Records`).
    pub category: SoundCategory,
    /// The resolved sound entry's volume (`sounds.json`'s `volume`, already
    /// multiplied by any packet volume) — clamped into the gain exactly like
    /// an ordinary [`SoundInstance`](crate::event::SoundInstance).
    pub volume: f32,
    /// Playback-rate multiplier. Vanilla never varies music pitch, but the
    /// field exists for parity with every other voice type and is clamped to
    /// `[0.5, 2.0]` here exactly as [`Voice::new`](crate::voice::Voice::new)
    /// does.
    pub pitch: f32,
    /// Set by the producer once it will push no further samples (decode
    /// finished or errored). Checked only after every buffered sample has
    /// actually been consumed.
    pub ended: Arc<AtomicBool>,
}

/// One playing streaming voice — see the module doc.
#[derive(Debug)]
pub struct StreamVoice {
    handle: PlayHandle,
    ring: Arc<SampleRing>,
    /// `0` when construction rejected the source (bad/too-many channels): the
    /// voice is immediately finished and contributes silence.
    source_channels: usize,
    source_rate: u32,
    category: SoundCategory,
    instance_volume: f32,
    pitch: f32,
    ended: Arc<AtomicBool>,
    /// Interleaved source frames currently buffered, oldest first. Bulk-filled
    /// from `ring` at the top of every [`render_into`](Self::render_into)
    /// call, and trimmed from the front as `pos` advances past them.
    window: Vec<f32>,
    /// Fractional frame position into `window` (`0.0` = its first frame).
    /// Always non-negative.
    pos: f64,
    finished: bool,
}

impl StreamVoice {
    pub(crate) fn new(handle: PlayHandle, source: StreamSource) -> Self {
        let channels = usize::from(source.source_channels);
        let rejected = channels == 0 || channels > MAX_SOURCE_CHANNELS;
        // Reserve for the ring's full capacity, doubled: `top_up` can add up
        // to `ring.capacity()` samples in one call, and `trim_consumed` only
        // drops what has actually played, so a block that tops up heavily
        // while consuming little can transiently hold close to two ring's
        // worth before the next trim catches up. Reserving this up front is
        // what keeps steady-state operation allocation-free, which is the
        // realtime-safety property that matters — an occasional cold-start
        // realloc on a freshly constructed voice (never on the render
        // callback's hot path once warmed) is not a correctness issue.
        let reserve = if rejected {
            0
        } else {
            source.ring.capacity().saturating_mul(2)
        };
        Self {
            handle,
            ring: source.ring,
            source_channels: if rejected { 0 } else { channels },
            source_rate: source.source_rate,
            category: source.category,
            instance_volume: source.volume,
            pitch: source.pitch.clamp(0.5, 2.0),
            ended: source.ended,
            window: Vec::with_capacity(reserve),
            pos: 0.0,
            // A rejected source never becomes audible; it is reaped on the
            // next render like any other finished voice, not left dangling.
            finished: rejected,
        }
    }

    /// This voice's handle.
    pub fn handle(&self) -> PlayHandle {
        self.handle
    }

    /// The mixer bus this voice plays on.
    pub fn category(&self) -> SoundCategory {
        self.category
    }

    /// The per-instance volume (feeds the bus-gain computation, same as
    /// [`Voice::instance_volume`](crate::voice::Voice::instance_volume)).
    pub fn instance_volume(&self) -> f32 {
        self.instance_volume
    }

    /// Whether the voice has finished: the producer signalled no more data is
    /// coming and every buffered sample has been played.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Additively mixes this voice into `out` (interleaved stereo), exactly
    /// like [`Voice::render_into`](crate::voice::Voice::render_into) —
    /// `bus_gain` is the pre-computed category/master/instance gain. Music is
    /// always head-relative and never panned (stereo assets play flat; a
    /// mono one would play centred), so unlike `Voice` this needs no
    /// [`Listener`](crate::spatial::Listener).
    pub fn render_into(&mut self, out: &mut [f32], out_rate: u32, bus_gain: f32) {
        if self.finished
            || out.is_empty()
            || out_rate == 0
            || self.source_rate == 0
            || self.source_channels == 0
        {
            return;
        }
        self.top_up();

        let step = f64::from(self.pitch) * f64::from(self.source_rate) / f64::from(out_rate);
        let ch = self.source_channels;
        let out_frames = out.len() / 2;

        for f in 0..out_frames {
            let frames = self.window.len() / ch;
            if frames == 0 {
                self.maybe_finish();
                return; // nothing buffered at all: silence for the rest of this block
            }
            let i = self.pos.floor() as usize;
            if i >= frames {
                self.maybe_finish();
                return; // consumed everything buffered so far: silence for the rest of this block
            }
            let t = (self.pos - i as f64) as f32;
            let (l, r) = self.read_lerp(i, frames, ch, t);
            out[f * 2] += l * bus_gain;
            out[f * 2 + 1] += r * bus_gain;
            self.pos += step;
        }

        self.trim_consumed();
    }

    /// Marks the voice finished — called only once nothing is left to render
    /// at the current position, so this is correct whether that emptiness is
    /// "the producer has not caught up yet" (checked via `ended`, below) or
    /// "the track is genuinely over".
    fn maybe_finish(&mut self) {
        if self.ended.load(Ordering::Acquire) {
            self.finished = true;
        }
    }

    /// Reads channel 0/1 at frame `i` with linear interpolation toward
    /// `i + 1`, clamping to `i` itself (no extrapolation) once `i + 1` would
    /// run past the buffered window — identical rule to
    /// [`PcmBuffer::read_channel_lerp`](crate::decode::PcmBuffer::read_channel_lerp),
    /// which is what lets the very last sample of a track play in full
    /// instead of needing an unavailable look-ahead frame to confirm it.
    /// Mono duplicates channel 0 into both outputs, matching
    /// [`PcmBuffer::sample`](crate::decode::PcmBuffer::sample)'s rule.
    fn read_lerp(&self, i: usize, frames: usize, ch: usize, t: f32) -> (f32, f32) {
        let sample = |frame: usize, channel: usize| self.window[frame * ch + channel];
        let (l0, r0) = if ch >= 2 {
            (sample(i, 0), sample(i, 1))
        } else {
            (sample(i, 0), sample(i, 0))
        };
        if i + 1 >= frames {
            return (l0, r0);
        }
        let (l1, r1) = if ch >= 2 {
            (sample(i + 1, 0), sample(i + 1, 1))
        } else {
            (sample(i + 1, 0), sample(i + 1, 0))
        };
        (lerp(l0, l1, t), lerp(r0, r1, t))
    }

    /// Bulk-appends whatever whole frames the ring currently holds onto
    /// `window`. A single [`SampleRing::read`] call per render block, rather
    /// than one per source frame — cheaper, and it is what keeps a block that
    /// needs many source frames (a fast pitch, or a block after an underrun)
    /// from paying per-sample atomic-load overhead.
    fn top_up(&mut self) {
        let ch = self.source_channels;
        let readable_frames = self.ring.len() / ch;
        if readable_frames == 0 {
            return;
        }
        let readable_samples = readable_frames * ch;
        let old_len = self.window.len();
        self.window.resize(old_len + readable_samples, 0.0);
        let n = self.ring.read(&mut self.window[old_len..]);
        // `readable_frames` was derived from `ring.len()` just above and this
        // is the sole consumer, so `n` must equal `readable_samples`; keep
        // the window frame-aligned defensively rather than trusting that.
        self.window.truncate(old_len + n - (n % ch));
    }

    /// Drops every fully-consumed leading frame (everything before
    /// `pos.floor()`) from `window`, shifting `pos` down to match — the
    /// memmove that keeps `window` from growing without bound over a
    /// multi-minute track. A [`Vec::drain`] is a memmove, not an allocation,
    /// so this stays realtime-safe.
    fn trim_consumed(&mut self) {
        let ch = self.source_channels;
        let frames = self.window.len() / ch;
        let i = (self.pos.floor() as usize).min(frames);
        if i == 0 {
            return;
        }
        self.window.drain(0..i * ch);
        self.pos -= i as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_with(samples: &[f32], capacity: usize) -> Arc<SampleRing> {
        let ring = Arc::new(SampleRing::with_min_capacity(capacity));
        assert_eq!(ring.write(samples), samples.len(), "fixture must fit");
        ring
    }

    fn source(ring: Arc<SampleRing>, channels: u16, rate: u32, ended: bool) -> StreamSource {
        StreamSource {
            ring,
            source_channels: channels,
            source_rate: rate,
            category: SoundCategory::Music,
            volume: 1.0,
            pitch: 1.0,
            ended: Arc::new(AtomicBool::new(ended)),
        }
    }

    #[test]
    fn mono_stream_plays_centred_at_unit_rate() {
        // Same shape as `voice::tests::unit_pitch_same_rate_copies_samples_centred`,
        // but through the ring instead of a PcmBuffer — a direct cross-check that
        // streaming produces identical numbers to the eager path at bus_gain=1,
        // including the tail: all three samples must play, none dropped.
        let ring = ring_with(&[1.0, -1.0, 0.5], 8);
        let src = source(ring, 1, 48_000, true);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [0.0f32; 8]; // 4 stereo frames
        v.render_into(&mut out, 48_000, 1.0);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 1.0);
        assert_eq!(out[2], -1.0);
        assert_eq!(out[3], -1.0);
        assert_eq!(out[4], 0.5, "the last sample must play, not be dropped");
        assert_eq!(out[5], 0.5);
        assert!(v.is_finished());
        assert_eq!(out[6], 0.0);
        assert_eq!(out[7], 0.0);
    }

    #[test]
    fn stereo_stream_plays_flat_left_right() {
        let ring = ring_with(&[0.3, 0.7], 8);
        let src = source(ring, 2, 48_000, true);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, 0.5);
        assert!((out[0] - 0.3 * 0.5).abs() < 1e-6, "left {}", out[0]);
        assert!((out[1] - 0.7 * 0.5).abs() < 1e-6, "right {}", out[1]);
    }

    #[test]
    fn device_rate_conversion_resamples_like_voice_does() {
        // Mirrors `voice::tests::device_rate_conversion_halves_step`: source at
        // 24 kHz played to a 48 kHz device -> step 0.5, each source sample heard
        // for two output frames with interpolation between.
        let ring = ring_with(&[0.0, 2.0], 8);
        let src = source(ring, 1, 24_000, true);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [0.0f32; 8];
        v.render_into(&mut out, 48_000, 1.0);
        // positions 0, 0.5, 1.0, 1.5 -> 0.0, 1.0, 2.0, 2.0 (clamped at the tail).
        assert!((out[0] - 0.0).abs() < 1e-6, "{}", out[0]);
        assert!((out[2] - 1.0).abs() < 1e-6, "{}", out[2]);
        assert!((out[4] - 2.0).abs() < 1e-6, "{}", out[4]);
        assert!((out[6] - 2.0).abs() < 1e-6, "{}", out[6]);
    }

    #[test]
    fn underrun_before_any_data_is_silent_and_does_not_finish() {
        // Control pair: an EMPTY, NOT-ended ring must render silence and must
        // NOT be marked finished (the producer might still be starting up).
        let ring = Arc::new(SampleRing::with_min_capacity(8));
        let src = source(ring, 2, 48_000, false);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [7.0f32; 8]; // non-zero sentinel so a no-op is visible
        v.render_into(&mut out, 48_000, 1.0);
        assert_eq!(out, [7.0; 8], "render_into must not touch `out` on underrun");
        assert!(!v.is_finished(), "an underrun while not ended must retry, not end");
    }

    #[test]
    fn underrun_mid_block_leaves_the_tail_silent_then_recovers() {
        // The healthy-arm control this class of test needs: prove data that
        // WOULD have played, would have played, by feeding the same voice again
        // once the producer catches up.
        let ring = Arc::new(SampleRing::with_min_capacity(8));
        // Only one stereo frame available; two are requested.
        assert_eq!(ring.write(&[0.4, 0.6]), 2);
        let src = StreamSource {
            ring: Arc::clone(&ring),
            source_channels: 2,
            source_rate: 48_000,
            category: SoundCategory::Music,
            volume: 1.0,
            pitch: 1.0,
            ended: Arc::new(AtomicBool::new(false)),
        };
        let mut v = StreamVoice::new(PlayHandle(1), src);

        // `render_into` is ADDITIVE (like `Voice::render_into` — `Mixer::render`
        // is what zeroes the buffer before calling any voice), so only the
        // frame that must stay untouched carries a non-zero sentinel; the
        // frame expected to receive real data starts at the same `0.0` a real
        // `Mixer::render` call would present it with.
        let mut out = [0.0f32, 0.0f32, 9.0f32, 9.0f32]; // 2 stereo frames requested, only 1 buffered
        v.render_into(&mut out, 48_000, 1.0);
        assert_eq!(&out[..2], &[0.4, 0.6], "the buffered frame must play");
        assert_eq!(
            &out[2..],
            &[9.0, 9.0],
            "the second frame must stay untouched (silent), not repeat the first"
        );
        assert!(!v.is_finished(), "producer is not ended -> retry, not end");

        // Healthy-arm control: push the missing frame and render again into a
        // fresh (zeroed) buffer — proves the earlier gap really was "no data
        // yet", not a permanent stall or a bug that would also eat this frame.
        assert_eq!(ring.write(&[0.1, 0.2]), 2);
        let mut out2 = [0.0f32; 2];
        v.render_into(&mut out2, 48_000, 1.0);
        assert_eq!(out2, [0.1, 0.2], "recovery must resume exactly where it left off");
    }

    #[test]
    fn ended_and_drained_finishes_the_voice() {
        let ring = ring_with(&[1.0, 1.0], 8);
        let src = source(ring, 2, 48_000, true);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, 1.0);
        assert_eq!(out, [1.0, 1.0]);
        // One more render: nothing left, ended=true -> finished.
        let mut out2 = [0.0f32; 2];
        v.render_into(&mut out2, 48_000, 1.0);
        assert!(v.is_finished());
        assert_eq!(out2, [0.0, 0.0]);
    }

    #[test]
    fn ended_but_not_drained_keeps_playing() {
        // Control for the "ended" flag's own precedence: ended=true must not
        // truncate audio still sitting in the ring.
        let ring = ring_with(&[1.0, 1.0, 2.0, 2.0], 8);
        let src = source(ring, 2, 48_000, true);
        let mut v = StreamVoice::new(PlayHandle(1), src);
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, 1.0);
        assert_eq!(out, [1.0, 1.0], "first buffered frame must still play");
        assert!(!v.is_finished(), "data remained in the ring -> not finished yet");
    }

    #[test]
    fn too_many_channels_is_rejected_not_corrupting() {
        let ring = ring_with(&[1.0; 32], 64);
        let src = source(ring, 9, 48_000, false);
        let v = StreamVoice::new(PlayHandle(1), src);
        assert!(v.is_finished(), "an unsupported channel count must never play");
    }

    #[test]
    fn a_long_track_across_many_blocks_stays_bit_faithful_and_bounded() {
        // Realistic-scale guard: 20,000 mono frames pushed in bursts (never all
        // at once), rendered in small blocks, reconstructed and compared
        // sample-for-sample against the source — proving `top_up`/`trim_consumed`
        // never lose, duplicate, or reorder a sample over many render calls.
        const N: usize = 20_000;
        let source_samples: Vec<f32> = (0..N).map(|i| (i % 997) as f32 / 997.0).collect();
        let ring = Arc::new(SampleRing::with_min_capacity(2048));
        let ended = Arc::new(AtomicBool::new(false));
        let src = StreamSource {
            ring: Arc::clone(&ring),
            source_channels: 1,
            source_rate: 48_000,
            category: SoundCategory::Music,
            volume: 1.0,
            pitch: 1.0,
            ended: Arc::clone(&ended),
        };
        let mut v = StreamVoice::new(PlayHandle(1), src);

        let mut produced = 0usize;
        let mut reconstructed: Vec<f32> = Vec::with_capacity(N);
        let mut out = [0.0f32; 64]; // 32 stereo frames per block
        loop {
            // Feed a burst, capped by the ring's free space.
            let burst_end = (produced + 137).min(N);
            let mut off = produced;
            while off < burst_end {
                let w = ring.write(&source_samples[off..burst_end]);
                if w == 0 {
                    break; // ring full; drain via render below before pushing more
                }
                off += w;
            }
            produced = off;
            if produced == N {
                ended.store(true, Ordering::Release);
            }

            v.render_into(&mut out, 48_000, 1.0);
            // Left/right are identical (mono), so only the left channel is kept.
            reconstructed.extend(out.iter().step_by(2));
            out = [0.0f32; 64];

            if v.is_finished() {
                break;
            }
            assert!(
                reconstructed.len() < N * 3,
                "voice never finished after producing far more output than input \
                 ({} frames out for {N} in) — top_up/trim likely stuck",
                reconstructed.len()
            );
        }

        // Trim the reconstructed tail to the real sample count (unit pitch,
        // unit rate: every rendered frame should map 1:1) and compare exactly.
        reconstructed.truncate(N);
        assert_eq!(reconstructed.len(), N, "must have produced every sample");
        assert_eq!(
            reconstructed, source_samples,
            "streamed output must equal the source bit-for-bit at unit rate"
        );
    }
}
