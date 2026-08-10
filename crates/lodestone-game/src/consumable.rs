//! The `minecraft:consumable` item component: what eating or drinking an item
//! looks like, sounds like, and how long it takes.
//!
//! # What it is
//!
//! Vanilla's `Consumable` record is `(consumeSeconds, animation, sound,
//! hasConsumeParticles, onConsumeEffects)`. This module carries the first four —
//! everything a *client* needs to animate a consume and everything a *server*
//! needs to broadcast its sounds. The effect lists (a golden apple's regeneration,
//! milk clearing effects, chorus fruit's teleport) are gameplay and live with
//! whatever applies them.
//!
//! # Why here, and not in `lodestone-data`
//!
//! `minecraft:consumable` is a **prototype** component: it is never on the wire, so
//! no packet capture can produce it, and `lodestone-data`'s contract is that every
//! table there is dumped from a headless jar. There is no consumable column in any
//! existing dump. The record definition is therefore the source, and at 43 items it
//! is small enough to transcribe exactly — the same call
//! `lodestone_server::item_use`'s `FOODS` already made for the `minecraft:food`
//! half.
//!
//! It lives in `lodestone-game` because **both sides read it**, and they read
//! disjoint halves of the *same* effect. Vanilla runs
//! `Consumable.emitParticlesAndSounds` on client and server alike and each side
//! drops what it cannot do:
//!
//! | half | why it lands where it does |
//! |---|---|
//! | particles | `ServerLevel.addParticle` is a no-op, so particles are **always** client-predicted |
//! | sound | `Entity.playSound` → `level.playSound(null, …)`; `ClientLevel.playSeededSound` skips it because `except == null` is not the local player, so the sound is **always** the server's broadcast |
//!
//! So a client that emits the sound itself double-plays it against a real server,
//! and a server that emits particles sends nothing anyone can see. The split is
//! vanilla's, not a convenience.
//!
//! # How to change it
//!
//! Adding an item is one row in [`CONSUMABLES`], which is sorted by id and looked
//! up by binary search — keep it sorted. Only two items in 26.2 override
//! `consumeSeconds` and only one overrides `sound`, so a new row is almost always
//! a `defaultFood()` or `defaultDrink()` clone; `Consumables.defaultFood()` and
//! `defaultDrink()` are the two shapes and [`EAT_SOUND`]/[`DRINK_SOUND`] their
//! sounds.
//!
//! The gotcha is `Consumable.Builder::soundAfterConsume`, which is **not** a
//! `sound` override — it lowers to `onConsume(new PlaySoundConsumeEffect(…))`. An
//! ominous bottle's `item.ominous_bottle.dispose` is a completion effect and its
//! `sound` field is still `entity.generic.drink`; transcribing it into the `sound`
//! column would make the bottle play its disposal noise six times while drinking.

/// Which of vanilla's two consume animations an item uses
/// (`ItemUseAnimation.EAT` / `DRINK`).
///
/// No `Consumable` in 26.2 uses any other `ItemUseAnimation` value, so this is a
/// two-variant enum rather than the full ten-variant one — the other eight are
/// selected from `BLOCKS_ATTACKS`/`KINETIC_WEAPON`/per-item identity and are not
/// consume animations at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeAnimation {
    /// `ItemUseAnimation.EAT`.
    Eat,
    /// `ItemUseAnimation.DRINK`.
    Drink,
}

/// One item's `minecraft:consumable` component, narrowed to the four fields the
/// animation, the particles and the sound need.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Consumable {
    /// `Consumable.consumeTicks()` — `(int)(consumeSeconds * 20.0F)`.
    ///
    /// This is also the item's `getUseDuration`, i.e. what
    /// `LivingEntity.useItemRemaining` counts down from, so it is the divisor in
    /// every consume animation expression.
    pub consume_ticks: u32,
    /// `Consumable.animation()`.
    pub animation: ConsumeAnimation,
    /// `Consumable.hasConsumeParticles()`. **False for every drink**, which is why
    /// a potion produces no crumbs and a carrot does.
    pub has_consume_particles: bool,
    /// `Consumable.sound()`, as a `minecraft:sound_event` registry id.
    pub sound: &'static str,
}

/// `Consumables.defaultFood()`'s sound — `SoundEvents.GENERIC_EAT`.
pub const EAT_SOUND: &str = "minecraft:entity.generic.eat";
/// `Consumables.defaultDrink()`'s sound — `SoundEvents.GENERIC_DRINK`.
pub const DRINK_SOUND: &str = "minecraft:entity.generic.drink";
/// `Consumables.HONEY_BOTTLE`'s sound — `SoundEvents.HONEY_DRINK`. The only item
/// in 26.2 that overrides the `sound` field at all.
pub const HONEY_DRINK_SOUND: &str = "minecraft:item.honey_bottle.drink";

/// `SoundEvents.PLAYER_BURP` — the extra sound `FoodProperties.onConsume` plays
/// once a **player** finishes a food, at volume `0.5F` on `SoundSource.PLAYERS`.
///
/// Player-only and food-only: it comes from the `minecraft:food` component's
/// `ConsumableListener`, so a mob eating, or anyone drinking a potion, does not
/// burp.
pub const BURP_SOUND: &str = "minecraft:entity.player.burp";

/// `Consumables.DEFAULT_CONSUME_SECONDS` (`1.6F`) in ticks.
///
/// 41 of the 43 consumables in 26.2 use this. `1.6F * 20.0F` is exactly `32.0F` in
/// float, so the `(int)` cast has no truncation-to-31 trap here.
pub const DEFAULT_CONSUME_TICKS: u32 = 32;

/// `Consumable.CONSUME_EFFECTS_INTERVAL` — effects fire on every 4th *remaining*
/// tick.
pub const CONSUME_EFFECTS_INTERVAL: u32 = 4;

/// `Consumable.CONSUME_EFFECTS_START_FRACTION` — no effects until this fraction of
/// the use has elapsed.
pub const CONSUME_EFFECTS_START_FRACTION: f32 = 0.218_75;

/// `ItemStack.onUseTick`'s particle count — the burst on each periodic emission.
pub const PERIODIC_PARTICLE_COUNT: u32 = 5;

/// `Consumable.onConsume`'s particle count — the larger burst on the final bite.
pub const FINISH_PARTICLE_COUNT: u32 = 16;

/// The `minecraft:consumable` component of `item` (a full registry name such as
/// `"minecraft:carrot"`), or `None` when the item cannot be eaten or drunk.
#[must_use]
pub fn consumable_for_item(item: &str) -> Option<Consumable> {
    CONSUMABLES
        .binary_search_by_key(&item, |&(name, _)| name)
        .ok()
        .map(|index| CONSUMABLES[index].1)
}

/// `Consumable.shouldEmitParticlesAndSounds(useItemRemainingTicks)`, verbatim:
///
/// ```java
/// int itemUsedForTicks = this.consumeTicks() - useItemRemainingTicks;
/// int waitTicksBeforeUseEffects = (int)(this.consumeTicks() * 0.21875F);
/// boolean isValidTime = itemUsedForTicks > waitTicksBeforeUseEffects;
/// return isValidTime && useItemRemainingTicks % 4 == 0;
/// ```
///
/// # Both conjuncts, and why one of them is easy to lose
///
/// This is the shape `CLAUDE.md` warns about: implementing only the modulo gives
/// code that emits crumbs on a plausible cadence and is wrong at the *start* of
/// every use — vanilla stays silent for the first `(int)(duration * 0.21875)`
/// ticks, which is 7 of a food's 32. Implementing only the fraction gives one
/// emission per tick. Each clause alone looks like a working eat animation, so the
/// count over a fixed span is the discriminating quantity and its presence is not.
///
/// For the default 32-tick food that is emissions at `remaining` = 24, 20, 16, 12,
/// 8 and 4 — **six**, not eight, because `remaining` 32 and 28 are inside the
/// wait. The wrong-hypothesis counts are 8 (modulo only) and 24 (fraction only).
///
/// `remaining_ticks` is vanilla's `getUseItemRemainingTicks()`, i.e. the value
/// `updateUsingItem` passes to `onUseTick` **before** decrementing, so a use runs
/// through `consume_ticks, consume_ticks - 1, …, 1`. Derive it with
/// [`remaining_ticks`] if you are holding an upward-counting tick total instead.
#[must_use]
pub fn should_emit_consume_effects(consume_ticks: u32, remaining_ticks: u32) -> bool {
    let used = consume_ticks.saturating_sub(remaining_ticks);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "vanilla's own `(int)(consumeTicks * 0.21875F)`; the operand is at most a few hundred ticks"
    )]
    let wait = (consume_ticks as f32 * CONSUME_EFFECTS_START_FRACTION) as u32;
    used > wait && remaining_ticks % CONSUME_EFFECTS_INTERVAL == 0
}

/// `getUseItemRemainingTicks()` from an upward-counting tick total —
/// the inverse of `LivingEntity.getTicksUsingItem()`, which is
/// `getUseDuration() - getUseItemRemainingTicks()`.
///
/// This client counts **up** (`lodestone_ecs::player::ItemUseTicks`) because
/// counting up needs no per-item `getUseDuration` for a bow, whose duration is
/// 72000. A consume does have a real duration, so the two directions are
/// interconvertible here and this is the one place that conversion happens.
///
/// Saturates at zero: a use that has run past its own duration — the server
/// completed it and we have not yet been told — has no remaining ticks rather
/// than wrapping to an enormous count.
#[must_use]
pub fn remaining_ticks(consume_ticks: u32, ticks_used: u32) -> u32 {
    consume_ticks.saturating_sub(ticks_used)
}

/// `Consumables.defaultFood()` — the 39-of-43 case.
const fn food(consume_ticks: u32) -> Consumable {
    Consumable {
        consume_ticks,
        animation: ConsumeAnimation::Eat,
        has_consume_particles: true,
        sound: EAT_SOUND,
    }
}

/// `Consumables.defaultDrink()` — note `hasConsumeParticles(false)`, which is the
/// whole reason a potion throws no crumbs.
const fn drink(consume_ticks: u32, sound: &'static str) -> Consumable {
    Consumable {
        consume_ticks,
        animation: ConsumeAnimation::Drink,
        has_consume_particles: false,
        sound,
    }
}

/// Every item in 26.2 carrying `minecraft:consumable`, sorted by registry name for
/// [`consumable_for_item`]'s binary search.
///
/// # Provenance
///
/// A three-way join over the 26.2 decompile: `Items.java`'s 40 `.food(…)` calls
/// plus the three direct `.component(DataComponents.CONSUMABLE, …)` registrations
/// (milk bucket, potion, ominous bottle), `Item.Properties::food`'s one-argument
/// overload (which implicitly attaches `Consumables.DEFAULT_FOOD`), and
/// `Consumables.java`'s named builders.
///
/// # Two adjacent traps, both checked
///
/// * **`minecraft:food` is not `minecraft:consumable`.** The four mob buckets
///   (`cod_bucket`, `salmon_bucket`, `pufferfish_bucket`, `tropical_fish_bucket`)
///   carry `FOOD` with **no** `CONSUMABLE` and so are not edible — vanilla's
///   `Fox` goal tests both components for exactly this reason. They are absent
///   here on purpose.
/// * **`CONSUMABLE` is not `minecraft:food`.** `milk_bucket`, `potion` and
///   `ominous_bottle` are drinkable and have no food component, which is why
///   `lodestone_server::item_use`'s food table has 40 rows and this one has 43.
///   A consumer that resolves "can this be used" through the food table alone
///   cannot drink.
/// * `splash_potion` and `lingering_potion` carry `POTION_CONTENTS` only. They are
///   thrown, not drunk, and correctly absent.
pub const CONSUMABLES: &[(&str, Consumable)] = &[
    ("minecraft:apple", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:baked_potato", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:beef", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:beetroot", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:beetroot_soup", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:bread", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:carrot", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:chicken", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:chorus_fruit", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cod", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_beef", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_chicken", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_cod", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_mutton", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_porkchop", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_rabbit", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cooked_salmon", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:cookie", food(DEFAULT_CONSUME_TICKS)),
    // `Consumables.DRIED_KELP = defaultFood().consumeSeconds(0.8F)` — the fast one,
    // and the reason `consume_ticks` is per item rather than a constant.
    ("minecraft:dried_kelp", food(16)),
    ("minecraft:enchanted_golden_apple", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:glow_berries", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:golden_apple", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:golden_carrot", food(DEFAULT_CONSUME_TICKS)),
    // `Consumables.HONEY_BOTTLE = defaultDrink().consumeSeconds(2.0F)
    //  .sound(SoundEvents.HONEY_DRINK)` — the slow one, and the only `sound`
    // override in the game.
    ("minecraft:honey_bottle", drink(40, HONEY_DRINK_SOUND)),
    ("minecraft:melon_slice", food(DEFAULT_CONSUME_TICKS)),
    (
        "minecraft:milk_bucket",
        drink(DEFAULT_CONSUME_TICKS, DRINK_SOUND),
    ),
    ("minecraft:mushroom_stew", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:mutton", food(DEFAULT_CONSUME_TICKS)),
    // `Consumables.OMINOUS_BOTTLE = defaultDrink().soundAfterConsume(…)`. That
    // builder call is an `onConsume` effect, **not** a `sound` override — see the
    // module docs.
    (
        "minecraft:ominous_bottle",
        drink(DEFAULT_CONSUME_TICKS, DRINK_SOUND),
    ),
    ("minecraft:poisonous_potato", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:porkchop", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:potato", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:potion", drink(DEFAULT_CONSUME_TICKS, DRINK_SOUND)),
    ("minecraft:pufferfish", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:pumpkin_pie", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:rabbit", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:rabbit_stew", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:rotten_flesh", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:salmon", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:spider_eye", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:suspicious_stew", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:sweet_berries", food(DEFAULT_CONSUME_TICKS)),
    ("minecraft:tropical_fish", food(DEFAULT_CONSUME_TICKS)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_complete() {
        assert!(
            CONSUMABLES.windows(2).all(|w| w[0].0 < w[1].0),
            "CONSUMABLES must be sorted by id for the binary search"
        );
        assert_eq!(CONSUMABLES.len(), 43, "26.2 has 43 consumable items");
        assert_eq!(
            CONSUMABLES
                .iter()
                .filter(|(_, c)| c.animation == ConsumeAnimation::Drink)
                .count(),
            4,
            "honey_bottle, milk_bucket, ominous_bottle and potion are the drinks"
        );
    }

    /// Every drink has `hasConsumeParticles = false` and every food has it true,
    /// because both come from `defaultDrink()`/`defaultFood()` and nothing
    /// overrides the flag. Asserted rather than folded into `animation`, so a
    /// version that *does* override it fails here instead of silently agreeing.
    #[test]
    fn particles_track_the_animation_in_this_version() {
        for (item, consumable) in CONSUMABLES {
            let expected = consumable.animation == ConsumeAnimation::Eat;
            assert_eq!(
                consumable.has_consume_particles, expected,
                "{item} has_consume_particles"
            );
        }
    }

    /// The `soundAfterConsume` trap: an ominous bottle's `sound` field is the
    /// plain drink sound, not its disposal noise.
    #[test]
    fn an_ominous_bottle_drinks_with_the_generic_drink_sound() {
        let bottle = consumable_for_item("minecraft:ominous_bottle").expect("in the table");
        assert_eq!(bottle.sound, DRINK_SOUND);
        assert_eq!(
            consumable_for_item("minecraft:honey_bottle")
                .expect("in the table")
                .sound,
            HONEY_DRINK_SOUND,
            "honey bottle is the one real sound override"
        );
    }

    /// A mob bucket carries `minecraft:food` and **not** `minecraft:consumable`,
    /// so it must not resolve here — the conjunction vanilla's `Fox` goal tests.
    #[test]
    fn a_mob_bucket_is_food_but_not_consumable() {
        assert_eq!(consumable_for_item("minecraft:cod_bucket"), None);
        assert_eq!(consumable_for_item("minecraft:tropical_fish_bucket"), None);
        assert_eq!(consumable_for_item("minecraft:splash_potion"), None);
        assert_eq!(consumable_for_item("minecraft:stone"), None);
    }

    /// The cadence, by **count over a full use**, with both wrong hypotheses
    /// evaluated at the same input so the assertion can distinguish them.
    ///
    /// A food is 32 ticks and `remaining` runs 32 down to 1, so:
    ///
    /// | hypothesis | emissions |
    /// |---|---|
    /// | both conjuncts (correct) | **6** — remaining 24, 20, 16, 12, 8, 4 |
    /// | modulo only | 8 — adds remaining 32 and 28 |
    /// | start fraction only | 24 — every tick after the wait |
    #[test]
    fn a_food_emits_six_times_over_its_use() {
        let ticks = DEFAULT_CONSUME_TICKS;
        let fired: Vec<u32> = (1..=ticks)
            .rev()
            .filter(|&remaining| should_emit_consume_effects(ticks, remaining))
            .collect();
        assert_eq!(fired, vec![24, 20, 16, 12, 8, 4]);

        // The two wrong hypotheses, computed from the same constants, so the count
        // above is a prediction rather than a transcription of the run.
        let modulo_only = (1..=ticks).filter(|r| r % CONSUME_EFFECTS_INTERVAL == 0).count();
        let fraction_only = (1..=ticks)
            .filter(|&r| ticks - r > (ticks as f32 * CONSUME_EFFECTS_START_FRACTION) as u32)
            .count();
        assert_eq!((modulo_only, fraction_only), (8, 24));
        assert_ne!(fired.len(), modulo_only);
        assert_ne!(fired.len(), fraction_only);
    }

    /// The two non-default durations, whose emission counts differ from a food's —
    /// so the cadence cannot be replaced by a constant schedule.
    #[test]
    fn the_two_odd_durations_emit_different_counts() {
        let count = |ticks: u32| {
            (1..=ticks)
                .filter(|&remaining| should_emit_consume_effects(ticks, remaining))
                .count()
        };
        // Dried kelp: 16 ticks, wait = (int)(16 * 0.21875) = 3, so remaining
        // 12, 8, 4.
        assert_eq!(count(16), 3);
        // Honey bottle: 40 ticks, wait = (int)(40 * 0.21875) = 8, and remaining 32
        // is `used == 8`, which is *not* `> 8` — so it starts at 28.
        assert_eq!(count(40), 7);
        assert!(!should_emit_consume_effects(40, 32));
        assert!(should_emit_consume_effects(40, 28));
    }

    #[test]
    fn remaining_is_the_inverse_of_ticks_used() {
        assert_eq!(remaining_ticks(32, 0), 32);
        assert_eq!(remaining_ticks(32, 8), 24);
        assert_eq!(remaining_ticks(32, 32), 0);
        // Past the end, rather than wrapping.
        assert_eq!(remaining_ticks(32, 99), 0);
    }
}
