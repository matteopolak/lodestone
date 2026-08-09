//! The general server-side status-effect registry: duration countdown, amplifier
//! stacking, and the periodic damage/heal ticks.
//!
//! # What it is
//!
//! [`ActiveEffects`] is one entity's set of live effects — vanilla's
//! `LivingEntity.activeEffects`, a map from effect id to [`EffectInstance`]
//! (`MobEffectInstance`). One [`tick`](ActiveEffects::tick) call advances every
//! instance and reports what to do to health and hunger as an [`EffectTick`].
//!
//! **This is the registry `lodestone-physics::effect` is scoped *not* to be.** That
//! module's own doc says it classifies which effects the movement integrator reads
//! directly and which fold into `MOVEMENT_SPEED`; it holds no duration, no amplifier
//! stacking and no periodic tick, because movement is all it claims. This module is
//! the general store, and the movement classifier becomes a *consumer* of it rather
//! than keeping its own state.
//!
//! Nothing applied an effect before this existed. `crate::brewing` knew the effect
//! *names* a potion recipe produces and there was no state for one to land in.
//!
//! # How it works
//!
//! ## The periodic tick is a shift, and at high amplifiers it fires every tick
//!
//! Every periodic effect is `shouldApplyEffectTickThisTick(tickCount, amplifier)`
//! plus `applyEffectTick`. The interval is a **right shift by the amplifier**, and
//! the guard is the part that gets dropped:
//!
//! ```java
//! int interval = 25 >> amplification;
//! return interval > 0 ? tickCount % interval == 0 : true;
//! ```
//!
//! So once the shift reaches zero the effect applies **every tick**, not never. For
//! poison (`25`) that is amplifier 5; for wither (`40`) amplifier 6; for
//! regeneration (`50`) amplifier 6. An implementation that computed `tickCount %
//! interval` without the `interval > 0` guard divides by zero; one that returned
//! `false` there makes Poison VI *harmless*, which is the failure that looks like a
//! sensible clamp.
//!
//! | effect | base interval | damage / heal | health guard |
//! |---|---|---|---|
//! | `poison` | 25 | 1.0 magic | **only if `health > 1.0`** |
//! | `wither` | 40 | 1.0 wither | **none — wither can kill** |
//! | `regeneration` | 50 | heal 1.0 | only if hurt |
//! | `hunger` | every tick | `0.005 * (amplifier + 1)` exhaustion | — |
//! | `instant_health` | instantaneous | heal `4 << amplifier` | — |
//! | `instant_damage` | instantaneous | `6 << amplifier` magic | — |
//!
//! **Poison cannot kill and wither can**, and the asymmetry is one `if` in vanilla's
//! source. Giving poison a health guard it does not have makes it survivable when it
//! should not be; giving wither one it does not have makes it never lethal. Both are
//! plausible, and neither is what a direction-only test would notice.
//!
//! ## `tickCount` is the *remaining duration*, not an age
//!
//! `tickServer` passes `this.duration` for a finite effect and `target.tickCount`
//! for an infinite one. So the modulo counts **down**, and the ticks a periodic
//! effect fires on are decided by the duration remaining. A 200-tick poison fires at
//! remaining 200, 175, 150, … — eight hits — and an implementation counting *up*
//! from zero fires at a different set of ticks whenever the duration is not a
//! multiple of the interval.
//!
//! ## Stacking has a hidden-effect chain, and that is the whole rule
//!
//! `MobEffectInstance.update`, on applying a new instance of an effect already
//! present:
//!
//! | new vs current | result |
//! |---|---|
//! | **higher** amplifier | takes over. If the newcomer is *shorter*, the current one is **pushed onto a hidden chain** and resurfaces when the newcomer expires |
//! | **equal** amplifier, longer duration | duration is replaced |
//! | **equal** amplifier, shorter duration | ignored |
//! | **lower** amplifier, longer duration | becomes a hidden effect *behind* the current one |
//! | **lower** amplifier, shorter duration | ignored entirely |
//!
//! So the answer to "does a new application of a lower amplifier get ignored or
//! replace" is **neither**: it is remembered, and it comes back. Drinking Strength II
//! then Strength I leaves you with Strength II now and Strength I afterwards. A
//! registry that only kept the strongest instance loses the tail; one that only kept
//! the newest loses the strength.
//!
//! An infinite duration (`-1`) is longer than everything, which is why
//! `isShorterDurationThan` is written as
//! `!this.isInfiniteDuration() && (this.duration < other.duration || other.isInfiniteDuration())`
//! rather than as a plain comparison.
//!
//! # What is deliberately not here
//!
//! * **Attribute-modifier effects** (`speed`, `slowness`, `health_boost`,
//!   `absorption`). Those need an attribute system; `lodestone_physics::effect`
//!   already classifies the movement ones, and this module's job is to be the store
//!   it reads from rather than to duplicate its table.
//! * **Area-effect clouds and lingering-potion colour mixing.** That needs an entity
//!   with a radius and a per-tick membership test; there is no cloud entity.
//! * **`ambient` / `visible` / `showIcon`** and the `blendState`. All three are
//!   purely presentational and travel in the `update_mob_effect` packet, which
//!   nothing encodes.
//!
//! # How to change it
//!
//! * **Another periodic effect**: add it to [`periodic_effect`]. The interval and the
//!   amount both come from its own `MobEffect` subclass; do not derive one from
//!   another.
//! * **A consumer**: `crate::server`'s vitals tick calls
//!   [`ActiveEffects::tick`] and applies the [`EffectTick`]. Everything else should
//!   *read* [`ActiveEffects::amplifier_of`] rather than keep its own copy.
//!
//! # Dependencies
//!
//! None beyond `std`. No world access, no RNG, no clock — the caller supplies the
//! entity's own tick count for the infinite-duration case.

use std::collections::BTreeMap;

/// Vanilla's sentinel for an infinite effect (`MobEffectInstance.isInfiniteDuration`
/// tests `duration == -1`).
pub const INFINITE_DURATION: i32 = -1;

/// `PoisonMobEffect.DAMAGE_INTERVAL`.
pub const POISON_INTERVAL: i32 = 25;

/// `WitherMobEffect.DAMAGE_INTERVAL`.
pub const WITHER_INTERVAL: i32 = 40;

/// `RegenerationMobEffect`'s interval — a literal `50` in its
/// `shouldApplyEffectTickThisTick`, with no named constant beside it.
pub const REGENERATION_INTERVAL: i32 = 50;

/// What a periodic effect does when its interval comes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicAction {
    /// `1.0` damage, refused when health is at or below `1.0` — poison's own guard,
    /// which is why poison cannot kill.
    PoisonDamage,
    /// `1.0` damage with **no** health guard. Wither can kill.
    WitherDamage,
    /// Heal `1.0`, only when hurt.
    Regenerate,
    /// `0.005 * (amplifier + 1)` hunger exhaustion, every tick.
    Exhaust,
}

/// The `(base interval, action)` for an effect id, or `None` for one this module does
/// not tick.
///
/// **Base** interval: the effective one is `base >> amplifier`, and zero means every
/// tick — see this module's doc. `hunger` has an interval of `1` because its
/// `shouldApplyEffectTickThisTick` is a bare `return true`, which `1 >> anything` also
/// gives; encoding it as `1` keeps one code path rather than a special case.
#[must_use]
pub fn periodic_effect(effect_id: &str) -> Option<(i32, PeriodicAction)> {
    // Accepts a bare path as well as a namespaced id, matching how
    // `lodestone_physics::effect::classify` reads its input.
    let path = effect_id.strip_prefix("minecraft:").unwrap_or(effect_id);
    match path {
        "poison" => Some((POISON_INTERVAL, PeriodicAction::PoisonDamage)),
        "wither" => Some((WITHER_INTERVAL, PeriodicAction::WitherDamage)),
        "regeneration" => Some((REGENERATION_INTERVAL, PeriodicAction::Regenerate)),
        "hunger" => Some((1, PeriodicAction::Exhaust)),
        _ => None,
    }
}

/// Whether a periodic effect fires on this tick — `shouldApplyEffectTickThisTick`,
/// including the `interval > 0` guard that makes a high amplifier fire *every* tick
/// rather than never.
///
/// `tick_count` is the **remaining duration** for a finite effect (vanilla passes
/// `this.duration`) or the entity's own tick count for an infinite one.
#[must_use]
pub fn should_apply_this_tick(base_interval: i32, amplifier: u32, tick_count: i32) -> bool {
    // `>>` in Java on an int is a shift by `amplification & 31`; an amplifier past 31
    // is not reachable from any real source, but saturating at zero is the honest
    // Rust equivalent and avoids a shift-overflow panic in debug builds.
    let interval = if amplifier >= 31 {
        0
    } else {
        base_interval >> amplifier
    };
    if interval > 0 {
        tick_count % interval == 0
    } else {
        true
    }
}

/// The instant-health heal for an amplifier — `HealOrHarmMobEffect`'s
/// `4 << amplification`.
#[must_use]
pub fn instant_health_amount(amplifier: u32) -> f32 {
    (4i32 << amplifier.min(24)) as f32
}

/// The instant-damage hit for an amplifier — `6 << amplification`.
///
/// Note this is **6**, not 4: harming hits harder than healing heals at the same
/// amplifier, which is easy to lose by factoring the two into one function.
#[must_use]
pub fn instant_damage_amount(amplifier: u32) -> f32 {
    (6i32 << amplifier.min(24)) as f32
}

/// One live effect — vanilla's `MobEffectInstance`, reduced to the fields that decide
/// behaviour (see the module doc for the presentational ones that are omitted).
///
/// `hidden` is the chain `update` builds: a weaker-but-longer application is stored
/// behind the active one and resurfaces when it expires. `Box` because the chain is
/// recursive and can be more than one deep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectInstance {
    duration: i32,
    amplifier: u32,
    hidden: Option<Box<EffectInstance>>,
}

impl EffectInstance {
    /// A fresh instance. `duration` of [`INFINITE_DURATION`] is vanilla's infinite.
    #[must_use]
    pub fn new(duration: i32, amplifier: u32) -> Self {
        Self {
            duration,
            amplifier,
            hidden: None,
        }
    }

    /// Remaining duration in ticks, or [`INFINITE_DURATION`].
    #[must_use]
    pub fn duration(&self) -> i32 {
        self.duration
    }

    /// The amplifier — `0` is level I, `1` is level II.
    #[must_use]
    pub fn amplifier(&self) -> u32 {
        self.amplifier
    }

    /// `isInfiniteDuration`.
    #[must_use]
    pub fn is_infinite(&self) -> bool {
        self.duration == INFINITE_DURATION
    }

    /// Whether a hidden instance is queued behind this one.
    #[must_use]
    pub fn has_hidden(&self) -> bool {
        self.hidden.is_some()
    }

    /// `hasRemainingDuration` — infinite, or strictly positive.
    #[must_use]
    fn has_remaining(&self) -> bool {
        self.is_infinite() || self.duration > 0
    }

    /// `isShorterDurationThan`, and it is **not** a plain comparison: an infinite
    /// duration is longer than every finite one, and nothing is shorter than an
    /// infinite instance.
    fn is_shorter_than(&self, other: &Self) -> bool {
        !self.is_infinite() && (self.duration < other.duration || other.is_infinite())
    }

    /// `MobEffectInstance.update` — the stacking rule, transcribed. Returns whether
    /// anything the client would need told about changed.
    ///
    /// See this module's doc for the five-row table this implements, and in particular
    /// for why a **lower** amplifier is neither ignored nor applied but *remembered*.
    pub fn update(&mut self, take_over: &Self) -> bool {
        let mut changed = false;
        if take_over.amplifier > self.amplifier {
            if take_over.is_shorter_than(self) {
                // The stronger newcomer runs out first, so the current instance is
                // pushed down the chain and comes back afterwards. Dropping this is
                // what makes "Strength II then Strength I" lose the tail.
                let previous_hidden = self.hidden.take();
                let mut demoted = Self::new(self.duration, self.amplifier);
                demoted.hidden = previous_hidden;
                self.hidden = Some(Box::new(demoted));
            }
            self.amplifier = take_over.amplifier;
            self.duration = take_over.duration;
            changed = true;
        } else if self.is_shorter_than(take_over) {
            if take_over.amplifier == self.amplifier {
                self.duration = take_over.duration;
                changed = true;
            } else if self.hidden.is_none() {
                self.hidden = Some(Box::new(take_over.clone()));
            } else {
                // Recurse: the chain is ordered, so a second weaker application slots
                // in behind the first.
                self.hidden.as_mut().expect("checked above").update(take_over);
            }
        }
        changed
    }

    /// `tickDownDuration` — decrements this instance **and** every hidden one, so a
    /// queued effect's own clock runs while it waits. A chain that only ticked the
    /// visible instance would resurface a weaker effect at its full original
    /// duration.
    fn tick_down(&mut self) {
        if let Some(hidden) = self.hidden.as_mut() {
            hidden.tick_down();
        }
        if !self.is_infinite() && self.duration != 0 {
            self.duration -= 1;
        }
    }

    /// `downgradeToHiddenEffect` — when this instance hits exactly zero and something
    /// is queued behind it, that one becomes active.
    fn downgrade(&mut self) -> bool {
        if self.duration == 0 && self.hidden.is_some() {
            let hidden = self.hidden.take().expect("checked above");
            self.duration = hidden.duration;
            self.amplifier = hidden.amplifier;
            self.hidden = hidden.hidden;
            true
        } else {
            false
        }
    }
}

/// What one [`ActiveEffects::tick`] decided, for the caller to apply to health and
/// hunger.
///
/// Accumulated across every active effect in one tick rather than reported per
/// effect, because that is how the caller will use it — and because poison and wither
/// firing on the same tick really do both land.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EffectTick {
    /// Total healing to apply (regeneration, and any instant health).
    pub heal: f32,
    /// Damage that respects poison's `health > 1.0` guard — the caller must apply it
    /// through the same guard, or as a hit that cannot reduce health below `1.0`.
    pub poison_damage: f32,
    /// Damage with no guard. Wither can kill.
    pub wither_damage: f32,
    /// Hunger exhaustion to charge.
    pub exhaustion: f32,
    /// `true` when an effect expired or a hidden one surfaced, so the caller knows
    /// the client's effect list is stale.
    pub list_changed: bool,
}

impl EffectTick {
    /// Whether this tick produced anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heal == 0.0
            && self.poison_damage == 0.0
            && self.wither_damage == 0.0
            && self.exhaustion == 0.0
            && !self.list_changed
    }
}

/// One entity's live effects — vanilla's `LivingEntity.activeEffects`.
///
/// A `BTreeMap` rather than a `HashMap` so iteration order is stable: several effects
/// can fire on the same tick, and a caller reporting them (or a gate asserting them)
/// should not depend on hash order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActiveEffects(BTreeMap<String, EffectInstance>);

impl ActiveEffects {
    /// No effects.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any effect is active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many distinct effects are active (a hidden instance does not count — it is
    /// not active).
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The active instance for `effect_id`, if any.
    #[must_use]
    pub fn get(&self, effect_id: &str) -> Option<&EffectInstance> {
        self.0.get(effect_id)
    }

    /// The active amplifier for `effect_id`, or `None` if it is not present.
    ///
    /// **This is what every other consumer should read**, rather than keeping its own
    /// copy — `lodestone_physics::effect::classify` takes exactly an id and an
    /// amplifier, so it composes directly.
    #[must_use]
    pub fn amplifier_of(&self, effect_id: &str) -> Option<u32> {
        self.0.get(effect_id).map(EffectInstance::amplifier)
    }

    /// Every active `(id, amplifier)`, in stable order — for handing the movement
    /// classifier the whole set.
    #[must_use]
    pub fn active(&self) -> Vec<(&str, u32)> {
        self.0
            .iter()
            .map(|(id, instance)| (id.as_str(), instance.amplifier))
            .collect()
    }

    /// Applies an effect — `LivingEntity.addEffect`, which forwards to
    /// [`EffectInstance::update`] when one is already present.
    ///
    /// Returns `true` when something changed, matching `addEffect`'s own return so a
    /// caller can decide whether to send an update packet.
    pub fn apply(&mut self, effect_id: &str, duration: i32, amplifier: u32) -> bool {
        let incoming = EffectInstance::new(duration, amplifier);
        match self.0.get_mut(effect_id) {
            Some(existing) => existing.update(&incoming),
            None => {
                self.0.insert(effect_id.to_owned(), incoming);
                true
            }
        }
    }

    /// Removes an effect outright, hidden chain and all — a milk bucket, or
    /// `/effect clear`. Returns whether anything was there.
    pub fn remove(&mut self, effect_id: &str) -> bool {
        self.0.remove(effect_id).is_some()
    }

    /// Removes every effect.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Advances every effect by one tick — `LivingEntity.tickEffects` over
    /// `MobEffectInstance.tickServer`.
    ///
    /// `entity_tick_count` is the entity's own age, used **only** as the modulo input
    /// for an infinite effect (vanilla's `target.tickCount`); a finite effect counts
    /// against its own remaining duration instead. Getting that backwards makes a
    /// finite poison fire on the wrong ticks whenever its duration is not a multiple
    /// of 25.
    ///
    /// `health` and `max_health` gate poison's `health > 1.0` and regeneration's
    /// `health < max_health` — vanilla's guards live inside `applyEffectTick`, so they
    /// are consulted here rather than left to the caller. The caller still applies the
    /// numbers.
    pub fn tick(&mut self, entity_tick_count: i32, health: f32, max_health: f32) -> EffectTick {
        let mut out = EffectTick::default();
        let mut expired: Vec<String> = Vec::new();

        for (id, instance) in &mut self.0 {
            if !instance.has_remaining() {
                expired.push(id.clone());
                continue;
            }
            let tick_count = if instance.is_infinite() {
                entity_tick_count
            } else {
                instance.duration
            };
            if let Some((base, action)) = periodic_effect(id)
                && should_apply_this_tick(base, instance.amplifier, tick_count)
            {
                match action {
                    // Poison's own guard: `if (mob.getHealth() > 1.0F)`. Poison
                    // cannot kill, and this is the one `if` that says so.
                    PeriodicAction::PoisonDamage => {
                        if health > 1.0 {
                            out.poison_damage += 1.0;
                        }
                    }
                    // No guard at all — see the module doc's asymmetry note.
                    PeriodicAction::WitherDamage => out.wither_damage += 1.0,
                    PeriodicAction::Regenerate => {
                        if health < max_health {
                            out.heal += 1.0;
                        }
                    }
                    PeriodicAction::Exhaust => {
                        out.exhaustion += 0.005 * (instance.amplifier + 1) as f32;
                    }
                }
            }
            instance.tick_down();
            if instance.downgrade() {
                out.list_changed = true;
            }
            if !instance.has_remaining() {
                expired.push(id.clone());
            }
        }

        for id in expired {
            self.0.remove(&id);
            out.list_changed = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the interval shift, and the guard that makes it fire every tick ----

    /// **The `interval > 0` guard**, which is the single easiest thing to drop here.
    ///
    /// Poison's base interval is 25, so `25 >> amplifier` is `25, 12, 6, 3, 1, 0`.
    /// At amplifier 5 the interval reaches **zero**, and vanilla's ternary makes that
    /// mean **every tick** — Poison VI is the fastest poison, not a harmless one.
    ///
    /// The two hypotheses, both reachable:
    ///
    /// | hypothesis | hits in 100 ticks at amplifier 5 |
    /// |---|---|
    /// | `interval > 0 ? … : true` (correct) | **100** |
    /// | returning `false` at zero | **0** |
    ///
    /// Both are asserted, the wrong one negatively.
    #[test]
    fn a_zero_interval_fires_every_tick_rather_than_never() {
        // The shift ladder, from the record definition.
        assert_eq!(POISON_INTERVAL >> 0, 25);
        assert_eq!(POISON_INTERVAL >> 4, 1);
        assert_eq!(POISON_INTERVAL >> 5, 0, "amplifier 5 is where the shift bottoms out");

        let hits = |amplifier: u32| {
            (1..=100)
                .filter(|&t| should_apply_this_tick(POISON_INTERVAL, amplifier, t))
                .count()
        };
        assert_eq!(hits(5), 100, "a zero interval fires every tick");
        assert_ne!(hits(5), 0, "returning false at zero would make Poison VI harmless");
        // And the ladder in between is exactly the count the interval predicts.
        assert_eq!(hits(0), 4, "100 / 25");
        assert_eq!(hits(1), 8, "100 / 12, floored — 12, 24, …, 96");
        assert_eq!(hits(4), 100, "interval 1 also fires every tick");
        // An absurd amplifier must not panic on the shift.
        assert!(should_apply_this_tick(POISON_INTERVAL, 99, 7));
    }

    /// The three base intervals are three different numbers, so an implementation
    /// that reused one constant fails here. Read off each effect's own class.
    #[test]
    fn the_three_periodic_intervals_are_distinct_and_from_their_own_classes() {
        assert_eq!(periodic_effect("minecraft:poison"), Some((25, PeriodicAction::PoisonDamage)));
        assert_eq!(periodic_effect("minecraft:wither"), Some((40, PeriodicAction::WitherDamage)));
        assert_eq!(
            periodic_effect("minecraft:regeneration"),
            Some((50, PeriodicAction::Regenerate))
        );
        assert_eq!(periodic_effect("minecraft:hunger"), Some((1, PeriodicAction::Exhaust)));
        // A bare path works too, matching the movement classifier's input handling.
        assert_eq!(periodic_effect("poison"), Some((25, PeriodicAction::PoisonDamage)));
        // Not every effect is periodic — an attribute effect must not be ticked here.
        assert_eq!(periodic_effect("minecraft:speed"), None);
        assert_eq!(periodic_effect("minecraft:night_vision"), None);
        assert_eq!(periodic_effect("not_an_effect"), None);
    }

    // ---- poison cannot kill, wither can ----

    /// **The asymmetry that is one `if` in vanilla.** Poison refuses to fire at or
    /// below `1.0` health; wither has no such guard.
    ///
    /// Asserted at the exact boundary in both directions, because the interesting
    /// value is `1.0` itself: `health > 1.0` is strict, so `1.0` is already safe.
    #[test]
    fn poison_stops_at_one_health_and_wither_does_not() {
        let poison_at = |health: f32| {
            let mut effects = ActiveEffects::new();
            effects.apply("minecraft:poison", 100, 0);
            // Duration 100 is a multiple of 25, so the very first tick fires.
            effects.tick(0, health, 20.0).poison_damage
        };
        assert_eq!(poison_at(1.5), 1.0, "above 1.0 health, poison lands");
        assert_eq!(poison_at(1.0), 0.0, "the guard is strictly greater, so 1.0 is safe");
        assert_eq!(poison_at(0.5), 0.0);

        let wither_at = |health: f32| {
            let mut effects = ActiveEffects::new();
            effects.apply("minecraft:wither", 80, 0);
            effects.tick(0, health, 20.0).wither_damage
        };
        assert_eq!(wither_at(1.5), 1.0);
        assert_eq!(
            wither_at(0.5),
            1.0,
            "wither has no health guard at all — it is what makes it lethal"
        );
    }

    /// Regeneration only heals when hurt — the `health < maxHealth` guard.
    #[test]
    fn regeneration_only_fires_when_hurt() {
        let heal_at = |health: f32| {
            let mut effects = ActiveEffects::new();
            effects.apply("minecraft:regeneration", 100, 0);
            // 100 is a multiple of 50, so tick one fires.
            effects.tick(0, health, 20.0).heal
        };
        assert_eq!(heal_at(10.0), 1.0);
        assert_eq!(heal_at(20.0), 0.0, "a full-health entity must not be healed");
    }

    // ---- the modulo counts DOWN ----

    /// **`tickCount` is the remaining duration**, so the modulo counts down. A
    /// 200-tick poison fires on the ticks where the *remaining* duration is a multiple
    /// of 25 — 200, 175, …, 25 — which is **eight** hits.
    ///
    /// The discriminating case is a duration that is *not* a multiple of the interval.
    /// A 210-tick poison counting down fires at remaining 200, 175, …, 25: still
    /// eight, but the first hit is on tick **11**, not tick 1. An implementation
    /// counting up from zero fires on tick 1 (`0 % 25 == 0`) and lands on a different
    /// set of ticks entirely. Both the count and the first tick are asserted.
    #[test]
    fn the_periodic_modulo_counts_down_from_the_remaining_duration() {
        let run = |duration: i32| {
            let mut effects = ActiveEffects::new();
            effects.apply("minecraft:poison", duration, 0);
            let mut hits = Vec::new();
            for t in 1..=duration {
                if effects.tick(t, 20.0, 20.0).poison_damage > 0.0 {
                    hits.push(t);
                }
            }
            hits
        };
        let exact = run(200);
        assert_eq!(exact.len(), 8, "200 / 25 hits: {exact:?}");
        assert_eq!(exact[0], 1, "at duration 200, remaining is 200 on the first tick");

        let offset = run(210);
        assert_eq!(
            offset[0], 11,
            "at duration 210 the first multiple of 25 is remaining 200, i.e. tick 11 — \
             counting UP would fire on tick 1"
        );
        assert_eq!(offset.len(), 8, "still eight hits: {offset:?}");
    }

    /// An effect expires on the tick its duration reaches zero — **not** the tick
    /// after.
    ///
    /// This gate was written wrong first, predicting a fourth tick. `tickServer`
    /// returns `hasRemainingDuration()` *after* `tickDownDuration`, and
    /// `LivingEntity.tickEffects` removes the effect when that is false — so a
    /// 3-tick effect is gone at the end of tick **3**, having applied its last
    /// periodic hit on tick 3 and not on 4. An off-by-one here gives every effect in
    /// the game one extra tick of life.
    #[test]
    fn an_effect_expires_on_the_tick_its_duration_reaches_zero() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:poison", 3, 0);

        for t in 1..=2 {
            let out = effects.tick(0, 20.0, 20.0);
            assert!(!out.list_changed, "still running at tick {t}");
            assert_eq!(effects.len(), 1);
        }
        let out = effects.tick(0, 20.0, 20.0);
        assert!(
            out.list_changed,
            "the third tick takes the duration to 0, which is when it is removed"
        );
        assert!(effects.is_empty(), "not present for a fourth tick");

        // A further tick is a clean no-op rather than a second expiry report.
        assert!(effects.tick(0, 20.0, 20.0).is_empty());
    }

    /// An infinite effect never expires, and its modulo uses the **entity's** tick
    /// count — the only place `entity_tick_count` is read. Passing the duration there
    /// instead would make an infinite effect fire never (`-1 % 25 != 0`) or always.
    #[test]
    fn an_infinite_effect_never_expires_and_uses_the_entity_tick_count() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:poison", INFINITE_DURATION, 0);
        let mut hits = 0;
        for t in 1..=100 {
            if effects.tick(t, 20.0, 20.0).poison_damage > 0.0 {
                hits += 1;
            }
        }
        assert_eq!(hits, 4, "the entity tick count 1..=100 crosses 25, 50, 75, 100");
        assert_eq!(effects.len(), 1, "infinite means infinite");
        assert!(effects.get("minecraft:poison").expect("present").is_infinite());
    }

    // ---- stacking ----

    /// **The question the issue asks — "does a new application of a lower amplifier
    /// get ignored or replace" — has a third answer: it is remembered.**
    ///
    /// Strength II for 100 ticks, then Strength I for 400. The active effect stays
    /// amplifier 1, and when it expires amplifier 0 **surfaces** with the remainder of
    /// its own clock (400 - 100 = 300 ticks).
    ///
    /// A registry keeping only the strongest instance loses the tail; one keeping only
    /// the newest loses the strength. Both are asserted against.
    #[test]
    fn a_weaker_longer_application_is_remembered_and_resurfaces() {
        let mut effects = ActiveEffects::new();
        assert!(effects.apply("minecraft:strength", 100, 1));
        // The weaker-but-longer application is not "changed" from the client's point
        // of view — the active instance is untouched.
        assert!(!effects.apply("minecraft:strength", 400, 0));

        let active = effects.get("minecraft:strength").expect("present");
        assert_eq!(active.amplifier(), 1, "the stronger effect stays active");
        assert_eq!(active.duration(), 100);
        assert!(active.has_hidden(), "and the weaker one is queued behind it");

        // Run out the stronger one. The hidden instance surfaces on the tick the
        // active duration hits zero.
        let mut surfaced_at = None;
        for t in 1..=101 {
            if effects.tick(0, 20.0, 20.0).list_changed {
                surfaced_at = Some(t);
                break;
            }
        }
        assert_eq!(surfaced_at, Some(100), "the downgrade happens as duration reaches 0");

        let active = effects.get("minecraft:strength").expect("still present");
        assert_eq!(active.amplifier(), 0, "the weaker effect is now active");
        assert_eq!(
            active.duration(),
            300,
            "and its OWN clock ran while it waited: 400 - 100. A chain that only \
             ticked the visible instance would surface it at 400"
        );
    }

    /// A **stronger but shorter** application pushes the current one onto the chain,
    /// so the longer weak effect comes back — the mirror image of the case above, and
    /// vanilla's `takeOver.isShorterDurationThan(this)` branch.
    #[test]
    fn a_stronger_shorter_application_demotes_the_current_one() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:strength", 400, 0);
        assert!(effects.apply("minecraft:strength", 100, 1), "a stronger effect changes");

        let active = effects.get("minecraft:strength").expect("present");
        assert_eq!(active.amplifier(), 1);
        assert_eq!(active.duration(), 100);
        assert!(active.has_hidden(), "the long weak effect was demoted, not discarded");

        for _ in 0..100 {
            effects.tick(0, 20.0, 20.0);
        }
        let active = effects.get("minecraft:strength").expect("present");
        assert_eq!(active.amplifier(), 0);
        assert_eq!(active.duration(), 300);
    }

    /// A **stronger and longer** application simply takes over with nothing queued —
    /// the control that proves the demotion above is conditional on the duration
    /// rather than unconditional.
    #[test]
    fn a_stronger_longer_application_leaves_nothing_behind() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:strength", 100, 0);
        assert!(effects.apply("minecraft:strength", 400, 1));
        let active = effects.get("minecraft:strength").expect("present");
        assert_eq!(active.amplifier(), 1);
        assert_eq!(active.duration(), 400);
        assert!(
            !active.has_hidden(),
            "the old instance would have expired first anyway, so vanilla drops it"
        );
    }

    /// Equal amplifier: a longer duration replaces, a shorter one is **ignored**.
    /// Without the second half, re-drinking a weak potion would shorten your effect.
    #[test]
    fn an_equal_amplifier_takes_the_longer_duration_and_ignores_the_shorter() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:poison", 200, 1);
        assert!(effects.apply("minecraft:poison", 500, 1), "longer replaces");
        assert_eq!(effects.get("minecraft:poison").expect("present").duration(), 500);

        assert!(!effects.apply("minecraft:poison", 50, 1), "shorter is ignored");
        assert_eq!(
            effects.get("minecraft:poison").expect("present").duration(),
            500,
            "a shorter re-application must not cut the effect short"
        );
    }

    /// A **weaker and shorter** application is ignored entirely — no change, no hidden
    /// instance. The fifth row of the stacking table, and the one an implementation
    /// that queued every weaker application would get wrong.
    #[test]
    fn a_weaker_shorter_application_is_ignored_entirely() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:strength", 400, 2);
        assert!(!effects.apply("minecraft:strength", 50, 0));
        let active = effects.get("minecraft:strength").expect("present");
        assert_eq!(active.amplifier(), 2);
        assert_eq!(active.duration(), 400);
        assert!(!active.has_hidden(), "nothing worth remembering was offered");
    }

    /// An infinite duration is longer than every finite one, which is why
    /// `isShorterDurationThan` is not a plain comparison. An infinite weak effect
    /// behind a finite strong one must be remembered.
    #[test]
    fn an_infinite_duration_is_longer_than_every_finite_one() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:strength", 100, 1);
        effects.apply("minecraft:strength", INFINITE_DURATION, 0);
        assert!(
            effects.get("minecraft:strength").expect("present").has_hidden(),
            "an infinite weak effect is longer, so it queues behind the strong one"
        );

        // And nothing displaces an infinite instance by duration alone.
        let mut forever = ActiveEffects::new();
        forever.apply("minecraft:poison", INFINITE_DURATION, 0);
        assert!(
            !forever.apply("minecraft:poison", 1_000_000, 0),
            "no finite duration is longer than infinite"
        );
        assert!(forever.get("minecraft:poison").expect("present").is_infinite());
    }

    // ---- instant effects ----

    /// Instant damage is `6 << amplifier` and instant health is `4 << amplifier` —
    /// **different constants**, which is what factoring the two into one function
    /// loses.
    #[test]
    fn instant_damage_hits_harder_than_instant_health_heals() {
        assert_eq!(instant_health_amount(0), 4.0);
        assert_eq!(instant_health_amount(1), 8.0);
        assert_eq!(instant_health_amount(2), 16.0);
        assert_eq!(instant_damage_amount(0), 6.0);
        assert_eq!(instant_damage_amount(1), 12.0);
        assert_ne!(
            instant_damage_amount(1),
            instant_health_amount(1),
            "6 << 1 and 4 << 1 are different numbers"
        );
    }

    // ---- the registry as a whole ----

    /// Several effects tick together and accumulate independently — poison and wither
    /// on the same tick both land, which the `if/else` shape of a single-effect model
    /// would miss.
    #[test]
    fn independent_effects_accumulate_in_one_tick() {
        let mut effects = ActiveEffects::new();
        // Durations chosen so both are multiples of their own intervals at tick one:
        // 200 % 25 == 0 and 200 % 40 == 0.
        effects.apply("minecraft:poison", 200, 0);
        effects.apply("minecraft:wither", 200, 0);
        effects.apply("minecraft:regeneration", 200, 0);
        assert_eq!(effects.len(), 3);

        let out = effects.tick(0, 10.0, 20.0);
        assert_eq!(out.poison_damage, 1.0);
        assert_eq!(out.wither_damage, 1.0);
        assert_eq!(out.heal, 1.0, "200 % 50 == 0, so regeneration fires too");
    }

    /// Hunger charges exhaustion every tick, scaled by the amplifier — the one
    /// periodic effect with no interval.
    #[test]
    fn hunger_charges_exhaustion_every_tick_scaled_by_amplifier() {
        let total = |amplifier: u32| {
            let mut effects = ActiveEffects::new();
            effects.apply("minecraft:hunger", 100, amplifier);
            let mut sum = 0.0;
            for _ in 0..100 {
                sum += effects.tick(0, 20.0, 20.0).exhaustion;
            }
            sum
        };
        // 100 ticks * 0.005 = 0.5 at amplifier 0, doubled at amplifier 1.
        assert!((total(0) - 0.5).abs() < 1e-4, "got {}", total(0));
        assert!((total(1) - 1.0).abs() < 1e-4, "got {}", total(1));
    }

    /// **Control**: an empty registry ticks to nothing, and an effect this module does
    /// not tick (an attribute effect) contributes nothing either while still being
    /// *present* for a consumer to read. Without this, a registry that fired for every
    /// id would pass every gate above.
    #[test]
    fn an_unticked_effect_is_present_but_contributes_nothing() {
        let mut empty = ActiveEffects::new();
        assert!(empty.tick(0, 10.0, 20.0).is_empty());

        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:speed", 400, 2);
        let out = effects.tick(0, 10.0, 20.0);
        assert_eq!(out.poison_damage, 0.0);
        assert_eq!(out.heal, 0.0);
        assert_eq!(out.exhaustion, 0.0);
        // But it is readable, which is the point of being the shared store.
        assert_eq!(effects.amplifier_of("minecraft:speed"), Some(2));
        assert_eq!(effects.amplifier_of("minecraft:slowness"), None);
        assert_eq!(effects.active(), vec![("minecraft:speed", 2)]);
    }

    /// `remove` takes the hidden chain with it — a milk bucket clears the effect, not
    /// just its strongest instance.
    #[test]
    fn removing_an_effect_takes_its_hidden_chain_too() {
        let mut effects = ActiveEffects::new();
        effects.apply("minecraft:strength", 100, 1);
        effects.apply("minecraft:strength", 400, 0);
        assert!(effects.remove("minecraft:strength"));
        assert!(effects.is_empty(), "the queued weak instance must go too");
        assert!(!effects.remove("minecraft:strength"), "and removing twice is a no-op");
    }
}
