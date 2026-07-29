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

use lodestone_model::{Identifier, ItemEnchantment, Text, ToolPatch};

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
/// Well-known component identifier for the tool behaviour patch
/// (`minecraft:tool`).
pub const TOOL_COMPONENT: &str = "minecraft:tool";
/// Well-known component identifier for enchantments.
pub const ENCHANTMENTS_COMPONENT: &str = "minecraft:enchantments";
/// Well-known component identifier for the worn-slot component
/// (`minecraft:equippable`). Only the slot name is carried; the other nine
/// fields of vanilla's record are unmodelled.
pub const EQUIPPABLE_COMPONENT: &str = "minecraft:equippable";

/// A canonical, version-free component value.
///
/// Component payloads are, in the wire protocol, arbitrary NBT. Rather than
/// leak a protocol NBT type into this crate, the handful of values that game
/// logic actually inspects (counts, damage, names, tool behaviour,
/// enchantments) get typed variants, and everything else is carried as an
/// opaque, order-independent [`Opaque`] blob that only needs to compare equal
/// to itself. That is enough for the two questions gameplay asks of
/// components: *are these two stacks mergeable* (structural equality) and
/// *what is the max stack size / damage / tool* (typed lookups).
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
    /// What the stack's `DataComponentPatch` said about `minecraft:tool`.
    ///
    /// Carried verbatim from [`lodestone_model::ToolPatch`], including the
    /// `Inherited` vs `Set` vs `Removed` distinction — see that type's docs.
    /// Collapsing `Inherited` (no override; resolve against the item's
    /// built-in prototype) into `Removed` (explicitly bare-handed) or into a
    /// `Set` value would make a datapack's explicit override
    /// (`/give …[minecraft:tool={…}]`) indistinguishable from an ordinary
    /// vanilla tool, so this variant only appears when the patch is *not*
    /// `Inherited` — see [`ItemStack`]'s `From` impl.
    Tool(ToolPatch),
    /// Enchantments applied to the stack (`minecraft:enchantments`), carried
    /// verbatim and in wire order from [`lodestone_model::ItemEnchantment`].
    Enchantments(Vec<ItemEnchantment>),
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

impl From<&lodestone_model::ItemStack> for ItemStack {
    /// Lifts the model's wire stack (`item` + `count` + component patch) into
    /// the rich component-carrying canonical stack.
    ///
    /// This is the version-free "translate upward" adapter from plan §3.4.
    /// The model's [`lodestone_model::ItemComponents`] is itself a *patch*
    /// (the delta the wire actually carried, not the item's resolved
    /// effective components — see that type's docs), so each field is folded
    /// in only when the patch says something, and left absent from the
    /// resulting map otherwise:
    ///
    /// * `custom_name` / `damage` / non-empty `enchantments` become the
    ///   matching typed component when present.
    /// * `tool` becomes [`ComponentValue::Tool`] **only** when it is `Set` or
    ///   `Removed` — an `Inherited` patch means "no override" and is left out
    ///   entirely, so a plain vanilla tool (which always ships an empty tool
    ///   patch; the component lives in the item's built-in prototype) still
    ///   converts to an empty component set, matching a freshly-constructed
    ///   [`ItemStack::new`]. This is what keeps `Inherited` from becoming
    ///   indistinguishable from an absent component while also not inventing
    ///   a default value for items that never had one.
    ///
    /// `has_unmodeled` is not itself a component; it only flags that the wire
    /// patch had at least one trailing field this build cannot decode, which
    /// does not change the meaning of the fields that *were* decoded, so it
    /// carries no representation here.
    fn from(stack: &lodestone_model::ItemStack) -> Self {
        let mut components = ItemComponents::new();

        if let Some(name) = &stack.components.custom_name
            && let Ok(key) = CUSTOM_NAME_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Text(name.clone()));
        }

        if let Some(damage) = stack.components.damage
            && let Ok(key) = DAMAGE_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(damage)));
        }

        if !stack.components.enchantments.is_empty()
            && let Ok(key) = ENCHANTMENTS_COMPONENT.parse()
        {
            components.insert(
                key,
                ComponentValue::Enchantments(stack.components.enchantments.clone()),
            );
        }

        if !matches!(stack.components.tool, ToolPatch::Inherited)
            && let Ok(key) = TOOL_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Tool(stack.components.tool.clone()));
        }

        // The three *effective* fields — prototype folded with patch by the
        // version adapter. Unlike the patch-only fields above, these are
        // present for ordinary stacks, and without them armour cannot be
        // equipped at all (`equippable_slot` answers `None`, so an armour
        // slot's `may_place` is `None == Some(_)`) and every stack reads as
        // capping at 64, which is wrong for 295 of 1537 items.
        if let Some(slot) = stack.components.equippable
            && let Ok(key) = EQUIPPABLE_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Str(slot.name().to_string()));
        }

        if let Some(max) = stack.components.max_stack_size
            && let Ok(key) = MAX_STACK_SIZE_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(max)));
        }

        // Carried so `is_damageable` — and through it `is_stackable` — stops
        // answering "always stackable". Vanilla's predicate is
        // `MAX_DAMAGE && !UNBREAKABLE && DAMAGE`; `minecraft:unbreakable` is
        // still unmodelled, and `MAX_DAMAGE`/`DAMAGE` agree for all 1537 items
        // in 26.2 (asserted by the census), so this is sufficient today.
        if let Some(max) = stack.components.max_damage
            && let Ok(key) = MAX_DAMAGE_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(max)));
        }

        Self::with_components(
            stack.item.clone(),
            i32::try_from(stack.count).unwrap_or(i32::MAX),
            components,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{ItemComponents as ModelItemComponents, ItemTool, ToolBlocks, ToolRule};

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    /// A plain wire stack (empty component patch) must convert to an empty
    /// component set — an ordinary vanilla tool ships exactly this way (the
    /// component lives in the item's built-in prototype, not the delta), and
    /// this must stay indistinguishable from a freshly-built `ItemStack::new`.
    #[test]
    fn inherited_tool_patch_carries_no_tool_component() {
        let model = lodestone_model::ItemStack {
            item: id("minecraft:diamond_pickaxe"),
            count: 1,
            components: ModelItemComponents::default(),
        };
        let converted = ItemStack::from(&model);
        assert!(converted.components().is_empty());
        assert_eq!(
            converted,
            ItemStack::new(id("minecraft:diamond_pickaxe"), 1)
        );
    }

    /// An explicit `minecraft:tool` override (`/give …[minecraft:tool={…}]`)
    /// must survive the conversion and stay distinguishable from `Inherited`.
    #[test]
    fn explicit_tool_patch_is_carried_through() {
        let tool = ItemTool::new(
            vec![ToolRule::new(
                ToolBlocks::Tag(id("minecraft:mineable/pickaxe")),
                Some(8.0),
                Some(true),
            )],
            1.0,
            1,
            false,
        );
        let model = lodestone_model::ItemStack {
            item: id("minecraft:stick"),
            count: 1,
            components: ModelItemComponents {
                tool: ToolPatch::Set(tool.clone()),
                ..ModelItemComponents::default()
            },
        };
        let converted = ItemStack::from(&model);
        let key: Identifier = TOOL_COMPONENT.parse().expect("valid id");
        assert_eq!(
            converted.components().get(&key),
            Some(&ComponentValue::Tool(ToolPatch::Set(tool)))
        );
    }

    /// An explicit removal (`/give …[!minecraft:tool]`) is a distinct, real
    /// patch value and must not collapse into `Inherited`'s "no component"
    /// representation.
    #[test]
    fn removed_tool_patch_is_carried_through_and_distinct_from_inherited() {
        let model = lodestone_model::ItemStack {
            item: id("minecraft:diamond_pickaxe"),
            count: 1,
            components: ModelItemComponents {
                tool: ToolPatch::Removed,
                ..ModelItemComponents::default()
            },
        };
        let converted = ItemStack::from(&model);
        let key: Identifier = TOOL_COMPONENT.parse().expect("valid id");
        assert_eq!(
            converted.components().get(&key),
            Some(&ComponentValue::Tool(ToolPatch::Removed))
        );
        assert_ne!(
            converted,
            ItemStack::new(id("minecraft:diamond_pickaxe"), 1)
        );
    }

    /// Custom name, damage and enchantments all fold in when present.
    #[test]
    fn custom_name_damage_and_enchantments_are_carried_through() {
        let model = lodestone_model::ItemStack {
            item: id("minecraft:diamond_sword"),
            count: 1,
            components: ModelItemComponents {
                custom_name: Some(Text::literal("Excalibur")),
                damage: Some(3),
                enchantments: vec![lodestone_model::ItemEnchantment { id: 9, level: 4 }],
                ..ModelItemComponents::default()
            },
        };
        let converted = ItemStack::from(&model);
        assert_eq!(converted.components().len(), 3);
        assert_eq!(
            converted.components().get_str(CUSTOM_NAME_COMPONENT),
            Some(&ComponentValue::Text(Text::literal("Excalibur")))
        );
        assert_eq!(converted.components().get_int(DAMAGE_COMPONENT), Some(3));
        assert_eq!(
            converted.components().get_str(ENCHANTMENTS_COMPONENT),
            Some(&ComponentValue::Enchantments(vec![
                lodestone_model::ItemEnchantment { id: 9, level: 4 }
            ]))
        );
    }
}
