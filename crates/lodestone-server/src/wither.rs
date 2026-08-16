//! The wither boss fight — a port of `WitherBoss`/`WitherSkull`/
//! `WitherSkullBlock`'s summon detection and combat rules
//! (`.cache/mc/26.2/src/net/minecraft/world/entity/boss/wither/WitherBoss.java`,
//! `.../world/entity/projectile/hurtingprojectile/WitherSkull.java`,
//! `.../world/level/block/WitherSkullBlock.java`).
//!
//! # What it is
//!
//! A pure, world-free module — no world, no entity, no packet — following the
//! shape [`crate::dragon`] already established for the other boss fight (see
//! `docs/dragon-fight.md`): every function here takes inputs and returns a
//! new value or an effect for a caller to perform. The block-pattern search
//! itself lives in `crate::mobs::wither_pattern`, next to
//! `crate::mobs::golem`'s own matcher, because it needs `MobSim`'s
//! `try_construct_golem` machinery to share a caller.
//!
//! # The three-clause invulnerable "emerging" phase, each clause cited
//!
//! `WitherBoss.makeInvulnerable`/`customServerAiStep`'s invulnerable branch:
//!
//! 1. `this.setInvulnerableTicks(220); this.setHealth(this.getMaxHealth() / 3.0F);`
//!    — [`spawn_health`], [`INVULNERABLE_TICKS`].
//! 2. Every tick: `newCount = ticks - 1; bossEvent.setProgress(1.0F - newCount / 220.0F);`
//!    — [`invulnerable_tick`], [`boss_bar_progress_while_invulnerable`].
//! 3. `if (this.tickCount % 10 == 0) this.heal(10.0F);` — a **10 HP heal every
//!    10 ticks while invulnerable**, distinct from the 1 HP/20 ticks the
//!    active phase below uses — [`should_heal_while_invulnerable`].
//!
//! When `newCount <= 0`: `level.explode(this, x, eyeY, z, 7.0F, false, MOB)`
//! — the emergence blast, [`WitherEffect::EmergeBlast`] and
//! [`EMERGE_EXPLOSION_POWER`]. This is the "damage-aura pulse against nearby
//! entities" issue #278 names: a single radius-based `MOB`-interaction
//! explosion at the moment invulnerability ends, not a recurring aura —
//! there is no separate periodic entity-damage pulse anywhere in
//! `WitherBoss.java` (its only other periodic effect,
//! `destroyBlocksTick`, breaks **blocks**, not entities, and is not ported
//! here — see "What this does not attempt" below).
//!
//! # The active phase
//!
//! `else { super.customServerAiStep(level); ...; if (tickCount % 20 == 0)
//! heal(1.0F); bossEvent.setProgress(health / maxHealth); }` —
//! [`should_heal_while_active`], [`boss_bar_progress_while_active`].
//! `isPowered()` (`health <= maxHealth / 2.0F`) gates two things: the
//! "wither armor" that makes the wither immune to arrows and wind charges
//! while below half health ([`blocks_projectile_while_powered`]), and a
//! purely cosmetic particle tint this module does not model.
//!
//! # Damage/invulnerability gates, from `hurtServer`
//!
//! * `if (this.getInvulnerableTicks() > 0 && !source.is(BYPASSES_INVULNERABILITY)) return false;`
//!   — [`blocked_by_emerging_invulnerability`].
//! * `if (this.isPowered()) { if (directEntity instanceof AbstractArrow || WindCharge) return false; }`
//!   — [`blocks_projectile_while_powered`].
//! * `if (sourceEntity != null && sourceEntity.is(WITHER_FRIENDS)) return false;`
//!   and `if (source.getEntity() instanceof WitherBoss) return false;` — both
//!   are targeting/friendly-fire exemptions this module does not model (no
//!   entity-tag registry or wither-vs-wither identity check reaches this
//!   crate); disclosed rather than silently applied.
//!
//! # `WitherSkull`'s own numbers
//!
//! `onHitEntity`: `hurtServer(..., damageSources().witherSkull(this, owner), 8.0F)`
//! when the shooter is a living owner ([`SKULL_DAMAGE_WITH_OWNER`]), the
//! `5.0F` no-owner case ([`SKULL_DAMAGE_NO_OWNER`]) is unreachable in this
//! sim's production wiring (every skull this crate spawns has a real owner —
//! see `mobs::wither`'s own doc). A killing hit heals the owner
//! [`OWNER_HEAL_ON_KILL`] `5.0F`. A landed hit additionally applies
//! `MobEffects.WITHER` at `Normal` = 10s / `Hard` = 40s, amplifier `1`
//! ([`wither_effect_ticks`]). `onHit` (any surface, not just an entity)
//! always explodes at [`SKULL_EXPLOSION_POWER`] `1.0F`, `MOB` interaction,
//! then discards the skull — [`WitherEffect::SkullImpactBlast`].
//!
//! # What this does not attempt, and why
//!
//! * **`destroyBlocksTick`'s block-destruction pulse** (20 ticks after being
//!   hurt, breaks non-wither-immune blocks in a radius around the wither) —
//!   a world-mutation feature this pure module has no block-write authority
//!   for, matching `crate::dragon::fight`'s own "no block-write authority"
//!   scope note. Not the same mechanic as the emergence blast above.
//! * **The three independently-targeting heads.** Vanilla tracks
//!   `DATA_TARGET_A/B/C` (three `EntityDataAccessor<Integer>`s, indices 16-18
//!   per the committed
//!   `crates/protocol/v770/tests/support/entity_data_index_jvm.txt` dump —
//!   see `mobs::wither`'s own doc for the full collision census) and fires
//!   two side heads independently of the main ranged-attack head, including
//!   an idle "no target found in 15 updates -> fire a dangerous skull at a
//!   random nearby point" branch. This module exposes one skull-firing
//!   cooldown per wither rather than three — a named scope reduction, not an
//!   oversight, so `mobs::wither`'s single firing schedule stands in for
//!   vanilla's three.
//! * **`isPowered`'s cosmetic particle tint** and the emergence's smoke
//!   particles (`aiStep`, client-visible only) — no client-visible effect
//!   pipeline in this crate's remit renders wither particles.

/// `WitherBoss.INVULNERABLE_TICKS` — the "emerging" phase's duration.
pub const INVULNERABLE_TICKS: i32 = 220;

/// `WitherBoss.makeInvulnerable`'s heal-tick interval while invulnerable —
/// **not** the same interval the active phase uses below.
pub const HEAL_INTERVAL_INVULNERABLE_TICKS: i64 = 10;
/// The heal amount that interval applies — `this.heal(10.0F)`.
pub const HEAL_AMOUNT_INVULNERABLE: f32 = 10.0;

/// `WitherBoss.customServerAiStep`'s active-phase heal interval —
/// `this.tickCount % 20 == 0`. Four times sparser than the invulnerable-phase
/// interval above; a gate predicting one from the other would be checking the
/// wrong constant, which is exactly why both are named separately rather than
/// factored into one "heal interval" the caller parameterizes.
pub const HEAL_INTERVAL_ACTIVE_TICKS: i64 = 20;
/// The active-phase heal amount — `this.heal(1.0F)`.
pub const HEAL_AMOUNT_ACTIVE: f32 = 1.0;

/// `Level.explode(..., 7.0F, false, MOB)` — the emergence blast's power.
pub const EMERGE_EXPLOSION_POWER: f32 = 7.0;

/// `WitherSkull.onHit`'s own `explode(..., 1.0F, false, MOB)`.
pub const SKULL_EXPLOSION_POWER: f32 = 1.0;

/// `WitherSkull.onHitEntity`'s damage when the shooter is a living owner —
/// the case this sim's production skull spawns always hit (see module doc).
pub const SKULL_DAMAGE_WITH_OWNER: f32 = 8.0;
/// The no-owner case — `hurtServer(..., damageSources().magic(), 5.0F)`.
/// Unreachable in production (see module doc); kept as a named constant
/// rather than a bare literal so a future dispenser-fired skull (vanilla
/// supports firing a wither skull from a dispenser) has somewhere correct to
/// read it from.
pub const SKULL_DAMAGE_NO_OWNER: f32 = 5.0;
/// `livingOwner.heal(5.0F)` — the owner's reward for a killing skull hit.
pub const OWNER_HEAL_ON_KILL: f32 = 5.0;

/// `WitherSkull.performRangedAttack`'s `head == 0 && random.nextFloat() < 0.001F` —
/// the main head's own tiny chance of firing a "dangerous" (blue, block-
/// breaking) skull even at a real target, independent of the idle-random-fire
/// branch this module does not port (see module doc).
pub const MAIN_HEAD_DANGEROUS_CHANCE: f32 = 0.001;

/// The wither's own `isPowered` threshold — `health <= maxHealth / 2.0F`.
#[must_use]
pub fn is_powered(health: f32, max_health: f32) -> bool {
    health <= max_health / 2.0
}

/// `WitherBoss.makeInvulnerable`'s `setHealth(maxHealth / 3.0F)` — the health
/// a freshly-summoned wither starts at, **not** full health (it heals up to
/// full over the 220-tick emergence, `HEAL_AMOUNT_INVULNERABLE` at a time).
#[must_use]
pub fn spawn_health(max_health: f32) -> f32 {
    max_health / 3.0
}

/// One effect [`invulnerable_tick`] or a skull impact can report — a world
/// mutation the caller performs, since this module has no world to act on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WitherEffect {
    /// The emergence blast: `EMERGE_EXPLOSION_POWER` at the wither's eye
    /// position, `MOB` interaction, plus vanilla's level event `1023`
    /// (wither-spawn sound) if not silent.
    EmergeBlast,
    /// `WitherSkull.onHit`'s unconditional impact explosion —
    /// `SKULL_EXPLOSION_POWER`, `MOB` interaction, at the skull's own impact
    /// point, regardless of whether it struck a block or an entity.
    SkullImpactBlast,
}

/// One tick of the invulnerable countdown — `WitherBoss.customServerAiStep`'s
/// invulnerable branch, minus the heal (see [`should_heal_while_invulnerable`],
/// applied separately since it needs the entity's own age, not this
/// countdown). Returns the new tick count and, on the tick it reaches zero,
/// [`WitherEffect::EmergeBlast`] for the caller to perform.
///
/// Call only while `ticks > 0`; a caller that keeps calling this once the
/// count reaches `0` gets `(0, None)` forever rather than going negative —
/// vanilla's own `newCount <= 0` guard means the invulnerable branch is never
/// entered again once the phase ends, so there is no vanilla behaviour to
/// match past the first zero.
#[must_use]
pub fn invulnerable_tick(ticks: i32) -> (i32, Option<WitherEffect>) {
    if ticks <= 0 {
        return (0, None);
    }
    let new_count = ticks - 1;
    if new_count <= 0 {
        (0, Some(WitherEffect::EmergeBlast))
    } else {
        (new_count, None)
    }
}

/// `bossEvent.setProgress(1.0F - newCount / 220.0F)` — the boss bar fills as
/// the countdown empties. `ticks_remaining` is the value **after**
/// [`invulnerable_tick`]'s decrement (`newCount`), matching vanilla's own
/// read order (the progress line runs immediately after `setInvulnerableTicks`
/// in the same tick).
#[must_use]
pub fn boss_bar_progress_while_invulnerable(ticks_remaining: i32) -> f32 {
    1.0 - (ticks_remaining as f32) / (INVULNERABLE_TICKS as f32)
}

/// `bossEvent.setProgress(health / maxHealth)` — the active-phase bar.
#[must_use]
pub fn boss_bar_progress_while_active(health: f32, max_health: f32) -> f32 {
    (health / max_health).clamp(0.0, 1.0)
}

/// `this.tickCount % 10 == 0` — the invulnerable-phase heal roll.
#[must_use]
pub fn should_heal_while_invulnerable(entity_age: i64) -> bool {
    entity_age.rem_euclid(HEAL_INTERVAL_INVULNERABLE_TICKS) == 0
}

/// `this.tickCount % 20 == 0` — the active-phase heal roll.
#[must_use]
pub fn should_heal_while_active(entity_age: i64) -> bool {
    entity_age.rem_euclid(HEAL_INTERVAL_ACTIVE_TICKS) == 0
}

/// `WitherBoss.hurtServer`'s invulnerability gate:
/// `getInvulnerableTicks() > 0 && !source.is(BYPASSES_INVULNERABILITY)`.
/// Returns `true` when the hit must be refused.
#[must_use]
pub fn blocked_by_emerging_invulnerability(invulnerable_ticks: i32, bypasses_invulnerability: bool) -> bool {
    invulnerable_ticks > 0 && !bypasses_invulnerability
}

/// `WitherBoss.hurtServer`'s powered-armor gate:
/// `isPowered() && (directEntity instanceof AbstractArrow || WindCharge)`.
/// Returns `true` when the hit must be refused. `is_arrow_or_wind_charge` is
/// the caller's own classification of the *direct* damage source entity
/// (not, e.g., a thrown trident, which is a distinct type from
/// `AbstractArrow` in vanilla despite riding the same flight code).
#[must_use]
pub fn blocks_projectile_while_powered(is_powered_now: bool, is_arrow_or_wind_charge: bool) -> bool {
    is_powered_now && is_arrow_or_wind_charge
}

/// `WitherSkull.onHitEntity`'s wither-effect duration by difficulty —
/// `Normal -> 10s, Hard -> 40s, else -> 0` (no effect applied), always
/// amplifier `1`. Returns ticks (`20 * seconds`), `0` meaning "do not apply".
#[must_use]
pub fn wither_effect_ticks(difficulty: lodestone_model::Difficulty) -> i32 {
    use lodestone_model::Difficulty;
    match difficulty {
        Difficulty::Normal => 20 * 10,
        Difficulty::Hard => 20 * 40,
        Difficulty::Peaceful | Difficulty::Easy => 0,
    }
}

/// `MobEffectInstance(MobEffects.WITHER, ticks, 1)`'s amplifier — always `1`
/// (displayed as "Wither II"), regardless of difficulty.
pub const WITHER_EFFECT_AMPLIFIER: u32 = 1;

/// `head == 0 && random.nextFloat() < 0.001F` — `roll` is the caller's own
/// `[0.0, 1.0)` draw.
#[must_use]
pub fn should_fire_dangerous_skull(roll: f32) -> bool {
    roll < MAIN_HEAD_DANGEROUS_CHANCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::Difficulty;

    #[test]
    fn spawn_health_is_one_third_max_not_full() {
        assert_eq!(spawn_health(300.0), 100.0);
    }

    #[test]
    fn is_powered_flips_exactly_at_half_health() {
        assert!(!is_powered(150.1, 300.0), "just above half must not be powered");
        assert!(is_powered(150.0, 300.0), "exactly half must be powered");
        assert!(is_powered(149.9, 300.0));
    }

    #[test]
    fn invulnerable_countdown_reaches_zero_after_exactly_220_ticks() {
        let mut ticks = INVULNERABLE_TICKS;
        let mut effect = None;
        for i in 0..220 {
            let (new_ticks, e) = invulnerable_tick(ticks);
            ticks = new_ticks;
            effect = e;
            if i < 219 {
                assert_eq!(effect, None, "must not emerge before the 220th decrement (tick {i})");
            }
        }
        assert_eq!(ticks, 0);
        assert_eq!(effect, Some(WitherEffect::EmergeBlast), "the 220th decrement must fire the emergence blast");
    }

    #[test]
    fn invulnerable_tick_is_idempotent_once_it_reaches_zero() {
        assert_eq!(invulnerable_tick(0), (0, None));
        assert_eq!(invulnerable_tick(-1), (0, None), "must not go negative if ever called past zero");
    }

    #[test]
    fn boss_bar_progress_starts_empty_and_ends_full() {
        // Predict the exact values, not just "increases" — a gate asserting
        // only monotonicity would pass a bar that fills backwards from a
        // different formula.
        assert_eq!(boss_bar_progress_while_invulnerable(220), 0.0);
        assert_eq!(boss_bar_progress_while_invulnerable(110), 0.5);
        assert_eq!(boss_bar_progress_while_invulnerable(0), 1.0);
        // 220 is not a coincidental round divisor of every intermediate value
        // — 33 remaining is a real fraction, not a plausible guess.
        let expected = 1.0 - 33.0 / 220.0;
        assert!((boss_bar_progress_while_invulnerable(33) - expected).abs() < 1e-6);
    }

    #[test]
    fn active_boss_bar_is_the_health_fraction() {
        assert_eq!(boss_bar_progress_while_active(300.0, 300.0), 1.0);
        assert_eq!(boss_bar_progress_while_active(75.0, 300.0), 0.25);
        assert_eq!(boss_bar_progress_while_active(0.0, 300.0), 0.0);
    }

    #[test]
    fn heal_intervals_are_ten_and_twenty_not_interchangeable() {
        // Pick ticks where the two hypotheses (10 vs 20) disagree: 10 itself.
        assert!(should_heal_while_invulnerable(10));
        assert!(!should_heal_while_active(10), "20-tick interval must not fire at tick 10");
        assert!(should_heal_while_active(20));
        assert!(should_heal_while_invulnerable(20), "10-tick interval also fires at every multiple of 20");
    }

    #[test]
    fn emerging_invulnerability_blocks_a_hit_unless_it_bypasses() {
        assert!(blocked_by_emerging_invulnerability(1, false));
        assert!(!blocked_by_emerging_invulnerability(1, true), "a bypassing damage type must land");
        assert!(!blocked_by_emerging_invulnerability(0, false), "no longer invulnerable at 0");
    }

    #[test]
    fn powered_armor_blocks_only_arrows_and_wind_charges() {
        assert!(blocks_projectile_while_powered(true, true));
        assert!(!blocks_projectile_while_powered(true, false), "e.g. a melee hit must still land while powered");
        assert!(!blocks_projectile_while_powered(false, true), "not powered: arrows land normally");
    }

    #[test]
    fn wither_effect_duration_differs_by_difficulty_and_hard_is_not_double_normal() {
        // A "just double" hypothesis (10 -> 20) would be wrong: it's 4x.
        assert_eq!(wither_effect_ticks(Difficulty::Normal), 200);
        assert_eq!(wither_effect_ticks(Difficulty::Hard), 800);
        assert_eq!(wither_effect_ticks(Difficulty::Easy), 0);
        assert_eq!(wither_effect_ticks(Difficulty::Peaceful), 0);
        assert_ne!(
            wither_effect_ticks(Difficulty::Hard),
            2 * wither_effect_ticks(Difficulty::Normal),
            "this assertion documents that hard is 4x, not 2x, normal — a control against the plausible-round-number trap"
        );
    }

    #[test]
    fn dangerous_skull_roll_is_a_tight_probability_not_a_coin_flip() {
        assert!(should_fire_dangerous_skull(0.0));
        assert!(should_fire_dangerous_skull(0.0009));
        assert!(!should_fire_dangerous_skull(0.001), "the bound itself is exclusive");
        assert!(!should_fire_dangerous_skull(0.5));
    }
}
