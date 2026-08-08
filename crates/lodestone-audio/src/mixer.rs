//! The [`Mixer`]: the device-free heart of the engine.
//!
//! A mixer owns the active [`Voice`]s, the [`Listener`], and the
//! [`CategoryVolumes`]. Its output is always **interleaved stereo `f32`** — the
//! game mixes down to two channels — and [`Mixer::render`] fills a caller-owned
//! block by summing every voice through the full vanilla gain chain. It calls no
//! device and no clock: the native `cpal` sink and the browser's `AudioWorklet`
//! both drive it by handing it a block to fill.
//!
//! The mixer does not apply a final limiter; the sum of many loud voices can
//! exceed `[-1, 1]`. Clamping to the device's representable range is the sink's
//! job, so the mixer stays linear and exactly assertable in tests.

use std::sync::Arc;

use crate::category::CategoryVolumes;
use crate::event::SoundInstance;
use crate::spatial::Listener;
use crate::voice::{PlayHandle, Voice};

/// The number of output channels the mixer produces (stereo).
pub const OUTPUT_CHANNELS: usize = 2;

/// A device-free stereo mixer.
#[derive(Debug)]
pub struct Mixer {
    output_sample_rate: u32,
    listener: Listener,
    volumes: CategoryVolumes,
    voices: Vec<Voice>,
    next_handle: u64,
}

impl Mixer {
    /// Creates a mixer that renders at `output_sample_rate` Hz (the device rate).
    pub fn new(output_sample_rate: u32) -> Self {
        Self {
            output_sample_rate,
            listener: Listener::default(),
            volumes: CategoryVolumes::new(),
            voices: Vec::new(),
            next_handle: 1,
        }
    }

    /// The output sample rate in Hz.
    pub fn output_sample_rate(&self) -> u32 {
        self.output_sample_rate
    }

    /// The listener transform.
    pub fn listener(&self) -> &Listener {
        &self.listener
    }

    /// Sets the listener transform (camera moved/rotated).
    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = listener;
    }

    /// Updates the world-space position of a playing voice — the entity-bound
    /// (`SOUND_ENTITY`) case, where the source moves while the sound plays.
    ///
    /// The mixer holds only the latest position *snapshot* and never queries an
    /// entity itself (it has no entity store, and must not gain one). The caller
    /// — which owns the entity state — re-reads the entity's live position each
    /// frame and pushes it here before [`render`](Self::render). Returns `false`
    /// if no live voice has `handle` (the sound finished and was reaped), which
    /// is the caller's signal to stop tracking it.
    pub fn set_voice_position(&mut self, handle: PlayHandle, position: glam::Vec3) -> bool {
        match self.voices.iter_mut().find(|v| v.handle() == handle) {
            Some(v) => {
                v.set_position(position);
                true
            }
            None => false,
        }
    }

    /// Re-sets a playing voice's per-instance volume. Returns `false` when no
    /// live voice has `handle`.
    ///
    /// The ambient-loop crossfade needs this: a loop starts at volume 0 and is
    /// ramped over 40 ticks by the caller's fade state, so the gain moves while
    /// the voice plays.
    pub fn set_voice_volume(&mut self, handle: PlayHandle, volume: f32) -> bool {
        match self.voices.iter_mut().find(|v| v.handle() == handle) {
            Some(v) => {
                v.set_instance_volume(volume);
                true
            }
            None => false,
        }
    }

    /// Read access to the category volumes.
    pub fn volumes(&self) -> &CategoryVolumes {
        &self.volumes
    }

    /// Mutable access to the category volumes (to move a slider, duck a bus…).
    pub fn volumes_mut(&mut self) -> &mut CategoryVolumes {
        &mut self.volumes
    }

    /// The number of currently active voices.
    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    /// Starts playing a resolved [`SoundInstance`], returning a handle that can
    /// later [`stop`](Self::stop) it.
    pub fn play(&mut self, instance: SoundInstance) -> PlayHandle {
        let handle = PlayHandle(self.next_handle);
        self.next_handle += 1;
        let voice = Voice::new(
            handle,
            Arc::clone(&instance.pcm),
            instance.category,
            instance.spatialization(),
            instance.pitch,
            instance.looping,
        );
        self.voices.push(voice);
        handle
    }

    /// Stops and removes the voice with `handle`, if present. Returns whether a
    /// voice was removed.
    pub fn stop(&mut self, handle: PlayHandle) -> bool {
        let before = self.voices.len();
        self.voices.retain(|v| v.handle() != handle);
        self.voices.len() != before
    }

    /// Stops every voice.
    pub fn stop_all(&mut self) {
        self.voices.clear();
    }

    /// Renders the next block into `out`, an interleaved stereo buffer
    /// (`out.len()` should be even; a trailing odd sample is ignored). The
    /// buffer is **overwritten** (zeroed, then every voice summed in). Voices
    /// that finish during the block are dropped afterwards.
    pub fn render(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            *s = 0.0;
        }
        let rate = self.output_sample_rate;
        let listener = self.listener;
        for voice in &mut self.voices {
            let bus_gain = self.volumes.gain(voice.category(), voice.instance_volume());
            voice.render_into(out, rate, &listener, bus_gain);
        }
        self.voices.retain(|v| !v.is_finished());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::category::SoundCategory;
    use crate::decode::PcmBuffer;
    use crate::spatial::Attenuation;
    use glam::Vec3;

    fn mono(rate: u32, samples: Vec<f32>) -> Arc<PcmBuffer> {
        Arc::new(PcmBuffer::from_interleaved(rate, 1, samples).unwrap())
    }

    fn relative_instance(pcm: Arc<PcmBuffer>) -> SoundInstance {
        SoundInstance::relative(pcm, SoundCategory::Master)
    }

    #[test]
    fn render_overwrites_the_buffer() {
        let mut m = Mixer::new(48_000);
        let mut out = [123.0f32; 8];
        m.render(&mut out); // no voices -> silence, not leftover garbage
        assert_eq!(out, [0.0; 8]);
    }

    #[test]
    fn two_voices_sum_sample_for_sample() {
        // Two centred non-spatial mono voices, master bus, master=1. Each sample
        // is (a + b) * centre_gain(0.707) in both channels.
        let mut m = Mixer::new(48_000);
        m.play(relative_instance(mono(48_000, vec![0.5, 0.25])));
        m.play(relative_instance(mono(48_000, vec![0.1, 0.1])));
        let mut out = [0.0f32; 4]; // 2 frames
        m.render(&mut out);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - (0.5 + 0.1) * c).abs() < 1e-6, "{}", out[0]);
        assert!((out[1] - (0.5 + 0.1) * c).abs() < 1e-6);
        assert!((out[2] - (0.25 + 0.1) * c).abs() < 1e-6, "{}", out[2]);
    }

    #[test]
    fn finished_voices_are_dropped_after_render() {
        let mut m = Mixer::new(48_000);
        m.play(relative_instance(mono(48_000, vec![1.0]))); // 1 frame
        assert_eq!(m.voice_count(), 1);
        let mut out = [0.0f32; 8]; // 4 frames -> voice ends
        m.render(&mut out);
        assert_eq!(m.voice_count(), 0, "one-shot voice should be reaped");
    }

    #[test]
    fn stop_removes_a_specific_voice() {
        let mut m = Mixer::new(48_000);
        let a = m.play(relative_instance(mono(48_000, vec![0.0; 48_000])));
        let _b = m.play(relative_instance(mono(48_000, vec![0.0; 48_000])));
        assert_eq!(m.voice_count(), 2);
        assert!(m.stop(a));
        assert_eq!(m.voice_count(), 1);
        assert!(
            !m.stop(a),
            "stopping an already-removed voice returns false"
        );
    }

    #[test]
    fn category_volume_scales_the_mix() {
        // A BLOCKS sound with blocks=0.5 renders at half amplitude.
        let mut m = Mixer::new(48_000);
        m.volumes_mut().set_user(SoundCategory::Blocks, 0.5);
        let inst = SoundInstance {
            category: SoundCategory::Blocks,
            ..relative_instance(mono(48_000, vec![1.0]))
        };
        m.play(inst);
        let mut out = [0.0f32; 2];
        m.render(&mut out);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - 0.5 * c).abs() < 1e-6, "{}", out[0]);
    }

    #[test]
    fn distant_and_near_voices_mix_at_realistic_scale() {
        // Realistic-scale guard (not a 1-sample degenerate case): 64 voices, a
        // 1024-frame block, mixed positional sounds at varied distances. Asserts
        // the near voice dominates and the far one is attenuated to silence.
        let mut m = Mixer::new(48_000);
        let listener = Listener::default();
        m.set_listener(listener);

        // 48 filler voices spread around, plus a known near and known far one.
        for i in 0..48 {
            let ang = i as f32;
            let inst = SoundInstance::positional(
                mono(48_000, vec![0.01; 2048]),
                SoundCategory::Ambient,
                Vec3::new(ang.sin() * 30.0, 0.0, ang.cos() * 30.0),
            );
            m.play(inst);
        }
        // Near voice, straight ahead, distance 1 -> gain ~ (1 - 1/16) = 0.9375.
        let near = SoundInstance::positional(
            mono(48_000, vec![1.0; 2048]),
            SoundCategory::Blocks,
            listener.position + listener.forward * 1.0,
        );
        m.play(near);
        // Far voice beyond range -> silent.
        let far = SoundInstance::positional(
            mono(48_000, vec![1.0; 2048]),
            SoundCategory::Blocks,
            listener.position + listener.forward * 1000.0,
        );
        m.play(far);

        assert_eq!(m.voice_count(), 50);
        let mut out = vec![0.0f32; 1024 * OUTPUT_CHANNELS];
        m.render(&mut out);

        // The block is non-silent and stays within a sane range (filler + near).
        let peak = out.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak > 0.5, "near voice should dominate, peak {peak}");
        assert!(peak.is_finite());
        // Voices are long (2048 frames) so none finished in a 1024-frame block.
        assert_eq!(m.voice_count(), 50);
    }

    #[test]
    fn none_attenuation_is_not_distance_scaled() {
        // A far-away Attenuation::None sound still plays at full centred gain.
        let mut m = Mixer::new(48_000);
        let inst = SoundInstance {
            attenuation: Attenuation::None,
            relative: false,
            position: Vec3::new(0.0, 0.0, -1000.0),
            ..relative_instance(mono(48_000, vec![1.0]))
        };
        m.play(inst);
        let mut out = [0.0f32; 2];
        m.render(&mut out);
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - c).abs() < 1e-6, "{}", out[0]);
    }

    #[test]
    fn set_voice_position_moves_a_live_voice_and_repans() {
        // Entity-following (SOUND_ENTITY): a playing voice repositioned from the
        // right to the left must flip which channel is louder on the *next*
        // block, proving the update reaches the gain computation rather than
        // merely being stored. A long buffer keeps the voice alive across both
        // renders.
        let mut m = Mixer::new(48_000);
        m.set_listener(Listener::default());
        let inst = SoundInstance::positional(
            mono(48_000, vec![1.0; 256]),
            SoundCategory::Blocks,
            Vec3::new(4.0, 0.0, 0.0), // to the right
        );
        let h = m.play(inst);

        let mut out = [0.0f32; 2];
        m.render(&mut out);
        assert!(
            out[1].abs() > out[0].abs(),
            "source on right: R {} !> L {}",
            out[1],
            out[0]
        );

        // Move the source to the left; the next block must favour the left.
        assert!(m.set_voice_position(h, Vec3::new(-4.0, 0.0, 0.0)));
        let mut out2 = [0.0f32; 2];
        m.render(&mut out2);
        assert!(
            out2[0].abs() > out2[1].abs(),
            "after move left: L {} !> R {}",
            out2[0],
            out2[1]
        );
    }

    #[test]
    fn set_voice_position_reports_whether_the_voice_is_live() {
        // The caller (which owns the entity store) uses the bool to know when to
        // stop tracking: a reaped/unknown handle returns false.
        let mut m = Mixer::new(48_000);
        let h = m.play(relative_instance(mono(48_000, vec![0.0; 128])));
        assert!(m.set_voice_position(h, Vec3::new(1.0, 0.0, 0.0)));
        assert!(
            !m.set_voice_position(PlayHandle(424_242), Vec3::ZERO),
            "unknown handle must report not-found"
        );
    }
}
