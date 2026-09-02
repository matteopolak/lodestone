//! `PerlinSimplexNoise` — vanilla's own multi-octave wrapper class of the same
//! name, over
//! [`super::simplex::SimplexNoise`], plus the two `Biome` climate noise fields
//! `freeze_top_layer` reads.
//!
//! [`super::simplex`] already ports the *single-octave* reduction of this class
//! (vanilla's own biome-info-noise constant, octave set `[0]`, where the frequency and value
//! factors both collapse to 1). That reduction is not enough for
//! vanilla's own FROZEN temperature-modifier variant, whose `FROZEN_TEMPERATURE_NOISE` is
//! vanilla's own construction seeded from a legacy random source with seed
//! `3456L` and octave set `[-2, -1, 0]` — three octaves, with
//! per-octave input and value factors. So this module ports the real wrapper.
//!
//! # Scope: `highFreqOctaves == 0` only
//!
//! `PerlinSimplexNoise`'s constructor has a second half
//! in vanilla that only runs when the octave set contains
//! a **positive** octave: it derives a second seed from
//! `zeroOctave.getValue(zeroOctave.xo, zeroOctave.yo, zeroOctave.zo)` and builds
//! the high-frequency levels from it. Both climate fields in `Biome` have
//! `octaveSet.lastInt() == 0`, so that branch is unreachable for every caller
//! this engine has — and reaching it would additionally require
//! [`super::simplex::SimplexNoise`] to *retain* the `xo`/`yo`/`zo` offsets it
//! currently discards (correctly: vanilla's own "use noise start" flag is
//! `false` at every call site).
//! [`PerlinSimplexNoise::new`] therefore **panics** on a positive octave rather
//! than silently producing a wrong field, which is the difference between a port
//! with a named boundary and a port that is quietly wrong outside its tested
//! range.
//!
//! # Why the noise fields are built once, not per call
//!
//! [`super::simplex::biome_info_noise_value`] constructs a fresh
//! `SimplexNoise` on every call — value-identical (it is a pure function of a
//! fixed seed) but ~260 RNG draws each time. `freeze_top_layer` asks for a
//! temperature up to twice per column, 256 columns per chunk, so that shape
//! would cost ~130k draws per chunk for data that never changes. [`ClimateNoise`]
//! holds all three fields and is built once per generator instead.

use crate::rng::{LegacyRandomSource, RandomSource};

use super::simplex::SimplexNoise;

/// Vanilla's own `PerlinSimplexNoise` class, restricted to
/// octave sets whose largest octave is `0` (see the module doc).
#[derive(Debug, Clone)]
pub struct PerlinSimplexNoise {
    /// Vanilla's own noise-levels field, index 0 = the octave-`0` level.
    /// `None` for an octave the
    /// set omits (vanilla's own 262-draw consume-count skip — no caller needs it yet,
    /// but the shape is kept so the draw order is vanilla's).
    levels: Vec<Option<SimplexNoise>>,
    highest_freq_input_factor: f64,
    highest_freq_value_factor: f64,
}

impl PerlinSimplexNoise {
    /// `new PerlinSimplexNoise(random, octaveSet)`.
    ///
    /// `octaves` is vanilla's `List<Integer>`, in any order — it is loaded into
    /// an `IntRBTreeSet` (sorted, de-duplicated) before use, which this mirrors.
    ///
    /// # Panics
    /// Panics if `octaves` is empty, or if its largest entry is positive (see
    /// the module doc's scope section).
    #[must_use]
    pub fn new<R: RandomSource>(random: &mut R, octaves: &[i32]) -> Self {
        let mut sorted: Vec<i32> = octaves.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert!(!sorted.is_empty(), "Need some octaves!");
        let low_freq_octaves = -sorted[0];
        let high_freq_octaves = sorted[sorted.len() - 1];
        assert!(
            high_freq_octaves <= 0,
            "PerlinSimplexNoise: positive octaves are outside this port's scope \
             (would need SimplexNoise's xo/yo/zo offsets); got {sorted:?}"
        );
        let octave_count = low_freq_octaves + high_freq_octaves + 1;
        assert!(
            octave_count >= 1,
            "Total number of octaves needs to be >= 1"
        );

        // `zeroOctaveIndex = highFreqOctaves`, which this scope pins to 0.
        let zero_octave_index = high_freq_octaves;
        let mut levels: Vec<Option<SimplexNoise>> = Vec::with_capacity(octave_count as usize);
        // The zero octave is drawn FIRST, before any of the loop's levels —
        // vanilla constructs it unconditionally on line 32 and only then decides
        // whether the set actually contains 0.
        let zero_octave = SimplexNoise::new(random);
        for _ in 0..octave_count {
            levels.push(None);
        }
        if zero_octave_index >= 0 && zero_octave_index < octave_count && sorted.contains(&0) {
            levels[zero_octave_index as usize] = Some(zero_octave);
        }
        for i in (zero_octave_index + 1)..octave_count {
            if i >= 0 && sorted.contains(&(zero_octave_index - i)) {
                levels[i as usize] = Some(SimplexNoise::new(random));
            } else {
                // vanilla: `random.consumeCount(262)`. Unreached by every octave
                // set this engine constructs (both are contiguous), so rather
                // than add a `consume_count` to every `RandomSource`, this is a
                // hard stop — a silently-skipped draw budget would desynchronise
                // every later octave.
                panic!(
                    "PerlinSimplexNoise: non-contiguous octave set {sorted:?} needs \
                     RandomSource::consume_count(262), which this port does not implement"
                );
            }
        }

        Self {
            levels,
            highest_freq_input_factor: crate::math::exp2_exact(high_freq_octaves),
            highest_freq_value_factor: 1.0 / (crate::math::exp2_exact(octave_count) - 1.0),
        }
    }

    /// `getValue(x, y, useNoiseStart = false)`.
    ///
    /// Only the `useNoiseStart = false` overload is ported: both `Biome` climate
    /// fields pass `false`, and `true` would need the per-level `xo`/`yo`
    /// offsets [`SimplexNoise`] discards.
    #[must_use]
    pub fn get_value(&self, x: f64, y: f64) -> f64 {
        let mut value = 0.0;
        let mut factor = self.highest_freq_input_factor;
        let mut value_factor = self.highest_freq_value_factor;
        for level in &self.levels {
            if let Some(noise) = level {
                value += noise.get_value(x * factor, y * factor) * value_factor;
            }
            factor /= 2.0;
            value_factor *= 2.0;
        }
        value
    }
}

/// The three `Biome` noise fields the `freeze_top_layer` temperature path reads,
/// built once (see the module doc for why not per call).
///
/// Vanilla holds all three as `static final` singletons on its own biome
/// class; each is a pure function of a fixed seed, so building
/// them per generator is value-identical and keeps this engine free of shared
/// mutable state.
#[derive(Debug, Clone)]
pub struct ClimateNoise {
    /// `Biome.TEMPERATURE_NOISE` — `LegacyRandomSource(1234L)`, octaves `[0]`.
    /// Read by vanilla's own height-adjusted-temperature accessor above `seaLevel + 17`.
    temperature: PerlinSimplexNoise,
    /// `Biome.FROZEN_TEMPERATURE_NOISE` — `LegacyRandomSource(3456L)`, octaves
    /// `[-2, -1, 0]`. Read only by vanilla's own FROZEN temperature-modifier
    /// variant, i.e. only in `frozen_ocean`/`deep_frozen_ocean`.
    frozen_temperature: PerlinSimplexNoise,
    /// `Biome.BIOME_INFO_NOISE` — `LegacyRandomSource(2345L)`, octaves `[0]`.
    /// Read twice by vanilla's own FROZEN temperature-modifier variant. This
    /// is the same field [`super::simplex::biome_info_noise_value`] exposes as a
    /// free function for `crate::feature::vegetation`; held here as an instance
    /// so the frozen path pays for it once rather than per sample.
    biome_info: PerlinSimplexNoise,
}

impl ClimateNoise {
    /// Builds all three fields. Cheap enough to call once per generator
    /// (~780 RNG draws total), never per column.
    #[must_use]
    pub fn new() -> Self {
        Self {
            temperature: PerlinSimplexNoise::new(&mut LegacyRandomSource::new(1234), &[0]),
            frozen_temperature: PerlinSimplexNoise::new(
                &mut LegacyRandomSource::new(3456),
                &[-2, -1, 0],
            ),
            biome_info: PerlinSimplexNoise::new(&mut LegacyRandomSource::new(2345), &[0]),
        }
    }

    /// `Biome.TEMPERATURE_NOISE.getValue(x, z, false)`.
    #[must_use]
    pub fn temperature(&self, x: f64, z: f64) -> f64 {
        self.temperature.get_value(x, z)
    }

    /// `Biome.FROZEN_TEMPERATURE_NOISE.getValue(x, z, false)`.
    #[must_use]
    pub fn frozen_temperature(&self, x: f64, z: f64) -> f64 {
        self.frozen_temperature.get_value(x, z)
    }

    /// `Biome.BIOME_INFO_NOISE.getValue(x, z, false)`.
    #[must_use]
    pub fn biome_info(&self, x: f64, z: f64) -> f64 {
        self.biome_info.get_value(x, z)
    }
}

impl Default for ClimateNoise {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-octave case must be **bit-identical** to the reduction
    /// [`super::super::simplex`] already ports and that `vegetation_parity`
    /// already validates against a real JVM dump. That makes this wrapper's
    /// scaling arithmetic checked against existing external evidence rather than
    /// against itself: if vanilla's own highest-frequency input/value factor
    /// fields were
    /// transcribed wrongly, the `[0]` case would stop agreeing.
    #[test]
    fn single_octave_matches_the_already_validated_simplex_reduction() {
        let wrapper = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(2345), &[0]);
        for i in -25..25 {
            let x = f64::from(i) * 13.7;
            let z = f64::from(i) * -8.3;
            let expected = crate::noise::simplex::biome_info_noise_value(x, z);
            assert_eq!(
                wrapper.get_value(x, z).to_bits(),
                expected.to_bits(),
                "single-octave wrapper diverged from the validated reduction at ({x}, {z})"
            );
        }
    }

    /// The three-octave field's factors, checked against the closed form rather
    /// than against the implementation: `[-2,-1,0]` gives
    /// `octaves = 3`, `highestFreqInputFactor = 2^0 = 1`,
    /// `highestFreqValueFactor = 1/(2^3-1) = 1/7`, so the accumulated value is
    /// `n0(x)/7 + n1(x/2)*2/7 + n2(x/4)*4/7` and its range is bounded by
    /// `(1+2+4)/7 = 1`.
    #[test]
    fn three_octave_field_is_bounded_by_its_value_factors() {
        let noise = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(3456), &[-2, -1, 0]);
        assert_eq!(noise.levels.len(), 3, "three octaves");
        assert!(
            noise.levels.iter().all(Option::is_some),
            "a contiguous octave set fills every level"
        );
        assert_eq!(noise.highest_freq_input_factor.to_bits(), 1.0_f64.to_bits());
        assert_eq!(
            noise.highest_freq_value_factor.to_bits(),
            (1.0_f64 / 7.0).to_bits()
        );
        for i in -40..40 {
            let v = noise.get_value(f64::from(i) * 0.05, f64::from(i) * -0.05);
            assert!((-1.0..=1.0).contains(&v), "out of range at {i}: {v}");
        }
    }

    /// A three-octave field must NOT equal its own octave-0 level: that is the
    /// control proving the extra levels are actually summed in, and it is the
    /// failure mode a copy-paste of the single-octave reduction would have.
    #[test]
    fn three_octave_field_differs_from_its_zero_octave_alone() {
        let three = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(3456), &[-2, -1, 0]);
        let one = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(3456), &[0]);
        let mut differences = 0;
        let mut coinciding = Vec::new();
        for i in -40..40 {
            let x = f64::from(i) * 0.05;
            let z = f64::from(i) * -0.05;
            if three.get_value(x, z).to_bits() == one.get_value(x, z).to_bits() {
                coinciding.push(i);
            } else {
                differences += 1;
            }
        }
        // The origin is the one input where the two CANNOT differ, and it is a
        // property of simplex noise rather than of this wrapper: at `(0, 0)` the
        // input scaling `x * factor` is the identity for every octave, so all
        // three levels sample the same point — and `SimplexNoise::get_value`
        // there reduces to gradient dot products against a zero displacement
        // vector for the first corner. Excluding it, every sample must differ.
        assert_eq!(
            coinciding,
            vec![0],
            "the only input where a three-octave field may match its zero octave alone is the \
             origin; anything else means the loop is summing one level"
        );
        assert_eq!(differences, 79, "79 of the 80 sampled inputs differ");
    }

    #[test]
    fn climate_noise_is_deterministic_across_independent_constructions() {
        let a = ClimateNoise::new();
        let b = ClimateNoise::new();
        for i in -10..10 {
            let x = f64::from(i) * 7.5;
            let z = f64::from(i) * 3.25;
            assert_eq!(a.temperature(x, z).to_bits(), b.temperature(x, z).to_bits());
            assert_eq!(
                a.frozen_temperature(x, z).to_bits(),
                b.frozen_temperature(x, z).to_bits()
            );
            assert_eq!(a.biome_info(x, z).to_bits(), b.biome_info(x, z).to_bits());
        }
    }

    /// The three fields must be genuinely different noise, not three views of
    /// one seed — a copy-paste in [`ClimateNoise::new`] is otherwise invisible.
    #[test]
    fn the_three_climate_fields_are_distinct() {
        let n = ClimateNoise::new();
        let (x, z) = (123.5, -77.25);
        assert_ne!(n.temperature(x, z).to_bits(), n.biome_info(x, z).to_bits());
        assert_ne!(
            n.temperature(x, z).to_bits(),
            n.frozen_temperature(x, z).to_bits()
        );
        assert_ne!(
            n.biome_info(x, z).to_bits(),
            n.frozen_temperature(x, z).to_bits()
        );
    }

    #[test]
    #[should_panic(expected = "positive octaves are outside this port's scope")]
    fn positive_octaves_are_rejected_rather_than_silently_wrong() {
        let _ = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(1), &[0, 1]);
    }

    #[test]
    #[should_panic(expected = "non-contiguous octave set")]
    fn non_contiguous_octaves_are_rejected() {
        let _ = PerlinSimplexNoise::new(&mut LegacyRandomSource::new(1), &[-2, 0]);
    }
}
