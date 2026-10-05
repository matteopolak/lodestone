//! Tests for finishing a consumed item (potions, milk, ominous bottles).

use super::*;
use crate::server::tests::stack;

/// Completing a `minecraft:ominous_bottle` use grants
/// `minecraft:bad_omen` for 120000 ticks at amplifier 0 and consumes the
/// bottle. The assertion covers both the effect result and the held-item
/// decrement.
#[test]
fn finish_drinking_ominous_bottle_grants_bad_omen_and_consumes_the_bottle() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:ominous_bottle", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:ominous_bottle".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    assert!(effects.get("minecraft:bad_omen").is_none(), "precondition: no Bad Omen carried yet");
    let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
    let (native, remainder) = result.expect("a present ominous bottle must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed, leaving the slot empty");
    assert_eq!(inv.native(0), None, "the bottle must actually leave the inventory");

    let instance = effects.get("minecraft:bad_omen").expect("Bad Omen must now be carried");
    assert_eq!(instance.amplifier(), 0);
    assert_eq!(instance.duration(), 120_000, "OminousBottleAmplifier.EFFECT_DURATION");
}

/// **Control**: any other item (even another drinkable, like a plain
/// potion) must not grant Bad Omen or be consumed through this path —
/// this function's whole reason to be separate from [`finish_consuming`]
/// is that it is a one-item special case, not a general drink handler.
#[test]
fn finish_drinking_ominous_bottle_ignores_every_other_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:potion", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
    assert!(result.is_none(), "a plain potion must not be handled by the ominous-bottle path");
    assert!(effects.get("minecraft:bad_omen").is_none(), "no effect must be granted");
    assert_eq!(inv.native(0), Some(&stack("minecraft:potion", 1)), "the potion must stay in hand, untouched");
}

pub(super) fn potion_stack(potion: &str, count: u32) -> ItemStack {
    let mut s = stack("minecraft:potion", count);
    s.components.potion = Some(lodestone_data::potion::potion_id(potion).expect("real potion"));
    s
}

/// A real timed-effect potion (Strength II) must
/// land its full, **unscaled** duration and amplifier on the drinker and
/// consume the bottle — the whole reason this function exists, since before
/// it every potion in the game did nothing at all.
#[test]
fn finish_drinking_potion_grants_the_full_unscaled_effect() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:strong_strength", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (native, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("a real potion must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed");
    assert_eq!(
        effects,
        vec![crate::mob_effects::SplashEffect::Timed {
            effect_id: lodestone_data::mob_effects::MobEffectId::STRENGTH,
            duration: 1800,
            amplifier: 1,
        }],
        "Strong Strength: amplifier II, 1:30 — unscaled, not the splash falloff"
    );
}

/// An instant potion (Harming) reaches the caller as a full-strength
/// [`SplashEffect::Instant`] — `6 << amplifier` unscaled, the same
/// `splash_instant_amount` computation at `scale = 1.0` a direct hit at
/// point-blank range would produce, proving drinking is not merely "a splash
/// with the thrower standing on the target".
#[test]
fn finish_drinking_potion_carries_an_instant_effect_at_full_strength() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:harming", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, _, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("harming must finish");
    assert_eq!(
        effects,
        vec![crate::mob_effects::SplashEffect::Instant {
            effect_id: lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE,
            amount: 6.0,
        }]
    );
}

/// **Control**: a water bottle's `minecraft:potion` id resolves (it is a
/// real potion), but its built-in effect list is empty, so drinking it must
/// still fully consume the bottle and yield zero grants — not "not handled"
/// and not a panic on an empty list.
#[test]
fn finish_drinking_potion_water_bottle_control() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(potion_stack("minecraft:water", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("water must still finish");
    assert!(remainder.is_none());
    assert!(effects.is_empty());
}

/// An out-of-census component value remains a wire-boundary failure, not
/// an empty entry in the built-in potion table. The stack still finishes
/// consuming, but it cannot grant an arbitrary built-in effect.
#[test]
fn finish_drinking_potion_rejects_an_unknown_component_id() {
    let mut invalid = stack("minecraft:potion", 1);
    invalid.components.potion = Some(-1);
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(invalid));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:potion".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, effects) =
        finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("the stack must finish");
    assert!(remainder.is_none(), "the invalid component does not cancel consumption");
    assert!(effects.is_empty(), "an unknown raw id cannot become a built-in effect");
}

/// **Control**: any other item, including food, must not be handled by
/// this path.
#[test]
fn finish_drinking_potion_ignores_every_other_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:golden_apple", 1)));
    let started = ItemInUse {
        native: 0,
        item: "minecraft:golden_apple".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };
    assert!(finish_drinking_potion(&mut inv, &started, GameMode::Survival).is_none());
}

/// Drinking milk clears every active effect and
/// reports exactly the ids that were cleared, and consumes the bucket.
#[test]
fn finish_drinking_milk_clears_every_active_effect() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    effects.apply("minecraft:poison", 100, 0);
    effects.apply("minecraft:speed", 200, 1);
    let started = ItemInUse {
        native: 0,
        item: "minecraft:milk_bucket".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (native, remainder, mut cleared) =
        finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
    assert_eq!(native, 0);
    assert!(remainder.is_none());
    cleared.sort();
    assert_eq!(cleared, vec!["minecraft:poison".to_owned(), "minecraft:speed".to_owned()]);
    assert!(effects.is_empty(), "every effect must actually be gone");
}

/// **Control**: milk drunk with nothing active clears nothing (an empty
/// `Vec`, not a sentinel) but still consumes the bucket — matching this
/// crate's own water-bottle-control convention for "ran, and had nothing to
/// do" versus "did not run".
#[test]
fn finish_drinking_milk_with_no_active_effects_control() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let started = ItemInUse {
        native: 0,
        item: "minecraft:milk_bucket".to_owned(),
        finish_tick: 0,
        last_effect_remaining: None,
    };

    let (_, remainder, cleared) =
        finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
    assert!(remainder.is_none(), "the bucket is still consumed");
    assert!(cleared.is_empty());
}
