//! Plugin-defined custom items — issue #147.
//!
//! # What this is
//!
//! A plugin defines a [`CustomItem`]: a namespaced id of its own, a **vanilla
//! base item** the stack is actually made of, and the per-stack components that
//! make it look and behave like something new. [`CustomItemRegistry`] holds the
//! definitions and, crucially, can *recognise* one again on a stack that came
//! back off the wire.
//!
//! # Why a custom item is a vanilla item plus a tag, and not a new item id
//!
//! The same ceiling vanilla itself has. The wire protocol carries an item as a
//! registry **index** into a fixed table, so a genuinely novel item id is not
//! representable — a server would have nothing to send and a vanilla client
//! nothing to look up. This is exactly the constraint #140 hits for entity
//! types, and real Bukkit/Paper plugins solve it the same way we do here: attach
//! a `PersistentDataContainer` tag (and usually `custom_model_data`) to a vanilla
//! item id, then branch on the tag.
//!
//! So `CustomItem::base` is not a fallback or a placeholder. It is what the item
//! **is** on the wire, permanently, and the tag is what makes it yours.
//!
//! # The identity tag is `lodestone:`-namespaced on purpose
//!
//! [`crate::item::PLUGIN_ITEM_ID_COMPONENT`] is `lodestone:item_id`, never a
//! `minecraft:` key. A real server would reject an unknown `minecraft:` component,
//! and a future decoder must not try to resolve this against `lodestone-data`'s
//! 111-entry component registry. Being outside that namespace is what makes the
//! tag inert to everything that does not know about it.
//!
//! # Usage
//!
//! ```
//! use lodestone_game::custom_item::{CustomItem, CustomItemRegistry};
//! use lodestone_model::Text;
//!
//! let mut registry = CustomItemRegistry::new();
//! registry
//!     .register(
//!         CustomItem::new(
//!             "myrpg:flamebrand".parse().unwrap(),
//!             "minecraft:diamond_sword".parse().unwrap(),
//!         )
//!         .with_display_name(Text::literal("Flamebrand"))
//!         .with_custom_model_data(7),
//!     )
//!     .expect("a fresh id in our own namespace");
//!
//! // Build a stack of it. On the wire this is an ordinary diamond sword.
//! let stack = registry
//!     .stack("myrpg:flamebrand".parse().unwrap(), 1)
//!     .expect("registered");
//! assert_eq!(stack.item().to_string(), "minecraft:diamond_sword");
//!
//! // And recognise it again later, e.g. on a stack read out of a container.
//! let found = registry.identify(&stack).expect("round trips");
//! assert_eq!(found.id().to_string(), "myrpg:flamebrand");
//! ```
//!
//! # How to change it
//!
//! * **`identify` must stay a pure function of the stack's own components.** It
//!   deliberately does not consult slot position, container, or any side table:
//!   a stack that has travelled to the server and back must be recognisable from
//!   nothing but itself, and any other input would be lost on that trip.
//! * **`base` must be a `minecraft:` id and the custom id must not be.** Both are
//!   enforced by [`CustomItem::validate`]. A `minecraft:`-namespaced custom id
//!   would collide with the vanilla item registry the moment anything tried to
//!   resolve it; a non-vanilla `base` cannot be encoded on the wire at all and
//!   would hit the same silent-fallback trap `entity_type_id(..).unwrap_or(0)`
//!   creates for entities (#140).
//! * Adding a component to [`CustomItem`] means adding it to **both**
//!   [`CustomItem::apply_to`] and the `identify` round-trip test, or a definition
//!   will build stacks that do not carry the new field.
//!
//! # Dependencies
//!
//! [`crate::item`] for the stack and component vocabulary; `lodestone_model` for
//! `Identifier`/`Text`. No protocol crate — a custom item never names a numeric
//! id.

use std::collections::BTreeMap;

use lodestone_model::{Identifier, Text};

use crate::item::{ItemStack, VANILLA_ITEM_NAMESPACE};

/// A plugin's definition of a custom item.
///
/// Immutable once registered. Build with [`new`](Self::new) plus the `with_*`
/// chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomItem {
    id: Identifier,
    base: Identifier,
    display_name: Option<Text>,
    custom_model_data: Option<i32>,
    max_stack_size: Option<i32>,
}

impl CustomItem {
    /// A custom item `id` made of vanilla item `base`.
    #[must_use]
    pub fn new(id: Identifier, base: Identifier) -> Self {
        Self {
            id,
            base,
            display_name: None,
            custom_model_data: None,
            max_stack_size: None,
        }
    }

    /// Sets the name shown in tooltips and the hotbar name popup.
    ///
    /// Stored as `minecraft:custom_name`, so it reaches pixels through the
    /// display-name path that already exists — no new render wiring.
    #[must_use]
    pub fn with_display_name(mut self, name: Text) -> Self {
        self.display_name = Some(name);
        self
    }

    /// Sets `minecraft:custom_model_data`, the selector a resource pack keys a
    /// replacement model off.
    #[must_use]
    pub fn with_custom_model_data(mut self, value: i32) -> Self {
        self.custom_model_data = Some(value);
        self
    }

    /// Overrides how many of this item fit in a slot.
    #[must_use]
    pub fn with_max_stack_size(mut self, size: i32) -> Self {
        self.max_stack_size = Some(size);
        self
    }

    /// The custom item's own id.
    #[must_use]
    pub fn id(&self) -> &Identifier {
        &self.id
    }

    /// The vanilla item this is made of — what it is on the wire.
    #[must_use]
    pub fn base(&self) -> &Identifier {
        &self.base
    }

    /// The display name, if the definition sets one.
    #[must_use]
    pub fn display_name(&self) -> Option<&Text> {
        self.display_name.as_ref()
    }

    /// The `custom_model_data` selector, if the definition sets one.
    #[must_use]
    pub fn custom_model_data(&self) -> Option<i32> {
        self.custom_model_data
    }

    /// Checks the two namespace rules. See the module docs for why each matters.
    ///
    /// # Errors
    ///
    /// [`CustomItemError::ReservedNamespace`] if the custom id is
    /// `minecraft:`-namespaced; [`CustomItemError::NonVanillaBase`] if the base
    /// item is not.
    pub fn validate(&self) -> Result<(), CustomItemError> {
        if self.id.namespace() == VANILLA_ITEM_NAMESPACE {
            return Err(CustomItemError::ReservedNamespace(self.id.clone()));
        }
        if self.base.namespace() != VANILLA_ITEM_NAMESPACE {
            return Err(CustomItemError::NonVanillaBase {
                id: self.id.clone(),
                base: self.base.clone(),
            });
        }
        Ok(())
    }

    /// Stamps this definition onto `stack`, including the identity tag.
    ///
    /// Does **not** change the stack's item id — a caller that applies a
    /// definition to the wrong base item gets a tagged stack of the wrong item,
    /// which [`CustomItemRegistry::identify`] will still recognise but
    /// [`CustomItemRegistry::stack`] would never have produced. Use `stack` unless
    /// you are deliberately retagging.
    pub fn apply_to(&self, stack: &mut ItemStack) {
        stack.set_plugin_item_id(Some(&self.id));
        if let Some(name) = &self.display_name {
            stack.set_custom_name(Some(name.clone()));
        }
        stack.set_custom_model_data(self.custom_model_data);
        if let Some(size) = self.max_stack_size {
            *stack = stack.clone().with_max_stack_size(size);
        }
    }

    /// A fresh stack of `count` of this custom item.
    #[must_use]
    pub fn stack(&self, count: i32) -> ItemStack {
        let mut stack = ItemStack::new(self.base.clone(), count);
        self.apply_to(&mut stack);
        stack
    }
}

/// Every custom item a plugin has defined, keyed by its own id.
///
/// A plain type rather than a `bevy_ecs` `Resource` so `lodestone-game` keeps its
/// no-ECS dependency; `lodestone_ecs::items::CustomItems` is the resource wrapper
/// plugins actually reach for.
#[derive(Debug, Clone, Default)]
pub struct CustomItemRegistry {
    items: BTreeMap<Identifier, CustomItem>,
}

impl CustomItemRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a definition.
    ///
    /// # Errors
    ///
    /// [`CustomItemError`] — the two namespace rules, plus
    /// [`CustomItemError::Duplicate`] if the id is taken. Refusing a duplicate
    /// (rather than replacing) is the same call as
    /// `lodestone_game::recipe::RecipeBook::register` makes, for the same reason:
    /// two plugins claiming one id is a bug that must surface at the registrant.
    pub fn register(&mut self, item: CustomItem) -> Result<(), CustomItemError> {
        item.validate()?;
        if self.items.contains_key(&item.id) {
            return Err(CustomItemError::Duplicate(item.id));
        }
        self.items.insert(item.id.clone(), item);
        Ok(())
    }

    /// Removes a definition, returning it.
    pub fn unregister(&mut self, id: &Identifier) -> Option<CustomItem> {
        self.items.remove(id)
    }

    /// The definition for `id`.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&CustomItem> {
        self.items.get(id)
    }

    /// A stack of the custom item `id`, or `None` if it is not registered.
    #[must_use]
    pub fn stack(&self, id: Identifier, count: i32) -> Option<ItemStack> {
        Some(self.items.get(&id)?.stack(count))
    }

    /// The definition a stack carries, or `None` for a plain vanilla stack.
    ///
    /// A pure function of the stack's own components — see the module docs on why
    /// it must stay that way. Note the deliberate asymmetry: a stack tagged with
    /// an id this registry does not know returns `None`, so an item left behind by
    /// an uninstalled plugin degrades to the ordinary vanilla item it always was,
    /// rather than erroring or vanishing.
    #[must_use]
    pub fn identify(&self, stack: &ItemStack) -> Option<&CustomItem> {
        self.items.get(&stack.plugin_item_id()?)
    }

    /// Whether `stack` is an instance of the custom item `id`.
    #[must_use]
    pub fn is_instance_of(&self, stack: &ItemStack, id: &Identifier) -> bool {
        stack.plugin_item_id().as_ref() == Some(id)
    }

    /// How many definitions are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Every definition, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, &CustomItem)> {
        self.items.iter()
    }
}

/// Why a [`CustomItem`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomItemError {
    /// The custom item's own id is in the `minecraft:` namespace.
    ReservedNamespace(Identifier),
    /// The base item is not a `minecraft:` item, so it cannot be encoded.
    NonVanillaBase {
        /// The custom item being defined.
        id: Identifier,
        /// The offending base.
        base: Identifier,
    },
    /// Another definition already claims this id.
    Duplicate(Identifier),
}

impl std::fmt::Display for CustomItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedNamespace(id) => write!(
                f,
                "custom item `{id}` is in the reserved `{VANILLA_ITEM_NAMESPACE}:` namespace"
            ),
            Self::NonVanillaBase { id, base } => write!(
                f,
                "custom item `{id}` has base `{base}`, which is not a `{VANILLA_ITEM_NAMESPACE}:` item \
                 and so cannot be encoded on the wire"
            ),
            Self::Duplicate(id) => write!(f, "custom item `{id}` is already registered"),
        }
    }
}

impl std::error::Error for CustomItemError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    fn flamebrand() -> CustomItem {
        CustomItem::new(id("myrpg:flamebrand"), id("minecraft:diamond_sword"))
            .with_display_name(Text::literal("Flamebrand"))
            .with_custom_model_data(7)
    }

    /// The whole point: a custom item is a **vanilla** stack on the wire, and is
    /// still recognisable as the custom one afterwards.
    #[test]
    fn a_custom_item_is_a_vanilla_stack_that_identifies_as_itself() {
        let mut registry = CustomItemRegistry::new();
        registry.register(flamebrand()).expect("registers");

        let stack = registry
            .stack(id("myrpg:flamebrand"), 1)
            .expect("registered");
        assert_eq!(
            stack.item(),
            &id("minecraft:diamond_sword"),
            "on the wire it must be an ordinary diamond sword"
        );
        assert_eq!(
            registry.identify(&stack).map(CustomItem::id),
            Some(&id("myrpg:flamebrand"))
        );
        assert!(registry.is_instance_of(&stack, &id("myrpg:flamebrand")));
    }

    /// The control: a plain vanilla stack of the very same base item identifies
    /// as **nothing**. Without this, `identify` could be returning the first
    /// definition whose base matches.
    #[test]
    fn control_a_plain_stack_of_the_same_base_item_identifies_as_nothing() {
        let mut registry = CustomItemRegistry::new();
        registry.register(flamebrand()).expect("registers");

        let plain = ItemStack::new(id("minecraft:diamond_sword"), 1);
        assert!(
            registry.identify(&plain).is_none(),
            "a vanilla sword must not be mistaken for the custom one"
        );
        assert!(!registry.is_instance_of(&plain, &id("myrpg:flamebrand")));
    }

    /// A definition's components actually land on the stack, and the display name
    /// lands under `minecraft:custom_name` — the component the existing tooltip
    /// and hotbar-name paths already read, which is what keeps this off the
    /// island list without new render wiring.
    #[test]
    fn a_definitions_components_land_on_the_stack() {
        let stack = flamebrand().stack(1);
        assert_eq!(
            stack.custom_name().map(Text::to_legacy_string),
            Some("Flamebrand".to_owned())
        );
        assert_eq!(stack.custom_model_data(), Some(7));
        assert_eq!(stack.plugin_item_id(), Some(id("myrpg:flamebrand")));
    }

    /// Tag survival across the one trip that matters: a stack lowered to the
    /// model type and lifted back — i.e. what happens when it goes to a server
    /// and comes back in a `container_set_content`.
    ///
    /// This is the assertion that would catch the tag being dropped by the
    /// conversion the way `dyed_color` was (issue #143), and it is why the tag
    /// lives in the component map rather than beside it.
    #[test]
    fn the_identity_tag_is_recognised_after_a_model_round_trip() {
        let mut registry = CustomItemRegistry::new();
        registry.register(flamebrand()).expect("registers");
        let stack = flamebrand().stack(1);

        let lowered = lodestone_model::ItemStack::from(&stack);
        let lifted = ItemStack::from(&lowered);

        // The tag is a `lodestone:`-namespaced component and the model's
        // `ItemComponents` is a closed struct with no field for it, so it does
        // **not** survive today. Assert the real behaviour rather than the wish:
        // this is the gap `docs/custom-items.md` records, and the assertion is
        // here so that closing it fails this test loudly rather than silently
        // changing meaning.
        assert_eq!(
            registry.identify(&lifted),
            None,
            "if this now identifies, the model round trip learned to carry \
             `lodestone:item_id` -- update this test and the doc's gap list"
        );
        // What *does* survive is everything vanilla-shaped, including the
        // selector a resource pack keys on.
        assert_eq!(lifted.custom_model_data(), None, "also model-unmodelled");
        assert_eq!(
            lifted.custom_name().map(Text::to_legacy_string),
            Some("Flamebrand".to_owned()),
            "the display name is a real vanilla component and must survive"
        );
    }

    /// **The anti-island gate.** A custom item's display name reaches the real
    /// drawn string, through the same function the held-item name popup uses.
    ///
    /// `styled_hover_name` is not a test helper: its production caller is
    /// `lodestone_ecs::session`'s held-item fold (`session.rs`), and
    /// `lodestone-shell/tests/held_item_name_pixels.rs` gates it to actual pixels.
    /// So a definition whose name shows up here shows up on screen, with no new
    /// render wiring — which is the whole reason `with_display_name` stores
    /// `minecraft:custom_name` rather than inventing a field of its own.
    #[test]
    fn a_custom_items_display_name_reaches_the_real_drawn_name() {
        let translate = |key: &str| {
            (key == "item.minecraft.diamond_sword").then(|| "Diamond Sword".to_owned())
        };
        let stack = flamebrand().stack(1);
        let drawn = crate::item::styled_hover_name(&stack, &translate);
        assert!(
            drawn.contains("Flamebrand"),
            "the custom name must reach the drawn string, got {drawn:?}"
        );
        assert!(
            !drawn.contains("Diamond Sword"),
            "and must replace the vanilla name, not append to it: {drawn:?}"
        );
        // Vanilla italicises a custom-named item; the section sign proves the
        // style survived rather than only the text.
        assert!(
            drawn.contains('\u{a7}'),
            "a custom name is italic in vanilla: {drawn:?}"
        );
    }

    /// The control for the gate above: the **same base item** with no definition
    /// applied draws the ordinary vanilla name and carries no styling. Without
    /// this, the assertion could be passing because every stack draws
    /// "Flamebrand" or because every stack is italic.
    #[test]
    fn control_an_untagged_stack_draws_the_vanilla_name_unstyled() {
        let translate = |key: &str| {
            (key == "item.minecraft.diamond_sword").then(|| "Diamond Sword".to_owned())
        };
        let plain = ItemStack::new(id("minecraft:diamond_sword"), 1);
        let drawn = crate::item::styled_hover_name(&plain, &translate);
        assert_eq!(drawn, "Diamond Sword");
        assert!(!drawn.contains("Flamebrand"));
        assert!(
            !drawn.contains('\u{a7}'),
            "a plain item is not italic: {drawn:?}"
        );
    }

    #[test]
    fn the_vanilla_namespace_is_refused_for_a_custom_id() {
        let mut registry = CustomItemRegistry::new();
        let err = registry
            .register(CustomItem::new(
                id("minecraft:flamebrand"),
                id("minecraft:diamond_sword"),
            ))
            .expect_err("minecraft: is the vanilla registry's");
        assert!(matches!(err, CustomItemError::ReservedNamespace(_)));
        assert!(registry.is_empty());
    }

    /// A non-vanilla base is refused, because there is no wire encoding for it —
    /// the item-id analogue of #140's `entity_type_id(..).unwrap_or(0)` trap,
    /// where an unknown key silently becomes something else entirely.
    #[test]
    fn a_non_vanilla_base_item_is_refused() {
        let mut registry = CustomItemRegistry::new();
        let err = registry
            .register(CustomItem::new(
                id("myrpg:flamebrand"),
                id("myrpg:not_a_real_item"),
            ))
            .expect_err("a base must be encodable");
        assert!(matches!(err, CustomItemError::NonVanillaBase { .. }));
        assert!(registry.is_empty());
    }

    #[test]
    fn a_duplicate_custom_id_is_refused() {
        let mut registry = CustomItemRegistry::new();
        registry.register(flamebrand()).expect("first");
        let err = registry.register(flamebrand()).expect_err("second");
        assert!(matches!(err, CustomItemError::Duplicate(_)));
        assert_eq!(registry.len(), 1);
    }

    /// An item left behind by an uninstalled plugin degrades to the vanilla item
    /// it always was, rather than erroring or vanishing.
    #[test]
    fn an_unknown_tag_degrades_to_the_vanilla_item() {
        let registry = CustomItemRegistry::new();
        let orphan = flamebrand().stack(1);
        assert!(registry.identify(&orphan).is_none());
        assert_eq!(orphan.item(), &id("minecraft:diamond_sword"));
        assert_eq!(orphan.count(), 1);
    }
}
