//! Hermetic controls for the committed 26.2 data-component-type registry.
//!
//! The typed boundary is tested with literal wire ids as well as a table-wide
//! round trip. The literals make a shifted or reordered generated table fail
//! without deriving both expected values from that same table.

use lodestone_data::data_component_types::{
    self, DATA_COMPONENT_TYPE_COUNT, DataComponentTypeId,
};

#[test]
fn data_component_type_id_validates_the_table_domain() {
    for raw in 0..DATA_COMPONENT_TYPE_COUNT as i32 {
        let id = DataComponentTypeId::new(raw).expect("table id validates");
        assert!(
            !data_component_types::component_type_name(id).is_empty(),
            "id {id:?} in 0..{DATA_COMPONENT_TYPE_COUNT} did not resolve to a name"
        );
    }

    assert_eq!(DataComponentTypeId::new(-1), None);
    assert_eq!(DataComponentTypeId::new(DATA_COMPONENT_TYPE_COUNT as i32), None);
    assert_eq!(DataComponentTypeId::new(i32::MAX), None);

    let lookup: fn(DataComponentTypeId) -> &'static str = data_component_types::component_type_name;
    let reverse: fn(&str) -> Option<DataComponentTypeId> = data_component_types::component_type_id;
    let custom_data = DataComponentTypeId::new(0).expect("known id validates");
    assert_eq!(reverse(lookup(custom_data)), Some(custom_data));
}

#[test]
fn literal_wire_ids_resolve_to_their_identifiers() {
    assert_eq!(
        data_component_types::component_type_name(
            DataComponentTypeId::new(0).expect("custom_data id validates"),
        ),
        "minecraft:custom_data"
    );
    assert_eq!(
        data_component_types::component_type_name(
            DataComponentTypeId::new(28).expect("tool id validates"),
        ),
        "minecraft:tool"
    );
    assert_eq!(
        data_component_types::component_type_name(
            DataComponentTypeId::new(110).expect("shulker_color id validates"),
        ),
        "minecraft:shulker/color"
    );
}

#[test]
fn custom_or_future_name_does_not_acquire_a_builtin_id() {
    assert_eq!(
        data_component_types::component_type_id("example:custom_component"),
        None
    );
}
