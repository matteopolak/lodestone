//! The map decoration sheet against the real jar: every decoration type the
//! renderer knows has a sprite, every sprite on the sheet belongs to a type, and
//! the type table lists the registry in the jar's own protocol order.
//!
//! ```text
//! cargo test -p lodestone-render --test items map_decoration -- --ignored --nocapture
//! ```

use std::collections::BTreeSet;

use lodestone_assets::map_decoration_atlas::load_map_decoration_atlas;
use lodestone_assets::{ResourceLocation, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::map_item::MAP_DECORATION_TYPES;

#[path = "../gate_harness/mod.rs"]
mod gate_harness;

fn jar_manager() -> ResourceManager {
    let path = gate_harness::require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = ZipSource::from_bytes(bytes).unwrap_or_else(|e| panic!("open jar: {e}"));
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

#[test]
#[ignore = "requires the vanilla client.jar"]
fn map_decoration_types_and_sheet_sprites_cover_each_other() {
    let atlas = load_map_decoration_atlas(&jar_manager()).expect("the jar's decoration atlas descriptor loads");
    let on_sheet: BTreeSet<String> = atlas.sprites().iter().map(|sprite| sprite.location.to_string()).collect();
    let wanted: BTreeSet<String> = MAP_DECORATION_TYPES
        .iter()
        .map(|ty| format!("minecraft:{}", ty.sprite))
        .collect();
    assert_eq!(
        wanted.difference(&on_sheet).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "a decoration type names a sprite the jar does not ship"
    );
    assert_eq!(
        on_sheet.difference(&wanted).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the jar ships a decoration sprite no type uses"
    );
    for sprite in atlas.sprites() {
        assert_eq!((sprite.width, sprite.height), (8, 8), "{}", sprite.location);
        let _: &ResourceLocation = &sprite.location;
    }
}

#[test]
#[ignore = "requires the vanilla client.jar and its generated registry report"]
fn map_decoration_type_table_follows_the_jar_registry_order() {
    let jar = gate_harness::require_client_jar();
    let report = gate_harness::require_blocks_report(&jar)
        .parent()
        .expect("the blocks report sits in a reports directory")
        .join("registries.json");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap_or_else(|e| panic!("read {}: {e}", report.display())))
            .expect("registries.json parses");
    let entries = json["minecraft:map_decoration_type"]["entries"]
        .as_object()
        .expect("the report lists the map decoration type registry");
    assert_eq!(entries.len(), MAP_DECORATION_TYPES.len());
    for (name, entry) in entries {
        let id = entry["protocol_id"].as_u64().expect("protocol_id") as usize;
        let ty = MAP_DECORATION_TYPES.get(id).unwrap_or_else(|| panic!("{name}: id {id} is past the table"));
        assert_eq!(format!("minecraft:{}", ty.key), *name, "table entry {id}");
    }
}
