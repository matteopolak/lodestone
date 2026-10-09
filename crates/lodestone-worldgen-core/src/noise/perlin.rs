//! `PerlinNoise` — a stack of `ImprovedNoise` octaves.
//!
//! Reproduces the reference octave stack's positional seeding: each octave is
//! seeded from the source's positional fork hashed with `"octave_{octave}"`.
//! The legacy sequential-seeding arm is not implemented; nothing here needs it.

use crate::math::lfloor;
use crate::noise::improved::ImprovedNoise;
use crate::rng::{PositionalRandomFactory, RandomSource};

const ROUND_OFF: f64 = 3.355_443_2e7;

/// Folds a large coordinate back toward the origin, one `ROUND_OFF` period at a time.
#[must_use]
pub fn wrap(x: f64) -> f64 {
    x - (lfloor(x / ROUND_OFF + 0.5) as f64) * ROUND_OFF
}

/// A stack of improved-noise octaves with per-octave amplitudes.
#[derive(Debug, Clone)]
pub struct PerlinNoise {
    /// Slots in the amplitude list, zero amplitudes included.
    octave_count: usize,
    /// Non-zero octaves in slot order (lowest frequency first). Every entry
    /// carries the factors it had at its slot in the full walk.
    active_levels: Vec<ActiveOctave>,
}

/// One non-zero octave with the factors of its original slot walk precomputed.
#[derive(Debug, Clone)]
struct ActiveOctave {
    noise: ImprovedNoise,
    amplitude: f64,
    input_factor: f64,
    value_factor: f64,
}

impl PerlinNoise {
    /// Builds from an explicit `(first_octave, amplitudes)` pair, the shape a
    /// datapack noise definition carries.
    pub fn create<R: RandomSource>(random: &mut R, first_octave: i32, amplitudes: &[f64]) -> Self {
        let octaves = amplitudes.len();
        let positional = random.fork_positional();
        let mut input_factor = crate::math::exp2_exact(first_octave);
        let mut value_factor = crate::math::exp2_exact(octaves as i32 - 1)
            / (crate::math::exp2_exact(octaves as i32) - 1.0);
        let mut active_levels = Vec::with_capacity(octaves);
        for (i, &amplitude) in amplitudes.iter().enumerate() {
            if amplitude != 0.0 {
                let octave = first_octave + i as i32;
                let mut octave_rng = positional.from_hash_of(&format!("octave_{octave}"));
                active_levels.push(ActiveOctave {
                    noise: ImprovedNoise::new(&mut octave_rng),
                    amplitude,
                    input_factor,
                    value_factor,
                });
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
        Self { octave_count: octaves, active_levels }
    }

    /// Samples the octave stack at `(x, y, z)`.
    #[must_use]
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        crate::counters::bump_noise_active_visits(self.active_levels.len() as u64);
        crate::counters::bump_noise_skipped_visits(
            (self.octave_count - self.active_levels.len()) as u64,
        );
        let mut value = 0.0;
        for level in &self.active_levels {
            let noise_val = level.noise.noise(
                wrap(x * level.input_factor),
                wrap(y * level.input_factor),
                wrap(z * level.input_factor),
            );
            value += level.amplitude * noise_val * level.value_factor;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::PerlinNoise;
    use crate::rng::XoroshiroRandomSource;

    /// Independent slot walk used to control the packed active-level loop.
    /// It derives both starting factors from the amplitude list itself and
    /// advances them for every slot, including an absent one, so a descriptor
    /// carrying the preceding factor cannot pass by merely preserving order.
    fn scalar_reference(
        noise: &PerlinNoise, first_octave: i32, amplitudes: &[f64], x: f64, y: f64, z: f64,
    ) -> f64 {
        let n = amplitudes.len() as i32;
        let mut input_factor = 2f64.powi(first_octave);
        let mut value_factor = 2f64.powi(n - 1) / (2f64.powi(n) - 1.0);
        let mut active = noise.active_levels.iter();
        let mut value = 0.0;
        for &amplitude in amplitudes {
            if amplitude != 0.0 {
                let level = active.next().expect("one active level per non-zero amplitude");
                let sampled = level.noise.noise(
                    super::wrap(x * input_factor),
                    super::wrap(y * input_factor),
                    super::wrap(z * input_factor),
                );
                value += amplitude * sampled * value_factor;
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
        assert!(active.next().is_none(), "more active levels than non-zero amplitudes");
        value
    }

    fn coordinates() -> impl Iterator<Item = (f64, f64, f64)> {
        // Includes negative/fractional values and coordinates just beyond
        // multiple wrap periods. The generator is deterministic and avoids a
        // round-number-only control that can collapse interpolation factors.
        let mut state = 0xD1B5_4A32_19C7_EF03_u64;
        (0..512).map(move |_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let unit = |bits: u64| (bits as f64) / (u64::MAX as f64);
            let x = (unit(state) * 2.0 - 1.0) * 3.355_443_2e7 + 0.375;
            state = state.rotate_left(17);
            let y = (unit(state) * 2.0 - 1.0) * 2048.0 + 0.125;
            state = state.rotate_left(29);
            let z = (unit(state) * 2.0 - 1.0) * 3.355_443_2e7 - 0.625;
            (x, y, z)
        })
    }

    #[test]
    fn active_descriptors_match_scalar_slot_walk_for_sparse_and_zero_amplitudes() {
        let cases: &[(&[f64], i32, i64)] = &[
            (&[0.0, 1.0, 0.0, -2.5, 0.0, 0.75], -7, 42),
            (&[1.0, 0.0, 0.0, 0.0, 3.0, 0.0, -0.5], -9, -1234),
            (&[0.0, -0.0, 0.0, 0.0], -3, 7),
            (&[0.0, 0.0, 0.0, 0.0, 0.0], 2, 99),
        ];
        for &(amplitudes, first_octave, seed) in cases {
            let mut rng = XoroshiroRandomSource::new(seed);
            let noise = PerlinNoise::create(&mut rng, first_octave, amplitudes);
            for (index, (x, y, z)) in coordinates().enumerate() {
                let expected = scalar_reference(&noise, first_octave, amplitudes, x, y, z);
                let actual = noise.get_value(x, y, z);
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "sparse slot walk diverged at case {first_octave}/{seed}, sample {index}"
                );
            }
        }
    }

    #[test]
    fn seeded_construction_keeps_sample_bits_stable() {
        let amplitudes = [1.5, 0.0, -2.0, 0.0, 0.25, 0.0, 0.0, 3.0];
        let mut first_rng = XoroshiroRandomSource::new(0x1234_5678);
        let mut second_rng = XoroshiroRandomSource::new(0x1234_5678);
        let first = PerlinNoise::create(&mut first_rng, -11, &amplitudes);
        let second = PerlinNoise::create(&mut second_rng, -11, &amplitudes);
        for (x, y, z) in coordinates().take(128) {
            assert_eq!(
                first.get_value(x, y, z).to_bits(),
                second.get_value(x, y, z).to_bits()
            );
        }
    }
}
