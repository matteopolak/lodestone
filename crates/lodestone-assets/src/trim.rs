//! Armour trims — the `minecraft:trim` decal layer
//! (`net.minecraft.world.item.equipment.trim.ArmorTrim`).
//!
//! ## What it is
//!
//! A trim is a `(pattern, material)` pair a smithing-table upgrade attaches
//! to an armour piece. Vanilla draws it as a fifth layer over the piece's own
//! texture(s) — see [`docs/armour-rendering.md`](../../../docs/armour-rendering.md)'s
//! "Trims" section for the end-to-end design this module is the data half of.
//! This module owns the two vanilla registries (`trim_pattern`,
//! `trim_material`) and the sprite this pair resolves to; it knows nothing
//! about the wire, the wearer's skeleton, or a render pipeline.
//!
//! ## How it works
//!
//! `ArmorTrim.layerAssetId(layerAssetPrefix, equipmentAsset)` is:
//!
//! ```text
//! suffix = material.assets().assetId(equipmentAsset).suffix()   // wearer-aware
//! path   = pattern.assetId().path()
//! sprite = layerAssetPrefix + "/" + path + "_" + suffix
//! ```
//!
//! `layerAssetPrefix` is `"trims/entity/" + layerType.id` (`humanoid` or
//! `humanoid_leggings`). The interesting part is `MaterialAssetGroup.assetId`:
//! a material's suffix is normally its own
//! id, but five materials declare a **wearer-keyed override**, each
//! overriding exactly the armour material that matches their own name —
//! `iron` trim on `iron` armour resolves to `iron_darker`, but `iron` trim on
//! `diamond` armour resolves to plain `iron`. [`TrimMaterial::suffix_for`] is
//! that lookup.
//!
//! The **sprite pixels** are not shipped per-pattern-per-material on disk —
//! `client.jar` has one `trims/entity/humanoid/<pattern>.png` per pattern (18
//! of them, each an eight-step greyscale index image) and a matching
//! `trims/color_palettes/<suffix>.png` eight-colour strip per suffix (16 of
//! them: 11 materials plus the 5 `_darker` overrides). They are combined at
//! load time by [`crate::palette_bake`], driven by the real
//! `minecraft:paletted_permutations` atlas source
//! (`assets/minecraft/atlases/armor_trims.json`, [`ARMOR_TRIMS_ATLAS_PATH`])
//! — [`TrimAtlas`] is the thin wrapper that loads that descriptor, bakes it,
//! and resolves `(pattern, material, layer type, wearer asset)` to a decoded
//! [`Image`], the same "individually addressable decoded sprite" shape
//! [`crate::banner_pattern_atlas::BannerPatternAtlas`] already uses.
//!
//! ## How to change it
//!
//! [`TRIM_PATTERNS`]/[`TRIM_MATERIALS`] are the only two hand-transcribed
//! tables here (registry content with no generic-atlas-descriptor
//! equivalent: `decal` and the override map are Java statics, not resource
//! files) — everything else this module needs (which sprites exist, their
//! pixels) is discovered from the real `armor_trims.json` descriptor plus the
//! real palette/pattern PNGs, per this crate's own "discovered, not
//! hand-listed" rule (see `banner_pattern_atlas`'s module docs for the fuller
//! argument). A new pattern or material added by a future version is a new
//! row in one of these two tables and nothing else, provided its sprite
//! assets follow the same `paletted_permutations` shape.
//!
//! **Gotcha**: `decal` selects a *pipeline*, not a texture. Every one of the
//! 18 patterns in 26.2 has `"decal": false` (checked directly against every
//! `data/minecraft/trim_pattern/*.json` in `client.jar` — see [`TRIM_PATTERNS`]),
//! so the `decal: true` branch (vanilla's `ARMOR_DECAL_CUTOUT_NO_CULL`,
//! `lodestone_render`'s `EntityPipeline::trim_decal_pipeline`) is exercised by
//! no real vanilla trim today. It still has to exist and be selected
//! correctly — a resource pack, or a future vanilla release, can set it, and
//! `Sheets.armorTrimsSheet(decal)`'s branch is a real fork, not a vanilla
//! implementation detail this crate is free to collapse.

use std::collections::HashMap;

use crate::atlas_source::{AtlasDefinition, AtlasSource};
use crate::equipment::ArmourLayerType;
use crate::error::TrimAtlasError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::palette_bake::{self, PaletteBakeReport};
use crate::texture::Image;

/// In-pack path of vanilla's own armour-trim atlas descriptor.
pub const ARMOR_TRIMS_ATLAS_PATH: &str = "assets/minecraft/atlases/armor_trims.json";

/// One `trim_pattern` registry entry (`data/minecraft/trim_pattern/*.json`).
///
/// `id` is both the registry name and the `assetId` path segment — every one
/// of 26.2's 18 patterns declares `"asset_id": "minecraft:<id>"` with `<id>`
/// identical to its own file's stem, so there is no separate field to carry
/// (unlike [`TrimMaterial`], whose *suffix* can differ from its id via the
/// override table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimPattern {
    /// The registry name / `assetId` path segment, e.g. `"sentry"`.
    pub id: &'static str,
    /// `TrimPattern.decal` — selects `Sheets.armorTrimsSheet`'s pipeline
    /// branch (`ARMOR_CUTOUT_NO_CULL` when `false`, `ARMOR_DECAL_CUTOUT_NO_CULL`
    /// when `true`). See this module's "Gotcha" note: every 26.2 pattern is
    /// `false`.
    pub decal: bool,
}

/// Every `trim_pattern` in 26.2, transcribed from
/// `data/minecraft/trim_pattern/*.json` in `client.jar` (18 files, `decal`
/// read directly off each one — none set it).
pub const TRIM_PATTERNS: &[TrimPattern] = &[
    TrimPattern { id: "bolt", decal: false },
    TrimPattern { id: "coast", decal: false },
    TrimPattern { id: "dune", decal: false },
    TrimPattern { id: "eye", decal: false },
    TrimPattern { id: "flow", decal: false },
    TrimPattern { id: "host", decal: false },
    TrimPattern { id: "raiser", decal: false },
    TrimPattern { id: "rib", decal: false },
    TrimPattern { id: "sentry", decal: false },
    TrimPattern { id: "shaper", decal: false },
    TrimPattern { id: "silence", decal: false },
    TrimPattern { id: "snout", decal: false },
    TrimPattern { id: "spire", decal: false },
    TrimPattern { id: "tide", decal: false },
    TrimPattern { id: "vex", decal: false },
    TrimPattern { id: "ward", decal: false },
    TrimPattern { id: "wayfinder", decal: false },
    TrimPattern { id: "wild", decal: false },
];

/// One `(wearer armour asset id, override suffix)` pair —
/// `MaterialAssetGroup`'s `override_armor_assets` map, one entry at a time.
pub type MaterialOverride = (&'static str, &'static str);

/// One `trim_material` registry entry
/// (`data/minecraft/trim_material/*.json`), transcribed from
/// `MaterialAssetGroup.java`'s eleven static instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimMaterial {
    /// The registry name, e.g. `"iron"`.
    pub id: &'static str,
    /// The suffix used when [`Self::suffix_for`] finds no override —
    /// identical to `id` for every 26.2 material (`MaterialAssetGroup.create`
    /// always seeds `base` from the same string as the material's own name).
    pub base_suffix: &'static str,
    /// Wearer-armour-asset-id-keyed overrides. Only five materials declare
    /// any (iron, netherite, copper, gold, diamond), and each declares
    /// exactly one — itself.
    pub overrides: &'static [MaterialOverride],
}

impl TrimMaterial {
    /// `MaterialAssetGroup.assetId(equipmentAssetId)` — the suffix to append
    /// to the pattern's path when this material trims a piece whose own
    /// armour material is `wearer_asset_id` (an [`crate::equipment::ArmourAsset::id`],
    /// e.g. `"diamond"`).
    ///
    /// Wearer-aware: a diamond trim on **diamond** armour resolves to
    /// `diamond_darker`; the same diamond trim on **iron** armour resolves to
    /// plain `diamond`, because the override is keyed by the *wearer's*
    /// material, not the trim's own. Picking the sprite from the trim
    /// material alone (ignoring `wearer_asset_id` entirely) is exactly the
    /// plausible-looking-but-wrong colour `docs/armour-rendering.md`'s
    /// "Trims" section warns about.
    #[must_use]
    pub fn suffix_for(&self, wearer_asset_id: &str) -> &'static str {
        self.overrides
            .iter()
            .find(|(asset, _)| *asset == wearer_asset_id)
            .map_or(self.base_suffix, |(_, suffix)| suffix)
    }
}

/// Every `trim_material` in 26.2, transcribed from `MaterialAssetGroup.java`'s
/// static instances (`QUARTZ`, `IRON`, `NETHERITE`, `REDSTONE`, `COPPER`,
/// `GOLD`, `EMERALD`, `DIAMOND`, `LAPIS`, `AMETHYST`, `RESIN`) and
/// cross-checked against every `data/minecraft/trim_material/*.json` in
/// `client.jar`.
pub const TRIM_MATERIALS: &[TrimMaterial] = &[
    TrimMaterial { id: "quartz", base_suffix: "quartz", overrides: &[] },
    TrimMaterial {
        id: "iron",
        base_suffix: "iron",
        overrides: &[("iron", "iron_darker")],
    },
    TrimMaterial {
        id: "netherite",
        base_suffix: "netherite",
        overrides: &[("netherite", "netherite_darker")],
    },
    TrimMaterial { id: "redstone", base_suffix: "redstone", overrides: &[] },
    TrimMaterial {
        id: "copper",
        base_suffix: "copper",
        overrides: &[("copper", "copper_darker")],
    },
    TrimMaterial {
        id: "gold",
        base_suffix: "gold",
        overrides: &[("gold", "gold_darker")],
    },
    TrimMaterial { id: "emerald", base_suffix: "emerald", overrides: &[] },
    TrimMaterial {
        id: "diamond",
        base_suffix: "diamond",
        overrides: &[("diamond", "diamond_darker")],
    },
    TrimMaterial { id: "lapis", base_suffix: "lapis", overrides: &[] },
    TrimMaterial { id: "amethyst", base_suffix: "amethyst", overrides: &[] },
    TrimMaterial { id: "resin", base_suffix: "resin", overrides: &[] },
];

/// Looks up a pattern by its registry name.
#[must_use]
pub fn trim_pattern(id: &str) -> Option<&'static TrimPattern> {
    TRIM_PATTERNS.iter().find(|p| p.id == id)
}

/// Looks up a material by its registry name.
#[must_use]
pub fn trim_material(id: &str) -> Option<&'static TrimMaterial> {
    TRIM_MATERIALS.iter().find(|m| m.id == id)
}

/// `ArmorTrim.layerAssetId` — the sprite id a `(pattern, material)` pair
/// resolves to for a given layer type and wearer armour asset, e.g.
/// `minecraft:trims/entity/humanoid/sentry_iron_darker`. This is exactly the
/// key [`palette_bake::bake_paletted_permutations`] produces (base texture id
/// `+ "_" +` permutation suffix), so it doubles as the [`TrimAtlas`] lookup
/// key.
///
/// # Errors
///
/// Only if the composed string is not a valid [`ResourceLocation`] — every
/// real vanilla `(pattern, material)` pair produces one, so this is a defence
/// against a corrupt custom pattern/material id, not a real vanilla case.
pub fn trim_sprite_id(
    pattern: &TrimPattern,
    material: &TrimMaterial,
    layer_type: ArmourLayerType,
    wearer_asset_id: &str,
) -> Result<ResourceLocation, crate::error::ResourceLocationError> {
    let suffix = material.suffix_for(wearer_asset_id);
    ResourceLocation::parse(&format!(
        "minecraft:trims/entity/{}/{}_{suffix}",
        layer_type.serialized_name(),
        pattern.id
    ))
}

/// A census of what [`TrimAtlas::load_reported`] produced.
#[derive(Debug, Clone, Default)]
pub struct TrimAtlasReport {
    /// The underlying [`palette_bake`] bake report.
    pub bake: PaletteBakeReport,
}

/// The real armour-trim sprite sheet, baked from
/// `assets/minecraft/atlases/armor_trims.json` — every `(pattern, material,
/// layer type)` sprite `client.jar` can produce, decoded and individually
/// addressable. See this module's docs for why this is a flat map of decoded
/// images rather than a stitched GPU sheet.
#[derive(Debug, Default)]
pub struct TrimAtlas {
    sprites: HashMap<ResourceLocation, Image>,
}

impl TrimAtlas {
    /// Loads the real trim atlas, discarding the report.
    ///
    /// # Errors
    ///
    /// See [`Self::load_reported`].
    pub fn load(manager: &ResourceManager) -> Result<Self, TrimAtlasError> {
        Ok(Self::load_reported(manager)?.0)
    }

    /// Loads the real trim atlas and returns a coverage report alongside it.
    ///
    /// # Errors
    ///
    /// Only if `atlases/armor_trims.json` itself is missing or unparsable —
    /// an individual missing or undecodable sprite is recorded in the report
    /// (via [`palette_bake::PaletteBakeReport`]), not fatal.
    pub fn load_reported(
        manager: &ResourceManager,
    ) -> Result<(Self, TrimAtlasReport), TrimAtlasError> {
        // Stacked, not single-winner: a server pack shipping its own
        // `armor_trims.json` must extend the jar's `paletted_permutations`
        // source, not replace it outright (`AtlasDefinition::load_stacked`'s
        // own doc — `SpriteSourceList.load`'s `getResourceStack`).
        let definition = AtlasDefinition::load_stacked(manager, ARMOR_TRIMS_ATLAS_PATH)
            .ok_or_else(|| TrimAtlasError::DescriptorMissing {
                path: ARMOR_TRIMS_ATLAS_PATH.to_string(),
            })?;

        let mut sprites = HashMap::new();
        let mut bake_report = PaletteBakeReport::default();
        for source in &definition.sources {
            if matches!(source, AtlasSource::PalettedPermutations { .. }) {
                let (baked, report) = palette_bake::bake_paletted_permutations(source, manager);
                sprites.extend(baked);
                bake_report.loaded += report.loaded;
                bake_report
                    .missing_base_textures
                    .extend(report.missing_base_textures);
                bake_report.decode_errors.extend(report.decode_errors);
                bake_report.palette_errors.extend(report.palette_errors);
                if bake_report.reference_palette_error.is_none() {
                    bake_report.reference_palette_error = report.reference_palette_error;
                }
            }
        }
        Ok((
            Self { sprites },
            TrimAtlasReport { bake: bake_report },
        ))
    }

    /// Resolves a `(pattern, material)` pair to its decoded sprite for
    /// `layer_type`, on a piece whose own armour material is
    /// `wearer_asset_id` — the full [`trim_sprite_id`] lookup.
    #[must_use]
    pub fn sprite_for(
        &self,
        pattern: &TrimPattern,
        material: &TrimMaterial,
        layer_type: ArmourLayerType,
        wearer_asset_id: &str,
    ) -> Option<&Image> {
        let id = trim_sprite_id(pattern, material, layer_type, wearer_asset_id).ok()?;
        self.sprites.get(&id)
    }

    /// Number of decoded sprites (18 patterns × 16 suffixes × 2 layer types =
    /// 576 when every one bakes successfully against a real `client.jar`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    /// Whether no sprites decoded at all — the pack has no `client.jar`-shaped
    /// texture tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_id_is_unique_and_none_are_decal_in_26_2() {
        assert_eq!(TRIM_PATTERNS.len(), 18, "26.2 has 18 trim patterns");
        let mut ids: Vec<_> = TRIM_PATTERNS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TRIM_PATTERNS.len(), "pattern ids must be unique");
        assert!(
            TRIM_PATTERNS.iter().all(|p| !p.decal),
            "every 26.2 pattern.json has \"decal\": false — a true here would be stale"
        );
    }

    #[test]
    fn every_material_id_is_unique() {
        assert_eq!(TRIM_MATERIALS.len(), 11, "26.2 has 11 trim materials");
        let mut ids: Vec<_> = TRIM_MATERIALS.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TRIM_MATERIALS.len());
    }

    /// The exact worked example `docs/armour-rendering.md`'s "Trims" section
    /// gives: a diamond trim on diamond armour is darker than the same trim
    /// on any other material.
    #[test]
    fn diamond_trim_darkens_only_on_diamond_armour() {
        let diamond = trim_material("diamond").expect("diamond material exists");
        assert_eq!(diamond.suffix_for("diamond"), "diamond_darker");
        assert_eq!(diamond.suffix_for("iron"), "diamond");
        assert_eq!(diamond.suffix_for("netherite"), "diamond");
        assert_eq!(diamond.suffix_for("leather"), "diamond");
    }

    /// All five overriding materials, each overriding exactly itself.
    #[test]
    fn the_five_overriding_materials_override_only_their_own_armour() {
        for id in ["iron", "netherite", "copper", "gold", "diamond"] {
            let m = trim_material(id).unwrap_or_else(|| panic!("{id} material exists"));
            assert_eq!(m.overrides.len(), 1, "{id} declares exactly one override");
            let (wearer, suffix) = m.overrides[0];
            assert_eq!(wearer, id, "{id} overrides only itself");
            assert_eq!(suffix, format!("{id}_darker"));
            // And it must NOT fire for a different wearer.
            assert_eq!(m.suffix_for("leather"), id);
        }
    }

    #[test]
    fn the_six_non_overriding_materials_never_change_suffix() {
        for id in ["quartz", "redstone", "emerald", "lapis", "amethyst", "resin"] {
            let m = trim_material(id).unwrap_or_else(|| panic!("{id} material exists"));
            assert!(m.overrides.is_empty(), "{id} must declare no override");
            assert_eq!(m.suffix_for(id), id);
            assert_eq!(m.suffix_for("diamond"), id);
        }
    }

    #[test]
    fn trim_sprite_id_matches_armor_trim_layer_asset_id() {
        let sentry = trim_pattern("sentry").expect("sentry exists");
        let iron = trim_material("iron").expect("iron exists");
        let id = trim_sprite_id(sentry, iron, ArmourLayerType::Humanoid, "diamond")
            .expect("valid location");
        assert_eq!(id.to_string(), "minecraft:trims/entity/humanoid/sentry_iron");

        let id_on_iron = trim_sprite_id(sentry, iron, ArmourLayerType::Humanoid, "iron")
            .expect("valid location");
        assert_eq!(
            id_on_iron.to_string(),
            "minecraft:trims/entity/humanoid/sentry_iron_darker"
        );

        let leggings = trim_sprite_id(sentry, iron, ArmourLayerType::HumanoidLeggings, "diamond")
            .expect("valid location");
        assert_eq!(
            leggings.to_string(),
            "minecraft:trims/entity/humanoid_leggings/sentry_iron"
        );
    }

    #[test]
    fn missing_descriptor_reports_missing_not_a_panic() {
        let manager = ResourceManager::new(Vec::new());
        let err = TrimAtlas::load(&manager).expect_err("no pack, no descriptor");
        assert!(matches!(err, TrimAtlasError::DescriptorMissing { .. }));
    }
}
