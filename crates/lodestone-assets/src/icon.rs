//! Turning an item into a drawable 2-D inventory icon.
//!
//! An inventory renderer, given an `ItemStack`, needs to know *what to draw in a
//! slot*. Since 1.21.4 that answer is spread across two files: the item
//! *definition* (`assets/<ns>/items/<id>.json`, a selector tree resolved against
//! runtime stack state) names a model, and the model (`assets/<ns>/models/...`)
//! says whether it is a flat sprite, a 3-D model, or a code-driven special
//! renderer. This module joins the two and emits a GPU-free [`ItemIcon`] the
//! renderer consumes.
//!
//! The three drawable shapes, faithful to `net/minecraft/client/renderer/item`
//! in the decompiled 26.2 client:
//!
//! * **Sprite** — the `builtin/generated` path, ~97% of items. A stack of item-
//!   atlas sprites (`layer0`..`layer4`), each with an optional tint. The renderer
//!   draws each as a screen-aligned quad. This is the cheap path, and it is why
//!   this module emits **sprite references** rather than geometry for it: a 2-D
//!   inventory slot never sees the extruded sides.
//! * **Model** — a block or genuine 3-D item model, drawn under the model's
//!   `display.gui` transform (the isometric-looking pose). This module emits the
//!   model reference plus that transform and the GUI lighting mode; the renderer
//!   bakes the geometry with the existing [`crate::bake_model`] against its atlas.
//!   Display transforms are render-time pose-stack ops in vanilla, not baked into
//!   block-local quads, so they are carried, not applied here.
//!
//! The same geometry is also what the world draws for a **dropped**, **held** or
//! **worn** item, under a different `display` slot each time, so
//! [`ItemIcon::display`] carries all nine of vanilla's slots and not just `gui`.
//! It used to carry only `gui`, which is why `lodestone_render`'s dropped-item
//! path had to name `block/block`'s `ground` numbers as constants.
//! * **Special** — the ex-`builtin/entity` items (chest, shulker box, banner,
//!   shield, …) that vanilla draws with a dedicated block-entity renderer. These
//!   *only* surface through the `items/*.json` definition (their `models/item`
//!   entry is now an empty shell), which is the concrete reason this pipeline is
//!   keyed on item definitions rather than models. The icon carries the special
//!   `kind` and a `base` sprite model the renderer can fall back to.
//!
//! A `composite` item model yields several parts, drawn back-to-front; an
//! `empty` model (or `builtin/empty`) yields none, and [`ItemIcon::is_drawable`]
//! reports `false`. Resolution is pure over the definition tree (data) and an
//! [`ItemPropertyContext`] the game supplies; [`DefaultItemContext`] gives the
//! default inventory appearance a fresh stack shows.
//!
//! A handful of items (`spyglass`, `trident`, the spears, every bundle) branch
//! on `minecraft:display_context` at the top of their definition, with a `gui`
//! case naming the flat inventory sprite and the *fallback* naming the in-hand
//! held model. [`DefaultItemContext`] never answers a `select`, so resolving
//! through it silently takes that fallback — the wrong (in-hand) model — for
//! those items. [`GuiItemContext`] is [`DefaultItemContext`] plus
//! `minecraft:display_context -> "gui"`, and is what an inventory/GUI-slot
//! resolution (e.g. [`crate::item_atlas::ItemAtlas`]) should use instead.
//!
//! **But pinning the GUI context is only right for a GUI slot**, and this module
//! used to be resolved once, at load, under that pin — so the *inventory* form
//! was the only form baked, and a spyglass in the hand drew the flat sprite
//! rather than `item/spyglass_in_hand`. [`DisplayContextItemContext`] answers the
//! same property for any of vanilla's nine contexts, and
//! [`ItemIconBuilder::definition`] / [`ItemModel::outputs`] /
//! [`ItemIconBuilder::part_for_model`] together let a baker enumerate and build
//! *every* form up front instead of one. See `docs/item-variants.md`.

use crate::error::IconError;
use crate::item::LAYER_NAMES;
use crate::item_model::{ItemModel, ItemModelOutput, ItemPropertyContext, TintSource};
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::model::{
    DisplaySlot, DisplayTransform, DisplayTransforms, GuiLight, ModelResolver, TextureBinding,
};

/// One layer of a flat (generated) icon: an item-atlas sprite plus its tint.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteLayer {
    /// The sprite's resource location (e.g. `minecraft:item/diamond_sword`). The
    /// renderer resolves it against the item atlas.
    pub sprite: ResourceLocation,
    /// The tint applied to this layer, or `None` for an untinted layer. The
    /// discriminant and constant default are carried; the game evaluates the
    /// live colour (dye, grass, …).
    pub tint: Option<TintSource>,
}

/// One drawable part of an [`ItemIcon`]. Usually an icon has exactly one part; a
/// `composite` item model produces several, drawn in order (back to front).
#[derive(Debug, Clone, PartialEq)]
pub enum IconPart {
    /// A flat generated sprite stack. Draw each layer as a screen-aligned quad,
    /// applying its tint. The overwhelming-majority, cheap path.
    Sprite {
        /// The layers, `layer0` first.
        layers: Vec<SpriteLayer>,
    },
    /// A 3-D block/item model drawn under the GUI display transform. The renderer
    /// bakes `model` with [`crate::bake_model`] against its atlas, then applies
    /// `transform` (a render-time pose, not baked in) and shades per `gui_light`.
    Model {
        /// The model to bake (e.g. `minecraft:block/stone`).
        model: ResourceLocation,
        /// The `display.gui` transform from the model JSON (the isometric pose),
        /// or the identity transform when the model omits one.
        ///
        /// Exactly `ItemIcon::display.get(DisplaySlot::Gui)`. The other eight
        /// slots — `ground`, `thirdperson_righthand`, `head`, … — live on
        /// [`ItemIcon::display`] rather than here; see that field for why they
        /// are not per-part.
        transform: DisplayTransform,
        /// The GUI lighting mode (side-lit for blocks, front-lit for flat items).
        gui_light: GuiLight,
        /// The `minecraft:model` **item-definition-tree** node's own
        /// root-to-node `"transformation"` chain — [`ItemModelOutput::Model`]'s
        /// field of the same name, carried through unchanged. Distinct from
        /// [`Self::Model::transform`] above: that one is the *model JSON's*
        /// own `display.gui` slot (read once, per model, Euler-angle based);
        /// this one is the *item definition's* selector-tree-level TRS chain
        /// (1.21.4+, quaternion based), composed **on top of** whatever pose
        /// a caller already builds from `transform` — see
        /// [`crate::item_model::ItemNodeTransform`]'s doc for the exact
        /// composition order. Empty for the overwhelming majority of items;
        /// every coloured bed's `foot` sub-model is the real, shipped case
        /// (see [`crate::item_model::ItemModelNode::Model`]'s own doc for the
        /// jar-wide count).
        node_transformation: Vec<crate::item_model::ItemNodeTransform>,
    },
    /// A code-driven special renderer (chest, shulker box, banner, shield, …) —
    /// ten `kind`s over 91 item definitions, and the whole family has **no item
    /// model and no block model** in vanilla: every triangle comes from a
    /// block-entity renderer.
    ///
    /// # Two corrections to what this doc used to say
    ///
    /// It read: *"The renderer's block-entity path draws it; `base` is a real item
    /// model it can fall back to as a flat sprite."* Both halves were misleading.
    ///
    /// * **"the block-entity path draws it" was a plan, not a fact.** For a long
    ///   while nothing consumed this variant on any 3-D surface —
    ///   `lodestone-render`'s item baker discarded it at four sites, so a chest in
    ///   the hand, on the ground or in an item frame drew literally nothing while
    ///   the inventory slot drew a real chest. `lodestone_render::special_item_rig`
    ///   plus `ItemVariants::resolve_special` is what makes the claim true; a
    ///   consumer must call **both** that and the baked path, in that order.
    /// * **The "flat sprite fallback" does not exist.** Every one of the ten
    ///   special `base` models in 26.2 has no `elements` and no `layer0` — only a
    ///   `particle` texture, which is a *block* texture and is not in the item
    ///   atlas. So [`ItemIconBuilder::part_for_model`] classifies every one of them
    ///   as undrawable and the "fallback" draws the same zero pixels as no fallback
    ///   at all. Measured against the real jar by
    ///   `lodestone-shell/tests/hotbar_special_item_pixels.rs`.
    ///
    /// What `base` is genuinely for, and the only reason it is resolved, is its
    /// `display` map: a chest's `gui` pose is `[30, 45, 0]` at scale `0.625` and its
    /// `firstperson_righthand` pose likewise, both authored on
    /// `item/template_chest`. See [`ItemIconBuilder::part_for`], which returns that
    /// map for a `Special`.
    Special {
        /// The base sprite model.
        base: ResourceLocation,
        /// The special renderer id (e.g. `minecraft:chest`).
        kind: String,
        /// Every `"transformation"` on the definition tree's path down to this
        /// `special` node, outermost first — see
        /// [`crate::ItemModelNode::Special`]'s field of the same name for why
        /// an ancestor's counts, and [`crate::ItemNodeTransform`] for how one
        /// entry composes. Always empty for the ex-`builtin/entity` arm (a
        /// different model type entirely, with no such field). A caller folds
        /// the chain *underneath* [`ItemIcon::display`]'s pose.
        transformation: Vec<crate::ItemNodeTransform>,
    },
}

/// A drawable inventory icon: the ordered parts to draw for one item slot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemIcon {
    /// The parts to draw, back-to-front. Empty means the item renders nothing.
    pub parts: Vec<IconPart>,
    /// Every `display` slot of the model behind [`Self::parts`] — the pose to
    /// use in each of vanilla's drawing contexts, not just the inventory one.
    ///
    /// # Why this is on the icon and not on each part
    ///
    /// A `display` map belongs to a *model*, and a `composite` icon has one
    /// model per part, so strictly this should be per-part. It is not, for two
    /// reasons. The blunt one: `IconPart`'s variants are destructured by struct
    /// pattern in a dozen places across three crates, and a new variant field
    /// breaks every one of them for no gain. The substantive one: **nothing
    /// downstream is per-part anyway.** `BlockModels::items` is keyed by item
    /// id and keeps only the *first* model part of a composite (the rest are
    /// reported in `item_bake_misses`), so a per-part map would have exactly one
    /// live entry in every case that reaches a pixel.
    ///
    /// So this is the display map of the **first drawable part's** model, which
    /// is the part every consumer actually draws. Revisit it the day composite
    /// icons bake more than one part.
    ///
    /// Slots the model chain never declared read back as the identity through
    /// [`DisplayTransforms::get`]; use
    /// [`declared`](DisplayTransforms::declared) to tell "vanilla says
    /// identity" from "we found nothing".
    pub display: DisplayTransforms,
}

impl ItemIcon {
    /// An icon that draws nothing (air, or an `empty`/`builtin/empty` model).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            parts: Vec::new(),
            display: DisplayTransforms::NONE,
        }
    }

    /// Whether this icon draws anything at all.
    #[must_use]
    pub fn is_drawable(&self) -> bool {
        !self.parts.is_empty()
    }
}

/// The default [`ItemPropertyContext`]: every condition is false, no select key
/// is set, every range reads `0`. Resolving a definition against it yields the
/// default inventory appearance a fresh, unmodified stack shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultItemContext;

impl ItemPropertyContext for DefaultItemContext {
    fn condition(&self, _property: &str, _component: Option<&str>) -> bool {
        false
    }
    fn select(&self, _property: &str) -> Option<String> {
        None
    }
    fn range(&self, _property: &str) -> f32 {
        0.0
    }
}

/// The `select` property naming which of vanilla's nine drawing contexts is
/// being rendered — `ItemDisplayContext`, whose serialised names are exactly
/// [`DisplaySlot::json_name`]'s (verified against
/// `net/minecraft/world/item/ItemDisplayContext.java`, whose `NONE` is the only
/// value with no `DisplaySlot`).
pub const DISPLAY_CONTEXT_PROPERTY: &str = "minecraft:display_context";

/// [`DefaultItemContext`] plus one honest answer for
/// [`DISPLAY_CONTEXT_PROPERTY`]: the drawing context this pass is in.
///
/// The **static** half of item variant selection, and the half that needs no
/// game state at all. 26 of 26.2's items branch on nothing else, and resolving
/// them under a pinned `"gui"` is why a spyglass in the hand drew the flat
/// inventory sprite instead of `item/spyglass_in_hand`'s tube: the branch is not
/// "which stack is this", it is "which pass am I".
///
/// Prefer this over [`GuiItemContext`] anywhere the answer is not literally an
/// inventory slot — a dropped item is [`DisplaySlot::Ground`], a mob's hand is
/// [`DisplaySlot::ThirdPersonRightHand`], our own is
/// [`DisplaySlot::FirstPersonRightHand`], and each of those resolves to a
/// genuinely different model for those items.
///
/// It answers no `condition` and no other `select`, so a state-dependent item
/// (a drawn bow) still flattens to its `on_false` form here — that needs live
/// state, which this type deliberately does not pretend to have.
#[derive(Debug, Clone, Copy)]
pub struct DisplayContextItemContext(pub DisplaySlot);

impl ItemPropertyContext for DisplayContextItemContext {
    fn condition(&self, property: &str, component: Option<&str>) -> bool {
        DefaultItemContext.condition(property, component)
    }
    fn select(&self, property: &str) -> Option<String> {
        if property == DISPLAY_CONTEXT_PROPERTY {
            Some(self.0.json_name().to_string())
        } else {
            DefaultItemContext.select(property)
        }
    }
    fn range(&self, property: &str) -> f32 {
        DefaultItemContext.range(property)
    }
}

/// The [`ItemPropertyContext`] for the inventory/GUI slot appearance
/// specifically: identical to [`DefaultItemContext`] except that
/// `minecraft:display_context` resolves to `"gui"`.
///
/// Exactly [`DisplayContextItemContext`]`(DisplaySlot::Gui)`, kept as its own
/// name because the GUI slot is the one context a *baker* has to single out: it
/// is the form [`crate::item_atlas::ItemAtlas`] stitches and the fallback every
/// other context degrades to.
///
/// A handful of 26.2 items (`spyglass`, `trident`, the spears, every bundle)
/// branch on `minecraft:display_context` at the top of their definition tree,
/// with a `gui` case pointing at the flat inventory sprite and *no* fallback
/// case for it — the fallback is the in-hand/held 3-D model instead. Under
/// [`DefaultItemContext`] (which never answers a `select`), that branch always
/// takes the fallback, so those items' item-atlas icon silently becomes the
/// wrong (in-hand) model. This context exists so [`ItemIconBuilder::icon_with`]
/// can resolve the tree the way the game does when drawing a hotbar/inventory
/// slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuiItemContext;

impl ItemPropertyContext for GuiItemContext {
    fn condition(&self, property: &str, component: Option<&str>) -> bool {
        DefaultItemContext.condition(property, component)
    }
    fn select(&self, property: &str) -> Option<String> {
        DisplayContextItemContext(DisplaySlot::Gui).select(property)
    }
    fn range(&self, property: &str) -> f32 {
        DefaultItemContext.range(property)
    }
}

/// Builds [`ItemIcon`]s from a pack stack. Resolved models are cached across
/// items (shared ancestors such as `item/generated` are parsed once).
#[derive(Debug)]
pub struct ItemIconBuilder<'a> {
    manager: &'a ResourceManager,
    resolver: ModelResolver<'a>,
}

impl<'a> ItemIconBuilder<'a> {
    /// Creates a builder over the given pack stack.
    #[must_use]
    pub fn new(manager: &'a ResourceManager) -> Self {
        Self {
            manager,
            resolver: ModelResolver::new(manager),
        }
    }

    /// Builds the default inventory icon for `item` (e.g. `minecraft:stone`),
    /// resolving its definition tree against [`DefaultItemContext`].
    ///
    /// # Errors
    ///
    /// Returns [`IconError::DefinitionMissing`] if no `items/<id>.json` exists,
    /// or [`IconError::Definition`]/[`IconError::Model`] if the definition or a
    /// referenced model fails to parse or resolve.
    pub fn icon(&self, item: &ResourceLocation) -> Result<ItemIcon, IconError> {
        self.icon_with(item, &DefaultItemContext)
    }

    /// Builds the icon for `item` under a caller-supplied property context (a
    /// concrete stack's runtime state), for items whose appearance depends on it.
    ///
    /// # Errors
    ///
    /// Same as [`Self::icon`].
    pub fn icon_with(
        &self,
        item: &ResourceLocation,
        ctx: &impl ItemPropertyContext,
    ) -> Result<ItemIcon, IconError> {
        let def = self.definition(item)?;
        self.icon_of(&def, ctx)
    }

    /// Reads and parses `items/<id>.json` — the selector tree, without resolving
    /// it against any context.
    ///
    /// A caller that needs *every* form an item can take (a baker seeding an
    /// atlas, a renderer re-resolving per frame) wants the tree itself, not one
    /// context's answer: [`ItemModel::outputs`] enumerates the variants and
    /// [`Self::part_for_model`] classifies each. Handing the tree out is also
    /// what stops the definition being parsed once per context — resolution is
    /// pure, so one parse serves every frame.
    ///
    /// # Errors
    ///
    /// [`IconError::DefinitionMissing`] if no `items/<id>.json` exists, or
    /// [`IconError::Definition`] if it does not parse.
    pub fn definition(&self, item: &ResourceLocation) -> Result<ItemModel, IconError> {
        let bytes = self
            .manager
            .read_asset(item, "items", "json")
            .ok_or_else(|| IconError::DefinitionMissing(item.to_string()))?;
        Ok(ItemModel::parse(&bytes)?)
    }

    /// [`Self::icon_with`] over an already-parsed definition, so a caller holding
    /// one from [`Self::definition`] does not re-read and re-parse the JSON to
    /// resolve a second context.
    ///
    /// # Errors
    ///
    /// [`IconError::Model`] if a model the chosen branch names fails to resolve.
    pub fn icon_of(
        &self,
        def: &ItemModel,
        ctx: &impl ItemPropertyContext,
    ) -> Result<ItemIcon, IconError> {
        let mut parts = Vec::new();
        // The **first** drawable part's display map wins; see `ItemIcon::display`
        // for why the icon carries one map rather than one per part. A part that
        // renders nothing must not claim the slot, or a `composite` whose first
        // entry is an `empty` model would report that empty model's (absent)
        // transforms for the geometry actually drawn.
        let mut display = None;
        for output in def.resolve(ctx) {
            let (part, part_display) = self.part_for(output)?;
            if let Some(part) = part {
                parts.push(part);
                display = display.or(part_display);
            }
        }
        Ok(ItemIcon {
            parts,
            display: display.unwrap_or(DisplayTransforms::NONE),
        })
    }

    /// Classifies one resolved definition output into a drawable part (`None`
    /// when it renders nothing: an `empty`/`builtin/empty` model, or a generated
    /// model with no resolvable layers), plus that part's model's `display` map.
    ///
    /// The display map is returned even for an `IconPart::Special`, whose `base`
    /// is a real model — a shield's in-hand transform is authored there even
    /// though the geometry comes from a block-entity renderer we do not have.
    ///
    /// Public because a *variant* baker walks [`ItemModel::outputs`] rather than
    /// [`ItemModel::resolve`], and so needs the same classification without going
    /// through [`Self::icon_of`] — which would collapse the tree back to one form,
    /// the very thing the variant axis exists to stop.
    #[allow(clippy::type_complexity)]
    pub fn part_for(
        &self,
        output: ItemModelOutput<'_>,
    ) -> Result<(Option<IconPart>, Option<DisplayTransforms>), IconError> {
        match output {
            ItemModelOutput::Special {
                base,
                kind,
                transformation,
            } => {
                let display = self
                    .resolver
                    .resolve(base)
                    .ok()
                    .map(|r| r.display_transforms());
                Ok((
                    Some(IconPart::Special {
                        base: base.clone(),
                        kind: kind.to_string(),
                        transformation: transformation.to_vec(),
                    }),
                    display,
                ))
            }
            ItemModelOutput::Model {
                model,
                tints,
                transformation,
            } => self.part_for_model_transformed(model, tints, transformation),
        }
    }

    /// Resolves a model reference and classifies it into a [`IconPart`] plus the
    /// model's `display` transforms.
    ///
    /// Public for the same reason [`Self::part_for`] is, and it is the entry point
    /// a variant baker actually wants: the `display` map that comes back is
    /// **this model's**, not the icon's first-drawable-part map. That distinction
    /// is the held-item transform bug — `item/bow_pulling_1` and
    /// `item/spyglass_in_hand` each author their own `firstperson_righthand`, and
    /// an [`ItemIcon`] resolved in the GUI context reports `item/generated`'s
    /// instead.
    #[allow(clippy::type_complexity)]
    pub fn part_for_model(
        &self,
        model: &ResourceLocation,
        tints: &[TintSource],
    ) -> Result<(Option<IconPart>, Option<DisplayTransforms>), IconError> {
        self.part_for_model_transformed(model, tints, &[])
    }

    /// [`Self::part_for_model`], carrying a `minecraft:model` item-definition-tree
    /// node's own root-to-node `"transformation"` chain onto the resulting
    /// [`IconPart::Model::node_transformation`] — see that field's doc. `&[]`
    /// (what [`Self::part_for_model`] passes) is right for every caller that
    /// only wants to know *which model* a definition resolves to (picking the
    /// GUI form, say); a caller building the actual placed geometry — one
    /// [`ItemModelOutput::Model`] at a time — must pass that output's own
    /// `transformation` through here instead, or a coloured bed's `foot`
    /// sub-model bakes with no record of the offset that keeps it from
    /// z-fighting the `head`.
    #[allow(clippy::type_complexity)]
    pub fn part_for_model_transformed(
        &self,
        model: &ResourceLocation,
        tints: &[TintSource],
        transformation: &[crate::item_model::ItemNodeTransform],
    ) -> Result<(Option<IconPart>, Option<DisplayTransforms>), IconError> {
        let resolved = self.resolver.resolve(model)?;
        // Read once, up front, and hand to whichever arm wins: *every* drawable
        // shape needs the non-GUI slots, and the sprite arm below is the one
        // that historically had no model reference left to go back for.
        let display = resolved.display_transforms();
        let part = match resolved.builtin.as_deref() {
            // The generated (flat sprite) path: gather layer0.. sprite refs in
            // order, pairing each with its tint index. Stop at the first missing
            // or unresolved layer, matching vanilla's contiguous layer scan.
            Some("generated") => {
                let mut layers = Vec::new();
                for (i, name) in LAYER_NAMES.iter().enumerate() {
                    match resolved.textures.get(*name) {
                        Some(TextureBinding::Resolved(sprite)) => layers.push(SpriteLayer {
                            sprite: sprite.clone(),
                            tint: tints.get(i).cloned(),
                        }),
                        _ => break,
                    }
                }
                if layers.is_empty() {
                    None
                } else {
                    Some(IconPart::Sprite { layers })
                }
            }
            // `builtin/entity` is the pre-1.21.4 special-renderer sentinel. It
            // should not appear for 26.2 items (they moved to `special` nodes),
            // but if a pack still uses it, surface it as a special rather than
            // dropping the item silently.
            Some("entity") => Some(IconPart::Special {
                base: model.clone(),
                kind: "minecraft:builtin_entity".to_string(),
                transformation: Vec::new(),
            }),
            // `builtin/empty` and any other builtin sentinel render nothing here.
            Some(_) => None,
            // An ordinary model: 3-D geometry becomes a Model part under the GUI
            // transform; a geometry-less shell (e.g. a chest's `models/item`
            // template) renders nothing on this path.
            None => {
                if resolved.elements.is_empty() {
                    None
                } else {
                    Some(IconPart::Model {
                        model: model.clone(),
                        transform: display.get(DisplaySlot::Gui),
                        gui_light: resolved.gui_light,
                        node_transformation: transformation.to_vec(),
                    })
                }
            }
        };
        Ok((part, Some(display)))
    }
}
