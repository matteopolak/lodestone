//! `XoroshiroRandomSource` — the xoroshiro128++ generator used since 1.18.

use super::gaussian::Gaussian;
use super::{PositionalRandomFactory, RandomSource, get_seed};
use crate::hash::md5;

const GOLDEN_RATIO_64: i64 = -7_046_029_254_386_353_131;
const SILVER_RATIO_64: i64 = 7_640_891_576_956_012_809;
const FLOAT_MULTIPLIER: f32 = 5.960_464_5e-8;
const DOUBLE_MULTIPLIER: f64 = 1.110_223e-16_f32 as f64;

/// `RandomSupport.mixStafford13` — the finaliser applied to seed halves.
#[must_use]
pub(crate) fn mix_stafford13(mut z: i64) -> i64 {
    z = (z ^ ((z as u64 >> 30) as i64)).wrapping_mul(-4_658_895_280_553_007_687);
    z = (z ^ ((z as u64 >> 27) as i64)).wrapping_mul(-7_723_592_293_110_705_685);
    z ^ ((z as u64 >> 31) as i64)
}

/// `RandomSupport.upgradeSeedTo128bit`: expand a 64-bit seed into a mixed
/// 128-bit `(lo, hi)` pair.
fn upgrade_seed_to_128bit(legacy_seed: i64) -> (i64, i64) {
    let lo = legacy_seed ^ SILVER_RATIO_64;
    let hi = lo.wrapping_add(GOLDEN_RATIO_64);
    (mix_stafford13(lo), mix_stafford13(hi))
}

/// `RandomSupport.seedFromHashOf`: the 128-bit seed derived from an MD5 digest.
fn seed_from_hash_of(name: &str) -> (i64, i64) {
    let digest = md5(name.as_bytes());
    let lo = i64::from_be_bytes(digest[0..8].try_into().unwrap());
    let hi = i64::from_be_bytes(digest[8..16].try_into().unwrap());
    (lo, hi)
}

/// The raw xoroshiro128++ engine.
#[derive(Debug, Clone, Copy)]
struct Xoroshiro128PlusPlus {
    lo: i64,
    hi: i64,
}

impl Xoroshiro128PlusPlus {
    fn new(lo: i64, hi: i64) -> Self {
        if lo | hi == 0 {
            // Avoid the all-zero fixed point.
            Self {
                lo: GOLDEN_RATIO_64,
                hi: SILVER_RATIO_64,
            }
        } else {
            Self { lo, hi }
        }
    }

    fn next_long(&mut self) -> i64 {
        let s0 = self.lo;
        let s1 = self.hi;
        let result = (s0.wrapping_add(s1)).rotate_left(17).wrapping_add(s0);
        let s1 = s1 ^ s0;
        self.lo = s0.rotate_left(49) ^ s1 ^ (s1 << 21);
        self.hi = s1.rotate_left(28);
        result
    }
}

/// The 1.18+ xoroshiro128++ random source.
#[derive(Debug, Clone)]
pub struct XoroshiroRandomSource {
    engine: Xoroshiro128PlusPlus,
    gaussian: Gaussian,
}

impl XoroshiroRandomSource {
    /// Seeds exactly as `new XoroshiroRandomSource(seed)` (upgrade + mix).
    #[must_use]
    pub fn new(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed_to_128bit(seed);
        Self::from_128bit(lo, hi)
    }

    /// Seeds from a raw `(lo, hi)` pair without upgrade/mix (used by positional
    /// factories, matching the `(seedLo, seedHi)` constructor).
    #[must_use]
    pub fn from_128bit(lo: i64, hi: i64) -> Self {
        Self {
            engine: Xoroshiro128PlusPlus::new(lo, hi),
            gaussian: Gaussian::default(),
        }
    }

    fn next_bits(&mut self, bits: u32) -> i64 {
        ((self.engine.next_long() as u64) >> (64 - bits)) as i64
    }
}

impl RandomSource for XoroshiroRandomSource {
    type Positional = XoroshiroPositionalFactory;

    fn fork_positional(&mut self) -> XoroshiroPositionalFactory {
        XoroshiroPositionalFactory {
            seed_lo: self.engine.next_long(),
            seed_hi: self.engine.next_long(),
        }
    }

    fn set_seed(&mut self, seed: i64) {
        let (lo, hi) = upgrade_seed_to_128bit(seed);
        self.engine = Xoroshiro128PlusPlus::new(lo, hi);
        self.gaussian.reset();
    }

    fn next_bits(&mut self, bits: u32) -> i32 {
        // Matches WorldgenRandom's non-legacy arm: (int)(nextLong() >>> (64-bits)).
        ((self.engine.next_long() as u64) >> (64 - bits)) as i32
    }

    fn next_int(&mut self) -> i32 {
        self.engine.next_long() as i32
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        // Lemire's nearly-divisionless bounded generation.
        let bound_u = bound as u64;
        let mut random_bits = u64::from(self.next_int() as u32);
        let mut multiplied = random_bits.wrapping_mul(bound_u);
        let mut fractional = multiplied & 0xFFFF_FFFF;
        if fractional < bound_u {
            let threshold = u64::from((bound as u32).wrapping_neg() % bound as u32);
            while fractional < threshold {
                random_bits = u64::from(self.next_int() as u32);
                multiplied = random_bits.wrapping_mul(bound_u);
                fractional = multiplied & 0xFFFF_FFFF;
            }
        }
        (multiplied >> 32) as i32
    }

    fn next_long(&mut self) -> i64 {
        self.engine.next_long()
    }

    fn next_bool(&mut self) -> bool {
        self.engine.next_long() & 1 != 0
    }

    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * FLOAT_MULTIPLIER
    }

    fn next_double(&mut self) -> f64 {
        self.next_bits(53) as f64 * DOUBLE_MULTIPLIER
    }

    fn next_gaussian(&mut self) -> f64 {
        let mut engine = self.engine;
        let result = self.gaussian.next(|| {
            let bits = ((engine.next_long() as u64) >> 11) as i64;
            bits as f64 * DOUBLE_MULTIPLIER
        });
        self.engine = engine;
        result
    }

    fn consume_count(&mut self, rounds: u32) {
        for _ in 0..rounds {
            self.engine.next_long();
        }
    }
}

/// Positional factory for [`XoroshiroRandomSource`].
#[derive(Debug, Clone, Copy)]
pub struct XoroshiroPositionalFactory {
    seed_lo: i64,
    seed_hi: i64,
}

impl PositionalRandomFactory for XoroshiroPositionalFactory {
    type Source = XoroshiroRandomSource;

    fn at(&self, x: i32, y: i32, z: i32) -> XoroshiroRandomSource {
        let positional = get_seed(x, y, z);
        XoroshiroRandomSource::from_128bit(positional ^ self.seed_lo, self.seed_hi)
    }

    fn from_hash_of(&self, name: &str) -> XoroshiroRandomSource {
        let (lo, hi) = seed_from_hash_of(name);
        XoroshiroRandomSource::from_128bit(lo ^ self.seed_lo, hi ^ self.seed_hi)
    }

    fn from_seed(&self, seed: i64) -> XoroshiroRandomSource {
        XoroshiroRandomSource::from_128bit(seed ^ self.seed_lo, seed ^ self.seed_hi)
    }
}
