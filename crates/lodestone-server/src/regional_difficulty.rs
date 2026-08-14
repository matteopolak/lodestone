//! Regional difficulty — vanilla's `DifficultyInstance`: a scalar grown from
//! world difficulty, elapsed world time (moon phase included) and how long a
//! chunk has been loaded, clamped into a small range.
//!
//! # What it is
//!
//! `getCurrentDifficultyAt` (`ServerLevel.java`) builds one of these per
//! query from four inputs — the world's [`Difficulty`], the world's total game
//! time, one chunk's "local" (inhabited) time, and the moon's brightness at
//! that position — and reduces them to a single `f32`,
//! [`DifficultyInstance::effective_difficulty`]. Nothing in this crate read or
//! computed this before: `grep -rn 'RegionalDifficulty\|DifficultyInstance'`
//! across the tree returned nothing outside this module.
//!
//! # How it works
//!
//! [`calculate_difficulty`] is `DifficultyInstance.calculateDifficulty`
//! (`DifficultyInstance.java`), transcribed clause by clause:
//!
//! ```java
//! private float calculateDifficulty(Difficulty base, long totalGameTime, long localGameTime, float moonBrightness) {
//!    if (base == Difficulty.PEACEFUL) {
//!       return 0.0F;
//!    }
//!    boolean isHard = base == Difficulty.HARD;
//!    float scale = 0.75F;
//!    float globalScale = Mth.clamp(((float)totalGameTime + -72000.0F) / 1440000.0F, 0.0F, 1.0F) * 0.25F;
//!    scale += globalScale;
//!    float localScale = 0.0F;
//!    localScale += Mth.clamp((float)localGameTime / 3600000.0F, 0.0F, 1.0F) * (isHard ? 1.0F : 0.75F);
//!    localScale += Mth.clamp(moonBrightness * 0.25F, 0.0F, globalScale);
//!    if (base == Difficulty.EASY) {
//!       localScale *= 0.5F;
//!    }
//!    scale += localScale;
//!    return base.getId() * scale;
//! }
//! ```
//!
//! Each clause and where it lands in [`calculate_difficulty`]:
//!
//! | clause | Java | here |
//! |---|---|---|
//! | Peaceful short-circuit | `if (base == PEACEFUL) return 0.0F` | the first `if` |
//! | base scale | `float scale = 0.75F` | `let mut scale = 0.75` |
//! | global (world-age) ramp | `globalScale = clamp((total - 72000) / 1440000, 0, 1) * 0.25` | `global_scale` |
//! | fold into scale | `scale += globalScale` | `scale += global_scale` |
//! | local (inhabited-time) ramp, hard-vs-not coefficient | `clamp(local / 3600000, 0, 1) * (isHard ? 1.0 : 0.75)` | first `local_scale +=` |
//! | moon term, clamped by `globalScale` **not** `1.0` | `clamp(moonBrightness * 0.25, 0, globalScale)` | second `local_scale +=` |
//! | Easy halves the local scale | `if (base == EASY) localScale *= 0.5` | the `if base == Difficulty::Easy` |
//! | fold and scale by difficulty id | `return base.getId() * scale` | final line |
//!
//! The moon clause is the one easy to get wrong: it is clamped against
//! **`globalScale`**, a value that only reaches `0.25` once `totalGameTime`
//! has climbed past the `72_000`-tick offset, **not** against `1.0`. Early in
//! a fresh world `globalScale` is `0.0`, so a full moon (`moonBrightness ==
//! 1.0`) contributes **nothing** — [`regional_difficulty_moon_term_is_capped_by_global_scale_not_by_one`]
//! is the test that would fail if that upper bound were written as `1.0`
//! instead.
//!
//! [`moon_brightness_for_day_time`] is `DimensionType.MOON_BRIGHTNESS_PER_PHASE`
//! (`DimensionType.java`) indexed by `ServerLevel.getMoonBrightness`'s moon
//! phase, `(dayTime / 24000) % 8` (`MoonPhase.java`'s `startTick`/`index`
//! relationship, read backwards): day 0 is a full moon (brightness `1.0`), and
//! the cycle is 8 phases of 24,000 ticks each. This duplicates the array
//! [`crate::natural_spawn`] already carries for the surface-slime spawn
//! chance (that one is scaled `* 0.5` for a different vanilla formula; this
//! one is the raw `DimensionType` table `DifficultyInstance` itself reads) —
//! two call sites, not two facts, and if they ever drift the jar is the
//! tiebreaker.
//!
//! `total_game_time` is `ServerLevel.getOverworldClockTime()`, which this
//! crate does not model as a distinct world-clock (26.2 splits `game_time`
//! from a per-dimension clock; see `crate::world_state`'s module doc). Every
//! call site here passes [`crate::world_state::WorldStateHandle`]'s
//! `game_time` instead, which is the same quantity for the overworld (the
//! only dimension this crate hosts) before any per-dimension clock offset
//! exists to diverge them.
//!
//! # What is out of reach from here
//!
//! **`local_game_time` (a chunk's `InhabitedTime`) is not tracked anywhere in
//! this crate.** `crate::chunk_nbt`'s `InhabitedTime` field is a hardcoded
//! `Nbt::Long(0)` — vanilla increments it once per natural-spawning cycle by
//! `ServerChunkCache.tickSpawningChunk`'s `chunk.incrementInhabitedTime(timeDiff)`,
//! and no per-chunk counter exists here to increment. [`DifficultyInstance::new`]
//! therefore takes `local_game_time` as a plain parameter rather than deriving
//! it, so every call site passes `0` until chunk-inhabited-time tracking
//! lands — which understates `effective_difficulty` by up to `0.75`/`1.0` (the
//! local scale's own ceiling) for an old chunk, but never overstates it, and
//! never wrongly disagrees about Peaceful (still forced to `0.0`) or about
//! which side of a threshold a *fresh* chunk sits on.
//!
//! **The consumers the issue names — zombie/skeleton spawned-with-enchanted-gear
//! chance (`VanillaEnchantmentProviders.MOB_SPAWN_EQUIPMENT`, read against a
//! `DifficultyInstance` in `SkeletonTrapGoal.enchant` and the natural-spawn
//! equipment path), zombie reinforcement calling for backup, and spawn-cap
//! composition — do not exist anywhere in this tree yet.** `grep`ing
//! `crate::mob_spawn`, `crate::natural_spawn` and `crate::mobs` for
//! "reinforce", "enchant" or "gear" turns up nothing: there is no zombie AI,
//! no spawn-equipment path and no reinforcement mechanic to wire this scalar
//! into. Building those is a separate, unimplemented feature, not a
//! reachable consumer of this module. The one genuine, tested consumer in
//! this tree is [`crate::lightning`]'s skeleton-horse-trap roll
//! (`ServerLevel.tickThunder`'s `random.nextDouble() < difficulty.getEffectiveDifficulty() * 0.01`),
//! which reads [`DifficultyInstance::effective_difficulty`] directly — see
//! that module's doc for why the *spawn* half of that roll (an actual
//! `SkeletonHorse` entity) is out of reach the same way.
//!
//! # Dependencies
//!
//! [`lodestone_model::Difficulty`] only. No world access, no protocol — the
//! caller resolves `total_game_time`/`local_game_time`/`moon_brightness` and
//! passes them in, the same shape [`crate::food`]'s module doc describes for
//! its own difficulty gate.

use lodestone_model::Difficulty;

/// `DifficultyInstance.DIFFICULTY_TIME_GLOBAL_OFFSET`.
const DIFFICULTY_TIME_GLOBAL_OFFSET: f32 = -72_000.0;
/// `DifficultyInstance.MAX_DIFFICULTY_TIME_GLOBAL`.
const MAX_DIFFICULTY_TIME_GLOBAL: f32 = 1_440_000.0;
/// `DifficultyInstance.MAX_DIFFICULTY_TIME_LOCAL`.
const MAX_DIFFICULTY_TIME_LOCAL: f32 = 3_600_000.0;

/// `DimensionType.MOON_BRIGHTNESS_PER_PHASE` — indexed by moon phase, `0` a
/// full moon. See the module doc for why this duplicates (rather than
/// reuses) `crate::natural_spawn`'s copy of the same array.
pub const MOON_BRIGHTNESS_PER_PHASE: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];

/// `ServerLevel.getMoonBrightness`: `MOON_BRIGHTNESS_PER_PHASE[(dayTime / 24000) % 8]`,
/// with a `div_euclid`/`rem_euclid` pair so a negative `day_time` (`/time set`
/// can produce one) still indexes a real phase rather than panicking.
#[must_use]
pub fn moon_brightness_for_day_time(day_time: i64) -> f32 {
    let phase = day_time.div_euclid(24_000).rem_euclid(8) as usize;
    MOON_BRIGHTNESS_PER_PHASE[phase]
}

/// `Difficulty.getId` — Peaceful 0, Easy 1, Normal 2, Hard 3, exactly the
/// enum's declaration order, so a plain cast suffices. [`crate::fire`] already
/// establishes this mapping under the name `difficulty_id`; this is the same
/// fact, kept local so this module has no dependency on `crate::fire`.
#[must_use]
fn difficulty_id(base: Difficulty) -> i32 {
    match base {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 1,
        Difficulty::Normal => 2,
        Difficulty::Hard => 3,
    }
}

/// `DifficultyInstance` — an immutable snapshot of the world's [`Difficulty`]
/// plus the derived [`effective_difficulty`](Self::effective_difficulty)
/// scalar, computed once at construction exactly as the `@Immutable` Java
/// type does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifficultyInstance {
    base: Difficulty,
    effective_difficulty: f32,
}

impl DifficultyInstance {
    /// `new DifficultyInstance(base, totalGameTime, localGameTime, moonBrightness)`.
    ///
    /// See the module doc for what each argument means and where it comes
    /// from in this crate; `local_game_time` is `0` at every call site today
    /// (chunk-inhabited-time tracking does not exist here yet).
    #[must_use]
    pub fn new(base: Difficulty, total_game_time: i64, local_game_time: i64, moon_brightness: f32) -> Self {
        Self {
            base,
            effective_difficulty: calculate_difficulty(base, total_game_time, local_game_time, moon_brightness),
        }
    }

    /// `DifficultyInstance.getDifficulty`.
    #[must_use]
    pub fn difficulty(&self) -> Difficulty {
        self.base
    }

    /// `DifficultyInstance.getEffectiveDifficulty`.
    #[must_use]
    pub fn effective_difficulty(&self) -> f32 {
        self.effective_difficulty
    }

    /// `DifficultyInstance.isHard` — `effectiveDifficulty >= Difficulty.HARD.ordinal()`.
    #[must_use]
    pub fn is_hard(&self) -> bool {
        self.effective_difficulty >= difficulty_id(Difficulty::Hard) as f32
    }

    /// `DifficultyInstance.isHarderThan` — a **strict** `>`, unlike
    /// [`is_hard`](Self::is_hard)'s `>=`.
    #[must_use]
    pub fn is_harder_than(&self, required_difficulty: f32) -> bool {
        self.effective_difficulty > required_difficulty
    }

    /// `DifficultyInstance.getSpecialMultiplier` — `0` below `2.0`, `1` above
    /// `4.0`, and a linear ramp between.
    #[must_use]
    pub fn special_multiplier(&self) -> f32 {
        if self.effective_difficulty < 2.0 {
            0.0
        } else if self.effective_difficulty > 4.0 {
            1.0
        } else {
            (self.effective_difficulty - 2.0) / 2.0
        }
    }
}

/// `Mth.clamp(value, min, max)` for `f32`: unlike [`f32::clamp`] this never
/// panics when `min > max` (dead here since every call site's bounds are
/// non-negative, but matching the Java semantics exactly rather than relying
/// on that).
fn mth_clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else {
        value.min(max)
    }
}

/// `DifficultyInstance.calculateDifficulty` — see the module doc's clause
/// table for the line-by-line correspondence.
#[must_use]
fn calculate_difficulty(base: Difficulty, total_game_time: i64, local_game_time: i64, moon_brightness: f32) -> f32 {
    if base == Difficulty::Peaceful {
        return 0.0;
    }
    let is_hard = base == Difficulty::Hard;
    let mut scale = 0.75_f32;
    let global_scale = mth_clamp(
        (total_game_time as f32 + DIFFICULTY_TIME_GLOBAL_OFFSET) / MAX_DIFFICULTY_TIME_GLOBAL,
        0.0,
        1.0,
    ) * 0.25;
    scale += global_scale;
    let mut local_scale = 0.0_f32;
    local_scale += mth_clamp(local_game_time as f32 / MAX_DIFFICULTY_TIME_LOCAL, 0.0, 1.0)
        * if is_hard { 1.0 } else { 0.75 };
    // Clamped by `global_scale`, not by `1.0` — see the module doc.
    local_scale += mth_clamp(moon_brightness * 0.25, 0.0, global_scale);
    if base == Difficulty::Easy {
        local_scale *= 0.5;
    }
    scale += local_scale;
    difficulty_id(base) as f32 * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-derived from an independent `numpy.float32` transcription of
    /// `calculateDifficulty` (not from this module), reproducing Java's `f32`
    /// arithmetic step for step. Each case is picked to be non-round and,
    /// where a clause could plausibly be implemented two ways, to land on a
    /// value only the correct clause produces.
    #[test]
    fn effective_difficulty_matches_hand_computed_f32_values() {
        // Normal, mid global ramp, mid local ramp, full moon: base id 2,
        // global_scale = clamp((500000-72000)/1440000,0,1)*0.25 = 0.297222*0.25 = 0.0742...,
        // local_scale = clamp(1800000/3600000,0,1)*0.75 + clamp(1.0*0.25,0,0.0742...) = 0.375+0.0742...
        let d = DifficultyInstance::new(Difficulty::Normal, 500_000, 1_800_000, 1.0);
        assert!(
            (d.effective_difficulty() - 2.547_222).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );

        // Hard, same times, new moon: the isHard local coefficient is 1.0
        // instead of 0.75, and the moon term is zero.
        let d = DifficultyInstance::new(Difficulty::Hard, 500_000, 1_800_000, 0.0);
        assert!(
            (d.effective_difficulty() - 3.972_917).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );

        // Easy, same times, full moon: the Easy branch halves local_scale.
        let d = DifficultyInstance::new(Difficulty::Easy, 500_000, 1_800_000, 1.0);
        assert!(
            (d.effective_difficulty() - 1.048_958).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );

        // Normal, zero total/local time, full moon: global_scale is 0 this
        // early (the -72000 offset has not been crossed), so the moon term
        // must clamp to 0 too — see the dedicated test below for the
        // discriminating case this only hints at.
        let d = DifficultyInstance::new(Difficulty::Normal, 0, 0, 1.0);
        assert!(
            (d.effective_difficulty() - 1.5).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );

        // Normal, both times saturated far past their maxima, full moon:
        // every clamp pins at its upper bound.
        let d = DifficultyInstance::new(Difficulty::Normal, 5_000_000, 5_000_000, 1.0);
        assert!(
            (d.effective_difficulty() - 4.0).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );

        // Negative game time (a `/time set` underflow, or a fresh world's
        // `-72000` offset window): must clamp to the same floor as zero, not
        // panic and not go negative.
        let d = DifficultyInstance::new(Difficulty::Normal, -100_000, -100_000, 0.5);
        assert!(
            (d.effective_difficulty() - 1.5).abs() < 1e-4,
            "got {}",
            d.effective_difficulty()
        );
    }

    /// Peaceful returns exactly `0.0` regardless of how saturated every other
    /// input is — the short-circuit branch, isolated from the arithmetic.
    #[test]
    fn peaceful_is_always_zero() {
        let d = DifficultyInstance::new(Difficulty::Peaceful, 999_999_999, 999_999_999, 1.0);
        assert_eq!(d.effective_difficulty(), 0.0);
        assert!(!d.is_hard());
        assert_eq!(d.special_multiplier(), 0.0);
    }

    /// The discriminating case for the moon clause: at `total_game_time == 0`
    /// (so `global_scale == 0.0`) a full moon must contribute **nothing**,
    /// because the moon term is clamped by `global_scale`, not by `1.0`. A
    /// port that clamped it against `1.0` instead would add `0.25` here and
    /// land on `2.0`, not `1.5` — the two hypotheses differ, so this is a
    /// real discriminator rather than an input where they coincide.
    #[test]
    fn regional_difficulty_moon_term_is_capped_by_global_scale_not_by_one() {
        let correct = DifficultyInstance::new(Difficulty::Normal, 0, 0, 1.0);
        assert!(
            (correct.effective_difficulty() - 1.5).abs() < 1e-4,
            "correct hypothesis: got {}",
            correct.effective_difficulty()
        );

        // The wrong hypothesis, computed independently (not by calling this
        // module): scale = 0.75 (base) + 0.0 (global_scale) + local_scale,
        // where local_scale = 0.0 (local ramp, local_game_time == 0) +
        // clamp(1.0 * 0.25, 0.0, 1.0) = 0.25. scale = 1.0, times base id 2 = 2.0.
        let wrong_hypothesis_moon_clamped_to_one = 2.0_f32;
        assert!(
            (correct.effective_difficulty() - wrong_hypothesis_moon_clamped_to_one).abs() > 0.1,
            "the two hypotheses must differ for this input to discriminate them"
        );
        assert_ne!(correct.effective_difficulty(), wrong_hypothesis_moon_clamped_to_one);
    }

    /// The discriminating case for the hard-vs-not local coefficient: Normal
    /// and Hard at identical, non-saturating local time must differ by
    /// exactly the `0.75` vs `1.0` coefficient (scaled by each difficulty's
    /// own `getId`), not merely "Hard is bigger" (which the `* getId()` term
    /// alone would already guarantee and so would not isolate the clause).
    #[test]
    fn hard_uses_the_full_local_coefficient_normal_does_not() {
        // Half-saturated local ramp, no global/moon contribution (total time
        // 0 keeps global_scale, and therefore the moon clamp, at 0).
        let normal = DifficultyInstance::new(Difficulty::Normal, 0, 1_800_000, 0.0);
        let hard = DifficultyInstance::new(Difficulty::Hard, 0, 1_800_000, 0.0);
        // normal: (0.75 + 0.5*0.75) * 2 = 1.125 * 2 = 2.25
        // hard:   (0.75 + 0.5*1.0)  * 3 = 1.25  * 3 = 3.75
        assert!((normal.effective_difficulty() - 2.25).abs() < 1e-4, "got {}", normal.effective_difficulty());
        assert!((hard.effective_difficulty() - 3.75).abs() < 1e-4, "got {}", hard.effective_difficulty());
    }

    /// `getSpecialMultiplier`'s three arms, at values that land inside each
    /// one rather than exactly on a boundary (so the test is not itself an
    /// instance of the coincident-threshold trap).
    #[test]
    fn special_multiplier_matches_the_three_arms() {
        assert_eq!(DifficultyInstance::new(Difficulty::Easy, 0, 0, 0.0).special_multiplier(), 0.0);
        // effective_difficulty 2.547222 (from the first test's Normal case).
        let mid = DifficultyInstance::new(Difficulty::Normal, 500_000, 1_800_000, 1.0);
        assert!(
            (mid.special_multiplier() - 0.273_611).abs() < 1e-4,
            "got {}",
            mid.special_multiplier()
        );
        // Saturated Hard sits above 4.0: (0.75+0.25+1.0+0.25)*3 = 6.75.
        let saturated_hard = DifficultyInstance::new(Difficulty::Hard, 5_000_000, 5_000_000, 1.0);
        assert_eq!(saturated_hard.special_multiplier(), 1.0);
    }

    /// `isHard`'s `>=` against `isHarderThan`'s strict `>`, at a value picked
    /// to be near but not on the Hard boundary (`3.0`) so the two predicates'
    /// difference is visible: `is_harder_than(3.0)` must be false at exactly
    /// the boundary the way `>` always is, while a Hard-difficulty instance
    /// clears `is_hard`'s `>=` easily.
    #[test]
    fn is_hard_is_inclusive_is_harder_than_is_strict() {
        let hard = DifficultyInstance::new(Difficulty::Hard, 0, 0, 0.0);
        // scale = 0.75, effective = 3 * 0.75 = 2.25 — below the Hard ordinal (3).
        assert!((hard.effective_difficulty() - 2.25).abs() < 1e-4);
        assert!(!hard.is_hard(), "2.25 must not clear the >= 3.0 threshold");
        assert!(hard.is_harder_than(2.0));
        assert!(!hard.is_harder_than(2.5));

        let saturated_hard = DifficultyInstance::new(Difficulty::Hard, 5_000_000, 5_000_000, 1.0);
        // effective 6.75, well past 3.0.
        assert!(saturated_hard.is_hard());
        assert!(saturated_hard.is_harder_than(6.75 - 0.01));
        assert!(!saturated_hard.is_harder_than(6.75));
        assert!(!saturated_hard.is_harder_than(6.75 + 0.01));
    }

    /// The moon-phase table, indexed the way `ServerLevel.getMoonBrightness`
    /// does: day 0 is a full moon, and the cycle wraps every 8 * 24000 ticks.
    /// Also exercises a negative `day_time` (`/time set` can produce one),
    /// which must index a real phase rather than panic.
    #[test]
    fn moon_brightness_follows_the_eight_phase_cycle() {
        assert_eq!(moon_brightness_for_day_time(0), 1.0, "day 0 is a full moon");
        assert_eq!(moon_brightness_for_day_time(12_000), 1.0, "still phase 0 at midday");
        assert_eq!(moon_brightness_for_day_time(24_000), 0.75, "phase 1");
        assert_eq!(moon_brightness_for_day_time(4 * 24_000), 0.0, "phase 4 is a new moon");
        assert_eq!(moon_brightness_for_day_time(8 * 24_000), 1.0, "the cycle wraps back to full");
        assert_eq!(
            moon_brightness_for_day_time(-24_000),
            0.75,
            "a negative day_time must still index a real phase (phase 7, by rem_euclid)"
        );
    }

    /// `Difficulty::getId` — pinned because [`calculate_difficulty`]'s final
    /// multiply and [`DifficultyInstance::is_hard`]'s threshold both depend
    /// on this exact ordinal mapping.
    #[test]
    fn difficulty_ids_match_vanilla_ordinals() {
        assert_eq!(difficulty_id(Difficulty::Peaceful), 0);
        assert_eq!(difficulty_id(Difficulty::Easy), 1);
        assert_eq!(difficulty_id(Difficulty::Normal), 2);
        assert_eq!(difficulty_id(Difficulty::Hard), 3);
    }
}
