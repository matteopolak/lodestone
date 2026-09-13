use lodestone_worldgen::feature::{
    HeightProvider, IntProvider, Placement, RuleTest, parse_ore_config, parse_placements,
};

#[test]
fn ore_json_boundary_accepts_known_shapes_and_rejects_unknown_ones() {
    let placed = serde_json::json!({
        "placement": [
            {"type": "minecraft:count", "count": 3},
            {"type": "minecraft:rarity_filter", "chance": 4},
            {"type": "minecraft:in_square"},
            {"type": "minecraft:height_range", "height": {
                "type": "minecraft:uniform",
                "min_inclusive": {"absolute": 0},
                "max_inclusive": {"absolute": 10}
            }},
            {"type": "minecraft:biome"}
        ]
    });
    let placements = parse_placements(&placed);
    assert_eq!(placements.len(), 5);
    assert!(matches!(
        placements[0],
        Placement::Count(IntProvider::Constant(3))
    ));
    assert!(matches!(placements[1], Placement::RarityFilter(4)));
    assert!(matches!(placements[2], Placement::InSquare));
    assert!(matches!(
        placements[3],
        Placement::HeightRange(HeightProvider::Uniform { .. })
    ));
    assert!(matches!(placements[4], Placement::Biome));

    let config = serde_json::json!({
        "size": 1,
        "targets": [
            {
                "state": {"Name": "minecraft:iron_ore"},
                "target": {
                    "predicate_type": "minecraft:tag_match",
                    "tag": "minecraft:stone_ore_replaceables"
                }
            }
        ]
    });
    let ore = parse_ore_config(&config);
    assert!(matches!(
        ore.targets[0].target,
        RuleTest::TagMatch(ref value) if value == "minecraft:stone_ore_replaceables"
    ));
}

#[test]
#[should_panic(expected = "placement JSON")]
fn unknown_ore_placement_discriminator_is_rejected() {
    let unknown_placement = serde_json::json!({
        "placement": [{"type": "minecraft:not_a_placement"}]
    });
    let _ = parse_placements(&unknown_placement);
}

#[test]
#[should_panic(expected = "placement JSON")]
fn extra_ore_placement_field_is_rejected() {
    let extra_placement_field = serde_json::json!({
        "placement": [{"type": "minecraft:in_square", "unexpected": true}]
    });
    let _ = parse_placements(&extra_placement_field);
}

#[test]
#[should_panic(expected = "ore rule-test JSON")]
fn unknown_ore_rule_test_discriminator_is_rejected() {
    let unknown_rule_test = serde_json::json!({
        "size": 1,
        "targets": [{
            "state": {"Name": "minecraft:iron_ore"},
            "target": {
                "predicate_type": "minecraft:not_a_rule_test",
                "tag": "minecraft:stone_ore_replaceables"
            }
        }]
    });
    let _ = parse_ore_config(&unknown_rule_test);
}

#[test]
#[should_panic(expected = "ore rule-test JSON")]
fn extra_ore_rule_test_field_is_rejected() {
    let extra_rule_test_field = serde_json::json!({
        "size": 1,
        "targets": [{
            "state": {"Name": "minecraft:iron_ore"},
            "target": {
                "predicate_type": "minecraft:block_match",
                "block": "minecraft:stone",
                "unexpected": true
            }
        }]
    });
    let _ = parse_ore_config(&extra_rule_test_field);
}
