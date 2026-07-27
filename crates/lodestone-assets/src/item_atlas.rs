//! The flat item-sprite atlas: the texture the inventory renderer samples for
//! `item/generated` icons.
//!
//! Most items draw as a flat sprite (`item/generated`): a diamond, a stick, a
//! pickaxe. This module resolves every item definition in the pack stack to its
//! [`ItemIcon`], stitches the union of the flat sprites those icons reference
//! into one [`Atlas`], and caches the resolved icons so the per-frame draw path
//! never re-resolves. It is deliberately GPU-free — it emits a CPU [`Atlas`] the
//! renderer uploads once and sprite rects the draw path looks up by location.
//!
//! Block items (`IconPart::Model`) are **not** stitched here: their faces live in
//! the block atlas the terrain renderer already owns, and the 3-D GUI path bakes
//! and samples that atlas instead. Special renderers (`IconPart::Special`) carry
//! a `base` sprite that is stitched *opportunistically*: most (chests, shulkers,
//! shield, banners) are code-driven and have no flat texture, so their absence is
//! parked and reported separately, not treated as a failure.

use std::collections::{BTreeSet, HashMap};

use crate::atlas::{Atlas, AtlasBuilder, AtlasSprite};
use crate::error::{AtlasError, ItemAtlasError};
use crate::icon::{IconPart, ItemIcon, ItemIconBuilder};
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// A census of what an [`ItemAtlas::build_reported`] run produced, for coverage
/// reporting. A percentage with named failures is worth more than a bare count.
#[derive(Debug, Clone, Default)]
pub struct ItemAtlasReport {
    /// Item definitions discovered under `assets/<ns>/items/`.
    pub items: usize,
    /// Items that produced a drawable icon (at least one part).
    pub drawable: usize,
    /// Items whose icon has at least one flat `Sprite` part.
    pub sprite_items: usize,
    /// Items whose icon has at least one 3-D `Model` part.
    pub model_items: usize,
    /// Items whose icon has at least one `Special` part.
    pub special_items: usize,
    /// Distinct sprites successfully stitched into the atlas.
    pub sprites: usize,
    /// Flat `Sprite`-layer textures referenced but absent/undecodable (named).
    /// These are real failures — a generated item with no texture.
    pub missing_textures: Vec<String>,
    /// `Special`-renderer base sprites with no flat texture (named). Expected and
    /// parked: chests/shulkers/shield/banners are drawn by a code-driven
    /// block-entity path, so their `base` has no flat sprite to stitch.
    pub missing_special_bases: Vec<String>,
    /// Item ids whose definition failed to parse or resolve (named).
    pub unresolved_items: Vec<String>,
}

/// The stitched flat item-sprite atlas plus the resolved icon for every item.
#[derive(Debug)]
pub struct ItemAtlas {
    atlas: Atlas,
    icons: HashMap<ResourceLocation, ItemIcon>,
}

impl ItemAtlas {
    /// Builds the atlas over `manager`, discarding the report.
    ///
    /// # Errors
    ///
    /// Returns [`ItemAtlasError`] only if the underlying atlas cannot be built at
    /// all. Missing individual textures are recorded, not fatal.
    pub fn build(manager: &ResourceManager) -> Result<Self, ItemAtlasError> {
        Ok(Self::build_reported(manager)?.0)
    }

    /// Builds the atlas and returns a coverage [`ItemAtlasReport`] alongside it.
    ///
    /// # Errors
    ///
    /// See [`Self::build`].
    pub fn build_reported(
        manager: &ResourceManager,
    ) -> Result<(Self, ItemAtlasReport), ItemAtlasError> {
        let builder = ItemIconBuilder::new(manager);
        let mut icons: HashMap<ResourceLocation, ItemIcon> = HashMap::new();
        let mut sprite_locs: BTreeSet<ResourceLocation> = BTreeSet::new();
        let mut special_locs: BTreeSet<ResourceLocation> = BTreeSet::new();
        let mut report = ItemAtlasReport::default();

        for id in item_ids(manager) {
            report.items += 1;
            let icon = match builder.icon(&id) {
                Ok(icon) => icon,
                Err(_) => {
                    report.unresolved_items.push(id.to_string());
                    continue;
                }
            };
            if icon.is_drawable() {
                report.drawable += 1;
            }
            let (mut has_sprite, mut has_model, mut has_special) = (false, false, false);
            for part in &icon.parts {
                match part {
                    IconPart::Sprite { layers } => {
                        has_sprite = true;
                        for layer in layers {
                            sprite_locs.insert(layer.sprite.clone());
                        }
                    }
                    IconPart::Model { .. } => has_model = true,
                    IconPart::Special { base, .. } => {
                        has_special = true;
                        special_locs.insert(base.clone());
                    }
                }
            }
            report.sprite_items += usize::from(has_sprite);
            report.model_items += usize::from(has_model);
            report.special_items += usize::from(has_special);
            icons.insert(id, icon);
        }

        // Stitch the union of referenced flat sprites; a missing or corrupt
        // texture is recorded and skipped rather than aborting the whole atlas.
        let mut atlas_builder = AtlasBuilder::new();
        let mut loaded = 0usize;
        for loc in &sprite_locs {
            match atlas_builder.load(manager, loc) {
                Ok(_) => loaded += 1,
                Err(AtlasError::TextureMissing { location }) => {
                    report.missing_textures.push(location);
                }
                Err(AtlasError::Texture { location, source }) => {
                    report.missing_textures.push(format!("{location}: {source}"));
                }
                Err(other) => return Err(ItemAtlasError::Atlas(other)),
            }
        }

        // Special-renderer bases are best-effort: most (chests, shulkers, shield,
        // banners) are drawn by a code-driven path and carry no flat texture, so
        // their absence is expected and parked, not a failure. Any that *do* have
        // a sprite are stitched as an honest fallback.
        for loc in &special_locs {
            if sprite_locs.contains(loc) {
                continue;
            }
            match atlas_builder.load(manager, loc) {
                Ok(_) => loaded += 1,
                Err(AtlasError::TextureMissing { location }) => {
                    report.missing_special_bases.push(location);
                }
                Err(AtlasError::Texture { location, source }) => {
                    report.missing_special_bases.push(format!("{location}: {source}"));
                }
                Err(other) => return Err(ItemAtlasError::Atlas(other)),
            }
        }

        // An all-block (or empty) pack references no flat sprites; seed a 1x1
        // transparent sprite so the atlas is still valid to build and upload.
        if loaded == 0 {
            atlas_builder.add_texture(
                ResourceLocation::new("minecraft", "empty").expect("valid literal location"),
                Image {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 0],
                },
                None,
            );
        }

        let atlas = atlas_builder.build()?;
        report.sprites = loaded;
        Ok((Self { atlas, icons }, report))
    }

    /// The stitched CPU atlas (upload once to the GPU).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The cached, pre-resolved icon for `item` (e.g. `minecraft:diamond`).
    #[must_use]
    pub fn icon(&self, item: &ResourceLocation) -> Option<&ItemIcon> {
        self.icons.get(item)
    }

    /// The stitched sprite for a texture location (e.g. `minecraft:item/diamond`).
    #[must_use]
    pub fn sprite(&self, location: &ResourceLocation) -> Option<&AtlasSprite> {
        self.atlas.sprite(location)
    }

    /// The number of items with a cached icon.
    #[must_use]
    pub fn len(&self) -> usize {
        self.icons.len()
    }

    /// Whether no items were resolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.icons.is_empty()
    }
}

/// Discovers item ids by scanning for `assets/<ns>/items/<path>.json`. Sorted and
/// deduplicated so a given pack stack yields a byte-identical atlas.
fn item_ids(manager: &ResourceManager) -> Vec<ResourceLocation> {
    let mut ids = BTreeSet::new();
    for path in manager.list("assets/") {
        let Some(rest) = path.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        let Some(item_path) = tail.strip_prefix("items/").and_then(|p| p.strip_suffix(".json"))
        else {
            continue;
        };
        if let Ok(loc) = ResourceLocation::parse(&format!("{namespace}:{item_path}")) {
            ids.insert(loc);
        }
    }
    ids.into_iter().collect()
}
