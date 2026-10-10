//! The map-decoration sprite sheet: every PNG the pack's
//! `atlases/map_decorations.json` directory source names, stitched into one
//! [`Atlas`] keyed by sprite id (`minecraft:player`, `minecraft:red_banner`, ...).
//!
//! The sprite list is whatever the descriptor resolves to, never a hand-kept
//! filename list, so a pack that adds a decoration sprite extends the sheet.
//! Stacked descriptors extend the jar's own source like every other atlas
//! ([`AtlasDefinition::load_stacked`]).

use crate::atlas::{Atlas, AtlasBuilder};
use crate::atlas_source::AtlasDefinition;
use crate::error::AtlasError;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// In-pack path of the decoration atlas descriptor.
pub const MAP_DECORATIONS_ATLAS_PATH: &str = "assets/minecraft/atlases/map_decorations.json";

/// Errors loading the decoration sheet.
#[derive(Debug, thiserror::Error)]
pub enum MapDecorationAtlasError {
    /// No pack carries `atlases/map_decorations.json`.
    #[error("map-decoration atlas descriptor not found: {path}")]
    DescriptorMissing {
        /// The in-pack path that was probed.
        path: String,
    },
    /// The descriptor resolved to sprites but stitching them failed.
    #[error("map-decoration atlas: {0}")]
    Atlas(#[from] AtlasError),
}

/// Stitches the decoration sprites. A sprite whose bytes are missing or do not
/// decode is skipped; an empty result is [`AtlasError::Empty`].
///
/// # Errors
///
/// See [`MapDecorationAtlasError`].
pub fn load_map_decoration_atlas(manager: &ResourceManager) -> Result<Atlas, MapDecorationAtlasError> {
    let definition = AtlasDefinition::load_stacked(manager, MAP_DECORATIONS_ATLAS_PATH).ok_or_else(|| {
        MapDecorationAtlasError::DescriptorMissing {
            path: MAP_DECORATIONS_ATLAS_PATH.to_string(),
        }
    })?;
    let mut builder = AtlasBuilder::new().with_padding(1);
    for entry in definition.resolve(manager) {
        let Some(png) = manager.read(&entry.texture_path) else {
            continue;
        };
        if let Ok(image) = Image::decode_png(&png) {
            builder.add_texture(entry.sprite, image, None);
        }
    }
    Ok(builder.build()?)
}
