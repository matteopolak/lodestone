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
//!   [`crate::fall::FallTracker`] (issue #265) is now a second damage source
//!   reaching the player (no melee or explosions yet) and
//!   [`PlayerVitals::apply_fall_damage`] does not consult a `HurtCooldown` —
//!   still deferred, so a fall landing in the same window as a drowning hit
//!   would currently double-apply, exactly the gap this note already
//!   flagged. Not forgotten, just still nothing to gate against anything
//!   *else* yet.
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

/// `minecraft:fall`, resolved from the real damage-type registry
/// ([`lodestone_data::damage_types`], issue #263).
///
/// Not a `const`: [`DamageType::from_name`] is a table scan, so this is a
/// function. It is called once per landing, not per tick, and the panic is
/// unreachable for a name the generated table is asserted to contain.
fn fall_damage_type() -> lodestone_data::damage_types::DamageType {
    lodestone_data::damage_types::DamageType::from_name("minecraft:fall")
        .expect("minecraft:fall is in the generated damage-type table")
}

/// `minecraft:outside_border`, resolved from the real damage-type registry the
/// same way [`fall_damage_type`] is.
///
/// Called once per border-damage tick rather than per landing, so the table scan
/// is on a hotter path than `fall_damage_type`'s — still a scan of a generated
/// const table, and `crate::server`'s `vitals_tick` only reaches it when
/// `damage_for_position` is `Some`, i.e. when the player is actually past the
/// safe zone. With a default full-size border that is never.
///
/// The `expect` is pinned by `outside_border_resolves_and_is_bypasses_armor`
/// below, which is the successor to the tripwire that asserted the opposite.
fn border_damage_type() -> lodestone_data::damage_types::DamageType {
    lodestone_data::damage_types::DamageType::from_name("minecraft:outside_border")
        .expect("minecraft:outside_border is in the generated damage-type table")
}

/// `minecraft:drown`. Used only to *assert* the premise behind applying
/// [`DROWN_DAMAGE`] straight to health (see the tests): `drown` is
/// `bypasses_armor`-tagged, so with no armour/absorption model the raw
/// subtraction and the full pipeline agree today. When an equipment model lands,
/// route drowning through [`lodestone_entity::apply_reductions`] the way
/// [`PlayerVitals::apply_fall_damage`] already does.
#[cfg(test)]
fn drown_damage_type() -> lodestone_data::damage_types::DamageType {
    lodestone_data::damage_types::DamageType::from_name("minecraft:drown")
        .expect("minecraft:drown is in the generated damage-type table")
}

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
    /// The invulnerability-frame gate [`apply_damage`](Self::apply_damage)
    /// consults — **not** shared with drowning ([`tick`](Self::tick)) or
    /// fall damage ([`apply_fall_damage`](Self::apply_fall_damage)), which
    /// both predate this field and stay exactly as documented elsewhere in
    /// this file (drowning's own 20-tick reset cadence *is* its i-frame
    /// gate, fall bypasses one entirely). A future pass unifying all three
    /// under one cooldown is out of this change's scope.
    hurt_cooldown: lodestone_entity::HurtCooldown,
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
            hurt_cooldown: lodestone_entity::HurtCooldown::default(),
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

    /// Applies `raw` points (already vanilla's `floor(...)` value — see
    /// [`crate::fall::FallTracker::on_player_moved`]) of fall damage through
    /// the real reduction pipeline
    /// ([`lodestone_entity::apply_reductions`]), matching
    /// `LivingEntity.actuallyHurt`'s stage order.
    ///
    /// The flags come from the real `minecraft:damage_type` table
    /// ([`lodestone_entity::DamageFlags::for_damage_type`], issue #263) rather
    /// than a hand-written `bypasses_armor: true` — `minecraft:fall` *is*
    /// `bypasses_armor`-tagged, so the derived flag is `true` and armour never
    /// reduces fall damage, but that now comes from the datapack instead of from
    /// a prose citation next to a literal that nothing checked.
    ///
    /// Resistance and enchantment protection are correctly `None`/`0.0` because
    /// this crate tracks no potion effects or equipped items for the player yet
    /// (see this module's own doc comment for the same gap already noted for
    /// drowning) — not a placeholder, an accurate statement of what currently
    /// reduces it (nothing).
    ///
    /// Returns `Some(damage_dealt)` if the hit landed (a dead player is a
    /// no-op, mirroring [`tick`](Self::tick)'s own guard), `None` otherwise.
    pub fn apply_fall_damage(&mut self, raw: f32) -> Option<f32> {
        if self.health <= 0.0 {
            return None;
        }
        let outcome = lodestone_entity::apply_reductions(
            raw,
            &lodestone_entity::Defenses::default(),
            lodestone_entity::DamageFlags::for_damage_type(fall_damage_type()),
        );
        self.health = (self.health - outcome.to_health).max(0.0);
        Some(outcome.to_health)
    }

    /// Applies border damage (issue #326, B1 enforcement) — the
    /// `max(1, floor(-dist * damage_per_block))` hit `LivingEntity.baseTick`
    /// lands on a player standing past the world border's safe zone
    /// (`LivingEntity.java:425-434`, read verbatim):
    ///
    /// ```java
    /// else if (isPlayer && !level.getWorldBorder().isWithinBounds(this.getBoundingBox())) {
    ///     double dist = getDistanceToBorder() + getSafeZone();
    ///     if (dist < 0.0) {
    ///         double damagePerBlock = getDamagePerBlock();
    ///         if (damagePerBlock > 0.0) {
    ///             this.hurtServer(level, damageSources().outOfBorder(),
    ///                 Math.max(1, Mth.floor(-dist * damagePerBlock)));
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// It is an `else if` *before* the water-breath block in `baseTick`, which
    /// is why [`tick`](Self::tick) and this entry point run side by side: a
    /// player under water outside the border takes border damage *and*
    /// drowning, in that order (vanilla's own branch order, one branch per
    /// `baseTick`).
    ///
    /// `raw` is already the `max(1, floor(...))` value computed by
    /// [`crate::WorldBorder::damage_for_position`] — this method is the
    /// *application* half, mirroring how [`apply_fall_damage`](Self::apply_fall_damage)
    /// receives a pre-floored value from [`crate::FallTracker`]. Like that
    /// method it bypasses the i-frame gate ([`hurt_cooldown`](Self::hurt_cooldown)),
    /// matching vanilla's `outOfBorder()` damage type carrying no
    /// `bypassesCooldown`-style exemption — border damage lands **every
    /// tick** a player is past the safe zone, exactly as the plan's gate
    /// ("a player at distance d outside takes exactly `max(1, floor(d*0.2))`
    /// per tick") requires.
    ///
    /// # The flags come from the real table, and the old reasoning here was
    /// **wrong in both halves**
    ///
    /// This used to pass `DamageFlags::default()` and justify it like so:
    /// *"`minecraft:outside_border` is not in the generated table (verified:
    /// `grep outside_border crates/lodestone-data/src/damage_types.rs` → empty)
    /// … the vanilla JSON carries **no bypass tags**, so the derived flags would
    /// be all `false` anyway."*
    ///
    /// A tripwire test asserted that absence and named the production change it
    /// wanted if the entry ever appeared. It appeared, and the second half of the
    /// justification turned out to be false independently of the first:
    /// `crates/lodestone-data/src/generated/damage_types.rs` records
    /// `outside_border` as **`bypasses_armor bypasses_shield bypasses_wolf_armor
    /// no_knockback`** — so the derived flags are *not* all `false`, and
    /// `DamageFlags::default()` was never "exactly that". The original grep
    /// looked at `damage_types.rs`; the table lives in `generated/`, which is
    /// also why "not in the table" read as evidence.
    ///
    /// So the flags now resolve through [`border_damage_type`], the same route
    /// [`apply_fall_damage`](Self::apply_fall_damage) already takes. It is
    /// behaviour-neutral **today** — `Defenses::default()` has zero armour, and
    /// `bypasses_armor` cannot change a reduction of nothing — and that is the
    /// point: when a real equipment model lands, border damage will bypass armour
    /// because vanilla says it does, rather than because nobody revisited a
    /// hardcoded default.
    ///
    /// # One thing this deliberately does **not** change
    ///
    /// The table also says `outside_border` is **not** `bypasses_cooldown`, while
    /// this method bypasses the i-frame gate structurally (it never consults
    /// [`hurt_cooldown`](Self::hurt_cooldown)). Vanilla routes border damage
    /// through `hurtServer`, so its i-frame logic does apply — and since the
    /// damage at a fixed distance is constant, vanilla would land it once per 20
    /// ticks rather than every tick. That contradicts the plan gate's stated
    /// per-tick cadence, which this crate's tests pin. Recorded rather than
    /// changed: it is a behavioural question about the cadence, not about where
    /// the flags come from, and picking it up here would silently move a number
    /// three gates assert on.
    ///
    /// Returns `Some(damage_dealt)` if the hit landed (a dead player is a
    /// no-op, mirroring [`apply_fall_damage`](Self::apply_fall_damage)), `None`
    /// otherwise.
    pub fn apply_border_damage(&mut self, raw: f64) -> Option<f32> {
        if self.health <= 0.0 {
            return None;
        }
        let outcome = lodestone_entity::apply_reductions(
            raw as f32,
            &lodestone_entity::Defenses::default(),
            lodestone_entity::DamageFlags::for_damage_type(border_damage_type()),
        );
        self.health = (self.health - outcome.to_health).max(0.0);
        Some(outcome.to_health)
    }

    /// Applies a generic incoming hit (issue #12: "mob-on-player damage needs
    /// a `PlayerVitals` entry point") through the same two-stage pipeline
    /// [`crate::SimMob::apply_damage`] already runs for a mob: the
    /// invulnerability-frame gate ([`HurtCooldown::on_hurt`]), then
    /// [`lodestone_entity::apply_reductions`]. Unlike
    /// [`apply_fall_damage`](Self::apply_fall_damage) (which bypasses the
    /// gate entirely, matching fall's own `bypasses_cooldown`-style
    /// omission — see this module's own doc comment), a generic hit is
    /// gated by [`hurt_cooldown`](Self::hurt_cooldown), matching
    /// `LivingEntity.hurt`'s real i-frame behaviour for anything that is not
    /// specifically exempted.
    ///
    /// Returns `None` if the player is already dead or the hit was fully
    /// ignored by the i-frame gate — the same "landed vs not" distinction
    /// [`crate::SimMob::apply_damage`] draws, so a caller can tell "no
    /// effect" from "not alive to hit" only by checking [`health`](Self::health)
    /// separately if it needs to.
    ///
    /// # Status: a real, tested entry point with **no production caller yet**
    ///
    /// This closes the gap `lodestone_entity::damage`'s own module doc names
    /// ("`PlayerVitals` only has `tick` (drowning) and `apply_fall_damage` —
    /// no generic melee/mob-damage entry point"), but it does not by itself
    /// make a mob attack the player: no AI in this workspace gives a hostile
    /// [`crate::SimMob`] the connected player's position as an
    /// [`attack_target`](crate::SimMob::set_attack_target) at all —
    /// `crate::mobs`'s own module doc already scopes real player-targeting
    /// AI (`NearestAttackableTargetGoal`'s population search) as a separate,
    /// larger feature, and the server's unified tick loop
    /// (`crate::tick::run_tick_loop`, issue #284) has no player-position feed
    /// into the sim to begin with (`MobSim::despawn_pass`'s own "no despawn
    /// pass" scope note names the identical missing input). Wiring that is a
    /// materially larger change than this task's "reach a live mob's health
    /// from a connection" scope — disclosed here rather than silently
    /// left unfindable, matching this project's convention for a real,
    /// unit-tested piece with a documented reason nothing calls it yet
    /// (the same shape `ViewBob::hurt`/`camera_rig.rs`'s `bobHurt` is
    /// tracked in, per `docs/combat.md`).
    pub fn apply_damage(
        &mut self,
        raw_damage: f32,
        defenses: &lodestone_entity::Defenses,
        flags: lodestone_entity::DamageFlags,
    ) -> Option<f32> {
        if self.health <= 0.0 {
            return None;
        }
        let amount = match self.hurt_cooldown.on_hurt(raw_damage, flags) {
            lodestone_entity::HurtDecision::Ignored => return None,
            lodestone_entity::HurtDecision::Full { amount }
            | lodestone_entity::HurtDecision::Topup { amount } => amount,
        };
        let outcome = lodestone_entity::apply_reductions(amount, defenses, flags);
        self.health = (self.health - outcome.to_health).max(0.0);
        Some(outcome.to_health)
    }

    /// Resets vitals to a fresh-spawn state (full air, full health, no
    /// invulnerability frames outstanding) — the one piece of vanilla's
    /// `PlayerList::respawn` this module can model, per this module's own
    /// "Death/respawn... out of scope" doc note above: no corpse, no
    /// teleport, no dimension change, just the health/air a real respawn
    /// *does* reset (`Player`'s fresh-entity defaults, the same ones
    /// [`Default`](Self::default) already establishes for a brand-new
    /// connection). Issue #270's `ServerBound::ClientCommand { action: 0 }`
    /// consumer (`crate::server::apply_client_command`) is the only caller,
    /// and only once [`health`](Self::health) has actually reached `0.0` —
    /// mirroring vanilla's own `handleClientCommand` guard
    /// (`this.player.getHealth() > 0.0F` → early return).
    pub fn respawn(&mut self) {
        *self = Self::default();
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

    /// A fall-damage hit reduces health by exactly the raw value passed in —
    /// armour is bypassed (fall is `bypasses_armor`) and this crate tracks no
    /// other reduction source for the player, so with `Defenses::default()`
    /// the reduction pipeline is a pass-through. This is the **magnitude**
    /// check: a wrong wiring that accidentally left `bypasses_armor: false`
    /// would still show *some* reduction (a sign change) but land on the
    /// wrong number for any nonzero armour default — there is none here, so
    /// this instead pins the exact expected value directly.
    #[test]
    fn fall_damage_reaches_health_unreduced_with_no_armour_tracked() {
        let mut v = PlayerVitals::default();
        let dealt = v.apply_fall_damage(7.0);
        assert_eq!(dealt, Some(7.0));
        assert_eq!(v.health(), MAX_HEALTH - 7.0);
    }

    /// Issue #263: the flags [`PlayerVitals::apply_fall_damage`] passes come
    /// from the real damage-type table, not a literal.
    ///
    /// **What this can and cannot observe, stated plainly.** `apply_fall_damage`
    /// hardcodes `Defenses::default()`, i.e. `armor: 0.0` — so at that
    /// function's own output, `bypasses_armor: true` and `bypasses_armor: false`
    /// are *indistinguishable*: both yield the raw amount. The test above is
    /// therefore silent about the flag, and a gate written there would be the
    /// "world" species of vacuous test (correct assertion, input that cannot
    /// exercise it).
    ///
    /// So this asserts the two things that *are* observable: the derivation
    /// yields the real tag value, and that value is load-bearing when composed
    /// with armour. The flag matters the moment a player equipment model lands
    /// (issue #261); until then it is correct-and-inert here, which is worth
    /// pinning rather than leaving to a comment.
    #[test]
    fn fall_flags_come_from_the_damage_type_table_and_are_load_bearing() {
        let flags = lodestone_entity::DamageFlags::for_damage_type(fall_damage_type());

        // Derived from `bypasses_armor.json`, which lists `minecraft:fall`.
        assert!(flags.bypasses_armor, "minecraft:fall is bypasses_armor-tagged");
        assert_ne!(
            flags,
            lodestone_entity::DamageFlags::default(),
            "if the derived flags equalled the default, this wiring would carry no \
             information and the table would be an island here"
        );
        // fall is not in bypasses_effects/resistance/enchantments.
        assert!(!flags.bypasses_effects);
        assert!(!flags.bypasses_resistance);
        assert!(!flags.bypasses_enchantments);
        // bypasses_cooldown is empty in vanilla 26.2; this call site deliberately
        // skips the i-frame gate in its own code, not via a tag.
        assert!(!flags.bypasses_cooldown);

        // The flag is load-bearing: against full diamond armour a raw 10.0 stays
        // 10.0 with it, and would drop to 3.0 without it. Predicting both
        // hypotheses, not just the sign.
        let armoured = lodestone_entity::Defenses {
            armor: 20.0,
            armor_toughness: 8.0,
            ..Default::default()
        };
        let with = lodestone_entity::apply_reductions(10.0, &armoured, flags).to_health;
        let without = lodestone_entity::apply_reductions(
            10.0,
            &armoured,
            lodestone_entity::DamageFlags {
                bypasses_armor: false,
                ..flags
            },
        )
        .to_health;
        assert!((with - 10.0).abs() < 1e-4, "bypassed: expected 10.0, got {with}");
        assert!((without - 3.0).abs() < 1e-4, "reduced: expected 3.0, got {without}");
        assert!(
            with - without > 6.9,
            "the two hypotheses must differ by the full armour reduction"
        );
    }

    /// The premise behind subtracting [`DROWN_DAMAGE`] straight from health:
    /// `minecraft:drown` is `bypasses_armor`-tagged, so the shortcut and the full
    /// pipeline agree *today*. Pinned so that if a future version untags it, or
    /// an equipment model lands, this fails instead of silently over-damaging.
    #[test]
    fn the_drowning_shortcut_agrees_with_the_pipeline_for_now() {
        let flags = lodestone_entity::DamageFlags::for_damage_type(drown_damage_type());
        assert!(
            flags.bypasses_armor,
            "minecraft:drown is bypasses_armor-tagged, which is why applying DROWN_DAMAGE \
             directly to health is currently equivalent to running the pipeline"
        );
        let piped = lodestone_entity::apply_reductions(
            DROWN_DAMAGE,
            &lodestone_entity::Defenses::default(),
            flags,
        )
        .to_health;
        assert_eq!(
            piped, DROWN_DAMAGE,
            "the shortcut and the pipeline must agree while there is no armour model"
        );
    }

    /// Fall damage floors health at `0.0`, never going negative, mirroring
    /// [`tick`](PlayerVitals::tick)'s own drowning-damage clamp.
    #[test]
    fn fall_damage_floors_health_at_zero() {
        let mut v = PlayerVitals::default();
        let dealt = v.apply_fall_damage(999.0);
        assert_eq!(dealt, Some(999.0), "the full raw amount was applied");
        assert_eq!(v.health(), 0.0);
    }

    /// **Control**: a dead player's vitals must not take further fall
    /// damage, exactly like [`dead_vitals_do_not_tick`] proves for drowning —
    /// the same `health <= 0.0` guard applies to both damage sources.
    #[test]
    fn dead_player_takes_no_further_fall_damage() {
        let mut v = PlayerVitals::default();
        v.apply_fall_damage(999.0);
        assert_eq!(v.health(), 0.0);
        let dealt = v.apply_fall_damage(5.0);
        assert_eq!(dealt, None, "a dead player must not take more damage");
        assert_eq!(v.health(), 0.0);
    }

    // ---- `apply_border_damage` (issue #326, B1 enforcement) --------------

    /// A border hit reduces health by exactly the `max(1, floor(-dist *
    /// damage_per_block))` value [`WorldBorder::damage_for_position`] handed
    /// in — outside_border has no bypass tags but this crate tracks no armour
    /// either, so with `Defenses::default()` the pipeline is a pass-through.
    /// The magnitude is pinned, not just the sign: `2.0` in means `2.0` out
    /// and `20.0 - 2.0` on the wire's health.
    #[test]
    fn border_damage_reaches_health_unreduced_with_no_armour_tracked() {
        let mut v = PlayerVitals::default();
        let dealt = v.apply_border_damage(2.0);
        assert_eq!(dealt, Some(2.0));
        assert_eq!(v.health(), MAX_HEALTH - 2.0);
    }

    /// The `f64` → `f32` narrowing is faithful for the values the border can
    /// actually produce: a `f64` that lands exactly on a representable `f32`
    /// (every integer `max(1, floor(...))` value is) must not drift. The
    /// enforcement calls this with `f64` amounts — the plan gate's per-tick
    /// `max(1, floor(d * 0.2))` values are all small exact integers.
    #[test]
    fn border_damage_narrowing_f64_to_f32_is_exact_for_wire_values() {
        let mut v = PlayerVitals::default();
        for (raw, expected) in [(1.0, 1.0), (2.0, 2.0), (4.0, 4.0), (5.0, 5.0)] {
            let dealt = v.apply_border_damage(raw);
            assert_eq!(dealt, Some(expected));
            v = PlayerVitals::default();
        }
    }

    /// **Control**: a dead player's vitals must not take further border
    /// damage — the same `health <= 0.0` guard as every other damage source.
    #[test]
    fn dead_player_takes_no_further_border_damage() {
        let mut v = PlayerVitals::default();
        v.apply_border_damage(999.0);
        assert_eq!(v.health(), 0.0);
        let dealt = v.apply_border_damage(1.0);
        assert_eq!(dealt, None, "a dead player must not take more border damage");
        assert_eq!(v.health(), 0.0);
    }

    /// Border damage **bypasses the i-frame gate** — that is the plan gate's
    /// "per tick" cadence (`LivingEntity.baseTick` lands it every tick a
    /// player is past the safe zone). Two back-to-back hits both land, unlike
    /// a gated [`apply_damage`](Self::apply_damage) whose second call within
    /// 20 ticks is ignored. This is the property that distinguishes border
    /// damage from a generic hit, so it is asserted directly rather than left
    /// to a comment.
    #[test]
    fn border_damage_bypasses_the_iframe_gate_for_the_per_tick_cadence() {
        let mut v = PlayerVitals::default();
        assert_eq!(v.apply_border_damage(1.0), Some(1.0));
        assert_eq!(
            v.apply_border_damage(1.0),
            Some(1.0),
            "the very next tick lands too — no 20-tick cooldown"
        );
        assert_eq!(v.health(), MAX_HEALTH - 2.0);
    }

    /// The successor to a tripwire that asserted the **opposite** and fired.
    ///
    /// It read: *"`minecraft:outside_border` is not in the generated damage-type
    /// table … if this starts resolving, route `apply_border_damage` through
    /// `for_damage_type`"*. It started resolving, and the routing was done — so
    /// this pins the entry's presence, which is what
    /// [`border_damage_type`]'s `expect` rests on.
    ///
    /// It also pins the tag, because that is the half the old reasoning got
    /// **backwards**: the previous doc claimed the vanilla JSON "carries no bypass
    /// tags, so the derived flags would be all `false` anyway", and the generated
    /// table says `bypasses_armor`. An absence-only successor would have left that
    /// error in place.
    #[test]
    fn outside_border_resolves_and_is_bypasses_armor() {
        let ty = lodestone_data::damage_types::DamageType::from_name("minecraft:outside_border")
            .expect("minecraft:outside_border must be in the generated damage-type table");
        let flags = lodestone_entity::DamageFlags::for_damage_type(ty);
        assert!(
            flags.bypasses_armor,
            "the generated table records outside_border as bypasses_armor \
             (crates/lodestone-data/src/generated/damage_types.rs); the pre-fix doc comment \
             claimed it carried no bypass tags at all"
        );
        assert!(
            !flags.bypasses_cooldown,
            "and it is NOT bypasses_cooldown — see `apply_border_damage`'s doc comment for why \
             the per-tick cadence is nonetheless left as it is"
        );
    }

    /// **The magnitude gate for the routing**, and the only assertion here that
    /// can tell the two flag hypotheses apart.
    ///
    /// [`PlayerVitals::apply_border_damage`] passes `Defenses::default()`, so with
    /// zero armour `bypasses_armor` reduces nothing either way and every other
    /// border-damage test in this module passes under **both** hypotheses. That is
    /// a real exposure, not a nitpick: it is exactly why the wrong flags survived.
    ///
    /// So this drives the production flags through the production reduction
    /// function with armour that *does* bite, and requires the result to land on
    /// one of two numbers computed from outside this module:
    ///
    /// | hypothesis | expected |
    /// |---|---|
    /// | table flags (`bypasses_armor`) | `10.0` — armour skipped entirely |
    /// | `DamageFlags::default()` | `3.0` — the figure `apply_damage_runs_the_full_armour_reduction_pipeline` pins for the same defenses |
    ///
    /// Both are asserted, the wrong one negatively, so a future change that
    /// reverts the routing fails here rather than passing quietly.
    #[test]
    fn border_damage_flags_skip_armour_where_the_default_flags_would_not() {
        const NO_BYPASS_HYPOTHESIS: f32 = 3.0;
        let defenses = lodestone_entity::Defenses {
            armor: 20.0,
            armor_toughness: 8.0,
            ..Default::default()
        };
        let flags = lodestone_entity::DamageFlags::for_damage_type(border_damage_type());

        let dealt = lodestone_entity::apply_reductions(10.0, &defenses, flags).to_health;
        assert_eq!(
            dealt, 10.0,
            "outside_border is bypasses_armor-tagged, so a fully-armoured player takes the \
             raw amount"
        );
        assert_ne!(
            dealt, NO_BYPASS_HYPOTHESIS,
            "landing on {NO_BYPASS_HYPOTHESIS} would mean apply_border_damage is back on \
             DamageFlags::default() and the table lookup was reverted"
        );

        // The control: the same defenses under the wrong hypothesis really do
        // produce the other number, so the assertion above is separating two
        // reachable outcomes rather than one outcome and one impossibility.
        let unflagged = lodestone_entity::apply_reductions(
            10.0,
            &defenses,
            lodestone_entity::DamageFlags::default(),
        )
        .to_health;
        assert_eq!(
            unflagged, NO_BYPASS_HYPOTHESIS,
            "control: without bypasses_armor the same hit must reduce to \
             {NO_BYPASS_HYPOTHESIS}, or this gate is comparing 10.0 against nothing"
        );
    }

    // ---- `apply_damage` (issue #12's "mob-on-player" entry point) --------

    /// The full reduction pipeline runs, with the same live-verified
    /// diamond-armour number `mob_attack.rs` (`lodestone-server`'s own
    /// acceptance gate for `MobSim::attack`) pins for a mob — proving this
    /// entry point is not a second, independently-drifting formula.
    #[test]
    fn apply_damage_runs_the_full_armour_reduction_pipeline() {
        let mut v = PlayerVitals::default();
        let defenses = lodestone_entity::Defenses {
            armor: 20.0,
            armor_toughness: 8.0,
            ..Default::default()
        };
        let dealt = v.apply_damage(10.0, &defenses, lodestone_entity::DamageFlags::default());
        assert_eq!(dealt, Some(3.0));
        assert_eq!(v.health(), MAX_HEALTH - 3.0);
    }

    /// The invulnerability-frame gate is real and **separate** from
    /// drowning's/fall's own cadence: a weaker follow-up inside the 20-tick
    /// window is ignored (`None`, health untouched), the identical
    /// `HurtCooldown` behaviour `lodestone-entity`'s own tests pin.
    #[test]
    fn apply_damage_ignores_a_weaker_followup_inside_the_iframe_window() {
        let mut v = PlayerVitals::default();
        let defenses = lodestone_entity::Defenses::default();
        let flags = lodestone_entity::DamageFlags::default();

        let first = v.apply_damage(8.0, &defenses, flags);
        assert_eq!(first, Some(8.0));

        let second = v.apply_damage(5.0, &defenses, flags);
        assert_eq!(second, None, "a weaker follow-up must be ignored inside i-frames");
        assert_eq!(v.health(), MAX_HEALTH - 8.0, "health must not drop again");
    }

    /// **Control**: a dead player takes no further generic damage either —
    /// the third damage source now sharing the identical `health <= 0.0`
    /// guard [`dead_vitals_do_not_tick`]/[`dead_player_takes_no_further_fall_damage`]
    /// already prove for the other two.
    /// [`PlayerVitals::respawn`] must actually restore a dead player to full
    /// health/air, not merely leave `health` untouched at `0.0` — the
    /// positive half of issue #270's respawn consumer.
    #[test]
    fn respawn_restores_full_health_and_air_after_death() {
        let mut v = PlayerVitals::default();
        v.apply_fall_damage(999.0);
        assert_eq!(v.health(), 0.0, "expected the player to be dead before respawning");

        v.respawn();
        assert_eq!(v.health(), MAX_HEALTH, "respawn must restore full health");
        assert_eq!(v.air_supply(), MAX_AIR_SUPPLY, "respawn must restore full air");

        // Control: a respawned (alive) player must resume taking damage
        // normally — proof this isn't a cosmetic reset that leaves the
        // `health <= 0.0` guards still latched somehow.
        let dealt = v.apply_fall_damage(5.0);
        assert_eq!(dealt, Some(5.0), "a respawned player must be able to take damage again");
        assert_eq!(v.health(), MAX_HEALTH - 5.0);
    }

    #[test]
    fn dead_player_takes_no_further_generic_damage() {
        let mut v = PlayerVitals::default();
        v.apply_fall_damage(999.0);
        assert_eq!(v.health(), 0.0);
        let dealt = v.apply_damage(
            1.0,
            &lodestone_entity::Defenses::default(),
            lodestone_entity::DamageFlags::default(),
        );
        assert_eq!(dealt, None, "a dead player must not take more damage");
    }
}
