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
use crate::generated_potion_effect_keys::POTION_EFFECT_KEYS;
use crate::generated_potions::POTION_NAMES;

pub use crate::generated_potion_effects::POTION_EFFECTS;
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
        for &(effect_index, amplifier, _duration_ticks) in *effects {
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

/// `Potion.name()` for a network potion registry id — see
/// [`crate::generated_potion_effect_keys`]'s module doc for why this is not the same
/// string as [`potion_name`]'s registry path (`long_swiftness`/`strong_swiftness` both
/// resolve to `"swiftness"` here, matching every duration/potency variant sharing one
/// item name in vanilla).
#[must_use]
pub fn potion_effect_key(id: i32) -> Option<&'static str> {
    usize::try_from(id).ok().and_then(|index| POTION_EFFECT_KEYS.get(index).copied())
}

/// `PotionContents.getName(prefix)` (`PotionContents.java`), ported as a literal table
/// rather than composed from `"<prefix> of <effect>"` — four keys (`water`/`mundane`/
/// `thick`/`awkward`) don't follow that pattern at all, `turtle_master` inserts `"the"`,
/// and `tipped_arrow`'s wording ("Arrow of X", "Arrow of Splashing" for `water`) differs
/// from the three drinkable prefixes. Transcribed verbatim from
/// `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json`'s
/// `item.minecraft.<base_item>.effect.<key>` entries.
///
/// `base_item` is the bare path (`"potion"`, `"splash_potion"`, `"lingering_potion"`,
/// `"tipped_arrow"`) — a `minecraft:` prefix, if the caller has one, must be stripped
/// first. `None` for an unrecognised `base_item` or an `id` outside `0..POTION_COUNT`.
#[must_use]
pub fn potion_item_display_name(base_item: &str, id: i32) -> Option<&'static str> {
    let key = potion_effect_key(id)?;
    let index = ["water", "mundane", "thick", "awkward", "night_vision", "invisibility", "leaping", "fire_resistance", "swiftness", "slowness", "turtle_master", "water_breathing", "healing", "harming", "poison", "regeneration", "strength", "weakness", "luck", "slow_falling", "wind_charged", "weaving", "oozing", "infested"]
        .iter()
        .position(|candidate| *candidate == key)?;
    let names: [&str; 24] = match base_item {
        "potion" => [
            "Water Bottle", "Mundane Potion", "Thick Potion", "Awkward Potion",
            "Potion of Night Vision", "Potion of Invisibility", "Potion of Leaping",
            "Potion of Fire Resistance", "Potion of Swiftness", "Potion of Slowness",
            "Potion of the Turtle Master", "Potion of Water Breathing", "Potion of Healing",
            "Potion of Harming", "Potion of Poison", "Potion of Regeneration",
            "Potion of Strength", "Potion of Weakness", "Potion of Luck",
            "Potion of Slow Falling", "Potion of Wind Charging", "Potion of Weaving",
            "Potion of Oozing", "Potion of Infestation",
        ],
        "splash_potion" => [
            "Splash Water Bottle", "Mundane Splash Potion", "Thick Splash Potion", "Awkward Splash Potion",
            "Splash Potion of Night Vision", "Splash Potion of Invisibility", "Splash Potion of Leaping",
            "Splash Potion of Fire Resistance", "Splash Potion of Swiftness", "Splash Potion of Slowness",
            "Splash Potion of the Turtle Master", "Splash Potion of Water Breathing", "Splash Potion of Healing",
            "Splash Potion of Harming", "Splash Potion of Poison", "Splash Potion of Regeneration",
            "Splash Potion of Strength", "Splash Potion of Weakness", "Splash Potion of Luck",
            "Splash Potion of Slow Falling", "Splash Potion of Wind Charging", "Splash Potion of Weaving",
            "Splash Potion of Oozing", "Splash Potion of Infestation",
        ],
        "lingering_potion" => [
            "Lingering Water Bottle", "Mundane Lingering Potion", "Thick Lingering Potion", "Awkward Lingering Potion",
            "Lingering Potion of Night Vision", "Lingering Potion of Invisibility", "Lingering Potion of Leaping",
            "Lingering Potion of Fire Resistance", "Lingering Potion of Swiftness", "Lingering Potion of Slowness",
            "Lingering Potion of the Turtle Master", "Lingering Potion of Water Breathing", "Lingering Potion of Healing",
            "Lingering Potion of Harming", "Lingering Potion of Poison", "Lingering Potion of Regeneration",
            "Lingering Potion of Strength", "Lingering Potion of Weakness", "Lingering Potion of Luck",
            "Lingering Potion of Slow Falling", "Lingering Potion of Wind Charging", "Lingering Potion of Weaving",
            "Lingering Potion of Oozing", "Lingering Potion of Infestation",
        ],
        "tipped_arrow" => [
            "Arrow of Splashing", "Tipped Arrow", "Tipped Arrow", "Tipped Arrow",
            "Arrow of Night Vision", "Arrow of Invisibility", "Arrow of Leaping",
            "Arrow of Fire Resistance", "Arrow of Swiftness", "Arrow of Slowness",
            "Arrow of the Turtle Master", "Arrow of Water Breathing", "Arrow of Healing",
            "Arrow of Harming", "Arrow of Poison", "Arrow of Regeneration",
            "Arrow of Strength", "Arrow of Weakness", "Arrow of Luck",
            "Arrow of Slow Falling", "Arrow of Wind Charging", "Arrow of Weaving",
            "Arrow of Oozing", "Arrow of Infestation",
        ],
        _ => return None,
    };
    names.get(index).copied()
}

/// One line `PotionContents.addPotionTooltip` emits for a single effect: the effect's
/// own vanilla display name (`effect.minecraft.<path>` in `en_us.json`), its amplifier
/// (`MobEffectInstance.getAmplifier()` — `0` means no Roman-numeral suffix, matching
/// `getPotionDescription`'s `amplifier > 0` gate), and its raw, unscaled duration in
/// ticks (`MobEffectUtil.formatDuration`'s input before `POTION_DURATION_SCALE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PotionEffectEntry {
    /// `effect.minecraft.<path>`'s English text, e.g. `"Swiftness"`.
    pub effect_name: &'static str,
    /// `0` renders no numeral; vanilla's `potion.potency.<n>` table (`""`, `"II"`,
    /// `"III"`, …) starts numbering at this same raw amplifier, not `amplifier + 1`.
    pub amplifier: u8,
    /// Raw duration in ticks, unscaled. Vanilla only prints a duration when
    /// `!effect.endsWithin(20)`, i.e. `duration_ticks > 20` — an instant effect
    /// (`healing`/`harming`, `duration_ticks == 1`) prints no duration at all.
    pub duration_ticks: u32,
    /// `MobEffectCategory.getTooltipFormatting()`: `true` for `HARMFUL`
    /// (`ChatFormatting.RED`), `false` for `BENEFICIAL`/`NEUTRAL` (both
    /// `ChatFormatting.BLUE`) — `MobEffects.java`'s own category argument per
    /// effect, not derived from anything else.
    pub harmful: bool,
}

/// `effect.minecraft.<path>` plus its `MobEffectCategory` (`en_us.json` and
/// `MobEffects.java` respectively), for exactly the mob effects a potion in this
/// build's 46-entry registry can carry — scoped rather than a general
/// `minecraft:mob_effect` name table, because no other caller in this tree needs a
/// status effect's *display* name (only [`crate::mob_effects::mob_effect_name`]'s
/// canonical identifier). Indexed by the same `mob_effect_index` [`POTION_EFFECTS`]
/// uses. `harmful` is `true` for `MobEffectCategory::HARMFUL`.
const EFFECT_DISPLAY_NAMES: &[(usize, &str, bool)] = &[
    (0, "Speed", false),
    (1, "Slowness", true),
    (4, "Strength", false),
    (5, "Instant Health", false),
    (6, "Instant Damage", true),
    (7, "Jump Boost", false),
    (9, "Regeneration", false),
    (10, "Resistance", false),
    (11, "Fire Resistance", false),
    (12, "Water Breathing", false),
    (13, "Invisibility", false),
    (15, "Night Vision", false),
    (17, "Weakness", true),
    (18, "Poison", true),
    (25, "Luck", false),
    (27, "Slow Falling", false),
    (35, "Wind Charged", true),
    (36, "Weaving", true),
    (37, "Oozing", true),
    (38, "Infested", true),
];

/// The raw `(mob_effect_index, amplifier, base_duration_ticks)` triples backing
/// [`potion_effect_entries`], for a caller that needs each entry's *canonical*
/// mob-effect id (via [`crate::mob_effects::mob_effect_name`], called with
/// `effect_index as i32`) rather than [`potion_effect_entries`]'s display name —
/// `crate::mob_effects` is a network id->identifier resolver and this index is
/// exactly a network id (see [`POTION_EFFECTS`]'s own doc comment). `None` for
/// an id outside `0..POTION_COUNT`, matching every other lookup in this module.
#[must_use]
pub fn potion_built_in_effects(id: i32) -> Option<&'static [(usize, u8, u32)]> {
    usize::try_from(id).ok().and_then(|index| POTION_EFFECTS.get(index)).copied()
}

/// `PotionContents.getAllEffects()` with no `customEffects`, resolved to display data —
/// a potion registry entry's own built-in effect list, in `Potions.java`'s declaration
/// order (which is also `addPotionTooltip`'s iteration order). Empty for `water`/
/// `mundane`/`thick`/`awkward` and for an id outside `0..POTION_COUNT`, matching
/// `PotionContents.hasEffects() == false` (vanilla's cue to print `"No Effects"`
/// instead).
#[must_use]
pub fn potion_effect_entries(id: i32) -> Vec<PotionEffectEntry> {
    let Some(effects) = usize::try_from(id).ok().and_then(|index| POTION_EFFECTS.get(index)) else {
        return Vec::new();
    };
    effects
        .iter()
        .map(|&(effect_index, amplifier, duration_ticks)| {
            let (effect_name, harmful) = EFFECT_DISPLAY_NAMES
                .iter()
                .find(|&&(idx, _, _)| idx == effect_index)
                .map_or(("", false), |&(_, name, harmful)| (name, harmful));
            PotionEffectEntry { effect_name, amplifier, duration_ticks, harmful }
        })
        .collect()
}

/// One line under `potion.whenDrank` (`"When Applied:"`) — an attribute the effect
/// modifies while active, already scaled by `MobEffect.AttributeTemplate.create`'s
/// `amount * (amplifier + 1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttributeModifierEntry {
    /// `attribute.name.<id>`'s English text, e.g. `"Speed"`, `"Attack Damage"`.
    pub attribute_name: &'static str,
    /// The scaled amount, sign-carrying. Positive prints under
    /// `attribute.modifier.plus.*`, negative under `attribute.modifier.take.*`
    /// (with the sign stripped for display — the template supplies the `+`/`-`).
    pub amount: f64,
    /// `true` for `AttributeModifier::Operation::ADD_MULTIPLIED_TOTAL` (vanilla
    /// multiplies the display amount by 100 and suffixes `%`); `false` for
    /// `ADD_VALUE` (the raw amount, no suffix). No potion effect in this build's
    /// registry uses `ADD_MULTIPLIED_BASE`, so that third operation is not modelled.
    pub percent: bool,
}

/// `MobEffect.attributeModifiers` (`MobEffects.java`'s `.addAttributeModifier(...)`
/// chain) for exactly the effects a potion in this build's registry can carry.
/// `(mob_effect_index, attribute_name, base_amount, percent)`; `base_amount` is the
/// *unscaled* `AttributeModifier` constructor argument, scaled per-instance by
/// [`potion_attribute_modifiers`].
const EFFECT_ATTRIBUTE_MODIFIERS: &[(usize, &str, f64, bool)] = &[
    (0, "Speed", 0.2, true),                    // speed
    (1, "Speed", -0.15, true),                  // slowness
    (4, "Attack Damage", 3.0, false),            // strength
    (7, "Safe Fall Distance", 1.0, false),       // jump_boost
    (13, "Waypoint Transmit Range", -1.0, true), // invisibility
    (17, "Attack Damage", -4.0, false),          // weakness
    (25, "Luck", 1.0, false),                    // luck
];

/// `MobEffect.createModifiers` for one potion registry entry's built-in effect
/// list — the `"When Applied:"` section `PotionContents.addPotionTooltip` appends
/// after the effect lines, when at least one effect carries an attribute modifier.
/// Empty when none of the potion's effects modify an attribute (most of them: only
/// `speed`/`slowness`/`strength`/`weakness`/`luck`/`jump_boost`/`invisibility` do).
#[must_use]
pub fn potion_attribute_modifiers(id: i32) -> Vec<AttributeModifierEntry> {
    let Some(effects) = usize::try_from(id).ok().and_then(|index| POTION_EFFECTS.get(index)) else {
        return Vec::new();
    };
    effects
        .iter()
        .filter_map(|&(effect_index, amplifier, _duration_ticks)| {
            EFFECT_ATTRIBUTE_MODIFIERS
                .iter()
                .find(|&&(idx, _, _, _)| idx == effect_index)
                .map(|&(_, attribute_name, base_amount, percent)| AttributeModifierEntry {
                    attribute_name,
                    amount: base_amount * f64::from(u32::from(amplifier) + 1),
                    percent,
                })
        })
        .collect()
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

    /// The two discriminating potions from `en_us.json`, transcribed independently
    /// of [`potion_item_display_name`]'s own table — `swiftness` follows the regular
    /// `"<Prefix> of <Effect>"` shape, `turtle_master` is the one entry that inserts
    /// `"the"`, so a formula that only handles the regular case fails here, not there.
    /// Checked against all four item families at once, including `tipped_arrow`'s
    /// differently-worded set.
    #[test]
    fn item_display_name_covers_the_regular_and_irregular_shapes() {
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let turtle_master = potion_id("minecraft:turtle_master").unwrap();

        let mut mismatches = Vec::new();
        let cases: &[(&str, i32, &str)] = &[
            ("potion", swiftness, "Potion of Swiftness"),
            ("splash_potion", swiftness, "Splash Potion of Swiftness"),
            ("lingering_potion", swiftness, "Lingering Potion of Swiftness"),
            ("tipped_arrow", swiftness, "Arrow of Swiftness"),
            ("potion", turtle_master, "Potion of the Turtle Master"),
            ("splash_potion", turtle_master, "Splash Potion of the Turtle Master"),
            ("lingering_potion", turtle_master, "Lingering Potion of the Turtle Master"),
            ("tipped_arrow", turtle_master, "Arrow of the Turtle Master"),
        ];
        for &(base, id, expected) in cases {
            let actual = potion_item_display_name(base, id);
            if actual != Some(expected) {
                mismatches.push(format!("{base}#{id}: expected {expected:?}, got {actual:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// `long_swiftness`/`strong_swiftness` must resolve to the exact same item name
    /// as plain `swiftness` — vanilla's `Potion.name()` collapses every duration and
    /// potency variant onto one key, so the title never carries a Roman numeral or a
    /// "Long" qualifier; only the tooltip's effect line does.
    #[test]
    fn duration_and_potency_variants_share_one_item_name() {
        let base = potion_item_display_name("potion", potion_id("minecraft:swiftness").unwrap());
        let long = potion_item_display_name("potion", potion_id("minecraft:long_swiftness").unwrap());
        let strong = potion_item_display_name("potion", potion_id("minecraft:strong_swiftness").unwrap());
        assert_eq!(base, Some("Potion of Swiftness"));
        assert_eq!(base, long);
        assert_eq!(base, strong);
    }

    /// The four non-`"of"` names and `tipped_arrow`'s irregular `water` wording —
    /// every entry that a generic `"<Prefix> of <Effect>"` formula would get wrong.
    #[test]
    fn irregular_names_do_not_follow_the_of_pattern() {
        let water = potion_id("minecraft:water").unwrap();
        let mundane = potion_id("minecraft:mundane").unwrap();
        assert_eq!(potion_item_display_name("potion", water), Some("Water Bottle"));
        assert_eq!(potion_item_display_name("splash_potion", water), Some("Splash Water Bottle"));
        assert_eq!(potion_item_display_name("tipped_arrow", water), Some("Arrow of Splashing"));
        assert_eq!(potion_item_display_name("potion", mundane), Some("Mundane Potion"));
        assert_eq!(potion_item_display_name("tipped_arrow", mundane), Some("Tipped Arrow"));
    }

    /// The water-bottle control for the *effect list*, not just the colour: a water
    /// bottle carries no `MobEffectInstance` at all, so `addPotionTooltip` takes its
    /// `noEffects` branch and prints `"No Effects"` — this must be an empty entry
    /// list, not a default/placeholder entry, proving the empty case is deliberate.
    #[test]
    fn water_bottle_has_no_effect_entries() {
        let water = potion_id("minecraft:water").unwrap();
        assert_eq!(potion_effect_entries(water), Vec::new());
        assert_eq!(potion_attribute_modifiers(water), Vec::new());
    }

    /// Amplifier 0 vs non-zero, the arm `getPotionDescription`'s `amplifier > 0` gate
    /// gets wrong most often — a fixture using only amplifier 1 cannot see a build
    /// that always renders a numeral. `swiftness` (amplifier 0) and `strong_swiftness`
    /// (amplifier 1) are the same effect, so amplifier is the only thing that differs.
    #[test]
    fn amplifier_zero_is_distinguished_from_nonzero() {
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let strong = potion_id("minecraft:strong_swiftness").unwrap();
        let base = potion_effect_entries(swiftness);
        let strong_entries = potion_effect_entries(strong);
        assert_eq!(base.len(), 1);
        assert_eq!(strong_entries.len(), 1);
        assert_eq!(base[0].amplifier, 0);
        assert_eq!(strong_entries[0].amplifier, 1);
        assert_eq!(base[0].effect_name, "Speed");
        assert_eq!(strong_entries[0].effect_name, "Speed");
    }

    /// `strong_slowness` and `strong_turtle_master`'s `slowness` component are the two
    /// highest amplifiers present in this build's registry (3 and 5) — cross-checked
    /// against `Potions.java`'s own constructor arguments rather than guessed.
    #[test]
    fn high_amplifiers_are_carried_through_unmodified() {
        let strong_slowness = potion_id("minecraft:strong_slowness").unwrap();
        let strong_turtle_master = potion_id("minecraft:strong_turtle_master").unwrap();
        let slowness_entries = potion_effect_entries(strong_slowness);
        let turtle_entries = potion_effect_entries(strong_turtle_master);
        assert_eq!(slowness_entries, vec![PotionEffectEntry { effect_name: "Slowness", amplifier: 3, duration_ticks: 400, harmful: true }]);
        let mut mismatches = Vec::new();
        if !turtle_entries.contains(&PotionEffectEntry { effect_name: "Slowness", amplifier: 5, duration_ticks: 400, harmful: true }) {
            mismatches.push("missing slowness component");
        }
        if !turtle_entries.contains(&PotionEffectEntry { effect_name: "Resistance", amplifier: 3, duration_ticks: 400, harmful: false }) {
            mismatches.push("missing resistance component");
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// An instant effect's duration (`healing`/`harming`, `1` tick) is far below
    /// vanilla's `endsWithin(20)` cutoff — the discriminating input for "only print a
    /// duration when the effect actually lasts": a fixture built from `swiftness`
    /// alone (3600 ticks) cannot see a formatter that always prints a duration.
    #[test]
    fn instant_effect_duration_is_below_the_display_cutoff() {
        let healing = potion_id("minecraft:healing").unwrap();
        let entries = potion_effect_entries(healing);
        assert_eq!(entries, vec![PotionEffectEntry { effect_name: "Instant Health", amplifier: 0, duration_ticks: 1, harmful: false }]);
        assert!(entries[0].duration_ticks <= 20, "must fall inside endsWithin(20)");
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let sustained = potion_effect_entries(swiftness);
        assert!(sustained[0].duration_ticks > 20, "must fall outside endsWithin(20), the control");
    }

    /// Two potions whose attribute-modifier sections must differ: `swiftness` scales
    /// a percentage (`ADD_MULTIPLIED_TOTAL`), `strength` a flat value (`ADD_VALUE`) —
    /// and `night_vision` (an effect with no attribute modifier at all) must yield an
    /// empty list despite carrying a real effect, the control that separates "no
    /// modifiers" from "no effects".
    #[test]
    fn attribute_modifiers_distinguish_percent_from_flat_and_from_none() {
        let swiftness = potion_id("minecraft:swiftness").unwrap();
        let strong_swiftness = potion_id("minecraft:strong_swiftness").unwrap();
        let strength = potion_id("minecraft:strength").unwrap();
        let night_vision = potion_id("minecraft:night_vision").unwrap();

        let speed_mods = potion_attribute_modifiers(swiftness);
        assert_eq!(speed_mods, vec![AttributeModifierEntry { attribute_name: "Speed", amount: 0.2, percent: true }]);

        // Amplifier scaling: strong_swiftness is amplifier 1, so 0.2 * (1 + 1) = 0.4,
        // not the base 0.2 — the discriminator between "scaled" and "constant".
        let strong_speed_mods = potion_attribute_modifiers(strong_swiftness);
        assert_eq!(strong_speed_mods, vec![AttributeModifierEntry { attribute_name: "Speed", amount: 0.4, percent: true }]);

        let strength_mods = potion_attribute_modifiers(strength);
        assert_eq!(strength_mods, vec![AttributeModifierEntry { attribute_name: "Attack Damage", amount: 3.0, percent: false }]);

        assert_eq!(potion_attribute_modifiers(night_vision), Vec::new(), "an effect with no attribute modifier must yield none");
    }
}
