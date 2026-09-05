//! Public potion id resolution and the `minecraft:potion_contents` colour-mixing formula.
//!
//! # What it is
//!
//! `minecraft:potion_contents`' `potion` field carries a `minecraft:potion` registry
//! VarInt id — a fixed, built-in registry, the same kind of static id->name table as
//! [`crate::mob_effects`]. [`potion_name`]/[`potion_id`] resolve it; [`potion_color`]
//! implements the weighted-average colour mix a potion bottle's tint is drawn from.
//!
//! # How it works
//!
//! The colour resolution checks a custom override colour first; failing that, it folds
//! every effect in the potion's combined effect list (the potion's own built-in list,
//! from [`crate::generated_potion_effects::POTION_EFFECTS`], concatenated with any
//! custom effects) into a red/green/blue running sum weighted by `amplifier + 1`,
//! divides by the total weight, and falls back to the base potion colour constant
//! (`-13083194`) when there were no effects at all. Every mob-effect colour is
//! [`crate::generated_mob_effect_colors::MOB_EFFECT_COLORS`], and each effect entry's
//! own id is itself a network `minecraft:mob_effect` id (the same 0-based registry
//! shape `minecraft:potion` uses), so no extra indirection is needed between a wire
//! effect id and this table's index.
//!
//! # How to change it
//!
//! If a future protocol version renumbers `minecraft:potion` or `minecraft:mob_effect`,
//! regenerate [`crate::generated_potions`] and [`crate::generated_mob_effect_colors`]
//! from the new `registries.json` / decompile and this module needs no change — it
//! only ever indexes those tables.

use crate::generated_mob_effect_colors::MOB_EFFECT_COLORS;
use crate::generated_potion_effect_ids::POTION_EFFECT_BASE_IDS;
use crate::generated_potions::POTION_NAMES;

pub use crate::generated_potion_effects::POTION_EFFECTS;
pub use crate::generated_potions::POTION_COUNT;

/// A validated built-in `minecraft:potion` registry id for the canonical 26.2
/// data census.
///
/// The wire codec is still an `i32` VarInt and the version-free item model
/// retains that raw value, including an unrecognised one. Convert it at a
/// consumer's built-in-census boundary with [`Self::from_registry_id`]: an
/// unknown plugin, datapack, malformed, or future id then fails closed for
/// built-in lookup without being rewritten or mistaken for a known potion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PotionId(u8);

impl PotionId {
    /// Validates a wire `minecraft:potion` registry id against this census.
    #[must_use]
    pub fn from_registry_id(id: i32) -> Option<Self> {
        let id = u8::try_from(id).ok()?;
        (usize::from(id) < POTION_NAMES.len()).then_some(Self(id))
    }

    /// Returns this potion's wire registry id.
    #[must_use]
    pub const fn registry_id(self) -> i32 {
        self.0 as i32
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The base potion colour constant (`-13083194`), opaque ARGB, used when a potion has
/// no colour-contributing effects.
pub const BASE_POTION_COLOR: u32 = 0xFF38_5DC6;

/// Resolves a validated potion registry id to its canonical `minecraft:*`
/// identifier.
#[must_use]
pub fn potion_name(id: PotionId) -> &'static str {
    POTION_NAMES[id.index()]
}

/// Resolves a canonical `minecraft:*` potion identifier to its network registry id
/// for protocol 776. The reverse of [`potion_name`].
///
/// This raw output is retained for version-free model storage; validate it with
/// [`PotionId::from_registry_id`] before passing it back to a built-in census lookup.
#[must_use]
pub fn potion_id(name: &str) -> Option<i32> {
    POTION_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| i32::try_from(index).ok())
}

/// Forces alpha to `0xFF` (opaque).
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

/// The weighted effect-colour average that a `minecraft:potion_contents` component
/// resolves to, with an explicit custom colour taking precedence over any computed mix.
///
/// `potion` is a validated built-in `minecraft:potion` registry id (its built-in
/// effect list is looked up from [`POTION_EFFECTS`]); `custom_color` and
/// `custom_effects` are the component's own custom-colour and custom-effects fields,
/// `custom_effects` as `(network mob-effect id, amplifier)` pairs in wire order.
/// `custom_color` wins outright when present; otherwise every effect from both the
/// potion and the custom list is averaged, weighted by `amplifier + 1`; an empty result
/// (no potion holder, no custom effects) is [`BASE_POTION_COLOR`].
#[must_use]
pub fn potion_color(
    potion: Option<PotionId>,
    custom_color: Option<u32>,
    custom_effects: &[(i32, u8)],
) -> u32 {
    if let Some(c) = custom_color {
        return opaque(c);
    }
    let mut red = 0;
    let mut green = 0;
    let mut blue = 0;
    let mut weight = 0;
    if let Some(id) = potion {
        let effects = &POTION_EFFECTS[id.index()];
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

/// The base effect-key name for a validated potion registry id.
///
/// [`POTION_EFFECT_BASE_IDS`] explicitly maps each duration/potency variant to the
/// registry id of its base potion; the returned path then comes from the canonical
/// [`POTION_NAMES`] column. No alias is inferred from a `long_` or `strong_` prefix.
#[must_use]
pub fn potion_effect_key(id: PotionId) -> &'static str {
    let base_id = usize::from(POTION_EFFECT_BASE_IDS[id.index()]);
    POTION_NAMES[base_id]
        .strip_prefix("minecraft:")
        .expect("generated potion names are minecraft-namespaced")
}

/// The display-name-with-prefix formula, ported as a literal table rather than
/// composed from `"<prefix> of <effect>"` — four keys (`water`/`mundane`/`thick`/
/// `awkward`) don't follow that pattern at all, `turtle_master` inserts `"the"`, and
/// `tipped_arrow`'s wording ("Arrow of X", "Arrow of Splashing" for `water`) differs
/// from the three drinkable prefixes. Transcribed verbatim from the game's own English
/// localisation strings for each base item's per-effect display name
/// (`item.minecraft.<base_item>.effect.<key>` entries).
///
/// `base_item` is the bare path (`"potion"`, `"splash_potion"`, `"lingering_potion"`,
/// `"tipped_arrow"`) — a `minecraft:` prefix, if the caller has one, must be stripped
/// first. `None` for an unrecognised `base_item`.
#[must_use]
pub fn potion_item_display_name(base_item: &str, id: PotionId) -> Option<&'static str> {
    let key = potion_effect_key(id);
    potion_item_display_name_for_key(base_item, key)
}

/// Resolves a potion-family item title from an explicit effect-name suffix.
///
/// The component-local custom name uses this path before the potion registry
/// id. `"empty"` is the no-holder fallback. Unknown suffixes return `None`
/// because this crate carries only the built-in English localisation entries.
#[must_use]
pub fn potion_item_display_name_for_key(
    base_item: &str,
    key: &str,
) -> Option<&'static str> {
    if key == "empty" {
        return match base_item {
            "potion" => Some("Uncraftable Potion"),
            "splash_potion" => Some("Splash Uncraftable Potion"),
            "lingering_potion" => Some("Lingering Uncraftable Potion"),
            "tipped_arrow" => Some("Uncraftable Tipped Arrow"),
            _ => None,
        };
    }
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

/// One line the potion tooltip emits for a single effect: the effect's own vanilla
/// display name (`effect.minecraft.<path>` in the localisation strings), its amplifier
/// (`0` means no Roman-numeral suffix, matching the description formatter's
/// `amplifier > 0` gate), and its raw, unscaled duration in ticks (before the
/// duration-display scale factor that converts ticks to seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PotionEffectEntry {
    /// `effect.minecraft.<path>`'s English text, e.g. `"Swiftness"`.
    pub effect_name: &'static str,
    /// `0` renders no numeral; vanilla's `potion.potency.<n>` table (`""`, `"II"`,
    /// `"III"`, …) starts numbering at this same raw amplifier, not `amplifier + 1`.
    pub amplifier: u8,
    /// Raw duration in ticks, unscaled. Vanilla only prints a duration when it exceeds
    /// a 20-tick "ends within" cutoff, i.e. `duration_ticks > 20` — an instant effect
    /// (`healing`/`harming`, `duration_ticks == 1`) prints no duration at all.
    pub duration_ticks: u32,
    /// Vanilla's tooltip-colour category: `true` for a harmful effect (rendered red),
    /// `false` for a beneficial or neutral one (both rendered blue) — a fixed
    /// per-effect category, not derived from anything else.
    pub harmful: bool,
}

/// Each built-in effect's English display name plus harmful category, indexed by
/// the same network id as [`crate::mob_effects::mob_effect_name`]. Custom potion
/// effects can reference any registry entry, not only the subset used by built-in
/// potions, so this table covers the complete protocol-776 registry.
const EFFECT_DISPLAY_NAMES: &[(usize, &str, bool)] = &[
    (0, "Speed", false),
    (1, "Slowness", true),
    (2, "Haste", false),
    (3, "Mining Fatigue", true),
    (4, "Strength", false),
    (5, "Instant Health", false),
    (6, "Instant Damage", true),
    (7, "Jump Boost", false),
    (8, "Nausea", true),
    (9, "Regeneration", false),
    (10, "Resistance", false),
    (11, "Fire Resistance", false),
    (12, "Water Breathing", false),
    (13, "Invisibility", false),
    (14, "Blindness", true),
    (15, "Night Vision", false),
    (16, "Hunger", true),
    (17, "Weakness", true),
    (18, "Poison", true),
    (19, "Wither", true),
    (20, "Health Boost", false),
    (21, "Absorption", false),
    (22, "Saturation", false),
    (23, "Glowing", false),
    (24, "Levitation", true),
    (25, "Luck", false),
    (26, "Bad Luck", true),
    (27, "Slow Falling", false),
    (28, "Conduit Power", false),
    (29, "Dolphin's Grace", false),
    (30, "Bad Omen", false),
    (31, "Hero of the Village", false),
    (32, "Darkness", true),
    (33, "Trial Omen", false),
    (34, "Raid Omen", false),
    (35, "Wind Charged", true),
    (36, "Weaving", true),
    (37, "Oozing", true),
    (38, "Infested", true),
    (39, "Breath of the Nautilus", false),
];

/// English tooltip name and harmful category for a network mob-effect id.
#[must_use]
pub fn mob_effect_tooltip(effect_id: i32) -> Option<(&'static str, bool)> {
    let effect_index = usize::try_from(effect_id).ok()?;
    EFFECT_DISPLAY_NAMES
        .iter()
        .find(|&&(index, _, _)| index == effect_index)
        .map(|&(_, name, harmful)| (name, harmful))
}

/// The raw `(mob_effect_index, amplifier, base_duration_ticks)` triples backing
/// [`potion_effect_entries`], for a caller that needs each entry's *canonical*
/// mob-effect id (via [`crate::mob_effects::mob_effect_name`], called with
/// `effect_index as i32`) rather than [`potion_effect_entries`]'s display name —
/// `crate::mob_effects` is a network id->identifier resolver and this index is
/// exactly a network id (see [`POTION_EFFECTS`]'s own doc comment). `None` for
/// a caller with an unrecognised raw wire value must validate with [`PotionId`]
/// before reaching this lookup, preserving that raw value in its owning component.
#[must_use]
pub fn potion_built_in_effects(id: PotionId) -> &'static [(usize, u8, u32)] {
    POTION_EFFECTS[id.index()]
}

/// A potion's built-in effect list with no custom effects applied, resolved to
/// display data — in the registry's own declaration order (which is also the
/// tooltip's iteration order). Empty for `water`/`mundane`/`thick`/`awkward` and for
/// a caller with an unrecognised raw wire value must validate with [`PotionId`]
/// before reaching this lookup, which makes an unknown holder behave like no
/// built-in potion rather than a guessed one.
#[must_use]
pub fn potion_effect_entries(id: PotionId) -> Vec<PotionEffectEntry> {
    POTION_EFFECTS[id.index()]
        .iter()
        .map(|&(effect_index, amplifier, duration_ticks)| {
            let (effect_name, harmful) = mob_effect_tooltip(effect_index as i32)
                .unwrap_or(("", false));
            PotionEffectEntry { effect_name, amplifier, duration_ticks, harmful }
        })
        .collect()
}

/// One line under `potion.whenDrank` (`"When Applied:"`) — an attribute the effect
/// modifies while active, already scaled by `amount * (amplifier + 1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttributeModifierEntry {
    /// `attribute.name.<id>`'s English text, e.g. `"Speed"`, `"Attack Damage"`.
    pub attribute_name: &'static str,
    /// The scaled amount, sign-carrying. Positive prints under
    /// `attribute.modifier.plus.*`, negative under `attribute.modifier.take.*`
    /// (with the sign stripped for display — the template supplies the `+`/`-`).
    pub amount: f64,
    /// `true` for a percentage-scaled modifier (vanilla multiplies the display amount
    /// by 100 and suffixes `%`); `false` for a flat additive modifier (the raw amount,
    /// no suffix). No potion effect in this build's registry uses the third operation
    /// kind (multiply-by-base), so it is not modelled here.
    pub percent: bool,
}

/// Each mob effect's attribute-modifier declarations, for exactly the effects a
/// potion in this build's registry can carry. `(mob_effect_index, attribute_name,
/// base_amount, percent)`; `base_amount` is the *unscaled* declared amount, scaled
/// per-instance by [`potion_attribute_modifiers`].
const EFFECT_ATTRIBUTE_MODIFIERS: &[(usize, &str, f64, bool)] = &[
    (0, "Speed", 0.2, true),                    // speed
    (1, "Speed", -0.15, true),                  // slowness
    (2, "Attack Speed", 0.1, true),              // haste
    (3, "Attack Speed", -0.1, true),             // mining_fatigue
    (4, "Attack Damage", 3.0, false),            // strength
    (7, "Safe Fall Distance", 1.0, false),       // jump_boost
    (13, "Waypoint Transmit Range", -1.0, true), // invisibility
    (17, "Attack Damage", -4.0, false),          // weakness
    (20, "Max Health", 4.0, false),               // health_boost
    (21, "Max Absorption", 4.0, false),           // absorption
    (25, "Luck", 1.0, false),                    // luck
    (26, "Luck", -1.0, false),                   // unluck
];

/// Attribute modifiers produced by one mob-effect instance at `amplifier`.
#[must_use]
pub fn mob_effect_attribute_modifiers(
    effect_id: i32,
    amplifier: u8,
) -> Vec<AttributeModifierEntry> {
    let Ok(effect_index) = usize::try_from(effect_id) else {
        return Vec::new();
    };
    EFFECT_ATTRIBUTE_MODIFIERS
        .iter()
        .filter(|&&(index, _, _, _)| index == effect_index)
        .map(|&(_, attribute_name, base_amount, percent)| AttributeModifierEntry {
            attribute_name,
            amount: base_amount * f64::from(u32::from(amplifier) + 1),
            percent,
        })
        .collect()
}

/// The resolved attribute modifiers for one potion registry entry's built-in effect
/// list — the `"When Applied:"` section the tooltip appends after the effect lines,
/// when at least one effect carries an attribute modifier. Empty when none of the
/// potion's effects modify an attribute (most of them: only
/// `speed`/`slowness`/`strength`/`weakness`/`luck`/`jump_boost`/`invisibility` do).
#[must_use]
pub fn potion_attribute_modifiers(id: PotionId) -> Vec<AttributeModifierEntry> {
    POTION_EFFECTS[id.index()]
        .iter()
        .flat_map(|&(effect_index, amplifier, _duration_ticks)| {
            mob_effect_attribute_modifiers(effect_index as i32, amplifier)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potion_name_and_id_round_trip_every_entry() {
        for id in 0..POTION_COUNT as i32 {
            let id = PotionId::from_registry_id(id).expect("id within generated potion census");
            let name = potion_name(id);
            assert_eq!(potion_id(name), Some(id.registry_id()), "{name}");
        }
        assert_eq!(PotionId::from_registry_id(-1), None);
        assert_eq!(PotionId::from_registry_id(POTION_COUNT as i32), None);
        assert_eq!(potion_id("minecraft:not_a_potion"), None);
    }

    /// The base potion colour constant is `-13083194`, cross-checked the same way
    /// `lodestone_assets::item_tint::defaults::POTION_BASE` is.
    #[test]
    fn base_potion_color_matches_the_jar_constant() {
        assert_eq!(BASE_POTION_COLOR as i32, -13_083_194);
    }

    /// A water bottle: `potion: Some(water)`, no custom color, no custom effects.
    /// `water` carries no built-in effect at all, so the effect list is empty and the
    /// colour mix legitimately has nothing to average — the *control*: it resolves to
    /// the base colour, proving the gate below is not simply asserting "not the
    /// default" for everything.
    #[test]
    fn water_bottle_is_the_base_colour_control() {
        let water = PotionId::from_registry_id(potion_id("minecraft:water").unwrap()).unwrap();
        assert_eq!(potion_color(Some(water), None, &[]), opaque(BASE_POTION_COLOR));
    }

    /// Two potions whose expected colours are computed independently, straight from
    /// vanilla's own per-effect colour constants, and land far apart from each other
    /// and from the base colour — the discriminating pair. `swiftness` is a single `speed`
    /// effect at amplifier 0, so its colour is `speed`'s own `0x3402751`. `strong_
    /// harming` is a single `instant_damage` effect at amplifier 1, so its colour is
    /// `instant_damage`'s own `0xA9656A` unweighted-averaged against nothing else (one
    /// effect, any weight, divides back out to itself).
    #[test]
    fn swiftness_and_strong_harming_resolve_to_their_own_effect_colours() {
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
        let strong_harming = PotionId::from_registry_id(potion_id("minecraft:strong_harming").unwrap()).unwrap();

        let swiftness_color = potion_color(Some(swiftness), None, &[]);
        let harming_color = potion_color(Some(strong_harming), None, &[]);

        assert_eq!(swiftness_color, opaque(0x33_EBFF), "the speed effect's own colour");
        assert_eq!(harming_color, opaque(0xA9_656A), "the instant-damage effect's own colour");

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
        let turtle_master = PotionId::from_registry_id(potion_id("minecraft:turtle_master").unwrap()).unwrap();
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
    /// vanilla's own "get color or" accessor's first branch, an early
    /// present-optional check.
    #[test]
    fn custom_color_overrides_every_effect() {
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
        let custom = potion_color(Some(swiftness), Some(0x00FF_0000), &[]);
        assert_eq!(custom, 0xFFFF_0000);
    }

    /// `custom_effects` are appended to the potion's own list (vanilla's own
    /// "get all effects" accessor), not substituted for it — a bare
    /// `minecraft:potion` id with no holder plus one custom effect must
    /// average that one effect alone.
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
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
        let turtle_master = PotionId::from_registry_id(potion_id("minecraft:turtle_master").unwrap()).unwrap();

        let mut mismatches = Vec::new();
        let cases: &[(&str, PotionId, &str)] = &[
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
                mismatches.push(format!(
                    "{base}#{}: expected {expected:?}, got {actual:?}",
                    id.registry_id()
                ));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    #[test]
    fn custom_effect_tooltip_data_covers_the_whole_registry() {
        for effect_id in 0..40 {
            let (name, _harmful) = mob_effect_tooltip(effect_id)
                .unwrap_or_else(|| panic!("missing tooltip data for mob effect id {effect_id}"));
            assert!(!name.is_empty(), "mob effect id {effect_id}");
        }
        assert_eq!(mob_effect_tooltip(26), Some(("Bad Luck", true)));
        assert_eq!(mob_effect_tooltip(29), Some(("Dolphin's Grace", false)));
        assert_eq!(mob_effect_tooltip(-1), None);
        assert_eq!(mob_effect_tooltip(40), None);

        assert_eq!(
            mob_effect_attribute_modifiers(2, 1),
            vec![AttributeModifierEntry {
                attribute_name: "Attack Speed",
                amount: 0.2,
                percent: true,
            }]
        );
    }

    #[test]
    fn explicit_and_empty_potion_name_suffixes_resolve_before_registry_fallback() {
        assert_eq!(
            potion_item_display_name_for_key("potion", "night_vision"),
            Some("Potion of Night Vision")
        );
        assert_eq!(
            potion_item_display_name_for_key("potion", "empty"),
            Some("Uncraftable Potion")
        );
        assert_eq!(potion_item_display_name_for_key("potion", "server_name"), None);
    }

    /// `long_swiftness`/`strong_swiftness` must resolve to the exact same item name
    /// as plain `swiftness` — vanilla's effect-key resolution collapses every duration
    /// and potency variant onto one key, so the title never carries a Roman numeral or
    /// a "Long" qualifier; only the tooltip's effect line does.
    #[test]
    fn duration_and_potency_variants_share_one_item_name() {
        let id = |name| PotionId::from_registry_id(potion_id(name).unwrap()).unwrap();
        let base = potion_item_display_name("potion", id("minecraft:swiftness"));
        let long = potion_item_display_name("potion", id("minecraft:long_swiftness"));
        let strong = potion_item_display_name("potion", id("minecraft:strong_swiftness"));
        assert_eq!(base, Some("Potion of Swiftness"));
        assert_eq!(base, long);
        assert_eq!(base, strong);
    }

    /// The four non-`"of"` names and `tipped_arrow`'s irregular `water` wording —
    /// every entry that a generic `"<Prefix> of <Effect>"` formula would get wrong.
    #[test]
    fn irregular_names_do_not_follow_the_of_pattern() {
        let water = PotionId::from_registry_id(potion_id("minecraft:water").unwrap()).unwrap();
        let mundane = PotionId::from_registry_id(potion_id("minecraft:mundane").unwrap()).unwrap();
        assert_eq!(potion_item_display_name("potion", water), Some("Water Bottle"));
        assert_eq!(potion_item_display_name("splash_potion", water), Some("Splash Water Bottle"));
        assert_eq!(potion_item_display_name("tipped_arrow", water), Some("Arrow of Splashing"));
        assert_eq!(potion_item_display_name("potion", mundane), Some("Mundane Potion"));
        assert_eq!(potion_item_display_name("tipped_arrow", mundane), Some("Tipped Arrow"));
    }

    /// The water-bottle control for the *effect list*, not just the colour: a water
    /// bottle carries no active mob-effect instance at all, so vanilla's own
    /// "add potion tooltip" step takes its own no-effects branch and prints
    /// `"No Effects"` — this must be an empty entry list, not a
    /// default/placeholder entry, proving the empty case is deliberate.
    #[test]
    fn water_bottle_has_no_effect_entries() {
        let water = PotionId::from_registry_id(potion_id("minecraft:water").unwrap()).unwrap();
        assert_eq!(potion_effect_entries(water), Vec::new());
        assert_eq!(potion_attribute_modifiers(water), Vec::new());
    }

    /// Amplifier 0 vs non-zero, the arm vanilla's own "potion description" step's
    /// `amplifier > 0` gate gets wrong most often — a fixture using only amplifier 1
    /// cannot see a build
    /// that always renders a numeral. `swiftness` (amplifier 0) and `strong_swiftness`
    /// (amplifier 1) are the same effect, so amplifier is the only thing that differs.
    #[test]
    fn amplifier_zero_is_distinguished_from_nonzero() {
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
        let strong = PotionId::from_registry_id(potion_id("minecraft:strong_swiftness").unwrap()).unwrap();
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
    /// against the registry's own declared arguments rather than guessed.
    #[test]
    fn high_amplifiers_are_carried_through_unmodified() {
        let strong_slowness = PotionId::from_registry_id(potion_id("minecraft:strong_slowness").unwrap()).unwrap();
        let strong_turtle_master = PotionId::from_registry_id(potion_id("minecraft:strong_turtle_master").unwrap()).unwrap();
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
        let healing = PotionId::from_registry_id(potion_id("minecraft:healing").unwrap()).unwrap();
        let entries = potion_effect_entries(healing);
        assert_eq!(entries, vec![PotionEffectEntry { effect_name: "Instant Health", amplifier: 0, duration_ticks: 1, harmful: false }]);
        assert!(entries[0].duration_ticks <= 20, "must fall inside endsWithin(20)");
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
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
        let swiftness = PotionId::from_registry_id(potion_id("minecraft:swiftness").unwrap()).unwrap();
        let strong_swiftness = PotionId::from_registry_id(potion_id("minecraft:strong_swiftness").unwrap()).unwrap();
        let strength = PotionId::from_registry_id(potion_id("minecraft:strength").unwrap()).unwrap();
        let night_vision = PotionId::from_registry_id(potion_id("minecraft:night_vision").unwrap()).unwrap();

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
