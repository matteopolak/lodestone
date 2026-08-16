//! Entity burning: ignition, the fire-tick damage interval, and fire immunity.
//!
//! # What it is
//!
//! [`BurnState`] is vanilla's `Entity.remainingFireTicks` plus the `baseTick` block
//! that consumes it. A pure value type with a [`tick`](BurnState::tick) step reporting
//! a [`BurnTick`], the same split [`crate::food`] and [`crate::mob_effects`] draw — so
//! [`crate::vitals::PlayerVitals`] stays the single owner of health.
//!
//! **Fire *spread* between blocks is `crate::fire`'s**, not this module's. This is the
//! entity-facing half: standing in fire sets a counter, the counter deals damage on an
//! interval, and it keeps burning after the entity walks out.
//!
//! # How it works
//!
//! ## The burn tick, and the lava guard that stops double-damage
//!
//! `Entity.baseTick`, verbatim:
//!
//! ```java
//! if (this.remainingFireTicks > 0) {
//!    if (this.fireImmune()) {
//!       this.clearFire();
//!    } else {
//!       if (this.remainingFireTicks % 20 == 0 && !this.isInLava()) {
//!          this.hurtServer(serverLevel, this.damageSources().onFire(), 1.0F);
//!       }
//!       this.setRemainingFireTicks(this.remainingFireTicks - 1);
//!    }
//! }
//! ```
//!
//! Three details, each of which changes a number:
//!
//! * **The counter counts *down*, and the modulo is on the remaining value.** An
//!   8-second ignition is 160 ticks, so hits land at remaining 160, 140, …, 20 —
//!   **exactly 8**, one per second, and none at 0 because the outer `> 0` fails
//!   first. Counting *up* from zero lands on the same cadence only when the duration
//!   is a multiple of 20.
//! * **`&& !this.isInLava()`.** While actually standing in lava the burn deals **no**
//!   damage of its own: lava's own `lavaHurt` (4.0 per tick) is the damage, and
//!   without this guard an entity in lava takes both. The burn counter still ticks
//!   down, so leaving lava leaves the remainder burning.
//! * **`fireImmune()` calls `clearFire()`, which is `min(0, remaining)` — not `0`.**
//!   A *negative* counter is meaningful (see below), so zeroing it would discard the
//!   grace period. `clearFire` on a positive counter gives 0; on a negative one it
//!   leaves the negative value alone.
//!
//! ## Ignition only ever raises the counter
//!
//! ```java
//! public void igniteForTicks(final int numberOfTicks) {
//!    if (this.remainingFireTicks < numberOfTicks) {
//!       this.setRemainingFireTicks(numberOfTicks);
//!    }
//! }
//! ```
//!
//! So stepping out of lava (300 ticks) into fire (160) does **not** shorten the burn
//! to 160. A plain assignment would, and "walking through a campfire puts out your
//! lava burn" is exactly the kind of wrong that looks like nothing.
//!
//! `igniteForSeconds` is `floor(seconds * 20)`, so the seconds figure is the one to
//! quote and the tick count is derived.
//!
//! | source | vanilla call | ticks |
//! |---|---|---|
//! | fire / soul fire block | `BaseFireBlock.fireIgnite` → `igniteForSeconds(8.0F)` | 160 |
//! | lava | `Entity.lavaIgnite` → `igniteForSeconds(15.0F)` | 300 |
//!
//! And the *contact* damage is per-block, not shared: `FireBlock` passes `1.0F` to
//! `BaseFireBlock`'s constructor and `SoulFireBlock` passes **`2.0F`**. Lava's contact
//! damage is `4.0F` per tick, an order of magnitude above either.
//!
//! ## The negative counter is a grace period, and it is player-only
//!
//! `BaseFireBlock.fireIgnite`:
//!
//! ```java
//! if (!entity.fireImmune()) {
//!    if (entity.getRemainingFireTicks() < 0) {
//!       entity.setRemainingFireTicks(entity.getRemainingFireTicks() + 1);
//!    } else if (entity instanceof ServerPlayer) {
//!       int addedFireTicks = entity.level().getRandom().nextInt(1, 3);
//!       entity.setRemainingFireTicks(entity.getRemainingFireTicks() + addedFireTicks);
//!    }
//!    if (entity.getRemainingFireTicks() >= 0) {
//!       entity.igniteForSeconds(8.0F);
//!    }
//! }
//! ```
//!
//! A **player** does not ignite on the first contact tick: the counter ramps by
//! `nextInt(1, 3)` (i.e. 1 or 2) per tick and only once it is non-negative does the
//! 8-second ignition land. That is why running across a single fire block can leave
//! you unburnt while standing still cannot. The `else if` matters: a non-player entity
//! with a non-negative counter takes the ignition **immediately**.
//!
//! The negative branch is where a fire-immunity grace period lands
//! (`setRemainingFireTicks(-getFireImmuneTicks())`); `Entity.getFireImmuneTicks` is
//! `0` by default and only a few entities override it, so a plain player starts at 0
//! and one contact tick's ramp is enough.
//!
//! ## Fire Resistance is a damage-source check, not a burn-counter check
//!
//! `LivingEntity.hurt`: `if (source.is(DamageTypeTags.IS_FIRE) && this.hasEffect(MobEffects.FIRE_RESISTANCE))`
//! → immune. So the counter **still runs** and the entity still visibly burns; the
//! *damage* is refused. Clearing the counter instead would put the fire out, which is
//! a visible divergence and also loses the burn when the effect expires.
//!
//! The `#minecraft:is_fire` tag is `in_fire`, `campfire`, `on_fire`, `lava`,
//! `hot_floor`, `sulfur_cube_hot`, `unattributed_fireball`, `fireball` — read out of
//! the jar's own tag JSON rather than guessed, because `on_fire` (the burn tick) and
//! `in_fire` (standing in the block) are *two* entries and missing either makes fire
//! resistance half-work.
//!
//! # What is not here
//!
//! * **Lightning.** `LightningBolt` is an entity with target selection, a `nextInt`
//!   draw over a weather-eligible column, direct-hit damage and the
//!   creeper→charged / villager→witch transformations. It needs an entity type
//!   `MobSim` does not have, and the transformations need a per-species table; the
//!   issue groups it here because a strike's entity-facing effect *is* ignition plus
//!   a damage instance, and that part is this module.
//! * **A mob does not yet ignite from a fire/lava block.** `SimMob` now carries a
//!   real [`BurnState`] (`MobSim::tick_burning` consumes it every tick, and a small
//!   fireball's impact — `MobSim::resolve_projectile_hit` — raises it), but nothing
//!   yet reads what block a mob's feet are standing in the way the player path
//!   below does, so contact ignition and this module's lava/contact-damage guards
//!   never fire for a mob. Water contact does extinguish a mob
//!   (`MobSim::tick_burning`, through `SimMob::in_water`).
//! * **No client-visible `on_fire` flag streams for a mob.** `SimMob::is_on_fire`
//!   exists and a burning mob really does lose health over time, but nothing
//!   encodes the shared-flags metadata bit a client would render a fire overlay
//!   from — the wire encoder lives in the version crate, outside this module's
//!   reach. The damage is real; the client-side visual is not yet wired.
//! * **Rain does not extinguish anything**, mob or player. `Entity.baseTick`'s
//!   water block calls `clearFire()`; wiring rain needs a weather read this
//!   module is not given.
//!
//! # Dependencies
//!
//! None beyond `std`. The caller supplies what block the entity is standing in and
//! whether it has Fire Resistance.

/// `Entity.igniteForSeconds`' factor.
const TICKS_PER_SECOND: f32 = 20.0;

/// The interval `baseTick` deals burn damage on — `remainingFireTicks % 20 == 0`.
pub const BURN_DAMAGE_INTERVAL: i32 = 20;

/// The burn tick's own damage — `hurtServer(damageSources().onFire(), 1.0F)`.
pub const BURN_DAMAGE: f32 = 1.0;

/// `BaseFireBlock.fireIgnite`'s `igniteForSeconds(8.0F)`, in ticks.
pub const FIRE_IGNITE_TICKS: i32 = 160;

/// `Entity.lavaIgnite`'s `igniteForSeconds(15.0F)`, in ticks — nearly twice a fire
/// block's, which is why stepping from lava into fire must not shorten the burn.
pub const LAVA_IGNITE_TICKS: i32 = 300;

/// `FireBlock`'s `fireDamage`, the contact hit for standing in it.
pub const FIRE_CONTACT_DAMAGE: f32 = 1.0;

/// `SoulFireBlock`'s `fireDamage` — **twice** ordinary fire's. Read off its own
/// `super(properties, 2.0F)` call rather than assumed equal.
pub const SOUL_FIRE_CONTACT_DAMAGE: f32 = 2.0;

/// `Entity.lavaHurt`'s `4.0F`, applied **every tick** an entity is in lava.
pub const LAVA_CONTACT_DAMAGE: f32 = 4.0;

/// `Entity.igniteForSeconds` — `Mth.floor(seconds * 20.0F)`.
#[must_use]
pub fn ignite_ticks_for_seconds(seconds: f32) -> i32 {
    (seconds * TICKS_PER_SECOND).floor() as i32
}

/// What an entity is standing in, as far as burning is concerned.
///
/// Deliberately three variants rather than a bool: soul fire's contact damage is
/// double ordinary fire's, and lava both ignites for longer *and* suppresses the burn
/// tick's own damage while the entity is in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnSource {
    /// `minecraft:fire`.
    Fire,
    /// `minecraft:soul_fire`.
    SoulFire,
    /// `minecraft:lava` (or flowing lava).
    Lava,
}

impl BurnSource {
    /// The [`BurnSource`] for a block-state string, or `None` for anything that does
    /// not burn.
    #[must_use]
    pub fn for_block(state: &str) -> Option<Self> {
        match state.split('[').next().unwrap_or(state) {
            "minecraft:fire" => Some(Self::Fire),
            "minecraft:soul_fire" => Some(Self::SoulFire),
            "minecraft:lava" | "minecraft:flowing_lava" => Some(Self::Lava),
            _ => None,
        }
    }

    /// How long contact with this ignites for.
    #[must_use]
    pub fn ignite_ticks(self) -> i32 {
        match self {
            // Both fire blocks go through `BaseFireBlock.fireIgnite`, so they share
            // the duration and differ only in contact damage.
            Self::Fire | Self::SoulFire => FIRE_IGNITE_TICKS,
            Self::Lava => LAVA_IGNITE_TICKS,
        }
    }

    /// The per-tick contact damage while standing in this.
    #[must_use]
    pub fn contact_damage(self) -> f32 {
        match self {
            Self::Fire => FIRE_CONTACT_DAMAGE,
            Self::SoulFire => SOUL_FIRE_CONTACT_DAMAGE,
            Self::Lava => LAVA_CONTACT_DAMAGE,
        }
    }

    /// Whether this is lava — which suppresses the burn tick's own damage, so the
    /// entity takes `4.0` from the lava rather than `4.0 + 1.0`.
    #[must_use]
    pub fn is_lava(self) -> bool {
        matches!(self, Self::Lava)
    }
}

/// What one [`BurnState::tick`] decided.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BurnTick {
    /// Damage to apply. Already the sum of the contact hit and the burn tick's own
    /// (with the lava guard applied), and already zero when Fire Resistance refused
    /// it.
    pub damage: f32,
    /// `true` when the entity's visible on-fire flag changed, so a caller knows to
    /// restream metadata. Set on both ignition and burn-out.
    pub on_fire_changed: bool,
}

impl BurnTick {
    /// Whether this tick produced anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.damage == 0.0 && !self.on_fire_changed
    }
}

/// One entity's burn counter — vanilla's `remainingFireTicks`.
///
/// A negative value is a *grace period*, not "not burning": see this module's doc on
/// `fireIgnite`. `0` is the ordinary not-burning value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BurnState {
    remaining: i32,
}

impl BurnState {
    /// Not burning.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remaining fire ticks. Can be negative — a grace period.
    #[must_use]
    pub fn remaining(&self) -> i32 {
        self.remaining
    }

    /// Whether the entity is visibly on fire — `Entity.isOnFire`'s
    /// `remainingFireTicks > 0` half.
    #[must_use]
    pub fn is_on_fire(&self) -> bool {
        self.remaining > 0
    }

    /// `Entity.igniteForTicks` — **only ever raises** the counter.
    ///
    /// This is the guard that stops a short ignition cutting a long burn short. A
    /// plain assignment makes walking through fire extinguish a lava burn.
    pub fn ignite_for_ticks(&mut self, ticks: i32) {
        if self.remaining < ticks {
            self.remaining = ticks;
        }
    }

    /// `Entity.igniteForSeconds`.
    pub fn ignite_for_seconds(&mut self, seconds: f32) {
        self.ignite_for_ticks(ignite_ticks_for_seconds(seconds));
    }

    /// `Entity.clearFire` — `min(0, remaining)`, **not** `0`.
    ///
    /// A negative grace period survives; a positive burn is put out. Zeroing instead
    /// would discard the grace, so a fire-immune entity re-entering fire would ignite
    /// a tick sooner than vanilla's.
    pub fn clear(&mut self) {
        self.remaining = self.remaining.min(0);
    }

    /// `BaseFireBlock.fireIgnite` — contact with a fire block, which for a **player**
    /// ramps a negative counter rather than igniting immediately.
    ///
    /// `ramp` is the `nextInt(1, 3)` draw (1 or 2), supplied by the caller because
    /// this type owns no RNG; it is consumed **only** on the non-negative player
    /// branch, and the draw count is part of the specification.
    ///
    /// `is_player` selects vanilla's `else if (entity instanceof ServerPlayer)`: a
    /// non-player entity with a non-negative counter skips the ramp and ignites at
    /// once.
    pub fn fire_ignite(&mut self, is_player: bool, ramp: i32) {
        if self.remaining < 0 {
            // The grace period burns off one tick at a time, whoever the entity is.
            self.remaining += 1;
        } else if is_player {
            self.remaining += ramp.clamp(1, 2);
        }
        if self.remaining >= 0 {
            self.ignite_for_seconds(8.0);
        }
    }

    /// Advances the burn by one tick and reports the damage — `Entity.baseTick`'s
    /// fire block, plus the contact damage the block itself applies.
    ///
    /// * `standing_in` is what the entity's cell holds, or `None` for anything that
    ///   does not burn. It supplies both the contact damage and vanilla's
    ///   `isInLava()` for the burn-tick guard.
    /// * `fire_immune` is the entity type's own `fireImmune()` — a blaze, not an
    ///   effect. It **clears the fire** rather than merely refusing damage.
    /// * `fire_resistance` is the Fire Resistance *effect*. It refuses the damage and
    ///   leaves the counter running, because vanilla's check is on the damage source
    ///   and not on the counter.
    ///
    /// The ignition itself is **not** done here: a caller decides whether contact
    /// happened (it may be gated on a game mode, a grace period, or an RNG draw) and
    /// calls [`fire_ignite`](Self::fire_ignite) or
    /// [`ignite_for_ticks`](Self::ignite_for_ticks). This method is the consumption
    /// half only.
    pub fn tick(
        &mut self,
        standing_in: Option<BurnSource>,
        fire_immune: bool,
        fire_resistance: bool,
    ) -> BurnTick {
        let mut out = BurnTick::default();
        let was_on_fire = self.is_on_fire();

        // The block's own per-tick contact hit (`entityInside` → `hurt`, and
        // `lavaHurt` for lava). Independent of the burn counter: a fire-immune entity
        // takes neither, but Fire Resistance is what refuses it for everyone else.
        if let Some(source) = standing_in
            && !fire_immune
            && !fire_resistance
        {
            out.damage += source.contact_damage();
        }

        if self.remaining > 0 {
            if fire_immune {
                // `clearFire()`, not `= 0` — see that method's doc.
                self.clear();
            } else {
                // `remainingFireTicks % 20 == 0 && !isInLava()`. The lava guard is
                // what stops an entity in lava taking 4.0 + 1.0 in one tick.
                if self.remaining % BURN_DAMAGE_INTERVAL == 0
                    && !standing_in.is_some_and(BurnSource::is_lava)
                    && !fire_resistance
                {
                    out.damage += BURN_DAMAGE;
                }
                self.remaining -= 1;
            }
        }

        out.on_fire_changed = was_on_fire != self.is_on_fire();
        out
    }

    /// Rebuilds from saved NBT — vanilla's `Fire` field, a **`Short`**.
    ///
    /// Clamped to `i16`'s range for that reason: a value outside it cannot have come
    /// from a real save, and carrying one would write a `Fire` field the next reader
    /// truncates differently.
    #[must_use]
    pub fn restored(remaining: i32) -> Self {
        Self {
            remaining: remaining.clamp(i32::from(i16::MIN), i32::from(i16::MAX)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The durations come from `igniteForSeconds`' own floor, and fire and lava are
    /// **different**: 160 against 300.
    #[test]
    fn the_two_ignition_durations_are_eight_and_fifteen_seconds() {
        assert_eq!(ignite_ticks_for_seconds(8.0), 160);
        assert_eq!(ignite_ticks_for_seconds(15.0), 300);
        assert_eq!(FIRE_IGNITE_TICKS, 160);
        assert_eq!(LAVA_IGNITE_TICKS, 300);
        assert_eq!(BurnSource::Fire.ignite_ticks(), 160);
        assert_eq!(BurnSource::SoulFire.ignite_ticks(), 160, "soul fire shares the duration");
        assert_eq!(BurnSource::Lava.ignite_ticks(), 300);
        // The floor is a real floor, not a round.
        assert_eq!(ignite_ticks_for_seconds(0.55), 11, "0.55 * 20 = 11.0");
        assert_eq!(ignite_ticks_for_seconds(0.99), 19, "0.99 * 20 = 19.8, floored");
    }

    /// Soul fire hits **twice** as hard on contact, and lava four times as hard as
    /// ordinary fire — three distinct constants, so a shared one fails here.
    #[test]
    fn the_three_contact_damages_are_distinct() {
        assert_eq!(BurnSource::Fire.contact_damage(), 1.0);
        assert_eq!(BurnSource::SoulFire.contact_damage(), 2.0);
        assert_eq!(BurnSource::Lava.contact_damage(), 4.0);
        assert_ne!(
            BurnSource::SoulFire.contact_damage(),
            BurnSource::Fire.contact_damage(),
            "SoulFireBlock passes 2.0F to BaseFireBlock, FireBlock passes 1.0F"
        );
    }

    #[test]
    fn burn_sources_resolve_from_block_states() {
        assert_eq!(BurnSource::for_block("minecraft:fire[age=3]"), Some(BurnSource::Fire));
        assert_eq!(BurnSource::for_block("minecraft:soul_fire"), Some(BurnSource::SoulFire));
        assert_eq!(BurnSource::for_block("minecraft:lava[level=0]"), Some(BurnSource::Lava));
        assert_eq!(BurnSource::for_block("minecraft:flowing_lava"), Some(BurnSource::Lava));
        assert_eq!(BurnSource::for_block("minecraft:water"), None);
        assert_eq!(BurnSource::for_block("minecraft:stone"), None);
        assert_eq!(BurnSource::for_block("minecraft:campfire"), None, "not modelled yet");
    }

    /// **The magnitude gate for the burn cadence.** An 8-second ignition, left to burn
    /// out in open air, deals exactly **8** damage — one hit per second, at remaining
    /// 160, 140, …, 20, and **none at 0** because the outer `> 0` fails first.
    ///
    /// The wrong hypotheses, both computed from the constants:
    ///
    /// | hypothesis | total |
    /// |---|---|
    /// | `% 20` on the counting-down remaining value (correct) | **8** |
    /// | damage every tick | 160 |
    /// | one hit including remaining 0 | 9 |
    ///
    /// All three are separated, and the exact tick list is asserted rather than just
    /// the count.
    #[test]
    fn a_fire_ignition_deals_exactly_eight_damage_over_eight_seconds() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(FIRE_IGNITE_TICKS);

        let mut hits = Vec::new();
        let mut total = 0.0;
        for t in 1..=200 {
            let out = burn.tick(None, false, false);
            if out.damage > 0.0 {
                hits.push(t);
                total += out.damage;
            }
        }
        assert_eq!(total, 8.0, "one hit per second for eight seconds: {hits:?}");
        assert_ne!(total, 160.0, "damage every tick would be 160");
        assert_ne!(total, 9.0, "a hit at remaining 0 would be a ninth");
        assert_eq!(
            hits,
            vec![1, 21, 41, 61, 81, 101, 121, 141],
            "the first hit is on the ignition tick itself, because 160 % 20 == 0"
        );
        assert!(!burn.is_on_fire(), "the burn must have gone out");
        assert_eq!(burn.remaining(), 0);
    }

    /// **The lava guard.** While standing in lava the burn tick deals **no** damage of
    /// its own — the entity takes `4.0` from the lava, not `5.0`.
    ///
    /// Two hypotheses at exactly the tick where both would fire (remaining 300, a
    /// multiple of 20):
    ///
    /// | hypothesis | damage on that tick |
    /// |---|---|
    /// | with `!isInLava()` (correct) | **4.0** |
    /// | without the guard | 5.0 |
    ///
    /// And the counter still ticks down, so leaving lava leaves the remainder burning
    /// — asserted, because a guard implemented as "skip the whole block in lava" would
    /// freeze the counter instead.
    #[test]
    fn standing_in_lava_suppresses_the_burn_tick_but_not_the_countdown() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(LAVA_IGNITE_TICKS);
        assert_eq!(burn.remaining() % BURN_DAMAGE_INTERVAL, 0, "the guard is live this tick");

        let out = burn.tick(Some(BurnSource::Lava), false, false);
        assert_eq!(
            out.damage, 4.0,
            "lava's own contact damage only — 5.0 would mean the !isInLava guard is missing"
        );
        assert_eq!(
            burn.remaining(),
            LAVA_IGNITE_TICKS - 1,
            "the counter must still tick down while in lava, or leaving lava would \
             restart the full burn"
        );

        // Out of lava on the next multiple of 20: now the burn tick does fire, and
        // alone.
        let mut out_of_lava = BurnState::new();
        out_of_lava.ignite_for_ticks(LAVA_IGNITE_TICKS);
        let out = out_of_lava.tick(None, false, false);
        assert_eq!(out.damage, 1.0, "out of lava, the burn tick's own 1.0 lands");
    }

    /// **Ignition only raises.** Stepping out of lava (300 ticks) into fire (160) must
    /// leave 300, not 160.
    ///
    /// This is the case a plain assignment gets wrong, and the failure — "walking
    /// through a campfire puts out your lava burn" — looks like nothing happening.
    #[test]
    fn a_shorter_ignition_never_cuts_a_longer_burn_short() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(LAVA_IGNITE_TICKS);
        burn.ignite_for_ticks(FIRE_IGNITE_TICKS);
        assert_eq!(
            burn.remaining(),
            LAVA_IGNITE_TICKS,
            "160 is less than 300, so the counter must not move"
        );

        // The control: a *longer* ignition does raise it.
        let mut short = BurnState::new();
        short.ignite_for_ticks(FIRE_IGNITE_TICKS);
        short.ignite_for_ticks(LAVA_IGNITE_TICKS);
        assert_eq!(short.remaining(), LAVA_IGNITE_TICKS);
    }

    /// A **fire-immune** entity type clears the fire and takes nothing — neither the
    /// contact hit nor the burn tick.
    #[test]
    fn a_fire_immune_entity_takes_nothing_and_stops_burning() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(FIRE_IGNITE_TICKS);
        let out = burn.tick(Some(BurnSource::Lava), true, false);
        assert_eq!(out.damage, 0.0, "immune to the contact hit too");
        assert_eq!(burn.remaining(), 0, "clearFire on a positive counter gives 0");
        assert!(!burn.is_on_fire());
    }

    /// **`clearFire` is `min(0, remaining)`, not `0`** — so a negative grace period
    /// survives being cleared. Zeroing it would let a fire-immune entity ignite a tick
    /// sooner than vanilla's on re-entry.
    #[test]
    fn clearing_fire_preserves_a_negative_grace_period() {
        let mut grace = BurnState::restored(-7);
        grace.clear();
        assert_eq!(grace.remaining(), -7, "min(0, -7) is -7, not 0");

        let mut burning = BurnState::new();
        burning.ignite_for_ticks(100);
        burning.clear();
        assert_eq!(burning.remaining(), 0, "min(0, 100) is 0");
    }

    /// **Fire Resistance refuses the damage and leaves the entity burning**, because
    /// vanilla's check is on the damage source (`source.is(IS_FIRE)`) rather than on
    /// the counter.
    ///
    /// Clearing the counter instead would put the fire out — visibly different, and it
    /// would also lose the remaining burn when the effect expires. Both halves are
    /// asserted: zero damage over a whole 160-tick burn, and the counter still running
    /// throughout.
    #[test]
    fn fire_resistance_refuses_the_damage_but_keeps_the_entity_burning() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(FIRE_IGNITE_TICKS);
        let mut total = 0.0;
        for _ in 0..80 {
            total += burn.tick(Some(BurnSource::Fire), false, true).damage;
        }
        assert_eq!(total, 0.0, "no fire damage of any kind reaches a resistant entity");
        assert!(
            burn.is_on_fire(),
            "but the counter keeps running, so the entity still visibly burns and the \
             burn resumes when the effect expires"
        );
        assert_eq!(burn.remaining(), FIRE_IGNITE_TICKS - 80);

        // The control: the same 80 ticks without resistance really do hurt, so the
        // assertion above is separating two reachable outcomes.
        let mut unprotected = BurnState::new();
        unprotected.ignite_for_ticks(FIRE_IGNITE_TICKS);
        let mut hurt = 0.0;
        for _ in 0..80 {
            hurt += unprotected.tick(Some(BurnSource::Fire), false, false).damage;
        }
        assert!(hurt > 0.0, "control: an unprotected entity takes real damage");
    }

    /// **A player ramps rather than igniting on first contact**, which is why running
    /// across one fire block can leave you unburnt.
    ///
    /// From a negative grace period the counter climbs by 1 per contact tick whoever
    /// the entity is; only once it is non-negative does the 8-second ignition land. A
    /// **non-player** at a non-negative counter skips the ramp entirely and ignites at
    /// once — the `else if` branch, and the discriminating case between the two.
    #[test]
    fn a_player_ramps_out_of_a_grace_period_before_igniting() {
        // Grace of -3: three contact ticks bring it to 0, and the third ignites.
        let mut player = BurnState::restored(-3);
        for expected in [-2, -1] {
            player.fire_ignite(true, 1);
            assert_eq!(player.remaining(), expected, "the grace burns off one per tick");
            assert!(!player.is_on_fire());
        }
        player.fire_ignite(true, 1);
        assert_eq!(
            player.remaining(),
            FIRE_IGNITE_TICKS,
            "reaching 0 triggers the full 8-second ignition"
        );

        // A player at 0 ramps by the draw first, then ignites — so the ignition still
        // lands on the first contact tick, but the ramp is what the draw is for.
        let mut fresh = BurnState::new();
        fresh.fire_ignite(true, 2);
        assert_eq!(fresh.remaining(), FIRE_IGNITE_TICKS);

        // **The `else if`**: a non-player at a negative counter still ramps, but at a
        // non-negative one it takes no ramp at all.
        let mut mob = BurnState::restored(-3);
        mob.fire_ignite(false, 2);
        assert_eq!(mob.remaining(), -2, "the negative branch is not player-gated");
        let mut mob_ready = BurnState::new();
        mob_ready.fire_ignite(false, 2);
        assert_eq!(
            mob_ready.remaining(),
            FIRE_IGNITE_TICKS,
            "a non-player skips the ramp and ignites immediately"
        );
    }

    /// The ramp draw is clamped to vanilla's `nextInt(1, 3)` range, so a caller passing
    /// a nonsense value cannot skip the grace period in one step.
    #[test]
    fn the_ramp_draw_is_clamped_to_one_or_two() {
        let mut low = BurnState::restored(-100);
        low.fire_ignite(true, -50);
        assert_eq!(low.remaining(), -99, "the negative branch adds exactly 1 regardless");

        let mut ready = BurnState::new();
        ready.fire_ignite(true, 9_999);
        // The clamp caps the ramp at 2, and then the ignition raises to 160 anyway —
        // so the observable is that the counter is exactly the ignition value, not
        // 9,999.
        assert_eq!(ready.remaining(), FIRE_IGNITE_TICKS);
    }

    /// `on_fire_changed` fires on ignition and on burn-out, and **not** on the ticks in
    /// between — otherwise a caller would restream metadata every tick of a burn.
    #[test]
    fn the_on_fire_flag_change_is_reported_only_at_the_edges() {
        let mut burn = BurnState::new();
        burn.ignite_for_ticks(3);
        // Ticks 1 and 2 leave the flag set.
        for _ in 0..2 {
            assert!(!burn.tick(None, false, false).on_fire_changed);
        }
        assert!(
            burn.tick(None, false, false).on_fire_changed,
            "the tick that takes the counter to 0 must report the change"
        );
        assert!(!burn.is_on_fire());
        // And a further tick is a clean no-op.
        assert!(burn.tick(None, false, false).is_empty());
    }

    /// `restored` clamps into `Short`'s range, because vanilla's `Fire` field is a
    /// `Short` and a wider value cannot have come from a real save.
    #[test]
    fn restored_clamps_into_the_short_range_the_nbt_field_uses() {
        assert_eq!(BurnState::restored(999_999).remaining(), i32::from(i16::MAX));
        assert_eq!(BurnState::restored(-999_999).remaining(), i32::from(i16::MIN));
        assert_eq!(BurnState::restored(160).remaining(), 160);
    }
}
