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
//! Every periodic effect's real tick gate computes an interval and asks
//! whether the tick count is a multiple of it. The interval is a **right shift
//! by the amplifier**, and the guard is the part that gets dropped: with a
//! base interval of `25`, the real interval is `25 >> amplifier`, and the tick
//! fires when that interval is positive **and** the tick count is divisible by
//! it, or unconditionally — every tick — once the interval has shifted down to
//! zero.
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
//! ## A splash/lingering potion's impact-time burst is [`potion_splash_effects`]
//!
//! `ThrownSplashPotion.onHitAsPotion` splits a potion's built-in effect list by
//! instant-vs-timed and scales each by distance from the impact —
//! [`splash_scale`] is `1.0 - sqrt(dist) / 4.0`, and an instant effect
//! ([`effect_is_instantaneous`] — only `instant_health`/`instant_damage` are
//! reachable from a potion's own list) is scaled through
//! [`splash_instant_amount`] while a timed one is scaled through
//! [`splash_timed_duration`] and then dropped outright by
//! [`splash_would_be_dropped`] if that leaves it `endsWithin(20)` ticks.
//! [`potion_splash_effects`] runs the whole thing for one entity already known
//! to be in range; `crate::mobs::projectiles::resolve_potion_splash` is the
//! consumer that finds which entities that is and applies the result.
//!
//! This build's `ItemComponents` does not carry a thrown stack's
//! `customEffects` patch (only the potion's own built-in
//! `minecraft:potion_contents` `potion` id), so a `/give`-authored custom
//! effect on a splash potion is silently absent here — a declared gap, not a
//! silent wrong answer: [`potion_splash_effects`] returns exactly the potion's
//! *built-in* list, same as an unpatched stack would resolve to in vanilla.
//!
//! # What is deliberately not here
//!
//! * **Attribute-modifier effects** (`speed`, `slowness`, `health_boost`,
//!   `absorption`). Those need an attribute system; `lodestone_physics::effect`
//!   already classifies the movement ones, and this module's job is to be the store
//!   it reads from rather than to duplicate its table.
//! * **A lingering potion's own `AreaEffectCloud` entity** — radius, a
//!   radius-per-tick shrink, a duration, and a reapplication delay, so the same
//!   burst lands repeatedly over up to 30 seconds rather than once. See
//!   `crate::mobs::projectiles::resolve_potion_splash`'s own doc for what a
//!   lingering potion does today instead of that (one splash-shaped burst at
//!   impact, exactly like a splash potion) — tracked as a follow-up rather than
//!   built here.
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
//! * **The splash formula**: [`potion_splash_effects`] is the one entry point;
//!   its pieces ([`splash_scale`], [`splash_instant_amount`],
//!   [`splash_timed_duration`], [`splash_would_be_dropped`]) are separated
//!   because each is independently testable against `AbstractThrownPotion`'s own
//!   named constant or `MobEffectInstance` method.
//!
//! # Dependencies
//!
//! No world access, no RNG, no clock — the caller supplies the entity's own
//! tick count for the infinite-duration case. [`potion_splash_effects`] alone
//! reads `lodestone_data::potion`/`lodestone_data::mob_effects` for the potion
//! registry's built-in effect list and mob-effect id resolution; nothing else in
//! this module depends on that crate.

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

// ---------------------------------------------------------------------------
// A splash/lingering potion's impact-time burst — vanilla's own
// splash-potion on-hit-as-potion routine.
// ---------------------------------------------------------------------------

/// `AbstractThrownPotion.SPLASH_RANGE` — a splash/lingering blast only reaches
/// an entity within four blocks of the impact point.
pub const SPLASH_RANGE: f64 = 4.0;

/// `AbstractThrownPotion.SPLASH_RANGE_SQ` — the squared form `onHitAsPotion`'s
/// own `dist < 16.0` guard actually compares against, since the distance it has
/// in hand is already squared.
pub const SPLASH_RANGE_SQ: f64 = 16.0;

/// `ThrownSplashPotion.onHitAsPotion`'s falloff: `1.0 - Math.sqrt(dist) / 4.0`,
/// where `distance_sq` is a squared distance already checked against
/// [`SPLASH_RANGE_SQ`] (this function does not gate on it).
///
/// `1.0` at a direct hit (`distance_sq == 0.0`) and `0.0` at the very edge of the
/// blast (`distance_sq == SPLASH_RANGE_SQ`) — the two extremes "no falloff at
/// all" and "the correct falloff" disagree at every point in between but happen
/// to agree at neither of these on their own; both ends are asserted in this
/// module's tests for exactly that reason.
#[must_use]
pub fn splash_scale(distance_sq: f64) -> f64 {
    1.0 - distance_sq.sqrt() / SPLASH_RANGE
}

/// Returns `true` for the two instantaneous potion effects in this build's
/// registry: `instant_health` and `instant_damage`. A bare path and a
/// namespaced id are both accepted, matching [`periodic_effect`].
#[must_use]
pub fn effect_is_instantaneous(effect_id: &str) -> bool {
    matches!(
        effect_id.strip_prefix("minecraft:").unwrap_or(effect_id),
        "instant_health" | "instant_damage"
    )
}

/// Computes an instantaneous splash amount as
/// `(scale * base_amount + 0.5).floor()`, where `base_amount` is
/// [`instant_health_amount`] or [`instant_damage_amount`] at the effect's
/// amplifier. Both operands are non-negative, so truncation is well-defined.
#[must_use]
pub fn splash_instant_amount(base_amount: f32, scale: f64) -> f32 {
    ((scale * f64::from(base_amount)) + 0.5).floor() as f32
}

/// Computes a timed-effect duration after splash falloff and the item's
/// `minecraft:potion_duration_scale`: `(scale * d * duration_scale + 0.5).floor()`.
/// Pass `1.0` for the default; this build does not model a separate duration
/// component, so ordinary splashes use the unscaled duration.
#[must_use]
pub fn splash_timed_duration(base_duration_ticks: u32, scale: f64, duration_scale: f32) -> i32 {
    ((scale * f64::from(base_duration_ticks) * f64::from(duration_scale)) + 0.5).floor() as i32
}

/// A timed splash with duration `20` or less is **dropped entirely**, rather
/// than applied at a token duration. [`INFINITE_DURATION`] never qualifies;
/// the sentinel remains available for non-expiring effects.
#[must_use]
pub fn splash_would_be_dropped(duration: i32) -> bool {
    duration != INFINITE_DURATION && duration <= 20
}

/// One potion effect as it lands on one entity after splash falloff.
#[derive(Debug, Clone, PartialEq)]
pub enum SplashEffect {
    /// `effect_id` is `instant_health` or `instant_damage`; `amount` is already
    /// distance-scaled ([`splash_instant_amount`]) and ready to heal or damage.
    Instant {
        /// Canonical `minecraft:*` mob-effect id.
        effect_id: String,
        /// Already scaled by distance; never negative.
        amount: f32,
    },
    /// A fresh [`EffectInstance`]'s `(duration, amplifier)`, ready for
    /// [`ActiveEffects::apply`]. It has passed [`splash_would_be_dropped`], so
    /// every `Timed` value this module returns is meant to land.
    Timed {
        /// Canonical `minecraft:*` mob-effect id.
        effect_id: String,
        /// Distance-scaled duration in ticks, always `> 20`.
        duration: i32,
        /// Unscaled: falloff affects only duration and, for instant effects,
        /// amount.
        amplifier: u32,
    },
}

/// `ThrownSplashPotion.onHitAsPotion`'s whole per-entity loop, for one entity
/// already known to be in range: every one of `potion_registry_id`'s **built-in**
/// effects (see the module doc for why not `customEffects`), split
/// instant-vs-timed and scaled by `scale` ([`splash_scale`] of that entity's own
/// distance) and `duration_scale` (the item's `minecraft:potion_duration_scale`,
/// `1.0` for the default this build always uses).
///
/// `potion_registry_id` is the raw `minecraft:potion` network id
/// (`lodestone_model::item::ItemComponents::potion`). An id outside the
/// registry, or one whose built-in list is empty (`water`/`mundane`/`thick`/
/// `awkward` — a potion with no effects, `!potion.hasEffects()`), yields an
/// empty `Vec` — the water-bottle control this module's tests assert
/// explicitly, so an empty result is never mistaken for "not looked up yet".
#[must_use]
pub fn potion_splash_effects(potion_registry_id: i32, scale: f64, duration_scale: f32) -> Vec<SplashEffect> {
    let Some(potion_registry_id) =
        lodestone_data::potion::PotionId::from_registry_id(potion_registry_id)
    else {
        return Vec::new();
    };
    let built_in = lodestone_data::potion::potion_built_in_effects(potion_registry_id);
    built_in
        .iter()
        .filter_map(|&(effect_index, amplifier, base_duration)| {
            let effect_id = lodestone_data::mob_effects::MobEffectId::from_registry_id(
                i32::try_from(effect_index).ok()?,
            )?;
            let effect_id = lodestone_data::mob_effects::mob_effect_name_for(effect_id);
            let amplifier = u32::from(amplifier);
            if effect_is_instantaneous(effect_id) {
                let base_amount = match effect_id.strip_prefix("minecraft:").unwrap_or(effect_id) {
                    "instant_health" => instant_health_amount(amplifier),
                    "instant_damage" => instant_damage_amount(amplifier),
                    // No other id passes `effect_is_instantaneous`, so this
                    // arm is unreachable — kept explicit rather than panicking
                    // on a table this module does not own.
                    _ => return None,
                };
                Some(SplashEffect::Instant {
                    effect_id: effect_id.to_owned(),
                    amount: splash_instant_amount(base_amount, scale),
                })
            } else {
                let duration = splash_timed_duration(base_duration, scale, duration_scale);
                if splash_would_be_dropped(duration) {
                    None
                } else {
                    Some(SplashEffect::Timed {
                        effect_id: effect_id.to_owned(),
                        duration,
                        amplifier,
                    })
                }
            }
        })
        .collect()
}

/// One effect grant from a food item. A food grant is never distance-scaled and
/// never instantaneous, so this is the plain `(effect, duration, amplifier)`
/// triple [`ActiveEffects::apply`] takes, plus the probability that the caller
/// must roll.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodEffectGrant {
    /// Canonical `minecraft:*` mob-effect id.
    pub effect_id: &'static str,
    /// Duration in ticks.
    pub duration: i32,
    /// 0-based amplifier.
    pub amplifier: u32,
    /// `random.nextFloat() < probability` — `1.0` always applies.
    pub probability: f32,
}

/// The per-food status-effect-on-consume lists, transcribed exactly — every
/// registration in that table except `CHICKEN`'s duplicate-looking-but-distinct
/// entry, which *is* included below. Sorted by item for [`food_consume_effects`]'s
/// linear scan (the table is seven rows; a binary search would be a second thing to
/// keep sorted for no measured benefit).
///
/// `resistance`'s damage reduction and `absorption`'s extra hit points are
/// granted here and consumed by the damage path. [`ActiveEffects::overlay_
/// defenses`] overlays both onto the equipment-derived `Defenses` before
/// every real hit, so a granted Resistance or Absorption effect measurably
/// reduces damage. `fire_resistance` and `regeneration` use their own consumers
/// (`crate::burning`'s `fire_resistance` flag and `ActiveEffects::tick`'s heal).
static FOOD_EFFECTS: &[(&str, &[FoodEffectGrant])] = &[
    (
        "minecraft:chicken",
        &[FoodEffectGrant {
            effect_id: "minecraft:hunger",
            duration: 600,
            amplifier: 0,
            probability: 0.3,
        }],
    ),
    (
        "minecraft:enchanted_golden_apple",
        &[
            FoodEffectGrant {
                effect_id: "minecraft:regeneration",
                duration: 400,
                amplifier: 1,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:resistance",
                duration: 6000,
                amplifier: 0,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:fire_resistance",
                duration: 6000,
                amplifier: 0,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:absorption",
                duration: 2400,
                amplifier: 3,
                probability: 1.0,
            },
        ],
    ),
    (
        "minecraft:golden_apple",
        &[
            FoodEffectGrant {
                effect_id: "minecraft:regeneration",
                duration: 100,
                amplifier: 1,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:absorption",
                duration: 2400,
                amplifier: 0,
                probability: 1.0,
            },
        ],
    ),
    (
        "minecraft:poisonous_potato",
        &[FoodEffectGrant {
            effect_id: "minecraft:poison",
            duration: 100,
            amplifier: 0,
            probability: 0.6,
        }],
    ),
    (
        "minecraft:pufferfish",
        &[
            FoodEffectGrant {
                effect_id: "minecraft:poison",
                duration: 1200,
                amplifier: 1,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:hunger",
                duration: 300,
                amplifier: 2,
                probability: 1.0,
            },
            FoodEffectGrant {
                effect_id: "minecraft:nausea",
                duration: 300,
                amplifier: 0,
                probability: 1.0,
            },
        ],
    ),
    (
        "minecraft:rotten_flesh",
        &[FoodEffectGrant {
            effect_id: "minecraft:hunger",
            duration: 600,
            amplifier: 0,
            probability: 0.8,
        }],
    ),
    (
        "minecraft:spider_eye",
        &[FoodEffectGrant {
            effect_id: "minecraft:poison",
            duration: 100,
            amplifier: 0,
            probability: 1.0,
        }],
    ),
];

/// The effect grants `item` applies on a successful eat, or `&[]` for every
/// item vanilla's own consumables table gives no on-consume-effects list — including every
/// plain food (`FOODS` in `crate::item_use` has forty rows; this table has
/// seven, and the other thirty-three are correctly silent here).
#[must_use]
pub fn food_consume_effects(item: &str) -> &'static [FoodEffectGrant] {
    FOOD_EFFECTS
        .iter()
        .find(|&&(name, _)| name == item)
        .map_or(&[] as &[FoodEffectGrant], |&(_, grants)| grants)
}

/// `Consumables.HONEY_BOTTLE`'s `onConsume(new
/// RemoveStatusEffectsConsumeEffect(MobEffects.POISON))` — the one food whose
/// consume effect *removes* rather than grants. Deterministic (no probability
/// field on `RemoveStatusEffectsConsumeEffect`), so the caller need only check
/// this and, if `true`, call `ActiveEffects::remove("minecraft:poison")`.
#[must_use]
pub fn removes_poison_on_consume(item: &str) -> bool {
    item == "minecraft:honey_bottle"
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

/// Overlays this entity's live Resistance amplifier and Absorption pool onto
/// equipment-derived `Defenses` (`PlayerInventory::combat_stats().defenses` at
/// the caller), so food-granted defenses affect the damage path. The overlay
/// contributes the active Resistance reduction and Absorption pool to each hit.
    ///
/// `resistance_amplifier` is a direct read of the active amplifier consumed by
/// [`lodestone_entity::damage_after_resistance`]. `absorption` is the active
/// pool, `4.0 * (amplifier + 1)` at base `4.0`, scaled by level.
    ///
    /// **Disclosed simplification**: vanilla drains a separate `AbsorptionAmount`
    /// pool across hits within one effect's duration — a hit that only partially
    /// consumes the cushion leaves the remainder for the next one. This crate does
    /// not yet track that depleting pool (it would need to persist across ticks
    /// wherever the effect is granted, not just at the hit site), so every hit
    /// while Absorption is active sees the effect's full nominal cushion rather
    /// than whatever an earlier hit in the same duration already spent. Real
    /// reduction, still measurably wrong compared to vanilla's per-hit depletion —
    /// tracked, not silently approximated.
    #[must_use]
    pub fn overlay_defenses(&self, base: lodestone_entity::Defenses) -> lodestone_entity::Defenses {
        lodestone_entity::Defenses {
            resistance_amplifier: self.amplifier_of("minecraft:resistance").map(|a| a as i32),
            absorption: self
                .amplifier_of("minecraft:absorption")
                .map(|a| 4.0 * (a + 1) as f32)
                .unwrap_or(0.0),
            ..base
        }
    }

    /// Applies an effect, updating an existing [`EffectInstance`] when one is
    /// already present. Returns `true` when the stored state changed, allowing
    /// the caller to decide whether to send an update packet.
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

    // ---- the splash falloff, and its two extremes ----

    /// [`splash_scale`] at the two ends of the blast: `1.0` dead-on, `0.0` at the
    /// very edge — both from [`SPLASH_RANGE`]/[`SPLASH_RANGE_SQ`] directly, not a
    /// re-typed literal.
    #[test]
    fn splash_scale_is_one_at_zero_distance_and_zero_at_the_blast_edge() {
        assert_eq!(splash_scale(0.0), 1.0);
        assert_eq!(splash_scale(SPLASH_RANGE_SQ), 0.0);
        // Two blocks out (distance_sq = 4.0, distance = 2.0): halfway.
        assert_eq!(splash_scale(4.0), 0.5);
    }

    /// Only the two `HealOrHarmMobEffect` ids are instantaneous — every other
    /// potion-reachable effect, and a bare unrelated string, are not.
    #[test]
    fn only_heal_or_harm_effects_are_instantaneous() {
        assert!(effect_is_instantaneous("minecraft:instant_health"));
        assert!(effect_is_instantaneous("minecraft:instant_damage"));
        assert!(effect_is_instantaneous("instant_damage"), "bare path also works");
        assert!(!effect_is_instantaneous("minecraft:speed"));
        assert!(!effect_is_instantaneous("minecraft:regeneration"));
        assert!(!effect_is_instantaneous("minecraft:poison"));
    }

    /// [`splash_instant_amount`] at the two discriminating scales: full and a
    /// scale where "no falloff" and "the correct falloff" give different
    /// integers, not just different floats that happen to round the same.
    #[test]
    fn splash_instant_amount_scales_down_not_just_direction() {
        // instant_damage at amplifier 0 is 6.0 (HealOrHarmMobEffect's own `6 <<
        // amplification`).
        let base = instant_damage_amount(0);
        assert_eq!(splash_instant_amount(base, 1.0), 6.0, "a direct hit is unscaled");
        assert_eq!(
            splash_instant_amount(base, 0.5),
            3.0,
            "half scale must give half the amount, not the full 6"
        );
        assert_ne!(
            splash_instant_amount(base, 1.0),
            splash_instant_amount(base, 0.5),
            "the wrong hypothesis (no falloff) would make these equal"
        );
    }

    /// [`splash_timed_duration`] on a real potion's own base duration
    /// (swiftness: 3600 ticks, from `lodestone_data::potion::POTION_EFFECTS`),
    /// at two scales chosen so "no falloff" and "the real falloff" disagree by
    /// far more than rounding.
    #[test]
    fn splash_timed_duration_scales_a_real_potion_duration() {
        let base = 3600u32; // swiftness's own base duration
        assert_eq!(splash_timed_duration(base, 1.0, 1.0), 3600, "a direct hit is unscaled");
        assert_eq!(
            splash_timed_duration(base, 0.2, 1.0),
            720,
            "scale 0.2 of 3600 is 720, not 3600"
        );
        assert_ne!(
            splash_timed_duration(base, 1.0, 1.0),
            splash_timed_duration(base, 0.2, 1.0),
            "the wrong hypothesis (no falloff) would make these equal"
        );
    }

    /// `endsWithin(20)`: a duration of exactly 20 is dropped, 21 survives — the
    /// boundary `splash_would_be_dropped` exists to get right in both
    /// directions, plus the infinite sentinel never qualifying.
    #[test]
    fn splash_would_be_dropped_at_the_endswithin_twenty_boundary() {
        assert!(splash_would_be_dropped(20), "endsWithin(20) is <=, so 20 itself is dropped");
        assert!(splash_would_be_dropped(0));
        assert!(!splash_would_be_dropped(21), "21 survives the same boundary");
        assert!(
            !splash_would_be_dropped(INFINITE_DURATION),
            "an infinite duration never ends within anything"
        );
    }

    /// [`potion_splash_effects`] on a real timed-only potion (swiftness) at two
    /// distances that must disagree, computed from
    /// [`lodestone_data::potion::potion_effect_entries`] — an independently
    /// tested source for the base duration/amplifier — rather than from this
    /// module's own [`splash_timed_duration`].
    #[test]
    fn potion_splash_effects_scales_a_timed_effect_by_distance() {
        let swiftness = lodestone_data::potion::potion_id("minecraft:swiftness").expect("swiftness exists");
        let potion_id = lodestone_data::potion::PotionId::from_registry_id(swiftness)
            .expect("generated potion id is valid");
        let entries = lodestone_data::potion::potion_effect_entries(potion_id);
        assert_eq!(entries.len(), 1, "swiftness carries exactly one built-in effect");
        let base_duration = entries[0].duration_ticks;
        let amplifier = u32::from(entries[0].amplifier);
        assert_eq!(base_duration, 3600, "swiftness's own base duration");
        assert_eq!(amplifier, 0);

        let close = potion_splash_effects(swiftness, splash_scale(0.0), 1.0);
        let far = potion_splash_effects(swiftness, splash_scale(10.24), 1.0);
        assert_eq!(close.len(), 1);
        assert_eq!(far.len(), 1);
        match (&close[0], &far[0]) {
            (
                SplashEffect::Timed { effect_id: c_id, duration: c_dur, amplifier: c_amp },
                SplashEffect::Timed { effect_id: f_id, duration: f_dur, amplifier: f_amp },
            ) => {
                assert_eq!(c_id, "minecraft:speed");
                assert_eq!(f_id, "minecraft:speed");
                assert_eq!(*c_amp, amplifier);
                assert_eq!(*f_amp, amplifier);
                // scale(0.0) = 1.0 -> 3600; scale(10.24) = 1.0 - 3.2/4.0 = 0.2 -> 720.
                assert_eq!(*c_dur, 3600);
                assert_eq!(*f_dur, 720);
                assert_ne!(c_dur, f_dur, "distance must change the applied duration");
            }
            other => panic!("expected two Timed effects, got {other:?}"),
        }
    }

    /// The instant-vs-timed split, on a real instant-only potion (harming).
    #[test]
    fn potion_splash_effects_scales_an_instant_effect_by_distance() {
        let harming = lodestone_data::potion::potion_id("minecraft:harming").expect("harming exists");
        let close = potion_splash_effects(harming, splash_scale(0.0), 1.0);
        let far = potion_splash_effects(harming, splash_scale(10.24), 1.0);
        assert_eq!(close, vec![SplashEffect::Instant { effect_id: "minecraft:instant_damage".to_owned(), amount: 6.0 }]);
        assert_eq!(far, vec![SplashEffect::Instant { effect_id: "minecraft:instant_damage".to_owned(), amount: 1.0 }]);
    }

    /// **Control**: a water bottle (`minecraft:water`, `POTION_EFFECTS` empty)
    /// yields no splash effects at any scale — proving an empty result is the
    /// potion's own no-effects case, not this function failing to look anything
    /// up. Without this, a version that always returned an empty `Vec` would
    /// pass every gate above it vacuously.
    #[test]
    fn potion_splash_effects_water_bottle_control() {
        let water = lodestone_data::potion::potion_id("minecraft:water").expect("water exists");
        assert_eq!(potion_splash_effects(water, 1.0, 1.0), Vec::new());
        // An out-of-range id (this build's registry) must also yield nothing,
        // rather than panicking or guessing.
        assert_eq!(potion_splash_effects(-1, 1.0, 1.0), Vec::new());
        assert_eq!(
            potion_splash_effects(lodestone_data::potion::POTION_COUNT as i32, 1.0, 1.0),
            Vec::new()
        );
    }

    // ---- food consume effects  ----

    /// A golden apple grants exactly regeneration II (100 ticks) and
    /// absorption I (2400 ticks) — the two-row list `Consumables.GOLDEN_APPLE`
    /// declares, both guaranteed (`probability == 1.0`).
    #[test]
    fn golden_apple_grants_regeneration_and_absorption() {
        let grants = food_consume_effects("minecraft:golden_apple");
        assert_eq!(
            grants,
            &[
                FoodEffectGrant {
                    effect_id: "minecraft:regeneration",
                    duration: 100,
                    amplifier: 1,
                    probability: 1.0,
                },
                FoodEffectGrant {
                    effect_id: "minecraft:absorption",
                    duration: 2400,
                    amplifier: 0,
                    probability: 1.0,
                },
            ]
        );
    }

    /// The enchanted golden apple's list is **not** the plain apple's scaled up —
    /// it is four rows, a stronger and longer regeneration, plus resistance and
    /// fire resistance the plain apple does not grant at all, plus an absorption
    /// **amplifier** four times the plain apple's (`3` vs `0`, i.e. eight extra
    /// hearts against two). A table that shared one row between the two items
    /// would fail this exact assertion.
    #[test]
    fn enchanted_golden_apple_grants_a_distinct_four_row_list() {
        let plain = food_consume_effects("minecraft:golden_apple");
        let enchanted = food_consume_effects("minecraft:enchanted_golden_apple");
        assert_eq!(enchanted.len(), 4);
        assert_ne!(plain, enchanted);
        assert_eq!(enchanted[0].effect_id, "minecraft:regeneration");
        assert_eq!(enchanted[0].duration, 400);
        assert_eq!(enchanted[0].amplifier, 1);
        assert_eq!(enchanted[1].effect_id, "minecraft:resistance");
        assert_eq!(enchanted[2].effect_id, "minecraft:fire_resistance");
        assert_eq!(enchanted[3].effect_id, "minecraft:absorption");
        assert_eq!(enchanted[3].amplifier, 3);
        assert_ne!(
            enchanted[3].amplifier,
            plain
                .iter()
                .find(|g| g.effect_id == "minecraft:absorption")
                .unwrap()
                .amplifier
        );
    }

    /// The three probabilistic grants carry three **different** probabilities
    /// (`0.3`, `0.6`, `0.8`) — pairwise-distinct so a transposition between rows
    /// cannot survive this assertion, per this repo's own evidence standard for
    /// adjacent same-typed fields.
    #[test]
    fn probabilistic_grants_are_pairwise_distinct() {
        let chicken = food_consume_effects("minecraft:chicken")[0].probability;
        let poisonous_potato = food_consume_effects("minecraft:poisonous_potato")[0].probability;
        let rotten_flesh = food_consume_effects("minecraft:rotten_flesh")[0].probability;
        assert_eq!(chicken, 0.3);
        assert_eq!(poisonous_potato, 0.6);
        assert_eq!(rotten_flesh, 0.8);
        assert_ne!(chicken, poisonous_potato);
        assert_ne!(poisonous_potato, rotten_flesh);
        assert_ne!(chicken, rotten_flesh);
    }

    /// Pufferfish is the one three-row *guaranteed* list — poison II, hunger III
    /// and nausea I, all at `probability == 1.0`, distinguishing it from
    /// `poisonous_potato`'s single **probabilistic** poison row despite both
    /// granting poison.
    #[test]
    fn pufferfish_grants_three_guaranteed_effects() {
        let grants = food_consume_effects("minecraft:pufferfish");
        assert_eq!(grants.len(), 3);
        assert!(grants.iter().all(|g| g.probability == 1.0));
        assert_eq!(grants[0], FoodEffectGrant {
            effect_id: "minecraft:poison",
            duration: 1200,
            amplifier: 1,
            probability: 1.0,
        });
        assert_eq!(grants[1].effect_id, "minecraft:hunger");
        assert_eq!(grants[1].amplifier, 2);
        assert_eq!(grants[2].effect_id, "minecraft:nausea");
    }

    /// **Control**: an ordinary food with no `onConsumeEffects` list (an apple)
    /// yields nothing, and so does an item this table was never meant to cover
    /// (stone) — proving an empty result is the food's own no-effects case, not
    /// this function failing to look anything up at all.
    #[test]
    fn food_consume_effects_water_bottle_style_control() {
        assert_eq!(food_consume_effects("minecraft:apple"), &[] as &[FoodEffectGrant]);
        assert_eq!(food_consume_effects("minecraft:stone"), &[] as &[FoodEffectGrant]);
        // Splash-side and spider-eye must not be confused: a spider eye
        // itself grants poison, but that must not leak onto any *other* item.
        assert_eq!(food_consume_effects("minecraft:fermented_spider_eye"), &[] as &[FoodEffectGrant]);
    }

    /// Only honey bottle removes poison on consume — the deterministic
    /// `RemoveStatusEffectsConsumeEffect` arm, distinct from every probabilistic
    /// `ApplyStatusEffectsConsumeEffect` row above.
    #[test]
    fn only_honey_bottle_removes_poison() {
        assert!(removes_poison_on_consume("minecraft:honey_bottle"));
        assert!(!removes_poison_on_consume("minecraft:milk_bucket"));
        assert!(!removes_poison_on_consume("minecraft:golden_apple"));
        assert!(!removes_poison_on_consume("minecraft:apple"));
    }
}
