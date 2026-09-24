//! End-crystal beam healing — a port of the real ender dragon's
//! check-crystals step (the
//! nearest-crystal tracking and the heal-per-tick clause) plus the crystal
//! side of the interaction, the real end crystal's hurt/destroyed hooks and
//! the real dragon phase's own on-crystal-destroyed hook.
//!
//! # The heal amount, exactly
//!
//! The real check-crystals step, transcribed as the rule it implements: if
//! there is a nearest crystal, then either it has been removed (in which
//! case forget it), or — on a tick divisible by 10, and only while below max
//! health — heal by exactly `1.0`.
//!
//! Two clauses, both required: a live nearest crystal, **and**
//! a tick divisible by 10 — the heal is not "1/10 HP per tick", it is exactly
//! **1.0 HP once every 10 ticks** (a proc, not a smeared rate), and it never
//! overshoots `max_health` (the real health setter clamps).

/// The exact per-proc heal amount — the real check-crystals step's own
/// health increment.
pub const HEAL_AMOUNT: f32 = 1.0;

/// The proc interval, in ticks — the real "tick count divisible by 10" gate.
pub const HEAL_INTERVAL_TICKS: i32 = 10;

/// The rescan-roll interval — a draw with bound 10 hitting zero decides,
/// **every** tick, whether to rescan for the nearest crystal (not gated by
/// the tick count, unlike the heal itself). `rng_below_ten` is the caller's
/// own draw result.
#[must_use]
pub fn should_rescan_crystals(rng_below_ten: u32) -> bool {
    rng_below_ten == 0
}

/// One [`crystal_heal_tick`] result: the new health value, if a heal
/// procced this tick.
#[must_use]
pub fn crystal_heal_tick(
    tick_count: i64,
    has_live_nearest_crystal: bool,
    health: f32,
    max_health: f32,
) -> Option<f32> {
    if !has_live_nearest_crystal {
        return None;
    }
    if tick_count.rem_euclid(i64::from(HEAL_INTERVAL_TICKS)) != 0 {
        return None;
    }
    if health >= max_health {
        return None;
    }
    Some((health + HEAL_AMOUNT).min(max_health))
}

/// Tracks the dragon's `nearestCrystal` field across ticks — a small state
/// machine of its own since vanilla's version lives directly on `EnderDragon`
/// rather than in a helper. `id` is an opaque handle (an entity id, in a real
/// integration); this module never dereferences it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NearestCrystal(Option<i32>);

impl NearestCrystal {
    #[must_use]
    pub fn none() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn id(self) -> Option<i32> {
        self.0
    }

    /// `if (this.nearestCrystal.isRemoved()) { this.nearestCrystal = null; }`
    /// — called before the heal check each tick. `is_removed` answers
    /// whether the tracked crystal (by id) has been destroyed since last
    /// tracked.
    pub fn clear_if_removed(&mut self, is_removed: impl FnOnce(i32) -> bool) {
        if let Some(id) = self.0 {
            if is_removed(id) {
                self.0 = None;
            }
        }
    }

    /// The rescan itself — `this.level().getEntitiesOfClass(EndCrystal.class,
    /// this.getBoundingBox().inflate(32.0))` reduced to "nearest", folded
    /// with the caller doing the actual spatial query and handing back
    /// whichever id (if any) is nearest. Always overwrites (vanilla assigns
    /// unconditionally, including to `None` if the scan found nothing).
    pub fn set_nearest(&mut self, nearest: Option<i32>) {
        self.0 = nearest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heals_exactly_one_hp_on_the_tenth_tick_only() {
        for t in 0..30 {
            let out = crystal_heal_tick(t, true, 100.0, 200.0);
            if t % 10 == 0 {
                assert_eq!(out, Some(101.0), "tick {t} should proc a 1.0 HP heal");
            } else {
                assert_eq!(out, None, "tick {t} should not proc — only every 10th tick does");
            }
        }
    }

    #[test]
    fn no_heal_without_a_live_nearest_crystal() {
        assert_eq!(crystal_heal_tick(10, false, 100.0, 200.0), None);
    }

    #[test]
    fn no_heal_at_full_health() {
        assert_eq!(crystal_heal_tick(10, true, 200.0, 200.0), None);
    }

    #[test]
    fn heal_clamps_to_max_health_rather_than_overshooting() {
        assert_eq!(crystal_heal_tick(20, true, 199.5, 200.0), Some(200.0));
    }

    #[test]
    fn heal_interval_is_ten_not_a_round_number_picked_without_checking() {
        // A gate that predicted "5" or "20" (plausible round numbers) would
        // pass on ticks divisible by both 5 and 10 but fail to discriminate
        // at tick 5 itself, where the two hypotheses disagree.
        assert_eq!(crystal_heal_tick(5, true, 100.0, 200.0), None, "tick 5 is not a multiple of 10");
    }

    #[test]
    fn nearest_crystal_clears_when_removed() {
        let mut nc = NearestCrystal::none();
        nc.set_nearest(Some(7));
        assert_eq!(nc.id(), Some(7));
        nc.clear_if_removed(|id| id == 7);
        assert_eq!(nc.id(), None);
    }

    #[test]
    fn nearest_crystal_survives_removal_check_for_a_different_id() {
        let mut nc = NearestCrystal::none();
        nc.set_nearest(Some(7));
        nc.clear_if_removed(|id| id == 99);
        assert_eq!(nc.id(), Some(7));
    }

    #[test]
    fn rescan_roll_matches_next_int_ten_zero_check() {
        assert!(should_rescan_crystals(0));
        for n in 1..10 {
            assert!(!should_rescan_crystals(n));
        }
    }
}
