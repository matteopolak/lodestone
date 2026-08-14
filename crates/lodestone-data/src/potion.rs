//! Public potion id resolution and the `PotionContents` colour-mixing formula.
//!
//! # What it is
//!
//! `minecraft:potion_contents`' `potion` field carries a `minecraft:potion` registry
//! VarInt id — a fixed, built-in registry (`BuiltInRegistries.POTION`), the same kind
//! of static id->name table as [`crate::mob_effects`]. [`potion_name`]/[`potion_id`]
//! resolve it; [`potion_color`] ports `Potion.calculate` / `PotionContents.getColorOr`
//! / `PotionContents.getColorOptional` (`.cache/mc/26.2/client-src/net/minecraft/client
//! /color/item/Potion.java`, `.cache/mc/26.2/src/net/minecraft/world/item/alchemy
//! /PotionContents.java`) — the weighted-average mix a potion bottle's tint is drawn
//! from.
//!
//! # How it works
//!
//! `getColorOr` checks `customColor` first; failing that, `getColorOptional` folds
//! every effect in `getAllEffects()` (the potion's own built-in list, from
//! [`crate::generated_potion_effects::POTION_EFFECTS`], concatenated with any
//! `customEffects`) into a red/green/blue running sum weighted by `amplifier + 1`,
//! divides by the total weight, and falls back to `PotionContents.BASE_POTION_COLOR`
//! (`-13083194`) when there were no effects at all. Every mob-effect colour is
//! [`crate::generated_mob_effect_colors::MOB_EFFECT_COLORS`], and a
//! `MobEffectInstance`'s `effect` field is itself a network `minecraft:mob_effect` id
//! (`ByteBufCodecs.holderRegistry`, the same 0-based shape `minecraft:potion` uses),
//! so no extra indirection is needed between a wire effect id and this table's index.
//!
//! # How to change it
//!
//! If a future protocol version renumbers `minecraft:potion` or `minecraft:mob_effect`,
//! regenerate [`crate::generated_potions`] and [`crate::generated_mob_effect_colors`]
//! from the new `registries.json` / decompile and this module needs no change — it
//! only ever indexes those tables.

use crate::generated_mob_effect_colors::MOB_EFFECT_COLORS;
use crate::generated_potion_effects::POTION_EFFECTS;
use crate::generated_potions::POTION_NAMES;

pub use crate::generated_potions::POTION_COUNT;

/// `PotionContents.BASE_POTION_COLOR` (`-13083194`), opaque ARGB.
pub const BASE_POTION_COLOR: u32 = 0xFF38_5DC6;

/// Resolves a network potion registry id to its canonical `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..POTION_COUNT`, so a malformed or future-version
/// id surfaces as an explicit miss rather than a panic or a silently wrong potion.
#[must_use]
pub fn potion_name(id: i32) -> Option<&'static str> {
    usize::try_from(id).ok().and_then(|index| POTION_NAMES.get(index).copied())
}

/// Resolves a canonical `minecraft:*` potion identifier to its network registry id
/// for protocol 776. The reverse of [`potion_name`].
#[must_use]
pub fn potion_id(name: &str) -> Option<i32> {
    POTION_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
}

/// `ARGB.opaque`: force alpha to `0xFF`.
const fn opaque(rgb: u32) -> u32 {
    (rgb & 0x00FF_FFFF) | 0xFF00_0000
}

/// One effect's contribution: `(amplifier + 1) * channel`, folded into a running sum.
///
/// `effect_id` is a network `minecraft:mob_effect` id (see module doc); an id outside
/// [`MOB_EFFECT_COLORS`]' range contributes nothing, matching a caller that could not
/// resolve the effect either — silently dropping an unknown effect from the average
/// rather than panicking or poisoning the whole mix.
fn accumulate(effect_id: i32, amplifier: u8, red: &mut u32, green: &mut u32, blue: &mut u32, weight: &mut u32) {
    let Some(color) = usize::try_from(effect_id).ok().and_then(|i| MOB_EFFECT_COLORS.get(i)).copied() else {
        return;
    };
    let w = u32::from(amplifier) + 1;
    *red += w * ((color >> 16) & 0xFF);
    *green += w * ((color >> 8) & 0xFF);
    *blue += w * (color & 0xFF);
    *weight += w;
}

/// `Potion.calculate` folded with `PotionContents.getColorOr`/`getColorOptional`:
/// the opaque ARGB a `minecraft:potion_contents` component resolves to.
///
/// `potion` is the wire's `minecraft:potion` registry id (its built-in effect list is
/// looked up from [`POTION_EFFECTS`]); `custom_color` and `custom_effects` are the
/// component's own `customColor`/`customEffects` fields, `custom_effects` as
/// `(network mob-effect id, amplifier)` pairs in wire order. `custom_color` wins
/// outright when present (`getColorOr`'s first branch); otherwise every effect from
/// both the potion and the custom list is averaged, weighted by `amplifier + 1`
/// (`getColorOptional`); an empty result (no potion holder, no custom effects) is
/// [`BASE_POTION_COLOR`], matching `PotionContents.getColor()`'s own no-arg default.
#[must_use]
pub fn potion_color(potion: Option<i32>, custom_color: Option<u32>, custom_effects: &[(i32, u8)]) -> u32 {
    if let Some(c) = custom_color {
        return opaque(c);
    }
    let mut red = 0;
    let mut green = 0;
    let mut blue = 0;
    let mut weight = 0;
    if let Some(id) = potion.and_then(|id| usize::try_from(id).ok())
        && let Some(effects) = POTION_EFFECTS.get(id)
    {
        for &(effect_index, amplifier) in *effects {
            let effect_id = i32::try_from(effect_index).unwrap_or(-1);
            accumulate(effect_id, amplifier, &mut red, &mut green, &mut blue, &mut weight);
        }
    }
    for &(effect_id, amplifier) in custom_effects {
        accumulate(effect_id, amplifier, &mut red, &mut green, &mut blue, &mut weight);
    }
    if weight == 0 {
        opaque(BASE_POTION_COLOR)
    } else {
        opaque(((red / weight) << 16) | ((green / weight) << 8) | (blue / weight))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potion_name_and_id_round_trip_every_entry() {
        for id in 0..POTION_COUNT as i32 {
            let name = potion_name(id).unwrap_or_else(|| panic!("no name for potion id {id}"));
            assert_eq!(potion_id(name), Some(id), "{name}");
        }
        assert_eq!(potion_name(-1), None);
        assert_eq!(potion_name(POTION_COUNT as i32), None);
        assert_eq!(potion_id("minecraft:not_a_potion"), None);
    }

    /// `PotionContents.BASE_POTION_COLOR = -13083194`, cross-checked the same way
    /// `lodestone_assets::item_tint::defaults::POTION_BASE` is.
    #[test]
    fn base_potion_color_matches_the_jar_constant() {
        assert_eq!(BASE_POTION_COLOR as i32, -13_083_194);
    }

    /// A water bottle: `potion: Some(water)`, no custom color, no custom effects.
    /// `water`'s own `Potion("water")` constructor takes no `MobEffectInstance`, so
    /// `getAllEffects()` is empty and `getColorOptional` returns `OptionalInt.empty()`
    /// — the *control*: it legitimately resolves to the base colour, proving the gate
    /// below is not simply asserting "not the default" for everything.
    #[test]
    fn water_bottle_is_the_base_colour_control() {
        let water = potion_id("minecraft:water").unwrap();
        assert_eq!(potion_color(Some(water), None, &[]), opaque(BASE_POTION_COLOR));
    }

    /// Two potions whose expected colours are computed independently, straight from
    /// `MobEffects.java`'s own constants, and land far apart from each other and from
    /// the base colour — the discriminating pair. `swiftness` is a single `speed`
    /// effect at amplifier 0, so its colour is `speed`'s own `0x3402751`. `strong_
    /// harming` is a single `instant_damage` effect at amplifier 1, so its colour is
    /// `instant_damage`'s own `0xA9656A` unweighted-averaged against nothing else (one
    /// effect, any weight, divides back out to itself).
    #[test]
    fn swiftness_and_strong_harming_resolve_to_their_own_effect_colours() {
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let strong_harming = potion_id("minecraft:strong_harming").unwrap();

        let swiftness_color = potion_color(Some(swiftness), None, &[]);
        let harming_color = potion_color(Some(strong_harming), None, &[]);

        assert_eq!(swiftness_color, opaque(0x33_EBFF), "MobEffects.SPEED's own colour");
        assert_eq!(harming_color, opaque(0xA9_656A), "MobEffects.INSTANT_DAMAGE's own colour");

        // Both must differ from each other and from the water-bottle control by more
        // than a rounding error — the *magnitude* check, not just a sign.
        let base = opaque(BASE_POTION_COLOR);
        let channel_delta = |a: u32, b: u32| {
            let d = |s: u32| ((a >> s) & 0xFF) as i32 - ((b >> s) & 0xFF) as i32;
            d(16).abs() + d(8).abs() + d(0).abs()
        };
        assert!(channel_delta(swiftness_color, harming_color) > 60, "the two potions must visibly differ");
        assert!(channel_delta(swiftness_color, base) > 60, "swiftness must visibly differ from the base colour");
        assert!(channel_delta(harming_color, base) > 60, "strong harming must visibly differ from the base colour");
    }

    /// `turtle_master` mixes two effects (`slowness` amplifier 3, `resistance`
    /// amplifier 2) at different weights — the case a single-effect potion cannot
    /// exercise: the average must be weight-biased toward `slowness`, not a plain
    /// 50/50 mean of the two channel values.
    #[test]
    fn turtle_master_weights_by_amplifier_plus_one() {
        let turtle_master = potion_id("minecraft:turtle_master").unwrap();
        let mixed = potion_color(Some(turtle_master), None, &[]);

        // slowness = 0x8BAFE0 (weight 4), resistance = 0x9146F0 (weight 3).
        let (sr, sg, sb) = (0x8B, 0xAF, 0xE0);
        let (rr, rg, rb) = (0x91, 0x46, 0xF0);
        let (w_s, w_r) = (4u32, 3u32);
        let expected = opaque(
            (((sr * w_s + rr * w_r) / (w_s + w_r)) << 16)
                | (((sg * w_s + rg * w_r) / (w_s + w_r)) << 8)
                | ((sb * w_s + rb * w_r) / (w_s + w_r)),
        );
        assert_eq!(mixed, expected);

        // Not the plain (unweighted) average, which would land on a different value —
        // the discriminating assertion between "weighted" and "just averaged".
        let unweighted = opaque((((sr + rr) / 2) << 16) | (((sg + rg) / 2) << 8) | ((sb + rb) / 2));
        assert_ne!(mixed, unweighted, "the wrong hypothesis (plain average) must not coincide");
    }

    /// A `custom_color` component always wins, regardless of any effect list —
    /// `getColorOr`'s first branch, `Optional::isPresent`.
    #[test]
    fn custom_color_overrides_every_effect() {
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let custom = potion_color(Some(swiftness), Some(0x00FF_0000), &[]);
        assert_eq!(custom, 0xFFFF_0000);
    }

    /// `custom_effects` are appended to the potion's own list (`getAllEffects`), not
    /// substituted for it — a bare `minecraft:potion` id with no holder plus one
    /// custom effect must average that one effect alone.
    #[test]
    fn custom_effects_apply_with_no_potion_holder() {
        // mob effect id 0 = speed (see `MOB_EFFECT_NAMES`), amplifier 0.
        let color = potion_color(None, None, &[(0, 0)]);
        assert_eq!(color, opaque(0x33_EBFF));
    }
}
