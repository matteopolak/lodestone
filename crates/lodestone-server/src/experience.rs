//! Experience: the level curve, orb denominations, and a player's XP state.
//!
//! # What it is
//!
//! [`PlayerExperience`] is vanilla's own level / progress / total-points triple, with
//! [`give_points`](PlayerExperience::give_points) transcribed from the real
//! give-experience-points rule; [`orb_denominations`] is the real orb-award
//! splitting loop; [`level_up_cost`] is the real cost-of-next-level rule.
//!
//! Everything here is pure arithmetic over integers and one `f32`. There is no
//! entity, no world and no RNG — see "What is not here" for why the orb *entity* is
//! a separate, larger piece.
//!
//! # How it works
//!
//! ## The level curve is three regimes, and the boundaries are the bug
//!
//! The real cost-of-next-level rule is a three-way branch on the *current* level:
//! at or above 30 the cost is `112 + (level - 30) * 9`; otherwise, at or above 15,
//! it is `37 + (level - 15) * 5`; otherwise it is `7 + level * 2`.
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
//! The real largest-denomination rule returns the largest entry of
//! `[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]` that is `<= maxValue`, and the
//! real orb-award rule loops, subtracting each denomination it takes, until nothing
//! is left to spawn.
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
//! The real give-experience-points rule adds the points to progress as a fraction of
//! the current level's cost, clamps total points into range, and then — while
//! progress is `>= 1.0` — re-expresses the overflow in points at the *old* cost,
//! increments the level, and divides back down by the *new* cost.
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
//! * **Spending XP** (enchanting): the real enchantment-cost-spend rule subtracts
//!   *levels* and zeroes everything on underflow — [`PlayerExperience::take_levels`].
//! * **Persistence**: `XpP` / `XpLevel` / `XpTotal`, vanilla's own field names
//!   ([`PlayerExperience::restored`]).
//!
//! # Dependencies
//!
//! None. Pure arithmetic, so it is usable from the tick thread, a connection task or
//! a test with no setup.

/// The level at or above which the top regime of the real cost-of-next-level rule
/// applies. **Inclusive** — level 30 is in the top regime, not the middle one.
pub const TOP_REGIME_LEVEL: i32 = 30;

/// The level at or above which the middle regime applies (`>= 15`). Inclusive, for
/// the same reason.
pub const MIDDLE_REGIME_LEVEL: i32 = 15;

/// The denomination ladder from the real largest-denomination rule, largest first —
/// the order [`orb_value`] scans it in.
///
/// Transcribed, not generated: the ratios are irregular (`3 → 7` is ×2.33, `7 → 17`
/// is ×2.43, `17 → 37` is ×2.18), so no formula produces it.
pub const ORB_DENOMINATIONS: [i32; 11] = [2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1];

/// The points needed to advance *from* `level` to `level + 1` — the real
/// cost-of-next-level rule.
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

/// The largest denomination not exceeding `max_value` — the real
/// largest-denomination rule.
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

/// The orb values the real orb-award rule would spawn for `amount`, in the order it
/// spawns them (largest first).
///
/// Greedy over [`ORB_DENOMINATIONS`], **not** `amount / cap`: 100 becomes
/// `[73, 17, 7, 3]`, four orbs, and the last is `3` because `3` is a denomination in
/// its own right. Orb count is player-visible, so this is a behavioural difference
/// rather than a representational one.
///
/// Vanilla's loop also tries to merge into a nearby existing orb before spawning,
/// which depends on entity ids and a `next_int(40)` draw — see the module doc's
/// "What is not here". This function is the pre-merge denomination list, which is
/// what a spawner needs as input.
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

/// The `[min, max]` points a broken block pops, or `None` for a block that pops none —
/// the real per-block XP-range registration, plus the two block kinds that pass their
/// own range at the call site.
///
/// # Where the numbers come from
///
/// The block registrations themselves, read as record definitions: a uniform
/// `(3, 7)` range for diamond ore and so on. The deepslate variant of every ore is
/// registered with the same range as the stone one, which is why they appear in the
/// same arms.
///
/// | range | blocks | registration |
/// |---|---|---|
/// | none | iron, gold, copper ore (and deepslate) | constant `0` |
/// | `0..=1` | nether gold ore | uniform `(0, 1)` |
/// | `0..=2` | coal ore, deepslate coal ore | uniform `(0, 2)` |
/// | `1..=5` | redstone ore, deepslate redstone ore | uniform `(1, 5)` |
/// | `2..=5` | lapis ore, nether quartz ore (and deepslate lapis) | uniform `(2, 5)` |
/// | `3..=7` | diamond ore, emerald ore (and deepslate) | uniform `(3, 7)` |
/// | `15..=43` | spawner | two draws of `15 + next_int(15)` summed |
///
/// **The zero-XP ores are the trap.** Iron, gold and copper ore *are* registered
/// with the same XP-range mechanism as every paying ore — they just pass a constant
/// `0`, because they drop raw ore rather than a finished resource. "It carries an
/// XP-range registration, so it drops experience" is the wrong inference, and the
/// number of blocks it is wrong for is six.
///
/// The spawner is not a uniform range at all: two independent `nextInt(15)` draws sum
/// to a **triangular** distribution over `15..=43`, so a single `next_int(29) + 15`
/// would produce the right bounds with the wrong shape. [`block_break_points`] draws it
/// as two.
#[must_use]
pub fn block_break_experience_range(block_name: &str) -> Option<(i32, i32)> {
    let name = block_name
        .split_once('[')
        .map_or(block_name, |(name, _)| name)
        .trim();
    let path = name.split_once(':').map_or(name, |(_, path)| path);
    match path {
        "coal_ore" | "deepslate_coal_ore" => Some((0, 2)),
        "nether_gold_ore" => Some((0, 1)),
        "redstone_ore" | "deepslate_redstone_ore" => Some((1, 5)),
        "lapis_ore" | "deepslate_lapis_ore" | "nether_quartz_ore" => Some((2, 5)),
        "diamond_ore" | "deepslate_diamond_ore" | "emerald_ore" | "deepslate_emerald_ore" => {
            Some((3, 7))
        }
        "spawner" => Some((SPAWNER_XP_BASE, SPAWNER_XP_BASE + 2 * (SPAWNER_XP_DRAW_BOUND - 1))),
        _ => None,
    }
}

/// The real spawner-break XP rule's constant term.
const SPAWNER_XP_BASE: i32 = 15;

/// The bound of each of the spawner's **two** bounded-random-int draws.
const SPAWNER_XP_DRAW_BOUND: i32 = 15;

/// One roll of a broken block's experience, in vanilla's own draw shape.
///
/// The real uniform-sample rule is `min + next_int(max - min + 1)`; dropping the
/// `+ 1` makes the top of every range unreachable, which no "does mining an ore
/// give XP" assertion could see. The spawner is the exception and takes two draws —
/// see [`block_break_experience_range`].
///
/// `next_int` is passed as a closure rather than an RNG type so this stays free of
/// `crate::mob_spawn`, keeping this module's "pure arithmetic, no dependencies"
/// property.
#[must_use]
pub fn block_break_points(block_name: &str, mut next_int: impl FnMut(i32) -> i32) -> i32 {
    let Some((min, max)) = block_break_experience_range(block_name) else {
        return 0;
    };
    if min == SPAWNER_XP_BASE && max > SPAWNER_XP_BASE {
        return SPAWNER_XP_BASE
            + next_int(SPAWNER_XP_DRAW_BOUND)
            + next_int(SPAWNER_XP_DRAW_BOUND);
    }
    min + next_int(max - min + 1)
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
    /// The player's level.
    #[must_use]
    pub fn level(&self) -> i32 {
        self.level
    }

    /// Progress towards the next level, `0.0..1.0` — what the XP bar fills to.
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.progress
    }

    /// Lifetime points. See this type's own doc for why it is not derived.
    #[must_use]
    pub fn total(&self) -> i32 {
        self.total
    }

    /// The points needed to reach the next level from here.
    #[must_use]
    pub fn next_level_cost(&self) -> i32 {
        level_up_cost(self.level)
    }

    /// `/xp query <target> points`'s own reading — **not** [`total`](Self::total).
    /// The real points-query rule floors `progress * next_level_cost`: points
    /// *within the current level*, i.e. how far the bar has filled, converted
    /// back to an integer count. A player who has spent XP enchanting can have
    /// a `total` far below what their level implies, which is exactly why this
    /// is a separate reading rather than a partial application of it.
    #[must_use]
    pub fn query_points(&self) -> i32 {
        (self.progress * self.next_level_cost() as f32).floor() as i32
    }

    /// Awards `points` — the real give-experience-points rule, transcribed including
    /// the carry re-expression that makes a large single award level correctly.
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
                // Losing a level below 0 clamps the level at 0 and zeroes progress
                // *and* total, so a player cannot go into debt.
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

    /// Spends `levels` — the real enchantment-cost-spend rule, which zeroes progress
    /// and total on underflow rather than clamping only the level.
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
    /// only the level — the real enchantment-cost-spend rule, and the asymmetry a
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

    /// **The six ores that pop no experience**, which is the arm a "it carries an
    /// XP-range registration, so it drops experience" reading gets wrong.
    ///
    /// Iron, gold and copper ore — and all three deepslate variants — are registered
    /// with the same XP-range mechanism as every paying ore, with a constant `0`,
    /// because they drop raw ore rather than a finished resource. Asserting only the
    /// ores that *do* pay would leave six
    /// blocks silently rewarding XP forever, and nothing about a coal-ore test would
    /// notice.
    #[test]
    fn the_raw_ores_pop_no_experience_despite_being_experience_blocks() {
        let mut wrong: Vec<&str> = Vec::new();
        for ore in [
            "minecraft:iron_ore",
            "minecraft:deepslate_iron_ore",
            "minecraft:gold_ore",
            "minecraft:deepslate_gold_ore",
            "minecraft:copper_ore",
            "minecraft:deepslate_copper_ore",
            // Not an ore at all, and the commonest block in the game: a fallback that
            // returned a range for an unknown name would show up here first.
            "minecraft:stone",
            "minecraft:dirt",
        ] {
            if block_break_experience_range(ore).is_some() {
                wrong.push(ore);
            }
        }
        assert!(
            wrong.is_empty(),
            "these blocks must pop no experience: {wrong:?}"
        );
    }

    /// Every paying ore's exact `[min, max]`, and the deepslate variant matching its
    /// stone twin.
    ///
    /// Stated as a table so a failure names the block, and asserted as an exact pair
    /// rather than "is Some" — the plausible wrong answers are all *other ranges*
    /// (redstone as `1..=4` from a missing inclusive bound, lapis as `2..=4`), not an
    /// absent one.
    #[test]
    fn each_paying_ore_declares_the_range_its_registration_does() {
        const EXPECTED: &[(&str, (i32, i32))] = &[
            ("minecraft:coal_ore", (0, 2)),
            ("minecraft:deepslate_coal_ore", (0, 2)),
            ("minecraft:nether_gold_ore", (0, 1)),
            ("minecraft:redstone_ore", (1, 5)),
            ("minecraft:deepslate_redstone_ore", (1, 5)),
            ("minecraft:lapis_ore", (2, 5)),
            ("minecraft:deepslate_lapis_ore", (2, 5)),
            ("minecraft:nether_quartz_ore", (2, 5)),
            ("minecraft:diamond_ore", (3, 7)),
            ("minecraft:deepslate_diamond_ore", (3, 7)),
            ("minecraft:emerald_ore", (3, 7)),
            ("minecraft:deepslate_emerald_ore", (3, 7)),
            ("minecraft:spawner", (15, 43)),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for &(block, expected) in EXPECTED {
            let actual = block_break_experience_range(block);
            if actual != Some(expected) {
                mismatches.push(format!("{block}: expected {expected:?}, got {actual:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

        // A state string with properties resolves the same as a bare name — `lit=true`
        // is what a redstone ore looks like the instant a player breaks it.
        assert_eq!(
            block_break_experience_range("minecraft:redstone_ore[lit=true]"),
            Some((1, 5))
        );
    }

    /// **The inclusive upper bound of a uniform-int range, which is the off-by-one no
    /// "mining gives XP" assertion can see.**
    ///
    /// The real uniform-sample rule is `min + next_int(max - min + 1)`. Dropping the
    /// `+ 1` makes the top of every range unreachable — diamond ore would pay 3..=6
    /// instead of 3..=7, a difference nobody notices by eye. Both hypotheses are
    /// driven here from outside constants: a stub `next_int` returning `bound - 1`
    /// (its largest legal value) must produce **max**, and one returning `0` must
    /// produce **min**.
    ///
    /// The spawner is the same assertion against a **two-draw** roll:
    /// `15 + next_int(15) + next_int(15)`, so the extremes are 15 and 43. A single
    /// `15 + next_int(29)` gives those same two bounds with a uniform distribution
    /// instead of a triangular one, which is why the draw *count* is asserted rather
    /// than just the range.
    #[test]
    fn a_uniform_roll_can_reach_both_ends_of_its_range() {
        let mut mismatches: Vec<String> = Vec::new();
        for (block, (min, max)) in [
            ("minecraft:coal_ore", (0, 2)),
            ("minecraft:redstone_ore", (1, 5)),
            ("minecraft:diamond_ore", (3, 7)),
            ("minecraft:nether_gold_ore", (0, 1)),
        ] {
            let lowest = block_break_points(block, |_| 0);
            let highest = block_break_points(block, |bound| bound - 1);
            if lowest != min {
                mismatches.push(format!("{block}: a zero draw gave {lowest}, not {min}"));
            }
            if highest != max {
                mismatches.push(format!(
                    "{block}: the largest draw gave {highest}, not {max} — a missing \
                     inclusive `+ 1` looks exactly like this"
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

        // The spawner: two draws, so the roll asks for `nextInt(15)` twice.
        let mut draws = 0;
        let lowest = block_break_points("minecraft:spawner", |bound| {
            draws += 1;
            assert_eq!(bound, 15, "each spawner draw is nextInt(15)");
            0
        });
        assert_eq!(lowest, 15);
        assert_eq!(draws, 2, "the spawner rolls two draws, not one");
        let highest = block_break_points("minecraft:spawner", |bound| bound - 1);
        assert_eq!(highest, 43, "15 + 14 + 14");

        // And a block with no range consumes no draws at all, so breaking stone cannot
        // shift the next ore's roll.
        let mut stone_draws = 0;
        assert_eq!(
            block_break_points("minecraft:stone", |_| {
                stone_draws += 1;
                0
            }),
            0
        );
        assert_eq!(stone_draws, 0, "a block with no experience must not draw");
    }

    /// **The outside oracle: real player files written by a real 26.2 server.**
    ///
    /// Every other test in this module compares this transcription against the
    /// decompiled record definition — two readings of the same source. This one
    /// compares it against *output*: the `XpLevel`/`XpP`/`XpTotal` triples a vanilla
    /// server actually wrote for players who earned XP by playing, read out of
    /// `.cache/mc/survival/world/players/data` with a foreign parser (Python `gzip` +
    /// `struct.unpack`, sharing no code with this repo) exactly as
    /// `crate::player_data`'s own field names were.
    ///
    /// 247 files, 12 with non-zero XP; the six below are the ones with distinct
    /// totals. Committed as a table rather than read at test time so the gate does not
    /// depend on a `.cache` directory that is not repo state.
    ///
    /// | file | `XpTotal` | `XpLevel` | `XpP` |
    /// |---|---|---|---|
    /// | `206dff15…` | 3 | 0 | 0.4285714626312256 |
    /// | `1e071071…` | 5 | 0 | 0.7142857313156128 |
    /// | `42b0b1f9…` | 7 | 1 | 0.0 |
    /// | `48cc6aa7…` | 9 | 1 | 0.2222222089767456 |
    /// | `0962d3e1…` | 15 | 1 | 0.888888955116272 |
    /// | `142b2dd8…` | 24 | 2 | 0.7272728681564331 |
    ///
    /// **What these discriminate**, since they are all in the low regime and so say
    /// nothing about the 15/30 seams: they pin the **carry re-expression**. A total of
    /// 15 awarded in one call is `15/7 = 2.142…` progress; leaving the overflow as a
    /// bare `progress - 1.0` gives `1.142…`, another carry, and level **2** with the
    /// bar at 0.142 — vanilla says level 1 with the bar at 8/9. Every row past 7
    /// separates those two hypotheses, and the seams are pinned by
    /// [`the_level_curve_switches_regime_at_fifteen_and_thirty_inclusive`] instead.
    ///
    /// Mismatches are collected rather than asserted inside the loop: an `assert!` in
    /// a `for` proves one row and leaves the rest as an argument.
    #[test]
    fn every_vanilla_written_xp_triple_replays_through_the_curve() {
        /// `(XpTotal, XpLevel, XpP)`, straight off the files above.
        const VANILLA: [(i32, i32, f32); 6] = [
            (3, 0, 0.428_571_46),
            (5, 0, 0.714_285_73),
            (7, 1, 0.0),
            (9, 1, 0.222_222_21),
            (15, 1, 0.888_888_96),
            (24, 2, 0.727_272_87),
        ];

        let mut mismatches: Vec<String> = Vec::new();
        for (total, level, progress) in VANILLA {
            let mut xp = PlayerExperience::default();
            xp.give_points(total);
            if xp.level() != level || (xp.progress() - progress).abs() > 1e-6 {
                mismatches.push(format!(
                    "total {total}: vanilla wrote (level {level}, bar {progress}), we \
                     produced (level {}, bar {})",
                    xp.level(),
                    xp.progress()
                ));
            }
            if xp.total() != total {
                mismatches.push(format!(
                    "total {total}: the lifetime total came back as {}",
                    xp.total()
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of {} vanilla-written triples disagree:\n{}",
            mismatches.len(),
            VANILLA.len(),
            mismatches.join("\n")
        );
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

    /// `query_points` reads the *bar*, not [`PlayerExperience::total`] — this
    /// module's own doc names enchanting as the case where the two diverge
    /// (spending XP lowers `total` relative to what the level implies, but
    /// `progress`/`level` are what the bar and `/xp query … points` actually
    /// read). `restored` sets `progress`/`level` directly so this is an
    /// arithmetic check of the formula, independent of `give_points`'s carry
    /// loop.
    #[test]
    fn query_points_reads_the_bar_not_the_lifetime_total() {
        // level 5's cost is `level_up_cost(5) = 7 + 5*2 = 17`.
        let xp = PlayerExperience::restored(5, 0.5, 999);
        assert_eq!(xp.next_level_cost(), 17);
        assert_eq!(xp.query_points(), 8, "floor(0.5 * 17) = 8, not the 999 total");
        assert_ne!(xp.query_points(), xp.total(), "the two readings must diverge on this input");

        // level 0 fresh has no bar filled at all.
        assert_eq!(PlayerExperience::default().query_points(), 0);
    }
}
