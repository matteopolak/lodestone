//! Server-authoritative air supply and drowning damage (issue #267).
//!
//! # Where the truth comes from
//!
//! `LivingEntity.baseTick` (`.cache/mc/26.2/src/net/minecraft/world/entity/
//! LivingEntity.java:436-458`), the override `Entity.baseTick` itself does
//! not touch air at all — the water-breath block lives one class down:
//!
//! ```java
//! if (this.isEyeInFluid(FluidTags.WATER) && !level.getBlockState(eyePos).is(Blocks.BUBBLE_COLUMN)) {
//!     boolean canDrownInWater = !this.canBreatheUnderwater()
//!         && !MobEffectUtil.hasWaterBreathing(this)
//!         && (!isPlayer || !((Player) this).getAbilities().invulnerable);
//!     if (canDrownInWater) {
//!         this.setAirSupply(this.decreaseAirSupply(this.getAirSupply()));
//!         if (this.shouldTakeDrowningDamage()) {
//!             this.setAirSupply(0);
//!             level.broadcastEntityEvent(this, (byte) 67);
//!             this.hurtServer(level, this.damageSources().drown(), 2.0F);
//!         }
//!     } else if (this.getAirSupply() < this.getMaxAirSupply() && MobEffectUtil.shouldEffectsRefillAirsupply(this)) {
//!         this.setAirSupply(this.increaseAirSupply(this.getAirSupply()));
//!     }
//! } else if (this.getAirSupply() < this.getMaxAirSupply()) {
//!     this.setAirSupply(this.increaseAirSupply(this.getAirSupply()));
//! }
//! ```
//!
//! `decreaseAirSupply`/`increaseAirSupply`/`shouldTakeDrowningDamage`
//! (`LivingEntity.java:588-601`, `:506-508`):
//!
//! ```java
//! protected int decreaseAirSupply(int currentSupply) {
//!     AttributeInstance respiration = this.getAttribute(Attributes.OXYGEN_BONUS);
//!     double oxygenBonus = respiration != null ? respiration.getValue() : 0.0;
//!     return oxygenBonus > 0.0 && this.random.nextDouble() >= 1.0 / (oxygenBonus + 1.0)
//!         ? currentSupply : currentSupply - 1;
//! }
//! protected int increaseAirSupply(int currentSupply) {
//!     return Math.min(currentSupply + 4, this.getMaxAirSupply());
//! }
//! protected boolean shouldTakeDrowningDamage() {
//!     return this.getAirSupply() <= -20;
//! }
//! ```
//!
//! `Entity.TOTAL_AIR_SUPPLY = 300` (`Entity.java:194`) is the max/starting
//! value returned by the default `getMaxAirSupply()` (`Entity.java:2805`).
//!
//! # The cadence this produces
//!
//! Air decrements by exactly **1 per tick** while submerged (no respiration
//! modelled — see below), so a fully-submerged player takes exactly **300
//! ticks (15s)** to empty from full, then **20 more ticks (1s)** to reach the
//! `<= -20` threshold — at which point air is reset to `0` and a **2.0**
//! (one heart) hit lands via `damageSources().drown()`. Because the reset
//! re-arms the identical countdown, every subsequent hit is another flat
//! **20 ticks (1s)** apart, not "every tick underwater" — a player who is
//! merely low on air (air still `> -20`) must take **zero** damage, which is
//! exactly what the `shouldTakeDrowningDamage` gate is for and what this
//! module's negative-air-no-damage-yet test checks.
//!
//! Refilling is **gradual**, not instant, once the eye clears the water (or,
//! per the `else if` above, whenever it is not submerged at all):
//! `Math.min(currentSupply + 4, max)` per tick — full recovery from `0` takes
//! `ceil(300 / 4) = 75` ticks (3.75s). This must agree with the client's
//! `getCurrentAirSupplyBubble` ceiling-based bubble-count mapping
//! (`docs/sky-and-air-bubbles.md`), which is why this ticks the exact same
//! `+4`-capped-at-max integer step vanilla does rather than any smoothed
//! approximation.
//!
//! # What is deliberately not modelled, and why
//!
//! * **Respiration (`Attributes.OXYGEN_BONUS`) and the water-breathing /
//!   conduit-power effects** (`decreaseAirSupply`'s `oxygenBonus` term,
//!   `MobEffectUtil.hasWaterBreathing`/`shouldEffectsRefillAirsupply`).
//!   Nothing in `lodestone-server` or `lodestone-entity` models potion
//!   effects or enchantments at all yet (`grep -rl "MobEffect\|Enchantment"`
//!   across both crates turns up nothing but a doc-comment mention in
//!   `lodestone-entity::damage`), so there is no attribute or effect state
//!   to read here. `decrease_air_supply` below is the unconditional `-1`
//!   branch only. A future enchantment/effect system is the natural place to
//!   wire this back in — the seam is exactly `PlayerVitals::tick`'s
//!   `eye_in_water` boolean plus a rate parameter, not a rewrite.
//! * **Bubble columns** (`!level.getBlockState(eyePos).is(Blocks
//!   .BUBBLE_COLUMN)`). The overworld generator this crate serves does not
//!   place bubble columns (`docs/served-session-liveness.md` /
//!   `worldgen_data`'s documented scope), so the guard can never actually
//!   fire against real terrain; omitted rather than dead code.
//! * **Invulnerability i-frames** (vanilla's `invulnerableTime`, ticked
//!   elsewhere in `LivingEntity.baseTick` and consulted by `hurtServer`).
//!   Nothing else damages the player yet in this crate (no melee, no fall
//!   damage, no explosions reaching a player) for i-frames to matter
//!   against, and the drowning cadence above is already governed by the
//!   `shouldTakeDrowningDamage` reset, not by how fast repeated hits could
//!   land. Deferred, not forgotten — a second damage source landing in the
//!   same tick window as drowning would currently double-apply.
//! * **Non-player entities.** `Entity` carries air supply too (mobs can
//!   drown), but [`MobSim`](crate::MobSim) has no health-vs-submersion tick
//!   at all right now and streams no metadata to a client (see its own
//!   module doc comment: "does not stream the resulting positions to a
//!   connected client"). This module is player-only; a mob-drowning ticker
//!   would be a `MobSim::tick` addition reusing the same pure
//!   `PlayerVitals`-shaped step function, not a reason to touch this file.
//! * **Death/respawn.** Vanilla's air/drown block is guarded by
//!   `this.isAlive()` one level up; [`PlayerVitals::tick`] mirrors that by
//!   becoming a no-op once `health` reaches `0.0`. No death screen, no
//!   respawn packet, no corpse — out of scope for this issue, exactly like
//!   `SetHealth`'s own doc comment already flags for the offline-mode dead-
//!   player case.

/// Vanilla's `Entity.TOTAL_AIR_SUPPLY` (`Entity.java:194`) — the default
/// `getMaxAirSupply()` (`Entity.java:2805-2807`) for anything that does not
/// override it (nothing this crate models does).
pub const MAX_AIR_SUPPLY: i32 = 300;

/// Vanilla's `Avatar.DEFAULT_EYE_HEIGHT` (`.cache/mc/26.2/src/net/minecraft/
/// world/entity/Avatar.java:16`, also `:22`'s `withEyeHeight(1.62F)` on the
/// standing pose) — the only pose this crate's server-side player tracks, so
/// this is *the* eye-height constant rather than a pose-keyed table.
pub const EYE_HEIGHT: f64 = 1.62;

/// `LivingEntity.shouldTakeDrowningDamage`'s threshold (`LivingEntity.java:
/// 506-508`): `getAirSupply() <= -20`.
const DROWNING_DAMAGE_AIR_THRESHOLD: i32 = -20;

/// The raw drowning hit, matching `this.hurtServer(level,
/// this.damageSources().drown(), 2.0F)` (`LivingEntity.java:447`). Applied
/// directly to health: this crate has no player armour/absorption model to
/// reduce it through (the same "no inventory model" scope note
/// `crate::server`'s `UseItemOn` handling already carries).
pub const DROWN_DAMAGE: f32 = 2.0;

/// Starting/full player health. Matches `V770ServerProtocol::begin_play`'s
/// `SetHealth { health: 20.0, .. }` fresh-spawn default.
pub const MAX_HEALTH: f32 = 20.0;

/// What changed on one [`PlayerVitals::tick`] call, so the caller knows which
/// client-bound packets (if any) are worth sending. Both fields can be set on
/// the same tick — the tick that crosses the drowning threshold changes air
/// (reset to `0`) *and* deals damage in the same step, exactly as vanilla's
/// `setAirSupply(0)` immediately followed by `hurtServer` does.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VitalsTick {
    /// `Some(new_air)` when air supply changed this tick.
    pub air_changed: Option<i32>,
    /// `Some(damage_dealt)` when drowning damage landed this tick. The
    /// resulting health is [`PlayerVitals::health`] after the call, not
    /// carried here, so a caller that only wants "did damage land" need not
    /// separately track health.
    pub damage: Option<f32>,
}

impl VitalsTick {
    /// Whether this tick produced anything worth broadcasting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.air_changed.is_none() && self.damage.is_none()
    }
}

/// One connection's server-authoritative air supply and health, ticked once
/// per server tick against whether the player's eye is currently submerged.
///
/// This is deliberately a plain value type with a pure [`tick`](Self::tick)
/// step — no `ChunkSource`, no connection, no async — so the submersion test
/// (which does need the terrain and the player's position) stays in
/// `crate::server` and this module stays unit-testable in isolation, the
/// same split [`crate::mobs::SimMob::apply_damage`] draws between "what
/// changed" and "how the world decided it should change".
#[derive(Debug, Clone, Copy)]
pub struct PlayerVitals {
    air_supply: i32,
    health: f32,
}

impl Default for PlayerVitals {
    /// A freshly joined player: full air, full health — matching
    /// `V770ServerProtocol::begin_play`'s fresh-spawn `SetHealth` and the
    /// metadata default (`entityDataBuilder.define(DATA_AIR_SUPPLY_ID,
    /// this.getMaxAirSupply())`, `Entity.java:319`).
    fn default() -> Self {
        Self {
            air_supply: MAX_AIR_SUPPLY,
            health: MAX_HEALTH,
        }
    }
}

impl PlayerVitals {
    /// Current air supply, in ticks (`-20..=300`; briefly negative between
    /// the tick that exhausts it and the tick that resets it to `0` on a
    /// drowning hit).
    #[must_use]
    pub fn air_supply(&self) -> i32 {
        self.air_supply
    }

    /// Current health (`0.0..=20.0`). `0.0` means dead; [`tick`](Self::tick)
    /// becomes a no-op once this is reached (mirrors vanilla's `isAlive()`
    /// guard one level up from the air/drown block — see this module's own
    /// doc comment for why death/respawn are out of scope beyond that).
    #[must_use]
    pub fn health(&self) -> f32 {
        self.health
    }

    /// Advances vitals by exactly one server tick, given whether the eye is
    /// currently submerged in water (the caller's job — see
    /// `crate::server`'s use of [`ChunkSource::block_state`](crate::ChunkSource)
    /// at the eye position). Mirrors `LivingEntity.baseTick`'s water-breath
    /// block byte-for-byte within this module's documented scope (no
    /// respiration/water-breathing, no i-frames, no bubble columns).
    pub fn tick(&mut self, eye_in_water: bool) -> VitalsTick {
        if self.health <= 0.0 {
            return VitalsTick::default();
        }

        let mut out = VitalsTick::default();

        if eye_in_water {
            let before = self.air_supply;
            // `decreaseAirSupply` with no `OXYGEN_BONUS` attribute: the flat
            // `currentSupply - 1` branch, unconditionally (see module docs).
            self.air_supply -= 1;
            if self.air_supply != before {
                out.air_changed = Some(self.air_supply);
            }

            if self.air_supply <= DROWNING_DAMAGE_AIR_THRESHOLD {
                self.air_supply = 0;
                out.air_changed = Some(self.air_supply);
                self.health = (self.health - DROWN_DAMAGE).max(0.0);
                out.damage = Some(DROWN_DAMAGE);
            }
        } else if self.air_supply < MAX_AIR_SUPPLY {
            let before = self.air_supply;
            self.air_supply = (self.air_supply + 4).min(MAX_AIR_SUPPLY);
            if self.air_supply != before {
                out.air_changed = Some(self.air_supply);
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_vitals_are_full_air_and_full_health() {
        let v = PlayerVitals::default();
        assert_eq!(v.air_supply(), MAX_AIR_SUPPLY);
        assert_eq!(v.health(), MAX_HEALTH);
    }

    /// **Control**: a dry tick (eye not in water) at full air must change
    /// nothing — no air event, no damage. This is the negative control that
    /// proves the "increase" branch does not fire spuriously when there is
    /// nothing to refill.
    #[test]
    fn dry_tick_at_full_air_changes_nothing() {
        let mut v = PlayerVitals::default();
        let out = v.tick(false);
        assert!(out.is_empty(), "expected no change, got {out:?}");
        assert_eq!(v.air_supply(), MAX_AIR_SUPPLY);
        assert_eq!(v.health(), MAX_HEALTH);
    }

    /// The exact vanilla cadence: 300 ticks to empty from full, then exactly
    /// 20 more to reach the `<= -20` threshold — 320 submerged ticks to the
    /// first hit, not some rounder or approximated number.
    #[test]
    fn first_drowning_hit_lands_at_exactly_tick_320() {
        let mut v = PlayerVitals::default();
        let mut first_damage_tick = None;
        for t in 1..=320 {
            let out = v.tick(true);
            if out.damage.is_some() {
                first_damage_tick = Some(t);
                break;
            }
        }
        assert_eq!(
            first_damage_tick,
            Some(320),
            "expected the first hit at tick 320 (300 to empty + 20 to -20)"
        );
        assert_eq!(v.health(), MAX_HEALTH - DROWN_DAMAGE);
        assert_eq!(v.air_supply(), 0, "air must reset to 0 on the hit, not stay at -20");
    }

    /// **Control**: at tick 319 — one tick short of the threshold — zero
    /// damage must have landed yet, even though air is deeply negative
    /// (`-19`). This is the "full air must not take damage" / "gate
    /// actually gates" control: air being low is not itself sufficient,
    /// only crossing `<= -20` is.
    #[test]
    fn no_damage_before_the_threshold_is_actually_crossed() {
        let mut v = PlayerVitals::default();
        for _ in 1..=319 {
            let out = v.tick(true);
            assert!(out.damage.is_none(), "damage landed before tick 320");
        }
        assert_eq!(v.air_supply(), -19);
        assert_eq!(v.health(), MAX_HEALTH, "health must be untouched before the threshold");
    }

    /// After the first hit re-arms the countdown at `0`, the next hit is
    /// exactly 20 ticks later — proving the cadence is a fixed repeat, not a
    /// one-off.
    #[test]
    fn second_drowning_hit_lands_exactly_20_ticks_after_the_first() {
        let mut v = PlayerVitals::default();
        let mut hits = Vec::new();
        for t in 1..=340 {
            let out = v.tick(true);
            if out.damage.is_some() {
                hits.push(t);
            }
        }
        assert_eq!(hits, vec![320, 340], "expected hits exactly 20 ticks apart");
        assert_eq!(v.health(), MAX_HEALTH - 2.0 * DROWN_DAMAGE);
    }

    /// Refill is gradual (`+4`/tick, capped), not instant: from `0` it takes
    /// `ceil(300/4) = 75` ticks to reach max, and must never overshoot it.
    #[test]
    fn refill_is_gradual_and_caps_at_max_air() {
        let mut v = PlayerVitals::default();
        // Drain to exactly 0 without crossing the damage threshold.
        for _ in 1..=300 {
            v.tick(true);
        }
        assert_eq!(v.air_supply(), 0);

        for t in 1..=74 {
            let out = v.tick(false);
            assert_eq!(out.air_changed, Some(4 * t), "tick {t}");
            assert!(out.damage.is_none());
        }
        assert_eq!(v.air_supply(), 296, "74 ticks of +4 from 0 is 296, not yet full");

        let out = v.tick(false);
        assert_eq!(out.air_changed, Some(MAX_AIR_SUPPLY), "75th tick reaches the cap");
        assert_eq!(v.air_supply(), MAX_AIR_SUPPLY);

        // One more tick at full air must be a no-op, not an overshoot.
        let out = v.tick(false);
        assert!(out.is_empty());
        assert_eq!(v.air_supply(), MAX_AIR_SUPPLY);
    }

    /// A dead player's vitals must not keep ticking (mirrors vanilla's
    /// `isAlive()` guard): no further air loss, no further damage, even
    /// while nominally still submerged.
    #[test]
    fn dead_vitals_do_not_tick() {
        let mut v = PlayerVitals::default();
        // 10 hits of 2.0 damage empties 20.0 health exactly: the first at
        // tick 320, then every 20 ticks after (see the cadence tests above).
        for _ in 1..=320 + 9 * 20 {
            v.tick(true);
        }
        assert_eq!(v.health(), 0.0, "expected exactly 10 hits to exhaust 20.0 health");
        let out = v.tick(true);
        assert!(out.is_empty(), "a dead player's vitals must not still tick: {out:?}");
        assert_eq!(v.health(), 0.0);
    }
}
