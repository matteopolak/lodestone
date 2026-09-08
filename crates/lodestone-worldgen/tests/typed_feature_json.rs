//! External controls for the strict configured/placed-feature JSON boundary.
//!
//! These tests intentionally enter through the public parsers, so they prove
//! that malformed data is rejected at the same boundary production composition
//! uses rather than only exercising private serde structs.

use lodestone_worldgen::feature::{canon_state, parse_ore_config, parse_placements};
use lodestone_worldgen::feature::top_layer::SnowSupport;
use serde_json::json;

#[test]
#[should_panic(expected = "unknown field `unexpected`")]
fn placement_unknown_field_is_rejected_with_path() {
    let _ = parse_placements(&json!({
        "feature": "minecraft:ore_dirt",
        "placement": [{
            "type": "minecraft:count",
            "count": 3,
            "unexpected": true
        }]
    }));
}

#[test]
#[should_panic(expected = "placed_feature: unknown variant `minecraft:not_a_modifier`")]
fn placement_unknown_discriminator_is_rejected() {
    let _ = parse_placements(&json!({
        "feature": "minecraft:ore_dirt",
        "placement": [{"type": "minecraft:not_a_modifier"}]
    }));
}

#[test]
#[should_panic(expected = "unknown field `not_a_field`")]
fn ore_unknown_field_and_malformed_rule_are_rejected() {
    let _ = parse_ore_config(&json!({
        "size": 9,
        "targets": [],
        "not_a_field": 1
    }));
}

#[test]
#[should_panic(expected = "configured_feature.config.targets[0].target: unsupported rule test type")]
fn ore_malformed_rule_is_rejected_with_path() {
    let _ = parse_ore_config(&json!({
        "size": 9,
        "targets": [{
            "state": {"Name": "minecraft:iron_ore"},
            "target": {"predicate_type": "minecraft:not_a_rule"}
        }]
    }));
}

#[test]
fn block_properties_remain_dynamic_but_canonical() {
    let state = json!({
        "Name": "minecraft:ore",
        "Properties": {"z": "1", "a": "0"}
    });
    assert_eq!(canon_state(&state), "minecraft:ore[a=0,z=1]");
}

#[test]
#[should_panic(expected = "block_freeze_facts: unknown field `misspelled`")]
fn freeze_fact_unknown_column_is_rejected() {
    let _ = SnowSupport::parse(
        &json!({"misspelled": {"default": [], "states": {}}}),
        Default::default(),
        Default::default(),
    );
}

#[test]
#[should_panic(expected = "block_freeze_facts: invalid type: string")]
fn freeze_fact_state_values_are_boolean() {
    let _ = SnowSupport::parse(
        &json!({
            "blocks_motion": {"default": [], "states": {"minecraft:stone": "yes"}}
        }),
        Default::default(),
        Default::default(),
    );
}
