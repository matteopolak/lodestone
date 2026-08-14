//! The vanilla `minecraft:enchantment` registry: weight, level range, cost
//! curve, anvil fee, exclusivity and item eligibility for all 43 enchantments
//! 26.2 ships, plus the per-item `minecraft:enchantable` census the table's
//! and anvil's cost formulas both seed from.
//!
//! # What it is
//!
//! A static transcription of `.cache/mc/26.2/src/data/minecraft/enchantment/*.json`
//! (per-enchantment `weight`/`max_level`/`min_cost`/`max_cost`/`anvil_cost`/
//! `supported_items`), `tags/enchantment/{treasure,curse,exclusive_set/*}.json`,
//! `tags/item/enchantable/*.json` and the 77-item `minecraft:enchantable`
//! component census (`.cache/mc/26.2/generated/reports/minecraft/components/item/*.json`).
//! [`crate::anvil`], [`crate::enchanting`] and (for the grindstone's XP refund)
//! [`crate::anvil::grindstone_result`] all read this one table rather than each
//! carrying their own transcription, so a weight or cost figure only has one
//! place to be wrong.
//!
//! # How it works
//!
//! [`ENCHANTMENTS`] is the 43-entry census, each an [`EnchantmentDef`] carrying
//! the fields above. [`SupportedItems`] models the 14 distinct `supported_items`
//! tag values those 43 entries actually use (not the full `#minecraft:enchantable/*`
//! tag tree) — item-category membership is decided by suffix/exact-name match on
//! the item's registry path rather than by resolving nested block/item tag files,
//! because 26.2's tool/armour naming is exactly regular
//! (`<material>_<sword|axe|pickaxe|…>`, `<material>_<helmet|chestplate|…>`) and the
//! relevant tag files (`tags/item/swords.json` etc.) confirm the membership is
//! precisely that pattern with no exceptions.
//!
//! # Enchantment identity: an internal id, not vanilla's
//!
//! [`lodestone_model::ItemEnchantment::id`] is a **network registry id** —
//! session-scoped, assigned by whichever `registry_data` sync the connection
//! saw. This server's own `ServerProtocol::encode_registry_data`
//! (`crates/protocol/v770/src/server_protocol.rs`) sends only
//! `minecraft:dimension_type` and `minecraft:world_clock` during Configuration;
//! it does **not** send `minecraft:enchantment` at all. So there is no synced
//! registry for a real client to resolve an id against — meaning the
//! enchantment glint (a bare "is the list non-empty" check) will render, but
//! the enchantment's *name* will not, whatever id we pick. [`id_of`]/[`name_of`]
//! assign a stable internal index (alphabetical by key) so this crate's own
//! anvil/enchanting-table/grindstone logic can read and write
//! `ItemEnchantment.id` self-consistently; fixing client-visible names needs
//! `encode_registry_data` to grow a `minecraft:enchantment` entry, which is a
//! `crates/protocol/v770` change outside this crate's ownership — see this
//! module's own doc and the workstation docs for the open item.
//!
//! # How to change it
//!
//! A new enchantment (or a balance change to an existing one) means a new row
//! in [`ENCHANTMENTS`] plus, if it introduces a new `supported_items` tag
//! value, a new [`SupportedItems`] variant and its `matches` arm. Re-derive
//! every field from the jar's own JSON — do not guess a weight or cost from
//! the vanilla wiki, which drifts version to version (26.2 added `lunge`,
//! `density`, `breach` and `wind_burst`, none of which exist in older ports).

use lodestone_model::ItemStack;

/// One enchantment's registry definition.
#[derive(Debug, Clone, Copy)]
pub struct EnchantmentDef {
    /// Full key, e.g. `"minecraft:sharpness"`.
    pub key: &'static str,
    /// `Enchantment.EnchantmentDefinition.weight` — the table's weighted-random pick weight.
    pub weight: u32,
    /// `Enchantment.getMaxLevel()`. `getMinLevel()` is always `1` in 26.2.
    pub max_level: u32,
    /// `Enchantment.Cost.calculate` for `min_cost`: `base + per_level * (level - 1)`.
    pub min_cost_base: i32,
    pub min_cost_per: i32,
    /// Same shape for `max_cost`.
    pub max_cost_base: i32,
    pub max_cost_per: i32,
    /// `Enchantment.getAnvilCost()` — the anvil's per-level XP fee multiplier.
    pub anvil_cost: u32,
    /// `#minecraft:curse` membership (`binding_curse`, `vanishing_curse`).
    pub curse: bool,
    /// `#minecraft:treasure` membership — excluded from the enchanting table's
    /// weighted pool (`#minecraft:in_enchanting_table` is `#minecraft:non_treasure`)
    /// but still applicable via an anvil + enchanted book.
    pub treasure: bool,
    /// `Enchantment.EnchantmentDefinition.supportedItems` (`canEnchant`/`isSupportedItem`).
    pub supported: SupportedItems,
}

/// The 14 distinct `supported_items` tag values 26.2's 43 enchantments use.
///
/// Not the whole `#minecraft:enchantable/*` tag tree — only the leaves these
/// enchantments actually reference. See the module doc for why membership is
/// decided by name pattern rather than by resolving the tag files at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedItems {
    /// `#minecraft:enchantable/head_armor`.
    HeadArmor,
    /// `#minecraft:enchantable/foot_armor`.
    FootArmor,
    /// `#minecraft:enchantable/leg_armor`.
    LegArmor,
    /// `#minecraft:enchantable/armor` (all four armour slots).
    Armor,
    /// `#minecraft:enchantable/equippable` (armour + elytra + skulls + carved pumpkin).
    Equippable,
    /// `#minecraft:enchantable/weapon` (swords + spears — melee weapons only, no mace).
    Weapon,
    /// `#minecraft:enchantable/melee_weapon` (swords + spears).
    MeleeWeapon,
    /// `#minecraft:enchantable/sharp_weapon` (melee weapons + axes).
    SharpWeapon,
    /// `#minecraft:enchantable/sweeping` (swords only).
    Sweeping,
    /// `#minecraft:enchantable/fire_aspect` (melee weapons + mace).
    FireAspect,
    /// `#minecraft:enchantable/mining` (axes/pickaxes/shovels/hoes/shears).
    Mining,
    /// `#minecraft:enchantable/mining_loot` (axes/pickaxes/shovels/hoes).
    MiningLoot,
    /// `#minecraft:enchantable/durability` (every damageable item).
    Durability,
    /// `#minecraft:enchantable/vanishing` (durability + compass + carved pumpkin + skulls).
    Vanishing,
    /// A single named item: `minecraft:bow`, `crossbow`, `trident`, `fishing_rod`, `mace`.
    Item(&'static str),
    /// `#minecraft:enchantable/lunge` (spears).
    Spear,
}

fn is_sword(item: &str) -> bool {
    item.ends_with("_sword")
}
fn is_axe(item: &str) -> bool {
    item.ends_with("_axe")
}
fn is_pickaxe(item: &str) -> bool {
    item.ends_with("_pickaxe")
}
fn is_shovel(item: &str) -> bool {
    item.ends_with("_shovel")
}
fn is_hoe(item: &str) -> bool {
    item.ends_with("_hoe")
}
fn is_spear(item: &str) -> bool {
    item.ends_with("_spear")
}
fn is_head_armor(item: &str) -> bool {
    item.ends_with("_helmet") || item == "minecraft:turtle_helmet"
}
fn is_chest_armor(item: &str) -> bool {
    item.ends_with("_chestplate")
}
fn is_leg_armor(item: &str) -> bool {
    item.ends_with("_leggings")
}
fn is_foot_armor(item: &str) -> bool {
    item.ends_with("_boots")
}
fn is_armor(item: &str) -> bool {
    is_head_armor(item) || is_chest_armor(item) || is_leg_armor(item) || is_foot_armor(item)
}
fn is_melee_weapon(item: &str) -> bool {
    is_sword(item) || is_spear(item)
}
fn is_mining(item: &str) -> bool {
    is_axe(item) || is_pickaxe(item) || is_shovel(item) || is_hoe(item) || item == "minecraft:shears"
}
fn is_mining_loot(item: &str) -> bool {
    is_axe(item) || is_pickaxe(item) || is_shovel(item) || is_hoe(item)
}
fn is_skull(item: &str) -> bool {
    matches!(
        item,
        "minecraft:player_head"
            | "minecraft:creeper_head"
            | "minecraft:zombie_head"
            | "minecraft:skeleton_skull"
            | "minecraft:wither_skeleton_skull"
            | "minecraft:dragon_head"
            | "minecraft:piglin_head"
    )
}
/// `#minecraft:enchantable/durability` (`tags/item/enchantable/durability.json`):
/// every damageable item family, named directly rather than derived from
/// [`lodestone_data::item_prototypes`] so this module has no extra dependency.
fn is_durability(item: &str) -> bool {
    is_armor(item)
        || is_melee_weapon(item)
        || is_axe(item)
        || is_pickaxe(item)
        || is_shovel(item)
        || is_hoe(item)
        || matches!(
            item,
            "minecraft:elytra"
                | "minecraft:shield"
                | "minecraft:bow"
                | "minecraft:crossbow"
                | "minecraft:trident"
                | "minecraft:flint_and_steel"
                | "minecraft:shears"
                | "minecraft:brush"
                | "minecraft:fishing_rod"
                | "minecraft:carrot_on_a_stick"
                | "minecraft:warped_fungus_on_a_stick"
                | "minecraft:mace"
        )
}

impl SupportedItems {
    /// `Enchantment.canEnchant`/`isSupportedItem` — is `item` in this tag.
    #[must_use]
    pub fn matches(self, item: &str) -> bool {
        match self {
            Self::HeadArmor => is_head_armor(item),
            Self::FootArmor => is_foot_armor(item),
            Self::LegArmor => is_leg_armor(item),
            Self::Armor => is_armor(item),
            Self::Equippable => {
                is_armor(item)
                    || is_skull(item)
                    || matches!(item, "minecraft:elytra" | "minecraft:carved_pumpkin")
            }
            Self::Weapon => is_sword(item) || is_spear(item),
            Self::MeleeWeapon => is_melee_weapon(item),
            Self::SharpWeapon => is_melee_weapon(item) || is_axe(item),
            Self::Sweeping => is_sword(item),
            Self::FireAspect => is_melee_weapon(item) || item == "minecraft:mace",
            Self::Mining => is_mining(item),
            Self::MiningLoot => is_mining_loot(item),
            Self::Durability => is_durability(item),
            Self::Vanishing => {
                is_durability(item)
                    || is_skull(item)
                    || matches!(item, "minecraft:compass" | "minecraft:carved_pumpkin")
            }
            Self::Item(name) => item == name,
            Self::Spear => is_spear(item),
        }
    }
}

macro_rules! ench {
    ($key:literal, w=$weight:literal, max=$max:literal, min=($mb:literal,$mp:literal), maxc=($xb:literal,$xp:literal), anvil=$anvil:literal, $supported:expr) => {
        ench!($key, w=$weight, max=$max, min=($mb,$mp), maxc=($xb,$xp), anvil=$anvil, $supported, curse=false, treasure=false)
    };
    ($key:literal, w=$weight:literal, max=$max:literal, min=($mb:literal,$mp:literal), maxc=($xb:literal,$xp:literal), anvil=$anvil:literal, $supported:expr, treasure) => {
        ench!($key, w=$weight, max=$max, min=($mb,$mp), maxc=($xb,$xp), anvil=$anvil, $supported, curse=false, treasure=true)
    };
    ($key:literal, w=$weight:literal, max=$max:literal, min=($mb:literal,$mp:literal), maxc=($xb:literal,$xp:literal), anvil=$anvil:literal, $supported:expr, curse, treasure) => {
        ench!($key, w=$weight, max=$max, min=($mb,$mp), maxc=($xb,$xp), anvil=$anvil, $supported, curse=true, treasure=true)
    };
    ($key:literal, w=$weight:literal, max=$max:literal, min=($mb:literal,$mp:literal), maxc=($xb:literal,$xp:literal), anvil=$anvil:literal, $supported:expr, curse=$curse:literal, treasure=$treasure:literal) => {
        EnchantmentDef {
            key: concat!("minecraft:", $key),
            weight: $weight,
            max_level: $max,
            min_cost_base: $mb,
            min_cost_per: $mp,
            max_cost_base: $xb,
            max_cost_per: $xp,
            anvil_cost: $anvil,
            curse: $curse,
            treasure: $treasure,
            supported: $supported,
        }
    };
}

/// The 43-entry census, alphabetical by key (so [`id_of`]/[`name_of`] have a
/// stable, order-independent internal id space).
pub static ENCHANTMENTS: &[EnchantmentDef] = &[
    ench!("aqua_affinity", w=2, max=1, min=(1,0), maxc=(41,0), anvil=4, SupportedItems::HeadArmor),
    ench!("bane_of_arthropods", w=5, max=5, min=(5,8), maxc=(25,8), anvil=2, SupportedItems::MeleeWeapon),
    ench!("binding_curse", w=1, max=1, min=(25,0), maxc=(50,0), anvil=8, SupportedItems::Equippable, curse, treasure),
    ench!("blast_protection", w=2, max=4, min=(5,8), maxc=(13,8), anvil=4, SupportedItems::Armor),
    ench!("breach", w=2, max=4, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::Item("minecraft:mace")),
    ench!("channeling", w=1, max=1, min=(25,0), maxc=(50,0), anvil=8, SupportedItems::Item("minecraft:trident")),
    ench!("density", w=5, max=5, min=(5,8), maxc=(25,8), anvil=2, SupportedItems::Item("minecraft:mace")),
    ench!("depth_strider", w=2, max=3, min=(10,10), maxc=(25,10), anvil=4, SupportedItems::FootArmor),
    ench!("efficiency", w=10, max=5, min=(1,10), maxc=(51,10), anvil=1, SupportedItems::Mining),
    ench!("feather_falling", w=5, max=4, min=(5,6), maxc=(11,6), anvil=2, SupportedItems::FootArmor),
    ench!("fire_aspect", w=2, max=2, min=(10,20), maxc=(60,20), anvil=4, SupportedItems::FireAspect),
    ench!("fire_protection", w=5, max=4, min=(10,8), maxc=(18,8), anvil=2, SupportedItems::Armor),
    ench!("flame", w=2, max=1, min=(20,0), maxc=(50,0), anvil=4, SupportedItems::Item("minecraft:bow")),
    ench!("fortune", w=2, max=3, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::MiningLoot),
    ench!("frost_walker", w=2, max=2, min=(10,10), maxc=(25,10), anvil=4, SupportedItems::FootArmor, treasure),
    ench!("impaling", w=2, max=5, min=(1,8), maxc=(21,8), anvil=4, SupportedItems::Item("minecraft:trident")),
    ench!("infinity", w=1, max=1, min=(20,0), maxc=(50,0), anvil=8, SupportedItems::Item("minecraft:bow")),
    ench!("knockback", w=5, max=2, min=(5,20), maxc=(55,20), anvil=2, SupportedItems::MeleeWeapon),
    ench!("looting", w=2, max=3, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::MeleeWeapon),
    ench!("loyalty", w=5, max=3, min=(12,7), maxc=(50,0), anvil=2, SupportedItems::Item("minecraft:trident")),
    ench!("luck_of_the_sea", w=2, max=3, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::Item("minecraft:fishing_rod")),
    ench!("lunge", w=5, max=3, min=(5,8), maxc=(25,8), anvil=2, SupportedItems::Spear),
    ench!("lure", w=2, max=3, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::Item("minecraft:fishing_rod")),
    ench!("mending", w=2, max=1, min=(25,25), maxc=(75,25), anvil=4, SupportedItems::Durability, treasure),
    ench!("multishot", w=2, max=1, min=(20,0), maxc=(50,0), anvil=4, SupportedItems::Item("minecraft:crossbow")),
    ench!("piercing", w=10, max=4, min=(1,10), maxc=(50,0), anvil=1, SupportedItems::Item("minecraft:crossbow")),
    ench!("power", w=10, max=5, min=(1,10), maxc=(16,10), anvil=1, SupportedItems::Item("minecraft:bow")),
    ench!("projectile_protection", w=5, max=4, min=(3,6), maxc=(9,6), anvil=2, SupportedItems::Armor),
    ench!("protection", w=10, max=4, min=(1,11), maxc=(12,11), anvil=1, SupportedItems::Armor),
    ench!("punch", w=2, max=2, min=(12,20), maxc=(37,20), anvil=4, SupportedItems::Item("minecraft:bow")),
    ench!("quick_charge", w=5, max=3, min=(12,20), maxc=(50,0), anvil=2, SupportedItems::Item("minecraft:crossbow")),
    ench!("respiration", w=2, max=3, min=(10,10), maxc=(40,10), anvil=4, SupportedItems::HeadArmor),
    ench!("riptide", w=2, max=3, min=(17,7), maxc=(50,0), anvil=4, SupportedItems::Item("minecraft:trident")),
    ench!("sharpness", w=10, max=5, min=(1,11), maxc=(21,11), anvil=1, SupportedItems::SharpWeapon),
    ench!("silk_touch", w=1, max=1, min=(15,0), maxc=(65,0), anvil=8, SupportedItems::MiningLoot),
    ench!("smite", w=5, max=5, min=(5,8), maxc=(25,8), anvil=2, SupportedItems::MeleeWeapon),
    ench!("soul_speed", w=1, max=3, min=(10,10), maxc=(25,10), anvil=8, SupportedItems::FootArmor, treasure),
    ench!("sweeping_edge", w=2, max=3, min=(5,9), maxc=(20,9), anvil=4, SupportedItems::Sweeping),
    ench!("swift_sneak", w=1, max=3, min=(25,25), maxc=(75,25), anvil=8, SupportedItems::LegArmor, treasure),
    ench!("thorns", w=1, max=3, min=(10,20), maxc=(60,20), anvil=8, SupportedItems::Armor),
    ench!("unbreaking", w=5, max=3, min=(5,8), maxc=(55,8), anvil=2, SupportedItems::Durability),
    ench!("vanishing_curse", w=1, max=1, min=(25,0), maxc=(50,0), anvil=8, SupportedItems::Vanishing, curse, treasure),
    ench!("wind_burst", w=2, max=3, min=(15,9), maxc=(65,9), anvil=4, SupportedItems::Item("minecraft:mace"), treasure),
];

/// The seven `#minecraft:enchantment/exclusive_set/*` tags — enchantments that
/// cannot coexist on the same item (`Enchantment.areCompatible`).
static EXCLUSIVE_SETS: &[&[&str]] = &[
    &[
        "minecraft:protection",
        "minecraft:blast_protection",
        "minecraft:fire_protection",
        "minecraft:projectile_protection",
    ],
    &["minecraft:frost_walker", "minecraft:depth_strider"],
    &["minecraft:infinity", "minecraft:mending"],
    &["minecraft:multishot", "minecraft:piercing"],
    &[
        "minecraft:sharpness",
        "minecraft:smite",
        "minecraft:bane_of_arthropods",
        "minecraft:impaling",
        "minecraft:density",
        "minecraft:breach",
    ],
    &["minecraft:fortune", "minecraft:silk_touch"],
    &["minecraft:loyalty", "minecraft:channeling"],
];

/// The enchantment definition for `key` (e.g. `"minecraft:sharpness"`), or
/// `None` for anything outside the 43-entry census.
#[must_use]
pub fn by_key(key: &str) -> Option<&'static EnchantmentDef> {
    ENCHANTMENTS.iter().find(|e| e.key == key)
}

/// `Enchantment.getMinCost(level)`.
#[must_use]
pub fn min_cost(e: &EnchantmentDef, level: u32) -> i32 {
    e.min_cost_base + e.min_cost_per * (level as i32 - 1)
}

/// `Enchantment.getMaxCost(level)`.
#[must_use]
pub fn max_cost(e: &EnchantmentDef, level: u32) -> i32 {
    e.max_cost_base + e.max_cost_per * (level as i32 - 1)
}

/// `Enchantment.areCompatible(a, b)`: same enchantment is never "compatible"
/// with itself, and two different enchantments are incompatible exactly when
/// either names the other in its exclusive set.
#[must_use]
pub fn compatible(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    !EXCLUSIVE_SETS
        .iter()
        .any(|set| set.contains(&a) && set.contains(&b))
}

/// `#minecraft:non_treasure` / `#minecraft:in_enchanting_table` — every
/// enchantment the table's own weighted pool can roll.
pub fn non_treasure() -> impl Iterator<Item = &'static EnchantmentDef> {
    ENCHANTMENTS.iter().filter(|e| !e.treasure)
}

/// A stable, **this-server-only** enchantment id — alphabetical index into
/// [`ENCHANTMENTS`]. See the module doc for why this cannot be vanilla's real
/// registry id.
#[must_use]
pub fn id_of(key: &str) -> Option<i32> {
    ENCHANTMENTS
        .iter()
        .position(|e| e.key == key)
        .map(|i| i as i32)
}

/// The inverse of [`id_of`].
#[must_use]
pub fn name_of(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|i| ENCHANTMENTS.get(i))
        .map(|e| e.key)
}

/// `minecraft:enchantable`'s per-item value — the 77 items 26.2 lets an
/// enchanting table or anvil-with-book target, from the committed component
/// census (`.cache/mc/26.2/generated/reports/minecraft/components/item/*.json`).
/// `None` for every other item, including `minecraft:book` which is `1` (a
/// plain book *is* enchantable — see [`EnchantmentTableBlock`]'s own
/// `isEnchantable` gate for why a book with zero prior enchantments is what
/// makes slot 0 accept it at all).
#[must_use]
pub fn enchantable_value(item: &str) -> Option<u32> {
    // Strip the namespace once so every suffix match below compares a bare
    // material prefix (`"diamond"`) rather than `"minecraft:diamond"` — a
    // stripped-suffix remainder still carries the namespace.
    let item = item.strip_prefix("minecraft:").unwrap_or(item);
    let material_value = |suffix: &str, wood: u32, stone: u32, iron: u32, gold: u32, diamond: u32, netherite: u32, copper: u32| {
        if let Some(material) = item.strip_suffix(suffix) {
            return match material {
                "wooden" => Some(wood),
                "stone" => Some(stone),
                "iron" => Some(iron),
                "golden" => Some(gold),
                "diamond" => Some(diamond),
                "netherite" => Some(netherite),
                "copper" => Some(copper),
                _ => None,
            };
        }
        None
    };
    if let Some(v) = material_value("_sword", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    if let Some(v) = material_value("_axe", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    if let Some(v) = material_value("_pickaxe", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    if let Some(v) = material_value("_shovel", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    if let Some(v) = material_value("_hoe", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    if let Some(v) = material_value("_spear", 15, 5, 14, 22, 10, 15, 13) {
        return Some(v);
    }
    // Armour: no stone tier, chainmail instead of wood, and copper/leather values
    // differ per slot (boots/leggings/chestplate share one figure, helmet another).
    let armor_value = |suffix: &str, leather: u32, chain: u32, iron: u32, gold: u32, diamond: u32, netherite: u32, copper: u32| {
        item.strip_suffix(suffix).and_then(|material| match material {
            "leather" => Some(leather),
            "chainmail" => Some(chain),
            "iron" => Some(iron),
            "golden" => Some(gold),
            "diamond" => Some(diamond),
            "netherite" => Some(netherite),
            "copper" => Some(copper),
            _ => None,
        })
    };
    if let Some(v) = armor_value("_helmet", 15, 12, 9, 25, 10, 15, 8) {
        return Some(v);
    }
    if item == "turtle_helmet" {
        return Some(9);
    }
    if let Some(v) = armor_value("_chestplate", 15, 12, 9, 25, 10, 15, 8) {
        return Some(v);
    }
    if let Some(v) = armor_value("_leggings", 15, 12, 9, 25, 10, 15, 8) {
        return Some(v);
    }
    if let Some(v) = armor_value("_boots", 15, 12, 9, 25, 10, 15, 8) {
        return Some(v);
    }
    match item {
        "book" | "bow" | "crossbow" | "fishing_rod" | "trident" => Some(1),
        "mace" => Some(15),
        _ => None,
    }
}

/// `ItemStack.isEnchantable()`: the item has a `minecraft:enchantable`
/// prototype value **and** carries no enchantment yet — `input.components.enchantments`
/// must be empty (an already-enchanted item is never a valid table/anvil-book
/// target for a *fresh* enchant; the anvil's separate combine path handles
/// stacking existing enchantments).
#[must_use]
pub fn is_enchantable(item: &ItemStack) -> bool {
    enchantable_value(&item.item.to_string()).is_some() && item.components.enchantments.is_empty()
}

/// `EnchantmentHelper.canStoreEnchantments` — an item that can carry the
/// `minecraft:enchantments` (or `minecraft:stored_enchantments` for a book)
/// component at all. In 26.2 this is "is enchantable, or is already an
/// enchanted book" — the anvil's `input` slot accepts either.
#[must_use]
pub fn can_store_enchantments(item: &ItemStack) -> bool {
    let name = item.item.to_string();
    enchantable_value(&name).is_some() || name == "minecraft:enchanted_book"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_has_exactly_43_entries_alphabetical() {
        assert_eq!(ENCHANTMENTS.len(), 43);
        let mut sorted: Vec<&str> = ENCHANTMENTS.iter().map(|e| e.key).collect();
        let mut expected = sorted.clone();
        expected.sort_unstable();
        assert_eq!(sorted, expected, "table must stay alphabetical: id_of/name_of assume it");
        sorted.dedup();
        assert_eq!(sorted.len(), 43, "no duplicate keys");
    }

    #[test]
    fn seven_treasure_and_two_curse_enchantments() {
        assert_eq!(ENCHANTMENTS.iter().filter(|e| e.treasure).count(), 7);
        assert_eq!(ENCHANTMENTS.iter().filter(|e| e.curse).count(), 2);
    }

    /// `AnvilMenu`'s exclusive-set gate: sharpness and smite must conflict,
    /// but sharpness and unbreaking (unrelated sets) must not — a fixture
    /// where the "always incompatible" and "always compatible" hypotheses
    /// give the same answer would not test the exclusive-set lookup at all.
    #[test]
    fn exclusive_set_blocks_only_named_pairs() {
        assert!(!compatible("minecraft:sharpness", "minecraft:smite"));
        assert!(!compatible("minecraft:protection", "minecraft:fire_protection"));
        assert!(compatible("minecraft:sharpness", "minecraft:unbreaking"));
        assert!(!compatible("minecraft:sharpness", "minecraft:sharpness"));
    }

    #[test]
    fn min_cost_matches_the_linear_formula() {
        let sharpness = by_key("minecraft:sharpness").unwrap();
        // base=1, per=11: level 1 -> 1, level 5 -> 1 + 11*4 = 45.
        assert_eq!(min_cost(sharpness, 1), 1);
        assert_eq!(min_cost(sharpness, 5), 45);
        assert_eq!(max_cost(sharpness, 5), 21 + 11 * 4);
    }

    #[test]
    fn enchantable_value_matches_the_jar_census_for_discriminating_items() {
        // Netherite tools (15) and netherite armour boots (15) coincide, so the
        // discriminating pair is chainmail boots (12) vs iron boots (9): distinct
        // families that could plausibly be swapped by a transcription error.
        assert_eq!(enchantable_value("minecraft:chainmail_boots"), Some(12));
        assert_eq!(enchantable_value("minecraft:iron_boots"), Some(9));
        assert_eq!(enchantable_value("minecraft:golden_sword"), Some(22));
        assert_eq!(enchantable_value("minecraft:golden_helmet"), Some(25));
        assert_eq!(enchantable_value("minecraft:diamond_pickaxe"), Some(10));
        assert_eq!(enchantable_value("minecraft:turtle_helmet"), Some(9));
        assert_eq!(enchantable_value("minecraft:stone"), None);
    }

    #[test]
    fn supported_items_separates_neighbouring_categories() {
        assert!(SupportedItems::MeleeWeapon.matches("minecraft:diamond_sword"));
        assert!(!SupportedItems::MeleeWeapon.matches("minecraft:diamond_axe"));
        assert!(SupportedItems::SharpWeapon.matches("minecraft:diamond_axe"));
        assert!(!SupportedItems::Sweeping.matches("minecraft:diamond_spear"));
        assert!(SupportedItems::Sweeping.matches("minecraft:diamond_sword"));
    }

    #[test]
    fn id_round_trips_through_the_alphabetical_table() {
        let id = id_of("minecraft:sharpness").unwrap();
        assert_eq!(name_of(id), Some("minecraft:sharpness"));
    }
}
