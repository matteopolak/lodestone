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

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::atlas::{Atlas, AtlasBuilder, AtlasSprite};
use crate::error::{AtlasError, ItemAtlasError};
use crate::icon::{GuiItemContext, IconPart, ItemIcon, ItemIconBuilder};
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
    /// How many of [`Self::sprites`] were served by a pack layer *above* the
    /// bottom of the stack rather than by the built-in pack.
    ///
    /// This is the one number that separates "the pack's item art is in the
    /// atlas" from "the atlas was rebuilt and nothing changed", and the two are
    /// otherwise indistinguishable on screen and in every other counter here: a
    /// pack whose `textures/item/*.png` never reached the stitch looks exactly
    /// like a pack that chose not to override any item. `0` with a stack deeper
    /// than one layer is a defect report, not a status update — see
    /// `lodestone_shell::resources::load_item_atlas`, which warns on it.
    pub layered_sprites: usize,
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
            let icon = match builder.icon_with(&id, &GuiItemContext) {
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
        let layered = layered_texture_paths(manager);
        let mut layered_sprites = 0usize;
        for loc in &sprite_locs {
            match atlas_builder.load(manager, loc) {
                Ok(_) => {
                    loaded += 1;
                    if is_layered(&layered, loc) {
                        layered_sprites += 1;
                    }
                }
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
                Ok(_) => {
                    loaded += 1;
                    if is_layered(&layered, loc) {
                        layered_sprites += 1;
                    }
                }
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
        report.layered_sprites = layered_sprites;
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

/// Every in-pack path carried by a layer **above** the bottom of `manager`'s
/// stack — i.e. by a resource pack rather than by the built-in pack.
///
/// One `list` per pack layer, not one `read` per sprite: a `read` on a zip
/// decompresses the entry, and the question here is only whether the path
/// exists. With no pack selected the `skip(1)` yields nothing and this costs
/// nothing at all, which is the common case.
///
/// The *bottom* layer is the built-in pack by construction — every stack this
/// crate is handed is built lowest-priority-first with the vanilla jar at index
/// 0 (see [`ResourceManager::new`]). A stack assembled the other way round
/// would make this count the opposite thing, which is why it is derived here
/// rather than passed in as a flag a caller could get backwards.
fn layered_texture_paths(manager: &ResourceManager) -> HashSet<String> {
    let mut paths = HashSet::new();
    for source in manager.sources().iter().skip(1) {
        paths.extend(source.list("assets/"));
    }
    paths
}

/// Whether `location`'s PNG is carried by one of the pack layers in `layered`.
fn is_layered(layered: &HashSet<String>, location: &ResourceLocation) -> bool {
    !layered.is_empty()
        && layered.contains(&ResourceManager::asset_path(location, "textures", "png"))
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
