//! Compact projection of the official biome resources, independent of the parser.

use std::collections::BTreeMap;

use lodestone_data::entity_type::{EntityType, EntityTypeRef};
use lodestone_worldgen::spawners::{MobCategory, parse_biome_spawners};
use serde_json::{Value, json};

const NATURAL: &str = "minecraft:gameplay/natural_mob_spawns";
const PROBABILITY: &str = "minecraft:gameplay/creature_world_gen_spawn_probability";

fn corpus() -> Value {
    serde_json::from_str(include_str!("support/biome-spawns-26-3.json")).unwrap()
}

fn document(corpus: &Value, biome: &Value) -> Value {
    let categories: serde_json::Map<String, Value> = biome["categories"]
        .as_object().unwrap().iter()
        .map(|(category, indices)| {
            let entries: Vec<Value> = indices.as_array().unwrap().iter()
                .map(|index| corpus["entries"][index.as_u64().unwrap() as usize].clone())
                .collect();
            (category.clone(), Value::Array(entries))
        }).collect();
    let mut document = json!({"attributes": {NATURAL: {
        "modifier": "overlay",
        "argument": {"spawns_by_category": categories, "spawn_costs": biome["costs"]}
    }}});
    if let Some(probability) = biome.get("probability") {
        document["attributes"][PROBABILITY] = probability.clone();
    }
    document
}

#[test]
fn shipped_corpus_preserves_every_ordered_entry_count_cost_and_probability() {
    let corpus = corpus();
    let biomes = corpus["biomes"].as_array().unwrap();
    assert_eq!(biomes.len(), 67);
    assert_eq!(corpus["entries"].as_array().unwrap().len(), 108);
    let mut category_totals = BTreeMap::new();
    let mut uniform_ranges = BTreeMap::new();
    let mut constants = 0;
    let mut uniforms = 0;
    let mut entries = 0;
    let mut cost_biomes = 0;
    let mut overrides = BTreeMap::new();
    for biome in biomes {
        let document = document(&corpus, biome);
        let parsed = parse_biome_spawners(&document);
        entries += parsed.entry_count();
        for (category, source) in document["attributes"][NATURAL]["argument"]["spawns_by_category"]
            .as_object().unwrap()
        {
            let source = source.as_array().unwrap();
            let actual = parsed.for_category(MobCategory::parse(category));
            assert_eq!(actual.len(), source.len(), "{} {category}", biome["id"]);
            *category_totals.entry(category.clone()).or_insert(0) += actual.len();
            for (actual, source) in actual.iter().zip(source) {
                let entity = EntityTypeRef::from(EntityType::from_name(source["type"].as_str().unwrap()).unwrap());
                assert_eq!(actual.entity_type, entity, "{} {category}", biome["id"]);
                assert_eq!(actual.weight, source["weight"].as_i64().unwrap() as i32);
                let expected = if let Some(count) = source["count"].as_i64() {
                    constants += 1;
                    (count as i32, count as i32)
                } else {
                    uniforms += 1;
                    let bounds = (source["count"]["min_inclusive"].as_i64().unwrap() as i32,
                        source["count"]["max_inclusive"].as_i64().unwrap() as i32);
                    *uniform_ranges.entry(bounds).or_insert(0) += 1;
                    assert!(bounds.0 < bounds.1);
                    bounds
                };
                assert_eq!((actual.min_count, actual.max_count), expected);
            }
        }
        let costs = biome["costs"].as_object().unwrap();
        cost_biomes += usize::from(!costs.is_empty());
        assert_eq!(parsed.spawn_costs().len(), costs.len());
        for (entity, expected) in costs {
            let entity = EntityTypeRef::from(EntityType::from_name(entity).unwrap());
            let actual = parsed.spawn_cost(entity).unwrap();
            assert_eq!(actual.charge, expected["charge"].as_f64().unwrap());
            assert_eq!(actual.energy_budget, expected["energy_budget"].as_f64().unwrap());
        }
        let probability = biome.get("probability").and_then(Value::as_f64).unwrap_or(0.1) as f32;
        assert_eq!(parsed.creature_spawn_probability(), probability);
        if biome.get("probability").is_some() {
            overrides.insert(biome["id"].as_str().unwrap(), probability);
        }
    }
    assert_eq!(entries, 811);
    assert_eq!((constants, uniforms, cost_biomes), (605, 206, 2));
    assert_eq!(category_totals, BTreeMap::from([
        ("ambient", 55), ("axolotls", 1), ("creature", 176), ("misc", 0),
        ("monster", 480), ("underground_water_creature", 54),
        ("water_ambient", 20), ("water_creature", 25),
    ].map(|(category, total)| (category.to_owned(), total))));
    assert_eq!(uniform_ranges, BTreeMap::from([
        ((1, 2), 24), ((1, 3), 9), ((1, 4), 62), ((1, 5), 6),
        ((2, 3), 13), ((2, 4), 12), ((2, 5), 4), ((2, 6), 7),
        ((3, 4), 2), ((3, 6), 6), ((4, 6), 58), ((4, 8), 3),
    ]));
    assert_eq!(overrides, BTreeMap::from([
        ("minecraft:badlands", 0.03), ("minecraft:eroded_badlands", 0.03),
        ("minecraft:ice_spikes", 0.07), ("minecraft:snowy_plains", 0.07),
        ("minecraft:wooded_badlands", 0.04),
    ]));
}

#[test]
fn removing_current_attributes_and_probability_trips_the_corpus_controls() {
    let corpus = corpus();
    let mut missing_entries = 0;
    let mut lost_overrides = 0;
    for biome in corpus["biomes"].as_array().unwrap() {
        let mut document = document(&corpus, biome);
        document["attributes"].as_object_mut().unwrap().remove(NATURAL);
        missing_entries += parse_biome_spawners(&document).entry_count();
        if biome.get("probability").is_some() {
            document["attributes"].as_object_mut().unwrap().remove(PROBABILITY);
            assert_eq!(parse_biome_spawners(&document).creature_spawn_probability(), 0.1);
            assert_ne!(biome["probability"].as_f64().unwrap() as f32, 0.1);
            lost_overrides += 1;
        }
    }
    assert_eq!(missing_entries, 0);
    assert_ne!(missing_entries, 811);
    assert_eq!(lost_overrides, 5);
}

#[test]
fn current_payload_wins_over_transitional_top_level_settings() {
    let document = json!({
        "attributes": {NATURAL: {"modifier": "overlay", "argument": {
            "spawns_by_category": {"creature": []}, "spawn_costs": {}
        }}, PROBABILITY: 0.07},
        "spawners": {"creature": [{"type": "minecraft:pig", "weight": 10, "minCount": 4, "maxCount": 4}]},
        "spawn_costs": {"minecraft:pig": {"charge": 1.0, "energy_budget": 2.0}},
        "creature_spawn_probability": 0.04
    });
    let parsed = parse_biome_spawners(&document);
    assert!(parsed.is_empty());
    assert_eq!(parsed.creature_spawn_probability(), 0.07);
    let mut without_override = document;
    without_override["attributes"].as_object_mut().unwrap().remove(PROBABILITY);
    assert_eq!(parse_biome_spawners(&without_override).creature_spawn_probability(), 0.1);
}

#[test]
fn unsupported_counts_modifiers_and_malformed_maps_fail_explicitly() {
    let mut cases = Vec::new();
    for (count, diagnostic) in [
        (json!(1.5), "unsupported spawn count provider"),
        (json!(2147483648_i64), "spawn count fits i32"),
        (json!({"type": "minecraft:weighted_list"}), "unsupported spawn count provider"),
        (json!({"type": "minecraft:uniform", "min_inclusive": 4, "max_inclusive": 3}),
            "uniform spawn count requires distinct ordered endpoints"),
        (json!({"type": "minecraft:uniform", "min_inclusive": 4, "max_inclusive": 4}),
            "uniform spawn count requires distinct ordered endpoints"),
        (json!({"type": "minecraft:uniform", "min_inclusive": -2147483648_i64, "max_inclusive": 2147483647}),
            "uniform spawn count bound fits i32"),
    ] {
        let document = json!({"attributes": {NATURAL: {"modifier": "overlay", "argument": {
            "spawns_by_category": {"creature": [{"type": "minecraft:pig", "weight": 10, "count": count}]}
        }}}});
        cases.push((document, diagnostic));
    }
    cases.extend([
        (json!({"attributes": []}), "biome attributes are an object"),
        (json!({"attributes": {NATURAL: {"modifier": "replace", "argument": {}}}}),
            "unsupported natural spawn attribute modifier"),
        (json!({"attributes": {NATURAL: {"modifier": "overlay", "argument": []}}}),
            "natural spawn attribute argument is an object"),
        (json!({"attributes": {NATURAL: {"modifier": "overlay", "argument": {"spawns_by_category": []}}}}),
            "spawn categories are an object"),
        (json!({"attributes": {NATURAL: {"modifier": "overlay", "argument": {"spawn_costs": []}}}}),
            "spawn costs are an object"),
        (json!({"attributes": {PROBABILITY: 1.0}}),
            "creature spawn probability must be in [0, 0.9999999]"),
    ]);

    const CHILD_CASE: &str = "LODESTONE_BIOME_SPAWN_REJECTION_CASE";
    if let Ok(index) = std::env::var(CHILD_CASE) {
        let index: usize = index.parse().expect("child rejection case index");
        let _ = parse_biome_spawners(&cases[index].0);
        return;
    }
    for (index, (_, expected)) in cases.iter().enumerate() {
        let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
            .args(["--exact", "unsupported_counts_modifiers_and_malformed_maps_fail_explicitly", "--nocapture"])
            .env(CHILD_CASE, index.to_string())
            .output()
            .expect("run the biome spawn rejection control");
        let diagnostic = format!("{}{}", String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
        assert!(!output.status.success(),
            "case {index} unexpectedly accepted unsupported settings:\n{diagnostic}");
        assert!(diagnostic.contains(*expected),
            "case {index} failed without expected diagnostic {expected:?}:\n{diagnostic}");
    }
}
