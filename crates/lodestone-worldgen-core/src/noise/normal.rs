//! `NormalNoise` — two `PerlinNoise` stacks combined into the normalised noise
//! the density-function system consumes.
//!
//! Reproduces `net.minecraft.world.level.levelgen.synth.NormalNoise` (new-init
//! path): two Perlin stacks built back-to-back from the same source, the second
//! sampled at a slightly offset frequency, scaled by a deviation-based factor.

use crate::noise::perlin::PerlinNoise;
use crate::rng::RandomSource;

const INPUT_FACTOR: f64 = 1.018_126_888_217_522_7;

/// Normalised noise: `(first + second(scaled)) * valueFactor`.
#[derive(Debug, Clone)]
pub struct NormalNoise {
    first: PerlinNoise,
    second: PerlinNoise,
    value_factor: f64,
}

impl NormalNoise {
    /// Builds from `(first_octave, amplitudes)` — the `NoiseParameters` shape
    /// used by every named noise in the router.
    pub fn create<R: RandomSource>(random: &mut R, first_octave: i32, amplitudes: &[f64]) -> Self {
        let first = PerlinNoise::create(random, first_octave, amplitudes);
        let second = PerlinNoise::create(random, first_octave, amplitudes);

        Self {
            first,
            second,
            value_factor: value_factor(amplitudes),
        }
    }

    /// `NormalNoise.createLegacyNetherBiome` — the `useNewInitialization = false`
    /// arm (`NormalNoise.java:26-28`, `:41-47`).
    ///
    /// The **only** two noises in the game that take it are
    /// `minecraft:nether/temperature` and `minecraft:nether/vegetation`, and
    /// `RandomState`'s `NoiseWiringHelper.visitNoise` special-cases them by *id*
    /// — not by the dimension's `legacy_random_source` flag — seeding them from
    /// `new LegacyRandomSource(worldSeed + 0)` and `(worldSeed + 1)`
    /// respectively, on the **raw world seed** rather than a positional fork.
    /// Since the Nether router zeroes every other climate channel, these two
    /// noises *are* the Nether's biome map, so this path is not an edge case.
    pub fn create_legacy_nether_biome<R: RandomSource>(
        random: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        let first =
            PerlinNoise::create_legacy_for_legacy_nether_biome(random, first_octave, amplitudes);
        let second =
            PerlinNoise::create_legacy_for_legacy_nether_biome(random, first_octave, amplitudes);
        Self {
            first,
            second,
            value_factor: value_factor(amplitudes),
        }
    }

    /// Samples the normalised noise at `(x, y, z)`.
    #[must_use]
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        let x2 = x * INPUT_FACTOR;
        let y2 = y * INPUT_FACTOR;
        let z2 = z * INPUT_FACTOR;
        (self.first.get_value(x, y, z) + self.second.get_value(x2, y2, z2)) * self.value_factor
    }
}

fn expected_deviation(octave_span: i32) -> f64 {
    0.1 * (1.0 + 1.0 / f64::from(octave_span + 1))
}

/// `0.16666666666666666 / expectedDeviation(maxOctave - minOctave)`, over the
/// indices of the non-zero amplitudes. Shared by both constructor arms — the
/// `useNewInitialization` flag changes how the two Perlin stacks are *seeded*,
/// never this scale factor.
fn value_factor(amplitudes: &[f64]) -> f64 {
    let mut min_octave = i32::MAX;
    let mut max_octave = i32::MIN;
    for (i, amp) in amplitudes.iter().enumerate() {
        if *amp != 0.0 {
            min_octave = min_octave.min(i as i32);
            max_octave = max_octave.max(i as i32);
        }
    }
    0.166_666_666_666_666_66 / expected_deviation(max_octave - min_octave)
}
