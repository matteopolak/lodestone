//! Server-authoritative fall-distance tracking and fall damage (issue #265).
//!
//! # Where the truth comes from
//!
//! `Entity.checkFallDamage` (`.cache/mc/26.2/src/net/minecraft/world/entity/
//! Entity.java:1564-1582`), called every physics tick from `Entity.move`
//! (`Entity.java:784`, `this.checkFallDamage(movement.y, this.onGround(), ...)`):
//!
//! ```java
//! protected void checkFallDamage(final double ya, final boolean onGround, final BlockState onState, final BlockPos pos) {
//!    if (!this.isInWater() && ya < 0.0) {
//!       this.fallDistance -= (float)ya;
//!    }
//!    if (onGround) {
//!       if (this.fallDistance > 0.0) {
//!          onState.getBlock().fallOn(this.level(), onState, pos, this, this.fallDistance);
//!          ...
//!       }
//!       this.resetFallDistance();
//!    }
//! }
//! ```
//!
//! `Block.fallOn`'s default (`.cache/mc/26.2/src/net/minecraft/world/level/
//! block/Block.java:489-492`) is what turns a landing into damage:
//!
//! ```java
//! public void fallOn(final Level level, final BlockState state, final BlockPos pos, final Entity entity, final double fallDistance) {
//!    entity.causeFallDamage(fallDistance, 1.0F, entity.damageSources().fall());
//! }
//! ```
//!
//! and `LivingEntity.calculateFallDamage`/`calculateFallPower`
//! (`.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java:1846-1857`):
//!
//! ```java
//! protected int calculateFallDamage(final double fallDistance, final float damageModifier) {
//!    if (this.is(EntityTypeTags.FALL_DAMAGE_IMMUNE)) { return 0; }
//!    double baseDamage = this.calculateFallPower(fallDistance);
//!    return Mth.floor(baseDamage * damageModifier * this.getAttributeValue(Attributes.FALL_DAMAGE_MULTIPLIER));
//! }
//! private double calculateFallPower(final double fallDistance) {
//!    return fallDistance + 1.0E-6 - this.getAttributeValue(Attributes.SAFE_FALL_DISTANCE);
//! }
//! ```
//! (`calculateFallDamage`'s caller, `causeFallDamage` at `LivingEntity.java:
//! 1787-1815`, only actually hurts the entity when the resulting `dmg > 0`.)
//!
//! So the formula this module reproduces is:
//! `floor((fall_distance + 1e-6 - safe_fall_distance) * block_modifier * fall_damage_multiplier)`,
//! applied only when positive. `SAFE_FALL_DISTANCE` (default `3.0`) replaces
//! the classic "no damage below 3 blocks, ~1 damage per block after that"
//! folk description with the exact vanilla constant; `FALL_DAMAGE_MULTIPLIER`
//! (default `1.0`) is *not* a fixed 1.0 in vanilla — horses/foxes/etc.
//! override it via a different attribute base (`AbstractHorse.java:384-385`
//! registers `0.5`), but this crate has no per-species player-equivalent, so
//! [`FALL_DAMAGE_MULTIPLIER`] is the human-player default and the only value
//! in force here.
//!
//! # Why this lives here and not in `lodestone-physics`
//!
//! `lodestone-physics::player::PlayerState` already accumulates
//! `fall_distance` tick-by-tick for the *client's own* movement prediction
//! (see `accumulate_fall_distance`), but this crate's integrated server does
//! not run that simulation for the connected human player at all — movement
//! is client-authoritative here (`ServerBound::PlayerMoved` is just the
//! client's own reported position, tracked only for view-streaming and the
//! drowning submersion test before this issue). So there is no shared
//! `PlayerState` to read `fall_distance` off of server-side; this module is a
//! second, independent implementation of the same vanilla algorithm, driven
//! by the position samples that already arrive over the wire rather than by
//! a physics tick this crate does not run. It mirrors
//! [`crate::vitals::PlayerVitals`]'s shape deliberately: a plain value type
//! with a pure step function, fed by the caller with just the world-derived
//! bit (`on_ground`) it cannot derive on its own.
//!
//! # What is deliberately not modelled, and why
//!
//! * **Per-block `fallOn` overrides.** Hay (`HayBlock.java:24-27`, `0.2F`),
//!   slime (`SlimeBlock.java:22-26`, `0.0F` unless sneaking suppresses the
//!   bounce), honey (`HoneyBlock.java:58`, `0.2F`), and pointed dripstone
//!   (`PointedDripstoneBlock.java:66`, adds `+2.5` fall distance and uses
//!   `2.0F`) all change the block-damage modifier this module hardcodes at
//!   [`BLOCK_DAMAGE_MODIFIER`] `1.0`. This crate has no "what block is the
//!   player standing on" lookup at the fall-damage call site (only a
//!   `ChunkSource` reachable from `crate::server`, not threaded here) — every
//!   landing is treated as landing on a plain block.
//! * **`FALL_DAMAGE_IMMUNE` entities, elytra-glide grace
//!   (`isIgnoringFallDamageFromCurrentImpulse`), and Jump Boost's fall-damage
//!   reduction** (`LivingEntity.java:1787-1815`'s omitted middle). No elytra,
//!   no potion effects, no entity-type tags are modelled for the connected
//!   player anywhere in this crate yet.
//! * **Feather Falling and Resistance.** [`crate::vitals::PlayerVitals::
//!   apply_fall_damage`] routes the raw value through
//!   `lodestone_entity::apply_reductions` with `Defenses::default()` — armour
//!   is correctly bypassed (fall is tagged `bypasses_armor` in
//!   `.cache/mc/26.2/src/data/minecraft/tags/damage_type/bypasses_armor.json`),
//!   but `enchant_protection`/`resistance_amplifier` are `0.0`/`None` because
//!   nothing in `lodestone-server` tracks equipped items or potion effects
//!   for the player yet (the same gap `crate::vitals`'s own module doc
//!   already carries for drowning). The pipeline is real and will pick up
//!   Feather Falling for free the day boot enchantments are tracked — see
//!   `damage_after_protection`'s doc comment.
//! * **A landing reported only via `move_player_rot`/`move_player_status_only`.**
//!   `ServerBound::PlayerMoved` is only produced for the two movement
//!   packets that carry a position (`crates/lodestone-server/src/
//!   protocol.rs`'s own doc comment) — a landing tick where `y` happens not
//!   to change in that sample (rare: vanilla clients still send position
//!   packets while `y` is changing right up to the landing frame) would slip
//!   through unobserved. Not fixed here; the existing position-only filter
//!   predates this issue and widening it is a separate, larger change to
//!   `ServerBound`'s decode surface.
//! * **Teleport/respawn resets.** Vanilla resets `fallDistance` on every
//!   position snap (`Entity.java:2897`, `:2946`). This crate's server never
//!   sends a corrective teleport to the player today (movement is client-
//!   authoritative, unvalidated), so [`FallTracker::reset`] exists for a
//!   future caller but nothing calls it yet.

/// Vanilla's `Attributes.FALL_DAMAGE_MULTIPLIER` default
/// (`crates/lodestone-entity/src/attribute.rs:339`, itself sourced from
/// `Attributes.java`'s registration). No effect/attribute-modifier system
/// changes this for the connected player today, so the registered default is
/// the value in force — see this module's doc comment for species that
/// vanilla itself overrides it for.
pub const FALL_DAMAGE_MULTIPLIER: f32 = 1.0;

/// Vanilla's `Attributes.SAFE_FALL_DISTANCE` default
/// (`crates/lodestone-entity/src/attribute.rs:354`).
pub const SAFE_FALL_DISTANCE: f32 = 3.0;

/// `Block.fallOn`'s default `damageModifier` parameter (`.cache/mc/26.2/src/
/// net/minecraft/world/level/block/Block.java:489-492`, the `1.0F` passed by
/// `causeFallDamage`'s caller). See this module's doc comment for the
/// per-block overrides this crate does not look up.
const BLOCK_DAMAGE_MODIFIER: f32 = 1.0;

/// Server-side fall-distance accumulator for one connected player, fed by
/// consecutive `(y, on_ground)` samples off `ServerBound::PlayerMoved`.
///
/// Mirrors `Entity.checkFallDamage`'s two-part shape: `ya < 0.0` (a downward
/// step) adds to the running distance, and landing (`on_ground`) both
/// computes damage from whatever distance is outstanding and resets it —
/// unconditionally, whether or not it produced positive damage, exactly like
/// vanilla's `resetFallDistance()` sitting outside the `dmg > 0` check.
#[derive(Debug, Clone, Copy, Default)]
pub struct FallTracker {
    fall_distance: f64,
    last_y: Option<f64>,
}

impl FallTracker {
    /// The fall distance accumulated so far this fall (`0.0` once grounded).
    #[must_use]
    pub fn fall_distance(&self) -> f64 {
        self.fall_distance
    }

    /// Feeds one more `(y, on_ground)` sample and returns the vanilla fall
    /// damage (in whole damage points, i.e. half-hearts) if this sample was a
    /// landing with positive residual fall distance.
    ///
    /// The very first call has no previous `y` to diff against, so it only
    /// primes [`last_y`](Self) and never reports damage — matching a freshly
    /// joined connection having no fall in progress yet, not vanilla
    /// behaviour for an existing entity (which always has a previous `y`).
    pub fn on_player_moved(&mut self, y: f64, on_ground: bool) -> Option<i32> {
        if let Some(last_y) = self.last_y {
            let ya = y - last_y;
            if ya < 0.0 {
                self.fall_distance -= ya;
            }
        }
        self.last_y = Some(y);

        let mut damage = None;
        if on_ground {
            if self.fall_distance > 0.0 {
                let base_damage =
                    self.fall_distance + 1.0e-6 - f64::from(SAFE_FALL_DISTANCE);
                let dmg = (base_damage
                    * f64::from(BLOCK_DAMAGE_MODIFIER)
                    * f64::from(FALL_DAMAGE_MULTIPLIER))
                .floor() as i32;
                if dmg > 0 {
                    damage = Some(dmg);
                }
            }
            self.fall_distance = 0.0;
        }
        damage
    }

    /// Resets accumulated fall distance outside of landing (a corrective
    /// teleport or respawn — see this module's doc comment for why nothing
    /// calls this yet).
    pub fn reset(&mut self) {
        self.fall_distance = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A player standing still, never falling, must never report damage —
    /// the negative control proving a flat `y` alone cannot trigger the
    /// detector.
    #[test]
    fn standing_still_never_reports_damage() {
        let mut f = FallTracker::default();
        for _ in 0..20 {
            let out = f.on_player_moved(64.0, true);
            assert_eq!(out, None);
        }
        assert_eq!(f.fall_distance(), 0.0);
    }

    /// A fall of exactly the safe distance (3 blocks) must deal zero damage —
    /// `floor(3.000001 - 3.0) = 0`, not rounded up to 1.
    #[test]
    fn a_three_block_fall_deals_no_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(67.0, false); // airborne, primes last_y
        let out = f.on_player_moved(64.0, true); // 3 blocks down, lands
        assert_eq!(out, None, "exactly the safe distance must not hurt");
    }

    /// A fall of 4 blocks (1 past the safe distance) deals exactly 1 damage
    /// point (half a heart) — the classic "1 per block over 3" folk number,
    /// derived here from the exact vanilla formula rather than assumed.
    #[test]
    fn a_four_block_fall_deals_one_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(67.0, false);
        let out = f.on_player_moved(63.0, true); // 4 blocks down
        assert_eq!(out, Some(1));
    }

    /// A 10-block fall: `floor(10.000001 - 3.0) = 7` damage points (3.5
    /// hearts). This is the **magnitude** check (CLAUDE.md's vacuous-test
    /// species) — a linear-but-wrong formula (e.g. no `+1e-6`/no floor, or a
    /// flat `fall_distance - 3` without flooring at a block boundary) would
    /// pass a "damage increases with fall distance" sign check but land on
    /// the wrong number; this asserts the exact value.
    #[test]
    fn a_ten_block_fall_deals_seven_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(74.0, false);
        let out = f.on_player_moved(64.0, true);
        assert_eq!(out, Some(7));
    }

    /// Multiple downward ticks before landing must accumulate, not just
    /// measure the last step — a wrong implementation that only looked at the
    /// final `ya` would see a small partial-tick delta and wrongly report no
    /// damage.
    #[test]
    fn fall_distance_accumulates_across_several_ticks_before_landing() {
        let mut f = FallTracker::default();
        f.on_player_moved(74.0, false);
        f.on_player_moved(71.0, false); // -3, airborne
        f.on_player_moved(68.0, false); // -3, airborne
        assert_eq!(f.fall_distance(), 6.0, "6 blocks accumulated so far");
        let out = f.on_player_moved(64.0, true); // final -4, lands: 10 total
        assert_eq!(out, Some(7), "same 10-block result as the single-step case");
    }

    /// Landing resets fall distance to zero even when it produced no damage
    /// (a sub-safe-distance fall) — matching vanilla's unconditional
    /// `resetFallDistance()` outside the `dmg > 0` gate. This is the control
    /// that proves the reset is unconditional: a wrong implementation that
    /// only reset on `dmg > 0` would leave residual distance here.
    #[test]
    fn landing_resets_fall_distance_even_with_no_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(65.0, false);
        f.on_player_moved(64.0, true); // 1 block, lands, no damage
        assert_eq!(f.fall_distance(), 0.0);

        // A second, separate fall now starts fresh rather than continuing
        // from stale state. `64.0 -> 64.0` (airborne, no net movement yet)
        // primes a new reference point with zero carried-over distance
        // before the real 10-block drop.
        f.on_player_moved(64.0, false);
        let out = f.on_player_moved(54.0, true); // 10 blocks, fresh fall
        assert_eq!(out, Some(7));
    }

    /// Moving *upward* (jumping, an elevator, flight) must never add to fall
    /// distance — only `ya < 0.0` steps do, exactly like vanilla's guard.
    #[test]
    fn rising_never_accumulates_fall_distance() {
        let mut f = FallTracker::default();
        f.on_player_moved(60.0, false);
        f.on_player_moved(65.0, false); // +5, rising
        assert_eq!(f.fall_distance(), 0.0);
        let out = f.on_player_moved(65.0, true);
        assert_eq!(out, None);
    }

    /// **Control**: the very first sample (no previous `y`) must never report
    /// damage even if `on_ground` is `true` and even at an arbitrary starting
    /// height — there is no fall in progress yet for a freshly connected
    /// player, and this proves the "no previous sample" branch is not
    /// silently treating `last_y` as `0.0` (which would fabricate a fall from
    /// spawn height).
    #[test]
    fn first_sample_ever_reports_no_damage_regardless_of_height() {
        let mut f = FallTracker::default();
        let out = f.on_player_moved(200.0, true);
        assert_eq!(out, None, "no reference point yet");
        assert_eq!(f.fall_distance(), 0.0);
    }

    /// A fall in water (`on_ground` never observed true while submerged
    /// descent continues, matching vanilla's `!isInWater()` guard being the
    /// *caller's* job, not this tracker's) is exercised indirectly: this
    /// tracker only ever sees `on_ground` transitions the caller reports, so
    /// a caller that correctly withholds fall-distance accumulation for
    /// underwater ticks (as `crate::server`'s wiring does, matching
    /// `crate::vitals`'s existing eye-in-water test) needs no special case
    /// here. This test instead pins the plain boundary: landing exactly at
    /// the safe distance from a *very* large prior height must still only
    /// measure the *last* uninterrupted fall, once reset by an intermediate
    /// landing.
    #[test]
    fn an_intermediate_landing_caps_what_the_next_fall_measures() {
        let mut f = FallTracker::default();
        f.on_player_moved(100.0, false);
        f.on_player_moved(80.0, true); // lands after 20 blocks: big hit
        let first = f.fall_distance();
        assert_eq!(first, 0.0, "reset by the landing");

        f.on_player_moved(78.0, false); // falls only 2 more blocks after
        let out = f.on_player_moved(78.0, true);
        assert_eq!(out, None, "only the 2-block second fall should count");
    }
}
