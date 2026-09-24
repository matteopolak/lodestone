//! What an equipped item contributes to a living entity's combat attributes.
//!
//! This is the **feed** the damage pipeline never had.
//! [`crate::damage`]'s formulas were live-verified against a real vanilla 26.2
//! server long before this module existed, but nothing ever populated a
//! [`Defenses`](crate::damage::Defenses): armour reduced a number that came from
//! a per-species base attribute and never from a helmet, and every attack in the
//! workspace dealt a flat bare-hand `1.0` because no weapon reached the maths.
//!
//! # It feeds the attribute system rather than inventing a second one
//!
//! [`crate::attribute`] already models vanilla's arithmetic exactly —
//! [`Modifier`], [`Operation`], the `AddValue` → `AddMultipliedBase` →
//! `AddMultipliedTotal` fold, and the per-attribute clamp. Vanilla's own
//! equipment stats are *nothing but* attribute modifiers — every armour
//! material and every weapon's tool tier publishes its numbers this same
//! way — so this module emits [`Modifier`]s into an [`AttributeMap`] and lets that fold
//! decide the value. There is no parallel "equipment stats" arithmetic to keep
//! in step, and a future source of modifiers (an attribute-flavoured status
//! effect, a `/attribute` command, an enchantment) lands in the same map.
//!
//! Two consequences worth knowing before extending this:
//!
//! * **The modifier ids are vanilla's**, not invented — `minecraft:armor.helmet`
//!   for every head piece, `minecraft:base_attack_damage` for every weapon. That
//!   matters because [`AttributeInstance::add_or_update`](crate::attribute::AttributeInstance::add_or_update)
//!   is keyed by id: two helmets cannot stack, and a sword replacing an axe
//!   overwrites rather than accumulates, which is the vanilla behaviour and falls
//!   out of using the real ids.
//! * **The base value is the wearer's, not the item's.** A player's own base
//!   `attack_damage` is `1.0`, while the
//!   attribute registry's own default is `2.0` — so a diamond sword's `+6.0`
//!   modifier reads as `7.0` on a player and `8.0` on anything that took the
//!   registry default. [`player_attributes`] exists to make that distinction
//!   impossible to get wrong by accident.
//!
//! # Where the numbers come from
//!
//! Every constant below is read out of the 26.2 jar's own record definitions,
//! never a community table:
//!
//! * The eight armour materials, each publishing a durability, a per-piece
//!   defence value constructed **boots first, then legs, chest, helmet, body**,
//!   an enchantment value, an equip sound, a toughness and a knockback
//!   resistance. Note the defence-value argument order is **boots first**,
//!   which is the opposite of how the pieces are usually listed, and
//!   transcribing it head-first silently swaps a helmet's `1` with a boot's `3`.
//! * Each tool tier's own attack-damage bonus, and the baseline-plus-bonus sum
//!   that a sword or tool actually publishes. The published modifier is the
//!   **sum**, so a diamond sword is `3.0 + 3.0`, not `3.0`.
//! * Each weapon's own attack-damage baseline (a sword is `3.0`, an axe is
//!   per-tier, a shovel `1.5`, a pickaxe `1.0`, a hoe *negative* on several
//!   tiers), plus the trident's flat `8.0` and the mace's flat `5.0`, which are
//!   not tier-derived at all.
//!
//! # How to change it
//!
//! Adding an item means adding a row to [`item_modifiers`]. Adding a *stat*
//! means emitting another [`Modifier`] from that same function — nothing
//! downstream enumerates attributes, so a new one flows through
//! [`apply_equipment`] and out of [`AttributeMap::value`] with no other edit.
//!
//! The gotcha is the slot check. A modifier only applies in the slot vanilla
//! publishes it for (the main hand for a weapon, the piece's own slot for
//! armour), so [`ItemModifier::slot`] is load-bearing: without it a
//! sword in the off-hand or a helmet sitting in the hotbar would add its damage
//! or armour anyway. That is exactly the bug shape "held" versus "worn" invites,
//! and it is why [`apply_equipment`] takes `(slot, item)` pairs rather than a
//! bare item list.
//!
//! Enchantment protection is **not** here. [`Defenses::enchant_protection`] and
//! [`Defenses::enchant_effectiveness`] are the pipeline's own per-hit fields and
//! there is no enchantment model in this workspace to derive an EPF from; they
//! stay at their neutral defaults, which is an accurate statement of what
//! currently reduces damage rather than a stub.

use crate::attribute::{AttributeMap, Modifier, Operation};
use crate::damage::Defenses;
use lodestone_data::item::Item;
use lodestone_model::Identifier;
use std::str::FromStr;

/// The equipment slots that carry combat-relevant attribute modifiers.
///
/// A subset of vanilla's own equipment-slot set: the body-armour and saddle
/// slots are mount equipment with no player inventory slot, and nothing in
/// this workspace equips them, so admitting them here would be an unreachable
/// arm rather than coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EquipmentSlot {
    /// The selected hotbar item — the only slot a weapon's damage counts in.
    MainHand,
    /// The off-hand item. Carries no vanilla attack-damage modifier at all.
    OffHand,
    /// Helmet.
    Head,
    /// Chestplate.
    Chest,
    /// Leggings.
    Legs,
    /// Boots.
    Feet,
}

impl EquipmentSlot {
    /// Whether this slot is one of the four armour slots — the ones whose
    /// contents feed [`Defenses`].
    #[must_use]
    pub const fn is_armor(self) -> bool {
        matches!(self, Self::Head | Self::Chest | Self::Legs | Self::Feet)
    }
}

/// One attribute modifier an item publishes, and the slot it publishes it for.
///
/// Mirrors one entry of vanilla's own item-attribute-modifiers component: the
/// modifier plus the slot group it applies in. Every vanilla entry this module
/// models targets exactly one slot, so `slot` is a single value rather than a
/// group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemModifier {
    /// The slot the item must occupy for this modifier to count.
    pub slot: EquipmentSlot,
    /// The bare attribute path, e.g. `armor` or `attack_damage`.
    pub attribute: &'static str,
    /// Vanilla's stable modifier id, e.g. `armor.helmet`.
    pub id: &'static str,
    /// The modifier amount.
    pub amount: f64,
    /// How it combines. Every vanilla equipment modifier is `AddValue`.
    pub operation: Operation,
}

/// Per-material armour numbers, exactly the fields vanilla's own armour-material
/// record needs.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ArmorMaterial {
    /// The helmet defence value.
    helmet: f64,
    /// The chestplate defence value.
    chestplate: f64,
    /// The leggings defence value.
    leggings: f64,
    /// The boots defence value.
    boots: f64,
    /// Per-piece `armor_toughness`.
    toughness: f64,
    /// Per-piece `knockback_resistance`, `0.0` for every material but netherite.
    knockback_resistance: f64,
}

/// Leather: defence values `(boots 1, legs 2, chest 3, helm 1)`, toughness `0.0`.
const LEATHER: ArmorMaterial = ArmorMaterial {
    helmet: 1.0,
    chestplate: 3.0,
    leggings: 2.0,
    boots: 1.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};
/// Copper: defence values `(boots 1, legs 3, chest 4, helm 2)`.
const COPPER_ARMOR: ArmorMaterial = ArmorMaterial {
    helmet: 2.0,
    chestplate: 4.0,
    leggings: 3.0,
    boots: 1.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};
/// Chainmail: defence values `(boots 1, legs 4, chest 5, helm 2)`.
const CHAINMAIL: ArmorMaterial = ArmorMaterial {
    helmet: 2.0,
    chestplate: 5.0,
    leggings: 4.0,
    boots: 1.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};
/// Iron: defence values `(boots 2, legs 5, chest 6, helm 2)`.
const IRON_ARMOR: ArmorMaterial = ArmorMaterial {
    helmet: 2.0,
    chestplate: 6.0,
    leggings: 5.0,
    boots: 2.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};
/// Gold: defence values `(boots 1, legs 3, chest 5, helm 2)`.
const GOLD_ARMOR: ArmorMaterial = ArmorMaterial {
    helmet: 2.0,
    chestplate: 5.0,
    leggings: 3.0,
    boots: 1.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};
/// Diamond: defence values `(boots 3, legs 6, chest 8, helm 3)`, toughness `2.0`.
const DIAMOND_ARMOR: ArmorMaterial = ArmorMaterial {
    helmet: 3.0,
    chestplate: 8.0,
    leggings: 6.0,
    boots: 3.0,
    toughness: 2.0,
    knockback_resistance: 0.0,
};
/// Netherite: defence values `(boots 3, legs 6, chest 8, helm 3)`, toughness
/// `3.0`, knockback resistance `0.1` — the only material with a non-zero one.
const NETHERITE_ARMOR: ArmorMaterial = ArmorMaterial {
    helmet: 3.0,
    chestplate: 8.0,
    leggings: 6.0,
    boots: 3.0,
    toughness: 3.0,
    knockback_resistance: 0.1,
};
/// Turtle scute: defence values `(boots 2, legs 5, chest 6, helm 2)`. Only the
/// helmet exists as an item.
const TURTLE_SCUTE: ArmorMaterial = ArmorMaterial {
    helmet: 2.0,
    chestplate: 6.0,
    leggings: 5.0,
    boots: 2.0,
    toughness: 0.0,
    knockback_resistance: 0.0,
};

/// The armour piece an item id names, if it is humanoid armour: its material and
/// which slot it goes in.
fn armor_piece(path: &str) -> Option<(ArmorMaterial, EquipmentSlot)> {
    // `turtle_helmet` first: it does not follow the `<material>_<piece>` shape
    // (the material is `TURTLE_SCUTE`, the item is `turtle_helmet`), so a
    // prefix split would read its material as "turtle" and miss.
    if path == "turtle_helmet" {
        return Some((TURTLE_SCUTE, EquipmentSlot::Head));
    }
    let (material, piece) = path.rsplit_once('_')?;
    let slot = match piece {
        "helmet" => EquipmentSlot::Head,
        "chestplate" => EquipmentSlot::Chest,
        "leggings" => EquipmentSlot::Legs,
        "boots" => EquipmentSlot::Feet,
        _ => return None,
    };
    let material = match material {
        "leather" => LEATHER,
        "copper" => COPPER_ARMOR,
        "chainmail" => CHAINMAIL,
        "iron" => IRON_ARMOR,
        "golden" => GOLD_ARMOR,
        "diamond" => DIAMOND_ARMOR,
        "netherite" => NETHERITE_ARMOR,
        _ => return None,
    };
    Some((material, slot))
}

/// The defence points a material grants in `slot`.
const fn defense_for(material: ArmorMaterial, slot: EquipmentSlot) -> f64 {
    match slot {
        EquipmentSlot::Head => material.helmet,
        EquipmentSlot::Chest => material.chestplate,
        EquipmentSlot::Legs => material.leggings,
        EquipmentSlot::Feet => material.boots,
        EquipmentSlot::MainHand | EquipmentSlot::OffHand => 0.0,
    }
}

/// Vanilla's `armor.<type name>` modifier id for a slot.
const fn armor_modifier_id(slot: EquipmentSlot) -> &'static str {
    match slot {
        EquipmentSlot::Head => "armor.helmet",
        EquipmentSlot::Chest => "armor.chestplate",
        EquipmentSlot::Legs => "armor.leggings",
        EquipmentSlot::Feet => "armor.boots",
        EquipmentSlot::MainHand | EquipmentSlot::OffHand => "armor.body",
    }
}

/// Vanilla's own stable id, the one every weapon's damage modifier carries.
pub const BASE_ATTACK_DAMAGE_ID: &str = "base_attack_damage";

/// The published attack-damage modifier amount for a weapon item id, if it is
/// one — already `attackDamageBaseline + material.attackDamageBonus()`, which is
/// the value vanilla actually puts in the component.
///
/// `None` for anything that publishes no attack-damage modifier, which includes
/// a bow, a shield and every block: a bare hand and a bow deal the same melee
/// damage in vanilla, and reading `Some(0.0)` for one of them would be a
/// different (wrong) claim from `None`.
#[must_use]
pub fn weapon_attack_damage(path: &str) -> Option<f64> {
    // Checked before the `<material>_<kind>` split, not after: neither `trident`
    // nor `mace` contains an underscore, so a `rsplit_once('_')?` ahead of this
    // returns `None` for both and the tier-free branch is never reached — which
    // is precisely how the first version of this function reported that a trident
    // publishes no attack damage at all.
    if let Some(flat) = trident_or_mace(path) {
        return Some(flat);
    }
    let (material, kind) = path.rsplit_once('_')?;
    // Each tool tier's own attack-damage bonus, in vanilla's own order.
    let bonus = match material {
        "wooden" => 0.0,
        "stone" | "copper" => 1.0,
        "iron" => 2.0,
        "diamond" => 3.0,
        "netherite" => 4.0,
        "golden" => 0.0,
        _ => return None,
    };
    // Each item's own per-item attack-damage baseline. A sword is a flat 3.0
    // across every tier; the others are per-tier, and a hoe's is negative on
    // four of them, so this cannot be collapsed into one number per kind.
    let baseline = match (kind, material) {
        ("sword", _) => 3.0,
        ("shovel", _) => 1.5,
        ("pickaxe", _) => 1.0,
        ("axe", "wooden" | "golden") => 6.0,
        ("axe", "stone" | "copper") => 7.0,
        ("axe", "iron") => 6.0,
        ("axe", "diamond" | "netherite") => 5.0,
        ("hoe", "wooden" | "golden") => 0.0,
        ("hoe", "stone" | "copper") => -1.0,
        ("hoe", "iron") => -2.0,
        ("hoe", "diamond" | "netherite") => -3.0,
        _ => return None,
    };
    Some(baseline + bonus)
}

/// The two weapons whose attack damage is a flat literal rather than
/// tier-derived: the trident publishes `8.0` and the mace publishes `5.0`,
/// neither one derived from a tool tier.
fn trident_or_mace(path: &str) -> Option<f64> {
    match path {
        "trident" => Some(8.0),
        "mace" => Some(5.0),
        _ => None,
    }
}

/// Every attribute modifier an item id publishes, with the slot each applies in.
///
/// The single row-table this module is built around: add an item here and it
/// flows through [`apply_equipment`] with no other change.
#[must_use]
pub fn item_modifiers(item: Item) -> Vec<ItemModifier> {
    let path = item.path();
    if let Some((material, slot)) = armor_piece(path) {
        let id = armor_modifier_id(slot);
        // Vanilla emits ARMOR and ARMOR_TOUGHNESS unconditionally and
        // KNOCKBACK_RESISTANCE only when non-zero. Reproduced, because an
        // explicit `0.0` modifier is observable through `modifier_count`.
        let mut out = vec![
            ItemModifier {
                slot,
                attribute: "armor",
                id,
                amount: defense_for(material, slot),
                operation: Operation::AddValue,
            },
            ItemModifier {
                slot,
                attribute: "armor_toughness",
                id,
                amount: material.toughness,
                operation: Operation::AddValue,
            },
        ];
        if material.knockback_resistance > 0.0 {
            out.push(ItemModifier {
                slot,
                attribute: "knockback_resistance",
                id,
                amount: material.knockback_resistance,
                operation: Operation::AddValue,
            });
        }
        return out;
    }
    if let Some(amount) = weapon_attack_damage(path) {
        return vec![ItemModifier {
            slot: EquipmentSlot::MainHand,
            attribute: "attack_damage",
            id: BASE_ATTACK_DAMAGE_ID,
            amount,
            operation: Operation::AddValue,
        }];
    }
    Vec::new()
}

/// A source of an equipment item. Built-in [`Item`] values already establish
/// their registry identity; strings are the dynamic boundary and are resolved
/// once before the modifier table runs.
pub trait EquipmentItem {
    /// Resolves the input to one built-in item, if this registry knows it.
    fn built_in_item(self) -> Option<Item>;
}

impl EquipmentItem for Item {
    fn built_in_item(self) -> Option<Item> {
        Some(self)
    }
}

impl EquipmentItem for &str {
    fn built_in_item(self) -> Option<Item> {
        Item::from_name(self)
    }
}

/// Folds a set of `(slot, item)` pairs into `attrs` as real
/// [`Modifier`]s, skipping any modifier whose slot does not match the slot the
/// item is actually in.
///
/// Built-in [`Item`] values do not need lookup. A string is accepted only at
/// the dynamic boundary; namespaced and bare spellings resolve identically,
/// while an unrecognised item contributes nothing.
pub fn apply_equipment<I, T>(attrs: &mut AttributeMap, equipped: I)
where
    I: IntoIterator<Item = (EquipmentSlot, T)>,
    T: EquipmentItem,
{
    for (slot, item) in equipped {
        let Some(item) = item.built_in_item() else {
            continue;
        };
        for m in item_modifiers(item) {
            if m.slot != slot {
                continue;
            }
            let Ok(key) = Identifier::from_str(&format!("minecraft:{}", m.attribute)) else {
                continue;
            };
            let Ok(id) = Identifier::from_str(&format!("minecraft:{}", m.id)) else {
                continue;
            };
            attrs
                .get_or_default(&key)
                .add_or_update(Modifier::new(id, m.amount, m.operation));
        }
    }
}

/// A player's own base attributes, before equipment.
///
/// The one value that must not be taken from the attribute registry:
/// vanilla's own player base attributes set attack damage to **`1.0`**, while
/// [`crate::attribute::default_def`]'s registry default for the same attribute
/// is `2.0`. Every mob in the game gets its base from its own species table, so
/// the registry default is never a player's, and a bare-hand punch reading `2.0`
/// would be exactly twice as strong as vanilla's.
#[must_use]
pub fn player_attributes() -> AttributeMap {
    let mut attrs = AttributeMap::new();
    if let Ok(key) = Identifier::from_str("minecraft:attack_damage") {
        attrs.get_or_default(&key).set_base_value(PLAYER_BASE_ATTACK_DAMAGE);
    }
    attrs
}

/// A player's own base attack damage, before any equipment.
pub const PLAYER_BASE_ATTACK_DAMAGE: f64 = 1.0;

/// Reads a bare attribute path out of `attrs`, falling back to `0.0` when the
/// path does not parse.
fn value_of(attrs: &AttributeMap, path: &str) -> f64 {
    Identifier::from_str(&format!("minecraft:{path}"))
        .ok()
        .and_then(|id| attrs.value(&id))
        .unwrap_or(0.0)
}

/// The armour half of a [`Defenses`] as computed from `attrs`, leaving the
/// per-hit fields (Resistance amplifier, enchantment protection/effectiveness,
/// absorption) at their neutral defaults for a caller to fill in.
///
/// Split out rather than folded into one "make me a Defenses" call because
/// Resistance and absorption are *not* equipment-derived — they come from the
/// effect and absorption models — and a function that returned them as `None`
/// and `0.0` would look like it had considered them.
#[must_use]
pub fn defenses_from_attributes(attrs: &AttributeMap) -> Defenses {
    Defenses {
        armor: value_of(attrs, "armor") as f32,
        armor_toughness: value_of(attrs, "armor_toughness") as f32,
        ..Defenses::default()
    }
}

/// The `attack_damage` a wearer of `attrs` deals with a melee swing.
#[must_use]
pub fn attack_damage_from_attributes(attrs: &AttributeMap) -> f32 {
    value_of(attrs, "attack_damage") as f32
}

/// The `knockback_resistance` a wearer of `attrs` has.
#[must_use]
pub fn knockback_resistance_from_attributes(attrs: &AttributeMap) -> f64 {
    value_of(attrs, "knockback_resistance")
}

/// A player's combat stats given what is in their hand and on their body: the
/// [`Defenses`] an incoming hit reduces against, the melee damage their swing
/// deals, and their knockback resistance.
///
/// One call rather than three so a caller cannot feed armour to the reduction
/// and forget to feed the weapon to the attack — the exact split that let a
/// flat `1.0` survive next to a fully-verified armour formula.
#[must_use]
pub fn player_combat_stats<'a, I>(equipped: I) -> PlayerCombatStats
where
    I: IntoIterator<Item = (EquipmentSlot, &'a str)>,
{
    let mut attrs = player_attributes();
    apply_equipment(&mut attrs, equipped);
    PlayerCombatStats {
        defenses: defenses_from_attributes(&attrs),
        attack_damage: attack_damage_from_attributes(&attrs),
        knockback_resistance: knockback_resistance_from_attributes(&attrs),
        attributes: attrs,
    }
}

/// What [`player_combat_stats`] resolved, plus the [`AttributeMap`] it was
/// folded from so a caller can read any other attribute without a second fold.
///
/// Not `PartialEq`: [`AttributeMap`] is not, and comparing two of these by value
/// would compare modifier lists rather than resolved stats. Compare the three
/// scalar fields.
#[derive(Debug, Clone)]
pub struct PlayerCombatStats {
    /// Armour and toughness, per-hit fields left neutral.
    pub defenses: Defenses,
    /// Melee damage of one swing.
    pub attack_damage: f32,
    /// `knockback_resistance`, `0.0` without netherite.
    pub knockback_resistance: f64,
    /// The folded map, for anything else.
    pub attributes: AttributeMap,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::{DamageFlags, apply_reductions};

    fn full_diamond() -> Vec<(EquipmentSlot, &'static str)> {
        vec![
            (EquipmentSlot::Head, "diamond_helmet"),
            (EquipmentSlot::Chest, "diamond_chestplate"),
            (EquipmentSlot::Legs, "diamond_leggings"),
            (EquipmentSlot::Feet, "diamond_boots"),
        ]
    }

    /// The outside expectation: a full diamond set resolves to **armour 20.0,
    /// toughness 8.0** — the exact pair `crate::damage`'s own live-verified
    /// gate reached by hardcoding, and which a real vanilla server confirmed
    /// via `/attribute get` on a force-equipped pig.
    ///
    /// This is a magnitude check, not a "did it go up" one. The pieces are
    /// `3 + 8 + 6 + 3`, and the plausible wrong transcription — reading
    /// vanilla's defence-value arguments head-first instead of boots-first,
    /// which their own declared order invites — swaps helmet with boots and
    /// leggings with
    /// chestplate. That mis-read yields `1 + 6 + 8 + 3 = 18`... which is why
    /// the assertion below pins the *individual* pieces too: the swapped total
    /// for diamond happens to be close, and only the per-piece values separate
    /// the two hypotheses cleanly.
    #[test]
    fn full_diamond_set_resolves_to_the_live_verified_armour_pair() {
        let stats = player_combat_stats(full_diamond());
        assert!(
            (stats.defenses.armor - 20.0).abs() < 1e-6,
            "armor {}",
            stats.defenses.armor
        );
        assert!(
            (stats.defenses.armor_toughness - 8.0).abs() < 1e-6,
            "toughness {}",
            stats.defenses.armor_toughness
        );

        // Per-piece, so a boots/helmet transposition cannot hide inside a total.
        let helmet_only = player_combat_stats(vec![(EquipmentSlot::Head, "diamond_helmet")]);
        assert!((helmet_only.defenses.armor - 3.0).abs() < 1e-6);
        let chest_only = player_combat_stats(vec![(EquipmentSlot::Chest, "diamond_chestplate")]);
        assert!((chest_only.defenses.armor - 8.0).abs() < 1e-6);
        let legs_only = player_combat_stats(vec![(EquipmentSlot::Legs, "diamond_leggings")]);
        assert!((legs_only.defenses.armor - 6.0).abs() < 1e-6);
        let boots_only = player_combat_stats(vec![(EquipmentSlot::Feet, "diamond_boots")]);
        assert!((boots_only.defenses.armor - 3.0).abs() < 1e-6);
    }

    /// Iron is the case where the boots-first mis-read is *visible in the
    /// total*: the raw values `(2, 5, 6, 2, 5)` read head-first give helmet 2,
    /// chest 5, legs 6, boots 2 — a per-piece swap of legs and chest that the
    /// total (15 either way) cannot see. So this asserts the two pieces that
    /// differ, and evaluates the wrong hypothesis explicitly so the inputs
    /// cannot coincide.
    #[test]
    fn iron_pieces_land_on_the_boots_first_reading_not_the_head_first_one() {
        let chest = player_combat_stats(vec![(EquipmentSlot::Chest, "iron_chestplate")]);
        let legs = player_combat_stats(vec![(EquipmentSlot::Legs, "iron_leggings")]);
        // Correct: boots=2, legs=5, chest=6, helm=2, body=5.
        assert!((chest.defenses.armor - 6.0).abs() < 1e-6, "chest {}", chest.defenses.armor);
        assert!((legs.defenses.armor - 5.0).abs() < 1e-6, "legs {}", legs.defenses.armor);
        // The wrong hypothesis, computed rather than described: head-first
        // reading would give chest 5 and legs 6. The two differ at both pieces.
        assert!(
            (chest.defenses.armor - 5.0).abs() > 0.5 && (legs.defenses.armor - 6.0).abs() > 0.5,
            "the head-first reading must be excluded, not merely unlikely"
        );
    }

    /// A real sword does more damage than a fist, and lands on the exact
    /// vanilla number rather than merely being larger.
    ///
    /// Bare hand is `1.0` (a player's own base attack damage); a diamond sword
    /// adds a `3.0` baseline plus the diamond tier's `3.0` bonus, so `7.0`.
    /// The two competing wrong hypotheses are both evaluated here:
    /// forgetting the material bonus gives `4.0`, and taking the attribute
    /// registry's `2.0` base instead of the player's `1.0` gives `8.0`. All
    /// three answers differ, so the input is not one where they coincide.
    #[test]
    fn a_diamond_sword_lands_on_seven_not_four_and_not_eight() {
        let fist = player_combat_stats(Vec::new()).attack_damage;
        let sword = player_combat_stats(vec![(EquipmentSlot::MainHand, "diamond_sword")])
            .attack_damage;
        assert!((fist - 1.0).abs() < 1e-6, "fist {fist}");
        assert!((sword - 7.0).abs() < 1e-6, "sword {sword}");
        assert!(sword > fist, "a sword must beat a fist");
        // The baseline-only hypothesis (4.0) and the registry-base hypothesis
        // (8.0) are each a full point away from the measurement.
        assert!((sword - 4.0).abs() > 0.9, "material bonus must be included");
        assert!((sword - 8.0).abs() > 0.9, "the player base is 1.0, not the registry's 2.0");
    }

    /// The whole tier ladder, each against its own jar arithmetic. A netherite
    /// axe is the interesting one: its baseline drops to `5.0` while its bonus
    /// rises to `4.0`, so the ladder is not monotonic in either term alone.
    #[test]
    fn the_weapon_ladder_matches_baseline_plus_material_bonus() {
        let cases = [
            ("wooden_sword", 3.0),
            ("stone_sword", 4.0),
            ("iron_sword", 5.0),
            ("diamond_sword", 6.0),
            ("netherite_sword", 7.0),
            ("golden_sword", 3.0),
            ("wooden_axe", 6.0),
            ("stone_axe", 8.0),
            ("iron_axe", 8.0),
            ("diamond_axe", 8.0),
            ("netherite_axe", 9.0),
            ("diamond_pickaxe", 4.0),
            ("diamond_shovel", 4.5),
            ("diamond_hoe", 0.0),
            ("trident", 8.0),
            ("mace", 5.0),
        ];
        for (item, expected) in cases {
            let got = weapon_attack_damage(item)
                .unwrap_or_else(|| panic!("{item} should publish an attack-damage modifier"));
            assert!(
                (got - expected).abs() < 1e-6,
                "{item}: expected {expected}, got {got}"
            );
        }
        // A bow publishes no attack-damage modifier at all — `None`, which is a
        // different claim from `Some(0.0)`.
        assert_eq!(weapon_attack_damage("bow"), None);
        assert_eq!(weapon_attack_damage("shield"), None);
        assert_eq!(weapon_attack_damage("stone"), None);
    }

    /// The slot check is load-bearing, and this is the control for it: the same
    /// item in the wrong slot must contribute nothing.
    #[test]
    fn a_modifier_only_applies_in_the_slot_vanilla_publishes_it_for() {
        // A sword in the off-hand adds no damage.
        let offhand = player_combat_stats(vec![(EquipmentSlot::OffHand, "diamond_sword")]);
        assert!(
            (offhand.attack_damage - 1.0).abs() < 1e-6,
            "an off-hand sword must not add damage, got {}",
            offhand.attack_damage
        );
        // A helmet held in the main hand adds no armour.
        let held = player_combat_stats(vec![(EquipmentSlot::MainHand, "diamond_helmet")]);
        assert!(
            held.defenses.armor.abs() < 1e-6,
            "a held helmet must not add armour, got {}",
            held.defenses.armor
        );
        // Control: the very same items *do* contribute in their own slots, so
        // the two assertions above are not measuring a broken lookup.
        let worn = player_combat_stats(vec![
            (EquipmentSlot::MainHand, "diamond_sword"),
            (EquipmentSlot::Head, "diamond_helmet"),
        ]);
        assert!((worn.attack_damage - 7.0).abs() < 1e-6);
        assert!((worn.defenses.armor - 3.0).abs() < 1e-6);
    }

    /// Only netherite carries `knockback_resistance`, and a full set is `0.4`
    /// — four pieces of `0.1`, not one.
    #[test]
    fn only_netherite_grants_knockback_resistance() {
        let netherite = player_combat_stats(vec![
            (EquipmentSlot::Head, "netherite_helmet"),
            (EquipmentSlot::Chest, "netherite_chestplate"),
            (EquipmentSlot::Legs, "netherite_leggings"),
            (EquipmentSlot::Feet, "netherite_boots"),
        ]);
        assert!(
            (netherite.knockback_resistance - 0.4).abs() < 1e-6,
            "kbres {}",
            netherite.knockback_resistance
        );
        assert!(
            (netherite.defenses.armor_toughness - 12.0).abs() < 1e-6,
            "netherite toughness is 3.0 per piece, got {}",
            netherite.defenses.armor_toughness
        );
        let diamond = player_combat_stats(full_diamond());
        assert!(
            diamond.knockback_resistance.abs() < 1e-6,
            "diamond grants none, got {}",
            diamond.knockback_resistance
        );
    }

    /// Two helmets cannot stack: vanilla keys both modifiers `armor.helmet`, so
    /// the second replaces the first. Falls out of using the real ids, and is
    /// the reason they are not invented per-item.
    #[test]
    fn a_second_helmet_replaces_rather_than_stacks() {
        let mut attrs = player_attributes();
        apply_equipment(&mut attrs, vec![(EquipmentSlot::Head, "diamond_helmet")]);
        apply_equipment(&mut attrs, vec![(EquipmentSlot::Head, "iron_helmet")]);
        // Iron helmet is 2, diamond 3. Replacement gives 2; accumulation 5.
        let armor = defenses_from_attributes(&attrs).armor;
        assert!((armor - 2.0).abs() < 1e-6, "expected replacement to 2.0, got {armor}");
    }

    /// The join to the thing this module exists to feed: real equipment,
    /// through the real pipeline, landing on the number the live vanilla server
    /// produced.
    ///
    /// A raw `10.0` `minecraft:mob_attack` against a full diamond set measured
    /// **3.0** on a real 26.2 server. Before this module the same call took
    /// `Defenses::default()` and measured `10.0`, so the two arms differ by
    /// 7.0 of 10.0 — the detector demonstrably fires.
    #[test]
    fn equipment_derived_defenses_reproduce_the_live_verified_reduction() {
        let stats = player_combat_stats(full_diamond());
        let flags = DamageFlags::for_damage_type_name("mob_attack").expect("real damage type");
        let with_armour = apply_reductions(10.0, &stats.defenses, flags).to_health;
        let bare = apply_reductions(10.0, &Defenses::default(), flags).to_health;
        assert!(
            (with_armour - 3.0).abs() < 1e-4,
            "expected the live-verified 3.0, got {with_armour}"
        );
        assert!(
            (bare - 10.0).abs() < 1e-4,
            "control: with no equipment the same hit is unreduced, got {bare}"
        );
    }

    /// Validated items reach the modifier table directly; namespaced and bare
    /// strings resolve identically only at the explicit dynamic boundary.
    /// Unknown dynamic items remain a silent no-contribution rather than a
    /// panic.
    #[test]
    fn typed_items_bypass_lookup_while_dynamic_names_stay_at_the_boundary() {
        let helmet = Item::from_name("diamond_helmet").expect("built-in helmet");
        assert!(!item_modifiers(helmet).is_empty());
        assert!(
            item_modifiers(Item::from_name("cobblestone").expect("built-in block")).is_empty()
        );

        let bare = player_combat_stats(vec![(EquipmentSlot::Head, "diamond_helmet")]);
        let namespaced = player_combat_stats(vec![(EquipmentSlot::Head, "minecraft:diamond_helmet")]);
        assert_eq!(bare.defenses.armor, namespaced.defenses.armor);
        assert!((bare.defenses.armor - 3.0).abs() < 1e-6);

        let nothing = player_combat_stats(vec![(EquipmentSlot::Head, "cobblestone")]);
        assert!(nothing.defenses.armor.abs() < 1e-6);
    }
}
