//! Item **prototype** component census for protocol 776 (Minecraft 26.2): the
//! per-item `minecraft:max_stack_size`, `minecraft:max_damage` and
//! `minecraft:equippable` values that a clientbound stack never carries.
//!
//! # Why a version-owned census, and not the wire
//!
//! A clientbound `ItemStack` is `(count, item registry id, DataComponentPatch)`,
//! and that patch is the **delta** from the item's built-in prototype component
//! map. Vanilla keeps all three of these components in that prototype, so
//! `/give … diamond_helmet` arrives as an *empty patch* and the client is
//! expected to already know them. They cannot be captured from a packet dump at
//! any level of effort, because they are never on the wire.
//!
//! This is the same shape of problem [`crate::tool`] solves for
//! `minecraft:tool`, and the same answer: boot the real jar, walk the item
//! registry, commit the dump, generate the table.
//!
//! # What each one breaks while it is missing
//!
//! * **`minecraft:equippable`** — vanilla's own armour-slot "may place" check is
//!   an "is equippable in slot" check on the holding entity, which is
//!   `slot == equippable.slot() && canUseSlot(…) && equippable.canBeEquippedBy(…)`.
//!   With no component, the only slot that
//!   accepts anything is the main-hand slot — **no armour is equippable by any click
//!   type**.
//! * **`minecraft:max_stack_size`** — vanilla's own item-instance "get max
//!   stack size" accessor is
//!   `getOrDefault(MAX_STACK_SIZE, 1)`. Vanilla's own
//!   common-item-components default component map sets `64`, so a
//!   client that assumes 64 is right for most items and wrong for every bucket,
//!   shulker box, tool and egg — every drag distributing those over-fills and is
//!   corrected.
//! * **`minecraft:max_damage`** — gates vanilla's own "is damageable item" check
//!   and therefore its own "is stackable" check: without it two
//!   identically-componented swords merge into a stack of two.
//!
//! # Scope: the slot, not the whole equippable component
//!
//! Vanilla's own equippable component record has eleven fields. This census carries the two
//! that decide placement — the slot and whether the allowed-entities set is empty — and
//! deliberately not the equip sound, asset id, camera overlay, dispensable and
//! swappable flags, damage-on-hurt, equip-on-interact, can-be-sheared or
//! shearing-sound fields. See the module's own doc for the reasoning; a consumer
//! needing those wants the wire component, not this table.

use lodestone_model::{EquipmentSlot, ItemPrototype};

use crate::generated_item_prototypes as generated;

pub use generated::ITEM_COUNT;

/// One item's built-in prototype components, as stored in rodata.
///
/// The static mirror of [`lodestone_model::ItemPrototype`], narrowed to the
/// smallest integer types the real data needs (`max_stack_size` is `1..=99`,
/// `max_damage` peaks at 2031 for a netherite pickaxe).
#[derive(Clone, Copy, Debug)]
pub struct ItemPrototypeDef {
    /// Effective `minecraft:max_stack_size` (vanilla range `1..=99`).
    pub max_stack_size: u8,
    /// `minecraft:max_damage`, or `None` when the prototype has none at all.
    pub max_damage: Option<u16>,
    /// Vanilla's own equippable component's slot field, or `None` for a non-equippable item.
    pub equip_slot: Option<EquipmentSlot>,
    /// Whether the prototype also carries `minecraft:damage`, which
    /// vanilla's own "is damageable item" check separately requires.
    ///
    /// In 26.2 this is exactly `max_damage.is_some()` for every one of the 1,537
    /// items — asserted, not assumed, by `tests/item_prototypes.rs` — so nothing
    /// reads it today. It is carried so a future version where the two diverge
    /// fails that assertion instead of silently mis-answering "is damageable item".
    pub has_damage: bool,
    /// Whether the equippable component's allowed-entities set is empty, i.e. any entity may wear
    /// it (vanilla's own "can be equipped by" check).
    pub equippable_by_any_entity: bool,
}

/// The prototype components of `item` (for example `"minecraft:diamond_helmet"`),
/// or `None` for an item this version does not know.
///
/// The text is an external/dynamic boundary. Built-in names validate into an
/// [`crate::item::Item`] before indexing the census; custom or future keys stay
/// unresolved rather than being mistaken for a built-in item.
#[must_use]
pub fn prototype(item: &str) -> Option<&'static ItemPrototypeDef> {
    crate::item::Item::from_name(item).map(prototype_for)
}

/// The total lookup for a caller already holding a validated
/// [`crate::item::Item`].
///
/// Infallible: an [`crate::item::Item`] and this table's `0..ITEM_COUNT` are
/// the same `minecraft:item` registry (both generated from
/// `tests/support/item_prototype_jvm.txt`), so every valid `Item` indexes a
/// real row. The `.expect()` documents that invariant instead of pushing an
/// `Option` the caller has no way to hit onto every call site — the pattern
/// `docs/registry-types.md` calls out for `Identifier::new(..).expect(..)`
/// call sites once a registry has a typed, infallible id.
#[must_use]
pub fn prototype_for(item: crate::item::Item) -> &'static ItemPrototypeDef {
    generated::ITEM_PROTOTYPES
        .get(item.registry_id() as usize)
        .unwrap_or_else(|| {
        panic!(
            "Item::{item:?} (registry id {}) has no row in the generated item-prototype table",
            item.registry_id()
        )
    })
}

/// The version-free view of `item`'s prototype, for
/// `VersionAdapter::item_prototype`.
#[must_use]
pub fn model_prototype(item: &str) -> Option<ItemPrototype> {
    prototype(item).map(|def| ItemPrototype {
        max_stack_size: u32::from(def.max_stack_size),
        max_damage: def.max_damage.map(u32::from),
        equip_slot: def.equip_slot,
        equippable_by_any_entity: def.equippable_by_any_entity,
    })
}
