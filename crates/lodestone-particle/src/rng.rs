//! `java.util.Random`, reproduced exactly.
//!
//! Vanilla particles draw every one of their constants — lifetime, initial
//! velocity, colour jitter, sprite choice — from `RandomSource.create()`, which
//! is a [`LegacyRandomSource`] wrapping the same 48-bit LCG as
//! `java.util.Random`. Reproducing it means a seeded engine replays a byte-exact
//! particle burst, which is what makes the parity tests in this crate able to
//! assert concrete numbers instead of ranges.
//!
//! # Why this is not shared with `lodestone-worldgen`
//!
//! `lodestone-worldgen::rng::legacy` contains the same LCG. Sharing it would
//! mean this crate depending on the whole worldgen stack — biome sources, noise
//! routers, structure placement — to reuse thirty lines of integer arithmetic.
//! The duplication is deliberate and bounded: the algorithm is frozen by the
//! Java specification and cannot drift. If a third copy ever appears, that is
//! the signal to lift one into `lodestone-core` and depend on it from all three.
//!
//! # Parity is *not* required here, and that is worth stating
//!
//! Unlike physics or packets, particle randomness is never compared against a
//! server: particles are client-side decoration and no observer can tell our
//! stream from vanilla's. The reason to be exact anyway is **testability** — an
//! LCG with published constants lets a test's expected values originate outside
//! the code under test, which is the standing requirement in this project.

/// The multiplier from `java.util.Random` (`0x5DEECE66D`).
const MULTIPLIER: i64 = 0x5_DEEC_E66D;
/// The additive constant from `java.util.Random` (`0xB`).
const INCREMENT: i64 = 0xB;
/// `(1 << 48) - 1` — the LCG runs in 48 bits.
const MASK: i64 = (1 << 48) - 1;

/// A bit-exact `java.util.Random`.
///
/// Only the operations vanilla particle code actually calls are implemented
/// ([`Self::next_float`], [`Self::next_int_bound`], [`Self::next_double`],
/// [`Self::next_bool`]). Adding more is fine; guessing at their semantics is
/// not — each one has a precise definition in the Java specification.
#[derive(Debug, Clone)]
pub struct JavaRandom {
    seed: i64,
}

impl JavaRandom {
    /// `new Random(seed)` — note the scramble, which is part of the contract and
    /// not an implementation detail: two `JavaRandom`s built from seeds `s` and
    /// `s ^ MULTIPLIER` are *not* related in any simple way without it.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ MULTIPLIER) & MASK,
        }
    }

    /// `new Random()` — seeded from the system clock.
    ///
    /// Uses nanosecond wall time. This is the only nondeterministic entry point
    /// in the crate; every test uses [`Self::new`] instead.
    #[must_use]
    pub fn from_entropy() -> Self {
        // `lodestone_time::epoch_duration`, not `SystemTime::now()`: the latter traps
        // on wasm32, and this is the crate's one clock-seeded entry point. Identical
        // on native — see `lodestone_time`'s crate docs.
        let d = lodestone_time::epoch_duration();
        // Truncating the nanosecond count into 64 bits is exactly what we want
        // from a clock seed; the LCG masks it to 48 bits immediately anyway.
        let nanos = i64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(i64::MAX);
        Self::new(nanos)
    }

    /// `protected int next(int bits)` — advance the LCG and take the high `bits`.
    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT) & MASK;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Java's `(int)(seed >>> (48 - bits))` truncates by definition"
        )]
        {
            (self.seed >> (48 - i64::from(bits))) as i32
        }
    }

    /// `float nextFloat()` — uniform in `[0, 1)`.
    ///
    /// Exactly 24 bits of mantissa, so the result is always a multiple of
    /// `2^-24`. Particle code multiplies this by small constants in **`float`**
    /// arithmetic; keeping the return type `f32` is what makes those products
    /// reproduce.
    pub fn next_float(&mut self) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "24-bit value into f32 is exact; this is Java's definition"
        )]
        {
            // `1 << 24`. Exact in f32, so the division introduces no rounding of
            // its own and the result is precisely Java's.
            self.next(24) as f32 / 16_777_216.0
        }
    }

    /// `double nextDouble()` — uniform in `[0, 1)`, 53 bits.
    pub fn next_double(&mut self) -> f64 {
        let hi = i64::from(self.next(26)) << 27;
        let lo = i64::from(self.next(27));
        #[expect(
            clippy::cast_precision_loss,
            reason = "53-bit value into f64 is exact; this is Java's definition"
        )]
        {
            (hi + lo) as f64 * (1.0 / 9_007_199_254_740_992.0)
        }
    }

    /// `boolean nextBoolean()`.
    pub fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }

    /// `int nextInt(int bound)` — uniform in `[0, bound)`.
    ///
    /// Includes the rejection loop. Dropping it would bias the low values, which
    /// is exactly the kind of "close enough" that makes a replayed burst diverge
    /// after a few hundred draws.
    ///
    /// # Panics
    ///
    /// If `bound` is not positive, matching Java's `IllegalArgumentException`.
    pub fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        // Powers of two take the high bits directly, by Java's fast path.
        if bound & -bound == bound {
            let bits = i64::from(self.next(31));
            #[expect(
                clippy::cast_possible_truncation,
                reason = "product of a 31-bit value and `bound` is shifted back down by 31"
            )]
            {
                return ((i64::from(bound) * bits) >> 31) as i32;
            }
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            // Reject the tail that would wrap unevenly.
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }
}

impl Default for JavaRandom {
    fn default() -> Self {
        Self::from_entropy()
    }
}

#[cfg(test)]
mod tests {
    use super::JavaRandom;

    /// The expected values here were produced by **Java itself**, not by this
    /// implementation — `new Random(42)` followed by five `nextInt()` draws is
    /// one of the most widely published sequences there is, and it is stable
    /// across every JDK because `java.util.Random` is specified, not merely
    /// implemented. An expected value that originated from running this code
    /// would assert only that the code is self-consistent.
    #[test]
    fn matches_the_published_java_sequence_for_seed_42() {
        let mut r = JavaRandom::new(42);
        // `nextInt()` is `next(32)`; exposed here through the bounded form's
        // sibling so the test exercises the LCG core rather than a wrapper.
        let got: Vec<i32> = (0..5).map(|_| r.next(32)).collect();
        assert_eq!(
            got,
            vec![-1_170_105_035, 234_785_527, -1_360_544_799, 205_897_768, 1_325_939_940]
        );
    }

    #[test]
    fn next_float_is_in_range_and_quantised_to_24_bits() {
        let mut r = JavaRandom::new(7);
        for _ in 0..10_000 {
            let f = r.next_float();
            assert!((0.0..1.0).contains(&f), "nextFloat out of range: {f}");
            // Every value must be an exact multiple of 2^-24.
            let scaled = f * 16_777_216.0;
            assert!(
                (scaled - scaled.round()).abs() < 1e-3,
                "nextFloat not quantised: {f}"
            );
        }
    }

    #[test]
    fn bounded_int_stays_in_range_including_the_power_of_two_fast_path() {
        let mut r = JavaRandom::new(99);
        for bound in [1, 2, 3, 8, 10, 16, 9] {
            for _ in 0..2_000 {
                let v = r.next_int_bound(bound);
                assert!((0..bound).contains(&v), "nextInt({bound}) gave {v}");
            }
        }
    }

    /// **A real property of `java.util.Random`, pinned deliberately.**
    ///
    /// Adjacent seeds produce *nearly identical* first draws. The scramble in
    /// [`JavaRandom::new`] only XORs the seed, and `next(24)` takes the **high**
    /// 24 bits of a 48-bit state that adjacent seeds move by just the multiplier
    /// (`≈2.5e10` out of `≈2.8e14`), so the first `nextFloat()` shifts by only
    /// about `1e-4`.
    ///
    /// This test originally asserted the opposite — that adjacent seeds diverge
    /// — on the assumption that the scramble decorrelates them. It does not, and
    /// the failure was the code being right and the test being wrong.
    ///
    /// It matters practically: **never seed a particle engine from a small
    /// integer derived from position.** Seeding per-block from a coordinate hash
    /// would make bursts on neighbouring blocks visibly identical, which is
    /// exactly the sort of thing that looks like a rendering bug. The engine
    /// seeds from entropy once and draws from a single stream for this reason.
    #[test]
    fn adjacent_seeds_are_correlated_which_is_why_we_never_seed_per_position() {
        let a = JavaRandom::new(1000).next_float();
        let b = JavaRandom::new(1001).next_float();
        assert!(
            (a - b).abs() < 1e-3,
            "expected the known java.util.Random correlation, got {a} vs {b}"
        );
        // A control: seeds far apart are not correlated, so the assertion above
        // is measuring seed adjacency and not simply a broken generator.
        let far = JavaRandom::new(1_000_000_007).next_float();
        assert!(
            (a - far).abs() > 1e-3,
            "distant seeds should not be correlated: {a} vs {far}"
        );
    }

    #[test]
    fn replaying_the_same_seed_reproduces_the_stream() {
        let draws = |seed| {
            let mut r = JavaRandom::new(seed);
            (0..64).map(|_| r.next_float()).collect::<Vec<_>>()
        };
        assert_eq!(draws(2024), draws(2024));
        assert_ne!(draws(2024), draws(2025));
    }
}
