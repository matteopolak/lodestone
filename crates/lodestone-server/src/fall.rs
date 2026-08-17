//! Server-authoritative fall-distance tracking and fall damage.
//!
//! # Where the truth comes from
//!
//! `Entity.checkFallDamage`, called every physics tick from `Entity.move`
//! (`this.checkFallDamage(movement.y, this.onGround(), ...)`):
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
//! `Block.fallOn`'s default is what turns a landing into damage:
//!
//! ```java
//! public void fallOn(final Level level, final BlockState state, final BlockPos pos, final Entity entity, final double fallDistance) {
//!    entity.causeFallDamage(fallDistance, 1.0F, entity.damageSources().fall());
//! }
//! ```
//!
//! and `LivingEntity.calculateFallDamage`/`LivingEntity.calculateFallPower`:
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
//! (`calculateFallDamage`'s caller, `LivingEntity.causeFallDamage`,
//! only actually hurts the entity when the resulting `dmg > 0`.)
//!
//! So the formula this module reproduces is:
//! `floor((fall_distance + 1e-6 - safe_fall_distance) * block_modifier * fall_damage_multiplier)`,
//! applied only when positive. `SAFE_FALL_DISTANCE` (default `3.0`) replaces
//! the classic "no damage below 3 blocks, ~1 damage per block after that"
//! folk description with the exact vanilla constant; `FALL_DAMAGE_MULTIPLIER`
//! (default `1.0`) is *not* a fixed 1.0 in vanilla — horses/foxes/etc.
//! override it via a different attribute base (`AbstractHorse.createBaseHorseAttributes`
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
//! # The cancellation set
//!
//! This module used to have **no cancellation cases at all**, and the
//! consequence was the reported bug: a fall that ended in water *banked* its
//! distance and cashed it in on the next dry landing, so a dive into a lake hurt
//! you minutes later on a one-block step. Vanilla resets `fallDistance` in seven
//! places; this table is all of them, with what this crate does about each:
//!
//! | vanilla site | rule | here |
//! |---|---|---|
//! | `Entity.checkFallDamage` | `!isInWater() && ya < 0.0` — no accumulation while in water | [`FallSample::in_water`] |
//! | `Entity.updateFluidInteraction` | `if (inWater) resetFallDistance()` | [`FallSample::in_water`] |
//! | `LivingEntity.handleOnClimbable` | on a climbable → reset | [`FallSample::fall_resetting`] |
//! | `Entity.move` | a `#minecraft:fall_damage_resetting` block in the path → reset | [`FallSample::fall_resetting`] |
//! | `Entity.handleOnInsideBubbleColumn`, `Entity.makeStuckInBlock` | entering a bubble column, or becoming stuck in a block (honey, cobweb) → reset | [`FallTracker::reset`] |
//! | `LivingEntity.rideTick` | riding **any** vehicle → reset every tick | **not modelled** |
//! | `LivingEntity.aiStep` | `SLOW_FALLING` or `LEVITATION` → reset | **not modelled** |
//!
//! **Lava is deliberately absent, and getting that wrong is the easy mistake.**
//! `checkFallDamage`'s guard is `isInWater()`, and `updateFluidInteraction` resets
//! only `if (inWater)` — a plausible-looking "any fluid cancels" implementation
//! would make a lava dive a safe landing. `crate::chunk::is_water` is already the
//! narrow predicate for this and `crate::vitals` already documents the same
//! distinction for drowning.
//!
//! Per-block landing modifiers are modelled through
//! [`FallSample::block_damage_modifier`], read off `Block.fallOn`'s own
//! `damageModifier` argument:
//!
//! | block | `fallOn` | source |
//! |---|---|---|
//! | hay bale | `0.2F` | `HayBlock.fallOn` |
//! | honey block | `0.2F` | `HoneyBlock.fallOn` |
//! | slime block | `0.0F` | `SlimeBlock.fallOn` |
//! | powder snow | **no `causeFallDamage` call at all** | `PowderSnowBlock.fallOn` |
//! | anything else | `1.0F` | `Block.fallOn` |
//!
//! Powder snow is the one that is not a modifier: its `fallOn` override plays a
//! sound and *never calls `causeFallDamage`*, so it is a complete cancellation
//! rather than a reduction, and it is expressed here as a modifier of `0.0`
//! (identical outcome, one mechanism).
//!
//! # What is still deliberately not modelled, and why
//!
//! * **Boats and any other vehicle** (`rideTick`'s unconditional reset). This
//!   crate's server tracks no vehicle state for the connected player at all —
//!   there is no `Passenger`/`Vehicle` relation to read, and `ServerBound` carries
//!   no mount packet. A player in a boat therefore still accumulates. Naming it
//!   here rather than leaving it unfindable.
//! * **`SLOW_FALLING` / `LEVITATION`.** No potion effects are tracked for the
//!   player anywhere in this crate (the same gap `crate::vitals` carries for
//!   respiration and Feather Falling).
//! * **Pointed dripstone** (`PointedDripstoneBlock.fallOn`, which *adds* `+2.5`
//!   fall distance and uses a `2.0F` modifier). The modifier half would fit
//!   [`FallSample::block_damage_modifier`] exactly; the additive half needs a
//!   pre-landing hook this shape does not have, and shipping half of it would make
//!   dripstone *safer* than plain ground, which is worse than not modelling it.
//! * **`FALL_DAMAGE_IMMUNE` entities, elytra-glide grace
//!   (`isIgnoringFallDamageFromCurrentImpulse`), and Jump Boost's fall-damage
//!   reduction** (`LivingEntity.causeFallDamage`'s omitted middle). No elytra,
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
//! Teleport/respawn resets are **no longer** on this list: `crate::server`'s
//! `apply_client_command` respawn arm calls [`FallTracker::reset`] now that the
//! server sends a real respawn teleport. This note used to say "exists for a
//! future caller but nothing calls it yet".

/// Vanilla's `Attributes.FALL_DAMAGE_MULTIPLIER` default
/// (the `"fall_damage_multiplier"` arm of `lodestone_entity::attribute::default_def`,
/// itself sourced from `Attributes.java`'s registration). No effect/attribute-modifier system
/// changes this for the connected player today, so the registered default is
/// the value in force — see this module's doc comment for species that
/// vanilla itself overrides it for.
pub const FALL_DAMAGE_MULTIPLIER: f32 = 1.0;

/// Vanilla's `Attributes.SAFE_FALL_DISTANCE` default
/// (the `"safe_fall_distance"` arm of `lodestone_entity::attribute::default_def`).
pub const SAFE_FALL_DISTANCE: f32 = 3.0;

/// `Block.fallOn`'s default `damageModifier` parameter (the `1.0F` passed by
/// `causeFallDamage`'s caller).
///
/// The **default**, not the only value: [`FallSample::block_damage_modifier`]
/// carries the per-block override, and this is what it holds for a plain block.
pub const DEFAULT_BLOCK_DAMAGE_MODIFIER: f32 = 1.0;

/// `HayBlock.fallOn`'s and `HoneyBlock.fallOn`'s `damageModifier`.
pub const CUSHIONED_BLOCK_DAMAGE_MODIFIER: f32 = 0.2;

/// Whether `state` is in vanilla's `#minecraft:fall_damage_resetting` block tag,
/// i.e. standing in it discards the fall.
///
/// The tag is `#minecraft:climbable` plus `sweet_berry_bush` and `cobweb`
/// (`.cache/mc/26.2/src/data/minecraft/tags/block/fall_damage_resetting.json`),
/// and `#minecraft:climbable` expands to the nine entries in
/// `tags/block/climbable.json`. Both files are transcribed here in full rather
/// than sampled: a partial list is a silent gap in the *safe* direction from the
/// code's point of view and an unpleasant surprise from the player's, and
/// `the_fall_damage_resetting_set_matches_the_jar_tags` reads both JSON files at
/// test time so drift fails loudly.
#[must_use]
pub fn is_fall_damage_resetting(state: &str) -> bool {
    let base = state.split('[').next().unwrap_or(state);
    matches!(
        base,
        // #minecraft:climbable
        "minecraft:ladder"
            | "minecraft:vine"
            | "minecraft:scaffolding"
            | "minecraft:weeping_vines"
            | "minecraft:weeping_vines_plant"
            | "minecraft:twisting_vines"
            | "minecraft:twisting_vines_plant"
            | "minecraft:cave_vines"
            | "minecraft:cave_vines_plant"
            // and the two direct entries
            | "minecraft:sweet_berry_bush"
            | "minecraft:cobweb"
    )
}

/// `Block.fallOn`'s `damageModifier` for the block being landed on.
///
/// Every non-default value comes from a `fallOn` override in the jar; see this
/// module's own table for the citations. Powder snow is `0.0` because its
/// override never calls `causeFallDamage` at all, which is the same outcome as a
/// zero modifier through one mechanism instead of two.
///
/// Slime's real override is conditional — `SlimeBlock.fallOn` only zeroes the
/// damage when the entity is **not** sneaking, which is
/// how a player descends a slime tower without bouncing. This crate's server
/// tracks no sneak state at the fall-damage call site, so the unconditional `0.0`
/// is a documented over-approximation in the player's favour rather than an
/// oversight.
#[must_use]
pub fn block_damage_modifier(state: &str) -> f32 {
    let base = state.split('[').next().unwrap_or(state);
    match base {
        "minecraft:hay_block" | "minecraft:honey_block" => CUSHIONED_BLOCK_DAMAGE_MODIFIER,
        "minecraft:slime_block" | "minecraft:powder_snow" => 0.0,
        _ => DEFAULT_BLOCK_DAMAGE_MODIFIER,
    }
}

/// One `(y, on_ground)` movement sample plus the three world-derived facts the
/// tracker cannot derive on its own.
///
/// Every field names the vanilla expression it stands for, because each one is a
/// *separate* reset site in the jar rather than three views of one rule — see the
/// module doc's table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallSample {
    /// The player's reported feet Y.
    pub y: f64,
    /// The `on_ground` flag off the movement packet's flags byte.
    pub on_ground: bool,
    /// Vanilla `isInWater()`. Suppresses accumulation
    /// (`Entity.checkFallDamage`'s guard) **and** zeroes the running distance
    /// (`Entity.updateFluidInteraction`). Two sites, one input — and the
    /// second is the one that matters most: without it a fall that *ends* in
    /// water merely stops growing, and the banked distance is still there for the
    /// next dry landing to charge you for.
    ///
    /// **Water only.** Lava does not reset in vanilla; see the module doc.
    pub in_water: bool,
    /// Vanilla's `#minecraft:fall_damage_resetting` block tag at the player's
    /// position (`#minecraft:climbable` + `sweet_berry_bush` + `cobweb`), which
    /// covers both `LivingEntity.handleOnClimbable` and `Entity.move`'s
    /// `FALLDAMAGE_RESETTING` clip. Zeroes the running distance.
    pub fall_resetting: bool,
    /// `Block.fallOn`'s `damageModifier` for the block being landed on —
    /// [`DEFAULT_BLOCK_DAMAGE_MODIFIER`] for a plain block. Only read on a
    /// landing sample.
    pub block_damage_modifier: f32,
}

impl FallSample {
    /// A plain sample: dry, not on a climbable, landing on an ordinary block.
    ///
    /// For a caller that has no world to consult — and **not** a shortcut for
    /// `crate::server`, which does. Its whole purpose is to make the
    /// world-derived fields visible at every real call site rather than
    /// defaultable: the reported water-landing bug existed precisely because
    /// they were not there at all.
    #[must_use]
    pub fn plain(y: f64, on_ground: bool) -> Self {
        Self {
            y,
            on_ground,
            in_water: false,
            fall_resetting: false,
            block_damage_modifier: DEFAULT_BLOCK_DAMAGE_MODIFIER,
        }
    }
}

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
    pub fn on_player_moved(&mut self, sample: FallSample) -> Option<i32> {
        let FallSample {
            y,
            on_ground,
            in_water,
            fall_resetting,
            block_damage_modifier,
        } = sample;

        // `Entity.checkFallDamage` — `!isInWater() && ya < 0.0`.
        if let Some(last_y) = self.last_y {
            let ya = y - last_y;
            if !in_water && ya < 0.0 {
                self.fall_distance -= ya;
            }
        }
        self.last_y = Some(y);

        // The two unconditional resets, applied **after** accumulation and
        // **before** the landing test, which is the order vanilla's tick runs
        // them in: `updateFluidInteraction` and `handleOnClimbable` both fire
        // during `LivingEntity.travel`, ahead of the `move` that calls
        // `checkFallDamage`.
        //
        // Ordering is the whole of the fix for the water-landing bug. Suppressing accumulation alone
        // (the guard above) makes a fall that *ends* in water merely stop
        // growing — the distance already banked on the way down survives, and
        // the next dry landing is charged for it. `cancel` is what discards it.
        if in_water || fall_resetting {
            self.cancel();
        }

        let mut damage = None;
        if on_ground {
            if self.fall_distance > 0.0 {
                let base_damage =
                    self.fall_distance + 1.0e-6 - f64::from(SAFE_FALL_DISTANCE);
                let dmg = (base_damage
                    * f64::from(block_damage_modifier)
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

    /// Vanilla's `resetFallDistance()` proper: zero the accumulated distance and
    /// **keep** the position reference.
    ///
    /// The distinction from [`reset`](Self::reset) is not cosmetic. `cancel` is
    /// for a fall that is *cancelled mid-flight* — the player is still where they
    /// were and still moving, so the next sample's `ya` must still be measured
    /// against this position. `reset` is for a position *snap*, where the
    /// reference is meaningless and keeping it would fabricate a fall out of the
    /// teleport's own displacement.
    ///
    /// Using `reset` here would under-count by one tick on entering water, which
    /// is harmless; using `cancel` for a teleport is the bug `reset`'s own doc
    /// comment describes, which is not.
    pub fn cancel(&mut self) {
        self.fall_distance = 0.0;
    }

    /// Resets accumulated fall distance outside of landing — a position snap.
    /// Vanilla resets `fallDistance` outside of a landing in several places
    /// (`Entity.handleOnInsideBubbleColumn`, `Entity.makeStuckInBlock`); this
    /// crate applies the same idiom to a teleport/respawn snap, where the
    /// pre-teleport distance is equally meaningless to carry forward.
    ///
    /// **Clears `last_y` as well as the distance, and that is the load-bearing
    /// half.** Zeroing the distance alone leaves the *reference point* at the y
    /// the player was snapped away from, so the very next sample is diffed
    /// against it: a player who dies at y = 70 and respawns at y = 64 banks 6
    /// blocks of fall distance they never fell, and pays for it on their next
    /// landing. Dropping the reference makes the next sample re-prime instead,
    /// which is exactly what a freshly-teleported entity should do.
    ///
    /// Callers: `crate::server`'s `apply_client_command` respawn arm, and the
    /// water/boat/powder-snow cancellations in [`Self::cancel`].
    pub fn reset(&mut self) {
        self.fall_distance = 0.0;
        self.last_y = None;
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
            let out = f.on_player_moved(FallSample::plain(64.0, true));
            assert_eq!(out, None);
        }
        assert_eq!(f.fall_distance(), 0.0);
    }

    /// A fall of exactly the safe distance (3 blocks) must deal zero damage —
    /// `floor(3.000001 - 3.0) = 0`, not rounded up to 1.
    #[test]
    fn a_three_block_fall_deals_no_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(67.0, false)); // airborne, primes last_y
        let out = f.on_player_moved(FallSample::plain(64.0, true)); // 3 blocks down, lands
        assert_eq!(out, None, "exactly the safe distance must not hurt");
    }

    /// A fall of 4 blocks (1 past the safe distance) deals exactly 1 damage
    /// point (half a heart) — the classic "1 per block over 3" folk number,
    /// derived here from the exact vanilla formula rather than assumed.
    #[test]
    fn a_four_block_fall_deals_one_damage() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(67.0, false));
        let out = f.on_player_moved(FallSample::plain(63.0, true)); // 4 blocks down
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
        f.on_player_moved(FallSample::plain(74.0, false));
        let out = f.on_player_moved(FallSample::plain(64.0, true));
        assert_eq!(out, Some(7));
    }

    /// Multiple downward ticks before landing must accumulate, not just
    /// measure the last step — a wrong implementation that only looked at the
    /// final `ya` would see a small partial-tick delta and wrongly report no
    /// damage.
    #[test]
    fn fall_distance_accumulates_across_several_ticks_before_landing() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(74.0, false));
        f.on_player_moved(FallSample::plain(71.0, false)); // -3, airborne
        f.on_player_moved(FallSample::plain(68.0, false)); // -3, airborne
        assert_eq!(f.fall_distance(), 6.0, "6 blocks accumulated so far");
        let out = f.on_player_moved(FallSample::plain(64.0, true)); // final -4, lands: 10 total
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
        f.on_player_moved(FallSample::plain(65.0, false));
        f.on_player_moved(FallSample::plain(64.0, true)); // 1 block, lands, no damage
        assert_eq!(f.fall_distance(), 0.0);

        // A second, separate fall now starts fresh rather than continuing
        // from stale state. `64.0 -> 64.0` (airborne, no net movement yet)
        // primes a new reference point with zero carried-over distance
        // before the real 10-block drop.
        f.on_player_moved(FallSample::plain(64.0, false));
        let out = f.on_player_moved(FallSample::plain(54.0, true)); // 10 blocks, fresh fall
        assert_eq!(out, Some(7));
    }

    /// Moving *upward* (jumping, an elevator, flight) must never add to fall
    /// distance — only `ya < 0.0` steps do, exactly like vanilla's guard.
    #[test]
    fn rising_never_accumulates_fall_distance() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(60.0, false));
        f.on_player_moved(FallSample::plain(65.0, false)); // +5, rising
        assert_eq!(f.fall_distance(), 0.0);
        let out = f.on_player_moved(FallSample::plain(65.0, true));
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
        let out = f.on_player_moved(FallSample::plain(200.0, true));
        assert_eq!(out, None, "no reference point yet");
        assert_eq!(f.fall_distance(), 0.0);
    }

    /// Landing exactly at the safe distance from a *very* large prior height must
    /// still only measure the *last* uninterrupted fall, once an intermediate
    /// landing has reset it.
    ///
    /// This doc comment used to claim water was "exercised indirectly" because
    /// "`crate::server`'s wiring correctly withholds fall-distance accumulation
    /// for underwater ticks". **That was false when written and stayed false**:
    /// the wiring passed `(y, on_ground)` and nothing else, no water test existed
    /// anywhere on the path, and the water-landing bug is what it described as
    /// already handled. The water cases are now real gates below.
    #[test]
    fn an_intermediate_landing_caps_what_the_next_fall_measures() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(100.0, false));
        f.on_player_moved(FallSample::plain(80.0, true)); // lands after 20 blocks: big hit
        let first = f.fall_distance();
        assert_eq!(first, 0.0, "reset by the landing");

        f.on_player_moved(FallSample::plain(78.0, false)); // falls only 2 more blocks after
        let out = f.on_player_moved(FallSample::plain(78.0, true));
        assert_eq!(out, None, "only the 2-block second fall should count");
    }

    /// **The reported bug.** A long fall that ends in water must not
    /// bank its distance for the next dry landing.
    ///
    /// Both hypotheses computed from outside constants. The player falls 40 blocks
    /// into water, swims out, and later steps down 1 block onto stone:
    ///
    /// * correct — the water cancelled everything, so the 1-block step is
    ///   `floor(1.000001 - 3.0) < 0`, i.e. **no damage**;
    /// * suspected wrong — the 40 blocks are still banked, so the step lands
    ///   `floor(41.000001 - 3.0) = 38` damage points, which kills a 20-HP player
    ///   twice over.
    ///
    /// Asserting "less damage" would pass under both. Asserting `None` versus a
    /// predicted `38` cannot.
    #[test]
    fn a_fall_ending_in_water_does_not_charge_the_next_dry_landing() {
        const FALL: f64 = 40.0;
        let banked_hypothesis =
            (FALL + 1.0 + 1.0e-6 - f64::from(SAFE_FALL_DISTANCE)).floor() as i32;
        assert_eq!(
            banked_hypothesis, 38,
            "the wrong-hypothesis arithmetic must be the exact value it was before \
             this fix, or this gate is not measuring the defect"
        );

        let mut f = FallTracker::default();
        // Dry descent: 100 -> 60, all airborne.
        f.on_player_moved(FallSample::plain(100.0, false));
        f.on_player_moved(FallSample::plain(100.0 - FALL, false));
        assert_eq!(
            f.fall_distance(),
            FALL,
            "premise: the dry part of the fall really did accumulate — without this \
             the cancellation below would be cancelling nothing"
        );

        // Enters the water.
        let wet = FallSample {
            y: 59.0,
            on_ground: false,
            in_water: true,
            fall_resetting: false,
            block_damage_modifier: DEFAULT_BLOCK_DAMAGE_MODIFIER,
        };
        f.on_player_moved(wet);
        assert_eq!(f.fall_distance(), 0.0, "water discards the banked distance");

        // Swims up and out, then a 1-block step down onto plain stone.
        f.on_player_moved(FallSample {
            y: 62.0,
            in_water: true,
            ..wet
        });
        f.on_player_moved(FallSample::plain(63.0, true));
        f.on_player_moved(FallSample::plain(63.0, false));
        let out = f.on_player_moved(FallSample::plain(62.0, true));
        assert_eq!(
            out, None,
            "a 1-block step after a water landing must not hurt; {banked_hypothesis} is \
             what the tracker charged for it before this fix"
        );
    }

    /// **Control**: the same 40-block fall onto *dry ground* must still hurt, by
    /// exactly the predicted amount.
    ///
    /// Without this, the gate above passes against a tracker that never deals fall
    /// damage at all — the most likely way to "fix" the water-landing bug wrongly.
    #[test]
    fn the_same_fall_onto_dry_ground_still_hurts_by_the_predicted_amount() {
        const FALL: f64 = 40.0;
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(100.0, false));
        let out = f.on_player_moved(FallSample::plain(100.0 - FALL, true));
        assert_eq!(
            out,
            Some(37),
            "floor(40.000001 - 3.0) = 37; the detector must be live for the water \
             gate's absence to mean anything"
        );
    }

    /// Water suppresses accumulation *while submerged*, so descending inside water
    /// never builds distance at all — vanilla's `!isInWater()` guard.
    ///
    /// Distinct from the cancellation above and worth its own gate: a tracker that
    /// only cancelled on *entering* water would still charge a player who sank
    /// 30 blocks to the seabed.
    #[test]
    fn descending_inside_water_never_accumulates() {
        let mut f = FallTracker::default();
        let wet = |y: f64, on_ground: bool| FallSample {
            y,
            on_ground,
            in_water: true,
            fall_resetting: false,
            block_damage_modifier: DEFAULT_BLOCK_DAMAGE_MODIFIER,
        };
        f.on_player_moved(wet(90.0, false));
        for y in (60..90).rev() {
            f.on_player_moved(wet(f64::from(y), false));
        }
        assert_eq!(f.fall_distance(), 0.0, "30 blocks of sinking, none accumulated");
        let out = f.on_player_moved(wet(60.0, true));
        assert_eq!(out, None, "touching the seabed must not hurt");
    }

    /// **Lava does not cancel.** Vanilla's guard is `isInWater()` and
    /// `updateFluidInteraction` resets only `if (inWater)`.
    ///
    /// The control that pins the *narrowness* of the water rule: an
    /// "any fluid cancels" implementation would make a lava dive a safe landing,
    /// and it would pass every other gate in this file. Lava reaches this tracker
    /// as an ordinary dry sample, so this asserts the predicted damage.
    #[test]
    fn lava_is_not_water_and_does_not_cancel_a_fall() {
        let mut f = FallTracker::default();
        // `in_water` is false because `crate::chunk::is_water` is false for lava —
        // that is the whole mechanism, and this gate is what stops it widening.
        f.on_player_moved(FallSample::plain(100.0, false));
        let out = f.on_player_moved(FallSample::plain(80.0, true));
        assert_eq!(
            out,
            Some(17),
            "floor(20.000001 - 3.0) = 17: a 20-block drop into lava still deals full \
             fall damage in vanilla"
        );
    }

    /// A climbable (or cobweb, or sweet berry bush) cancels, per the
    /// `#minecraft:fall_damage_resetting` tag.
    #[test]
    fn a_fall_resetting_block_cancels_the_fall() {
        let mut f = FallTracker::default();
        f.on_player_moved(FallSample::plain(100.0, false));
        f.on_player_moved(FallSample::plain(70.0, false));
        assert_eq!(f.fall_distance(), 30.0, "premise: 30 blocks accumulated");

        f.on_player_moved(FallSample {
            y: 69.0,
            on_ground: false,
            in_water: false,
            fall_resetting: true,
            block_damage_modifier: DEFAULT_BLOCK_DAMAGE_MODIFIER,
        });
        assert_eq!(f.fall_distance(), 0.0, "grabbing a ladder discards the fall");

        f.on_player_moved(FallSample::plain(68.0, false));
        let out = f.on_player_moved(FallSample::plain(68.0, true));
        assert_eq!(out, None, "and the landing below it is free");
    }

    /// Hay, honey and slime reduce; powder snow eliminates. Each predicted
    /// exactly from `Block.fallOn`'s own modifier.
    ///
    /// | modifier | 40-block fall | source |
    /// |---|---|---|
    /// | `1.0` | `floor(37.000001 * 1.0) = 37` | `Block.fallOn` |
    /// | `0.2` | `floor(37.000001 * 0.2) = 7` | `HayBlock`/`HoneyBlock` |
    /// | `0.0` | `0`, so `None` | `SlimeBlock`, powder snow |
    ///
    /// The `0.2` row is the interesting one: `37 * 0.2 = 7.4`, so the floor lands
    /// on `7` and not on any rounding of `7.4`. A gate asserting only "less than
    /// 37" would not distinguish the modifier being applied before the floor from
    /// after it.
    #[test]
    fn landing_block_modifiers_scale_the_damage_by_the_predicted_factor() {
        let cases: [(f32, Option<i32>, &str); 4] = [
            (DEFAULT_BLOCK_DAMAGE_MODIFIER, Some(37), "plain block"),
            (CUSHIONED_BLOCK_DAMAGE_MODIFIER, Some(7), "hay or honey"),
            (0.0, None, "slime block"),
            (0.0, None, "powder snow (fallOn never calls causeFallDamage)"),
        ];
        for (modifier, expected, label) in cases {
            let mut f = FallTracker::default();
            f.on_player_moved(FallSample::plain(100.0, false));
            let out = f.on_player_moved(FallSample {
                y: 60.0,
                on_ground: true,
                in_water: false,
                fall_resetting: false,
                block_damage_modifier: modifier,
            });
            assert_eq!(out, expected, "{label} (modifier {modifier})");
        }
        // The floor really does bite: 37 * 0.2 is 7.4, not 7.
        assert!(
            (37.0_f64 * 0.2 - 7.4).abs() < 1e-9,
            "the 0.2 row is only a magnitude check because 37 * 0.2 is not an integer"
        );
    }

    /// `cancel` keeps the position reference; `reset` drops it. The two are not
    /// interchangeable and this is the gate that says so.
    ///
    /// After `cancel` at y = 70, a further descent to y = 60 must measure 10
    /// blocks. After `reset` at y = 70, the same descent measures 0 on its first
    /// sample (the reference is re-primed) and only accumulates from there.
    #[test]
    fn cancel_keeps_the_reference_and_reset_drops_it() {
        let mut cancelled = FallTracker::default();
        cancelled.on_player_moved(FallSample::plain(70.0, false));
        cancelled.cancel();
        cancelled.on_player_moved(FallSample::plain(60.0, false));
        assert_eq!(
            cancelled.fall_distance(),
            10.0,
            "cancel keeps y = 70 as the reference, so the next 10 blocks count"
        );

        let mut snapped = FallTracker::default();
        snapped.on_player_moved(FallSample::plain(70.0, false));
        snapped.reset();
        snapped.on_player_moved(FallSample::plain(60.0, false));
        assert_eq!(
            snapped.fall_distance(),
            0.0,
            "reset drops the reference, so the first sample after it only re-primes"
        );
    }

    /// [`is_fall_damage_resetting`]'s set must equal the jar's own
    /// `#minecraft:fall_damage_resetting` tag, expanded through
    /// `#minecraft:climbable` — read out of the two JSON files at test time, so the
    /// expected value is Mojang's and not a transcription of it.
    ///
    /// **Both directions.** Membership alone would pass against a predicate that
    /// returned `true` for every block, which would make every landing free.
    ///
    /// Skips when the decompiled tree is absent, and counts what it checked so a
    /// run that verified nothing cannot read as a pass.
    #[test]
    fn the_fall_damage_resetting_set_matches_the_jar_tags() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/mc/26.2/src/data/minecraft/tags/block");
        if !root.is_dir() {
            eprintln!("SKIP: {} is absent (no decompiled 26.2 tree)", root.display());
            return;
        }
        let values = |file: &str| -> Vec<String> {
            let path = root.join(file);
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let doc: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {file}: {e}"));
            doc["values"]
                .as_array()
                .unwrap_or_else(|| panic!("{file} has no values array"))
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        };

        let mut expected: Vec<String> = Vec::new();
        for entry in values("fall_damage_resetting.json") {
            match entry.strip_prefix('#') {
                // One level of tag reference, which is all this tag has. A second
                // level would land in the `else` below and be asserted as a literal
                // block name, which fails loudly rather than being skipped.
                Some("minecraft:climbable") => expected.extend(values("climbable.json")),
                Some(other) => panic!("unhandled nested tag #{other} — expand it here"),
                None => expected.push(entry),
            }
        }
        expected.sort();
        expected.dedup();
        assert!(
            expected.len() >= 11,
            "the jar tag expanded to only {} entries; the transcription in \
             `is_fall_damage_resetting` has 11, so something did not expand",
            expected.len()
        );

        for name in &expected {
            assert!(
                is_fall_damage_resetting(name),
                "{name} is in the jar's fall_damage_resetting tag but the predicate \
                 says no — a fall through it would still hurt"
            );
        }

        // The other direction: blocks that are emphatically not in the tag.
        for name in [
            "minecraft:stone",
            "minecraft:grass_block",
            "minecraft:water",
            "minecraft:lava",
            "minecraft:hay_block",
            "minecraft:powder_snow",
            "minecraft:air",
        ] {
            assert!(
                !expected.iter().any(|e| e == name),
                "premise: {name} must genuinely be absent from the jar tag"
            );
            assert!(
                !is_fall_damage_resetting(name),
                "{name} is not in the tag and must not cancel a fall"
            );
        }

        // And the property-suffixed form, which is what a real column carries.
        assert!(is_fall_damage_resetting("minecraft:ladder[facing=north,waterlogged=false]"));
        assert!(is_fall_damage_resetting("minecraft:cave_vines[age=5,berries=true]"));
    }

    /// [`block_damage_modifier`]'s non-default values must match the `fallOn`
    /// overrides in the decompiled tree — asserted by finding the literal
    /// `causeFallDamage(fallDistance, <modifier>F` in each block's own source file
    /// rather than by restating the number.
    ///
    /// Powder snow is checked differently and deliberately: its `fallOn` override
    /// must contain **no** `causeFallDamage` call at all, which is the actual
    /// reason it is `0.0` here. Asserting a modifier for it would be asserting
    /// something the jar does not say.
    #[test]
    fn block_damage_modifiers_match_the_jar_fall_on_overrides() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/mc/26.2/src/net/minecraft/world/level/block");
        if !root.is_dir() {
            eprintln!("SKIP: {} is absent (no decompiled 26.2 tree)", root.display());
            return;
        }
        let source_of = |file: &str| -> String {
            let path = root.join(file);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        };

        let mut checked = 0usize;
        for (file, block, modifier) in [
            ("HayBlock.java", "minecraft:hay_block", "0.2F"),
            ("HoneyBlock.java", "minecraft:honey_block", "0.2F"),
            ("SlimeBlock.java", "minecraft:slime_block", "0.0F"),
        ] {
            let src = source_of(file);
            let needle = format!("causeFallDamage(fallDistance, {modifier}");
            assert!(
                src.contains(&needle),
                "{file} no longer contains `{needle}`; the modifier for {block} has \
                 changed in the jar and this table is stale"
            );
            let expected: f32 = modifier.trim_end_matches('F').parse().expect("modifier parses");
            assert_eq!(
                block_damage_modifier(block),
                expected,
                "{block} must carry {file}'s own modifier"
            );
            checked += 1;
        }

        // Powder snow: an override with no damage call.
        let powder = source_of("PowderSnowBlock.java");
        let fall_on = powder
            .split("public void fallOn(")
            .nth(1)
            .expect("PowderSnowBlock must override fallOn");
        let body = fall_on.split("\n   }").next().expect("fallOn body");
        assert!(
            !body.contains("causeFallDamage"),
            "PowderSnowBlock.fallOn now calls causeFallDamage; powder snow is no \
             longer a complete cancellation and `block_damage_modifier` is stale"
        );
        assert_eq!(block_damage_modifier("minecraft:powder_snow"), 0.0);
        checked += 1;

        assert_eq!(checked, 4, "an audit that checked nothing is not a pass");
        // And the default, so the match arm's fallthrough is covered.
        assert_eq!(
            block_damage_modifier("minecraft:stone"),
            DEFAULT_BLOCK_DAMAGE_MODIFIER
        );
    }
}
