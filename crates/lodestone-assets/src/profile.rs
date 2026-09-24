//! The [`AssetProfile`] version-specific convention seam.
//!
//! Asset path conventions drift across Minecraft versions. Rather than teaching
//! the loader about versions, a version crate supplies an [`AssetProfile`]
//! describing the conventions in force, mirroring how a `PhysicsProfile` would
//! carry version-specific physics constants. The loader itself never branches on
//! version.

/// Version-specific asset conventions supplied by a version crate.
///
/// This captures the drift the loader must not hardcode, such as the
/// `textures/blocks/` (≤1.12) versus `textures/block/` (1.13+) flattening and
/// the `pack_format` numbers that gate pack validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetProfile {
    /// The canonical `pack_format` for this version's own assets.
    pub pack_format: u32,
    /// Inclusive `(min, max)` range of `pack_format` values accepted here.
    pub supported_pack_formats: (u32, u32),
    /// Directory segment for block textures: `block` (1.13+) or `blocks`
    /// (≤1.12).
    pub block_texture_dir: &'static str,
    /// Directory segment for item textures: `item` (1.13+) or `items` (≤1.12).
    pub item_texture_dir: &'static str,
    /// Whether this version defines atlases with declarative `atlases/*.json`
    /// source lists (1.13+). Pre-flattening versions have no atlas index; the
    /// terrain sheet is the implicit set of everything under the block-texture
    /// directory (see [`crate::AtlasDefinition::implicit_terrain`]).
    pub uses_atlas_index: bool,
}

impl AssetProfile {
    /// Convention profile for the 1.21.5–26.2 family (flattened directories).
    ///
    /// `pack_format` is set to `88`, matching Minecraft 26.2's
    /// `version.json` (`resource_major`). The exact number is version-specific
    /// and a dedicated version crate may override it; this is a usable default
    /// for the current family.
    pub const MODERN: AssetProfile = AssetProfile {
        pack_format: 88,
        supported_pack_formats: (55, 99),
        block_texture_dir: "block",
        item_texture_dir: "item",
        uses_atlas_index: true,
    };

    /// Convention profile for the 1.8.x family (pre-flattening).
    ///
    /// Verified against the real 1.8.9 `client.jar`: block textures live under
    /// `textures/blocks/` and item textures under `textures/items/` (both
    /// plural), block *models* are already singular (`models/block/`),
    /// blockstates use only the `variants` schema (no `multipart`), and there is
    /// no `atlases/*.json` index. `pack_format` 1 is a `SharedConstants` code
    /// constant — the 1.8.9 jar ships no `pack.mcmeta`/`version.json`.
    pub const LEGACY_1_8: AssetProfile = AssetProfile {
        pack_format: 1,
        supported_pack_formats: (1, 1),
        block_texture_dir: "blocks",
        item_texture_dir: "items",
        uses_atlas_index: false,
    };

    /// Convention profile for the 1.11–1.12.2 family (still pre-flattening).
    ///
    /// Verified against the real 1.12.2 `client.jar`: plural texture dirs,
    /// singular model dirs, `multipart` blockstates present (arrived in 1.9),
    /// still no `atlases/*.json`. `pack_format` 3 for the 1.11–1.12.2 line.
    pub const LEGACY_1_12: AssetProfile = AssetProfile {
        pack_format: 3,
        supported_pack_formats: (1, 3),
        block_texture_dir: "blocks",
        item_texture_dir: "items",
        uses_atlas_index: false,
    };

    /// Returns whether a pack's declared `pack_format` is accepted.
    pub fn supports_pack_format(&self, format: u32) -> bool {
        let (min, max) = self.supported_pack_formats;
        (min..=max).contains(&format)
    }

    /// Builds the version-appropriate block-texture sub-path (without extension),
    /// for example `block/stone`.
    pub fn block_texture_path(&self, name: &str) -> String {
        format!("{}/{}", self.block_texture_dir, name)
    }

    /// Builds the version-appropriate item-texture sub-path (without extension),
    /// for example `item/apple`.
    pub fn item_texture_path(&self, name: &str) -> String {
        format!("{}/{}", self.item_texture_dir, name)
    }
}
