//! `PerlinNoise` — a stack of `ImprovedNoise` octaves.
//!
//! Reproduces vanilla's own Perlin-noise class with the
//! `useNewInitialization = true` path used by the modern noise router: each
//! octave is seeded from `positional.fromHashOf("octave_" + octave)`.

use crate::math::lfloor;
use crate::noise::improved::ImprovedNoise;
use crate::rng::{PositionalRandomFactory, RandomSource};

const ROUND_OFF: f64 = 3.355_443_2e7;

/// Vanilla's own Perlin-noise wrap at `(x)` — folds large coordinates back toward the origin.
#[must_use]
pub fn wrap(x: f64) -> f64 {
    x - (lfloor(x / ROUND_OFF + 0.5) as f64) * ROUND_OFF
}

/// A stack of improved-noise octaves with per-octave amplitudes.
#[derive(Debug, Clone)]
pub struct PerlinNoise {
    /// Slot-to-active index map retained for exact signatures and the
    /// reverse-index compatibility accessor. Sampling never walks this map.
    level_indices: Vec<Option<usize>>,
    /// Active levels in original (lowest-to-highest slot) order. Every entry
    /// carries the factors it had at its slot in the full walk.
    active_levels: Vec<ActiveOctave>,
    amplitudes: Vec<f64>,
    lowest_freq_value_factor: f64,
    lowest_freq_input_factor: f64,
}

/// One non-zero octave with the factors of its original slot walk precomputed.
#[derive(Debug, Clone)]
pub(crate) struct ActiveOctave {
    pub(crate) noise: ImprovedNoise,
    pub(crate) amplitude: f64,
    pub(crate) input_factor: f64,
    pub(crate) value_factor: f64,
    /// The `pow` value used by the reverse octave walk in `BlendedNoise`.
    pub(crate) reverse_input_factor: f64,
}

impl PerlinNoise {
    /// Appends a complete, bit-exact description of this stack to `out` — see
    /// [`ImprovedNoise::write_signature`] for the contract and the two traps.
    ///
    /// Lengths are written before their contents so that no two differently
    /// shaped stacks can produce the same word sequence by concatenation
    /// (`[a] ++ [b, c]` against `[a, b] ++ [c]`), and a `None` octave writes a
    /// discriminant word so an absent level cannot alias a present one.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        out.push(self.level_indices.len() as u64);
        for level in &self.level_indices {
            match level {
                Some(index) => {
                    out.push(1);
                    self.active_levels[*index].noise.write_signature(out);
                }
                None => {
                    out.push(0);
                }
            }
        }
        out.push(self.amplitudes.len() as u64);
        out.extend(self.amplitudes.iter().map(|a| a.to_bits()));
        out.push(self.lowest_freq_value_factor.to_bits());
        out.push(self.lowest_freq_input_factor.to_bits());
    }

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

    /// Vanilla's own "create legacy for blended noise" constructor — the
    /// "use new initialization = false" path
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

    /// Vanilla's own "create legacy for legacy nether biome" constructor —
    /// the "use new initialization = false"
    /// path over an explicit `(first_octave, amplitudes)` pair.
    ///
    /// Same constructor arm as [`Self::create_legacy_for_blended_noise`]; the
    /// difference is only that the octave set arrives as amplitudes rather than
    /// as a range (vanilla's own Perlin-noise constructor). Reached from
    /// [`crate::noise::NormalNoise::create_legacy_nether_biome`], which is the
    /// only caller vanilla has.
    ///
    /// **The draw count is not the octave count.** For the two Nether noises
    /// (`first_octave -7`, amplitudes `[1.0, 1.0]`) vanilla's own "zero octave
    /// index" is 7 while
    /// there are only 2 octaves, so vanilla builds an `ImprovedNoise` for the
    /// zero octave and **throws it away**, then skips an octave (262 discarded
    /// `nextInt`s) five times before building the two it keeps. Getting that
    /// wrong yields a plausible Nether that is not vanilla's.
    pub fn create_legacy_for_legacy_nether_biome<R: RandomSource>(
        random: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        Self::new_legacy(random, first_octave, amplitudes.to_vec())
    }

    /// `getOctaveNoise(i)` = `noiseLevels[len - 1 - i]`.
    #[must_use]
    pub fn get_octave_noise(&self, i: usize) -> Option<&ImprovedNoise> {
        let slot = self.level_indices.len() - 1 - i;
        self.level_indices[slot].map(|index| &self.active_levels[index].noise)
    }

    /// Returns active levels in the reverse slot order used by blended noise.
    #[inline]
    pub(crate) fn active_octaves_rev(&self) -> impl DoubleEndedIterator<Item = &ActiveOctave> {
        self.active_levels.iter().rev()
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
        Self::pack(noise_levels, amplitudes, lowest_freq_input_factor, lowest_freq_value_factor)
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
        Self::pack(noise_levels, amplitudes, lowest_freq_input_factor, lowest_freq_value_factor)
    }

    fn pack(
        noise_levels: Vec<Option<ImprovedNoise>>,
        amplitudes: Vec<f64>,
        mut input_factor: f64,
        mut value_factor: f64,
    ) -> Self {
        let lowest_freq_input_factor = input_factor;
        let lowest_freq_value_factor = value_factor;
        let mut reverse_factors = vec![0.0; amplitudes.len()];
        let mut reverse_input_factor = 1.0;
        for slot in (0..amplitudes.len()).rev() {
            reverse_factors[slot] = reverse_input_factor;
            reverse_input_factor /= 2.0;
        }

        let mut level_indices = vec![None; amplitudes.len()];
        let mut active_levels = Vec::with_capacity(noise_levels.iter().flatten().count());
        for (slot, (noise, &amplitude)) in noise_levels.into_iter().zip(&amplitudes).enumerate() {
            if let Some(noise) = noise {
                let index = active_levels.len();
                level_indices[slot] = Some(index);
                active_levels.push(ActiveOctave {
                    noise,
                    amplitude,
                    input_factor,
                    value_factor,
                    reverse_input_factor: reverse_factors[slot],
                });
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
        Self {
            level_indices,
            active_levels,
            amplitudes,
            lowest_freq_value_factor,
            lowest_freq_input_factor,
        }
    }

    /// Samples the octave stack at `(x, y, z)`.
    #[must_use]
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        crate::counters::bump_noise_active_visits(self.active_levels.len() as u64);
        crate::counters::bump_noise_skipped_visits(
            (self.level_indices.len() - self.active_levels.len()) as u64,
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

/// Vanilla's own Perlin-noise make-amplitudes — turns a sorted octave set into a
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

#[cfg(test)]
mod tests {
    use super::PerlinNoise;
    use crate::rng::XoroshiroRandomSource;

    /// Independent slot walk used to control the packed active-level loop.
    /// It deliberately advances both factors for every slot, including an
    /// absent one, so a descriptor carrying the preceding factor cannot pass
    /// this test by merely preserving active order.
    fn scalar_reference(noise: &PerlinNoise, x: f64, y: f64, z: f64) -> f64 {
        let mut value = 0.0;
        let mut input_factor = noise.lowest_freq_input_factor;
        let mut value_factor = noise.lowest_freq_value_factor;
        for (slot, active_index) in noise.level_indices.iter().enumerate() {
            if let Some(active_index) = active_index {
                let level = &noise.active_levels[*active_index];
                let sampled = level.noise.noise(
                    super::wrap(x * input_factor),
                    super::wrap(y * input_factor),
                    super::wrap(z * input_factor),
                );
                value += noise.amplitudes[slot] * sampled * value_factor;
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
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
                let expected = scalar_reference(&noise, x, y, z);
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
    fn seeded_construction_keeps_signature_and_sample_bits_stable() {
        let amplitudes = [1.5, 0.0, -2.0, 0.0, 0.25, 0.0, 0.0, 3.0];
        let mut first_rng = XoroshiroRandomSource::new(0x1234_5678);
        let mut second_rng = XoroshiroRandomSource::new(0x1234_5678);
        let first = PerlinNoise::create(&mut first_rng, -11, &amplitudes);
        let second = PerlinNoise::create(&mut second_rng, -11, &amplitudes);
        let mut first_signature = Vec::new();
        let mut second_signature = Vec::new();
        first.write_signature(&mut first_signature);
        second.write_signature(&mut second_signature);
        assert_eq!(first_signature, second_signature);
        for (x, y, z) in coordinates().take(128) {
            assert_eq!(
                first.get_value(x, y, z).to_bits(),
                second.get_value(x, y, z).to_bits()
            );
        }
    }
}
