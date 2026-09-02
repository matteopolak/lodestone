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

/// The enchanting table's currency item — see [`SlotKind::LapisOnly`].
const LAPIS_LAZULI: &str = "minecraft:lapis_lazuli";

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
            // `"body"` is deliberately absent, not folded into `Chest`.
            // Vanilla gates humanoid armour on its own humanoid-armour
            // equipment-slot type, which covers feet/legs/chest/head and
            // **excludes** `BODY`. The prototype census makes this reachable
            // rather than theoretical: `wolf_armor` and all four
            // `*_horse_armor` items really are `body`, so folding them here put
            // animal armour in a player's chestplate slot.
            "chest" => Some(Self::Chest),
            "legs" => Some(Self::Legs),
            "feet" => Some(Self::Feet),
            "offhand" => Some(Self::Offhand),
            _ => None,
        }
    }

    /// This slot's index in the player's own inventory screen (window 0),
    /// matching [`crate::menu::Menu::player`]'s layout — `5..=8` for the four
    /// humanoid-armour positions, `None` for [`Offhand`](Self::Offhand).
    ///
    /// Restricted to `HUMANOID_ARMOR` the same way [`Self::from_name`] already
    /// excludes `"body"`: vanilla's own item-use step, through its own
    /// swap-with-equipment-slot step, reaches any [`Self`] the component names, but the
    /// only slot a *player's own hotbar* right-click can land in is one of
    /// these four — `Offhand` has no real item that declares it (the shield
    /// mechanic goes through `minecraft:blocks_attacks`, not a swap), and
    /// `Offhand`'s own menu index (`45`) is intentionally not returned here so
    /// a caller cannot accidentally wire the F-key swap-to-offhand action
    /// through this method instead of its own.
    #[must_use]
    pub fn player_menu_index(self) -> Option<usize> {
        match self {
            Self::Head => Some(5),
            Self::Chest => Some(6),
            Self::Legs => Some(7),
            Self::Feet => Some(8),
            Self::Offhand => None,
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
    /// The enchanting table's currency slot: accepts only lapis lazuli
    /// (vanilla's own enchanting-table lapis slot, whose `mayPlace`
    /// checks `itemStack.is(Items.LAPIS_LAZULI)`). A dedicated variant rather
    /// than a closure predicate because [`Menu`](crate::menu::Menu) derives
    /// `PartialEq`/`Eq` (needed for [`crate::reconcile`]'s predict/reconcile
    /// diffing), which a stored `fn` or closure would break.
    LapisOnly,
}

/// A view onto one index of one container, plus its rules.
///
/// `container` indexes into the owning menu's container list; `index` is the
/// slot within that container. `max_stack_size` is the slot's own cap (vanilla's
/// own max-stack-size getter), combined with the item's cap at query time.
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
    /// The GUI sprite drawn in this slot **while it is empty** — vanilla's
    /// own no-item-icon getter, `null` on an ordinary slot.
    ///
    /// A bare sprite id relative to `gui/sprites/`, e.g. `container/slot/helmet`,
    /// which is what `lodestone_render::GuiAtlas` keys on. Version-free: these are
    /// resource paths, not registry ids, and they have been stable since the
    /// sprite atlas replaced the old `empty_armor_slot_*` texture constants.
    ///
    /// # Why this is a field and not a match on the slot index
    ///
    /// Vanilla stores it per slot too — its own armour-slot type takes it as a constructor
    /// argument and the off-hand slot overrides the getter — and the reason is that the family is much
    /// larger than the player screen's four armour slots. 26.2 ships **36**
    /// `container/slot/*` sprites: horse armour, llama carpet, saddle, the five
    /// smithing/brewing ingredient hints, per-tool hints for the smithing table,
    /// and so on. Keying on "menu slot 5..=8 in the player menu" gets the four
    /// this client shows today and forfeits every one of those the day a horse or
    /// brewing menu is added.
    ///
    /// The **draw** rule is vanilla's, in `AbstractContainerScreen.extractSlot`
    /// (`:224-230`): when the slot is empty and active, the icon is blitted 16x16
    /// at the cell origin and `done = true` — so it *replaces* the item path
    /// rather than layering beneath it.
    pub no_item_icon: Option<&'static str>,
}

impl Slot {
    /// Creates a normal slot with the default cap of 64.
    #[must_use]
    pub fn normal(container: usize, index: usize) -> Self {
        Self::of(container, index, SlotKind::Normal)
    }

    /// Creates a slot of a given kind with the default cap of 64.
    #[must_use]
    pub fn of(container: usize, index: usize, kind: SlotKind) -> Self {
        Self {
            container,
            index,
            kind,
            max_stack_size: 64,
            no_item_icon: None,
        }
    }

    /// Attach an empty-slot sprite — see [`no_item_icon`](Self::no_item_icon).
    #[must_use]
    pub fn with_no_item_icon(mut self, icon: &'static str) -> Self {
        self.no_item_icon = Some(icon);
        self
    }

    /// Returns whether `stack` may be placed into this slot.
    ///
    /// Output slots reject everything. Armour slots accept only a stack whose
    /// `minecraft:equippable` component names the matching position; a stack
    /// with no such component is rejected, matching vanilla `ArmorSlot`. The
    /// enchanting table's lapis slot accepts only `minecraft:lapis_lazuli`. All
    /// other slots accept any item.
    #[must_use]
    pub fn may_place(&self, stack: &ItemStack) -> bool {
        match self.kind {
            SlotKind::Output => false,
            SlotKind::Armor(target) => equippable_slot(stack) == Some(target),
            SlotKind::LapisOnly => stack.item().to_string() == LAPIS_LAZULI,
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
