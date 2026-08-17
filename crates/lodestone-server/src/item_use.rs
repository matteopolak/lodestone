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
//! **`Consumable.onConsume`'s effect lists are now modelled** (issue #690) —
//! `crate::server`'s `finish_drinking_potion`/`finish_drinking_milk` plus the
//! `food_consume_effects`/`removes_poison_on_consume` grants
//! `crate::mob_effects` carries, wired into the same `finish_tick` callback this
//! module's landing created. A potion applies its full unscaled built-in effect
//! list, milk clears every active effect, and golden apple/pufferfish/rotten
//! flesh/spider eye/poisonous potato/chicken/honey bottle grant or remove the
//! effects `Consumables.java` names for them. `chorus_fruit`'s
//! `TeleportRandomlyConsumeEffect` is **not** among these — teleport-on-eat is a
//! movement mechanic, not a status effect, and stays unmodelled here.
//!
//! Still not modelled, and each one is a real omission rather than an
//! oversight: `usingConvertsTo` (a stew leaving a bowl, honey leaving a glass
//! bottle — milk consumes the whole bucket rather than leaving an empty one,
//! see `finish_drinking_milk`'s own doc), `useCooldown`, chorus fruit's
//! teleport, and — a mechanism gap wider than this module —
//! `minecraft:resistance`'s damage reduction and `minecraft:absorption`'s
//! extra hit points are granted but never consumed by the player damage
//! pipeline (`crate::mob_effects::FOOD_EFFECTS`'s own doc has the detail).

use lodestone_model::{EquipmentSlot, ItemStack, Vec3};

use crate::inventory::{
    CHEST_NATIVE, FEET_NATIVE, HEAD_NATIVE, HOTBAR_SIZE, LEGS_NATIVE, OFFHAND_NATIVE,
    PlayerInventory,
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
/// in ticks. Only **two** items in 26.2 differ — `DRIED_KELP` (`0.8F`) and
/// `HONEY_BOTTLE` (`2.0F`) — and each is spelled out in [`FOODS`], which says the
/// same thing correctly. This line said "three" and disagreed with its own table.
///
/// The same number is also `lodestone_game::consumable::DEFAULT_CONSUME_TICKS`,
/// which is the client-visible half of the component (the animation, the particle
/// cadence, the sound). The two tables are deliberately separate — this one is
/// `minecraft:food` and that one is `minecraft:consumable`, and the sets differ:
/// `milk_bucket`, `potion` and `ominous_bottle` are drinkable and are not food.
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

// ---------------------------------------------------------------------------
// Pick-block / pick-entity (middle-click, issue #558)
// ---------------------------------------------------------------------------
//
// `ServerGamePacketListenerImpl::handlePickItemFromBlock`/
// `handlePickItemFromEntity` resolve **what** to pick (a block's clone-item
// stack, or an entity's `getPickResult()`), then both funnel into the same
// `tryPickItem`, which is vanilla's three-way split: already in the hotbar ->
// select it; elsewhere in the inventory -> swap it into a hotbar slot;
// nowhere at all and creative -> mint it. `crate::server`'s consumer resolves
// the "what" (it alone has the world/mob lookups) and calls
// [`try_pick_item`] for the "where it goes" — the same split this module's
// `swap_with_equipment_slot` already keeps as a pure function over
// [`PlayerInventory`].
//
// Vanilla's client does **no** local prediction here: `Minecraft
// ::pickBlockOrEntity` unconditionally forwards to
// `MultiPlayerGameMode::handlePickItemFromBlock`/`handlePickItemFromEntity`,
// which do nothing but send the packet — the whole decision lives in
// `tryPickItem`, server-side. So there is no client-side hotbar-prediction
// gap to close here: the existing `ClientboundSetHeldSlotPacket` round trip
// (this crate's client already decodes `SET_HELD_SLOT` into
// `ClientEvent::HeldSlotChanged`) is exactly vanilla's own latency, not a
// missing optimisation.

/// The item [`try_pick_item`] should receive for a middle-click on the block
/// whose canonical state string is `block_state` (bare `"minecraft:stone"` or
/// with properties, `"minecraft:oak_stairs[facing=east,...]"` — only the base
/// name matters). This is `BlockState.getCloneItemStack`'s **default** arm
/// (`new ItemStack(this.asItem())`, `BlockBehaviour.java`); see
/// [`lodestone_data::block_items::item_for_block`]'s own doc comment for the
/// per-block `getCloneItemStack` overrides (crops, flower pots, banners,
/// beehives, ...) this does not model.
///
/// `None` for a state naming no built-in block and for a block with no
/// registered `BlockItem` at all (air, fluids, redstone wire, portal
/// blocks, ...) — vanilla's own `tryPickItem` no-ops identically on an empty
/// stack.
#[must_use]
pub(crate) fn clone_item_stack_for_block(block_state: &str) -> Option<ItemStack> {
    let name = block_state.split('[').next().unwrap_or(block_state);
    let block = lodestone_data::block::Block::from_name(name)?;
    let item = lodestone_data::block_items::item_for_block(block)?;
    Some(ItemStack::new(item.name().parse().ok()?, 1))
}

/// The item [`try_pick_item`] should receive for a middle-click on an entity
/// of type `entity_type` (a full registry name, `"minecraft:sheep"`) — the
/// only modelled arm of `Entity.getPickResult()`: `Mob.getPickResult()`'s own
/// `SpawnEggItem.byId(this.getType())`, a mob's spawn egg.
///
/// Derived by name exactly as [`crate::spawn_egg::entity_type_for_egg`]
/// derives the reverse (`{entity path}` <-> `{entity path}_spawn_egg`), and
/// checked against the real item registry the same way — see that module's
/// own doc comment for why the derivation is exact rather than a guess (88
/// registrations, zero mismatches against the pinned 26.2 decompile).
///
/// `None` for every entity whose `getPickResult` also returns `null` by
/// vanilla's own default (arrows, most projectiles, item entities, XP
/// orbs, the player) and for the handful of non-`Mob` overrides this crate
/// does not model (minecarts, boats, item frames, paintings, end crystals,
/// leash knots, armour stands) — each returns a *different* item than a
/// spawn egg, so folding them into this function would silently hand back
/// the wrong stack rather than none.
#[must_use]
pub(crate) fn spawn_egg_for_entity_type(entity_type: &str) -> Option<ItemStack> {
    let (namespace, path) = entity_type.split_once(':')?;
    let egg = format!("{namespace}:{path}_spawn_egg");
    lodestone_data::items::item_id(&egg)?;
    Some(ItemStack::new(egg.parse().ok()?, 1))
}

/// `Player.isWithinEntityInteractionRange(entity, extraDistance)`, flattened
/// the same way [`crate::block_breaking::within_interaction_range`] flattens
/// the block version: a single generous centre-to-eye radius rather than
/// vanilla's per-attribute `ENTITY_INTERACTION_RANGE` (`3.0` base, `+2.0`
/// creative) plus this packet's own `+3.0` tolerance. `None` feet permits
/// unconditionally, matching that same function's "no data yet, don't guess".
#[must_use]
pub(crate) fn within_entity_pick_range(feet: Option<Vec3>, entity_pos: Vec3) -> bool {
    /// `3.0` base + `2.0` creative headroom + `3.0` packet tolerance,
    /// flattened into one flat radius — see this function's own doc comment
    /// for why a single constant stands in for vanilla's per-mode attribute.
    const MAX_ENTITY_PICK_DISTANCE: f64 = 8.0;
    let Some(feet) = feet else { return true };
    let eye = Vec3::new(feet.x, feet.y + crate::vitals::EYE_HEIGHT, feet.z);
    let (dx, dy, dz) = (entity_pos.x - eye.x, entity_pos.y - eye.y, entity_pos.z - eye.z);
    dx * dx + dy * dy + dz * dz <= MAX_ENTITY_PICK_DISTANCE * MAX_ENTITY_PICK_DISTANCE
}

/// What a pick changed, for the caller to echo back to the client —
/// `tryPickItem`'s own two effects (`ClientboundSetHeldSlotPacket` plus
/// `broadcastChanges`), reduced to exactly the natives this crate's model
/// touched rather than a diff of the whole menu.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickOutcome {
    /// The hotbar slot selected after the pick. Always populated — vanilla's
    /// own `tryPickItem` sends `ClientboundSetHeldSlotPacket` unconditionally,
    /// whether or not anything actually moved.
    pub selected: u8,
    /// Every native slot whose contents changed, in the order they were
    /// written. Empty for the "already in the hotbar" case (only the
    /// selection moved); two entries for a swap or an overflowed create.
    pub changed: Vec<usize>,
}

/// Native slots vanilla's `Inventory.items` covers: hotbar (`0..=8`) plus
/// main storage (`9..=35`). Restated from `crate::inventory`'s private
/// `ITEMS_SIZE` rather than imported — `inventory.rs` is a heavily contended
/// shared file and this is the only pick-item consumer of the boundary,
/// which is `Inventory.INVENTORY_SIZE` in vanilla and has not changed since
/// Minecraft added the off-hand slot.
const PICK_SCAN_SIZE: usize = 36;

/// `Inventory.findSlotMatchingItem` — the first native slot in hotbar-then-
/// main-storage order (`0..36`; armour and the off-hand are excluded,
/// matching vanilla's `items` list) holding a stack of the same item and
/// components as `stack`.
fn find_slot_matching_item(inventory: &PlayerInventory, stack: &ItemStack) -> Option<usize> {
    (0..PICK_SCAN_SIZE).find(|&native| {
        inventory
            .native(native)
            .is_some_and(|slot| slot.item == stack.item && slot.components == stack.components)
    })
}

/// `Inventory.getSuitableHotbarSlot` — the first **empty** hotbar slot
/// starting at the currently selected one and wrapping. This crate has no
/// enchantment model, so vanilla's second pass ("first *unenchanted* slot")
/// always accepts its very first candidate — the currently selected slot —
/// which is exactly vanilla's own final fallback, so the two collapse into
/// one `else`.
fn suitable_hotbar_slot(inventory: &PlayerInventory) -> u8 {
    let selected = inventory.selected_hotbar_slot();
    for offset in 0..HOTBAR_SIZE {
        let index = (selected + offset) % HOTBAR_SIZE;
        if inventory.native(usize::from(index)).is_none() {
            return index;
        }
    }
    selected
}

/// `ServerGamePacketListenerImpl::tryPickItem`'s three-way split (issue
/// #558): `stack` is the resolved clone-item-stack or spawn egg (`None`
/// upstream is "nothing to pick" and never reaches here — an empty
/// `itemStack.isEmpty()` in vanilla); `creative` is
/// `player.hasInfiniteMaterials()`.
///
/// 1. **Already in the hotbar** (`Inventory.isHotbarSlot`) -> just move the
///    selection there.
/// 2. **Elsewhere in the inventory** -> swap it into a
///    [`suitable_hotbar_slot`] (`Inventory.pickSlot`).
/// 3. **Not held at all, creative only** -> mint it into a suitable hotbar
///    slot, banking whatever was displaced into the first free slot
///    (`Inventory.addAndPickItem`). Survival falls all the way through and
///    changes nothing but still reports the current selection, matching
///    vanilla's own unconditional `ClientboundSetHeldSlotPacket` send.
///
/// `isItemEnabled` (experimental-feature items) has no model in this crate
/// and is not checked.
pub(crate) fn try_pick_item(
    inventory: &mut PlayerInventory,
    stack: ItemStack,
    creative: bool,
) -> PickOutcome {
    let mut changed = Vec::new();
    match find_slot_matching_item(inventory, &stack) {
        Some(native) if native < usize::from(HOTBAR_SIZE) => {
            let slot = u8::try_from(native).expect("native < HOTBAR_SIZE always fits u8");
            inventory.set_selected_hotbar_slot(slot);
        }
        Some(native) => {
            let suitable = suitable_hotbar_slot(inventory);
            inventory.set_selected_hotbar_slot(suitable);
            let suitable_native = usize::from(suitable);
            let displaced = inventory.native(suitable_native).cloned();
            let picked = inventory.native(native).cloned();
            inventory.set_native(suitable_native, picked);
            inventory.set_native(native, displaced);
            changed.push(suitable_native);
            changed.push(native);
        }
        None if creative => {
            let suitable = suitable_hotbar_slot(inventory);
            inventory.set_selected_hotbar_slot(suitable);
            let suitable_native = usize::from(suitable);
            if let Some(displaced) = inventory.native(suitable_native).cloned() {
                // `Inventory.getFreeSlot()`: the first empty slot in `0..36`.
                // Vanilla drops the displaced stack on the floor when there is
                // none; this crate has no command-less world-drop path for
                // that corner (the same scope cut `swap_with_equipment_slot`'s
                // `spilled` field documents above), so a full inventory simply
                // overwrites the displaced stack rather than banking it — lossy
                // only in that one already-rare corner.
                if let Some(free) = (0..PICK_SCAN_SIZE).find(|&native| inventory.native(native).is_none()) {
                    inventory.set_native(free, Some(displaced));
                    changed.push(free);
                }
            }
            inventory.set_native(suitable_native, Some(stack));
            changed.push(suitable_native);
        }
        None => {}
    }
    PickOutcome {
        selected: inventory.selected_hotbar_slot(),
        changed,
    }
}

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

    // -----------------------------------------------------------------------
    // Pick-block / pick-entity (issue #558)
    // -----------------------------------------------------------------------

    /// [`clone_item_stack_for_block`]'s default arm: an ordinary block clones
    /// to itself, a state with properties still resolves by its base name,
    /// and a block with no `BlockItem` (water) or no such block at all is
    /// `None`.
    #[test]
    fn clone_item_stack_for_block_resolves_the_default_arm() {
        assert_eq!(
            clone_item_stack_for_block("minecraft:dirt").map(|s| s.item.to_string()),
            Some("minecraft:dirt".to_string())
        );
        assert_eq!(
            clone_item_stack_for_block("minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]")
                .map(|s| s.item.to_string()),
            Some("minecraft:oak_stairs".to_string()),
            "properties must be stripped before the name lookup"
        );
        assert_eq!(clone_item_stack_for_block("minecraft:water"), None);
        assert_eq!(clone_item_stack_for_block("minecraft:not_a_real_block"), None);
    }

    /// [`spawn_egg_for_entity_type`]: a real mob resolves to its real spawn
    /// egg item, and an entity type with no spawn egg (the player) or no
    /// such type at all is `None`.
    #[test]
    fn spawn_egg_for_entity_type_resolves_real_mobs_only() {
        assert_eq!(
            spawn_egg_for_entity_type("minecraft:sheep").map(|s| s.item.to_string()),
            Some("minecraft:sheep_spawn_egg".to_string())
        );
        assert_eq!(
            spawn_egg_for_entity_type("minecraft:zombie_villager").map(|s| s.item.to_string()),
            Some("minecraft:zombie_villager_spawn_egg".to_string())
        );
        assert_eq!(spawn_egg_for_entity_type("minecraft:player"), None);
        assert_eq!(spawn_egg_for_entity_type("minecraft:not_a_real_entity"), None);
    }

    /// Fixture 1 (of the three the pick-block three behaviours need):
    /// **already in the hotbar** — a middle-click on a block whose item is
    /// already carried in a hotbar slot must only move the selection there.
    /// No native slot's *contents* change; a bug that instead swapped or
    /// re-minted the item would show up as a non-empty `changed`.
    ///
    /// Pairwise-distinct slots throughout this file's pick tests
    /// (`2`, `5`, `12`, `31`, ...) so a transposition of "the matched slot"
    /// and "the selected slot" cannot survive unnoticed.
    #[test]
    fn already_in_the_hotbar_only_moves_the_selection() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(5));
        inv.set_native(2, Some(stack("minecraft:diamond_pickaxe", 1)));
        let before = inv.native(2).cloned();

        let outcome = try_pick_item(&mut inv, stack("minecraft:diamond_pickaxe", 1), false);

        assert_eq!(outcome.selected, 2, "selection must move to the hotbar slot that already held it");
        assert!(outcome.changed.is_empty(), "no slot's contents may change: {:?}", outcome.changed);
        assert_eq!(inv.native(2), before.as_ref(), "the hotbar slot's own stack must be untouched");
        assert_eq!(inv.selected_hotbar_slot(), 2);
    }

    /// Fixture 2: **elsewhere in the inventory, not the hotbar** — the item
    /// must swap into a suitable (empty) hotbar slot, and the two touched
    /// natives must be reported so the caller can echo both slots. This is
    /// the case a fixture that only ever tests "already in the hotbar" (or
    /// only ever tests the creative-create arm) cannot exercise: the matched
    /// native here (`12`) is deliberately in main storage, distinct from
    /// every hotbar index and from the selected slot below (`0`).
    #[test]
    fn elsewhere_in_the_inventory_swaps_into_the_hotbar() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(0)); // hotbar slot 0 starts empty
        inv.set_native(12, Some(stack("minecraft:golden_carrot", 3)));

        let outcome = try_pick_item(&mut inv, stack("minecraft:golden_carrot", 3), false);

        assert_eq!(outcome.selected, 0, "the empty hotbar slot 0 is the suitable one");
        let mut changed = outcome.changed.clone();
        changed.sort_unstable();
        assert_eq!(changed, vec![0, 12], "both the hotbar slot and the source slot must be reported");
        assert_eq!(
            inv.native(0).map(|s| s.item.to_string()),
            Some("minecraft:golden_carrot".to_string()),
            "the picked item must land in the hotbar"
        );
        assert!(inv.native(12).is_none(), "the source slot must be left empty, not duplicated");
    }

    /// The same "elsewhere in the inventory" case, but the suitable hotbar
    /// slot is **occupied** — the two stacks must trade places rather than
    /// one silently overwriting or dropping the other. A gate using only an
    /// empty destination (the fixture above) cannot see a swap implemented as
    /// a plain overwrite.
    #[test]
    fn swapping_into_an_occupied_hotbar_slot_trades_both_stacks() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(4));
        // Every hotbar slot filled, so `suitable_hotbar_slot`'s "first empty,
        // wrapping" pass finds nothing and falls back to the selected slot
        // itself — the only way to force the destination to be occupied,
        // since a fixture with any other hotbar slot free would land there
        // instead (as the first version of this test discovered: it filled
        // only slot 4 and the pick silently landed in slot 5).
        for native in 0..usize::from(HOTBAR_SIZE) {
            inv.set_native(native, Some(stack("minecraft:stick", 1)));
        }
        inv.set_native(4, Some(stack("minecraft:torch", 16)));
        inv.set_native(19, Some(stack("minecraft:golden_carrot", 3)));

        let outcome = try_pick_item(&mut inv, stack("minecraft:golden_carrot", 3), false);

        assert_eq!(outcome.selected, 4);
        assert_eq!(
            inv.native(4).map(|s| s.item.to_string()),
            Some("minecraft:golden_carrot".to_string())
        );
        assert_eq!(
            inv.native(19).map(|s| s.item.to_string()),
            Some("minecraft:torch".to_string()),
            "the displaced torch must land where the carrot came from, not vanish"
        );
    }

    /// Fixture 3: **not held anywhere, creative** — the item is minted into a
    /// suitable hotbar slot. Distinct from fixtures 1 and 2 by construction:
    /// this is the one input where `find_slot_matching_item` returns `None`,
    /// which fixtures 1/2 cannot exercise (they require a match) and this one
    /// cannot exercise the swap behaviour (there is nothing to swap) — the
    /// "two mutually exclusive claims need two gates" shape.
    #[test]
    fn a_creative_miss_mints_the_item_into_a_suitable_hotbar_slot() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(7));

        let outcome = try_pick_item(&mut inv, stack("minecraft:command_block", 1), true);

        assert_eq!(outcome.selected, 7, "hotbar slot 7 was empty and is the suitable one");
        assert_eq!(outcome.changed, vec![7]);
        assert_eq!(
            inv.native(7).map(|s| s.item.to_string()),
            Some("minecraft:command_block".to_string())
        );
    }

    /// The creative-create arm's displaced-stack case: the suitable hotbar
    /// slot is occupied, so the old stack must be banked into the first free
    /// slot rather than deleted — a naive "just overwrite" implementation
    /// passes the plain-mint fixture above but loses an item here.
    #[test]
    fn a_creative_miss_banks_the_displaced_stack_rather_than_deleting_it() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(3));
        // As in the swap fixture above: the whole hotbar must be full, or
        // `suitable_hotbar_slot` lands on some other empty slot instead of
        // the occupied one this test means to exercise.
        for native in 0..usize::from(HOTBAR_SIZE) {
            inv.set_native(native, Some(stack("minecraft:stick", 1)));
        }
        inv.set_native(3, Some(stack("minecraft:torch", 4)));

        let outcome = try_pick_item(&mut inv, stack("minecraft:command_block", 1), true);

        assert_eq!(outcome.selected, 3);
        assert_eq!(
            inv.native(3).map(|s| s.item.to_string()),
            Some("minecraft:command_block".to_string())
        );
        let banked = (0..crate::inventory::PLAYER_NATIVE_SIZE)
            .find(|&native| native != 3 && inv.native(native).is_some_and(|s| s.item.to_string() == "minecraft:torch"));
        assert!(banked.is_some(), "the displaced torch must land in some other slot, not vanish");
    }

    /// Survival's miss arm: vanilla does nothing but still reports the
    /// current selection (the caller always sends `SET_HELD_SLOT`). The
    /// control for fixture 3 above — same input, `creative: false` — must
    /// answer differently, or the creative gate is not actually being
    /// checked.
    #[test]
    fn a_survival_miss_changes_nothing_but_still_reports_the_selection() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(6));

        let outcome = try_pick_item(&mut inv, stack("minecraft:command_block", 1), false);

        assert_eq!(outcome.selected, 6);
        assert!(outcome.changed.is_empty());
        assert!(inv.native(6).is_none(), "survival must not mint anything");
    }

    /// [`suitable_hotbar_slot`] wraps forward from the selected slot rather
    /// than always starting at `0` — a fixed-start implementation would pass
    /// every fixture above (all of which happen to have low-index empty
    /// slots) and only fail here.
    #[test]
    fn suitable_hotbar_slot_wraps_forward_from_the_selected_one() {
        let mut inv = PlayerInventory::default();
        assert!(inv.set_selected_hotbar_slot(7));
        for native in 0..9usize {
            if native != 8 {
                inv.set_native(native, Some(stack("minecraft:torch", 1)));
            }
        }
        // Every hotbar slot but 8 is full; selection starts at 7.
        assert_eq!(suitable_hotbar_slot(&inv), 8, "must wrap past 7 to find the one empty slot at 8");
    }

    /// [`within_entity_pick_range`]: a target well inside the flattened
    /// radius passes, one far outside refuses, and `None` feet (no position
    /// report yet) never blocks a pick — mirrors
    /// `block_breaking::within_interaction_range`'s own "no data yet, don't
    /// guess" control.
    #[test]
    fn within_entity_pick_range_gates_on_distance_and_permits_with_no_feet() {
        let feet = Vec3::new(0.0, 64.0, 0.0);
        assert!(within_entity_pick_range(Some(feet), Vec3::new(1.0, 64.0, 1.0)));
        assert!(!within_entity_pick_range(Some(feet), Vec3::new(100.0, 64.0, 100.0)));
        assert!(within_entity_pick_range(None, Vec3::new(9999.0, 9999.0, 9999.0)));
    }
}
