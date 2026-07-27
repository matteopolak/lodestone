//! Tests for [`AssetProfile`], the version-specific convention seam.

use lodestone_assets::AssetProfile;

#[test]
fn modern_profile_uses_flattened_dirs() {
    let p = AssetProfile::MODERN;
    assert_eq!(p.block_texture_dir, "block");
    assert_eq!(p.item_texture_dir, "item");
}

#[test]
fn modern_profile_pack_format_is_current() {
    // 26.2's resource pack format is 88 (from client.jar's version.json).
    assert_eq!(AssetProfile::MODERN.pack_format, 88);
}

#[test]
fn modern_profile_texture_subpaths() {
    let p = AssetProfile::MODERN;
    assert_eq!(p.block_texture_path("stone"), "block/stone");
    assert_eq!(p.item_texture_path("apple"), "item/apple");
}

#[test]
fn legacy_style_profile_uses_plural_dirs() {
    // A version crate for <=1.12 would supply plural directories. The loader
    // never branches on version; it only reads these fields.
    let legacy = AssetProfile {
        pack_format: 3,
        supported_pack_formats: (1, 3),
        block_texture_dir: "blocks",
        item_texture_dir: "items",
        uses_atlas_index: false,
    };
    assert_eq!(legacy.block_texture_path("stone"), "blocks/stone");
    assert_eq!(legacy.item_texture_path("apple"), "items/apple");
}

#[test]
fn pack_format_support_range() {
    let p = AssetProfile::MODERN;
    assert!(p.supports_pack_format(p.pack_format));
    assert!(p.supports_pack_format(p.supported_pack_formats.0));
    assert!(p.supports_pack_format(p.supported_pack_formats.1));
    assert!(!p.supports_pack_format(p.supported_pack_formats.0 - 1));
    assert!(!p.supports_pack_format(p.supported_pack_formats.1 + 1));
}

// --- Task A3: version-drift profiles, measured from the real jars -------------
//
// The named legacy profiles encode facts verified against the actual
// 1.8.9/1.12.2 client jars (plural texture dirs; no declarative atlas index),
// not documentation.

#[test]
fn legacy_1_8_profile_matches_the_real_jar() {
    let p = AssetProfile::LEGACY_1_8;
    // 1.8.9 client.jar: textures/blocks/ (382), textures/items/ (229).
    assert_eq!(p.block_texture_dir, "blocks");
    assert_eq!(p.item_texture_dir, "items");
    // 1.8.x resource packs declare pack_format 1 (SharedConstants; the jar
    // itself carries no pack.mcmeta/version.json — it's a code constant there).
    assert_eq!(p.pack_format, 1);
    // 1.8.9 has no atlases/*.json — the terrain sheet is implicit.
    assert!(!p.uses_atlas_index);
}

#[test]
fn legacy_1_12_profile_matches_the_real_jar() {
    let p = AssetProfile::LEGACY_1_12;
    // 1.12.2 client.jar: textures/blocks/ (500), textures/items/ (343), and
    // still no atlases/ index. pack_format 3 for the 1.11–1.12.2 line.
    assert_eq!(p.block_texture_dir, "blocks");
    assert_eq!(p.item_texture_dir, "items");
    assert_eq!(p.pack_format, 3);
    assert!(!p.uses_atlas_index);
}

#[test]
fn modern_profile_uses_declarative_atlas_index() {
    // 26.2 ships 13 atlases/*.json source lists.
    let p = AssetProfile::MODERN;
    assert!(p.uses_atlas_index);
}
