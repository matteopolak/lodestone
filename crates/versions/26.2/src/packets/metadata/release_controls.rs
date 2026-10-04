use super::*;
use crate::packets::registry::{ClientRegistries, PackedRegistryEntry, RegistryData};
use lodestone_data::GameDataVersion;

fn latest() -> ProtocolDialect {
    ProtocolDialect::v26_2().with_game_data_version(GameDataVersion::V26_3)
}

#[test]
fn selected_block_display_state_is_canonical_before_it_is_reported() {
    let registries = ClientRegistries::default();
    let tracked = TrackedEntity { class: Some(MetadataClass::BlockDisplay), ..TrackedEntity::default() };
    for (dialect, bytes) in [
        (ProtocolDialect::v26_2(), [23, 14, 0x93, 0x54, 255]),
        (latest(), [23, 14, 0xe2, 0x61, 255]),
    ] {
        let mut reader = Reader::new(&bytes);
        let decoded = read_entity_metadata_with(&mut reader, tracked, &StackCodecContext::new(dialect, &registries)).unwrap();
        reader.ensure_empty().unwrap();
        assert_eq!(decoded.metadata.display_block_state, Some(BlockStateRef::canonical(10771)));
    }
    let mut reader = Reader::new(&[23, 14, 0xe2, 0x61, 255]);
    let wrong = read_entity_metadata(&mut reader, tracked).unwrap();
    assert_ne!(wrong.metadata.display_block_state, Some(BlockStateRef::canonical(10771)));
}

#[test]
fn particle_list_consumes_selected_options_before_the_following_health() {
    let registries = ClientRegistries::default();
    let context = StackCodecContext::new(latest(), &registries);
    let bytes = [10, 17, 1, 1, 0xe2, 0x61, 9, 3, 0x41, 0x18, 0, 0, 255];
    let mut reader = Reader::new(&bytes);
    let decoded = read_entity_metadata_with(&mut reader, TrackedEntity::default(), &context).unwrap();
    reader.ensure_empty().unwrap();
    assert!(decoded.complete);
    assert_eq!(decoded.metadata.health, Some(9.5));
    assert!(read_entity_metadata_with(&mut Reader::new(&bytes[..bytes.len() - 1]), TrackedEntity::default(), &context).is_err());
}

#[test]
fn nested_item_uses_the_selected_item_census() {
    let registries = ClientRegistries::default();
    let context = StackCodecContext::new(latest(), &registries);
    let bytes = [8, 7, 2, 0xf4, 7, 0, 0, 9, 3, 0x41, 0x18, 0, 0, 255];
    let mut reader = Reader::new(&bytes);
    let decoded = read_entity_metadata_with(&mut reader, TrackedEntity::default(), &context).unwrap();
    reader.ensure_empty().unwrap();
    let Reported::Reported(Some(stack)) = decoded.metadata.item else { panic!("missing nested item"); };
    assert_eq!(stack.item.to_string(), "minecraft:diamond");
    assert_eq!(stack.count, 2);
    assert_eq!(decoded.metadata.health, Some(9.5));
}

#[test]
fn appearance_uses_connection_order_and_missing_sync_is_not_a_global_fallback() {
    let mut registries = ClientRegistries::default();
    registries.apply(RegistryData {
        registry: "minecraft:wolf_variant".into(),
        entries: vec![
            PackedRegistryEntry { id: "test:first".into(), data: None },
            PackedRegistryEntry { id: "test:second".into(), data: None },
        ],
    });
    let context = StackCodecContext::new(latest(), &registries);
    let bytes = [18, 25, 2, 255];
    let decoded = read_entity_metadata_with(&mut Reader::new(&bytes), TrackedEntity::default(), &context).unwrap();
    assert_eq!(decoded.metadata.variant, Some(EntityVariant::Keyed("test:second".parse().unwrap())));
    let missing = ClientRegistries::default();
    assert!(read_entity_metadata_with(&mut Reader::new(&bytes), TrackedEntity::default(),
        &StackCodecContext::new(latest(), &missing)).is_err());
}

#[test]
fn appended_dye_serializer_is_selected_and_class_gated() {
    let registries = ClientRegistries::default();
    let context = StackCodecContext::new(latest(), &registries);
    let tracked = TrackedEntity { class: Some(MetadataClass::Cushion), ..TrackedEntity::default() };
    for (ordinal, color) in [(13, 13), (99, 0)] {
        let bytes = [8, 43, ordinal, 255];
        let decoded = read_entity_metadata_with(&mut Reader::new(&bytes), tracked, &context).unwrap();
        assert_eq!(decoded.metadata.variant, Some(EntityVariant::Dyed { color, sheared: false }));
        assert!(read_entity_metadata(&mut Reader::new(&bytes), tracked).is_err());
        let unrelated = read_entity_metadata_with(&mut Reader::new(&bytes), TrackedEntity::default(), &context).unwrap();
        assert_eq!(unrelated.metadata.variant, None);
    }
}

#[test]
fn attribute_reader_uses_selected_fixed_registry_translation() {
    fn decode(kind: FixedRegistryKind, raw: i32) -> Option<i32> {
        (kind == FixedRegistryKind::Attribute && raw == 26).then_some(1)
    }
    static MAPPING: crate::dialect::FixedRegistryMappings = crate::dialect::FixedRegistryMappings {
        decode, encode: |_, _| None, name: |_, _| None,
    };
    let bytes = [0xa3, 2, 1, 26, 0x3f, 0xfc, 0, 0, 0, 0, 0, 0, 0];
    let (_, original) = read_update_attributes(&mut Reader::new(&bytes)).unwrap();
    assert_eq!(original[0].attribute.to_string(), "minecraft:movement_speed");
    let mut reader = Reader::new(&bytes);
    let (entity, selected) = read_update_attributes_with(&mut reader,
        latest().with_fixed_registries(&MAPPING)).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(entity, 291);
    assert_eq!(selected[0].attribute.to_string(), "minecraft:armor");
    assert_eq!(selected[0].base, 1.75);
}
