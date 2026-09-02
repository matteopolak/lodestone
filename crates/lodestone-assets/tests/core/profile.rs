//! Tests for [`AssetProfile`], the version-specific convention seam.

use lodestone_assets::AssetProfile;

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

