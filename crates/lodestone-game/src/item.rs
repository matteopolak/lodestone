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

use lodestone_model::{ArmorTrim, Identifier, ItemEnchantment, Text, ToolPatch};

/// The default maximum stack size when an item carries no
/// `minecraft:max_stack_size` component. Matches vanilla's `Item.Properties`
/// default of 64.
pub const DEFAULT_MAX_STACK_SIZE: i32 = 64;

/// The namespace the vanilla item registry owns.
///
/// Load-bearing in two opposite directions for [`crate::custom_item`]: a
/// plugin's *own* item id may not be in it (that would collide with the vanilla
/// registry), while a custom item's **base** item must be, because only a
/// `minecraft:` item has a wire encoding at all.
pub const VANILLA_ITEM_NAMESPACE: &str = "minecraft";

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

/// Well-known component identifier for `minecraft:trim`.
pub const TRIM_COMPONENT: &str = "minecraft:trim";

/// Well-known component identifier for `minecraft:map_id`.
pub const MAP_ID_COMPONENT: &str = "minecraft:map_id";
/// Well-known component identifier for enchantments.
pub const ENCHANTMENTS_COMPONENT: &str = "minecraft:enchantments";
/// Well-known component identifier for the worn-slot component
/// (`minecraft:equippable`). Only the slot name is carried; the other nine
/// fields of vanilla's record are unmodelled.
pub const EQUIPPABLE_COMPONENT: &str = "minecraft:equippable";
/// Well-known component identifier for `minecraft:dyed_color`.
///
/// Added by issue #143. Before it, this crate defined no key for the component
/// and [`ItemStack`]'s `From<&lodestone_model::ItemStack>` had no branch for it,
/// so **the dye was silently dropped at the crate boundary**: armour rendered
/// dyed (that path reads the *model* stack off `Equipment`) while the same
/// item's GUI icon did not, because the icon reads the game stack. The value is
/// the raw wire int, low 24 bits RGB — see
/// [`lodestone_model::ItemComponents::dyed_color`] for why it is not pre-split.
pub const DYED_COLOR_COMPONENT: &str = "minecraft:dyed_color";
/// Well-known component identifier for `minecraft:potion_contents`.
///
/// The value carried under this key is not the raw component — it is
/// [`lodestone_model::ItemComponents::potion_color`]'s already-mixed opaque ARGB
/// (`Potion.calculate`'s result), because nothing on this side of the crate boundary
/// needs the potion id or effect list back out, only the colour a GUI icon or
/// equipped-item render would tint by. Same crate-boundary gap `DYED_COLOR_COMPONENT`
/// was added to close: without a branch here a potion's colour is silently dropped
/// converting a decoded stack into this crate's shape.
pub const POTION_COLOR_COMPONENT: &str = "minecraft:potion_contents";
/// Well-known component identifier for `minecraft:custom_model_data`.
///
/// Vanilla's own "make this item look different" channel, and half of how real
/// Paper plugins ship custom items. Carried as [`ComponentValue::Int`] — only the
/// first `floats`/`flags`/`strings`/`colors` entry vanilla's record holds is
/// modelled, because that is the field resource packs actually select on.
pub const CUSTOM_MODEL_DATA_COMPONENT: &str = "minecraft:custom_model_data";
/// The component a plugin's *own* item identity lives under (issue #147).
///
/// **Deliberately in the `lodestone:` namespace, not `minecraft:`.** It is not a
/// vanilla component and must never be mistaken for one: a real server would
/// reject it, and a future decoder must not try to resolve it against
/// `lodestone-data`'s 111-entry component registry. The value is
/// [`ComponentValue::Str`] holding the custom item's namespaced id.
///
/// This is the typed equivalent of the `PersistentDataContainer` tag every
/// Bukkit/Paper custom-item plugin attaches to a vanilla item id, and it exists
/// for the same reason theirs does: the wire has a **fixed item-id space**, so a
/// genuinely novel item id is not representable, exactly as a novel entity type
/// is not (#140). A custom item is a vanilla item plus this tag, and nothing else.
pub const PLUGIN_ITEM_ID_COMPONENT: &str = "lodestone:item_id";

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
    /// The stack's `minecraft:trim` — a smithing-table armour trim's material
    /// and pattern, carried verbatim from [`lodestone_model::ArmorTrim`].
    ///
    /// A variant of its own rather than a `Str` pair because a trim is two
    /// registry entries that must stay together: half a trim is not a trim, and
    /// the renderer keys its decal atlas on the pair.
    Trim(ArmorTrim),
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

    // -- Typed component read/write for plugins (issue #143) -----------
    //
    // `components_mut` has been public for a long time and had **zero
    // production callers** — every call site was a test. That is because the
    // component set is an opaque `BTreeMap<Identifier, ComponentValue>`: to
    // write one correctly a caller had to know the exact key string *and* the
    // right `ComponentValue` variant *and* the `Inherited`-is-not-absent rule
    // for `minecraft:tool`. Getting any of the three wrong produces a component
    // that reads back as absent, silently.
    //
    // These accessors are the typed surface issue #143 asks for — "a typed API,
    // not raw NBT bytes". They are deliberately *not* a second storage: every
    // one reads and writes the same `ItemComponents` map, so a plugin writing
    // through them and a decoder writing through the `From` impl are
    // indistinguishable downstream.

    /// The stack's custom display name, if the component set carries one.
    #[must_use]
    pub fn custom_name(&self) -> Option<&Text> {
        match self.components.get_str(CUSTOM_NAME_COMPONENT)? {
            ComponentValue::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Sets or clears the custom display name.
    pub fn set_custom_name(&mut self, name: Option<Text>) {
        self.write_component(CUSTOM_NAME_COMPONENT, name.map(ComponentValue::Text));
    }

    /// Accumulated durability damage, or `None` when the stack carries no
    /// `minecraft:damage`.
    #[must_use]
    pub fn damage(&self) -> Option<i32> {
        self.components
            .get_int(DAMAGE_COMPONENT)
            .and_then(|v| i32::try_from(v).ok())
    }

    /// Sets or clears accumulated durability damage.
    pub fn set_damage(&mut self, damage: Option<i32>) {
        self.write_component(
            DAMAGE_COMPONENT,
            damage.map(|d| ComponentValue::Int(i64::from(d))),
        );
    }

    /// The stack's `minecraft:dyed_color`, low 24 bits RGB, or `None` when
    /// undyed.
    ///
    /// Distinct from `Some(0)`, which is a dye that resolves to black —
    /// vanilla treats that as undyed downstream but the two are different
    /// states on the wire, and flattening them here would lose the difference
    /// for a plugin that wants it.
    #[must_use]
    pub fn dyed_color(&self) -> Option<u32> {
        self.components
            .get_int(DYED_COLOR_COMPONENT)
            .and_then(|v| u32::try_from(v).ok())
    }

    /// Sets or clears `minecraft:dyed_color`.
    pub fn set_dyed_color(&mut self, rgb: Option<u32>) {
        self.write_component(
            DYED_COLOR_COMPONENT,
            rgb.map(|c| ComponentValue::Int(i64::from(c))),
        );
    }

    /// The stack's mixed `minecraft:potion_contents` colour — opaque ARGB, or
    /// `None` when the stack carries no potion contents at all (a non-potion item,
    /// or one whose decode never modelled the component). See
    /// [`POTION_COLOR_COMPONENT`] for why this is already-mixed rather than raw.
    #[must_use]
    pub fn potion_color(&self) -> Option<u32> {
        self.components
            .get_int(POTION_COLOR_COMPONENT)
            .and_then(|v| u32::try_from(v).ok())
    }

    /// Sets or clears the mixed `minecraft:potion_contents` colour.
    pub fn set_potion_color(&mut self, argb: Option<u32>) {
        self.write_component(
            POTION_COLOR_COMPONENT,
            argb.map(|c| ComponentValue::Int(i64::from(c))),
        );
    }

    /// The stack's enchantments in wire order, or an empty slice when it has
    /// none.
    ///
    /// This is the read `lodestone_render::glint::has_foil` could not do from
    /// the shell: that function takes the *model* `ItemComponents`, whose
    /// `enchantments` is a plain `Vec` field, and the shell's stacks are these.
    /// `docs/enchantment-glint.md` records the split.
    #[must_use]
    pub fn enchantments(&self) -> &[ItemEnchantment] {
        match self.components.get_str(ENCHANTMENTS_COMPONENT) {
            Some(ComponentValue::Enchantments(list)) => list,
            _ => &[],
        }
    }

    /// Replaces the stack's enchantments. An empty list **removes** the
    /// component rather than storing an empty one, matching what the decode
    /// path does — an empty `minecraft:enchantments` and an absent one are the
    /// same state, and storing the former would make two otherwise-identical
    /// stacks refuse to merge.
    pub fn set_enchantments(&mut self, enchantments: Vec<ItemEnchantment>) {
        let value = (!enchantments.is_empty()).then_some(ComponentValue::Enchantments(enchantments));
        self.write_component(ENCHANTMENTS_COMPONENT, value);
    }

    /// What the stack says about `minecraft:tool`.
    ///
    /// An **absent** component reads as [`ToolPatch::Inherited`], which is the
    /// correct lift: the wire omitting the component and the wire saying
    /// "inherit" mean the same thing, and it is `Removed` that means
    /// explicitly bare-handed. Collapsing these the other way is the trap
    /// `lodestone_model::ToolPatch`'s own docs describe — it makes every real
    /// pickaxe mine at fist speed.
    #[must_use]
    pub fn tool(&self) -> ToolPatch {
        match self.components.get_str(TOOL_COMPONENT) {
            Some(ComponentValue::Tool(patch)) => patch.clone(),
            _ => ToolPatch::Inherited,
        }
    }

    /// Sets the `minecraft:tool` patch. [`ToolPatch::Inherited`] **removes** the
    /// component, for the same reason [`Self::tool`] lifts an absent one to
    /// `Inherited`.
    pub fn set_tool(&mut self, patch: ToolPatch) {
        let value = (!matches!(patch, ToolPatch::Inherited)).then(|| ComponentValue::Tool(patch));
        self.write_component(TOOL_COMPONENT, value);
    }

    /// The stack's `minecraft:trim`, or `None` for untrimmed armour and every
    /// non-armour item.
    #[must_use]
    pub fn trim(&self) -> Option<ArmorTrim> {
        match self.components.get_str(TRIM_COMPONENT) {
            Some(ComponentValue::Trim(trim)) => Some(trim.clone()),
            _ => None,
        }
    }

    /// Sets or clears `minecraft:trim`.
    pub fn set_trim(&mut self, trim: Option<ArmorTrim>) {
        self.write_component(TRIM_COMPONENT, trim.map(ComponentValue::Trim));
    }

    /// Which saved map a `filled_map` stack shows, or `None` for anything else.
    #[must_use]
    pub fn map_id(&self) -> Option<i32> {
        self.components
            .get_int(MAP_ID_COMPONENT)
            .and_then(|v| i32::try_from(v).ok())
    }

    /// Sets or clears `minecraft:map_id`.
    pub fn set_map_id(&mut self, id: Option<i32>) {
        self.write_component(MAP_ID_COMPONENT, id.map(|v| ComponentValue::Int(i64::from(v))));
    }

    /// The stack's `minecraft:custom_model_data` selector, if any (issue #147).
    #[must_use]
    pub fn custom_model_data(&self) -> Option<i32> {
        self.components
            .get_int(CUSTOM_MODEL_DATA_COMPONENT)
            .and_then(|v| i32::try_from(v).ok())
    }

    /// Sets or clears `minecraft:custom_model_data`.
    pub fn set_custom_model_data(&mut self, value: Option<i32>) {
        self.write_component(
            CUSTOM_MODEL_DATA_COMPONENT,
            value.map(|v| ComponentValue::Int(i64::from(v))),
        );
    }

    /// The plugin-defined item identity this stack carries, if any — the
    /// `lodestone:item_id` tag (issue #147).
    ///
    /// `None` for every vanilla stack, which is what makes this a usable
    /// discriminator: a plugin asking "is this one of mine?" gets a definite no
    /// for anything the server sent that it did not itself tag.
    #[must_use]
    pub fn plugin_item_id(&self) -> Option<Identifier> {
        match self.components.get_str(PLUGIN_ITEM_ID_COMPONENT)? {
            ComponentValue::Str(raw) => raw.parse().ok(),
            _ => None,
        }
    }

    /// Sets or clears the plugin-defined item identity.
    ///
    /// Prefer building stacks through [`crate::custom_item::CustomItem::stack`],
    /// which sets this together with the rest of the definition; this is the
    /// escape hatch for retagging a stack that already exists.
    pub fn set_plugin_item_id(&mut self, id: Option<&Identifier>) {
        self.write_component(
            PLUGIN_ITEM_ID_COMPONENT,
            id.map(|id| ComponentValue::Str(id.to_string())),
        );
    }

    /// Inserts `value` under `key`, or removes the component when `value` is
    /// `None`.
    ///
    /// One place where the "an unparseable key is a silent no-op" behaviour of
    /// the underlying map lives, rather than eleven. Every well-known key in
    /// this module is a valid identifier, so the `Err` arm is unreachable for
    /// them; it exists so a bad key cannot panic a plugin.
    fn write_component(&mut self, key: &str, value: Option<ComponentValue>) {
        let Ok(key) = key.parse::<Identifier>() else {
            return;
        };
        match value {
            Some(value) => {
                self.components.insert(key, value);
            }
            None => {
                self.components.remove(&key);
            }
        }
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

        // Issue #143. Without this branch the dye was dropped at the crate
        // boundary: `entities.rs` reads `stack.components.dyed_color` off the
        // *model* stack for the armour layer, so dyed leather armour rendered
        // correctly on a body while the identical item's GUI icon did not,
        // because the icon path holds a game stack. `hud/item_icon.rs` even
        // documented the absence ("that crate defines no `DYED_COLOR_COMPONENT`")
        // rather than it being an oversight nobody had noticed.
        if let Some(rgb) = stack.components.dyed_color
            && let Ok(key) = DYED_COLOR_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(rgb)));
        }

        // Same crate-boundary loss as the dye above: without this branch a potion,
        // splash potion, lingering potion or tipped arrow's mixed colour never
        // reaches this crate's shape at all.
        if let Some(argb) = stack.components.potion_color
            && let Ok(key) = POTION_COLOR_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(argb)));
        }

        // Same crate-boundary loss as the dye above, and with the same symptom
        // seen from the other side: remote players, mobs and armour stands drew
        // their trims off the *model* stack while the local player — whose stack
        // arrives here — did not.
        if let Some(trim) = stack.components.trim.clone()
            && let Ok(key) = TRIM_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Trim(trim));
        }

        // Without this the renderer cannot tell which saved map a `filled_map`
        // shows, and falls back to the lowest-numbered known map.
        if let Some(id) = stack.components.map_id
            && let Ok(key) = MAP_ID_COMPONENT.parse()
        {
            components.insert(key, ComponentValue::Int(i64::from(id)));
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

impl From<&ItemStack> for lodestone_model::ItemStack {
    /// Lowers a canonical stack back into the model's wire-shaped stack — the
    /// direction that did not exist before issue #143.
    ///
    /// # Why its absence mattered
    ///
    /// There was exactly one game -> model path in the tree,
    /// `lodestone_shell::sim`'s `tool_mining_item`, and it reconstructed a model
    /// stack carrying **only** `minecraft:tool`, zeroing every other component.
    /// Its own doc claimed "the round trip is exact in both directions", which is
    /// true for `tool` and false for everything else. So a plugin that mutated a
    /// stack's components had nowhere to send the result: no conversion existed
    /// to hand it to a renderer keyed on the model type
    /// (`glint::has_foil`, `armour_layer_tint_with_dye`, `ItemTintContext`) or
    /// back toward the wire.
    ///
    /// # What is and is not recoverable
    ///
    /// The five *patch* fields round-trip exactly. Of the rest:
    ///
    /// * `has_unmodeled` is **always `false`** here, and that is honest rather
    ///   than lossy: the flag means "the wire carried a component this build
    ///   could not decode", which is a property of a decode that this stack is
    ///   no longer the product of. A plugin-built stack has no undecoded
    ///   remainder. Note the consequence — a stack that came *off* the wire with
    ///   unmodelled components and is round-tripped through here loses the
    ///   warning, because the forward conversion never carried it in the first
    ///   place (see that impl's doc).
    /// * `max_stack_size` / `max_damage` / `equippable` are the *effective*
    ///   fields, and they do round-trip, because the forward conversion stores
    ///   them. They are prototype-derived rather than patch-derived, so a
    ///   consumer must not treat their presence here as "the wire said so".
    fn from(stack: &ItemStack) -> Self {
        let components = lodestone_model::ItemComponents {
            custom_name: stack.custom_name().cloned(),
            damage: stack.damage().and_then(|d| u32::try_from(d).ok()),
            enchantments: stack.enchantments().to_vec(),
            dyed_color: stack.dyed_color(),
            potion_color: stack.potion_color(),
            trim: stack.trim(),
            map_id: stack.map_id(),
            // This crate's own component map has no `pot_decorations` slot — its
            // `ComponentValue` cannot represent four item keys — so there is
            // nothing to carry across. A decorated pot round-tripped through here
            // loses its sherds, the same way an unmodelled component loses its
            // warning above.
            pot_decorations: None,
            tool: stack.tool(),
            max_stack_size: stack
                .components
                .get_int(MAX_STACK_SIZE_COMPONENT)
                .and_then(|v| u32::try_from(v).ok()),
            max_damage: stack
                .components
                .get_int(MAX_DAMAGE_COMPONENT)
                .and_then(|v| u32::try_from(v).ok()),
            // Read from the component map by *name*, not through
            // `crate::container::equippable_slot` — that returns this crate's own
            // `container::EquipmentSlot`, a **different type** from
            // `lodestone_model::EquipmentSlot` with the same name and the same
            // variants. (Yet another instance of the duplication class issue #143
            // is about; the compiler caught this one.)
            equippable: match stack.components.get_str(EQUIPPABLE_COMPONENT) {
                Some(ComponentValue::Str(name)) => {
                    lodestone_model::EquipmentSlot::from_name(name)
                }
                _ => None,
            },
            // Same story as `pot_decorations` above: this crate's component map
            // has no slot for an opaque NBT blob, so there is nothing to carry.
            custom_data: None,
            // `repair_cost` is server-side-only bookkeeping (see its own doc on
            // `lodestone_model::ItemComponents`) with no slot in this crate's
            // component map — same "nothing to carry" story as `pot_decorations`
            // and `custom_data` above.
            repair_cost: 0,
            // See the doc above: not lossy, out of scope.
            has_unmodeled: false,
        };
        Self {
            item: stack.item.clone(),
            count: u32::try_from(stack.count).unwrap_or(0),
            components,
        }
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

/// `stack`'s hover name, `§`-coded and forced **italic** when it carries a
/// custom name — the exact text/style the held-item name highlight
/// ([`crate::player_state::HeldItemHighlight`], issue #126) draws, and, once
/// an item tooltip lands, what its title line would reuse.
///
/// Mirrors `Hud.extractSelectedItemName`
/// (`Hud.java:625-648` in the 26.2 client):
///
/// ```java
/// MutableComponent str = Component.empty().append(this.lastToolHighlight.getHoverName())
///     .withStyle(this.lastToolHighlight.getRarity().color());
/// if (this.lastToolHighlight.has(DataComponents.CUSTOM_NAME)) {
///     str.withStyle(ChatFormatting.ITALIC);
/// }
/// ```
///
/// Two narrower gaps than #117's, both because the data does not exist in
/// this build yet rather than because the draw side drops it:
///
/// * **No rarity colour.** `ItemStack` here carries no rarity data (no
///   `minecraft:rarity` component, and no per-item default-rarity table), so
///   every name draws in the caller's base colour (white) instead of
///   vanilla's common/uncommon/rare/epic tint. The overwhelming majority of
///   items are common (white) anyway, so this is right far more often than
///   wrong.
/// * **No `item.minecraft.*` translation table wired to this call.** There is
///   no existing "resolve an item's display name" path anywhere in this tree
///   (checked before writing this — the issue's claim that one exists to
///   reuse was stale). [`base_display_name`] does the best available
///   approximation: try `item.minecraft.<path>`, then `block.minecraft.<path>`
///   (vanilla's own two `descriptionId` families — `Item.java:634-645`, a
///   plain `Item` defaults to the former, a `BlockItem` to the latter, and
///   this build has no per-item classification of which is which), then a
///   humanised fallback so an unresolvable id still reads as words rather
///   than a raw snake_case key.
#[must_use]
pub fn styled_hover_name(stack: &ItemStack, translate: &dyn Fn(&str) -> Option<String>) -> String {
    let custom = match stack.components().get_str(CUSTOM_NAME_COMPONENT) {
        Some(ComponentValue::Text(text)) => Some(text.clone()),
        _ => None,
    };
    let hover = custom
        .clone()
        .unwrap_or_else(|| Text::literal(base_display_name(stack.item(), translate)));
    let mut root = Text::literal(String::new());
    if custom.is_some() {
        root.style.italic = Some(true);
    }
    root.extra.push(hover);
    root.to_legacy_string()
}

/// The best-effort plain display name for `item` with no custom-name
/// override — see [`styled_hover_name`]'s docs for exactly what this
/// approximates and why.
#[must_use]
pub fn base_display_name(item: &Identifier, translate: &dyn Fn(&str) -> Option<String>) -> String {
    let path = item.path();
    if let Some(name) = translate(&format!("item.minecraft.{path}")) {
        return name;
    }
    if let Some(name) = translate(&format!("block.minecraft.{path}")) {
        return name;
    }
    humanize_path(path)
}

/// `"diamond_sword"` -> `"Diamond Sword"`: the last-resort fallback when
/// neither translation key resolves.
fn humanize_path(path: &str) -> String {
    path.split(['_', '/'])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    fn no_translation(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn plain_item_resolves_via_the_item_translation_key() {
        let stack = ItemStack::new(id("minecraft:diamond_sword"), 1);
        let translate = |key: &str| {
            (key == "item.minecraft.diamond_sword").then(|| "Diamond Sword".to_owned())
        };
        assert_eq!(styled_hover_name(&stack, &translate), "Diamond Sword");
    }

    #[test]
    fn block_item_falls_back_to_the_block_translation_key() {
        let stack = ItemStack::new(id("minecraft:stone"), 1);
        let translate =
            |key: &str| (key == "block.minecraft.stone").then(|| "Stone".to_owned());
        assert_eq!(styled_hover_name(&stack, &translate), "Stone");
    }

    #[test]
    fn unresolvable_id_humanises_to_title_case() {
        let stack = ItemStack::new(id("minecraft:totally_unknown_item"), 1);
        assert_eq!(
            styled_hover_name(&stack, &no_translation),
            "Totally Unknown Item"
        );
    }

    #[test]
    fn custom_named_item_is_forced_italic_and_keeps_its_own_text() {
        let mut stack = ItemStack::new(id("minecraft:diamond_sword"), 1);
        let key: Identifier = CUSTOM_NAME_COMPONENT.parse().unwrap();
        stack
            .components_mut()
            .insert(key, ComponentValue::Text(Text::literal("Excalibur")));
        // `§o` is vanilla's italic legacy code — forced on by
        // `has(DataComponents.CUSTOM_NAME)`, not carried by the custom name
        // text itself (which here is a bare literal with no style).
        assert_eq!(
            styled_hover_name(&stack, &no_translation),
            "§oExcalibur"
        );
    }

    #[test]
    fn plain_item_is_never_forced_italic() {
        let stack = ItemStack::new(id("minecraft:diamond_sword"), 1);
        let translate = |key: &str| {
            (key == "item.minecraft.diamond_sword").then(|| "Diamond Sword".to_owned())
        };
        let name = styled_hover_name(&stack, &translate);
        assert!(!name.contains('\u{a7}'), "no format codes expected: {name}");
    }

    // -- Issue #143: the component read/write seam ---------------------

    /// The bug the `dyed_color` branch fixes, pinned. Before it, a dyed model
    /// stack converted to a game stack with **no** dye component at all, so the
    /// GUI icon path could not see it while the armour path (which reads the
    /// model stack) could.
    #[test]
    fn dyed_color_survives_the_model_to_game_conversion() {
        let model = lodestone_model::ItemStack {
            item: id("minecraft:leather_chestplate"),
            count: 1,
            components: ModelItemComponents {
                dyed_color: Some(0x00_A0_40_20),
                ..ModelItemComponents::default()
            },
        };
        let game = ItemStack::from(&model);
        assert_eq!(
            game.dyed_color(),
            Some(0x00_A0_40_20),
            "the dye must cross the crate boundary"
        );
        assert!(
            game.components().get_str(DYED_COLOR_COMPONENT).is_some(),
            "and must be stored under the canonical key, not a private one"
        );
    }

    /// The control for the above: an *undyed* leather item gets no dye
    /// component, so the assertion above is measuring the dye rather than a
    /// component that is always present.
    #[test]
    fn control_an_undyed_item_gains_no_dye_component() {
        let model = lodestone_model::ItemStack::new(id("minecraft:leather_chestplate"), 1);
        let game = ItemStack::from(&model);
        assert_eq!(game.dyed_color(), None);
        assert!(game.components().get_str(DYED_COLOR_COMPONENT).is_none());
    }

    /// The round trip that had no reverse leg before this issue.
    ///
    /// Deliberately populates every modelled component at once — a per-field
    /// test would pass while the whole-struct lowering dropped a neighbour, which
    /// is precisely how `dyed_color` went missing in the forward direction.
    #[test]
    fn every_modelled_component_survives_a_game_model_round_trip() {
        let tool = ItemTool::new(
            vec![ToolRule::new(
                ToolBlocks::Tag(id("minecraft:mineable/pickaxe")),
                Some(8.0),
                Some(true),
            )],
            1.0,
            1,
            true,
        );
        let original = lodestone_model::ItemStack {
            item: id("minecraft:diamond_pickaxe"),
            count: 3,
            components: ModelItemComponents {
                custom_name: Some(Text::literal("Excalibur")),
                damage: Some(37),
                enchantments: vec![ItemEnchantment { id: 12, level: 4 }],
                dyed_color: Some(0x00_11_22_33),
                potion_color: Some(0xFF_38_5D_C6),
                trim: Some(ArmorTrim {
                    material: "netherite".to_string(),
                    pattern: "silence".to_string(),
                }),
                map_id: Some(1701),
                // Not round-tripped by design: this crate's component map has no
                // slot for four item keys, so the conversion sets `None` either
                // way and a `Some` here would fail the round-trip assertion for
                // the wrong reason.
                pot_decorations: None,
                tool: ToolPatch::Set(tool),
                max_stack_size: Some(1),
                max_damage: Some(1561),
                equippable: Some(lodestone_model::EquipmentSlot::Head),
                // Not round-tripped by design, same reason as `pot_decorations`
                // above: this crate's `ComponentValue` has no opaque-blob slot, so
                // the conversion sets `None` either way.
                custom_data: None,
                // Server-side-only bookkeeping (see its own doc on
                // `lodestone_model::ItemComponents`); not round-tripped for the
                // same reason as `pot_decorations`/`custom_data` above.
                repair_cost: 0,
                has_unmodeled: false,
            },
        };

        let game = ItemStack::from(&original);
        let back = lodestone_model::ItemStack::from(&game);

        assert_eq!(back, original, "the round trip must be exact");
    }

    /// A plugin's typed writes land in the same component map the decoder writes,
    /// so they are indistinguishable downstream — the property that makes the
    /// write half of #143 real rather than a parallel store.
    #[test]
    fn a_plugin_write_is_indistinguishable_from_a_decoded_component() {
        let decoded = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:leather_boots"),
            count: 1,
            components: ModelItemComponents {
                dyed_color: Some(0x00_DE_AD_BE),
                custom_name: Some(Text::literal("Swift")),
                ..ModelItemComponents::default()
            },
        });

        let mut built = ItemStack::new(id("minecraft:leather_boots"), 1);
        built.set_dyed_color(Some(0x00_DE_AD_BE));
        built.set_custom_name(Some(Text::literal("Swift")));

        assert_eq!(
            built, decoded,
            "a plugin-built stack must compare equal to the decoded one"
        );
        // And therefore stack with it, which is the observable consequence.
        assert!(ItemStack::is_same_item_same_components(&built, &decoded));
    }

    /// Clearing a component removes it rather than storing a zero, so a cleared
    /// stack merges with one that never had the component.
    #[test]
    fn clearing_a_component_removes_it_rather_than_zeroing_it() {
        let mut stack = ItemStack::new(id("minecraft:leather_boots"), 1);
        stack.set_dyed_color(Some(0x00_FF_00_00));
        stack.set_custom_name(Some(Text::literal("temp")));
        stack.set_enchantments(vec![ItemEnchantment { id: 22, level: 1 }]);
        assert_eq!(stack.components().len(), 3);

        stack.set_dyed_color(None);
        stack.set_custom_name(None);
        stack.set_enchantments(Vec::new());

        assert!(
            stack.components().is_empty(),
            "left {:?}",
            stack.components().iter().collect::<Vec<_>>()
        );
        assert_eq!(stack, ItemStack::new(id("minecraft:leather_boots"), 1));
    }

    /// `Inherited` is not a value: setting it removes the component, and an
    /// absent component reads back as `Inherited`. Getting this backwards is what
    /// makes every real pickaxe mine at fist speed.
    #[test]
    fn an_absent_tool_component_reads_as_inherited_and_back() {
        let mut stack = ItemStack::new(id("minecraft:diamond_pickaxe"), 1);
        assert!(matches!(stack.tool(), ToolPatch::Inherited));
        assert!(stack.components().is_empty());

        stack.set_tool(ToolPatch::Removed);
        assert!(matches!(stack.tool(), ToolPatch::Removed));
        assert_eq!(stack.components().len(), 1);

        stack.set_tool(ToolPatch::Inherited);
        assert!(
            stack.components().is_empty(),
            "Inherited must remove the component, not store a third value"
        );
        assert!(matches!(stack.tool(), ToolPatch::Inherited));
    }

    /// The enchantment read the shell could not previously do — the reason
    /// `glint::has_foil_enchantments` exists as a sibling of `has_foil`.
    #[test]
    fn enchantments_are_readable_off_a_game_stack() {
        let stack = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:diamond_sword"),
            count: 1,
            components: ModelItemComponents {
                enchantments: vec![ItemEnchantment { id: 5, level: 5 }],
                ..ModelItemComponents::default()
            },
        });
        assert_eq!(stack.enchantments().len(), 1);
        assert_eq!(stack.enchantments()[0].level, 5);
        // The model-typed glint predicate is now reachable from a game stack via
        // the lowering, which is what closes the gap the sibling function papered
        // over.
        let lowered = lodestone_model::ItemStack::from(&stack);
        assert!(!lowered.components.enchantments.is_empty());
    }

    /// Control for the above: an unenchanted stack reports no enchantments, so
    /// the assertion is not satisfied by a non-empty default.
    #[test]
    fn control_an_unenchanted_stack_has_no_enchantments() {
        let stack = ItemStack::new(id("minecraft:diamond_sword"), 1);
        assert!(stack.enchantments().is_empty());
        let lowered = lodestone_model::ItemStack::from(&stack);
        assert!(lowered.components.enchantments.is_empty());
    }
}
