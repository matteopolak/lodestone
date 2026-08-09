//! Experience: the level curve, orb denominations, and a player's XP state.
//!
//! # What it is
//!
//! [`PlayerExperience`] is vanilla's `Player.experienceLevel` /
//! `experienceProgress` / `totalExperience` triple with
//! [`give_points`](PlayerExperience::give_points) transcribed from
//! `Player.giveExperiencePoints`; [`orb_denominations`] is
//! `ExperienceOrb.awardWithDirection`'s splitting loop; [`level_up_cost`] is
//! `Player.getXpNeededForNextLevel`.
//!
//! Everything here is pure arithmetic over integers and one `f32`. There is no
//! entity, no world and no RNG — see "What is not here" for why the orb *entity* is
//! a separate, larger piece.
//!
//! # How it works
//!
//! ## The level curve is three regimes, and the boundaries are the bug
//!
//! `getXpNeededForNextLevel`:
//!
//! ```java
//! if (this.experienceLevel >= 30) {
//!    return 112 + (this.experienceLevel - 30) * 9;
//! } else {
//!    return this.experienceLevel >= 15 ? 37 + (this.experienceLevel - 15) * 5 : 7 + this.experienceLevel * 2;
//! }
//! ```
//!
//! | level | cost of the *next* level |
//! |---|---|
//! | 0 | 7 |
//! | 14 | 35 |
//! | **15** | **37** |
//! | 29 | 107 |
//! | **30** | **112** |
//!
//! The two seams are `>= 15` and `>= 30`, and they are **inclusive** — level 15
//! itself is in the middle regime, level 30 in the top one. Note the curve is
//! *continuous but not smooth*: 14→35 then 15→37 is a step of 2, while inside the
//! middle regime the step is 5. An off-by-one at either seam produces a plausible
//! number, which is what makes this the "frequent source of off-by-one-regime bugs"
//! the issue names. Both boundary values are pinned by name below.
//!
//! ## Orb denominations are a fixed ladder, not a division
//!
//! `ExperienceOrb.getExperienceValue` returns the largest entry of
//! `[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]` that is `<= maxValue`, and
//! `awardWithDirection` loops, subtracting each denomination it takes:
//!
//! ```java
//! while (amount > 0) {
//!    int newCount = getExperienceValue(amount);
//!    amount -= newCount;
//!    …spawn an orb worth newCount…
//! }
//! ```
//!
//! So it is **greedy change-making over an irregular ladder**, not `amount / cap`.
//! An award of 100 becomes `73 + 17 + 7 + 3` — four orbs, and the fourth is `3`
//! rather than `1 + 1 + 1`, because `3` is itself a denomination. A uniform-cap
//! implementation would emit a different *count* of orbs for almost every amount,
//! and orb count is player-visible.
//!
//! The ladder is roughly doubling but not exactly (`3, 7, 17, 37, 73, 149, …`), so
//! it cannot be generated — it is transcribed.
//!
//! ## `give_points` is two `while` loops, and the inner arithmetic is asymmetric
//!
//! ```java
//! this.experienceProgress += (float)i / this.getXpNeededForNextLevel();
//! this.totalExperience = Mth.clamp(this.totalExperience + i, 0, Integer.MAX_VALUE);
//! while (this.experienceProgress >= 1.0F) {
//!    this.experienceProgress = (this.experienceProgress - 1.0F) * this.getXpNeededForNextLevel();
//!    this.giveExperienceLevels(1);
//!    this.experienceProgress = this.experienceProgress / this.getXpNeededForNextLevel();
//! }
//! ```
//!
//! The carry is **re-expressed in the new level's units**: the overflow fraction is
//! multiplied back by the *old* cost to recover points, the level increments, and it
//! is divided by the *new* cost. Skipping the re-expression (just leaving
//! `progress - 1.0`) makes a single large award over-level, because every level past
//! the first would be charged the first level's price.
//!
//! The negative loop is the mirror image and is **not** symmetric: at level 0 it
//! zeroes progress rather than borrowing, so points cannot go below zero.
//!
//! # What is not here, and what it would need
//!
//! **The orb entity.** `ExperienceOrb` is a real entity with a value, an age, a
//! merge rule (`(orb.getId() - id) % 40 == 0 && orb.getValue() == value`, keyed on
//! the *entity id*, which is why it is not reproducible without one), a pickup
//! radius and an absorption animation. `crate::mobs::MobSim` has no orb variant and
//! streams no orb metadata, so nothing here spawns one — [`orb_denominations`] hands
//! back the values an entity spawner *would* use. Adding the entity is a `MobSim`
//! change plus a metadata index, not a change to this module.
//!
//! That split is deliberate rather than a shortcut: the level curve and the
//! denomination ladder are the parts other systems need (smelting, breeding, fishing
//! and enchanting all award *points*), and they are testable to the exact integer
//! without an entity existing.
//!
//! # How to change it
//!
//! * **A new XP source**: call [`PlayerExperience::give_points`] and send the result
//!   with `ServerProtocol::encode_set_experience`. Do not add a second curve.
//! * **Spending XP** (enchanting): vanilla's `onEnchantmentPerformed` subtracts
//!   *levels* and zeroes everything on underflow — [`PlayerExperience::take_levels`].
//! * **Persistence**: `XpP` / `XpLevel` / `XpTotal`, vanilla's own field names
//!   ([`PlayerExperience::restored`]).
//!
//! # Dependencies
//!
//! None. Pure arithmetic, so it is usable from the tick thread, a connection task or
//! a test with no setup.

/// The `experienceLevel` at or above which the top regime applies
/// (`getXpNeededForNextLevel`'s `>= 30`). **Inclusive** — level 30 is in the top
/// regime, not the middle one.
pub const TOP_REGIME_LEVEL: i32 = 30;

/// The `experienceLevel` at or above which the middle regime applies (`>= 15`).
/// Inclusive, for the same reason.
pub const MIDDLE_REGIME_LEVEL: i32 = 15;

/// The denomination ladder from `ExperienceOrb.getExperienceValue`, largest first —
/// the order [`orb_value`] scans it in.
///
/// Transcribed, not generated: the ratios are irregular (`3 → 7` is ×2.33, `7 → 17`
/// is ×2.43, `17 → 37` is ×2.18), so no formula produces it.
pub const ORB_DENOMINATIONS: [i32; 11] = [2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1];

/// The points needed to advance *from* `level` to `level + 1` —
/// `Player.getXpNeededForNextLevel`.
///
/// A negative level is treated as `0`, which is where
/// [`PlayerExperience`]'s own clamping keeps it anyway; returning something for a
/// nonsense input beats panicking on a value read off disk.
#[must_use]
pub fn level_up_cost(level: i32) -> i32 {
    let level = level.max(0);
    if level >= TOP_REGIME_LEVEL {
        112 + (level - TOP_REGIME_LEVEL) * 9
    } else if level >= MIDDLE_REGIME_LEVEL {
        37 + (level - MIDDLE_REGIME_LEVEL) * 5
    } else {
        7 + level * 2
    }
}

/// The total points needed to reach `level` from zero — the running sum of
/// [`level_up_cost`].
///
/// Not in vanilla as a function; vanilla only ever adds points incrementally. It is
/// here because it is the natural way to state an expected value in a gate ("30
/// levels is 1395 points") without restating the curve.
#[must_use]
pub fn total_points_for_level(level: i32) -> i32 {
    (0..level.max(0)).map(level_up_cost).sum()
}

/// The largest denomination not exceeding `max_value` —
/// `ExperienceOrb.getExperienceValue`.
///
/// `0` for a non-positive input, which vanilla never asks for (its caller's `while
/// (amount > 0)` guards it) but which a Rust caller can.
#[must_use]
pub fn orb_value(max_value: i32) -> i32 {
    if max_value <= 0 {
        return 0;
    }
    ORB_DENOMINATIONS
        .into_iter()
        .find(|&denomination| max_value >= denomination)
        .unwrap_or(1)
}

/// The orb values `ExperienceOrb.awardWithDirection` would spawn for `amount`, in
/// the order it spawns them (largest first).
///
/// Greedy over [`ORB_DENOMINATIONS`], **not** `amount / cap`: 100 becomes
/// `[73, 17, 7, 3]`, four orbs, and the last is `3` because `3` is a denomination in
/// its own right. Orb count is player-visible, so this is a behavioural difference
/// rather than a representational one.
///
/// Vanilla's loop also tries to merge into a nearby existing orb before spawning
/// (`tryMergeToExisting`), which depends on entity ids and a `nextInt(40)` draw —
/// see the module doc's "What is not here". This function is the pre-merge
/// denomination list, which is what a spawner needs as input.
#[must_use]
pub fn orb_denominations(amount: i32) -> Vec<i32> {
    let mut out = Vec::new();
    let mut remaining = amount;
    while remaining > 0 {
        let value = orb_value(remaining);
        remaining -= value;
        out.push(value);
    }
    out
}

/// One player's experience — vanilla's three fields, which are genuinely three
/// pieces of state rather than one derived from another.
///
/// `total` is *not* recoverable from `level` and `progress`: vanilla clamps it at
/// zero on a level underflow and never recomputes it, so a player who has spent XP
/// enchanting has a `total` that no longer matches their level. It is the "score"
/// number, and it is what the wire carries.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerExperience {
    level: i32,
    progress: f32,
    total: i32,
}

impl PlayerExperience {
    /// The player's level (`experienceLevel`).
    #[must_use]
    pub fn level(&self) -> i32 {
        self.level
    }

    /// Progress towards the next level, `0.0..1.0` (`experienceProgress`) — what the
    /// XP bar fills to.
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.progress
    }

    /// Lifetime points (`totalExperience`). See this type's own doc for why it is not
    /// derived.
    #[must_use]
    pub fn total(&self) -> i32 {
        self.total
    }

    /// The points needed to reach the next level from here.
    #[must_use]
    pub fn next_level_cost(&self) -> i32 {
        level_up_cost(self.level)
    }

    /// Awards `points` — `Player.giveExperiencePoints`, transcribed including the
    /// carry re-expression that makes a large single award level correctly.
    ///
    /// Returns the number of levels gained (`0` if the award only moved the bar), so
    /// a caller can decide whether to play the level-up sound without diffing the
    /// level itself.
    ///
    /// A negative `points` runs vanilla's downward loop, which **zeroes rather than
    /// borrowing** at level 0.
    pub fn give_points(&mut self, points: i32) -> i32 {
        let before = self.level;
        self.progress += points as f32 / self.next_level_cost() as f32;
        self.total = self.total.saturating_add(points).max(0);

        while self.progress < 0.0 {
            let remaining = self.progress * self.next_level_cost() as f32;
            if self.level > 0 {
                self.level -= 1;
                self.progress = 1.0 + remaining / self.next_level_cost() as f32;
            } else {
                // Vanilla's `giveExperienceLevels(-1)` clamps the level at 0 and
                // zeroes progress *and* total, so a player cannot go into debt.
                self.level = 0;
                self.progress = 0.0;
                self.total = 0;
            }
        }

        while self.progress >= 1.0 {
            // The carry, in points rather than in fractions — see the module doc.
            // Multiplying by the *old* cost recovers the overflow as points, then the
            // level increments, then it is divided by the *new* cost. Leaving it as
            // `progress - 1.0` charges every subsequent level the first one's price.
            self.progress = (self.progress - 1.0) * self.next_level_cost() as f32;
            self.level = self.level.saturating_add(1);
            self.progress /= self.next_level_cost() as f32;
        }

        self.level - before
    }

    /// Spends `levels` — `Player.onEnchantmentPerformed`, which zeroes progress and
    /// total on underflow rather than clamping only the level.
    pub fn take_levels(&mut self, levels: i32) {
        self.level -= levels;
        if self.level < 0 {
            self.level = 0;
            self.progress = 0.0;
            self.total = 0;
        }
    }

    /// Resets to a fresh player. Vanilla's respawn drops XP entirely (there is no
    /// `keepInventory`-style rule for it in the base game).
    pub fn respawn(&mut self) {
        *self = Self::default();
    }

    /// Rebuilds from saved player NBT — vanilla's `XpLevel` / `XpP` / `XpTotal`.
    ///
    /// Clamped rather than trusted, for [`crate::vitals::PlayerVitals::restored`]'s
    /// reason: a `progress` outside `0.0..1.0` read off disk would make
    /// [`give_points`](Self::give_points)'s loops run on the very first award, which
    /// is a level jump the player did not earn.
    #[must_use]
    pub fn restored(level: i32, progress: f32, total: i32) -> Self {
        Self {
            level: level.max(0),
            // `< 1.0` rather than `<= 1.0`: exactly 1.0 is the state the carry loop
            // exists to resolve, so it is not a legal resting value.
            progress: if progress.is_finite() {
                progress.clamp(0.0, 0.999_999)
            } else {
                0.0
            },
            total: total.max(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The three regimes at their exact boundaries.** These are the values an
    /// off-by-one produces a *plausible* wrong answer for, so each seam is asserted
    /// on both sides rather than sampled in the middle.
    ///
    /// Derived from the transcribed record definition, not from a run:
    ///
    /// | level | regime | cost |
    /// |---|---|---|
    /// | 0 | low, `7 + 0*2` | 7 |
    /// | 14 | low, `7 + 14*2` | 35 |
    /// | 15 | **middle**, `37 + 0*5` | 37 |
    /// | 29 | middle, `37 + 14*5` | 107 |
    /// | 30 | **top**, `112 + 0*9` | 112 |
    /// | 31 | top, `112 + 1*9` | 121 |
    #[test]
    fn the_level_curve_switches_regime_at_fifteen_and_thirty_inclusive() {
        assert_eq!(level_up_cost(0), 7);
        assert_eq!(level_up_cost(1), 9);
        assert_eq!(level_up_cost(14), 35, "still the low regime at 14");
        assert_eq!(level_up_cost(15), 37, "the middle regime starts AT 15, not after it");
        assert_eq!(level_up_cost(29), 107, "still the middle regime at 29");
        assert_eq!(level_up_cost(30), 112, "the top regime starts AT 30, not after it");
        assert_eq!(level_up_cost(31), 121);

        // The seams are steps, not smooth joins — which is why an off-by-one is
        // hard to spot by eye. Inside the low regime the step is 2 and inside the
        // middle regime it is 5, but *across* the seam it is 2 again.
        assert_eq!(level_up_cost(15) - level_up_cost(14), 2);
        assert_eq!(level_up_cost(16) - level_up_cost(15), 5);
        assert_eq!(level_up_cost(30) - level_up_cost(29), 5);
        assert_eq!(level_up_cost(31) - level_up_cost(30), 9);

        // **The wrong hypothesis, asserted negatively**: exclusive boundaries would
        // put level 15 in the low regime at 37... no — they would give
        // `7 + 15*2 = 37` too, which is *the same number*. That coincidence is
        // exactly why this bug survives, so the discriminating level is 16:
        // exclusive seams give `7 + 16*2 = 39`, inclusive give `37 + 5 = 42`.
        assert_eq!(level_up_cost(16), 42, "exclusive boundaries would give 39 here");
        assert_ne!(level_up_cost(16), 39);
        // And at the top seam, level 31: exclusive gives `37 + 16*5 = 117`.
        assert_ne!(level_up_cost(31), 117);
    }

    /// The running total, which is the figure most external references quote: 30
    /// levels is **1395** points and level 1 is 7.
    #[test]
    fn thirty_levels_costs_exactly_1395_points() {
        assert_eq!(total_points_for_level(0), 0);
        assert_eq!(total_points_for_level(1), 7);
        assert_eq!(total_points_for_level(15), 315);
        assert_eq!(total_points_for_level(30), 1395);
    }

    /// **Greedy change-making over the irregular ladder, not a division.**
    ///
    /// The two hypotheses for an award of 100:
    ///
    /// | hypothesis | orbs |
    /// |---|---|
    /// | greedy over the ladder (correct) | `[73, 17, 7, 3]` — **four** |
    /// | uniform cap of 2477 | `[100]` — **one** |
    ///
    /// Orb count is player-visible, so this is behaviour rather than representation.
    /// The `3` at the end is the detail a hand-written ladder drops: it is a
    /// denomination in its own right, so the tail is not `1 + 1 + 1`.
    #[test]
    fn orb_splitting_is_greedy_over_the_irregular_ladder() {
        assert_eq!(orb_denominations(100), vec![73, 17, 7, 3]);
        assert_ne!(orb_denominations(100), vec![100], "not a single orb");
        assert_eq!(orb_denominations(1), vec![1]);
        assert_eq!(orb_denominations(2), vec![1, 1], "2 is not a denomination");
        assert_eq!(orb_denominations(3), vec![3], "but 3 is");
        assert_eq!(orb_denominations(6), vec![3, 3]);
        assert_eq!(orb_denominations(7), vec![7]);
        // A very large award repeats the top denomination, which is the only place
        // the ladder behaves like a cap.
        assert_eq!(
            orb_denominations(5_000),
            vec![2477, 2477, 37, 7, 1, 1],
            "the top denomination repeats, then the ladder resumes"
        );
        assert_eq!(orb_denominations(0), Vec::<i32>::new());
        assert_eq!(orb_denominations(-5), Vec::<i32>::new());
    }

    /// **The conservation invariant**, which is the property that would catch a
    /// wrong ladder even where the count happened to match: the orbs must sum to the
    /// award, for every amount in a wide sweep. Checked over `1..=3000` — 3000
    /// independent claims, and any denomination transcribed wrong breaks the sum
    /// somewhere in that range.
    #[test]
    fn every_split_sums_back_to_the_award() {
        for amount in 1..=3_000 {
            let orbs = orb_denominations(amount);
            assert_eq!(
                orbs.iter().sum::<i32>(),
                amount,
                "split of {amount} was {orbs:?}"
            );
            assert!(!orbs.is_empty());
            // Largest first, always — the spawn order is what a client sees.
            assert!(
                orbs.windows(2).all(|w| w[0] >= w[1]),
                "split of {amount} is not descending: {orbs:?}"
            );
        }
    }

    /// **The carry re-expression**, and the reason `give_points` is not
    /// `progress - 1.0`.
    ///
    /// Award exactly `total_points_for_level(30) = 1395` points to a fresh player in
    /// **one** call. The correct answer is level **30** with progress `0.0`; an
    /// implementation that left the overflow as a bare fraction charges every level
    /// the first level's price of 7 and over-levels badly.
    ///
    /// Both hypotheses are computed from outside constants: 30 is the running sum
    /// above, and the naive figure would be far higher.
    #[test]
    fn one_large_award_lands_on_exactly_the_level_the_running_sum_predicts() {
        let mut xp = PlayerExperience::default();
        let gained = xp.give_points(total_points_for_level(30));
        assert_eq!(xp.level(), 30, "1395 points is exactly 30 levels");
        assert_eq!(gained, 30, "give_points must report the levels gained");
        assert!(
            xp.progress() < 1e-4,
            "landing exactly on a level boundary leaves an empty bar, got {}",
            xp.progress()
        );
        assert_eq!(xp.total(), 1395);
    }

    /// Incremental awards must reach the same place as one big one — which is the
    /// cross-check that the carry is arithmetic rather than an approximation.
    ///
    /// 1395 points delivered as 1395 separate single points must also land on level
    /// 30. Float progress accumulates differently, so the level is asserted exactly
    /// and the bar within a tolerance.
    #[test]
    fn awarding_one_point_at_a_time_reaches_the_same_level() {
        let mut xp = PlayerExperience::default();
        for _ in 0..total_points_for_level(30) {
            xp.give_points(1);
        }
        assert_eq!(
            xp.level(),
            30,
            "1395 single points must reach the same level as one award of 1395"
        );
        assert_eq!(xp.total(), 1395);
    }

    /// The first level costs 7, so 7 points is exactly one level and 6 is none.
    /// The smallest possible off-by-one in the curve shows up here.
    #[test]
    fn seven_points_is_exactly_one_level_and_six_is_none() {
        let mut six = PlayerExperience::default();
        assert_eq!(six.give_points(6), 0);
        assert_eq!(six.level(), 0);
        assert!((six.progress() - 6.0 / 7.0).abs() < 1e-5);

        let mut seven = PlayerExperience::default();
        assert_eq!(seven.give_points(7), 1);
        assert_eq!(seven.level(), 1);
        assert!(seven.progress() < 1e-4);
    }

    /// Spending levels zeroes progress and total on underflow rather than clamping
    /// only the level — vanilla's `onEnchantmentPerformed`, and the asymmetry a
    /// "clamp at zero" implementation gets wrong (it would leave a full bar at
    /// level 0).
    #[test]
    fn spending_more_levels_than_you_have_zeroes_everything() {
        let mut xp = PlayerExperience::default();
        xp.give_points(total_points_for_level(5) + 3);
        assert_eq!(xp.level(), 5);
        assert!(xp.progress() > 0.0, "the bar is partly full before spending");

        xp.take_levels(99);
        assert_eq!(xp.level(), 0);
        assert_eq!(xp.progress(), 0.0, "a partial bar must not survive an underflow");
        assert_eq!(xp.total(), 0);

        // The control: an affordable spend leaves progress alone.
        let mut afford = PlayerExperience::default();
        afford.give_points(total_points_for_level(5) + 3);
        let bar = afford.progress();
        afford.take_levels(2);
        assert_eq!(afford.level(), 3);
        assert_eq!(afford.progress(), bar, "an affordable spend must not touch the bar");
    }

    /// A negative award borrows a level, and at level 0 it zeroes instead — so a
    /// player cannot go into XP debt.
    #[test]
    fn a_negative_award_borrows_a_level_and_never_goes_below_zero() {
        let mut xp = PlayerExperience::default();
        xp.give_points(total_points_for_level(3));
        assert_eq!(xp.level(), 3);
        assert_eq!(xp.give_points(-1), -1, "one point below a boundary drops a level");
        assert_eq!(xp.level(), 2);
        assert!(xp.progress() > 0.9, "and leaves the bar nearly full: {}", xp.progress());

        let mut broke = PlayerExperience::default();
        broke.give_points(-1_000);
        assert_eq!(broke.level(), 0);
        assert_eq!(broke.progress(), 0.0);
        assert_eq!(broke.total(), 0);
    }

    /// `restored` clamps, and in particular a `progress` of exactly `1.0` off disk is
    /// **not** a legal resting value: it is the state the carry loop exists to
    /// resolve, so keeping it would level the player up on their next award of zero.
    #[test]
    fn restored_clamps_progress_below_one() {
        let xp = PlayerExperience::restored(-3, 5.0, -7);
        assert_eq!(xp.level(), 0);
        assert!(xp.progress() < 1.0, "progress must be strictly below 1.0");
        assert_eq!(xp.total(), 0);

        let nan = PlayerExperience::restored(4, f32::NAN, 10);
        assert_eq!(nan.progress(), 0.0, "a NaN off disk must not poison the bar");
        assert_eq!(nan.level(), 4);
    }
}
