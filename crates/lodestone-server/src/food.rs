//! Server-authoritative hunger: exhaustion, saturation, food level, natural
//! regeneration and starvation.
//!
//! # What it is
//!
//! [`FoodData`] is a transcription of vanilla's `net.minecraft.world.food.FoodData`
//! — a plain value type with a pure [`tick`](FoodData::tick) step. It owns the four
//! numbers vanilla's does (`foodLevel`, `saturationLevel`, `exhaustionLevel`,
//! `tickTimer`) and reports what a tick decided as a [`FoodTick`] rather than
//! reaching into health itself, so [`crate::vitals::PlayerVitals`] stays the single
//! owner of health and this module stays unit-testable in isolation. That is the
//! same split `PlayerVitals::tick` already draws for drowning.
//!
//! Before this existed there was no hunger simulation at all: `lodestone-server`
//! sent a hardcoded `food: 20, saturation: 5.0` on every `SetHealth`, so the HUD's
//! haunches never moved and food was decoration.
//!
//! # How it works
//!
//! Vanilla's model is a **three-layer buffer**, and the middle layer is the one
//! that gets skipped in reimplementations:
//!
//! 1. **Exhaustion** accumulates from actions, capped at `40.0`.
//! 2. Every tick, exhaustion above [`EXHAUSTION_DROP`] (`4.0`) is spent: `4.0` is
//!    subtracted, and **one point of saturation** goes with it.
//! 3. Only when saturation has reached `0.0` does the **food level** drop — and
//!    even then, not on Peaceful.
//!
//! So the visible haunches do not move until the invisible saturation bar is empty,
//! which is why a well-fed player can sprint a long way before the HUD reacts at
//! all. A model that decremented food directly from exhaustion would deplete
//! **five times faster** at a fresh spawn (`START_SATURATION` is `5.0`).
//!
//! ## The exhaustion costs, and the one that is zero
//!
//! Read off `FoodConstants` and `ServerPlayer.checkMovementStatistics`, which is
//! where the per-distance ones live. Vanilla's distance term is
//! `round(sqrt(dx² + dz²) * 100)` centimetres, multiplied by `0.01` again — so the
//! per-**block** cost is just the leading constant:
//!
//! | action | vanilla expression | per block / per event |
//! |---|---|---|
//! | sprint | `0.1F * cm * 0.01F` | **0.1** per block |
//! | walk | `0.0F * cm * 0.01F` | **zero** |
//! | crouch | `0.0F * cm * 0.01F` | **zero** |
//! | swim / underwater | `0.01F * cm * 0.01F` | 0.01 per block |
//! | jump | `EXHAUSTION_JUMP` | 0.05 |
//! | sprint-jump | `EXHAUSTION_SPRINT_JUMP` | 0.2 |
//! | break a block | `EXHAUSTION_MINE` | 0.005 |
//! | attack | `EXHAUSTION_ATTACK` | 0.1 |
//! | natural regen | `EXHAUSTION_HEAL` | 6.0 |
//!
//! **Walking costs nothing.** This is the single most commonly-wrong fact about
//! hunger, and vanilla writes it as a literal `0.0F` multiply rather than by
//! omitting the branch — the statistic beside it is still awarded. Only sprinting,
//! swimming, jumping, mining, attacking, taking damage and *healing* cost anything.
//! A model that charged walking would deplete a stationary-ish player who never
//! sprints, which vanilla never does.
//!
//! The consequence worth remembering, because it is the number to predict in a
//! gate: at `0.1` per block, **40 blocks of sprinting** spend one point of
//! saturation. The exact cadence has an off-by-one worth knowing — the threshold
//! test is `exhaustion > 4.0`, **strictly** greater, so exhaustion has to reach
//! `4.1` and the *k*-th drop lands on block `40k + 1`:
//!
//! | block | saturation | food |
//! |---|---|---|
//! | 41 | 4.0 | 20 |
//! | 161 | 1.0 | 20 |
//! | **201** | 0.0 | 20 |
//! | **241** | 0.0 | **19** |
//!
//! So a fresh spawn sprints **241** blocks before the visible bar moves, not 200,
//! and "200 blocks" is the round number that is wrong by one drop. Every food point
//! after that is another 40.
//!
//! ## Regeneration and starvation are one if/else chain
//!
//! `FoodData.tick`'s second half is a four-arm chain sharing **one** `tickTimer`,
//! and the exclusivity is load-bearing — a player cannot be regenerating and
//! starving in the same tick, and any arm not taken resets the timer to zero:
//!
//! | arm | condition | period | effect |
//! |---|---|---|---|
//! | saturated regen | `natural_regen && saturation > 0 && hurt && food >= 20` | 10 ticks | heal `min(sat, 6)/6`, exhaust by the amount spent |
//! | slow regen | `natural_regen && food >= 18 && hurt` | 80 ticks | heal `1.0`, exhaust `6.0` |
//! | starvation | `food <= 0` | 80 ticks | `1.0` starve damage, gated by difficulty |
//! | idle | otherwise | — | reset the timer |
//!
//! The saturated arm is a **partial** heal: spending `3.0` saturation heals `0.5`,
//! not a whole heart. Rounding it up makes regeneration roughly twice as effective
//! as vanilla's.
//!
//! ## The difficulty gate on starvation is not "peaceful is safe"
//!
//! `player.getHealth() > 10.0F || difficulty == HARD || (health > 1.0F && difficulty == NORMAL)`.
//! So on **Easy and Peaceful a starving player is still hurt down to 10 health**,
//! on Normal down to 1, and on Hard all the way to death. Peaceful's protection is
//! upstream instead: the depletion branch's own `difficulty != PEACEFUL` guard means
//! the food level never *reaches* zero there in the first place. Two different
//! mechanisms, and modelling only the obvious one gets Peaceful wrong in both
//! directions.
//!
//! # How to change it
//!
//! * **A new exhaustion producer**: call [`crate::vitals::PlayerVitals::add_exhaustion`]
//!   from the site that knows the action happened. The constants live here; the
//!   producer sites are in `crate::server`.
//! * **Eating**: [`FoodData::eat`] takes nutrition and a saturation *modifier*, and
//!   `saturationByModifier` is `nutrition * modifier * 2.0` — not `nutrition *
//!   modifier`. The per-item table is data, not formula, and lives in the item's
//!   food component; nothing supplies it yet, so `eat` has no production caller.
//! * **Persistence**: the four fields map to `level.dat`-style player NBT under
//!   `foodLevel` / `foodTickTimer` / `foodSaturationLevel` / `foodExhaustionLevel`,
//!   vanilla's own names ([`FoodData::restored`]).
//!
//! # Dependencies
//!
//! `lodestone-model` for [`Difficulty`]. No protocol, no world access, no clock —
//! the caller supplies difficulty, the natural-regeneration rule and whether the
//! player is hurt.

use lodestone_model::Difficulty;

/// `FoodConstants.MAX_FOOD` — the food level a fresh player has and the cap
/// [`FoodData::eat`] clamps to.
pub const MAX_FOOD: i32 = 20;

/// `FoodConstants.START_SATURATION` — a fresh player's hidden saturation buffer.
///
/// The reason a new spawn can sprint 200 blocks before the haunches move: at `0.1`
/// exhaustion per block and `4.0` exhaustion per saturation point, `5.0` saturation
/// is `5 * 40` blocks.
pub const START_SATURATION: f32 = 5.0;

/// `FoodConstants.EXHAUSTION_DROP` — how much exhaustion one point of saturation
/// (or, once saturation is gone, one point of food) costs.
pub const EXHAUSTION_DROP: f32 = 4.0;

/// The cap `addExhaustion` clamps to (`Math.min(this.exhaustionLevel + amount, 40.0F)`).
///
/// Not in `FoodConstants` — it is a literal inside `FoodData.addExhaustion`, which
/// is why it is easy to leave out. Without it a single huge exhaustion event (a
/// long fall's worth of damage, a scripted teleport) would drain the whole food bar
/// over the following ticks instead of costing at most ten saturation points.
pub const MAX_EXHAUSTION: f32 = 40.0;

/// `FoodConstants.HEAL_LEVEL` — the food level at or above which the slow
/// regeneration arm runs.
pub const HEAL_LEVEL: i32 = 18;

/// `FoodConstants.HEALTH_TICK_COUNT` — the slow regeneration and starvation period.
pub const HEALTH_TICK_COUNT: i32 = 80;

/// `FoodConstants.HEALTH_TICK_COUNT_SATURATED` — the fast regeneration period,
/// available only at a full food bar with saturation left.
pub const HEALTH_TICK_COUNT_SATURATED: i32 = 10;

/// `FoodConstants.EXHAUSTION_HEAL` — what the slow regeneration arm charges for one
/// heart. Six saturation points per half-heart-and-a-half is why regenerating from
/// low health empties a food bar.
pub const EXHAUSTION_HEAL: f32 = 6.0;

/// `FoodConstants.EXHAUSTION_JUMP`.
pub const EXHAUSTION_JUMP: f32 = 0.05;

/// `FoodConstants.EXHAUSTION_SPRINT_JUMP` — four times a walking jump.
pub const EXHAUSTION_SPRINT_JUMP: f32 = 0.2;

/// `FoodConstants.EXHAUSTION_MINE` — per block broken.
pub const EXHAUSTION_MINE: f32 = 0.005;

/// `FoodConstants.EXHAUSTION_ATTACK` — per swing that lands.
pub const EXHAUSTION_ATTACK: f32 = 0.1;

/// `FoodConstants.EXHAUSTION_SPRINT`, as the leading constant of
/// `checkMovementStatistics`' `0.1F * cm * 0.01F` — i.e. **per block**, since the
/// `cm` term is `distance * 100` and the trailing factor is `0.01`.
pub const EXHAUSTION_SPRINT_PER_BLOCK: f32 = 0.1;

/// `FoodConstants.EXHAUSTION_SWIM`, per block, derived the same way as
/// [`EXHAUSTION_SPRINT_PER_BLOCK`]. Also the cost of walking with the eye
/// underwater and of walking on water, which share the constant.
pub const EXHAUSTION_SWIM_PER_BLOCK: f32 = 0.01;

/// `FoodConstants.EXHAUSTION_WALK` — **zero**, and written as an explicit `0.0F`
/// multiply in vanilla rather than as a missing branch. See this module's doc.
pub const EXHAUSTION_WALK_PER_BLOCK: f32 = 0.0;

/// The starvation hit (`player.hurtServer(level, damageSources().starve(), 1.0F)`).
pub const STARVE_DAMAGE: f32 = 1.0;

/// What one [`FoodData::tick`] decided, so the caller can apply it to health and
/// decide whether the client needs telling.
///
/// `heal` and `starve` are mutually exclusive by construction — vanilla's four arms
/// are an if/else chain sharing one timer — but both are `Option`s rather than one
/// signed number, because they are different damage/heal *sources* and a caller may
/// well want to route them differently.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FoodTick {
    /// `Some(amount)` when the regeneration arm fired. The saturated arm can
    /// produce a **partial** heal (`min(saturation, 6.0) / 6.0`), so this is not
    /// always `1.0`.
    pub heal: Option<f32>,
    /// `Some(1.0)` when the starvation arm fired *and* the difficulty gate allowed
    /// it. `None` on a starving tick the gate refused, which is the common case at
    /// low health on Easy.
    pub starve: Option<f32>,
    /// `true` when the food level or saturation changed, so the caller knows a
    /// `SetHealth` is worth sending. Exhaustion and the tick timer are invisible to
    /// the client and deliberately do **not** set this — otherwise every sprinting
    /// tick would send a packet.
    pub display_changed: bool,
}

impl FoodTick {
    /// Whether this tick produced anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heal.is_none() && self.starve.is_none() && !self.display_changed
    }
}

/// One player's hunger state — vanilla's `FoodData`, field for field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodData {
    food_level: i32,
    saturation: f32,
    exhaustion: f32,
    tick_timer: i32,
}

impl Default for FoodData {
    /// A fresh player: `foodLevel = 20`, `saturationLevel = 5.0F`, the other two
    /// zero — vanilla's own field initialisers.
    fn default() -> Self {
        Self {
            food_level: MAX_FOOD,
            saturation: START_SATURATION,
            exhaustion: 0.0,
            tick_timer: 0,
        }
    }
}

impl FoodData {
    /// The food level (`0..=20`), what the HUD's haunches draw.
    #[must_use]
    pub fn food_level(&self) -> i32 {
        self.food_level
    }

    /// The hidden saturation buffer (`0.0..=food_level`).
    #[must_use]
    pub fn saturation(&self) -> f32 {
        self.saturation
    }

    /// Accumulated exhaustion (`0.0..=40.0`). Invisible to the client; exposed for
    /// gates and for persistence.
    #[must_use]
    pub fn exhaustion(&self) -> f32 {
        self.exhaustion
    }

    /// The shared regeneration/starvation timer. Exposed for persistence, which
    /// vanilla also saves (`foodTickTimer`).
    #[must_use]
    pub fn tick_timer(&self) -> i32 {
        self.tick_timer
    }

    /// `vanilla FoodData.addExhaustion` — clamped at [`MAX_EXHAUSTION`].
    ///
    /// The caller is responsible for vanilla's `Player.causeFoodExhaustion` guard
    /// (`!abilities.invulnerable`), because this type does not know the game mode.
    /// Skipping that guard makes a creative player starve.
    pub fn add_exhaustion(&mut self, amount: f32) {
        self.exhaustion = (self.exhaustion + amount).min(MAX_EXHAUSTION);
    }

    /// `vanilla FoodData.eat(int, float)` — nutrition plus
    /// `FoodConstants.saturationByModifier(nutrition, modifier)`, which is
    /// `nutrition * modifier * 2.0F`.
    ///
    /// **The `* 2.0` is the part that gets dropped**, and dropping it halves every
    /// food's saturation value. Saturation is clamped to the *new* food level, not
    /// to `20.0`: eating a golden carrot at 4 food cannot bank 12 saturation.
    pub fn eat(&mut self, nutrition: i32, saturation_modifier: f32) {
        self.add(nutrition, nutrition as f32 * saturation_modifier * 2.0);
    }

    /// `vanilla FoodData.add` — the private half [`eat`](Self::eat) goes through.
    fn add(&mut self, food: i32, saturation: f32) {
        self.food_level = (self.food_level + food).clamp(0, MAX_FOOD);
        self.saturation = (self.saturation + saturation).clamp(0.0, self.food_level as f32);
    }

    /// Advances hunger by exactly one server tick — `FoodData.tick`, transcribed.
    ///
    /// `health` and `max_health` are the player's, for the `isHurt()` test
    /// (`getHealth() < getMaxHealth()`) and for the starvation difficulty gate.
    /// `natural_regen` is the `natural_health_regeneration` game rule.
    ///
    /// Returns what to apply rather than applying it — see [`FoodTick`].
    #[must_use]
    pub fn tick(
        &mut self,
        difficulty: Difficulty,
        natural_regen: bool,
        health: f32,
        max_health: f32,
    ) -> FoodTick {
        let mut out = FoodTick::default();
        let food_before = self.food_level;
        let saturation_before = self.saturation;

        // Layer 2 of the buffer: spend exhaustion, taking saturation first and only
        // then food. `>` not `>=`, so exhaustion sitting at exactly 4.0 does not
        // fire — a detail that shifts the depletion cadence by one tick.
        if self.exhaustion > EXHAUSTION_DROP {
            self.exhaustion -= EXHAUSTION_DROP;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if difficulty != Difficulty::Peaceful {
                // Peaceful's real protection: the food level never drops, so it can
                // never reach the starvation arm below. The starvation arm's own
                // difficulty gate is a *different* mechanism (see the module doc).
                self.food_level = (self.food_level - 1).max(0);
            }
        }

        let hurt = health < max_health;
        if natural_regen && self.saturation > 0.0 && hurt && self.food_level >= MAX_FOOD {
            // Saturated regeneration: every 10 ticks, spend up to 6 saturation and
            // heal a *proportional* amount. A 3.0 spend heals 0.5, not a whole
            // heart — rounding this up roughly doubles vanilla's regen rate.
            self.tick_timer += 1;
            if self.tick_timer >= HEALTH_TICK_COUNT_SATURATED {
                let spent = self.saturation.min(EXHAUSTION_HEAL);
                out.heal = Some(spent / EXHAUSTION_HEAL);
                self.add_exhaustion(spent);
                self.tick_timer = 0;
            }
        } else if natural_regen && self.food_level >= HEAL_LEVEL && hurt {
            self.tick_timer += 1;
            if self.tick_timer >= HEALTH_TICK_COUNT {
                out.heal = Some(1.0);
                self.add_exhaustion(EXHAUSTION_HEAL);
                self.tick_timer = 0;
            }
        } else if self.food_level <= 0 {
            self.tick_timer += 1;
            if self.tick_timer >= HEALTH_TICK_COUNT {
                if starvation_allowed(difficulty, health) {
                    out.starve = Some(STARVE_DAMAGE);
                }
                // Reset regardless of whether the gate allowed the hit — vanilla
                // resets outside the `if`. A version that only reset on a landed hit
                // would fire on the very next tick once health crossed the
                // threshold, instead of 80 ticks later.
                self.tick_timer = 0;
            }
        } else {
            self.tick_timer = 0;
        }

        out.display_changed =
            self.food_level != food_before || (self.saturation - saturation_before).abs() > f32::EPSILON;
        out
    }

    /// Resets to a fresh-spawn state — vanilla's respawn replaces `FoodData`
    /// wholesale, so exhaustion and the timer go too.
    pub fn respawn(&mut self) {
        *self = Self::default();
    }

    /// Rebuilds from saved player NBT, under vanilla's own field names
    /// (`readAdditionalSaveData`'s `foodLevel` / `foodTickTimer` /
    /// `foodSaturationLevel` / `foodExhaustionLevel`).
    ///
    /// Every value is **clamped to its legal range** rather than trusted, for the
    /// reason [`crate::vitals::PlayerVitals::restored`] gives: a hand-edited `.dat`
    /// must not put the tick into a state it cannot leave. Saturation above the
    /// stored food level in particular would keep the fast-regeneration arm armed
    /// forever.
    #[must_use]
    pub fn restored(food_level: i32, saturation: f32, exhaustion: f32, tick_timer: i32) -> Self {
        let food_level = food_level.clamp(0, MAX_FOOD);
        Self {
            food_level,
            saturation: saturation.clamp(0.0, food_level as f32),
            exhaustion: exhaustion.clamp(0.0, MAX_EXHAUSTION),
            tick_timer: tick_timer.clamp(0, HEALTH_TICK_COUNT),
        }
    }
}

/// Vanilla's starvation difficulty gate, verbatim:
///
/// ```java
/// player.getHealth() > 10.0F
///    || difficulty == Difficulty.HARD
///    || player.getHealth() > 1.0F && difficulty == Difficulty.NORMAL
/// ```
///
/// Read as behaviour rather than as three clauses: **Hard starves you to death,
/// Normal stops at 1 health, and Easy *and Peaceful* stop at 10.** Peaceful appears
/// here at all only because the expression never mentions it — its actual
/// protection is the depletion branch's own guard, which stops the food level ever
/// reaching zero. A gate written as "peaceful never starves" is right about the
/// outcome and wrong about the mechanism, and therefore wrong the moment a command
/// sets the food level directly.
#[must_use]
pub fn starvation_allowed(difficulty: Difficulty, health: f32) -> bool {
    health > 10.0
        || difficulty == Difficulty::Hard
        || (health > 1.0 && difficulty == Difficulty::Normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_player_is_full_with_the_hidden_saturation_buffer() {
        let food = FoodData::default();
        assert_eq!(food.food_level(), 20);
        assert_eq!(food.saturation(), 5.0);
        assert_eq!(food.exhaustion(), 0.0);
    }

    /// **The magnitude gate for the three-layer buffer**, and the number to know:
    /// sprinting costs `0.1` exhaustion per block, `4.0` exhaustion spends one
    /// saturation point, so a fresh spawn's `5.0` saturation is **200 blocks** and
    /// the food level does not move until then.
    ///
    /// Two hypotheses, both computed from vanilla's constants:
    ///
    /// | hypothesis | food level after 200 blocks of sprinting |
    /// |---|---|
    /// | three-layer buffer (correct) | **20** — saturation absorbed all of it |
    /// | exhaustion decrements food directly | **15** — five points gone |
    ///
    /// Both are asserted, the wrong one negatively, because a direction-only check
    /// ("hunger went down") passes under either and is satisfied even by a model
    /// that is five times too fast.
    ///
    /// # The off-by-one, which this gate was first written wrong about
    ///
    /// The first version of this test predicted saturation `0.0` at block 200 and
    /// failed, measuring `1.0`. The cause is that vanilla's threshold is
    /// `exhaustion > 4.0`, **strictly** — so exhaustion has to reach `4.1` and the
    /// *k*-th drop lands on block `40k + 1`, not `40k`. Four drops by block 200,
    /// five by 201, and the food level does not move until block **241**. The
    /// figures below were re-derived arithmetically from the constants rather than
    /// read off a run of this code, which is why they can be exact.
    #[test]
    fn saturation_absorbs_the_first_two_hundred_blocks_of_sprinting() {
        const WRONG_DIRECT_HYPOTHESIS: i32 = 15;
        let sprint = |food: &mut FoodData, blocks: usize| {
            for _ in 0..blocks {
                food.add_exhaustion(EXHAUSTION_SPRINT_PER_BLOCK);
                food.tick(Difficulty::Normal, true, 20.0, 20.0);
            }
        };
        let mut food = FoodData::default();
        sprint(&mut food, 200);
        assert_eq!(
            food.food_level(),
            20,
            "the food level must not move while saturation has anything left"
        );
        assert_ne!(
            food.food_level(),
            WRONG_DIRECT_HYPOTHESIS,
            "landing on {WRONG_DIRECT_HYPOTHESIS} means exhaustion is decrementing food \
             directly and hunger depletes five times too fast"
        );
        assert_eq!(
            food.saturation(),
            1.0,
            "four drops land by block 200 (at 41, 81, 121, 161), not five: the threshold \
             test is strictly greater than 4.0"
        );

        // Block 201 is the fifth drop, which empties the buffer.
        sprint(&mut food, 1);
        assert_eq!(food.saturation(), 0.0, "the fifth drop lands on block 201");
        assert_eq!(food.food_level(), 20, "and it still costs saturation, not food");

        // Block 241 is the sixth, and the first one the player can see.
        sprint(&mut food, 40);
        assert_eq!(
            food.food_level(),
            19,
            "with saturation empty the sixth drop costs food, at block 241"
        );
    }

    /// **Walking costs nothing.** The most commonly-wrong fact about hunger, and the
    /// one vanilla writes as an explicit `0.0F` multiply. A player who walks a
    /// thousand blocks and never sprints must be exactly as full as when they
    /// started, saturation included.
    #[test]
    fn walking_a_thousand_blocks_costs_nothing_at_all() {
        let mut food = FoodData::default();
        for _ in 0..1000 {
            food.add_exhaustion(EXHAUSTION_WALK_PER_BLOCK);
            food.tick(Difficulty::Normal, true, 20.0, 20.0);
        }
        assert_eq!(food.food_level(), 20);
        assert_eq!(food.saturation(), 5.0, "not even the hidden buffer moves");
        assert_eq!(food.exhaustion(), 0.0);
    }

    /// Swimming is a tenth of sprinting's cost, so the same 200 blocks spend
    /// `2.0` exhaustion — not enough for even one `4.0` drop, so nothing changes at
    /// all. The control against a model that used one constant for all movement.
    #[test]
    fn swimming_is_a_tenth_of_sprinting_so_two_hundred_blocks_spend_nothing() {
        let mut food = FoodData::default();
        for _ in 0..200 {
            food.add_exhaustion(EXHAUSTION_SWIM_PER_BLOCK);
            food.tick(Difficulty::Normal, true, 20.0, 20.0);
        }
        assert_eq!(food.saturation(), 5.0, "2.0 exhaustion never reaches the 4.0 drop");
        assert!((food.exhaustion() - 2.0).abs() < 1e-3, "got {}", food.exhaustion());
    }

    /// Exhaustion is capped at `40.0`, so one enormous event costs at most ten
    /// saturation-or-food points rather than the whole bar.
    #[test]
    fn exhaustion_is_capped_at_forty() {
        let mut food = FoodData::default();
        food.add_exhaustion(1_000.0);
        assert_eq!(food.exhaustion(), MAX_EXHAUSTION);
    }

    /// The slow regeneration arm: at food >= 18 and hurt, exactly `1.0` health every
    /// **80** ticks, charging `6.0` exhaustion.
    ///
    /// # Regeneration eats the food bar, and then stops itself
    ///
    /// This gate was also first written wrong — it predicted heals at 80 *and* 160
    /// from a food level of 18, and measured only one. The reason is the charge: the
    /// heal adds `6.0` exhaustion, which is over the `4.0` drop threshold, so the
    /// next tick spends it. With no saturation banked that costs a **food** point,
    /// dropping the bar to 17 — below `HEAL_LEVEL` — and regeneration switches
    /// itself off.
    ///
    /// So the two arms of this gate are the two derived sequences, and the
    /// difference between them is the whole mechanism:
    ///
    /// | starting food | heals in 400 ticks | food afterwards |
    /// |---|---|---|
    /// | 18 | **one**, at tick 80 | 17 — one point below `HEAL_LEVEL` |
    /// | 20 | **three**, at 80 / 160 / 240 | 16 |
    ///
    /// Both figures were re-derived from the constants rather than read off a run.
    /// A model that healed for free would produce five heals from either start.
    #[test]
    fn slow_regeneration_heals_one_health_every_eighty_ticks_and_pays_for_it_in_food() {
        let heals_over = |start_food: i32| {
            let mut food = FoodData::restored(start_food, 0.0, 0.0, 0);
            let mut heals = Vec::new();
            for t in 1..=400 {
                if let Some(amount) = food.tick(Difficulty::Normal, true, 10.0, 20.0).heal {
                    heals.push((t, amount));
                }
            }
            (heals, food.food_level())
        };

        // Starting at exactly HEAL_LEVEL: one heart, and the charge takes the bar
        // below the threshold so nothing more happens.
        let (heals, remaining) = heals_over(18);
        assert_eq!(heals, vec![(80, 1.0)], "one heart, at tick 80");
        assert_eq!(
            remaining, 17,
            "the 6.0 charge costs a food point, which is what stops regeneration"
        );

        // A full bar affords three, at exactly 80-tick spacing.
        let (heals, remaining) = heals_over(20);
        assert_eq!(
            heals,
            vec![(80, 1.0), (160, 1.0), (240, 1.0)],
            "three hearts at 80-tick spacing, then the bar falls below HEAL_LEVEL"
        );
        assert_eq!(remaining, 16, "three heals cost four food points");
    }

    /// **Control**: an unhurt player must not regenerate at all, and the timer must
    /// not accumulate — which is the `else` arm's reset. Without this the gate above
    /// is satisfied by a model that heals unconditionally.
    #[test]
    fn a_full_health_player_never_regenerates() {
        let mut food = FoodData::default();
        for _ in 0..400 {
            let out = food.tick(Difficulty::Normal, true, 20.0, 20.0);
            assert!(out.heal.is_none(), "an unhurt player must not heal");
        }
        assert_eq!(food.tick_timer(), 0, "the idle arm resets the timer");
    }

    /// **Control**: with `natural_health_regeneration` off, neither regeneration arm
    /// runs — and the *starvation* arm still can, because it is not gated by the
    /// rule. Two claims in one, because the rule sits on two of the four arms only.
    #[test]
    fn the_natural_regeneration_rule_gates_healing_but_not_starving() {
        let mut food = FoodData::restored(20, 5.0, 0.0, 0);
        for _ in 0..400 {
            let out = food.tick(Difficulty::Normal, false, 5.0, 20.0);
            assert!(out.heal.is_none(), "the rule is off, so nothing heals");
        }

        let mut starving = FoodData::restored(0, 0.0, 0.0, 0);
        let mut hits = 0;
        for _ in 0..160 {
            if starving.tick(Difficulty::Normal, false, 20.0, 20.0).starve.is_some() {
                hits += 1;
            }
        }
        assert_eq!(hits, 2, "starvation is not gated by the regeneration rule");
    }

    /// The saturated arm heals a **proportional** amount, not a whole heart: with
    /// `3.0` saturation banked the spend is `min(3.0, 6.0) = 3.0` and the heal is
    /// `3.0 / 6.0 = 0.5`.
    ///
    /// The wrong hypothesis is `1.0`, which is what rounding up or reusing the slow
    /// arm's constant produces — roughly double vanilla's regeneration rate.
    #[test]
    fn saturated_regeneration_heals_in_proportion_to_the_saturation_spent() {
        let mut food = FoodData::restored(20, 3.0, 0.0, 0);
        let mut first = None;
        for t in 1..=10 {
            if let Some(amount) = food.tick(Difficulty::Normal, true, 10.0, 20.0).heal {
                first = Some((t, amount));
            }
        }
        let (tick, amount) = first.expect("the fast arm fires within 10 ticks");
        assert_eq!(tick, 10, "HEALTH_TICK_COUNT_SATURATED is 10, not 80");
        assert!(
            (amount - 0.5).abs() < 1e-4,
            "3.0 saturation spent out of 6.0 heals 0.5, not {amount}"
        );
        assert_ne!(amount, 1.0, "1.0 would be the whole-heart hypothesis");
    }

    /// Starvation lands exactly `1.0` every 80 ticks once the food level is zero.
    #[test]
    fn starvation_lands_one_damage_every_eighty_ticks() {
        let mut food = FoodData::restored(0, 0.0, 0.0, 0);
        let mut hits = Vec::new();
        for t in 1..=240 {
            if let Some(amount) = food.tick(Difficulty::Hard, true, 20.0, 20.0).starve {
                hits.push((t, amount));
            }
        }
        assert_eq!(hits, vec![(80, 1.0), (160, 1.0), (240, 1.0)]);
    }

    /// The difficulty gate, at the exact boundaries the record definition names —
    /// and the row that matters is **Peaceful and Easy still starve you down to 10
    /// health**, which is not "peaceful is safe".
    #[test]
    fn the_starvation_gate_matches_the_record_definition_at_every_boundary() {
        // Above 10 health, every difficulty starves.
        for difficulty in [
            Difficulty::Peaceful,
            Difficulty::Easy,
            Difficulty::Normal,
            Difficulty::Hard,
        ] {
            assert!(
                starvation_allowed(difficulty, 10.5),
                "{difficulty:?} must starve a player above 10 health"
            );
        }
        // At or below 10: Easy and Peaceful stop.
        assert!(!starvation_allowed(Difficulty::Peaceful, 10.0));
        assert!(!starvation_allowed(Difficulty::Easy, 10.0));
        // Normal continues to 1.
        assert!(starvation_allowed(Difficulty::Normal, 10.0));
        assert!(starvation_allowed(Difficulty::Normal, 1.5));
        assert!(!starvation_allowed(Difficulty::Normal, 1.0));
        // Hard never stops.
        assert!(starvation_allowed(Difficulty::Hard, 1.0));
        assert!(starvation_allowed(Difficulty::Hard, 0.5));
    }

    /// Peaceful's *real* protection is upstream: the food level never drops, so the
    /// starvation arm is unreachable however long the player sprints.
    ///
    /// The control is the same run on Normal, which must lose food — otherwise this
    /// would pass for a model where exhaustion did nothing at all.
    #[test]
    fn peaceful_never_loses_food_however_much_exhaustion_accumulates() {
        let run = |difficulty: Difficulty| {
            let mut food = FoodData::restored(20, 0.0, 0.0, 0);
            for _ in 0..1000 {
                food.add_exhaustion(EXHAUSTION_SPRINT_PER_BLOCK);
                food.tick(difficulty, false, 20.0, 20.0);
            }
            food.food_level()
        };
        assert_eq!(run(Difficulty::Peaceful), 20, "peaceful never depletes the bar");
        // 1000 blocks is 100.0 exhaustion, i.e. 25 drops — capped by the bar itself.
        assert!(
            run(Difficulty::Normal) < 20,
            "control: the same exhaustion on Normal must deplete, or this gate measures nothing"
        );
    }

    /// `eat` applies `nutrition * modifier * 2.0` saturation, and clamps saturation
    /// to the **new food level** rather than to 20.
    ///
    /// Cooked beef is `nutrition 8, saturation modifier 0.8` in 26.2, giving
    /// `8 * 0.8 * 2 = 12.8` saturation. The dropped-`* 2.0` hypothesis is `6.4`.
    #[test]
    fn eating_applies_the_doubled_saturation_modifier_and_clamps_to_the_new_food_level() {
        let mut food = FoodData::restored(4, 0.0, 0.0, 0);
        food.eat(8, 0.8);
        assert_eq!(food.food_level(), 12, "4 + 8");
        assert!(
            (food.saturation() - 12.0).abs() < 1e-4,
            "12.8 saturation clamps to the new food level of 12, got {}",
            food.saturation()
        );

        // With room to spare, the full 12.8 lands — which is what separates the
        // doubled modifier from the halved one.
        let mut full = FoodData::restored(20, 0.0, 0.0, 0);
        full.eat(8, 0.8);
        assert!(
            (full.saturation() - 12.8).abs() < 1e-3,
            "8 * 0.8 * 2.0 = 12.8, not 6.4; got {}",
            full.saturation()
        );
    }

    /// `restored` clamps rather than trusts: saturation above the stored food level
    /// would keep the fast-regeneration arm armed forever.
    #[test]
    fn restored_clamps_every_field_into_its_legal_range() {
        let food = FoodData::restored(99, 50.0, 500.0, 9_999);
        assert_eq!(food.food_level(), MAX_FOOD);
        assert_eq!(food.saturation(), MAX_FOOD as f32);
        assert_eq!(food.exhaustion(), MAX_EXHAUSTION);
        assert_eq!(food.tick_timer(), HEALTH_TICK_COUNT);

        let negative = FoodData::restored(-5, -1.0, -1.0, -1);
        assert_eq!(negative.food_level(), 0);
        assert_eq!(negative.saturation(), 0.0);
        assert_eq!(negative.exhaustion(), 0.0);
        assert_eq!(negative.tick_timer(), 0);
    }

    /// `display_changed` fires only when the client-visible numbers moved, so a
    /// sprinting player does not provoke a `SetHealth` every tick.
    #[test]
    fn only_a_visible_change_marks_the_tick_as_worth_sending() {
        let mut food = FoodData::default();
        // Exhaustion below the drop threshold: nothing visible moves.
        food.add_exhaustion(1.0);
        let quiet = food.tick(Difficulty::Normal, true, 20.0, 20.0);
        assert!(!quiet.display_changed, "exhaustion alone is invisible to the client");

        // Enough to cross the threshold: saturation drops, which the HUD does read.
        food.add_exhaustion(4.0);
        let loud = food.tick(Difficulty::Normal, true, 20.0, 20.0);
        assert!(loud.display_changed, "a saturation drop must be broadcast");
    }
}
