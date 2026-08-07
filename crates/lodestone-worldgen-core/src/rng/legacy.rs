//! `LegacyRandomSource` — the `java.util.Random` linear congruential generator.

use super::gaussian::Gaussian;
use super::{PositionalRandomFactory, RandomSource, get_seed};
use crate::hash::java_string_hash;

const MULTIPLIER: i64 = 0x5_DEEC_E66D;
const INCREMENT: i64 = 0xB;
const MASK48: i64 = (1 << 48) - 1;
const FLOAT_MULTIPLIER: f32 = 5.960_464_5e-8;
// Vanilla's `DOUBLE_MULTIPLIER` is a *float* literal promoted to double.
const DOUBLE_MULTIPLIER: f64 = 1.110_223e-16_f32 as f64;

/// A `java.util.Random`-compatible LCG.
#[derive(Debug, Clone)]
pub struct LegacyRandomSource {
    seed: i64,
    gaussian: Gaussian,
}

impl LegacyRandomSource {
    /// Creates a generator seeded exactly as `new java.util.Random(seed)`.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        let mut s = Self {
            seed: 0,
            gaussian: Gaussian::default(),
        };
        s.set_seed(seed);
        s
    }

    /// `next(bits)` — advances the LCG and returns the top `bits` bits.
    fn next(&mut self, bits: i32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT) & MASK48;
        // seed is a non-negative 48-bit value; the arithmetic shift is safe.
        (self.seed >> (48 - bits)) as i32
    }
}

impl RandomSource for LegacyRandomSource {
    type Positional = LegacyPositionalFactory;

    fn fork_positional(&mut self) -> LegacyPositionalFactory {
        LegacyPositionalFactory {
            seed: self.next_long(),
        }
    }

    fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULTIPLIER) & MASK48;
        self.gaussian.reset();
    }

    fn next_bits(&mut self, bits: u32) -> i32 {
        self.next(bits as i32)
    }

    fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            // Power of two.
            return ((i64::from(bound).wrapping_mul(i64::from(self.next(31)))) >> 31) as i32;
        }
        loop {
            let sample = self.next(31);
            let modulo = sample % bound;
            // Java detects overflow of `sample - modulo + (bound-1)` via `< 0`.
            if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
                return modulo;
            }
        }
    }

    fn next_long(&mut self) -> i64 {
        let upper = self.next(32);
        let lower = self.next(32);
        (i64::from(upper) << 32).wrapping_add(i64::from(lower))
    }

    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }

    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * FLOAT_MULTIPLIER
    }

    fn next_double(&mut self) -> f64 {
        let upper = self.next(26);
        let lower = self.next(27);
        let combined = (i64::from(upper) << 27).wrapping_add(i64::from(lower));
        combined as f64 * DOUBLE_MULTIPLIER
    }

    fn next_gaussian(&mut self) -> f64 {
        // Split the borrow: pull the LCG through a closure so the cached-pair
        // state and the generator advance in vanilla's exact order.
        let mut seed = self.seed;
        let result = self.gaussian.next(|| {
            seed = seed.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT) & MASK48;
            let upper = (seed >> (48 - 26)) as i32;
            seed = seed.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT) & MASK48;
            let lower = (seed >> (48 - 27)) as i32;
            let combined = (i64::from(upper) << 27).wrapping_add(i64::from(lower));
            combined as f64 * DOUBLE_MULTIPLIER
        });
        self.seed = seed;
        result
    }

    fn consume_count(&mut self, rounds: u32) {
        for _ in 0..rounds {
            self.next(32);
        }
    }
}

/// Positional factory for [`LegacyRandomSource`].
#[derive(Debug, Clone, Copy)]
pub struct LegacyPositionalFactory {
    seed: i64,
}

impl LegacyPositionalFactory {
    /// Builds a factory from a raw seed (the `new LegacyPositionalRandomFactory(seed)` ctor).
    #[must_use]
    pub(crate) fn from_seed_value(seed: i64) -> Self {
        Self { seed }
    }
}

impl PositionalRandomFactory for LegacyPositionalFactory {
    type Source = LegacyRandomSource;

    fn at(&self, x: i32, y: i32, z: i32) -> LegacyRandomSource {
        LegacyRandomSource::new(get_seed(x, y, z) ^ self.seed)
    }

    fn from_hash_of(&self, name: &str) -> LegacyRandomSource {
        LegacyRandomSource::new(i64::from(java_string_hash(name)) ^ self.seed)
    }

    fn from_seed(&self, seed: i64) -> LegacyRandomSource {
        LegacyRandomSource::new(seed)
    }
}
