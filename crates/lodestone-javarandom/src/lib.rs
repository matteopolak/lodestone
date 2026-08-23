//! A single, bit-exact `java.util.Random` — the workspace's one copy of the
//! 48-bit linear congruential generator vanilla uses everywhere it needs a
//! seeded, reproducible draw: particle bursts, the enchanting-table book
//! animation, the lightning bolt's procedural geometry, seeded sound variant
//! selection, and the ghast model's nine tentacle lengths.
//!
//! # Why this crate exists
//!
//! Before this crate, the identical algorithm was reimplemented **six**
//! times: `lodestone-particle`, `lodestone-shell` (the enchanting-table book),
//! `lodestone-render` (the lightning bolt), `lodestone-audio` (sound variant
//! selection), `lodestone-assets` (`entity_models::ghast_model`'s local
//! `struct JavaRng`), and `lodestone-worldgen-core` (`LegacyRandomSource`).
//! The `ghast_model` copy was missed by the first pass over the workspace and
//! found only by grepping for the LCG multiplier constant afterwards — worth
//! remembering next time a "these N are the only copies" count is taken on
//! faith. All six agreed on the constants and, for the bounds each call site
//! actually uses, on the observable behaviour — but two of them (the
//! lightning bolt's and the ghast model's) used plain rather than wrapping
//! `i32` arithmetic in the rejection loop, which is correct only because
//! nothing currently calls either with a bound close to `i32::MAX`. Six
//! independent copies is six chances for that kind of thing to drift into a
//! real divergence unnoticed, since nothing but a person reading all six side
//! by side would ever catch it.
//!
//! Five of the six now share this crate. **`LegacyRandomSource`
//! (`lodestone-worldgen-core`) deliberately does not** — see the section
//! below.
//!
//! # Why `LegacyRandomSource` stays separate
//!
//! `lodestone-worldgen-core`'s `LegacyRandomSource` wraps the same LCG core
//! this crate implements, but it is not a peer of the other five: it
//! implements worldgen's `RandomSource` trait alongside
//! `XoroshiroRandomSource`, so callers can be generic over which algorithm a
//! noise router or feature placer was told to use; it carries `next_gaussian`
//! (a cached Box-Muller pair, order-sensitive with the raw LCG steps in a way
//! that needs direct access to the seed field, not a call through a method);
//! and it drives `LegacyPositionalFactory` (`at(x, y, z)`,
//! `from_hash_of(name)`) for vanilla's position- and name-seeded streams,
//! which nothing else here needs.
//!
//! Folding it into this crate would mean growing this crate's minimal API to
//! carry gaussian caching and positional seeding for the sake of one caller,
//! which is the wrong shape for a crate every other consumer wants to stay a
//! handful of methods and no dependencies. The reverse — having
//! `LegacyRandomSource` wrap this crate's `JavaRandom` as a field — was also
//! rejected: `lodestone-worldgen-core`'s own `Cargo.toml` states its
//! `serde_json` dependency is deliberately the *only* non-`std` dependency in
//! the crate "because this is a leaf, and every unit scheduled against it
//! wants it to stay one." Adding an internal `lodestone-*` dependency, however
//! small, breaks that documented invariant for a ~15-line saving. So the
//! duplication between this crate's `next`/`next_i32_bound` and
//! `LegacyRandomSource`'s is deliberate and bounded, exactly as
//! `lodestone-particle`'s old module doc already argued for its own copy: the
//! algorithm is frozen by the Java specification and cannot drift on its own.
//!
//! # The one thing worth knowing before you call [`JavaRandom::next_i32_bound`]
//!
//! Java's `nextInt(bound)` is **two different algorithms**, not one formula
//! with a bound plugged in:
//!
//! * When `bound` is a power of two, it is a multiply-and-shift with no
//!   rejection: `(bound * next(31)) >> 31`.
//! * Otherwise it is a **rejection loop**: draw `next(31)`, reduce it modulo
//!   `bound`, and retry if that would bias the low end of the range. Skipping
//!   the rejection step is the kind of "close enough" that looks identical
//!   for a few hundred draws and then visibly diverges.
//!
//! An implementation that only has the modulo branch, or only the fast path,
//! is not a `java.util.Random` — it agrees with one for *some* bounds and
//! silently disagrees for others, which is exactly the shape of bug this
//! crate's consolidation exists to make impossible.

/// The multiplier from `java.util.Random` (`0x5DEECE66D`).
const MULTIPLIER: i64 = 0x5_DEEC_E66D;
/// The additive constant from `java.util.Random` (`0xB`).
const INCREMENT: i64 = 0xB;
/// `(1 << 48) - 1` — the LCG runs in 48 bits.
const MASK: i64 = (1 << 48) - 1;

/// A bit-exact `java.util.Random`: a 48-bit truncated linear congruential
/// generator, transcribed from the Java specification (`java.util.Random`,
/// mirrored by 26.2's `LegacyRandomSource`/`BitRandomSource`).
///
/// Every method here is defined precisely by that specification; adding a new
/// one is fine, guessing at its semantics from the name is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaRandom {
    /// The 48-bit scrambled state, held in an `i64` (always in `[0, 2^48)`).
    seed: i64,
}

impl JavaRandom {
    /// `new Random(seed)` — note the scramble, which is part of the contract
    /// and not an implementation detail: two `JavaRandom`s built from seeds
    /// `s` and `s ^ MULTIPLIER` are *not* related in any simple way without
    /// it.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ MULTIPLIER) & MASK,
        }
    }

    /// `new Random()` — seeded from the system clock.
    ///
    /// Uses nanosecond wall time. This is the only nondeterministic entry
    /// point in the crate; every test and every reproducible replay uses
    /// [`Self::new`] instead.
    #[must_use]
    pub fn from_entropy() -> Self {
        // `lodestone_time::epoch_duration`, not `SystemTime::now()`: the
        // latter traps on wasm32, and this is the crate's one clock-seeded
        // entry point. Identical on native — see `lodestone_time`'s crate
        // docs.
        let d = lodestone_time::epoch_duration();
        // Truncating the nanosecond count into 64 bits is exactly what we
        // want from a clock seed; the LCG masks it to 48 bits immediately
        // anyway.
        let nanos = i64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(i64::MAX);
        Self::new(nanos)
    }

    /// `Random.setSeed(seed)` — reseeds in place, with the same scramble as
    /// [`Self::new`].
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULTIPLIER) & MASK;
    }

    /// `protected int next(int bits)` — advance the LCG and take the high
    /// `bits` bits. `bits` must be in `1..=32`.
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

    /// `int nextInt()` — a uniformly distributed `i32` over the whole range,
    /// i.e. `next(32)`.
    pub fn next_i32(&mut self) -> i32 {
        self.next(32)
    }

    /// `int nextInt(int bound)` — uniform in `[0, bound)`.
    ///
    /// Includes the rejection loop; see the module docs for why that is not
    /// optional. Arithmetic in the loop is `i32`-**wrapping**, matching
    /// Java's own `int` overflow semantics for the overflow guard — using
    /// plain (panicking-on-overflow, in a debug build) arithmetic there is a
    /// real bug this consolidation removes, not a style choice.
    ///
    /// # Panics
    ///
    /// If `bound` is not positive, matching Java's `IllegalArgumentException`.
    pub fn next_i32_bound(&mut self, bound: i32) -> i32 {
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
            // Reject the tail that would wrap unevenly. Wrapping arithmetic
            // reproduces Java's `int` overflow exactly; see the doc above.
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }

    /// `long nextLong()`: `((long) next(32) << 32) + next(32)`.
    pub fn next_i64(&mut self) -> i64 {
        let hi = i64::from(self.next(32));
        let lo = i64::from(self.next(32));
        (hi << 32).wrapping_add(lo)
    }

    /// `boolean nextBoolean()`: `next(1) != 0`.
    pub fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }

    /// `float nextFloat()` — uniform in `[0, 1)`.
    ///
    /// Exactly 24 bits of mantissa, so the result is always a multiple of
    /// `2^-24`. Vanilla code multiplies this by small constants in **`float`**
    /// arithmetic; keeping the return type `f32` is what makes those products
    /// reproduce.
    pub fn next_f32(&mut self) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "24-bit value into f32 is exact; this is Java's definition"
        )]
        {
            // `1 << 24`. Exact in f32, so the division introduces no
            // rounding of its own and the result is precisely Java's.
            self.next(24) as f32 / 16_777_216.0
        }
    }

    /// `double nextDouble()` — uniform in `[0, 1)`, 53 bits.
    pub fn next_f64(&mut self) -> f64 {
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

    /// A `roll`-shaped draw: a uniform value in `[0, bound)`, returned as
    /// `u32` for callers (weighted selection, sound-variant picking) that
    /// never see a negative index. `bound` must be `>= 1` and `<= i32::MAX`.
    ///
    /// Exactly [`Self::next_i32_bound`] with the sign stripped off; kept as a
    /// separate name because `roll(total_weight)` reads better than
    /// `next_i32_bound(total_weight as i32)` at a weighted-selection call
    /// site.
    ///
    /// # Panics
    ///
    /// If `bound` is `0` or exceeds `i32::MAX`.
    pub fn roll(&mut self, bound: u32) -> u32 {
        assert!(bound >= 1, "roll bound must be >= 1");
        assert!(bound <= i32::MAX as u32, "roll bound exceeds i32::MAX");
        #[expect(
            clippy::cast_possible_wrap,
            reason = "bound <= i32::MAX is asserted immediately above"
        )]
        let bound = bound as i32;
        #[expect(
            clippy::cast_sign_loss,
            reason = "next_i32_bound(bound) with bound > 0 always returns a non-negative value"
        )]
        {
            self.next_i32_bound(bound) as u32
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
    /// implementation — `new Random(42)` followed by five `nextInt()` draws
    /// is one of the most widely published sequences there is, and it is
    /// stable across every JDK because `java.util.Random` is specified, not
    /// merely implemented. An expected value that originated from running
    /// this code would assert only that the code is self-consistent.
    #[test]
    fn matches_the_published_java_sequence_for_seed_42() {
        let mut r = JavaRandom::new(42);
        let got: Vec<i32> = (0..5).map(|_| r.next_i32()).collect();
        assert_eq!(
            got,
            vec![-1_170_105_035, 234_785_527, -1_360_544_799, 205_897_768, 1_325_939_940]
        );
    }

    /// `new Random(0).nextInt()` is the other most-cited datum: it and its
    /// successor are quoted directly in `java.util.Random`'s own javadoc.
    #[test]
    fn matches_the_published_java_sequence_for_seed_0() {
        let mut r = JavaRandom::new(0);
        assert_eq!(r.next_i32(), -1_155_484_576);
        assert_eq!(r.next_i32(), -723_955_400);
    }

    /// A hand-expanded LCG (independent Python re-implementation of the exact
    /// `java.util.Random` specification, not a call into any Rust code in
    /// this workspace) supplies every value below, for both branches of
    /// `nextInt(bound)`:
    ///
    /// * `bound = 37` is not a power of two, so this exercises the rejection
    ///   loop.
    /// * `bound = 16` is, so this exercises the multiply-and-shift fast path.
    ///
    /// `decode(encode(x)) == x` against our own implementation would prove
    /// nothing here; these values originate outside the crate under test.
    #[test]
    fn next_i32_bound_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(12345);
        let got: Vec<i32> = (0..10).map(|_| r.next_i32_bound(37)).collect();
        assert_eq!(got, vec![32, 33, 20, 29, 7, 30, 30, 15, 13, 15]);

        let mut r = JavaRandom::new(999);
        let got: Vec<i32> = (0..5).map(|_| r.next_i32_bound(16)).collect();
        assert_eq!(got, vec![11, 14, 11, 7, 1]);
    }

    /// Same independent oracle, for `nextLong()`.
    #[test]
    fn next_i64_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(7);
        let got: Vec<i64> = (0..3).map(|_| r.next_i64()).collect();
        assert_eq!(
            got,
            vec![-4_967_725_919_621_401_576, -4_627_004_027_837_150_407, 6_425_179_856_112_732_765]
        );
    }

    /// Same independent oracle, for `nextFloat()`.
    #[test]
    fn next_f32_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(7);
        let got = [r.next_f32(), r.next_f32(), r.next_f32()];
        let expected = [0.730_699f32, 0.638_537_6f32, 0.749_169_6f32];
        for (g, e) in got.iter().zip(expected.iter()) {
            assert!((g - e).abs() < 1e-5, "{g} vs {e}");
        }
    }

    /// Same independent oracle, for `nextDouble()`.
    #[test]
    fn next_f64_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(7);
        let got = [r.next_f64(), r.next_f64(), r.next_f64()];
        let expected = [0.730_699_042_060_042_1, 0.749_169_603_133_633_1, 0.348_309_703_031_256_97];
        for (g, e) in got.iter().zip(expected.iter()) {
            assert!((g - e).abs() < 1e-12, "{g} vs {e}");
        }
    }

    /// Same independent oracle, for `nextBoolean()`.
    #[test]
    fn next_bool_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(7);
        let got: Vec<bool> = (0..5).map(|_| r.next_bool()).collect();
        assert_eq!(got, vec![true, true, true, false, false]);
    }

    /// A single seed drawn through four different methods in sequence, order
    /// matters here: each method consumes a different number of `next(bits)`
    /// steps (1, 1, 1 or 2, 2), so this is the test that would catch two
    /// methods silently swapping which one advances the state by how much.
    #[test]
    fn a_mixed_draw_sequence_matches_an_independent_lcg_reimplementation() {
        let mut r = JavaRandom::new(555);
        assert!(r.next_bool());
        assert!((r.next_f32() - 0.068_192_84).abs() < 1e-6);
        assert_eq!(r.next_i32_bound(37), 21);
        assert!((r.next_f64() - 0.009_459_508_527_765_67).abs() < 1e-12);
    }

    #[test]
    fn next_f32_is_in_range_and_quantised_to_24_bits() {
        let mut r = JavaRandom::new(7);
        for _ in 0..10_000 {
            let f = r.next_f32();
            assert!((0.0..1.0).contains(&f), "nextFloat out of range: {f}");
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
                let v = r.next_i32_bound(bound);
                assert!((0..bound).contains(&v), "nextInt({bound}) gave {v}");
            }
        }
    }

    /// `roll` is exactly `next_i32_bound` with the sign stripped; cross-check
    /// them against **two independently seeded** generators (never the same
    /// stream observed twice) so this cannot pass by both sides sharing state.
    #[test]
    fn roll_matches_next_i32_bound() {
        let mut a = JavaRandom::new(777);
        let mut b = JavaRandom::new(777);
        for bound in [1u32, 2, 3, 7, 100, 1968] {
            assert_eq!(a.roll(bound), b.next_i32_bound(bound as i32) as u32);
        }
    }

    /// **A real property of `java.util.Random`, pinned deliberately.**
    ///
    /// Adjacent seeds produce *nearly identical* first draws: the scramble in
    /// [`JavaRandom::new`] only XORs the seed, and `next(24)` takes the
    /// **high** 24 bits of a 48-bit state that adjacent seeds move by just
    /// the multiplier (`≈2.5e10` out of `≈2.8e14`), so the first `nextFloat()`
    /// shifts by only about `1e-4`.
    ///
    /// It matters practically: never seed a per-tick or per-position engine
    /// from a small integer derived from that position — neighbouring seeds
    /// give visibly identical output.
    #[test]
    fn adjacent_seeds_are_correlated() {
        let a = JavaRandom::new(1000).next_f32();
        let b = JavaRandom::new(1001).next_f32();
        assert!(
            (a - b).abs() < 1e-3,
            "expected the known java.util.Random correlation, got {a} vs {b}"
        );
        // A control: seeds far apart are not correlated, so the assertion
        // above is measuring seed adjacency and not simply a broken
        // generator.
        let far = JavaRandom::new(1_000_000_007).next_f32();
        assert!(
            (a - far).abs() > 1e-3,
            "distant seeds should not be correlated: {a} vs {far}"
        );
    }

    #[test]
    fn replaying_the_same_seed_reproduces_the_stream() {
        let draws = |seed| {
            let mut r = JavaRandom::new(seed);
            (0..64).map(|_| r.next_f32()).collect::<Vec<_>>()
        };
        assert_eq!(draws(2024), draws(2024));
        assert_ne!(draws(2024), draws(2025));
    }

    #[test]
    fn set_seed_reproduces_new() {
        let mut a = JavaRandom::new(9001);
        let mut b = JavaRandom::new(0);
        b.set_seed(9001);
        assert_eq!(a.next_i64(), b.next_i64());
    }
}
