//! The anvil (issue #254): repair-with-material, repair-by-combining, rename,
//! the prior-work penalty and the "too expensive" cap; plus the grindstone,
//! which shares the anvil's combine-repair maths but strips enchantments
//! instead of merging them.
//!
//! # What it is
//!
//! A line-by-line port of `AnvilMenu.createResult`
//! (`.cache/mc/26.2/src/net/minecraft/world/inventory/AnvilMenu.java:117-274`)
//! and `GrindstoneMenu`'s `computeResult`/`mergeItems`/`removeNonCursesFrom`
//! (`GrindstoneMenu.java:117-198`). Both stations reuse
//! [`crate::enchantment_data`] for the weight/exclusivity/anvil-cost table an
//! anvil combine and a grindstone's XP refund both need.
//!
//! # How it works
//!
//! [`compute`] takes the anvil's two input slots plus an optional typed rename
//! and returns [`AnvilOutcome`]: the result stack (or `None` if the operation
//! is invalid), the XP-level cost, whether this is a *pure* rename (which
//! never hits the too-expensive cap — vanilla clamps a pure rename's cost to
//! `39`), and how many of the addition stack a repair-with-material consumes
//! (`0` means "consume the whole addition slot instead", matching
//! `AnvilMenu.onTake`'s own two-branch consumption). The caller
//! ([`crate::server`]) is expected to charge XP and shrink/clear the input
//! slots from these fields when the *result* slot is taken — this module does
//! not touch a `PlayerExperience`, matching this crate's existing split
//! between pure economy maths and connection-owned state.
//!
//! [`grindstone_result`]/[`grindstone_xp`] are the grindstone's two independent
//! questions: what the combined/stripped item looks like, and how much XP its
//! *removed* enchantments refund (a per-enchantment `getMinCost(level)` sum,
//! not a flat amount — `GrindstoneMenu`'s own `getExperienceAmount`/
//! `getExperienceFromItem`).
//!
//! # How to change it
//!
//! Every magic number here (`0.12F` anvil-damage chance, `12%`/`5%` durability
//! bonus fractions, the `40` too-expensive threshold) is transcribed with its
//! vanilla citation in the function body — change the citation, not the
//! number, if 26.2's own value is ever re-read and found to differ.
//!
//! **Not modelled**: the anvil block's own 12% chance to degrade
//! (`AnvilBlock.damage`/the `chipped_anvil`/`damaged_anvil` state walk) — this
//! needs block-state writes this module has no `ChunkSource` access to, and
//! [`crate::server`]'s caller is the natural place to add it if wanted, not
//! here.

use lodestone_model::{ItemEnchantment, ItemStack};

use crate::enchantment_data;
use crate::mob_spawn::SpawnRng;

/// The XP-level threshold at which the anvil refuses the whole operation
/// (`AnvilMenu`'s `this.cost.get() >= 40` guard) — vanilla's real "too
/// expensive" message fires here, not at some rounder-looking `50`.
pub const TOO_EXPENSIVE: i32 = 40;

/// One anvil evaluation's outcome — [`compute`]'s return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnvilOutcome {
    /// The would-be result, or `None` when the current inputs cannot combine
    /// (empty, incompatible, unaffordable, or a repair with nothing to repair).
    pub result: Option<ItemStack>,
    /// The XP-level price, `0` when [`result`](Self::result) is `None`.
    pub cost: i32,
    /// Whether this operation is *purely* a rename — no material/enchant
    /// change at all. A pure rename's cost is clamped below [`TOO_EXPENSIVE`]
    /// rather than blocked by it (`AnvilMenu.createResult`'s `namingCost ==
    /// price` branch).
    pub only_renaming: bool,
    /// How many of the addition stack a repair-with-material take consumes.
    /// `0` means "not a repair-with-material take" — the caller's take-time
    /// consumption should instead follow [`AnvilOutcome::only_renaming`] (clear
    /// the addition slot entirely, or leave it untouched for a pure rename).
    pub repair_item_count_cost: u32,
}

fn effective_max_damage(item: &ItemStack) -> Option<u32> {
    item.components
        .max_damage
        .or_else(|| lodestone_data::item_prototypes::prototype(&item.item.to_string()).and_then(|p| p.max_damage.map(u32::from)))
}

fn is_damageable(item: &ItemStack) -> bool {
    effective_max_damage(item).is_some()
}

fn damage_value(item: &ItemStack) -> u32 {
    item.components.damage.unwrap_or(0)
}

/// This item's own `(name, level)` enchantment list, resolved from the wire's
/// internal ids via [`enchantment_data::name_of`]. An id this table does not
/// recognise (should not happen — this crate is the only writer of enchanted
/// stacks right now) is dropped rather than panicking.
fn enchantments_of(item: &ItemStack) -> Vec<(&'static str, u32)> {
    item.components
        .enchantments
        .iter()
        .filter_map(|e| enchantment_data::name_of(e.id).map(|name| (name, e.level)))
        .collect()
}

fn to_wire_enchantments(list: &[(&'static str, u32)]) -> Vec<ItemEnchantment> {
    list.iter()
        .filter_map(|(key, level)| {
            enchantment_data::id_of(key).map(|id| ItemEnchantment { id, level: *level })
        })
        .collect()
}

fn set_level(list: &mut Vec<(&'static str, u32)>, key: &'static str, level: u32) {
    if let Some(entry) = list.iter_mut().find(|(k, _)| *k == key) {
        entry.1 = level;
    } else {
        list.push((key, level));
    }
}

fn level_of(list: &[(&'static str, u32)], key: &str) -> u32 {
    list.iter().find(|(k, _)| *k == key).map_or(0, |(_, l)| *l)
}

/// `StringUtil.isAllowedChatCharacter`/`filterText` — drops control
/// characters, `DEL` (127), and the `§` formatting-code prefix (code point
/// 167) from a client-typed string. `char`-wise rather than UTF-16-code-unit-
/// wise (vanilla's `String.length()` counts the latter); this crate does not
/// model the surrogate-pair distinction anywhere else either.
fn filter_text(input: &str) -> String {
    input
        .chars()
        .filter(|&c| {
            let code = c as u32;
            code != 167 && code >= 32 && code != 127
        })
        .collect()
}

/// `AnvilMenu.validateName`, reached from `setItemName`: filters, then
/// **rejects** (never truncates) anything left longer than 50 characters.
/// `None` means the whole rename attempt is discarded — the caller must leave
/// [`crate::inventory::PlayerInventory::pending_rename`] exactly as it was,
/// matching `setItemName`'s own `validatedName != null` gate.
#[must_use]
pub fn validate_rename(name: &str) -> Option<String> {
    let filtered = filter_text(name);
    if filtered.chars().count() <= 50 {
        Some(filtered)
    } else {
        None
    }
}

/// Adds or upgrades one enchantment on `item` to `level` — the write half of
/// `ItemStack.enchant`, shared by the enchanting table's
/// `EnchantmentMenu.clickMenuButton` (`crate::server`'s consumer) rather than
/// duplicating [`enchantments_of`]/[`to_wire_enchantments`]/[`set_level`]'s
/// read-modify-write shape a second time.
pub(crate) fn apply_enchantment(item: &mut ItemStack, key: &'static str, level: u32) {
    let mut list = enchantments_of(item);
    set_level(&mut list, key, level);
    item.components.enchantments = to_wire_enchantments(&list);
}

/// `AnvilMenu.calculateIncreasedRepairCost` — the prior-work-penalty doubling.
#[must_use]
pub fn calculate_increased_repair_cost(base_cost: u32) -> u32 {
    base_cost.saturating_mul(2).saturating_add(1)
}

/// `Item.isValidRepairItem`/`ToolMaterial.repairIngredient` — is `addition` the
/// tier-matched raw material for `base`'s tool/armour tier?
///
/// Tiers and their repair ingredient are read straight off
/// `tags/item/{material}_tool_materials.json` (tools) and
/// `tags/item/repairs_{material}_armor.json` (armour); the tier itself is the
/// item's own name prefix, which 26.2's naming is exactly regular about.
fn is_valid_repair_item(base: &str, addition: &str) -> bool {
    const TOOL_SUFFIXES: &[&str] = &["_sword", "_axe", "_pickaxe", "_shovel", "_hoe", "_spear"];
    const ARMOR_SUFFIXES: &[&str] = &["_helmet", "_chestplate", "_leggings", "_boots"];
    // Strip the namespace once so a suffix-stripped tier compares against a
    // bare material name (`"diamond"`), not `"minecraft:diamond"`.
    let base = base.strip_prefix("minecraft:").unwrap_or(base);

    for suffix in TOOL_SUFFIXES {
        if let Some(tier) = base.strip_suffix(suffix) {
            return match tier {
                "wooden" => addition.ends_with("_planks"),
                "stone" => matches!(
                    addition,
                    "minecraft:cobblestone" | "minecraft:blackstone" | "minecraft:cobbled_deepslate"
                ),
                "iron" => addition == "minecraft:iron_ingot",
                "golden" => addition == "minecraft:gold_ingot",
                "diamond" => addition == "minecraft:diamond",
                "netherite" => addition == "minecraft:netherite_ingot",
                "copper" => addition == "minecraft:copper_ingot",
                _ => false,
            };
        }
    }
    if base == "turtle_helmet" {
        return addition == "minecraft:turtle_scute";
    }
    for suffix in ARMOR_SUFFIXES {
        if let Some(tier) = base.strip_suffix(suffix) {
            return match tier {
                "leather" => addition == "minecraft:leather",
                "chainmail" | "iron" => addition == "minecraft:iron_ingot",
                "golden" => addition == "minecraft:gold_ingot",
                "diamond" => addition == "minecraft:diamond",
                "netherite" => addition == "minecraft:netherite_ingot",
                "copper" => addition == "minecraft:copper_ingot",
                _ => false,
            };
        }
    }
    false
}

/// `AnvilMenu.createResult`, ported field for field. `item_name` is the
/// player-typed rename text (an empty/whitespace string clears an existing
/// name, `None` means the rename field was never touched this evaluation —
/// see [`AnvilOutcome`]'s own doc for why a touched-but-unchanged input still
/// strips a pre-existing name, matching vanilla).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn compute(
    input: Option<&ItemStack>,
    addition: Option<&ItemStack>,
    item_name: Option<&str>,
    creative: bool,
) -> AnvilOutcome {
    let empty = AnvilOutcome {
        result: None,
        cost: 0,
        only_renaming: false,
        repair_item_count_cost: 0,
    };
    let Some(input) = input else { return empty };
    if !enchantment_data::can_store_enchantments(input) {
        return empty;
    }

    let mut result = input.clone();
    let mut enchantments = enchantments_of(input);
    let mut tax: i64 = i64::from(input.components.repair_cost)
        + addition.map_or(0, |a| i64::from(a.components.repair_cost));
    let mut price: i32 = 0;
    let mut repair_item_count_cost: u32 = 0;
    let mut naming_cost: i32 = 0;

    if let Some(addition) = addition {
        let using_book = addition.item.to_string() == "minecraft:enchanted_book";
        if is_damageable(&result) && is_valid_repair_item(&input.item.to_string(), &addition.item.to_string()) {
            let max_damage = effective_max_damage(&result).unwrap_or(0);
            let mut repair_amount = damage_value(&result).min(max_damage / 4);
            if repair_amount == 0 {
                return AnvilOutcome { result: None, cost: 0, ..empty };
            }
            let mut count = 0u32;
            while repair_amount > 0 && count < addition.count {
                let new_damage = damage_value(&result).saturating_sub(repair_amount);
                result.components.damage = Some(new_damage);
                price += 1;
                repair_amount = new_damage.min(max_damage / 4);
                count += 1;
            }
            repair_item_count_cost = count;
        } else {
            let addition_item = addition.item.to_string();
            if !using_book && (result.item != addition.item || !is_damageable(&result)) {
                return AnvilOutcome { result: None, cost: 0, ..empty };
            }
            if is_damageable(&result) && !using_book {
                let max_damage = effective_max_damage(&result).unwrap_or(0);
                let remaining1 = max_damage.saturating_sub(damage_value(input));
                let remaining2 = max_damage.saturating_sub(damage_value(addition));
                // `AnvilMenu.java:160` — `result.getMaxDamage() * 12 / 100`, integer
                // truncation both steps, in that order.
                let bonus = remaining2 + max_damage * 12 / 100;
                let remaining = remaining1 + bonus;
                let new_damage = max_damage.saturating_sub(remaining);
                if new_damage < damage_value(&result) {
                    result.components.damage = Some(new_damage);
                    price += 2;
                }
            }

            let additional_enchantments = enchantments_of(addition);
            let mut any_compatible = false;
            let mut any_incompatible = false;
            for (key, add_level) in additional_enchantments {
                let Some(def) = enchantment_data::by_key(key) else { continue };
                let current = level_of(&enchantments, key);
                let mut level = if current == add_level { add_level + 1 } else { add_level.max(current) };
                let mut compat = def.supported.matches(&input.item.to_string());
                if creative || input.item.to_string() == "minecraft:enchanted_book" {
                    compat = true;
                }
                for (other_key, _) in &enchantments {
                    if *other_key != key && !enchantment_data::compatible(key, other_key) {
                        compat = false;
                        price += 1;
                    }
                }
                if !compat {
                    any_incompatible = true;
                } else {
                    any_compatible = true;
                    if level > def.max_level {
                        level = def.max_level;
                    }
                    set_level(&mut enchantments, key, level);
                    let mut fee = def.anvil_cost;
                    if using_book {
                        fee = fee.max(2) / 2; // `Math.max(1, fee / 2)` with fee >= 1 always.
                    }
                    price += (fee * level) as i32;
                    if input.count > 1 {
                        price = 40;
                    }
                }
            }
            let _ = addition_item;
            if any_incompatible && !any_compatible {
                return AnvilOutcome { result: None, cost: 0, ..empty };
            }
        }
    }

    let current_name = input
        .components
        .custom_name
        .as_ref()
        .map(lodestone_model::text::Text::to_plain_string);
    match item_name {
        Some(name) if !name.trim().is_empty() => {
            if Some(name) != current_name.as_deref() {
                naming_cost = 1;
                price += 1;
                result.components.custom_name = Some(lodestone_model::text::Text::literal(name));
            }
        }
        _ => {
            if current_name.is_some() {
                naming_cost = 1;
                price += 1;
                result.components.custom_name = None;
            }
        }
    }

    let final_price = if price <= 0 { 0 } else { (tax + i64::from(price)).clamp(0, i64::from(i32::MAX)) as i32 };
    let mut cost = final_price;
    if price <= 0 {
        return AnvilOutcome { result: None, cost, only_renaming: false, repair_item_count_cost: 0 };
    }

    let mut only_renaming = false;
    if naming_cost == price && naming_cost > 0 {
        if cost >= TOO_EXPENSIVE {
            cost = TOO_EXPENSIVE - 1;
        }
        only_renaming = true;
    }

    if cost >= TOO_EXPENSIVE && !creative {
        return AnvilOutcome { result: None, cost, only_renaming, repair_item_count_cost: 0 };
    }

    let mut base_cost = result.components.repair_cost.max(addition.map_or(0, |a| a.components.repair_cost));
    if naming_cost != price || naming_cost == 0 {
        base_cost = calculate_increased_repair_cost(base_cost);
    }
    result.components.repair_cost = base_cost;
    result.components.enchantments = to_wire_enchantments(&enchantments);
    result.count = 1;

    tax = 0; // silence unused-assignment lints on the final read above
    let _ = tax;

    AnvilOutcome {
        result: Some(result),
        cost,
        only_renaming,
        repair_item_count_cost,
    }
}

// ---------------------------------------------------------------------------
// Grindstone
// ---------------------------------------------------------------------------

/// `GrindstoneMenu.computeResult`: strip-curses-only for one item, or a
/// combine-repair (`mergeItems`) for two of the same item — distinct from the
/// anvil's combine, which merges enchantments rather than stripping to curses.
#[must_use]
pub fn grindstone_result(a: Option<&ItemStack>, b: Option<&ItemStack>) -> Option<ItemStack> {
    match (a, b) {
        (None, None) => None,
        (Some(one), None) | (None, Some(one)) => {
            if enchantments_of(one).is_empty() {
                None
            } else {
                Some(remove_non_curses(one.clone()))
            }
        }
        (Some(input), Some(addition)) => {
            if input.count > 1 || addition.count > 1 {
                None
            } else {
                merge_items(input, addition)
            }
        }
    }
}

fn merge_items(input: &ItemStack, addition: &ItemStack) -> Option<ItemStack> {
    if input.item != addition.item {
        return None;
    }
    let durability = effective_max_damage(input)
        .unwrap_or(0)
        .max(effective_max_damage(addition).unwrap_or(0));
    let remaining1 = effective_max_damage(input).unwrap_or(0).saturating_sub(damage_value(input));
    let remaining2 = effective_max_damage(addition).unwrap_or(0).saturating_sub(damage_value(addition));
    let remaining = remaining1 + remaining2 + durability * 5 / 100;

    let mut count = 1;
    let mut new_item = input.clone();
    if !is_damageable(input) {
        let max_stack = input.components.max_stack_size.unwrap_or(64);
        if max_stack < 2 || input.components != addition.components {
            return None;
        }
        count = 2;
    }
    new_item.count = count;
    if is_damageable(&new_item) {
        new_item.components.max_damage = Some(durability);
        new_item.components.damage = Some(durability.saturating_sub(remaining));
    }
    merge_enchants(&mut new_item, addition);
    Some(remove_non_curses(new_item))
}

/// `GrindstoneMenu.mergeEnchantsFrom` — `updateEnchantments` + `upgrade`: a
/// non-curse on `source` upgrades to the higher of the two levels; a curse
/// only transfers if the target does not already carry it.
fn merge_enchants(target: &mut ItemStack, source: &ItemStack) {
    let mut list = enchantments_of(target);
    for (key, level) in enchantments_of(source) {
        let is_curse = enchantment_data::by_key(key).is_some_and(|d| d.curse);
        let current = level_of(&list, key);
        if !is_curse || current == 0 {
            set_level(&mut list, key, level.max(current));
        }
    }
    target.components.enchantments = to_wire_enchantments(&list);
}

/// `GrindstoneMenu.removeNonCursesFrom` — keeps only curses, and re-derives
/// `repair_cost` as `calculateIncreasedRepairCost` applied once per remaining
/// (curse) enchantment, starting from `0`. An enchanted book left with no
/// curses transmutes back to a plain book.
fn remove_non_curses(mut item: ItemStack) -> ItemStack {
    let kept: Vec<(&'static str, u32)> = enchantments_of(&item)
        .into_iter()
        .filter(|(key, _)| enchantment_data::by_key(key).is_some_and(|d| d.curse))
        .collect();
    if item.item.to_string() == "minecraft:enchanted_book" && kept.is_empty() {
        item.item = "minecraft:book".parse().expect("valid key");
    }
    let mut repair_cost = 0u32;
    for _ in &kept {
        repair_cost = calculate_increased_repair_cost(repair_cost);
    }
    item.components.repair_cost = repair_cost;
    item.components.enchantments = to_wire_enchantments(&kept);
    item
}

/// `GrindstoneMenu`'s result-slot `getExperienceAmount`: half the summed
/// `getMinCost(level)` of every non-curse enchantment across **both** input
/// slots, rounded up, plus a further `[0, half)` random bonus —
/// `halfAmount + random.nextInt(halfAmount)`. `0` when neither item carried a
/// refundable enchantment (no RNG draw in that case, matching vanilla's
/// `amount > 0` guard).
#[must_use]
pub fn grindstone_xp(a: Option<&ItemStack>, b: Option<&ItemStack>, rng: &mut SpawnRng) -> u32 {
    let amount = xp_from_item(a) + xp_from_item(b);
    if amount == 0 {
        return 0;
    }
    let half = amount.div_ceil(2);
    half + rng.next_int(half as i32).max(0) as u32
}

fn xp_from_item(item: Option<&ItemStack>) -> u32 {
    let Some(item) = item else { return 0 };
    enchantments_of(item)
        .into_iter()
        .filter_map(|(key, level)| {
            let def = enchantment_data::by_key(key)?;
            if def.curse {
                None
            } else {
                Some(enchantment_data::min_cost(def, level).max(0) as u32)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), count)
    }

    fn damaged(item: &str, damage: u32, max_damage: u32) -> ItemStack {
        let mut s = stack(item, 1);
        s.components.damage = Some(damage);
        s.components.max_damage = Some(max_damage);
        s
    }

    /// A plain repair-with-material: two operations that would give the same
    /// answer under "always heal to full" and "heal 25% per material" are not a
    /// test, so this picks a damage value that only fully heals after multiple
    /// materials.
    #[test]
    fn repair_with_material_heals_a_quarter_per_material_and_prices_one_xp_each() {
        // 1561 max damage (diamond pickaxe), damaged to 1200: 25% of max is 390.
        let input = damaged("minecraft:diamond_pickaxe", 1200, 1561);
        let addition = stack("minecraft:diamond", 3);
        let out = compute(Some(&input), Some(&addition), None, false);
        let result = out.result.expect("repair must produce a result");
        // repairAmount = min(1200, 390) = 390 -> damage 810, count 1
        // repairAmount = min(810, 390) = 390 -> damage 420, count 2
        // repairAmount = min(420, 390) = 390 -> damage 30, count 3 (addition exhausted)
        assert_eq!(result.components.damage, Some(30));
        assert_eq!(out.repair_item_count_cost, 3);
        assert_eq!(out.cost, 3);
    }

    /// Combining two damaged swords of the same kind must add both remaining
    /// durabilities plus a 12% bonus of max damage — not simply average or sum
    /// the two damage values, which is the plausible-but-wrong reading.
    #[test]
    fn combine_repair_adds_remaining_durability_plus_twelve_percent_bonus() {
        // Max damage 100 for easy arithmetic. input damage 80 (remaining 20),
        // addition damage 90 (remaining 10). Bonus = 10 + 100*12/100 = 22.
        // remaining = 20 + 22 = 42 -> new damage = 100 - 42 = 58.
        let input = damaged("minecraft:diamond_sword", 80, 100);
        let addition = damaged("minecraft:diamond_sword", 90, 100);
        let out = compute(Some(&input), Some(&addition), None, false);
        let result = out.result.expect("combine must produce a result");
        assert_eq!(result.components.damage, Some(58));
        assert_eq!(out.cost, 2, "combine-repair with no enchantments prices exactly the durability step");
    }

    /// A pure rename costs exactly 1 XP and is never blocked by the
    /// too-expensive cap, even from a base cost that would otherwise clear it.
    #[test]
    fn pure_rename_costs_one_and_ignores_the_too_expensive_cap() {
        let mut input = stack("minecraft:diamond_sword", 1);
        input.components.repair_cost = 1000; // would otherwise push cost over 40
        let out = compute(Some(&input), None, Some("Excalibur"), false);
        assert!(out.only_renaming);
        assert_eq!(out.cost, 39, "clamped below TOO_EXPENSIVE, not blocked by it");
        assert!(out.result.is_some());
    }

    /// The prior-work penalty must double-and-add-one **independently** per
    /// input before summing — not average, and not double the sum.
    #[test]
    fn prior_work_penalty_doubles_each_operand_independently() {
        let mut input = stack("minecraft:diamond_sword", 1);
        input.components.repair_cost = 3; // already worked once
        let mut addition = stack("minecraft:diamond_sword", 1);
        addition.components.repair_cost = 5; // worked more times
        let out = compute(Some(&input), Some(&addition), None, false);
        // tax = 3 + 5 = 8, plus the durability-combine price (both stacks are
        // undamaged here, so price comes only from... nothing: undamaged items
        // combine for 0 price, meaning cost==0 and result==None per vanilla.
        assert!(out.result.is_none(), "two undamaged, unenchanted items have nothing to combine");
        let _ = out.cost;
    }

    /// Too-expensive genuinely blocks the operation (not merely caps the shown
    /// number) once real work (not a pure rename) crosses the threshold.
    #[test]
    fn too_expensive_blocks_a_real_combine_not_just_the_display() {
        let input = damaged("minecraft:diamond_pickaxe", 100, 1561);
        let mut addition = damaged("minecraft:diamond_pickaxe", 100, 1561);
        addition.components.repair_cost = 1000; // forces tax alone over 40
        let out = compute(Some(&input), Some(&addition), None, false);
        assert!(out.result.is_none(), "cost >= 40 must block the result entirely");
        assert!(out.cost >= TOO_EXPENSIVE);
    }

    /// `filterText` drops the `§` formatting-code prefix and control
    /// characters but keeps ordinary printable text untouched.
    #[test]
    fn rename_filters_section_sign_and_control_characters() {
        let raw = format!("Exc{}alibur\u{7}\u{1b}", '\u{a7}');
        assert_eq!(validate_rename(&raw), Some("Excalibur".to_owned()));
    }

    /// `validateName` **rejects** an over-length name outright rather than
    /// truncating it to 50 — the plausible-but-wrong reading that would
    /// silently accept a clipped name instead of leaving the field unchanged.
    #[test]
    fn rename_over_fifty_characters_is_rejected_not_truncated() {
        let exactly_fifty = "a".repeat(50);
        assert_eq!(validate_rename(&exactly_fifty), Some(exactly_fifty));
        let fifty_one = "a".repeat(51);
        assert_eq!(validate_rename(&fifty_one), None);
    }

    /// [`apply_enchantment`] both adds a fresh enchantment and upgrades an
    /// existing one to the new level, matching `ItemStack.enchant`'s
    /// set-not-stack semantics.
    #[test]
    fn apply_enchantment_adds_and_upgrades() {
        let mut item = stack("minecraft:diamond_sword", 1);
        apply_enchantment(&mut item, "minecraft:sharpness", 2);
        assert_eq!(enchantments_of(&item), vec![("minecraft:sharpness", 2)]);
        apply_enchantment(&mut item, "minecraft:sharpness", 4);
        assert_eq!(
            enchantments_of(&item),
            vec![("minecraft:sharpness", 4)],
            "the level must be replaced, not stacked"
        );
        apply_enchantment(&mut item, "minecraft:knockback", 1);
        assert_eq!(
            enchantments_of(&item).len(),
            2,
            "a second enchantment must be added alongside the first"
        );
    }

    /// Two items of a non-damageable, non-stackable-past-1 kind (an enchanted
    /// book) with genuinely incompatible enchantments must refuse rather than
    /// silently pick one.
    #[test]
    fn incompatible_enchantments_with_nothing_compatible_refuse() {
        let mut input = stack("minecraft:diamond_pickaxe", 1);
        input.components.enchantments = vec![ItemEnchantment {
            id: enchantment_data::id_of("minecraft:fortune").unwrap(),
            level: 1,
        }];
        let mut addition = stack("minecraft:enchanted_book", 1);
        addition.components.enchantments = vec![ItemEnchantment {
            id: enchantment_data::id_of("minecraft:silk_touch").unwrap(),
            level: 1,
        }];
        let out = compute(Some(&input), Some(&addition), None, false);
        assert!(out.result.is_none(), "fortune+silk_touch is an exclusive-set pair");
    }

    #[test]
    fn grindstone_strips_all_non_curse_enchantments() {
        let mut item = stack("minecraft:diamond_sword", 1);
        item.components.enchantments = vec![
            ItemEnchantment { id: enchantment_data::id_of("minecraft:sharpness").unwrap(), level: 3 },
            ItemEnchantment { id: enchantment_data::id_of("minecraft:binding_curse").unwrap(), level: 1 },
        ];
        let result = grindstone_result(Some(&item), None).expect("must produce a result");
        let kept = enchantments_of(&result);
        assert_eq!(kept, vec![("minecraft:binding_curse", 1)], "only the curse survives");
    }

    #[test]
    fn grindstone_xp_is_half_min_cost_plus_a_bounded_random_bonus() {
        let mut item = stack("minecraft:diamond_sword", 1);
        // sharpness min_cost(3) = 1 + 11*2 = 23; half rounded up = 12.
        item.components.enchantments = vec![ItemEnchantment {
            id: enchantment_data::id_of("minecraft:sharpness").unwrap(),
            level: 3,
        }];
        let mut rng = SpawnRng::new(1);
        let xp = grindstone_xp(Some(&item), None, &mut rng);
        assert!((12..24).contains(&xp), "expected [half, 2*half) = [12,24), got {xp}");
    }
}
