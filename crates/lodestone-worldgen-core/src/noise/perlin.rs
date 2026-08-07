//! `PerlinNoise` — a stack of `ImprovedNoise` octaves.
//!
//! Reproduces `net.minecraft.world.level.levelgen.synth.PerlinNoise` with the
//! `useNewInitialization = true` path used by the modern noise router: each
//! octave is seeded from `positional.fromHashOf("octave_" + octave)`.

use crate::math::lfloor;
use crate::noise::improved::ImprovedNoise;
use crate::rng::{PositionalRandomFactory, RandomSource};

const ROUND_OFF: f64 = 3.355_443_2e7;

/// `PerlinNoise.wrap(x)` — folds large coordinates back toward the origin.
#[must_use]
pub fn wrap(x: f64) -> f64 {
    x - (lfloor(x / ROUND_OFF + 0.5) as f64) * ROUND_OFF
}

/// A stack of improved-noise octaves with per-octave amplitudes.
#[derive(Debug, Clone)]
pub struct PerlinNoise {
    noise_levels: Vec<Option<ImprovedNoise>>,
    amplitudes: Vec<f64>,
    lowest_freq_value_factor: f64,
    lowest_freq_input_factor: f64,
}

impl PerlinNoise {
    /// Builds from an explicit `(first_octave, amplitudes)` pair — the
    /// `NormalNoise.NoiseParameters` shape. Uses new-style positional seeding.
    pub fn create<R: RandomSource>(random: &mut R, first_octave: i32, amplitudes: &[f64]) -> Self {
        Self::new(random, first_octave, amplitudes.to_vec())
    }

    /// Builds from a set of octave indices (vanilla's `create(random, octaves)`),
    /// where every listed octave gets amplitude `1.0`.
    pub fn create_from_octaves<R: RandomSource>(random: &mut R, octaves: &[i32]) -> Self {
        let (first_octave, amplitudes) = make_amplitudes(octaves);
        Self::new(random, first_octave, amplitudes)
    }

    /// `createLegacyForBlendedNoise` — the `useNewInitialization = false` path
    /// over a closed octave range `first..=last`, all amplitudes `1.0`.
    pub fn create_legacy_for_blended_noise<R: RandomSource>(
        random: &mut R,
        first: i32,
        last: i32,
    ) -> Self {
        let octaves: Vec<i32> = (first..=last).collect();
        let (first_octave, amplitudes) = make_amplitudes(&octaves);
        Self::new_legacy(random, first_octave, amplitudes)
    }

    /// `getOctaveNoise(i)` = `noiseLevels[len - 1 - i]`.
    #[must_use]
    pub fn get_octave_noise(&self, i: usize) -> Option<&ImprovedNoise> {
        self.noise_levels[self.noise_levels.len() - 1 - i].as_ref()
    }

    fn new_legacy<R: RandomSource>(
        random: &mut R,
        first_octave: i32,
        amplitudes: Vec<f64>,
    ) -> Self {
        let octaves = amplitudes.len();
        let zero_octave_index = -first_octave;
        let mut noise_levels: Vec<Option<ImprovedNoise>> = (0..octaves).map(|_| None).collect();
        let zero_octave = ImprovedNoise::new(random);
        if zero_octave_index >= 0 && (zero_octave_index as usize) < octaves {
            let zi = zero_octave_index as usize;
            if amplitudes[zi] != 0.0 {
                noise_levels[zi] = Some(zero_octave);
            }
        }
        let mut i = zero_octave_index - 1;
        while i >= 0 {
            let idx = i as usize;
            if idx < octaves {
                if amplitudes[idx] != 0.0 {
                    noise_levels[idx] = Some(ImprovedNoise::new(random));
                } else {
                    random.consume_count(262);
                }
            } else {
                random.consume_count(262);
            }
            i -= 1;
        }
        let lowest_freq_input_factor = crate::math::exp2_exact(-zero_octave_index);
        let lowest_freq_value_factor =
            crate::math::exp2_exact(octaves as i32 - 1)
                / (crate::math::exp2_exact(octaves as i32) - 1.0);
        Self {
            noise_levels,
            amplitudes,
            lowest_freq_value_factor,
            lowest_freq_input_factor,
        }
    }

    fn new<R: RandomSource>(random: &mut R, first_octave: i32, amplitudes: Vec<f64>) -> Self {
        let octaves = amplitudes.len();
        let zero_octave_index = -first_octave;
        let mut noise_levels: Vec<Option<ImprovedNoise>> = Vec::with_capacity(octaves);
        let positional = random.fork_positional();
        for (i, amp) in amplitudes.iter().enumerate() {
            if *amp != 0.0 {
                let octave = first_octave + i as i32;
                let mut octave_rng = positional.from_hash_of(&format!("octave_{octave}"));
                noise_levels.push(Some(ImprovedNoise::new(&mut octave_rng)));
            } else {
                noise_levels.push(None);
            }
        }
        let lowest_freq_input_factor = crate::math::exp2_exact(-zero_octave_index);
        let lowest_freq_value_factor =
            crate::math::exp2_exact(octaves as i32 - 1)
                / (crate::math::exp2_exact(octaves as i32) - 1.0);
        Self {
            noise_levels,
            amplitudes,
            lowest_freq_value_factor,
            lowest_freq_input_factor,
        }
    }

    /// Samples the octave stack at `(x, y, z)`.
    #[must_use]
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        let mut value = 0.0;
        let mut factor = self.lowest_freq_input_factor;
        let mut value_factor = self.lowest_freq_value_factor;
        for (i, level) in self.noise_levels.iter().enumerate() {
            if let Some(noise) = level {
                let noise_val = noise.noise(wrap(x * factor), wrap(y * factor), wrap(z * factor));
                value += self.amplitudes[i] * noise_val * value_factor;
            }
            factor *= 2.0;
            value_factor /= 2.0;
        }
        value
    }
}

/// `PerlinNoise.makeAmplitudes` — turns a sorted octave set into a
/// `(first_octave, amplitudes)` pair.
fn make_amplitudes(octave_set: &[i32]) -> (i32, Vec<f64>) {
    assert!(!octave_set.is_empty(), "Need some octaves!");
    let mut sorted: Vec<i32> = octave_set.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let low_freq_octaves = -sorted[0];
    let high_freq_octaves = *sorted.last().unwrap();
    let octaves = low_freq_octaves + high_freq_octaves + 1;
    assert!(octaves >= 1, "Total number of octaves needs to be >= 1");
    let mut amplitudes = vec![0.0; octaves as usize];
    for octave in sorted {
        amplitudes[(octave + low_freq_octaves) as usize] = 1.0;
    }
    (-low_freq_octaves, amplitudes)
}
