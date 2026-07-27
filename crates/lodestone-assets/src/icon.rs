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

use crate::error::IconError;
use crate::item::LAYER_NAMES;
use crate::item_model::{ItemModel, ItemModelOutput, ItemPropertyContext, TintSource};
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::model::{DisplayTransform, GuiLight, ModelResolver, TextureBinding};

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
        transform: DisplayTransform,
        /// The GUI lighting mode (side-lit for blocks, front-lit for flat items).
        gui_light: GuiLight,
    },
    /// A code-driven special renderer (chest, shulker box, banner, shield, …).
    /// The renderer's block-entity path draws it; `base` is a real item model it
    /// can fall back to as a flat sprite.
    Special {
        /// The base sprite model.
        base: ResourceLocation,
        /// The special renderer id (e.g. `minecraft:chest`).
        kind: String,
    },
}

/// A drawable inventory icon: the ordered parts to draw for one item slot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemIcon {
    /// The parts to draw, back-to-front. Empty means the item renders nothing.
    pub parts: Vec<IconPart>,
}

impl ItemIcon {
    /// An icon that draws nothing (air, or an `empty`/`builtin/empty` model).
    #[must_use]
    pub fn empty() -> Self {
        Self { parts: Vec::new() }
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
        let bytes = self
            .manager
            .read_asset(item, "items", "json")
            .ok_or_else(|| IconError::DefinitionMissing(item.to_string()))?;
        let def = ItemModel::parse(&bytes)?;
        let mut parts = Vec::new();
        for output in def.resolve(ctx) {
            if let Some(part) = self.part_for(output)? {
                parts.push(part);
            }
        }
        Ok(ItemIcon { parts })
    }

    /// Classifies one resolved definition output into a drawable part, or `None`
    /// when it renders nothing (an `empty`/`builtin/empty` model, or a generated
    /// model with no resolvable layers).
    fn part_for(&self, output: ItemModelOutput<'_>) -> Result<Option<IconPart>, IconError> {
        match output {
            ItemModelOutput::Special { base, kind } => Ok(Some(IconPart::Special {
                base: base.clone(),
                kind: kind.to_string(),
            })),
            ItemModelOutput::Model { model, tints } => self.classify_model(model, tints),
        }
    }

    /// Resolves a model reference and classifies it into a [`IconPart`].
    fn classify_model(
        &self,
        model: &ResourceLocation,
        tints: &[TintSource],
    ) -> Result<Option<IconPart>, IconError> {
        let resolved = self.resolver.resolve(model)?;
        match resolved.builtin.as_deref() {
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
                    Ok(None)
                } else {
                    Ok(Some(IconPart::Sprite { layers }))
                }
            }
            // `builtin/entity` is the pre-1.21.4 special-renderer sentinel. It
            // should not appear for 26.2 items (they moved to `special` nodes),
            // but if a pack still uses it, surface it as a special rather than
            // dropping the item silently.
            Some("entity") => Ok(Some(IconPart::Special {
                base: model.clone(),
                kind: "minecraft:builtin_entity".to_string(),
            })),
            // `builtin/empty` and any other builtin sentinel render nothing here.
            Some(_) => Ok(None),
            // An ordinary model: 3-D geometry becomes a Model part under the GUI
            // transform; a geometry-less shell (e.g. a chest's `models/item`
            // template) renders nothing on this path.
            None => {
                if resolved.elements.is_empty() {
                    Ok(None)
                } else {
                    let transform = resolved.display.get("gui").copied().unwrap_or_default();
                    Ok(Some(IconPart::Model {
                        model: model.clone(),
                        transform,
                        gui_light: resolved.gui_light,
                    }))
                }
            }
        }
    }
}
