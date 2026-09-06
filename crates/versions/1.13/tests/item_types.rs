//! Hermetic and drift checks for the protocol-404 flattened item registry.

use std::path::PathBuf;

use lodestone_v1_13::item_types::{self, ITEM_TYPE_COUNT};

#[test]
fn low_middle_and_high_protocol_404_ids_resolve_independently() {
    assert_eq!(ITEM_TYPE_COUNT, 789);
    assert_eq!(item_types::item_name(1), Some("minecraft:stone"));
    assert_eq!(item_types::item_name(493), Some("minecraft:diamond_sword"));
    assert_eq!(item_types::item_name(789), Some("minecraft:heart_of_the_sea"));
    assert_eq!(item_types::item_name(0), None);
    assert_eq!(item_types::item_name(790), None);
}

#[test]
#[ignore = "reads the gitignored vendor/minecraft-data source"]
fn committed_table_matches_the_vendored_protocol_404_census() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../vendor/minecraft-data/data/pc/1.13.2/items.json");
    let raw = std::fs::read_to_string(&source)
        .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    let entries: serde_json::Value = serde_json::from_str(&raw).expect("items.json parses");
    let entries = entries.as_array().expect("items.json is an array");
    assert_eq!(entries.len(), ITEM_TYPE_COUNT);
    for entry in entries {
        let id = entry["id"].as_i64().expect("item id") as i32;
        let name = entry["name"].as_str().expect("item name");
        assert_eq!(item_types::item_name(id), Some(format!("minecraft:{name}").as_str()));
    }
}
