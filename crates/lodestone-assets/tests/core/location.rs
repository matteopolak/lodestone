//! Tests for [`ResourceLocation`] parsing and formatting.

use lodestone_assets::ResourceLocation;

#[test]
fn defaults_namespace_to_minecraft() {
    let loc = ResourceLocation::parse("block/stone").unwrap();
    assert_eq!(loc.namespace(), "minecraft");
    assert_eq!(loc.path(), "block/stone");
}

#[test]
fn parses_explicit_namespace() {
    let loc = ResourceLocation::parse("mypack:custom/thing").unwrap();
    assert_eq!(loc.namespace(), "mypack");
    assert_eq!(loc.path(), "custom/thing");
}

#[test]
fn rejects_invalid_namespace_characters() {
    assert!(ResourceLocation::parse("MyPack:thing").is_err());
    assert!(ResourceLocation::parse("my pack:thing").is_err());
    assert!(ResourceLocation::parse("my/pack:thing").is_err());
}

#[test]
fn rejects_invalid_path_characters() {
    assert!(ResourceLocation::parse("minecraft:Block/Stone").is_err());
    assert!(ResourceLocation::parse("minecraft:block stone").is_err());
    assert!(ResourceLocation::parse("minecraft:block:stone").is_err());
}

#[test]
fn rejects_empty() {
    assert!(ResourceLocation::parse("").is_err());
    assert!(ResourceLocation::parse("minecraft:").is_err());
    assert!(ResourceLocation::parse(":stone").is_err());
}

#[test]
fn allows_valid_special_characters() {
    let loc = ResourceLocation::parse("my_pack.v2-beta:block/oak_log.2").unwrap();
    assert_eq!(loc.namespace(), "my_pack.v2-beta");
    assert_eq!(loc.path(), "block/oak_log.2");
}

#[test]
fn display_round_trips() {
    let loc = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(loc.to_string(), "minecraft:block/stone");

    let defaulted = ResourceLocation::parse("block/stone").unwrap();
    assert_eq!(defaulted.to_string(), "minecraft:block/stone");
}

#[test]
fn new_constructor_validates() {
    assert!(ResourceLocation::new("minecraft", "block/stone").is_ok());
    assert!(ResourceLocation::new("BAD", "block/stone").is_err());
}
