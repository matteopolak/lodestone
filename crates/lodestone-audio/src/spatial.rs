//! Positional audio: audible range, distance attenuation, and stereo panning.
//!
//! # Vanilla parity (Minecraft 26.2)
//!
//! **Range.** Vanilla computes
//! `attenuationDistance = max(instanceVolume, 1.0) * soundAttenuationDistance`,
//! where the per-sound attenuation distance defaults to `16`. So a sound at
//! volume ≤ 1 reaches its raw attenuation distance (16 blocks by default), and
//! a sound at volume > 1 reaches proportionally further — while its *gain* is
//! still clamped to 1 (see [`crate::category`]). Both facts are reproduced
//! here.
//!
//! **Attenuation model.** Vanilla's linear-attenuation setup configures the
//! OpenAL source with:
//! ```text
//! AL_DISTANCE_MODEL  = AL_LINEAR_DISTANCE (0xD003)
//! AL_MAX_DISTANCE    = maxDistance
//! AL_ROLLOFF_FACTOR  = 1.0
//! AL_REFERENCE_DISTANCE = 0.0
//! ```
//! The OpenAL 1.1 `AL_LINEAR_DISTANCE` gain, with reference 0 and rolloff 1,
//! reduces to `gain = 1 - clamp(distance, 0, max) / max`, i.e.
//! [`attenuation_gain`]. This is verified against both the decompiled source and
//! the OpenAL spec.
//!
//! **Panning — an explicit approximation.** OpenAL does the actual stereo
//! panning internally, and the exact geometry is an OpenAL-Soft implementation
//! detail (it uses a per-channel gain matrix and, with HRTF, filtering) that we
//! do **not** attempt to reproduce sample-for-sample. [`panning_gains`] instead
//! applies a standard constant-power pan from the source azimuth in the
//! listener's frame. It is correct in the properties that matter for gameplay
//! (hard-left/hard-right/centre, energy preservation) but is not claimed to match
//! OpenAL-Soft's output bit-for-bit. Vanilla also only spatialises **mono**
//! sources; stereo assets play unpanned — a rule enforced by the mixer.

use glam::Vec3;

/// Whether and how a sound attenuates with distance, mirroring vanilla's
/// `SoundInstance.Attenuation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attenuation {
    /// No distance attenuation; plays at full gain regardless of position
    /// (vanilla `Attenuation.NONE` / `Channel.disableAttenuation`).
    None,
    /// Linear distance attenuation (vanilla `Attenuation.LINEAR`).
    Linear,
}

/// The listener: where the "ears" are and which way they face.
///
/// `forward` and `up` are the camera basis vectors; `right` is derived as
/// `forward × up`. They need not be perfectly orthonormal — they are normalised
/// on use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    /// World-space listener position.
    pub position: Vec3,
    /// World-space forward direction.
    pub forward: Vec3,
    /// World-space up direction.
    pub up: Vec3,
}

impl Default for Listener {
    /// At the origin, looking down `-Z` with `+Y` up (vanilla's initial
    /// `ListenerTransform`-style convention).
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::new(0.0, 0.0, -1.0),
            up: Vec3::Y,
        }
    }
}

impl Listener {
    /// The listener's right-hand axis, `forward × up`, normalised.
    pub fn right(&self) -> Vec3 {
        let f = self.forward.normalize_or_zero();
        let u = self.up.normalize_or_zero();
        f.cross(u).normalize_or_zero()
    }
}

/// How a sound is placed in the world relative to the listener.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spatialization {
    /// World-space source position (ignored when [`relative`](Self::relative)).
    pub position: Vec3,
    /// Attenuation mode.
    pub attenuation: Attenuation,
    /// The sound's raw attenuation distance in blocks (vanilla default 16).
    pub attenuation_distance: f32,
    /// The per-instance volume, used for the range scaling `max(volume, 1)`
    /// (its clamped effect on *gain* is applied separately by the mixer).
    pub instance_volume: f32,
    /// When true the source is head-relative: no attenuation and no panning
    /// (vanilla's own head-relative flag), used for UI/music.
    pub relative: bool,
}

impl Spatialization {
    /// The audible range in blocks: `max(instance_volume, 1) * attenuation_distance`,
    /// or infinite when non-attenuating / relative.
    pub fn range(&self) -> f32 {
        if self.relative || self.attenuation == Attenuation::None {
            f32::INFINITY
        } else {
            self.instance_volume.max(1.0) * self.attenuation_distance
        }
    }
}

/// OpenAL `AL_LINEAR_DISTANCE` gain with reference 0 and rolloff 1:
/// `1 - clamp(distance, 0, max_distance) / max_distance`, clamped to `[0, 1]`.
///
/// A `max_distance <= 0` degenerates to "audible only exactly at the source":
/// full gain at distance 0, silent otherwise (avoids a divide-by-zero).
pub fn attenuation_gain(distance: f32, max_distance: f32) -> f32 {
    if max_distance <= 0.0 {
        return if distance <= 0.0 { 1.0 } else { 0.0 };
    }
    let clamped = distance.clamp(0.0, max_distance);
    (1.0 - clamped / max_distance).clamp(0.0, 1.0)
}

/// Constant-power stereo panning gains `(left, right)` for a source at `azimuth`,
/// a signed left/right position in `[-1, 1]` (`-1` hard left, `+1` hard right,
/// `0` centred).
///
/// Uses the equal-power law `left = cos(θ)`, `right = sin(θ)` with
/// `θ = (azimuth + 1) * π/4`, so `left² + right²` is constant across the pan and
/// a centred source is `-3 dB` in each channel (the standard convention). This
/// is the documented approximation of OpenAL's internal panner, not a bit-exact
/// reproduction of it.
pub fn panning_gains(azimuth: f32) -> (f32, f32) {
    let a = azimuth.clamp(-1.0, 1.0);
    let theta = (a + 1.0) * (std::f32::consts::FRAC_PI_4);
    (theta.cos(), theta.sin())
}

/// The signed left/right azimuth in `[-1, 1]` of `source` in the `listener`'s
/// frame: the component of the (normalised) listener→source direction along the
/// listener's right axis. `0` when the source coincides with the listener.
pub fn azimuth_of(listener: &Listener, source: Vec3) -> f32 {
    let to_source = source - listener.position;
    let dir = to_source.normalize_or_zero();
    if dir == Vec3::ZERO {
        return 0.0;
    }
    dir.dot(listener.right()).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_uses_max_volume_one_times_attenuation_distance() {
        // volume <= 1 -> exactly attenuation_distance (16 by default).
        let s = Spatialization {
            position: Vec3::ZERO,
            attenuation: Attenuation::Linear,
            attenuation_distance: 16.0,
            instance_volume: 0.5,
            relative: false,
        };
        assert_eq!(s.range(), 16.0);

        // volume 2 -> reaches twice as far, even though gain is clamped to 1.
        let loud = Spatialization {
            instance_volume: 2.0,
            ..s
        };
        assert_eq!(loud.range(), 32.0);
    }

    #[test]
    fn none_and_relative_are_infinite_range() {
        let base = Spatialization {
            position: Vec3::ZERO,
            attenuation: Attenuation::None,
            attenuation_distance: 16.0,
            instance_volume: 1.0,
            relative: false,
        };
        assert_eq!(base.range(), f32::INFINITY);
        let rel = Spatialization {
            attenuation: Attenuation::Linear,
            relative: true,
            ..base
        };
        assert_eq!(rel.range(), f32::INFINITY);
    }

    #[test]
    fn linear_attenuation_matches_openal_formula() {
        // gain = 1 - d/max. Checked at several independent points.
        assert_eq!(attenuation_gain(0.0, 16.0), 1.0);
        assert_eq!(attenuation_gain(4.0, 16.0), 0.75);
        assert_eq!(attenuation_gain(8.0, 16.0), 0.5);
        assert_eq!(attenuation_gain(16.0, 16.0), 0.0);
        // Beyond max is clamped to silence, never negative.
        assert_eq!(attenuation_gain(100.0, 16.0), 0.0);
    }

    #[test]
    fn attenuation_zero_max_distance_is_safe() {
        assert_eq!(attenuation_gain(0.0, 0.0), 1.0);
        assert_eq!(attenuation_gain(0.1, 0.0), 0.0);
        assert!(attenuation_gain(1.0, 0.0).is_finite());
    }

    #[test]
    fn panning_is_energy_preserving_and_correct_at_extremes() {
        let (l, r) = panning_gains(0.0);
        // Centre: equal, and -3 dB (1/sqrt(2)) each.
        assert!((l - r).abs() < 1e-6);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);

        let (ll, lr) = panning_gains(-1.0);
        assert!((ll - 1.0).abs() < 1e-6, "hard left -> full left");
        assert!(lr.abs() < 1e-6, "hard left -> no right");

        let (rl, rr) = panning_gains(1.0);
        assert!(rl.abs() < 1e-6, "hard right -> no left");
        assert!((rr - 1.0).abs() < 1e-6, "hard right -> full right");

        // Constant power across the sweep.
        for i in -10..=10 {
            let a = i as f32 / 10.0;
            let (l, r) = panning_gains(a);
            assert!(
                (l * l + r * r - 1.0).abs() < 1e-5,
                "power not preserved at {a}"
            );
        }
    }

    #[test]
    fn azimuth_places_source_left_and_right() {
        // Default listener looks down -Z, up +Y, so right = (-Z) x (+Y).
        // right = forward × up = (0,0,-1) × (0,1,0) = ( (0*0 - -1*1), (-1*0 - 0*0), (0*1 - 0*0) )
        //       = (1, 0, 0).  So +X is to the listener's right.
        let listener = Listener::default();
        assert!((listener.right() - Vec3::X).length() < 1e-6);

        let right_source = azimuth_of(&listener, Vec3::new(5.0, 0.0, 0.0));
        assert!(
            right_source > 0.9,
            "source at +X should pan right, got {right_source}"
        );

        let left_source = azimuth_of(&listener, Vec3::new(-5.0, 0.0, 0.0));
        assert!(
            left_source < -0.9,
            "source at -X should pan left, got {left_source}"
        );

        // Directly in front (down -Z): centred.
        let front = azimuth_of(&listener, Vec3::new(0.0, 0.0, -5.0));
        assert!(
            front.abs() < 1e-6,
            "front source should be centred, got {front}"
        );
    }
}
