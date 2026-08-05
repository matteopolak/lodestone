//! Sound-variant selection: the vanilla-exact seeded RNG plus the weighted walk.
//!
//! # Why this lives here, and what stays in `lodestone-assets`
//!
//! The *sound-event graph* — parsing `sounds.json`, following `type: event`
//! chains, the cycle guard, mapping a version's event name to a file — lives in
//! `lodestone-assets` ([`SoundRegistry::resolve`]), and must, because it needs
//! the registry, `ResourceLocation`, and pack-stacking knowledge that this
//! device-free audio crate deliberately does not depend on. Assets already does
//! the weighted selection over that graph, verified against the real 26.2
//! registry.
//!
//! What genuinely belongs *here* is the piece that makes that selection match
//! vanilla **bit-for-bit for seeded sounds**, and the generic mechanism under
//! it:
//!
//! * [`JavaRandom`] reproduces vanilla's `LegacyRandomSource` — which is exactly
//!   `java.util.Random` (26.2 `LegacyRandomSource`/`BitRandomSource`). Sound
//!   packets carry a `long` seed (`ClientboundSoundPacket`/
//!   `ClientboundSoundEntityPacket`); vanilla feeds it through
//!   `RandomSource.create(seed)` so that **every client hearing the event picks
//!   the same variant**. To match that, the variant draw must be this exact LCG.
//!   Validated against a real JVM (see `tests/select.rs` and the committed
//!   golden vectors), not against anything lodestone wrote.
//!
//! * [`select_weighted`] is vanilla's `WeighedSoundEvents.getSound` walk in
//!   version-free, asset-free form: draw `roll ∈ [0, total)` and subtract each
//!   weight until the running index goes negative. It operates on a plain
//!   `&[u32]` of weights so it carries no sound or protocol knowledge.
//!
//! The seam between the two: the client adapter builds a `roll` closure from a
//! [`JavaRandom`] (seeded from the packet seed for networked sounds, or from an
//! injected value for client-predicted/UI sounds — never from `Instant::now`,
//! which panics on wasm, nor `getrandom`, which drags a wasm-unsafe dependency)
//! and hands it to `SoundRegistry::resolve`. Then variant selection is both
//! deterministic for tests and vanilla-exact in multiplayer.
//!
//! [`SoundRegistry::resolve`]: https://docs.rs/lodestone-assets
//!
//! ## The `type: event` weight rule (a spec, not a guess)
//!
//! In a parent event, an entry of `type: event` contributes the **referenced
//! event's total weight** to the parent's selection sum — not its own declared
//! `weight` — because vanilla's delegating `Weighted` reports the target's
//! summed weight. When a caller flattens an event's entries into the `&[u32]`
//! passed here, an event-ref entry's slot must therefore hold the referenced
//! event's total weight. Selecting that slot then delegates to a *fresh* draw
//! within the referenced event (a second `roll`), exactly as vanilla recurses.
//! `select_weighted` models one level; the chained descent stays in assets.

/// Vanilla's `LegacyRandomSource`, bit-for-bit identical to `java.util.Random`.
///
/// A 48-bit linear congruential generator. Used for sound-variant selection so
/// that seeded (packet-driven) sounds pick the same variant as vanilla clients.
/// Pure integer math: no time source, no `getrandom`, so it compiles and runs
/// identically on native and `wasm32`.
///
/// Constants and algorithm transcribed from 26.2
/// `net.minecraft.world.level.levelgen.LegacyRandomSource` and
/// `BitRandomSource`.
#[derive(Debug, Clone)]
pub struct JavaRandom {
    /// The 48-bit scrambled state, held in an `i64` (always in `[0, 2^48)`).
    seed: i64,
}

impl JavaRandom {
    const MULTIPLIER: i64 = 0x5DEECE66D;
    const INCREMENT: i64 = 0xB;
    const MASK: i64 = (1 << 48) - 1;

    /// Creates a generator seeded exactly as `new java.util.Random(seed)` /
    /// `RandomSource.create(seed)`: the seed is scrambled by
    /// `(seed ^ 0x5DEECE66D) & (2^48 - 1)`.
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    /// Reseeds in place, matching `Random.setSeed`.
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ Self::MULTIPLIER) & Self::MASK;
    }

    /// The LCG core: advances the state and returns the top `bits` bits, exactly
    /// as `LegacyRandomSource.next(bits)`. `bits` must be in `1..=32`.
    fn next(&mut self, bits: u32) -> i32 {
        debug_assert!((1..=32).contains(&bits));
        self.seed = self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT)
            & Self::MASK;
        // `seed` is non-negative (masked to 48 bits), so the shift is logical.
        // Casting the low bits to i32 reproduces Java's `(int)` truncation,
        // which for bits == 32 yields a signed (possibly negative) result.
        (self.seed >> (48 - bits)) as i32
    }

    /// A uniformly distributed `i32`, matching `Random.nextInt()`.
    pub fn next_i32(&mut self) -> i32 {
        self.next(32)
    }

    /// A uniform `i32` in `[0, bound)`, matching `Random.nextInt(bound)` exactly,
    /// including the power-of-two fast path and the modulo-rejection loop that
    /// removes bias at the top of the range.
    ///
    /// Panics if `bound <= 0`, as vanilla throws `IllegalArgumentException`.
    pub fn next_i32_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            // Power of two: take the high 31 bits scaled — no rejection needed.
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let sample = self.next(31);
            let modulo = sample % bound;
            // Vanilla's overflow guard, in wrapping i32 to match Java `int`:
            // retry while `sample - modulo + (bound - 1)` overflows negative.
            if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
                return modulo;
            }
        }
    }

    /// A uniform `i64`, matching `Random.nextLong()`:
    /// `((long) next(32) << 32) + next(32)`.
    pub fn next_i64(&mut self) -> i64 {
        let hi = self.next(32) as i64;
        let lo = self.next(32) as i64;
        (hi << 32).wrapping_add(lo)
    }

    /// A uniform `f64` in `[0, 1)`, matching `Random.nextDouble()`:
    /// `(((long) next(26) << 27) + next(27)) * 2^-53`.
    ///
    /// Needed by biome ambient *additions*, which fire on
    /// `random.nextDouble() < tick_chance` (`BiomeAmbientSoundsHandler.java:65`)
    /// against chances as small as `0.0111`. Note this consumes **two** LCG steps,
    /// so substituting `next_i32_bound` scaled would desync any shared stream.
    pub fn next_f64(&mut self) -> f64 {
        let hi = (self.next(26) as i64) << 27;
        let lo = self.next(27) as i64;
        (hi + lo) as f64 * (1.0 / (1i64 << 53) as f64)
    }

    /// A uniform `f32` in `[0, 1)`, matching `Random.nextFloat()`:
    /// `next(24) / 2^24`. One LCG step, unlike [`JavaRandom::next_f64`].
    ///
    /// Vanilla uses it for the swim-sound pitch jitter
    /// `1.0 + (nextFloat() - nextFloat()) * 0.4` (`Entity.java:1490`), which draws
    /// twice and is therefore order-sensitive.
    pub fn next_f32(&mut self) -> f32 {
        self.next(24) as f32 / (1i32 << 24) as f32
    }

    /// A `roll`-shaped draw for weighted selection: a uniform value in
    /// `[0, bound)`, the signature `lodestone-assets`' `resolve` expects.
    ///
    /// `bound` must be `>= 1` and `<= i32::MAX`. The upper bound is not a
    /// limitation versus vanilla: vanilla's total weight is a Java `int`, so it
    /// is already confined to `i32::MAX`, and real sound events sum to a few
    /// thousand at most.
    pub fn roll(&mut self, bound: u32) -> u32 {
        debug_assert!(bound >= 1, "roll bound must be >= 1");
        debug_assert!(bound <= i32::MAX as u32, "roll bound exceeds i32::MAX");
        self.next_i32_bound(bound as i32) as u32
    }
}

/// Selects an index into `weights` with probability proportional to each
/// weight, using vanilla's cumulative-subtraction walk
/// (`WeighedSoundEvents.getSound`).
///
/// `roll` must return a uniform value in `[0, total)` where `total` is the sum
/// of `weights` (capped at `u32::MAX`); pass [`JavaRandom::roll`] for vanilla
/// parity, or any deterministic closure in tests. Returns `None` only when the
/// total weight is zero (vanilla's "empty sound"), matching assets.
///
/// This is the generic, version-free companion to
/// `lodestone-assets`' event-graph selection. The two currently hold separate
/// copies of the same walk; see the module docs — they must not be allowed to
/// diverge.
pub fn select_weighted(weights: &[u32], roll: &mut impl FnMut(u32) -> u32) -> Option<usize> {
    let total: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    if total == 0 {
        return None;
    }
    let capped = total.min(u64::from(u32::MAX)) as u32;
    let mut index = u64::from(roll(capped));
    for (i, &w) in weights.iter().enumerate() {
        let w = u64::from(w);
        if index < w {
            return Some(i);
        }
        index -= w;
    }
    // Unreachable for a well-behaved `roll` (result < total). Vanilla returns
    // EMPTY_SOUND here; we return the last index, matching assets' resolve.
    debug_assert!(false, "roll returned a value >= total weight");
    Some(weights.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_java_random_first_values() {
        // Cross-check against the single most-cited java.util.Random datum:
        // new Random(0).nextInt() == -1155484576. (Also in the JVM golden file.)
        let mut r = JavaRandom::new(0);
        assert_eq!(r.next_i32(), -1155484576);
        assert_eq!(r.next_i32(), -723955400);
    }

    #[test]
    fn power_of_two_bound_uses_fast_path() {
        // Fast path must still be deterministic and in-range.
        let mut r = JavaRandom::new(42);
        for _ in 0..1000 {
            let v = r.next_i32_bound(16);
            assert!((0..16).contains(&v));
        }
    }

    #[test]
    fn roll_matches_next_i32_bound() {
        let mut a = JavaRandom::new(777);
        let mut b = JavaRandom::new(777);
        for bound in [1u32, 2, 3, 7, 100, 1968] {
            assert_eq!(a.roll(bound), b.next_i32_bound(bound as i32) as u32);
        }
    }

    #[test]
    fn select_weighted_scripted_roll_hits_exact_boundaries() {
        // weights [2,3,5], total 10, cumulative bounds: [0,2)->0 [2,5)->1 [5,10)->2
        let w = [2u32, 3, 5];
        let cases = [
            (0u32, 0usize),
            (1, 0),
            (2, 1), // first index of entry 1 — off-by-one teeth
            (4, 1),
            (5, 2), // first index of entry 2
            (9, 2),
        ];
        for (roll_val, expected) in cases {
            let mut roll = |_total: u32| roll_val;
            assert_eq!(
                select_weighted(&w, &mut roll),
                Some(expected),
                "roll={roll_val}"
            );
        }
    }

    #[test]
    fn select_weighted_zero_total_is_none() {
        let mut roll = |_t: u32| 0u32;
        assert_eq!(select_weighted(&[], &mut roll), None);
        assert_eq!(select_weighted(&[0, 0], &mut roll), None);
    }

    #[test]
    fn select_weighted_event_ref_contributes_target_total() {
        // A parent with a file entry (weight 1) and an event-ref whose target
        // event totals 9. Per the spec the ref slot holds 9, so P(ref) = 9/10.
        // Roll 0 -> file (index 0); roll 1..=9 -> the ref slot (index 1).
        let flattened = [1u32, 9];
        let mut roll0 = |_t: u32| 0u32;
        assert_eq!(select_weighted(&flattened, &mut roll0), Some(0));
        for r in 1u32..=9 {
            let mut roll = |_t: u32| r;
            assert_eq!(select_weighted(&flattened, &mut roll), Some(1), "roll={r}");
        }
    }
}
