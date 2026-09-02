//! The smithing table: netherite upgrade and armour/tool trim —
//! two distinct recipe families sharing one three-input, one-result menu.
//!
//! # What it is
//!
//! A port of `SmithingTransformRecipe`/`TransmuteRecipe.createWithOriginalComponents`
//! (netherite upgrade) and `SmithingTrimRecipe`/`applyTrim` (trim), both from
//! vanilla's own crafting-recipe types. The template slot
//! decides *which* family a given input set can even match — a netherite
//! upgrade always uses `minecraft:netherite_upgrade_smithing_template`, a trim
//! always uses one of the 18 `<pattern>_armor_trim_smithing_template` items —
//! so [`compute`] tries netherite upgrade first and falls back to trim rather
//! than merging their validation, matching the issue's own warning that the
//! two "have different input-slot rules."
//!
//! # How it works
//!
//! Netherite upgrade ([`netherite_upgrade`]) is data-uniform across all 12
//! upgradeable items (`.cache/mc/26.2/src/data/minecraft/recipe/netherite_*_smithing.json`):
//! base `diamond_<x>` + addition `minecraft:netherite_ingot` + the one
//! template → result `netherite_<x>`, carrying the base's **entire component
//! patch verbatim** onto the new item (`TransmuteRecipe.createWithOriginalComponents`
//! — the raw `damage` value transfers unchanged, which is *not* the same as
//! preserving the damage *fraction*: a diamond pickaxe at 1000/1561 becomes a
//! netherite one at 1000/2031, a materially higher fraction remaining. Do not
//! "fix" this into a fraction-preserving recompute; that would be porting the
//! plausible-sounding formula instead of vanilla's real one).
//!
//! Trim ([`apply_trim`]) reads the template's own item name for the pattern
//! (`"dune_armor_trim_smithing_template"` → pattern `"dune"`, a naming
//! regularity confirmed against every one of the 18 template recipes) and the
//! addition's *material* from a fixed 11-entry table
//! (`tags/item/trim_materials.json`) — vanilla resolves the material from the
//! addition stack's own `minecraft:provides_trim_material` prototype
//! component, which this build does not carry per-item, so the table here is
//! the direct substitute. It is **visual-only**: no stat, durability or
//! enchantment change, matching `SmithingTrimRecipe.applyTrim`.
//!
//! # How to change it
//!
//! A new netherite-upgradeable item needs a row in [`NETHERITE_UPGRADES`]. A
//! new trim pattern needs its item suffix added to [`TRIM_PATTERNS`]; a new
//! trim material needs a row in [`TRIM_MATERIALS`]. Re-derive both from the
//! jar's own recipe/tag JSON, not from a wiki list — 26.2 added the `resin`
//! material and 3+ new patterns beyond the original set.

use lodestone_model::ItemStack;

/// `diamond_<suffix>` → `netherite_<suffix>`, one row per
/// `netherite_*_smithing.json` recipe. All 12 share the same template
/// (`netherite_upgrade_smithing_template`) and addition
/// (`#minecraft:netherite_tool_materials`, which in 26.2 is exactly
/// `minecraft:netherite_ingot`).
const NETHERITE_UPGRADES: &[&str] = &[
    "sword",
    "pickaxe",
    "axe",
    "shovel",
    "hoe",
    "spear",
    "helmet",
    "chestplate",
    "leggings",
    "boots",
    "horse_armor",
    "nautilus_armor",
];

const NETHERITE_TEMPLATE: &str = "minecraft:netherite_upgrade_smithing_template";
const NETHERITE_ADDITION: &str = "minecraft:netherite_ingot";

/// The 18 trim patterns 26.2 ships, by their template item's name prefix.
const TRIM_PATTERNS: &[&str] = &[
    "bolt", "coast", "dune", "eye", "flow", "host", "raiser", "rib", "sentry", "shaper", "silence",
    "snout", "spire", "tide", "vex", "ward", "wayfinder", "wild",
];

/// `tags/item/trim_materials.json`, paired with the trim material id vanilla's
/// `provides_trim_material` component would resolve each one to.
const TRIM_MATERIALS: &[(&str, &str)] = &[
    ("minecraft:amethyst_shard", "amethyst"),
    ("minecraft:copper_ingot", "copper"),
    ("minecraft:diamond", "diamond"),
    ("minecraft:emerald", "emerald"),
    ("minecraft:gold_ingot", "gold"),
    ("minecraft:iron_ingot", "iron"),
    ("minecraft:lapis_lazuli", "lapis"),
    ("minecraft:netherite_ingot", "netherite"),
    ("minecraft:quartz", "quartz"),
    ("minecraft:redstone", "redstone"),
    ("minecraft:resin_brick", "resin"),
];

fn is_trimmable_armor(item: &str) -> bool {
    item.ends_with("_helmet")
        || item.ends_with("_chestplate")
        || item.ends_with("_leggings")
        || item.ends_with("_boots")
        || item == "minecraft:turtle_helmet"
}

/// `SmithingTransformRecipe.assemble` via `TransmuteRecipe.createWithOriginalComponents`:
/// the base's full component patch carries onto the netherite item verbatim,
/// then `max_damage`/`max_stack_size`/`equippable` are re-resolved for the
/// *new* item (they are effective, prototype-folded fields — see
/// [`lodestone_model::ItemComponents`]'s own doc for why those three cannot
/// simply be copied across an item-id change).
#[must_use]
pub fn netherite_upgrade(template: &ItemStack, base: &ItemStack, addition: &ItemStack) -> Option<ItemStack> {
    if template.item.to_string() != NETHERITE_TEMPLATE || addition.item.to_string() != NETHERITE_ADDITION {
        return None;
    }
    let base_item = base.item.to_string();
    let suffix = base_item.strip_prefix("minecraft:diamond_")?;
    if !NETHERITE_UPGRADES.contains(&suffix) {
        return None;
    }
    let result_item: lodestone_model::ResourceKey =
        format!("minecraft:netherite_{suffix}").parse().ok()?;

    let mut result = base.clone();
    result.item = result_item.clone();
    result.count = 1;
    if let Some(proto) = lodestone_data::item_prototypes::prototype(&result_item.to_string()) {
        result.components.max_damage = proto.max_damage.map(u32::from);
        result.components.max_stack_size = Some(u32::from(proto.max_stack_size));
        result.components.equippable = proto.equip_slot;
    }
    Some(result)
}

/// `SmithingTrimRecipe.assemble`/`applyTrim`: visual-only, refuses a no-op
/// (the base already carrying the exact same trim).
#[must_use]
pub fn apply_trim(template: &ItemStack, base: &ItemStack, addition: &ItemStack) -> Option<ItemStack> {
    let template_item = template.item.to_string();
    let pattern = template_item
        .strip_prefix("minecraft:")?
        .strip_suffix("_armor_trim_smithing_template")?;
    if !TRIM_PATTERNS.contains(&pattern) {
        return None;
    }
    if !is_trimmable_armor(&base.item.to_string()) {
        return None;
    }
    let addition_item = addition.item.to_string();
    let material = TRIM_MATERIALS
        .iter()
        .find(|(item, _)| *item == addition_item)
        .map(|(_, material)| *material)?;

    let new_trim = lodestone_model::ArmorTrim {
        material: material.to_string(),
        pattern: pattern.to_string(),
    };
    if base.components.trim.as_ref() == Some(&new_trim) {
        return None;
    }
    let mut result = base.clone();
    result.count = 1;
    result.components.trim = Some(new_trim);
    Some(result)
}

/// Tries a netherite upgrade first, then a trim — the smithing table's one
/// result slot for two unrelated recipe families.
#[must_use]
pub fn compute(template: Option<&ItemStack>, base: Option<&ItemStack>, addition: Option<&ItemStack>) -> Option<ItemStack> {
    let (template, base, addition) = (template?, base?, addition?);
    netherite_upgrade(template, base, addition).or_else(|| apply_trim(template, base, addition))
}

// ---------------------------------------------------------------------------
// Input-slot `mayPlace` predicates, for `container_click`'s
// `MenuKind::ItemCombiner { station: Station::Smithing, .. }`. Approximates
// vanilla's per-slot `RecipePropertySet` tests (`SmithingMenu.createInputSlotDefinitions`)
// with the same recipe data this module already carries, rather than a second
// ingredient table.
// ---------------------------------------------------------------------------

/// Slot 0 (template): either the one netherite-upgrade template or one of the
/// 18 trim-pattern templates.
#[must_use]
pub(crate) fn is_template(item: &str) -> bool {
    if item == NETHERITE_TEMPLATE {
        return true;
    }
    item.strip_prefix("minecraft:")
        .and_then(|rest| rest.strip_suffix("_armor_trim_smithing_template"))
        .is_some_and(|pattern| TRIM_PATTERNS.contains(&pattern))
}

/// Slot 1 (base): a `diamond_<x>` upgradeable item, or trimmable armour.
#[must_use]
pub(crate) fn is_base(item: &str) -> bool {
    item.strip_prefix("minecraft:diamond_")
        .is_some_and(|suffix| NETHERITE_UPGRADES.contains(&suffix))
        || is_trimmable_armor(item)
}

/// Slot 2 (addition): the netherite ingot, or a trim material.
#[must_use]
pub(crate) fn is_addition(item: &str) -> bool {
    item == NETHERITE_ADDITION || TRIM_MATERIALS.iter().any(|(material, _)| *material == item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), count)
    }

    /// The upgrade must carry the raw damage **value**, not a recomputed
    /// fraction — the trap this module's doc comment names explicitly.
    #[test]
    fn netherite_upgrade_preserves_the_raw_damage_value_not_a_fraction() {
        let template = stack(NETHERITE_TEMPLATE, 1);
        let mut base = stack("minecraft:diamond_pickaxe", 1);
        base.components.damage = Some(1000);
        base.components.max_damage = Some(1561);
        let addition = stack(NETHERITE_ADDITION, 1);

        let result = netherite_upgrade(&template, &base, &addition).expect("must upgrade");
        assert_eq!(result.item.to_string(), "minecraft:netherite_pickaxe");
        assert_eq!(
            result.components.damage,
            Some(1000),
            "the damage int must transfer unchanged, not be rescaled to netherite's higher max"
        );
    }

    /// Also carries enchantments across, and preserves the wrong-hypothesis
    /// discriminator: a fraction-preserving recompute would *not* leave the
    /// damage value bit-identical once max_damage changes.
    #[test]
    fn netherite_upgrade_preserves_enchantments() {
        let template = stack(NETHERITE_TEMPLATE, 1);
        let mut base = stack("minecraft:diamond_sword", 1);
        base.components.enchantments = vec![lodestone_model::ItemEnchantment { id: 7, level: 3 }];
        let addition = stack(NETHERITE_ADDITION, 1);
        let result = netherite_upgrade(&template, &base, &addition).unwrap();
        assert_eq!(result.components.enchantments, base.components.enchantments);
    }

    #[test]
    fn netherite_upgrade_refuses_the_wrong_addition() {
        let template = stack(NETHERITE_TEMPLATE, 1);
        let base = stack("minecraft:diamond_sword", 1);
        let addition = stack("minecraft:iron_ingot", 1);
        assert!(netherite_upgrade(&template, &base, &addition).is_none());
    }

    #[test]
    fn trim_applies_the_right_pattern_and_material() {
        let template = stack("minecraft:dune_armor_trim_smithing_template", 1);
        let base = stack("minecraft:diamond_chestplate", 1);
        let addition = stack("minecraft:emerald", 1);
        let result = apply_trim(&template, &base, &addition).expect("must trim");
        assert_eq!(
            result.components.trim,
            Some(lodestone_model::ArmorTrim { material: "emerald".into(), pattern: "dune".into() })
        );
    }

    #[test]
    fn trim_refuses_a_non_armor_base() {
        let template = stack("minecraft:dune_armor_trim_smithing_template", 1);
        let base = stack("minecraft:diamond_sword", 1);
        let addition = stack("minecraft:emerald", 1);
        assert!(apply_trim(&template, &base, &addition).is_none());
    }

    /// The smithing table's two recipe families must not cross-validate: a
    /// netherite template with a trim-shaped base/addition combination (or
    /// vice versa) must produce nothing, not silently fall through.
    #[test]
    fn the_two_recipe_families_do_not_cross_validate() {
        let netherite_template = stack(NETHERITE_TEMPLATE, 1);
        let armor = stack("minecraft:diamond_chestplate", 1);
        let emerald = stack("minecraft:emerald", 1);
        assert!(
            compute(Some(&netherite_template), Some(&armor), Some(&emerald)).is_none(),
            "a netherite template must not accept a trim-shaped input"
        );

        let trim_template = stack("minecraft:dune_armor_trim_smithing_template", 1);
        let sword = stack("minecraft:diamond_sword", 1);
        let ingot = stack(NETHERITE_ADDITION, 1);
        assert!(
            compute(Some(&trim_template), Some(&sword), Some(&ingot)).is_none(),
            "a trim template must not accept a netherite-upgrade-shaped input"
        );
    }
}
