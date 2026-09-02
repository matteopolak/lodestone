//! A [`Voice`]: one playing sound instance inside the mixer.
//!
//! A voice owns a shared, immutable [`PcmBuffer`] plus its playback state (read
//! head, pitch, spatial placement, loop flag). Its one job is [`Voice::render_into`]:
//! additively mix itself into an interleaved stereo output block, applying the
//! full vanilla gain chain. Because it takes the [`Listener`] and a pre-computed
//! bus gain as arguments and touches no device, a voice is fully unit-testable
//! with synthetic PCM.
//!
//! # Mono vs stereo (an OpenAL rule vanilla inherits)
//!
//! OpenAL only spatialises **mono** sources. Vanilla decodes assets at their
//! native channel count and never downmixes, so a mono
//! `.ogg` is attenuated and panned while a stereo `.ogg` (music, records) plays
//! flat: channel 0 → left, channel 1 → right, gain only. [`Voice::render_into`]
//! reproduces exactly this split.

use std::sync::Arc;

use glam::Vec3;

use crate::category::SoundCategory;
use crate::decode::PcmBuffer;
use crate::spatial::{
    Attenuation, Listener, Spatialization, attenuation_gain, azimuth_of, panning_gains,
};

/// An opaque id for a playing voice, returned by the mixer so callers can stop
/// or update it later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlayHandle(pub u64);

/// One playing sound.
#[derive(Debug, Clone)]
pub struct Voice {
    handle: PlayHandle,
    pcm: Arc<PcmBuffer>,
    category: SoundCategory,
    spat: Spatialization,
    /// Playback-rate multiplier, already clamped to `[0.5, 2.0]` (vanilla's
    /// own pitch calculation).
    pitch: f32,
    looping: bool,
    /// Fractional read head, in source frames.
    pos: f64,
    finished: bool,
}

impl Voice {
    /// Creates a voice. `pitch` is clamped to vanilla's `[0.5, 2.0]` range here
    /// so callers cannot inject an out-of-range rate.
    pub fn new(
        handle: PlayHandle,
        pcm: Arc<PcmBuffer>,
        category: SoundCategory,
        spat: Spatialization,
        pitch: f32,
        looping: bool,
    ) -> Self {
        Self {
            handle,
            pcm,
            category,
            spat,
            pitch: pitch.clamp(0.5, 2.0),
            looping,
            pos: 0.0,
            finished: false,
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

    /// The per-instance volume (drives both clamped gain and audible range).
    pub fn instance_volume(&self) -> f32 {
        self.spat.instance_volume
    }

    /// Whether the voice has run to the end (looping voices never finish on
    /// their own).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Updates the spatial placement (e.g. an entity-bound sound moved).
    pub fn set_spatialization(&mut self, spat: Spatialization) {
        self.spat = spat;
    }

    /// Re-sets the per-instance volume on a live voice.
    ///
    /// The crossfade primitive: an ambient loop is started at volume 0 and ramped
    /// over 40 ticks, so its gain has to change *while it plays* rather than being
    /// fixed at `play` time. Note this also moves the audible range, because
    /// [`Spatialization::range`] derives from `instance_volume` — which is
    /// vanilla's behaviour too, and harmless for the head-relative loops this
    /// exists for (their range is already infinite).
    pub fn set_instance_volume(&mut self, volume: f32) {
        self.spat.instance_volume = volume;
    }

    /// Moves the source to a new world-space position, keeping every other
    /// spatial parameter (attenuation mode, range, per-instance volume). This is
    /// the entity-following primitive: a `SOUND_ENTITY` voice tracks its entity
    /// as it moves.
    pub fn set_position(&mut self, position: Vec3) {
        self.spat.position = position;
    }

    /// The spatial left/right channel gains for the current placement, computed
    /// once per render block. Returns `(left_gain, right_gain, is_stereo)`.
    ///
    /// * Stereo sources: `(bus_gain, bus_gain, true)` — flat, no spatialisation.
    /// * Mono, non-spatial (relative or `Attenuation::None`): centred equal-power.
    /// * Mono, attenuated: distance gain × equal-power pan from azimuth.
    fn channel_gains(&self, listener: &Listener, bus_gain: f32) -> (f32, f32, bool) {
        if !self.pcm.is_mono() {
            return (bus_gain, bus_gain, true);
        }
        let (pan_l, pan_r, dist_gain) =
            if self.spat.relative || self.spat.attenuation == Attenuation::None {
                let (l, r) = panning_gains(0.0);
                (l, r, 1.0)
            } else {
                let distance = (self.spat.position - listener.position).length();
                let dist_gain = attenuation_gain(distance, self.spat.range());
                let (l, r) = panning_gains(azimuth_of(listener, self.spat.position));
                (l, r, dist_gain)
            };
        (
            bus_gain * dist_gain * pan_l,
            bus_gain * dist_gain * pan_r,
            false,
        )
    }

    /// Additively mixes this voice into `out`, an interleaved **stereo** block
    /// (`[l, r, l, r, …]`) at `out_rate` Hz. `bus_gain` is the pre-computed
    /// category/master/instance gain from [`crate::CategoryVolumes`].
    ///
    /// The read head advances `pitch * source_rate / out_rate` source frames per
    /// output frame. When a non-looping voice reaches its end it sets
    /// [`is_finished`](Self::is_finished) and stops contributing; a looping voice
    /// wraps its read head.
    pub fn render_into(
        &mut self,
        out: &mut [f32],
        out_rate: u32,
        listener: &Listener,
        bus_gain: f32,
    ) {
        if self.finished || out.is_empty() || out_rate == 0 {
            return;
        }
        let frames = self.pcm.frames();
        if frames == 0 {
            self.finished = true;
            return;
        }
        let (gain_l, gain_r, stereo) = self.channel_gains(listener, bus_gain);
        let step = self.pitch as f64 * self.pcm.sample_rate() as f64 / out_rate as f64;
        let frames_f = frames as f64;

        let out_frames = out.len() / 2;
        for f in 0..out_frames {
            if !self.looping && self.pos.floor() as usize >= frames {
                self.finished = true;
                break;
            }
            if self.looping && self.pos >= frames_f {
                self.pos -= frames_f;
            }
            if stereo {
                let l = self.pcm.read_channel_lerp(self.pos, 0);
                let r = self.pcm.read_channel_lerp(self.pos, 1);
                out[f * 2] += l * gain_l;
                out[f * 2 + 1] += r * gain_r;
            } else {
                let s = self.pcm.read_channel_lerp(self.pos, 0);
                out[f * 2] += s * gain_l;
                out[f * 2 + 1] += s * gain_r;
            }
            self.pos += step;
        }
    }

    /// The source position (for tests/inspection).
    #[cfg(test)]
    pub(crate) fn position(&self) -> Vec3 {
        self.spat.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(sample_rate: u32, samples: Vec<f32>) -> Arc<PcmBuffer> {
        Arc::new(PcmBuffer::from_interleaved(sample_rate, 1, samples).unwrap())
    }

    fn non_spatial(position: Vec3) -> Spatialization {
        Spatialization {
            position,
            attenuation: Attenuation::None,
            attenuation_distance: 16.0,
            instance_volume: 1.0,
            relative: true,
        }
    }

    fn linear(position: Vec3, atten: f32) -> Spatialization {
        Spatialization {
            position,
            attenuation: Attenuation::Linear,
            attenuation_distance: atten,
            instance_volume: 1.0,
            relative: false,
        }
    }

    #[test]
    fn unit_pitch_same_rate_copies_samples_centred() {
        // Mono, non-spatial, bus gain 1: each output frame gets the source
        // sample times the centred equal-power gain (0.707) in both channels.
        let pcm = mono(48_000, vec![1.0, -1.0, 0.5]);
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            non_spatial(Vec3::ZERO),
            1.0,
            false,
        );
        let mut out = [0.0f32; 8]; // 4 stereo frames
        v.render_into(&mut out, 48_000, &Listener::default(), 1.0);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - 1.0 * c).abs() < 1e-6);
        assert!((out[1] - 1.0 * c).abs() < 1e-6);
        assert!((out[2] - -c).abs() < 1e-6);
        assert!((out[4] - 0.5 * c).abs() < 1e-6);
        // Ran out after 3 source frames.
        assert!(v.is_finished());
        assert_eq!(out[6], 0.0);
        assert_eq!(out[7], 0.0);
    }

    #[test]
    fn double_pitch_advances_twice_as_fast() {
        // A ramp read at pitch 2.0 should read positions 0,2,4,... -> 0,2,4.
        let ramp: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let pcm = mono(48_000, ramp);
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            non_spatial(Vec3::ZERO),
            2.0,
            false,
        );
        let mut out = [0.0f32; 8];
        v.render_into(&mut out, 48_000, &Listener::default(), 1.0);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - 0.0 * c).abs() < 1e-6);
        assert!((out[2] - 2.0 * c).abs() < 1e-6);
        assert!((out[4] - 4.0 * c).abs() < 1e-6);
        assert!((out[6] - 6.0 * c).abs() < 1e-6);
    }

    #[test]
    fn device_rate_conversion_halves_step() {
        // Source at 24 kHz played to a 48 kHz device -> step 0.5, so each source
        // sample is heard for two output frames (with interpolation between).
        let pcm = mono(24_000, vec![0.0, 2.0]);
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            non_spatial(Vec3::ZERO),
            1.0,
            false,
        );
        let mut out = [0.0f32; 8];
        v.render_into(&mut out, 48_000, &Listener::default(), 1.0);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        // positions 0,0.5,1,1.5 -> 0,1,2,2(clamped)
        assert!((out[0] - 0.0 * c).abs() < 1e-6);
        assert!((out[2] - 1.0 * c).abs() < 1e-6);
        assert!((out[4] - 2.0 * c).abs() < 1e-6);
    }

    #[test]
    fn looping_voice_wraps_and_never_finishes() {
        let pcm = mono(48_000, vec![1.0, 2.0]);
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            non_spatial(Vec3::ZERO),
            1.0,
            true,
        );
        let mut out = [0.0f32; 20]; // 10 frames, far past the 2-sample source
        v.render_into(&mut out, 48_000, &Listener::default(), 1.0);
        assert!(!v.is_finished(), "looping voice must not finish");
        let c = std::f32::consts::FRAC_1_SQRT_2;
        // frame 0 ->1, 1->2, 2->1 (wrapped), 3->2, ...
        assert!((out[0] - 1.0 * c).abs() < 1e-6);
        assert!((out[2] - 2.0 * c).abs() < 1e-6);
        assert!((out[4] - 1.0 * c).abs() < 1e-6);
        assert!((out[6] - 2.0 * c).abs() < 1e-6);
    }

    #[test]
    fn attenuation_reduces_distant_sound() {
        // Mono, linear attenuation, range 16. At distance 8 the distance gain is
        // 0.5. Source straight ahead so pan is centred (0.707 each).
        let pcm = mono(48_000, vec![1.0]);
        let listener = Listener::default();
        let pos = listener.position + listener.forward * 8.0; // 8 blocks in front
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            linear(pos, 16.0),
            1.0,
            false,
        );
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, &listener, 1.0);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        // 1.0 * dist_gain(0.5) * pan(0.707)
        assert!((out[0] - 0.5 * c).abs() < 1e-6, "left {}", out[0]);
        assert!((out[1] - 0.5 * c).abs() < 1e-6, "right {}", out[1]);
    }

    #[test]
    fn source_on_the_right_is_louder_on_the_right() {
        // A right-side source (at +X, in range) must have right gain > left gain.
        let pcm = mono(48_000, vec![1.0]);
        let listener = Listener::default();
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            linear(Vec3::new(4.0, 0.0, 0.0), 16.0),
            1.0,
            false,
        );
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, &listener, 1.0);
        assert!(
            out[1].abs() > out[0].abs(),
            "right {} !> left {}",
            out[1],
            out[0]
        );
        assert_eq!(v.position(), Vec3::new(4.0, 0.0, 0.0));
    }

    #[test]
    fn stereo_source_plays_flat_without_panning() {
        // Stereo asset: ch0 -> left, ch1 -> right, gain only, regardless of a
        // wildly off-axis position.
        let pcm = Arc::new(PcmBuffer::from_interleaved(48_000, 2, vec![0.3, 0.7]).unwrap());
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            linear(Vec3::new(100.0, 0.0, 0.0), 16.0),
            1.0,
            false,
        );
        let mut out = [0.0f32; 2];
        v.render_into(&mut out, 48_000, &Listener::default(), 0.5);
        assert!((out[0] - 0.3 * 0.5).abs() < 1e-6, "left {}", out[0]);
        assert!((out[1] - 0.7 * 0.5).abs() < 1e-6, "right {}", out[1]);
    }

    #[test]
    fn set_position_updates_the_source_location() {
        // The entity-following primitive: moving the source keeps every other
        // spatial parameter and just relocates it.
        let pcm = mono(48_000, vec![1.0; 8]);
        let mut v = Voice::new(
            PlayHandle(1),
            pcm,
            SoundCategory::Master,
            linear(Vec3::new(4.0, 0.0, 0.0), 16.0),
            1.0,
            false,
        );
        assert_eq!(v.position(), Vec3::new(4.0, 0.0, 0.0));
        v.set_position(Vec3::new(-9.0, 1.0, 2.0));
        assert_eq!(v.position(), Vec3::new(-9.0, 1.0, 2.0));
    }
}
