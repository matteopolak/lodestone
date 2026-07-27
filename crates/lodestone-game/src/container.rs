//! Slot-indexed containers and the slot rules that govern them.
//!
//! A [`Container`] is a flat, version-free array of optional stacks. A [`Slot`]
//! is a *view* onto one index of one container, carrying the placement and
//! pickup rules for that position (a furnace output rejects insertions, an
//! armour slot accepts only matching equipment, and so on). A menu (see
//! [`crate::menu`]) is an ordered list of slots layered over one or more
//! containers; the same backing container can appear both as menu slots and as
//! a natively-addressed inventory, which is exactly how a number-key swap moves
//! an item the open menu is simultaneously displaying.

use crate::item::ItemStack;

/// A flat array of item slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    slots: Vec<Option<ItemStack>>,
}

impl Container {
    /// Creates an empty container with `size` slots.
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self {
            slots: vec![None; size],
        }
    }

    /// Returns the number of slots.
    #[must_use]
    pub fn size(&self) -> usize {
        self.slots.len()
    }

    /// Returns the stack in `index`, or `None` for an empty slot or out-of-range
    /// index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&ItemStack> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// Replaces the stack in `index`, returning the previous contents.
    ///
    /// Out-of-range indices are ignored and return `None`.
    pub fn set(&mut self, index: usize, stack: Option<ItemStack>) -> Option<ItemStack> {
        match self.slots.get_mut(index) {
            Some(slot) => std::mem::replace(slot, stack),
            None => None,
        }
    }

    /// Returns whether every slot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// Iterates over the slots.
    pub fn iter(&self) -> impl Iterator<Item = &Option<ItemStack>> {
        self.slots.iter()
    }
}

/// The equipment position an armour slot accepts, matching the string form of a
/// `minecraft:equippable` component's `slot` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EquipmentSlot {
    /// Helmet slot.
    Head,
    /// Chestplate slot.
    Chest,
    /// Leggings slot.
    Legs,
    /// Boots slot.
    Feet,
    /// Off-hand slot.
    Offhand,
}

impl EquipmentSlot {
    /// Parses the canonical `minecraft:equippable` slot name.
    #[must_use]
    pub fn from_name(value: &str) -> Option<Self> {
        match value {
            "head" => Some(Self::Head),
            "chest" | "body" => Some(Self::Chest),
            "legs" => Some(Self::Legs),
            "feet" => Some(Self::Feet),
            "offhand" => Some(Self::Offhand),
            _ => None,
        }
    }
}

/// The behavioural category of a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// An ordinary storage slot that accepts and yields any item.
    Normal,
    /// A crafting-grid input slot (accepts any item; feeds a recipe).
    CraftingInput,
    /// A result/output slot: take-only, never accepts a placed item.
    Output,
    /// An armour slot accepting only equipment for the given position.
    Armor(EquipmentSlot),
    /// The off-hand slot (accepts any item, like vanilla's shield slot).
    Offhand,
}

/// A view onto one index of one container, plus its rules.
///
/// `container` indexes into the owning menu's container list; `index` is the
/// slot within that container. `max_stack_size` is the slot's own cap (vanilla
/// `Slot.getMaxStackSize`), combined with the item's cap at query time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Which backing container this slot reads and writes.
    pub container: usize,
    /// The index within the backing container.
    pub index: usize,
    /// The slot's behavioural category.
    pub kind: SlotKind,
    /// The slot's own maximum stack size before combining with the item's.
    pub max_stack_size: i32,
}

impl Slot {
    /// Creates a normal slot with the default cap of 64.
    #[must_use]
    pub fn normal(container: usize, index: usize) -> Self {
        Self {
            container,
            index,
            kind: SlotKind::Normal,
            max_stack_size: 64,
        }
    }

    /// Creates a slot of a given kind with the default cap of 64.
    #[must_use]
    pub fn of(container: usize, index: usize, kind: SlotKind) -> Self {
        Self {
            container,
            index,
            kind,
            max_stack_size: 64,
        }
    }

    /// Returns whether `stack` may be placed into this slot.
    ///
    /// Output slots reject everything. Armour slots accept only a stack whose
    /// `minecraft:equippable` component names the matching position; a stack
    /// with no such component is rejected, matching vanilla `ArmorSlot`. All
    /// other slots accept any item.
    #[must_use]
    pub fn may_place(&self, stack: &ItemStack) -> bool {
        match self.kind {
            SlotKind::Output => false,
            SlotKind::Armor(target) => equippable_slot(stack) == Some(target),
            SlotKind::Normal | SlotKind::CraftingInput | SlotKind::Offhand => true,
        }
    }

    /// Returns whether an item may be taken from this slot. Always `true` in the
    /// base model; a hook for locked/gated slots can override later.
    #[must_use]
    pub fn may_pickup(&self) -> bool {
        true
    }

    /// Returns the effective cap for `stack` in this slot: the smaller of the
    /// slot's own cap and the stack's own max size.
    #[must_use]
    pub fn effective_max(&self, stack: &ItemStack) -> i32 {
        self.max_stack_size.min(stack.max_stack_size())
    }
}

/// Returns the equipment position an item declares via its
/// `minecraft:equippable` component, if any.
///
/// The component is read as either a [`Str`](crate::item::ComponentValue::Str)
/// naming the slot directly (a canonical simplification) or is absent. Adapters
/// that carry the full equippable payload should surface the slot as this
/// string so version-free quick-move and armour-slot rules work without item
/// registry knowledge.
#[must_use]
pub fn equippable_slot(stack: &ItemStack) -> Option<EquipmentSlot> {
    match stack.components().get_str("minecraft:equippable")? {
        crate::item::ComponentValue::Str(slot) => EquipmentSlot::from_name(slot),
        _ => None,
    }
}
