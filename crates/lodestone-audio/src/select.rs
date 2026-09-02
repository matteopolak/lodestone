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
//! * [`JavaRandom`] reproduces vanilla's legacy random source — which is
//!   exactly `java.util.Random`. Sound packets carry a `long` seed; vanilla
//!   feeds it through its random-source constructor so that **every client
//!   hearing the event picks the same variant**. To match that, the variant
//!   draw must be this exact LCG.
//!   The type itself is [`lodestone_javarandom::JavaRandom`] — the workspace's
//!   one copy, shared with `lodestone-particle`, `lodestone-render`'s lightning
//!   bolt and `lodestone-shell`'s enchanting-table book — re-exported here so
//!   every existing caller of `lodestone_audio::JavaRandom` keeps working
//!   unchanged. Validated against a real JVM (see `tests/select.rs` and the
//!   committed golden vectors), not against anything lodestone wrote.
//!
//! * [`select_weighted`] is vanilla's weighted-sound-selection walk in
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

pub use lodestone_javarandom::JavaRandom;

/// Selects an index into `weights` with probability proportional to each
/// weight, using vanilla's cumulative-subtraction walk over weighted sound
/// events.
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
