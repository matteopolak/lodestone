//! Typed biome category lists, spawn costs and generation-pack probability.
//!
//! Current biome attributes compile into the same [`BiomeSpawners`] consumed by
//! generation population and natural spawning. Parsing reuses the resolver's
//! biome document; sampling and terrain placement remain with those consumers.
//! Unsupported attribute/count shapes fail explicitly instead of losing mobs.

use std::collections::BTreeMap;

use lodestone_data::entity_type::{EntityType, EntityTypeRef};
use serde_json::Value;

/// Mob categories in the order used by category-wide spawn attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MobCategory {
    Monster,
    Creature,
    Ambient,
    Axolotls,
    UndergroundWaterCreature,
    WaterCreature,
    WaterAmbient,
    Misc,
}

impl MobCategory {
    /// Every category in spawn-attempt order.
    pub const ALL: [MobCategory; 8] = [
        MobCategory::Monster,
        MobCategory::Creature,
        MobCategory::Ambient,
        MobCategory::Axolotls,
        MobCategory::UndergroundWaterCreature,
        MobCategory::WaterCreature,
        MobCategory::WaterAmbient,
        MobCategory::Misc,
    ];

    /// The serialized category key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            MobCategory::Monster => "monster",
            MobCategory::Creature => "creature",
            MobCategory::Ambient => "ambient",
            MobCategory::Axolotls => "axolotls",
            MobCategory::UndergroundWaterCreature => "underground_water_creature",
            MobCategory::WaterCreature => "water_creature",
            MobCategory::WaterAmbient => "water_ambient",
            MobCategory::Misc => "misc",
        }
    }

    /// Concurrent-mob cap; miscellaneous entities use the uncapped sentinel -1.
    #[must_use]
    pub fn max_instances(self) -> i32 {
        match self {
            MobCategory::Monster => 70,
            MobCategory::Creature => 10,
            MobCategory::Ambient => 15,
            MobCategory::Axolotls
            | MobCategory::UndergroundWaterCreature
            | MobCategory::WaterCreature => 5,
            MobCategory::WaterAmbient => 20,
            MobCategory::Misc => -1,
        }
    }

    /// Parses a `spawners` map key.
    ///
    /// # Panics
    /// Panics on an unrecognised key.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        match MobCategory::ALL.into_iter().find(|c| c.key() == key) {
            Some(category) => category,
            None => panic!("unsupported MobCategory key: {key}"),
        }
    }
}

/// An ordered category-list entry with its list weight and inclusive count bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnerEntry {
    /// `type` — a validated built-in entity registry reference.
    pub entity_type: EntityTypeRef,
    /// Weight in the category's ordered list.
    pub weight: i32,
    /// Constant count, or uniform lower endpoint.
    pub min_count: i32,
    /// Constant count, or uniform upper endpoint. Equal bounds denote a constant.
    pub max_count: i32,
}

/// Per-entity energy budget and charge, read by name rather than position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MobSpawnCost {
    /// `energy_budget`.
    pub energy_budget: f64,
    /// `charge`.
    pub charge: f64,
}

/// One biome's creature generation probability, category lists and spawn costs.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeSpawners {
    creature_spawn_probability: f32,
    /// Per-category spawner lists in **declaration order** of the list as it
    /// appears in the document. A category with an empty list is stored as an
    /// empty entry rather than omitted, so [`Self::for_category`] cannot confuse
    /// "declared empty" with "absent".
    spawners: BTreeMap<MobCategory, Vec<SpawnerEntry>>,
    /// Spawn costs keyed by entity registry reference.
    spawn_costs: BTreeMap<EntityTypeRef, MobSpawnCost>,
}

impl Default for BiomeSpawners {
    fn default() -> Self {
        Self {
            creature_spawn_probability: 0.1,
            spawners: BTreeMap::new(),
            spawn_costs: BTreeMap::new(),
        }
    }
}

impl BiomeSpawners {
    /// Probability of another creature pack during first-time chunk generation.
    #[must_use]
    pub fn creature_spawn_probability(&self) -> f32 {
        self.creature_spawn_probability
    }

    /// This category's entries, or an empty slice.
    #[must_use]
    pub fn for_category(&self, category: MobCategory) -> &[SpawnerEntry] {
        self.spawners.get(&category).map_or(&[], Vec::as_slice)
    }

    /// The spawn cost for an entity id, if this biome declares one.
    #[must_use]
    pub fn spawn_cost(&self, entity_type: EntityTypeRef) -> Option<MobSpawnCost> {
        self.spawn_costs.get(&entity_type).copied()
    }

    /// Every declared spawn cost, entity id -> cost.
    #[must_use]
    pub fn spawn_costs(&self) -> &BTreeMap<EntityTypeRef, MobSpawnCost> {
        &self.spawn_costs
    }

    /// `true` when the biome declares no spawner entry in any category and no
    /// spawn cost — the "no data supplied" answer, and what
    /// [`parse_biome_spawners`] returns for a document with neither field.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spawn_costs.is_empty() && self.spawners.values().all(Vec::is_empty)
    }

    /// Total spawner entries across every category — a non-degeneracy figure for
    /// a consumer's own gates, so "the parse ran" and "the parse found something"
    /// are separable.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.spawners.values().map(Vec::len).sum()
    }
}

const NATURAL_SPAWNS: &str = "minecraft:gameplay/natural_mob_spawns";
const GENERATION_PROBABILITY: &str = "minecraft:gameplay/creature_world_gen_spawn_probability";

fn count_integer(value: &Value) -> i32 {
    i32::try_from(value.as_i64().expect("spawn count is an integer"))
        .expect("spawn count fits i32")
}

fn count_bounds(value: &Value) -> (i32, i32) {
    if value.is_i64() || value.is_u64() {
        let count = count_integer(value);
        return (count, count);
    }
    assert_eq!(
        value.get("type").and_then(Value::as_str),
        Some("minecraft:uniform"),
        "unsupported spawn count provider"
    );
    let min = count_integer(&value["min_inclusive"]);
    let max = count_integer(&value["max_inclusive"]);
    // The bounds-only production representation uses equality for constants.
    // A degenerate uniform still consumes a draw, so it must not become one.
    assert!(min < max, "uniform spawn count requires distinct ordered endpoints");
    max.checked_sub(min).and_then(|span| span.checked_add(1))
        .expect("uniform spawn count bound fits i32");
    (min, max)
}

/// Compiles current biome spawn attributes into the shared production settings.
/// Missing attributes yield defaults; the old top-level fields remain accepted
/// only while the bundled resolver inputs are being replaced.
///
/// # Panics
/// Panics on malformed or unsupported settings, including a degenerate uniform
/// count that the current bounds-only representation cannot sample faithfully.
#[must_use]
pub fn parse_biome_spawners(document: &Value) -> BiomeSpawners {
    let attributes = document.get("attributes").map(|value| {
        value.as_object().expect("biome attributes are an object")
    });
    let current = attributes.and_then(|map| map.get(NATURAL_SPAWNS)).map(|value| {
        assert_eq!(
            value.get("modifier").and_then(Value::as_str),
            Some("overlay"),
            "unsupported natural spawn attribute modifier"
        );
        value.get("argument")
            .filter(|argument| argument.is_object())
            .expect("natural spawn attribute argument is an object")
    });
    let probability = attributes.and_then(|map| map.get(GENERATION_PROBABILITY))
        .or_else(|| {
            if current.is_none() { document.get("creature_spawn_probability") } else { None }
        });
    let creature_spawn_probability = probability.map_or(0.1, |value| {
        let probability = value.as_f64().expect("creature spawn probability is a number") as f32;
        assert!(
            (0.0..=0.9999999_f32).contains(&probability),
            "creature spawn probability must be in [0, 0.9999999]"
        );
        probability
    });
    let settings = current.unwrap_or(document);
    let category_key = if current.is_some() { "spawns_by_category" } else { "spawners" };
    let mut spawners: BTreeMap<MobCategory, Vec<SpawnerEntry>> = BTreeMap::new();
    if let Some(value) = settings.get(category_key) {
        let map = value.as_object().expect("spawn categories are an object");
        for (key, list) in map {
            let category = MobCategory::parse(key);
            let entries = list
                .as_array()
                .expect("spawners category is an array")
                .iter()
                .map(|entry| {
                    let (min_count, max_count) = if current.is_some() {
                        count_bounds(&entry["count"])
                    } else {
                        let min = count_integer(&entry["minCount"]);
                        let max = count_integer(&entry["maxCount"]);
                        assert!(min <= max, "spawn count endpoints must be ordered");
                        (min, max)
                    };
                    let weight = i32::try_from(entry["weight"].as_i64().expect("spawner entry weight"))
                        .expect("spawner weight fits i32");
                    assert!(weight >= 0, "spawner weight must be nonnegative");
                    SpawnerEntry {
                        entity_type: EntityType::from_name(
                            entry["type"]
                                .as_str()
                                .expect("spawner entry type is a string"),
                        )
                        .map(EntityTypeRef::from)
                        .unwrap_or_else(|| panic!("unsupported entity type in spawner entry")),
                        weight,
                        min_count,
                        max_count,
                    }
                })
                .collect();
            spawners.insert(category, entries);
        }
    }
    let mut spawn_costs = BTreeMap::new();
    if let Some(value) = settings.get("spawn_costs") {
        let map = value.as_object().expect("spawn costs are an object");
        for (entity_type, cost) in map {
            let entity_type = EntityType::from_name(entity_type)
                .map(EntityTypeRef::from)
                .unwrap_or_else(|| panic!("unsupported entity type in spawn cost: {entity_type}"));
            spawn_costs.insert(
                entity_type,
                MobSpawnCost {
                    energy_budget: cost["energy_budget"]
                        .as_f64()
                        .expect("spawn cost energy_budget"),
                    charge: cost["charge"].as_f64().expect("spawn cost charge"),
                },
            );
        }
    }
    BiomeSpawners {
        creature_spawn_probability,
        spawners,
        spawn_costs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_integer_counts_are_constants_without_positive_range_assumptions() {
        for count in [-3, 0, 1, 4, i32::MAX] {
            assert_eq!(count_bounds(&serde_json::json!(count)), (count, count));
        }
        assert_eq!(count_bounds(&serde_json::json!({
            "type": "minecraft:uniform", "min_inclusive": -2, "max_inclusive": 3
        })), (-2, 3));
    }

    #[test]
    fn creature_probability_defaults_and_preserves_biome_overrides() {
        assert_eq!(parse_biome_spawners(&serde_json::json!({})).creature_spawn_probability(), 0.1);
        for probability in [0.0, 0.03, 0.04, 0.07, 0.27] {
            let document = serde_json::json!({"creature_spawn_probability": probability});
            assert_eq!(parse_biome_spawners(&document).creature_spawn_probability(), probability as f32);
        }
    }

    #[test]
    #[should_panic(expected = "creature spawn probability must be")]
    fn probability_one_is_rejected_to_keep_generation_finite() {
        let _ = parse_biome_spawners(&serde_json::json!({"creature_spawn_probability": 1.0}));
    }

    #[test]
    fn every_category_key_round_trips() {
        for category in MobCategory::ALL {
            assert_eq!(MobCategory::parse(category.key()), category);
        }
        // The eight keys, spelled out rather than derived from `ALL`, so a typo
        // in `key()` cannot agree with itself.
        let keys: Vec<&str> = MobCategory::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(
            keys,
            vec![
                "monster",
                "creature",
                "ambient",
                "axolotls",
                "underground_water_creature",
                "water_creature",
                "water_ambient",
                "misc",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "unsupported MobCategory key")]
    fn an_unknown_category_is_a_hard_stop() {
        let _ = MobCategory::parse("dragons");
    }

    #[test]
    fn a_document_with_neither_field_is_empty_rather_than_defaulted() {
        let parsed = parse_biome_spawners(&serde_json::json!({"temperature": 0.8}));
        assert!(parsed.is_empty());
        assert_eq!(parsed.entry_count(), 0);
        assert_eq!(parsed.for_category(MobCategory::Monster), &[]);
        assert_eq!(parsed.spawn_cost(EntityType::Pig.into()), None);
        assert_eq!(parse_biome_spawners(&Value::Null), BiomeSpawners::default());
    }

    /// A verbatim slice of `assets/worldgen/biome/plains.json`, with a
    /// `spawn_costs` entry borrowed from `soul_sand_valley.json` so both halves
    /// of the parse are exercised by one fixture.
    #[test]
    fn parses_a_real_biome_document_slice() {
        let document = serde_json::json!({
            "spawn_costs": {
                "minecraft:skeleton": { "charge": 0.7, "energy_budget": 0.15 }
            },
            "spawners": {
                "ambient": [
                    { "type": "minecraft:bat", "maxCount": 8, "minCount": 8, "weight": 10 }
                ],
                "axolotls": [],
                "creature": [
                    { "type": "minecraft:sheep", "maxCount": 4, "minCount": 4, "weight": 12 },
                    { "type": "minecraft:pig", "maxCount": 4, "minCount": 4, "weight": 10 }
                ],
                "misc": [],
                "monster": [
                    { "type": "minecraft:zombie", "maxCount": 4, "minCount": 1, "weight": 95 }
                ],
                "underground_water_creature": [],
                "water_ambient": [],
                "water_creature": []
            }
        });
        let parsed = parse_biome_spawners(&document);
        assert!(!parsed.is_empty());
        assert_eq!(parsed.entry_count(), 4);

        // Declaration order inside a category is preserved: sheep before pig.
        let creature = parsed.for_category(MobCategory::Creature);
        assert_eq!(creature.len(), 2);
        assert_eq!(creature[0].entity_type, EntityType::Sheep.into());
        assert_eq!(creature[0].weight, 12);
        assert_eq!(creature[0].min_count, 4);
        assert_eq!(creature[0].max_count, 4);
        assert_eq!(creature[1].entity_type, EntityType::Pig.into());

        assert_eq!(
            parsed.for_category(MobCategory::Monster)[0],
            SpawnerEntry {
                entity_type: EntityType::Zombie.into(),
                weight: 95,
                min_count: 1,
                max_count: 4,
            }
        );
        // A declared-but-empty category is empty, not absent-and-guessed.
        assert_eq!(parsed.for_category(MobCategory::Misc), &[]);

        let cost = parsed
            .spawn_cost(EntityType::Skeleton.into())
            .expect("skeleton cost");
        // `energy_budget` and `charge` are read by name; a positional
        // transcription of the record would swap these two.
        assert!((cost.energy_budget - 0.15).abs() < 1e-12);
        assert!((cost.charge - 0.7).abs() < 1e-12);
        assert_eq!(parsed.spawn_cost(EntityType::Ghast.into()), None);
    }

    /// Independently captured category caps.
    #[test]
    fn per_category_caps_match_the_jar() {
        assert_eq!(MobCategory::Monster.max_instances(), 70);
        assert_eq!(MobCategory::Creature.max_instances(), 10);
        assert_eq!(MobCategory::Ambient.max_instances(), 15);
        assert_eq!(MobCategory::Axolotls.max_instances(), 5);
        assert_eq!(MobCategory::UndergroundWaterCreature.max_instances(), 5);
        assert_eq!(MobCategory::WaterCreature.max_instances(), 5);
        assert_eq!(MobCategory::WaterAmbient.max_instances(), 20);
        assert_eq!(MobCategory::Misc.max_instances(), -1);
    }
}
