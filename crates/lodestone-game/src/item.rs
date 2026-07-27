//! Version-free item stacks and item components.
//!
//! Modern Minecraft (1.20.5+) replaced the old `id + damage + NBT` item shape
//! with **data components**: a stack is an item identifier plus a set of typed
//! component values (`minecraft:max_stack_size`, `minecraft:damage`,
//! `minecraft:custom_name`, …). This crate models that newest concept, per the
//! plan's canonical-model-on-the-newest-version rule (§3.4). A 1.8 adapter that
//! only has a numeric id, damage and NBT is expected to translate *upward* into
//! this shape; it must never push numeric ids into this crate.
//!
//! [`ItemStack`] intentionally represents only *non-empty* stacks (`count >=
//! 1`). Emptiness is modelled with `Option<ItemStack>` at the slot/cursor level,
//! which is both more idiomatic in Rust and removes the whole class of "is this
//! the EMPTY singleton or a real zero-count stack?" bugs that the Java code
//! guards against with a shared sentinel.

use std::collections::BTreeMap;

use lodestone_model::{Identifier, Text};

/// The default maximum stack size when an item carries no
/// `minecraft:max_stack_size` component. Matches vanilla's `Item.Properties`
/// default of 64.
pub const DEFAULT_MAX_STACK_SIZE: i32 = 64;

/// Well-known component identifier for the maximum stack size.
pub const MAX_STACK_SIZE_COMPONENT: &str = "minecraft:max_stack_size";
/// Well-known component identifier for accumulated item damage.
pub const DAMAGE_COMPONENT: &str = "minecraft:damage";
/// Well-known component identifier for the maximum item damage.
pub const MAX_DAMAGE_COMPONENT: &str = "minecraft:max_damage";
/// Well-known component identifier for a custom display name.
pub const CUSTOM_NAME_COMPONENT: &str = "minecraft:custom_name";

/// A canonical, version-free component value.
///
/// Component payloads are, in the wire protocol, arbitrary NBT. Rather than
/// leak a protocol NBT type into this crate, the handful of values that game
/// logic actually inspects (counts, damage, names) get typed variants, and
/// everything else is carried as an opaque, order-independent [`Opaque`] blob
/// that only needs to compare equal to itself. That is enough for the two
/// questions gameplay asks of components: *are these two stacks mergeable*
/// (structural equality) and *what is the max stack size / damage* (typed
/// lookups).
///
/// [`Opaque`]: ComponentValue::Opaque
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentValue {
    /// A signed integer component (stack size, damage, …).
    Int(i64),
    /// A boolean flag component.
    Bool(bool),
    /// A UTF-8 string component.
    Str(String),
    /// A chat-component value (custom name, lore lines, …).
    Text(Text),
    /// An opaque, adapter-supplied payload compared byte-for-byte.
    ///
    /// The bytes are whatever canonical encoding the producing adapter chose
    /// (typically network NBT). This crate never interprets them; it only
    /// compares them, so two stacks with identical opaque blobs stack and two
    /// with differing blobs do not.
    Opaque(Vec<u8>),
}

/// The effective, resolved component set of an [`ItemStack`].
///
/// This is the *flattened* view (prototype defaults already merged with the
/// item's patch), not vanilla's `added`/`removed` patch representation. An
/// adapter is responsible for producing the effective set; keeping the patch
/// out of the shared model means click/craft logic never has to reason about
/// component inheritance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemComponents {
    values: BTreeMap<Identifier, ComponentValue>,
}

impl ItemComponents {
    /// Creates an empty component set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when no components are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the number of components present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Inserts or replaces a component, returning the previous value.
    pub fn insert(&mut self, key: Identifier, value: ComponentValue) -> Option<ComponentValue> {
        self.values.insert(key, value)
    }

    /// Removes a component by identifier, returning it if present.
    pub fn remove(&mut self, key: &Identifier) -> Option<ComponentValue> {
        self.values.remove(key)
    }

    /// Returns a component value by identifier.
    #[must_use]
    pub fn get(&self, key: &Identifier) -> Option<&ComponentValue> {
        self.values.get(key)
    }

    /// Returns a component value by its string identifier.
    ///
    /// Returns `None` for an unparseable identifier rather than panicking.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&ComponentValue> {
        let id: Identifier = key.parse().ok()?;
        self.values.get(&id)
    }

    /// Looks up an integer-valued component by string identifier.
    #[must_use]
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.get_str(key)? {
            ComponentValue::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Iterates over all components in identifier order.
    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, &ComponentValue)> {
        self.values.iter()
    }
}

/// A stack of a single item.
///
/// A stored stack normally has `count >= 1`; a `count` of zero means *empty* and
/// is the transient sentinel used mid-algorithm (matching vanilla, whose
/// `moveItemStackTo` drives a working stack's count to zero). Slots and the
/// cursor are `Option<ItemStack>` and normalise an empty stack to `None` at
/// every write boundary, so a stored `Some(_)` is always genuinely non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemStack {
    item: Identifier,
    count: i32,
    components: ItemComponents,
}

impl ItemStack {
    /// Creates a stack of `count` of `item` with no components.
    ///
    /// A negative `count` is clamped to zero (empty).
    #[must_use]
    pub fn new(item: Identifier, count: i32) -> Self {
        Self {
            item,
            count: count.max(0),
            components: ItemComponents::new(),
        }
    }

    /// Creates a stack with an explicit component set.
    #[must_use]
    pub fn with_components(item: Identifier, count: i32, components: ItemComponents) -> Self {
        Self {
            item,
            count: count.max(0),
            components,
        }
    }

    /// Returns whether this stack is empty (`count <= 0`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count <= 0
    }

    /// Returns the item identifier.
    #[must_use]
    pub fn item(&self) -> &Identifier {
        &self.item
    }

    /// Returns the stack count.
    #[must_use]
    pub fn count(&self) -> i32 {
        self.count
    }

    /// Returns the component set.
    #[must_use]
    pub fn components(&self) -> &ItemComponents {
        &self.components
    }

    /// Returns a mutable reference to the component set.
    pub fn components_mut(&mut self) -> &mut ItemComponents {
        &mut self.components
    }

    /// Sets the maximum stack size component and returns the modified stack.
    #[must_use]
    pub fn with_max_stack_size(mut self, size: i32) -> Self {
        if let Ok(key) = MAX_STACK_SIZE_COMPONENT.parse() {
            self.components
                .insert(key, ComponentValue::Int(i64::from(size)));
        }
        self
    }

    /// Returns the maximum number of items this stack can hold.
    ///
    /// Reads the `minecraft:max_stack_size` component and falls back to
    /// [`DEFAULT_MAX_STACK_SIZE`]. The value is clamped to `1..=99` to match
    /// vanilla's `ITEM_STACK_LIMIT`.
    #[must_use]
    pub fn max_stack_size(&self) -> i32 {
        let raw = self
            .components
            .get_int(MAX_STACK_SIZE_COMPONENT)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(DEFAULT_MAX_STACK_SIZE);
        raw.clamp(1, 99)
    }

    /// Returns whether more than one of this item may occupy a stack.
    ///
    /// Mirrors vanilla `ItemStack.isStackable`: a stack is stackable when its
    /// max size exceeds one and it is not a damaged tool. Damage is detected via
    /// the `minecraft:damage` / `minecraft:max_damage` components.
    #[must_use]
    pub fn is_stackable(&self) -> bool {
        self.max_stack_size() > 1 && !self.is_damaged()
    }

    /// Returns whether the item is currently damaged (damage > 0 and damageable).
    #[must_use]
    pub fn is_damaged(&self) -> bool {
        self.is_damageable() && self.components.get_int(DAMAGE_COMPONENT).unwrap_or(0) > 0
    }

    /// Returns whether the item can take damage (has a `minecraft:max_damage`).
    #[must_use]
    pub fn is_damageable(&self) -> bool {
        self.components.get_int(MAX_DAMAGE_COMPONENT).is_some()
    }

    /// Sets the stack count.
    ///
    /// A negative count is clamped to zero (empty).
    pub fn set_count(&mut self, count: i32) {
        self.count = count.max(0);
    }

    /// Increases the count by `amount`.
    pub fn grow(&mut self, amount: i32) {
        self.count = self.count.saturating_add(amount).max(0);
    }

    /// Decreases the count by `amount`, saturating at zero (empty).
    pub fn shrink(&mut self, amount: i32) {
        self.count = (self.count - amount).max(0);
    }

    /// Splits up to `amount` items off this stack.
    ///
    /// Returns the removed portion as a new stack (at most the current count)
    /// and reduces this stack, which may become empty (`count == 0`). Mirrors
    /// vanilla `ItemStack.split`.
    pub fn split(&mut self, amount: i32) -> ItemStack {
        let taken = amount.clamp(0, self.count);
        self.count -= taken;
        let mut removed = self.clone();
        removed.count = taken;
        removed
    }

    /// Returns whether two stacks are the same item, ignoring count and
    /// components.
    #[must_use]
    pub fn is_same_item(a: &ItemStack, b: &ItemStack) -> bool {
        a.item == b.item
    }

    /// Returns whether two stacks are the same item *and* carry identical
    /// components — the condition under which vanilla lets them merge.
    #[must_use]
    pub fn is_same_item_same_components(a: &ItemStack, b: &ItemStack) -> bool {
        a.item == b.item && a.components == b.components
    }
}

/// Extension helpers for the `Option<ItemStack>` slot/cursor representation.
///
/// These centralise the "empty is `None`" convention so the container and click
/// code never open-code `count == 0` checks.
pub trait SlotStack {
    /// Returns the count, treating `None` as zero.
    fn stack_count(&self) -> i32;
    /// Returns whether the slot is empty.
    fn is_empty(&self) -> bool;
}

impl SlotStack for Option<ItemStack> {
    fn stack_count(&self) -> i32 {
        self.as_ref().map_or(0, ItemStack::count)
    }

    fn is_empty(&self) -> bool {
        self.as_ref().is_none_or(ItemStack::is_empty)
    }
}

/// Normalises a stack into `Option`, collapsing an empty stack to `None`.
#[must_use]
pub fn normalize(stack: ItemStack) -> Option<ItemStack> {
    if stack.is_empty() { None } else { Some(stack) }
}
