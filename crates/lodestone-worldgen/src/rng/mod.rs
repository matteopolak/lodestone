//! Seeded random sources with vanilla-exact behaviour.
//!
//! Two generator families exist in Minecraft, both reproduced here:
//! * [`LegacyRandomSource`] — the `java.util.Random` LCG used before 1.18 and
//!   still used for some legacy generation paths.
//! * [`XoroshiroRandomSource`] — the xoroshiro128++ generator introduced in 1.18
//!   for the density-function worldgen system.
//!
//! Both expose the same [`RandomSource`] surface and a [`PositionalRandomFactory`]
//! (via `fork_positional`) that derives independent, position-keyed generators —
//! the mechanism worldgen uses to seed per-noise and per-feature randomness.

mod gaussian;
mod legacy;
mod xoroshiro;

pub use legacy::{LegacyPositionalFactory, LegacyRandomSource};
pub use xoroshiro::{XoroshiroPositionalFactory, XoroshiroRandomSource};

/// `RandomSupport.mixStafford13` — the 64-bit finaliser used when upgrading a
/// seed to xoroshiro's 128-bit state. Exposed for parity testing.
#[must_use]
pub fn mix_stafford13_pub(z: i64) -> i64 {
    xoroshiro::mix_stafford13(z)
}

/// `Mth.getSeed(x, y, z)` — the positional hash used to key positional
/// generators. Note the `x` term is a **32-bit** int multiply (it overflows and
/// wraps before promotion to `long`), which is why parity is subtle.
#[must_use]
pub fn get_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut seed = i64::from(x.wrapping_mul(3_129_871))
        ^ (i64::from(z).wrapping_mul(116_129_781))
        ^ i64::from(y);
    seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42_317_861)
        .wrapping_add(seed.wrapping_mul(11));
    seed >> 16
}

/// The shared surface of a vanilla random source.
///
/// Method semantics mirror `net.minecraft.util.RandomSource`. Callers that need
/// bit-exact parity must respect draw *order*: every method that advances the
/// generator does so in the same sequence vanilla uses.
pub trait RandomSource {
    /// The positional factory this source forks into (`forkPositional`).
    type Positional: PositionalRandomFactory;
    /// Forks an independent positional factory from the current state, exactly
    /// as `RandomSource.forkPositional()`.
    fn fork_positional(&mut self) -> Self::Positional;
    /// Re-seeds the generator, resetting any cached Gaussian.
    fn set_seed(&mut self, seed: i64);
    /// The primitive bit generator: advances the source and yields the top
    /// `bits` bits. For the LCG this is `java.util.Random.next(bits)`; for
    /// xoroshiro it is `(int)(nextLong() >>> (64 - bits))`. `WorldgenRandom`
    /// builds every other draw on top of this, which is why it must be on the
    /// trait rather than hidden in each impl.
    fn next_bits(&mut self, bits: u32) -> i32;
    /// Next uniformly distributed `i32` (full 32-bit range).
    fn next_int(&mut self) -> i32;
    /// Next `i32` in `[0, bound)`. Panics if `bound <= 0`.
    fn next_int_bounded(&mut self, bound: i32) -> i32;
    /// Next uniformly distributed `i64`.
    fn next_long(&mut self) -> i64;
    /// Next boolean.
    fn next_bool(&mut self) -> bool;
    /// Next `f32` in `[0, 1)`.
    fn next_float(&mut self) -> f32;
    /// Next `f64` in `[0, 1)`.
    fn next_double(&mut self) -> f64;
    /// Next standard-normal `f64` (Marsaglia polar, with vanilla's cached pair).
    fn next_gaussian(&mut self) -> f64;
    /// `consumeCount(rounds)` — advances the generator, discarding output. The
    /// draw shape differs by family (xoroshiro discards `nextLong`s, the LCG
    /// discards `nextInt`s), so it lives on the trait.
    fn consume_count(&mut self, rounds: u32);
}

/// A factory that derives independent generators keyed by position or name.
///
/// Mirrors `net.minecraft.world.level.levelgen.PositionalRandomFactory`.
pub trait PositionalRandomFactory {
    /// The concrete generator this factory produces.
    type Source: RandomSource;
    /// Derives a generator for the block position `(x, y, z)`.
    fn at(&self, x: i32, y: i32, z: i32) -> Self::Source;
    /// Derives a generator from a resource name (used to seed named noises).
    fn from_hash_of(&self, name: &str) -> Self::Source;
    /// Derives a generator from a raw seed.
    fn from_seed(&self, seed: i64) -> Self::Source;
}

/// `WorldgenRandom` seed-derivation helper.
///
/// In vanilla, `WorldgenRandom extends LegacyRandomSource` and overrides only
/// `next(bits)` to pull from its wrapped source — so **all** of its draws
/// (`nextLong`, `nextInt`, …) use the *legacy* `BitRandomSource` structure even
/// when the wrapped source is xoroshiro. Concretely `nextLong()` consumes two
/// `next(32)` calls, and for a non-legacy source `next(32) == (int)(inner.nextLong() >>> 32)`.
/// Reproducing that exactly is essential: a naive delegation to
/// `inner.next_long()` diverges immediately (and did, until this was fixed).
#[derive(Debug)]
pub struct WorldgenRandom<R: RandomSource> {
    inner: R,
    count: u32,
}

impl<R: RandomSource> WorldgenRandom<R> {
    /// Wraps a base generator.
    pub fn new(inner: R) -> Self {
        Self { inner, count: 0 }
    }

    /// Number of `next(bits)` draws performed (mirrors `getCount`).
    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    /// `setDecorationSeed(seed, blockX, blockZ)` — returns and installs the
    /// per-chunk decoration seed.
    pub fn set_decoration_seed(&mut self, seed: i64, block_x: i32, block_z: i32) -> i64 {
        self.set_seed(seed);
        let x_scale = self.next_long() | 1;
        let z_scale = self.next_long() | 1;
        let result = (i64::from(block_x).wrapping_mul(x_scale))
            .wrapping_add(i64::from(block_z).wrapping_mul(z_scale))
            ^ seed;
        self.set_seed(result);
        result
    }

    /// `setFeatureSeed(seed, index, step)`.
    pub fn set_feature_seed(&mut self, seed: i64, index: i32, step: i32) {
        let result = seed
            .wrapping_add(i64::from(index))
            .wrapping_add(10_000i64.wrapping_mul(i64::from(step)));
        self.set_seed(result);
    }

    /// `setLargeFeatureSeed(seed, chunkX, chunkZ)`.
    pub fn set_large_feature_seed(&mut self, seed: i64, chunk_x: i32, chunk_z: i32) {
        self.set_seed(seed);
        let x_scale = self.next_long();
        let z_scale = self.next_long();
        let result = (i64::from(chunk_x).wrapping_mul(x_scale))
            ^ (i64::from(chunk_z).wrapping_mul(z_scale))
            ^ seed;
        self.set_seed(result);
    }
}

impl<R: RandomSource> RandomSource for WorldgenRandom<R> {
    type Positional = LegacyPositionalFactory;

    fn fork_positional(&mut self) -> LegacyPositionalFactory {
        // WorldgenRandom extends LegacyRandomSource, so forkPositional yields a
        // legacy factory seeded from its (overridden) nextLong.
        LegacyPositionalFactory::from_seed_value(self.next_long())
    }

    fn set_seed(&mut self, seed: i64) {
        self.inner.set_seed(seed);
    }

    fn next_bits(&mut self, bits: u32) -> i32 {
        self.count = self.count.wrapping_add(1);
        self.inner.next_bits(bits)
    }

    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        // BitRandomSource semantics (inherited from LegacyRandomSource).
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound).wrapping_mul(i64::from(self.next_bits(31)))) >> 31) as i32;
        }
        loop {
            let sample = self.next_bits(31);
            let modulo = sample % bound;
            if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
                return modulo;
            }
        }
    }

    fn next_long(&mut self) -> i64 {
        let upper = self.next_bits(32);
        let lower = self.next_bits(32);
        (i64::from(upper) << 32).wrapping_add(i64::from(lower))
    }

    fn next_bool(&mut self) -> bool {
        self.next_bits(1) != 0
    }

    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * 5.960_464_5e-8
    }

    fn next_double(&mut self) -> f64 {
        let upper = self.next_bits(26);
        let lower = self.next_bits(27);
        let combined = (i64::from(upper) << 27).wrapping_add(i64::from(lower));
        combined as f64 * (1.110_223e-16_f32 as f64)
    }

    fn next_gaussian(&mut self) -> f64 {
        // Not needed by terrain; WorldgenRandom's gaussian would come from its
        // LegacyRandomSource superclass. Left unimplemented on purpose.
        unimplemented!("WorldgenRandom::next_gaussian is not used by terrain generation")
    }

    fn consume_count(&mut self, rounds: u32) {
        for _ in 0..rounds {
            self.next_int();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_seed_matches_known_origin() {
        // Mth.getSeed(0,0,0) == 0 (0*0*.. == 0, >>16 == 0).
        assert_eq!(get_seed(0, 0, 0), 0);
    }
}
