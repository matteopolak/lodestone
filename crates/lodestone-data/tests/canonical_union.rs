use lodestone_data::block::Block;
use lodestone_data::block_properties::Properties;
use lodestone_data::block_states::{self, StateId};
use lodestone_data::item::Item;
use lodestone_data::{
    block_blast, block_entity_types, block_items, block_solidity, block_survival,
    collision_shapes, face_occlusion, hardness, item_prototypes, light_props,
    outline_shapes, path_types, shade_brightness, snow_support, sound_types,
};

#[test]
fn appended_identities_reach_every_total_data_lookup() {
    assert_eq!((Block::COUNT, block_states::STATE_COUNT, Item::COUNT), (1_286, 35_723, 1_658));
    assert!(StateId::new(35_723).is_none());
    for raw in 32_366..35_723 {
        let state = StateId::new(raw).expect("appended state is canonical");
        let properties = Properties::from_state_id(state);
        assert_eq!(Properties::state_for_block(state.block(), &properties), Some(state));
        let _ = collision_shapes::collision_boxes(state);
        let _ = outline_shapes::outline_boxes(state);
        let _ = outline_shapes::interaction_boxes(state);
        let _ = light_props::light_props(state);
        let _ = hardness::hardness(state);
        let _ = block_solidity::legacy_solid(state);
        let _ = block_survival::solid_render(state);
        let _ = block_survival::sturdy_up(state);
        let _ = block_survival::center_support_down(state);
        let _ = block_survival::fire_flammable(state);
        let _ = face_occlusion::occlusion_mask(state);
        let _ = shade_brightness::shade_brightness(state);
        let _ = snow_support::face_full_up(state);
        let _ = snow_support::has_fluid_state(state);
        let _ = snow_support::is_water_source_liquid_block(state);
        let _ = snow_support::has_snowy_property(state);
        let _ = sound_types::sound_type(state);
        let _ = block_entity_types::block_entity_type(state);
        let _ = path_types::path_type(state);
        let _ = block_blast::explosion_resistance_for_state_id(state);
        let _ = block_blast::blast_for_block(state.block());
    }
    for raw in 1_537..1_658 {
        let item = Item::from_registry_id(raw).expect("appended item is canonical");
        let _ = block_items::block_placed_by(item);
        let _ = item_prototypes::prototype_for(item);
    }
}

#[test]
fn shifted_and_appended_report_witnesses_keep_distinct_meanings() {
    assert_eq!(Block::from_registry_id(23), Some(Block::BambooPlanks));
    assert_eq!(Block::from_registry_id(27), Some(Block::BirchSapling));
    assert_eq!(lodestone_data::GameDataVersion::V26_3.block_from_wire(23), Some(Block::PoplarPlanks));
    assert_eq!(Block::PoplarPlanks.registry_id(), 1_196);
    assert_eq!(Block::Water.default_state().raw(), 86);
    assert_eq!(Block::OakLog.default_state().raw(), 137);
    let high = StateId::new(35_722).expect("highest canonical state exists");
    assert_eq!(high.block(), Block::BlackConcreteSlab);
    assert_eq!(block_states::properties(high.raw()), Some(&[("type", "double"), ("waterlogged", "false")][..]));
    let boxes = collision_shapes::collision_boxes(high);
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0].min, [0.0, 0.0, 0.0]);
    assert_eq!(boxes[0].max, [1.0, 1.0, 1.0]);
    assert_eq!(light_props::light_props(high), (15, 0));
}

#[test]
fn base_only_regeneration_rejects_the_union_census() {
    const CHILD_CONTROL: &str = "LODESTONE_BASE_ONLY_GENERATION_CHILD";
    if std::env::var_os(CHILD_CONTROL).is_some() {
        include!("support/base-only-generation.rs");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args(["--exact", "base_only_regeneration_rejects_the_union_census", "--nocapture"])
        .env(CHILD_CONTROL, "1")
        .output()
        .expect("run the base-only regeneration rejection control");
    let diagnostic = format!("{}{}", String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success(), "base-only regeneration unexpectedly accepted the union");
    assert!(diagnostic.contains("This workflow only emits 26.2 tables."),
        "child failed without rejecting base-only regeneration:\n{diagnostic}");
}

#[test]
#[ignore = "requires both official report caches and all pinned behavior captures"]
fn committed_behavior_union_matches_inputs() {
    let output = std::process::Command::new("python3")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/behavior_union.py"))
        .arg("--runtime-check")
        .output()
        .expect("run the authoritative offline union drift check");
    assert!(output.status.success(), "union drift check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
}
