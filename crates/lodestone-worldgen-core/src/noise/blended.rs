//! `BlendedNoise` (`old_blended_noise`) — the legacy 3D terrain noise still used
//! by the overworld `base_3d_noise` density function.
//!
//! Reproduces vanilla's own blended-noise class: three
//! legacy-init Perlin stacks (two 16-octave limit noises and one 8-octave main
//! noise) combined with the vanilla smear/blend loop.

use crate::math::clamped_lerp;
use crate::noise::perlin::{PerlinNoise, wrap};
use crate::rng::RandomSource;

/// The blended terrain noise.
#[derive(Debug, Clone)]
pub struct BlendedNoise {
    min_limit_noise: PerlinNoise,
    max_limit_noise: PerlinNoise,
    main_noise: PerlinNoise,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
}

impl BlendedNoise {
    /// Appends a complete, bit-exact description of this noise to `out` — see
    /// [`crate::noise::ImprovedNoise::write_signature`] for the contract.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        self.min_limit_noise.write_signature(out);
        self.max_limit_noise.write_signature(out);
        self.main_noise.write_signature(out);
        for v in [
            self.xz_multiplier,
            self.y_multiplier,
            self.xz_factor,
            self.y_factor,
            self.smear_scale_multiplier,
        ] {
            out.push(v.to_bits());
        }
    }

    /// Builds from a freshly-seeded source (vanilla's own "with new random" path), consuming
    /// the min, max, then main noise stacks in order.
    pub fn new<R: RandomSource>(
        random: &mut R,
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    ) -> Self {
        let min_limit_noise = PerlinNoise::create_legacy_for_blended_noise(random, -15, 0);
        let max_limit_noise = PerlinNoise::create_legacy_for_blended_noise(random, -15, 0);
        let main_noise = PerlinNoise::create_legacy_for_blended_noise(random, -7, 0);
        Self {
            min_limit_noise,
            max_limit_noise,
            main_noise,
            xz_multiplier: 684.412 * xz_scale,
            y_multiplier: 684.412 * y_scale,
            xz_factor,
            y_factor,
            smear_scale_multiplier,
        }
    }

    /// Computes the blended noise at a block position.
    #[must_use]
    pub fn compute(&self, block_x: i32, block_y: i32, block_z: i32) -> f64 {
        let limit_x = f64::from(block_x) * self.xz_multiplier;
        let limit_y = f64::from(block_y) * self.y_multiplier;
        let limit_z = f64::from(block_z) * self.xz_multiplier;
        let main_x = limit_x / self.xz_factor;
        let main_y = limit_y / self.y_factor;
        let main_z = limit_z / self.xz_factor;
        let limit_smear = self.y_multiplier * self.smear_scale_multiplier;
        let main_smear = limit_smear / self.y_factor;

        let mut main_noise_value = 0.0;
        for octave in self.main_noise.active_octaves_rev() {
            let pow = octave.reverse_input_factor;
            main_noise_value += octave.noise.noise_scaled_nonzero(
                wrap(main_x * pow),
                wrap(main_y * pow),
                wrap(main_z * pow),
                main_smear * pow,
                main_y * pow,
            ) / pow;
        }

        let factor = (main_noise_value / 10.0 + 1.0) / 2.0;
        let is_max = factor >= 1.0;
        let is_min = factor <= 0.0;

        let mut blend_min = 0.0;
        let mut blend_max = 0.0;
        for octave in self.min_limit_noise.active_octaves_rev() {
            let pow = octave.reverse_input_factor;
            let wx = wrap(limit_x * pow);
            let wy = wrap(limit_y * pow);
            let wz = wrap(limit_z * pow);
            let y_scale_pow = limit_smear * pow;
            if !is_max {
                blend_min += octave.noise.noise_scaled_nonzero(
                    wx,
                    wy,
                    wz,
                    y_scale_pow,
                    limit_y * pow,
                ) / pow;
            }
        }
        for octave in self.max_limit_noise.active_octaves_rev() {
            let pow = octave.reverse_input_factor;
            let wx = wrap(limit_x * pow);
            let wy = wrap(limit_y * pow);
            let wz = wrap(limit_z * pow);
            let y_scale_pow = limit_smear * pow;
            if !is_min {
                blend_max += octave.noise.noise_scaled_nonzero(
                    wx,
                    wy,
                    wz,
                    y_scale_pow,
                    limit_y * pow,
                ) / pow;
            }
        }

        clamped_lerp(factor, blend_min / 512.0, blend_max / 512.0) / 128.0
    }
}

#[cfg(test)]
mod tests {
    use super::{BlendedNoise, wrap};
    use crate::math::clamped_lerp;
    use crate::rng::LegacyRandomSource;

    /// Independent control for the reverse active-level walk. It retains the
    /// original slot counter and `pow` updates so sparse descriptor factors
    /// cannot agree merely because the active sequence is in the same order.
    fn scalar_reference(noise: &BlendedNoise, block_x: i32, block_y: i32, block_z: i32) -> f64 {
        let limit_x = f64::from(block_x) * noise.xz_multiplier;
        let limit_y = f64::from(block_y) * noise.y_multiplier;
        let limit_z = f64::from(block_z) * noise.xz_multiplier;
        let main_x = limit_x / noise.xz_factor;
        let main_y = limit_y / noise.y_factor;
        let main_z = limit_z / noise.xz_factor;
        let limit_smear = noise.y_multiplier * noise.smear_scale_multiplier;
        let main_smear = limit_smear / noise.y_factor;

        let mut main_noise_value = 0.0;
        let mut pow = 1.0;
        for i in 0..8 {
            if let Some(octave) = noise.main_noise.get_octave_noise(i) {
                main_noise_value += octave.noise_scaled_nonzero(
                    wrap(main_x * pow),
                    wrap(main_y * pow),
                    wrap(main_z * pow),
                    main_smear * pow,
                    main_y * pow,
                ) / pow;
            }
            pow /= 2.0;
        }

        let factor = (main_noise_value / 10.0 + 1.0) / 2.0;
        let is_max = factor >= 1.0;
        let is_min = factor <= 0.0;
        let mut blend_min = 0.0;
        let mut blend_max = 0.0;
        pow = 1.0;
        for i in 0..16 {
            let wx = wrap(limit_x * pow);
            let wy = wrap(limit_y * pow);
            let wz = wrap(limit_z * pow);
            let y_scale_pow = limit_smear * pow;
            if !is_max {
                if let Some(octave) = noise.min_limit_noise.get_octave_noise(i) {
                    blend_min += octave.noise_scaled_nonzero(
                        wx,
                        wy,
                        wz,
                        y_scale_pow,
                        limit_y * pow,
                    ) / pow;
                }
            }
            if !is_min {
                if let Some(octave) = noise.max_limit_noise.get_octave_noise(i) {
                    blend_max += octave.noise_scaled_nonzero(
                        wx,
                        wy,
                        wz,
                        y_scale_pow,
                        limit_y * pow,
                    ) / pow;
                }
            }
            pow /= 2.0;
        }
        clamped_lerp(factor, blend_min / 512.0, blend_max / 512.0) / 128.0
    }

    #[test]
    fn reverse_descriptors_match_original_blended_slot_walk() {
        let mut rng = LegacyRandomSource::new(0x51_7A_9D);
        let noise = BlendedNoise::new(&mut rng, 1.0, 1.0, 80.0, 160.0, 0.125);
        let points = [
            (-257, -64, 511),
            (-32_768, 128, -16_385),
            (0, 0, 0),
            (31_999, 255, -4097),
        ];
        for &(x, y, z) in &points {
            let actual = noise.compute(x, y, z);
            let expected = scalar_reference(&noise, x, y, z);
            assert_eq!(actual.to_bits(), expected.to_bits(), "point ({x}, {y}, {z})");
        }
    }
}
