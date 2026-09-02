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
//!
//! Which family a *dimension* uses is data, not a constant: see [`Algorithm`]
//! (`WorldgenRandom.Algorithm`) and the [`AnyRandomSource`] /
//! [`AnyPositionalFactory`] pair it produces. The Overworld is xoroshiro; the
//! Nether and the End set `legacy_random_source: true` and are therefore LCG
//! from the noise stack down.

mod any;
mod gaussian;
mod legacy;
mod xoroshiro;

pub use any::{Algorithm, AnyPositionalFactory, AnyRandomSource};
pub use legacy::{LegacyPositionalFactory, LegacyRandomSource};
pub use xoroshiro::{XoroshiroPositionalFactory, XoroshiroRandomSource};

/// Vanilla's own random-support Stafford-13 mix — the 64-bit finaliser used when upgrading a
/// seed to xoroshiro's 128-bit state. Exposed for parity testing.
#[must_use]
pub fn mix_stafford13_pub(z: i64) -> i64 {
    xoroshiro::mix_stafford13(z)
}

/// Vanilla's own math-helper get-seed at `(x, y, z)` — the positional hash used to key positional
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
/// Method semantics mirror vanilla's own random-source interface. Callers that need
/// bit-exact parity must respect draw *order*: every method that advances the
/// generator does so in the same sequence vanilla uses.
pub trait RandomSource {
    /// The positional factory this source forks into (vanilla's own
    /// fork-positional call).
    type Positional: PositionalRandomFactory;
    /// Forks an independent positional factory from the current state, exactly
    /// as vanilla's own random-source fork-positional.
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
    /// Vanilla's own consume-count call — advances the generator, discarding output. The
    /// draw shape differs by family (xoroshiro discards `nextLong`s, the LCG
    /// discards `nextInt`s), so it lives on the trait.
    fn consume_count(&mut self, rounds: u32);
}

/// A factory that derives independent generators keyed by position or name.
///
/// Mirrors vanilla's own positional-random-factory interface.
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

    /// Number of `next(bits)` draws performed (mirrors vanilla's own
    /// get-count query).
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

    /// `setLargeFeatureWithSalt(seed, x, z, blend)` — the seed derivation every
    /// structure-set placement decision is made against.
    ///
    /// Transcribed from the record definition in vanilla's own worldgen-random
    /// class,
    /// not from a call site:
    ///
    /// ```text
    /// long result = x * 341873128712L + z * 132897987541L + seed + blend;
    /// setSeed(result);
    /// ```
    ///
    /// The decompiler names the fourth parameter `blend`; every caller passes a
    /// structure placement's `salt`. **`x * 341873128712L` widens before the
    /// multiply** (the literal is `long`), so both products are 64-bit — this is
    /// *not* the mixed-width arithmetic [`seed_slime_chunk`] has to reproduce.
    ///
    /// # The argument order at the call site is not what you would guess
    ///
    /// Vanilla's own structure-placement probability-reducer — the `default`
    /// `frequency_reduction_method`, the one 18 of the 20 bundled structure sets
    /// use — calls this as `setLargeFeatureWithSalt(seed, salt, sourceX, sourceZ)`:
    /// the *salt* lands in `x`, the chunk X in `z`, and the chunk Z in `blend`.
    /// Vanilla's own random-spread-placement potential-structure-chunk lookup calls it the
    /// straightforward way (`seed, gridX, gridZ, salt`). Both spellings are
    /// vanilla's own and both are load-bearing, so this method takes the
    /// parameters positionally and refuses to name them after their meaning.
    /// See [`crate::rng`]'s callers in `lodestone_worldgen::structure::placement`.
    pub fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, blend: i32) {
        let result = i64::from(x)
            .wrapping_mul(341_873_128_712)
            .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
            .wrapping_add(seed)
            .wrapping_add(i64::from(blend));
        self.set_seed(result);
    }
}

/// Vanilla's own worldgen-random slime-chunk seeding routine, `seedSlimeChunk(x, z, seed, salt)`.
///
/// A free function rather than a method because vanilla's is `static` and needs no
/// carrier state: it derives a fresh [`LegacyRandomSource`]
/// (vanilla's own thread-local random-source constructor, i.e. plain `java.util.Random`, **not**
/// xoroshiro) from the chunk coordinates and the world seed.
///
/// # The three ways to get this subtly wrong
///
/// ```text
/// seed + x * x * 4987142 + x * 5947611 + z * z * 4392871L + z * 389711 ^ salt
/// ```
///
/// 1. **The arithmetic mixes widths on purpose.** `x * x * 4987142` and
///    `x * 5947611` and `z * 389711` are Java `int` multiplications — 32-bit,
///    wrapping — while `z * z * 4392871L` widens *after* `z * z` because only the
///    last factor carries the `L`. Transcribing all four as `i64` gives a different
///    seed for large coordinates and an identical one for small ones, which is why
///    a fixture confined to small positive coordinates cannot tell the two apart.
/// 2. **`^` binds looser than `+`**, so the XOR applies to the whole sum.
/// 3. **The additions are `long`**, so each `int` product is sign-extended before
///    being added — hence the `as i64` on the wrapped 32-bit results rather than on
///    their operands.
#[must_use]
pub fn seed_slime_chunk(x: i32, z: i32, seed: i64, salt: i64) -> LegacyRandomSource {
    // Each `let` is one Java sub-expression, in Java's own evaluation order, so the
    // width of every operation is visible on its own line.
    let x_sq: i64 = i64::from(x.wrapping_mul(x).wrapping_mul(4_987_142));
    let x_lin: i64 = i64::from(x.wrapping_mul(5_947_611));
    let z_sq: i64 = i64::from(z.wrapping_mul(z)).wrapping_mul(4_392_871);
    let z_lin: i64 = i64::from(z.wrapping_mul(389_711));
    let mixed = seed
        .wrapping_add(x_sq)
        .wrapping_add(x_lin)
        .wrapping_add(z_sq)
        .wrapping_add(z_lin)
        ^ salt;
    LegacyRandomSource::new(mixed)
}

/// Vanilla's own slime-spawn-rules check's slime-chunk salt.
pub const SLIME_CHUNK_SALT: i64 = 987_234_911;

/// Whether `(chunk_x, chunk_z)` is a slime chunk for `seed` — a **pure function**
/// of those three values, so it is bit-exact or wrong; there is no tolerance
/// available and no way for a green test to be vacuous about magnitude.
///
/// The `nextInt(10) == 0` lives here rather than in [`seed_slime_chunk`] because
/// vanilla puts it at the call site: the derivation is worldgen's, the predicate is
/// spawning's. Vanilla's own slime-spawn-rules check combines this with a *separate*
/// `random.nextInt(10) == 0` off the spawn RNG and `pos.getY() < 40`, neither of
/// which belongs to this function.
#[must_use]
pub fn is_slime_chunk(chunk_x: i32, chunk_z: i32, seed: i64) -> bool {
    seed_slime_chunk(chunk_x, chunk_z, seed, SLIME_CHUNK_SALT).next_int_bounded(10) == 0
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
        // The single funnel for every production RNG draw: `next_int`,
        // `next_int_bounded`, `next_long`, `next_bool`, `next_float`,
        // `next_double` and `consume_count` all route through here, and all
        // terrain RNG goes through `WorldgenRandom<R>` rather than a bare
        // backend. Hooking the two backends' own primitives instead would
        // double-count (`next_long` is two `next_bits` calls on the legacy
        // source) and would also count the noise-construction draws that are
        // not part of any stage.
        crate::counters::bump_rng_draw();
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
        // the seed-mixing function at (0,0,0) reduces to 0*0*.. == 0, >>16 == 0.
        assert_eq!(get_seed(0, 0, 0), 0);
    }
    /// `is_slime_chunk` is a pure function of `(chunk_x, chunk_z,
    /// seed)`, so this is element-wise and exact — not a match count and not a
    /// "roughly 10%" statistic, which would be the *magnitude* species of vacuous
    /// test.
    ///
    /// # Where the expected values come from, stated honestly
    ///
    /// **Not a JVM run** — there is no JDK on this machine and the oracles run under
    /// Apple `container`, which was more setup than this warranted. They come from
    /// an independent transcription of `java.util.Random` plus the
    /// vanilla's own slime-chunk-seed expression in **Python**, where integers are arbitrary
    /// precision and every 32-bit wrap therefore had to be written out explicitly
    /// rather than happening for free. That is weaker than a captured JVM dump
    /// (same author, so a shared misreading of the spec survives), and stronger than
    /// `decode(encode(x))`: it is a different language, a different LCG
    /// implementation, and the width discipline is forced to be visible.
    ///
    /// # What makes the lattice discriminating
    ///
    /// The named trap for this unit is transcribing all four products as `i64`. The
    /// two hypotheses agree on every small coordinate, so a fixture confined to
    /// those cannot separate them. These do, measured at seed 42:
    ///
    /// | coordinate | correct seed | all-`i64` seed |
    /// |---|---|---|
    /// | (46341, 0) | 3,079,240,536 | 10,710,105,327,237,976 |
    /// | (65536, 65536) | −1,980,628,363 | 40,287,263,702,254,197 |
    /// | (100000, −100000) | 6,194,237,115,429,365 | 93,800,686,506,701,301 |
    ///
    /// 46341 is just above `sqrt(i32::MAX)`, so `x * x` alone overflows there.
    /// Negatives on both axes are present for the sign-extension half.
    #[test]
    fn slime_chunks_match_the_derivation_element_wise() {
        const COORDS: [(i32, i32); 16] = [
            (0, 0),
            (1, 0),
            (0, 1),
            (-1, -1),
            (-13, 7),
            (7, -13),
            (1234, -5678),
            (46341, 0),
            (0, 46341),
            (-46341, 46341),
            (65536, 65536),
            (100_000, -100_000),
            (2, 3),
            (4, 5),
            (-8, -9),
            (31, -64),
        ];
        // One character per coordinate, in `COORDS` order.
        const EXPECTED: [(i64, &str); 4] = [
            (42, "0000000001000000"),
            (-1, "0000000100000100"),
            (1_234_567_890_123, "0000000100001000"),
            (0, "0000010000010000"),
        ];
        for (seed, expected) in EXPECTED {
            let got: String = COORDS
                .iter()
                .map(|&(x, z)| if is_slime_chunk(x, z, seed) { '1' } else { '0' })
                .collect();
            assert_eq!(
                got, expected,
                "seed {seed}: slime-chunk lattice differs element-wise\n  coords {COORDS:?}"
            );
        }
    }

    /// The control for the test above: the all-`i64` transcription — the one
    /// mistake the issue names — must produce a *different* answer somewhere on
    /// this lattice. Observed, not described; without it the lattice could be
    /// passing because it never leaves the range where both hypotheses agree.
    #[test]
    fn the_all_i64_transcription_is_separated_by_this_lattice() {
        fn wrong(x: i32, z: i32, seed: i64, salt: i64) -> i64 {
            let x = i64::from(x);
            let z = i64::from(z);
            (seed
                .wrapping_add(x.wrapping_mul(x).wrapping_mul(4_987_142))
                .wrapping_add(x.wrapping_mul(5_947_611))
                .wrapping_add(z.wrapping_mul(z).wrapping_mul(4_392_871))
                .wrapping_add(z.wrapping_mul(389_711)))
                ^ salt
        }
        // Chosen because `x * x` overflows `i32` there; 46340^2 does not.
        let (x, z, seed) = (46341, 0, 42);
        let right = seed_slime_chunk(x, z, seed, SLIME_CHUNK_SALT);
        let mut wrong_src = LegacyRandomSource::new(wrong(x, z, seed, SLIME_CHUNK_SALT));
        let mut right_src = right;
        assert_ne!(
            right_src.next_int_bounded(1 << 30),
            wrong_src.next_int_bounded(1 << 30),
            "the all-i64 hypothesis must diverge at ({x}, {z}), or this lattice proves \
             nothing about the width mixing"
        );
    }
}
