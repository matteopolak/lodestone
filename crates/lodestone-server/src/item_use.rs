//! `Item.use` — what a right-click with nothing in front of you does.
//!
//! # What it is
//!
//! `server`'s `apply_use_item` handled exactly one thing: a bow draw or a
//! throwable's launch (`launch_intent`). Everything else — **all of eating and
//! drinking, and every equip-by-right-click** — reached the server and did
//! nothing at all, which is one gap rather than several: they are consecutive
//! arms of the same method.
//!
//! `Item.use` in 26.2, in its own order:
//!
//! 1. `DataComponents.CONSUMABLE` → `consumable.startConsuming(player, stack, hand)`
//! 2. `DataComponents.EQUIPPABLE`, **gated on `equippable.swappable()`** →
//!    `equippable.swapWithEquipmentSlot(stack, player)`
//! 3. `DataComponents.BLOCKS_ATTACKS` → `startUsingItem` (a shield raise)
//! 4. `DataComponents.KINETIC_WEAPON` → `startUsingItem` plus a sound
//!
//! **The order is load-bearing**: an item that is both consumable and equippable
//! eats rather than equips, and nothing about a wrong order is visible until you
//! meet such an item. This module supplies arms 1 and 2. Arms 3 and 4 are not
//! modelled — there is no blocking or kinetic-weapon model in this crate for a
//! raised shield to feed, and `startUsingItem` with no consumer would be an
//! island.
//!
//! # Where the numbers come from
//!
//! [`FOODS`] is a transcription of the three-way join vanilla spreads over
//! `Foods.java` (the `nutrition`/`saturationModifier`/`alwaysEdible` triple),
//! `Items.java` (which item carries which `Foods` constant, and which
//! `Consumables` constant overrides the default) and `Consumables.java` (the
//! `consumeSeconds`, default `1.6F`). No census in this repo carries the
//! `minecraft:food` or `minecraft:consumable` component — they are *prototype*
//! components, never on the wire — so the record definition is the source, and
//! it is small enough (40 items) to transcribe exactly.
//!
//! The equip half needs no transcription: `lodestone_data::item_prototypes`
//! already carries `Equippable.slot()` and `allowedEntities.isEmpty()` from a JVM
//! dump. It deliberately does **not** carry `swappable`, which is the one field
//! arm 2 is gated on, so [`UNSWAPPABLE`] names the nine items whose registration
//! sets it false — read straight off `Items.java`, and a set that small because
//! `Equippable.Builder`'s default is `true`.
//!
//! # How to change it
//!
//! Adding a food is one row in [`FOODS`]; the arithmetic is
//! `crate::food::FoodData::eat`'s and does not belong here. Adding arm 3 or 4
//! means finding a consumer for a held-use with no completion effect first.
//!
//! Not modelled, and each one is a real omission rather than an oversight:
//! `Consumable.onConsume`'s effect lists (a golden apple's regeneration,
//! rotten flesh's hunger, chorus fruit's teleport, milk's
//! `ClearAllStatusEffectsConsumeEffect`), `usingConvertsTo` (a stew leaving a
//! bowl, honey leaving a glass bottle), `useCooldown`, and potions — every one of
//! those needs `crate::mob_effects` or an item-conversion hook wired to a
//! *completion* callback, and the callback is what this landing creates.

use lodestone_model::{EquipmentSlot, ItemStack};

use crate::inventory::{
    CHEST_NATIVE, FEET_NATIVE, HEAD_NATIVE, LEGS_NATIVE, OFFHAND_NATIVE, PlayerInventory,
};

/// `minecraft:food` plus the `consumeSeconds` of the item's `minecraft:consumable`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Food {
    /// `FoodProperties.nutrition`.
    pub nutrition: i32,
    /// `FoodProperties.saturation`, which is the *modifier*
    /// (`FoodProperties.Builder.saturationModifier`) — the name in the record is
    /// `saturation` and the value is a modifier, which is exactly the trap
    /// `crate::food::FoodData::eat`'s own doc comment names.
    pub saturation_modifier: f32,
    /// `FoodProperties.canAlwaysEat` — a golden apple or honey may be eaten on a
    /// full bar.
    pub can_always_eat: bool,
    /// `Consumable.consumeTicks()`, which is `(int)(consumeSeconds * 20.0F)`.
    pub use_ticks: i32,
}

/// `Consumables.defaultFood()`/`defaultDrink()`'s shared `consumeSeconds(1.6F)`,
/// in ticks. Only three items in 26.2 differ, and each is spelled out in
/// [`FOODS`].
pub(crate) const DEFAULT_CONSUME_TICKS: i32 = 32;

/// The `minecraft:food` component of `item` (a full registry name), or `None`
/// when it is not food.
#[must_use]
pub(crate) fn food_for_item(item: &str) -> Option<Food> {
    FOODS
        .binary_search_by_key(&item, |&(name, _)| name)
        .ok()
        .map(|idx| FOODS[idx].1)
}

/// `Player.canEat(canAlwaysEat)` — `abilities.invulnerable || canAlwaysEat ||
/// foodData.needsFood()`, where `needsFood()` is `foodLevel < 20`.
///
/// This is the gate whose absence is very visible: without it a full player eats
/// steak after steak for nothing.
#[must_use]
pub(crate) fn can_eat(food: Food, food_level: i32, invulnerable: bool) -> bool {
    invulnerable || food.can_always_eat || food_level < crate::food::MAX_FOOD
}

/// The armour/off-hand slot `item` goes in, and the native inventory index that
/// slot is, or `None` when the item is not equippable, is not *swappable*, or
/// equips somewhere this crate has no slot for.
///
/// `Equippable.slot()` and `allowedEntities.isEmpty()` come from
/// `lodestone_data::item_prototypes`; `swappable` comes from [`UNSWAPPABLE`]
/// because that census does not carry it. `MAINHAND`/`BODY`/`SADDLE` return
/// `None`: `swapWithEquipmentSlot` into the main hand is a no-op and the other
/// two are not player slots.
#[must_use]
pub(crate) fn swappable_equip_slot(item: &str) -> Option<(EquipmentSlot, usize)> {
    if UNSWAPPABLE.contains(&item) {
        return None;
    }
    let prototype = lodestone_data::item_prototypes::prototype(item)?;
    // `Equippable.canBeEquippedBy(player.typeHolder())` — an empty
    // `allowedEntities` accepts everything, a non-empty one is a mob-only piece
    // (a horse's armour, a llama's carpet) and a player click must PASS.
    if !prototype.equippable_by_any_entity {
        return None;
    }
    let slot = prototype.equip_slot?;
    let native = match slot {
        EquipmentSlot::Head => HEAD_NATIVE,
        EquipmentSlot::Chest => CHEST_NATIVE,
        EquipmentSlot::Legs => LEGS_NATIVE,
        EquipmentSlot::Feet => FEET_NATIVE,
        EquipmentSlot::OffHand => OFFHAND_NATIVE,
        _ => return None,
    };
    Some((slot, native))
}

/// What an equip swap changed, so the caller can tell the client about exactly
/// those slots.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EquipSwap {
    /// The equipment slot's native index, and what is in it now.
    pub equipment: (usize, Option<ItemStack>),
    /// The hand slot's native index, and what is in it now.
    pub hand: (usize, Option<ItemStack>),
    /// Any other native slots the previously-equipped stack landed in (the
    /// `count > 1` branch puts it in the inventory rather than the hand).
    pub inventory: Vec<usize>,
    /// The previously-equipped stack when it fit nowhere —
    /// `player.drop(swappedToInventory, false)`. The caller pops it as an item
    /// entity.
    pub spilled: Option<ItemStack>,
}

/// `Equippable.swapWithEquipmentSlot(inHand, player)` for the stack in native
/// slot `hand_native`, or `None` when vanilla returns `FAIL`/`PASS` and nothing
/// moves.
///
/// # The count branch is the whole subtlety
///
/// `count <= 1`: the hand receives the *previously equipped* stack, or keeps its
/// own if the slot was empty, and creative **copies** the held stack into the
/// slot instead of clearing the hand
/// (`player.isCreative() ? inHand.copy() : inHand.copyAndClear()`).
///
/// `count > 1`: only **one** is taken from the hand
/// (`inHand.consumeAndReturn(1, player)`, which is itself creative-gated), the
/// hand keeps the rest, and the previously-equipped stack goes to the
/// **inventory** — or on the floor if it does not fit. A gate written against a
/// single helmet cannot see this branch at all.
///
/// The refusal that is easy to miss: `!ItemStack.isSameItemSameComponents(inHand,
/// inEquipmentSlot)`, so re-equipping the identical piece is a no-op rather than
/// a pointless swap. `EnchantmentHelper.has(…, PREVENT_ARMOR_CHANGE)` is the
/// other guard and there is no enchantment model here, so it cannot fire.
pub(crate) fn swap_with_equipment_slot(
    inventory: &mut PlayerInventory,
    hand_native: usize,
    creative: bool,
) -> Option<EquipSwap> {
    let in_hand = inventory.native(hand_native)?.clone();
    let (_, equipment_native) = swappable_equip_slot(&in_hand.item.to_string())?;
    let in_slot = inventory.native(equipment_native).cloned();
    // `ItemStack.isSameItemSameComponents`: this crate's `ItemStack` carries the
    // item and the count, so "same item" is the whole comparison it can make.
    if in_slot.as_ref().is_some_and(|equipped| {
        equipped.item == in_hand.item && equipped.components == in_hand.components
    }) {
        return None;
    }
    if in_hand.count <= 1 {
        let to_hand = in_slot.clone();
        let to_equipment = in_hand.clone();
        inventory.set_native(equipment_native, Some(to_equipment.clone()));
        // Creative copies rather than clearing, so the hand keeps what it had
        // *unless* the slot handed something back.
        let hand_now = if creative {
            to_hand.clone().or(Some(in_hand))
        } else {
            to_hand.clone()
        };
        inventory.set_native(hand_native, hand_now.clone());
        return Some(EquipSwap {
            equipment: (equipment_native, Some(to_equipment)),
            hand: (hand_native, hand_now),
            inventory: Vec::new(),
            spilled: None,
        });
    }
    // `count > 1`.
    let mut one = in_hand.clone();
    one.count = 1;
    inventory.set_native(equipment_native, Some(one.clone()));
    let hand_now = if creative {
        Some(in_hand.clone())
    } else {
        let mut rest = in_hand.clone();
        rest.count -= 1;
        Some(rest)
    };
    inventory.set_native(hand_native, hand_now.clone());
    let (touched, spilled) = match in_slot {
        Some(previous) => inventory.add(previous),
        None => (Vec::new(), None),
    };
    Some(EquipSwap {
        equipment: (equipment_native, Some(one)),
        hand: (hand_native, hand_now),
        inventory: touched,
        spilled,
    })
}

/// The nine items whose registration sets `Equippable.swappable` to `false`, so
/// `Item.use` falls straight past arm 2 for them.
///
/// Read off `Items.java`: `CARVED_PUMPKIN`'s explicit
/// `Equippable.builder(HEAD).setSwappable(false)`, the seven
/// `equippableUnswappable(EquipmentSlot.HEAD)` mob heads, and `SHIELD`'s
/// `equippableUnswappable(EquipmentSlot.OFFHAND)`. Sorted, for
/// [`slice::contains`]'s sake nothing — it is nine entries and a linear scan.
static UNSWAPPABLE: &[&str] = &[
    "minecraft:carved_pumpkin",
    "minecraft:creeper_head",
    "minecraft:dragon_head",
    "minecraft:piglin_head",
    "minecraft:player_head",
    "minecraft:shield",
    "minecraft:skeleton_skull",
    "minecraft:wither_skeleton_skull",
    "minecraft:zombie_head",
];

/// Every food item in 26.2, sorted by registry name.
///
/// The triple comes from `Foods.java`, the item→constant join from `Items.java`,
/// and `use_ticks` from `Consumables.java` — `DEFAULT_CONSUME_TICKS` unless the
/// item's registration names a `Consumables` constant that overrides
/// `consumeSeconds`, which in 26.2 is only `HONEY_BOTTLE` (`2.0F`) and
/// `DRIED_KELP` (`0.8F`).
///
/// `Foods.stew(n)` expands to `nutrition(n).saturationModifier(0.6F)`, which is
/// why the four stews all carry `0.6`.
static FOODS: &[(&str, Food)] = &[
    ("minecraft:apple", Food { nutrition: 4, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:baked_potato", Food { nutrition: 5, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:beef", Food { nutrition: 3, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:beetroot", Food { nutrition: 1, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:beetroot_soup", Food { nutrition: 6, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:bread", Food { nutrition: 5, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:carrot", Food { nutrition: 3, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:chicken", Food { nutrition: 2, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:chorus_fruit", Food { nutrition: 4, saturation_modifier: 0.3, can_always_eat: true, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cod", Food { nutrition: 2, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_beef", Food { nutrition: 8, saturation_modifier: 0.8, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_chicken", Food { nutrition: 6, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_cod", Food { nutrition: 5, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_mutton", Food { nutrition: 6, saturation_modifier: 0.8, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_porkchop", Food { nutrition: 8, saturation_modifier: 0.8, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_rabbit", Food { nutrition: 5, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cooked_salmon", Food { nutrition: 6, saturation_modifier: 0.8, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:cookie", Food { nutrition: 2, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:dried_kelp", Food { nutrition: 1, saturation_modifier: 0.3, can_always_eat: false, use_ticks: 16 }),
    ("minecraft:enchanted_golden_apple", Food { nutrition: 4, saturation_modifier: 1.2, can_always_eat: true, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:glow_berries", Food { nutrition: 2, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:golden_apple", Food { nutrition: 4, saturation_modifier: 1.2, can_always_eat: true, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:golden_carrot", Food { nutrition: 6, saturation_modifier: 1.2, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:honey_bottle", Food { nutrition: 6, saturation_modifier: 0.1, can_always_eat: true, use_ticks: 40 }),
    ("minecraft:melon_slice", Food { nutrition: 2, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:mushroom_stew", Food { nutrition: 6, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:mutton", Food { nutrition: 2, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:poisonous_potato", Food { nutrition: 2, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:porkchop", Food { nutrition: 3, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:potato", Food { nutrition: 1, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:pufferfish", Food { nutrition: 1, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:pumpkin_pie", Food { nutrition: 8, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:rabbit", Food { nutrition: 3, saturation_modifier: 0.3, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:rabbit_stew", Food { nutrition: 10, saturation_modifier: 0.6, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:rotten_flesh", Food { nutrition: 4, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:salmon", Food { nutrition: 2, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:spider_eye", Food { nutrition: 2, saturation_modifier: 0.8, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:suspicious_stew", Food { nutrition: 6, saturation_modifier: 0.6, can_always_eat: true, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:sweet_berries", Food { nutrition: 2, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
    ("minecraft:tropical_fish", Food { nutrition: 1, saturation_modifier: 0.1, can_always_eat: false, use_ticks: DEFAULT_CONSUME_TICKS }),
];

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::ResourceKey;

    fn stack(item: &str, count: u32) -> ItemStack {
        let (namespace, path) = item.split_once(':').expect("a namespaced item name");
        ItemStack::new(
            ResourceKey::new(namespace, path).expect("a legal registry name"),
            count,
        )
    }

    /// [`food_for_item`] binary-searches; an unsorted table answers `None` for
    /// real foods, silently and only for some of them.
    #[test]
    fn the_food_table_is_sorted_and_every_name_is_a_real_item() {
        let mut offenders = Vec::new();
        for pair in FOODS.windows(2) {
            if pair[0].0 >= pair[1].0 {
                offenders.push((pair[0].0, pair[1].0));
            }
        }
        for &(name, _) in FOODS {
            if lodestone_data::items::item_id(name).is_none() {
                offenders.push((name, "not an item in the 26.2 registry"));
            }
        }
        assert!(offenders.is_empty(), "{offenders:#?}");
        // `Items.java` carries exactly 40 `.food(` registrations in 26.2.
        assert_eq!(FOODS.len(), 40, "the join lost or gained a food");
    }

    /// The values, against `Foods.java` read directly. Chosen so a
    /// transposition of `nutrition` and `saturation_modifier` cannot survive:
    /// every pair here is distinct, and cooked beef's 8/0.8 is deliberately
    /// *not* one of them — a 1.2 modifier next to a nutrition of 4 is.
    #[test]
    fn the_transcription_matches_foods_java() {
        let mut wrong = Vec::new();
        for (item, nutrition, modifier, always) in [
            ("minecraft:apple", 4, 0.3f32, false),
            ("minecraft:golden_apple", 4, 1.2, true),
            ("minecraft:golden_carrot", 6, 1.2, false),
            ("minecraft:rabbit_stew", 10, 0.6, false),
            ("minecraft:honey_bottle", 6, 0.1, true),
            ("minecraft:spider_eye", 2, 0.8, false),
            ("minecraft:dried_kelp", 1, 0.3, false),
        ] {
            let Some(food) = food_for_item(item) else {
                wrong.push(format!("{item}: absent"));
                continue;
            };
            if food.nutrition != nutrition
                || (food.saturation_modifier - modifier).abs() > f32::EPSILON
                || food.can_always_eat != always
            {
                wrong.push(format!("{item}: {food:?} != ({nutrition}, {modifier}, {always})"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The three items whose `consumeSeconds` is not the 1.6 s default, and one
    /// that is — the fourth arm is the control that makes the other three mean
    /// something.
    #[test]
    fn consume_ticks_follow_the_consumables_overrides() {
        let mut wrong = Vec::new();
        for (item, ticks) in [
            ("minecraft:honey_bottle", 40),
            ("minecraft:dried_kelp", 16),
            ("minecraft:apple", 32),
            ("minecraft:cooked_beef", 32),
        ] {
            let got = food_for_item(item).map(|food| food.use_ticks);
            if got != Some(ticks) {
                wrong.push(format!("{item}: {got:?} != {ticks}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// `Player.canEat`'s three disjuncts, each shown to be the deciding one.
    #[test]
    fn can_eat_refuses_ordinary_food_on_a_full_bar_only() {
        let steak = food_for_item("minecraft:cooked_beef").unwrap();
        let golden = food_for_item("minecraft:golden_apple").unwrap();
        // 19 rather than 0: an empty bar cannot tell "the gate passed" from
        // "there is no gate", and a partial bar is the discriminating input.
        assert!(can_eat(steak, 19, false));
        assert!(!can_eat(steak, crate::food::MAX_FOOD, false));
        assert!(
            can_eat(golden, crate::food::MAX_FOOD, false),
            "can_always_eat is the exception"
        );
        assert!(
            can_eat(steak, crate::food::MAX_FOOD, true),
            "an invulnerable (creative) player always can"
        );
    }

    /// `swappable_equip_slot`, both answers. The `shield` arm is the one that
    /// separates "reads the census" from "reads the census *and* the swappable
    /// set" — a shield has a real `Equippable` naming the off-hand.
    #[test]
    fn only_swappable_equippables_resolve_to_a_slot() {
        let mut wrong = Vec::new();
        for (item, want) in [
            ("minecraft:diamond_helmet", Some(HEAD_NATIVE)),
            ("minecraft:iron_chestplate", Some(CHEST_NATIVE)),
            ("minecraft:leather_boots", Some(FEET_NATIVE)),
            ("minecraft:golden_leggings", Some(LEGS_NATIVE)),
            ("minecraft:elytra", Some(CHEST_NATIVE)),
            ("minecraft:shield", None),
            ("minecraft:carved_pumpkin", None),
            ("minecraft:skeleton_skull", None),
            ("minecraft:cooked_beef", None),
            ("minecraft:stone", None),
        ] {
            let got = swappable_equip_slot(item).map(|(_, native)| native);
            if got != want {
                wrong.push(format!("{item}: {got:?} != {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A stack of one into an **empty** slot: the hand ends up empty, because
    /// the slot handed nothing back. This is the case where "swap" degenerates
    /// into "equip" and a wrong implementation still looks right, so it is
    /// asserted separately from the occupied case below.
    #[test]
    fn a_single_helmet_into_an_empty_head_slot_leaves_the_hand_empty() {
        let mut inv = PlayerInventory::default();
        inv.set_native(0, Some(stack("minecraft:diamond_helmet", 1)));
        let swap = swap_with_equipment_slot(&mut inv, 0, false).expect("the swap must happen");
        assert_eq!(
            inv.native(HEAD_NATIVE).map(|s| s.item.to_string()),
            Some("minecraft:diamond_helmet".to_string())
        );
        assert!(inv.native(0).is_none(), "the hand must be empty");
        assert_eq!(swap.hand, (0, None));
        assert!(swap.spilled.is_none());
    }

    /// A stack of one into an **occupied** slot: the two trade places.
    #[test]
    fn a_single_helmet_swaps_with_the_one_already_worn() {
        let mut inv = PlayerInventory::default();
        inv.set_native(0, Some(stack("minecraft:diamond_helmet", 1)));
        inv.set_native(HEAD_NATIVE, Some(stack("minecraft:iron_helmet", 1)));
        swap_with_equipment_slot(&mut inv, 0, false).expect("the swap must happen");
        assert_eq!(
            inv.native(HEAD_NATIVE).map(|s| s.item.to_string()),
            Some("minecraft:diamond_helmet".to_string())
        );
        assert_eq!(
            inv.native(0).map(|s| s.item.to_string()),
            Some("minecraft:iron_helmet".to_string()),
            "the previously worn helmet goes to the hand"
        );
    }

    /// The `count > 1` branch, which no single-helmet gate can reach: one is
    /// consumed, the hand keeps the rest, and the previously-worn piece goes to
    /// the **inventory** rather than to the hand.
    #[test]
    fn a_stack_of_helmets_equips_one_and_banks_the_old_one_in_the_inventory() {
        let mut inv = PlayerInventory::default();
        inv.set_native(0, Some(stack("minecraft:diamond_helmet", 5)));
        inv.set_native(HEAD_NATIVE, Some(stack("minecraft:iron_helmet", 1)));
        let swap = swap_with_equipment_slot(&mut inv, 0, false).expect("the swap must happen");
        let mut wrong = Vec::new();
        if inv.native(HEAD_NATIVE).map(|s| (s.item.to_string(), s.count))
            != Some(("minecraft:diamond_helmet".to_string(), 1))
        {
            wrong.push(format!("head slot: {:?}", inv.native(HEAD_NATIVE)));
        }
        if inv.native(0).map(|s| (s.item.to_string(), s.count))
            != Some(("minecraft:diamond_helmet".to_string(), 4))
        {
            wrong.push(format!("hand: {:?}", inv.native(0)));
        }
        if swap.inventory.is_empty() {
            wrong.push("the old helmet must have landed in a real slot".to_string());
        }
        let iron_somewhere = (0..crate::inventory::PLAYER_NATIVE_SIZE).any(|native| {
            inv.native(native)
                .is_some_and(|s| s.item.to_string() == "minecraft:iron_helmet")
        });
        if !iron_somewhere {
            wrong.push("the old helmet vanished".to_string());
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Creative copies rather than consuming, in both count branches.
    #[test]
    fn creative_keeps_the_held_stack() {
        let mut single = PlayerInventory::default();
        single.set_native(0, Some(stack("minecraft:diamond_helmet", 1)));
        swap_with_equipment_slot(&mut single, 0, true).expect("the swap must happen");
        assert_eq!(
            single.native(0).map(|s| s.count),
            Some(1),
            "creative keeps the single helmet in hand"
        );
        let mut many = PlayerInventory::default();
        many.set_native(0, Some(stack("minecraft:diamond_helmet", 5)));
        swap_with_equipment_slot(&mut many, 0, true).expect("the swap must happen");
        assert_eq!(
            many.native(0).map(|s| s.count),
            Some(5),
            "creative consumes none of the stack"
        );
    }

    /// `!ItemStack.isSameItemSameComponents(inHand, inEquipmentSlot)` — the
    /// no-op refusal. Without it, right-clicking the helmet you are already
    /// wearing shuffles it pointlessly.
    #[test]
    fn re_equipping_the_identical_piece_does_nothing() {
        let mut inv = PlayerInventory::default();
        inv.set_native(0, Some(stack("minecraft:diamond_helmet", 1)));
        inv.set_native(HEAD_NATIVE, Some(stack("minecraft:diamond_helmet", 1)));
        assert!(swap_with_equipment_slot(&mut inv, 0, false).is_none());
        assert_eq!(inv.native(0).map(|s| s.count), Some(1));
    }
}
