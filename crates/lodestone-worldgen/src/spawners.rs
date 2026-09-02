//! Biome mob-spawn settings — vanilla's per-biome `spawners` and `spawn_costs`
//!.
//!
//! ## What it is
//!
//! Every one of 26.2's 66 overworld biome documents carries a `spawners` map and
//! a `spawn_costs` map, and until this module **nothing in the workspace parsed
//! either** — the data shipped in `crates/lodestone-server/assets/worldgen/biome/*.json`
//! and was read by no code at all. This module turns those two fields into typed
//! Rust, and [`crate::overworld::OverworldGenerator::biome_spawners`] exposes the
//! per-biome answer.
//!
//! ## This is deliberately only the parse
//!
//! The SPAWN generation work has four parts and this is part 1. There is
//! **no `SPAWN` generation step here**, and that is not an oversight: the
//! three unported parts need things that live
//! outside this crate: vanilla's own chunk-generation spawn-rule check
//! (`crates/lodestone-entity/src/spawn.rs`), an entity slot on the served chunk,
//! and entity persistence, which does not exist anywhere yet. A `SPAWN` stage
//! built on top of this today would place mobs from a light level the server
//! does not compute, which is the *world* species of vacuous test in
//! `CLAUDE.md`'s table — green against the only input it can be handed.
//!
//! So: **this module is data, not behaviour.** Its consumer, at the time this
//! was written, was a runtime spawner that did not exist yet; parts 2-4 have
//! since landed (`crate::spawn_stage` for chunk-generation spawns,
//! `lodestone_server::natural_spawn` for the tick-driven cycle and the
//! `SpawnConditions`-equivalent revalidation), so [`parse_biome_spawners`]
//! is no longer an island — both consumers call it directly rather than
//! through [`crate::overworld::OverworldGenerator::biome_spawners`], which
//! stays unused (see that method's own doc).
//!
//! ## How it works
//!
//! [`parse_biome_spawners`] reads a whole biome document (the same
//! [`Resolver::biome_document`](crate::density::Resolver::biome_document) value
//! [`crate::feature::top_layer::parse_biome_climate`] already consumes, so this
//! costs no extra JSON parse) and yields a [`BiomeSpawners`].
//!
//! ## How to change it
//!
//! * A new [`MobCategory`] variant means a new key in vanilla's `MobCategory`
//!   enum. [`MobCategory::parse`] **panics** on an unknown key rather than
//!   dropping it, for the same reason
//!   [`TemperatureModifier::parse`](crate::feature::top_layer::TemperatureModifier::parse)
//!   does: a silently-dropped category is a whole missing mob class, and it
//!   would read as a subtle spawn-rate residual instead of a missing port.
//! * The weights are **list weights, not a field of the per-entry record.**
//!   Vanilla's own spawner-entry record carries only the entity type and
//!   min/max count, and the `weight` key belongs to the weighted-list
//!   wrapper one level out.
//!   [`SpawnerEntry`] flattens the two because nothing here needs the
//!   distinction, but a port of vanilla's weighted pick must read
//!   [`SpawnerEntry::weight`] as the *list* weight.
//!
//! ## Two vanilla behaviours deliberately not modelled, both named
//!
//! * **Vanilla's own per-entry record construction rewrites a
//!   `MISC`-category entity type to the pig entity type.** Reproducing
//!   it needs an entity-type -> category table, which this crate does not
//!   have and should not grow (it belongs with `lodestone-entity`). It is also
//!   **unreachable from 26.2's own data**: measured across all 66 bundled biome
//!   documents, the `misc` list is empty in every one of them (`ambient` 54,
//!   `creature` 43, `monster` 63, `underground_water_creature` 53,
//!   `water_ambient` 13, `water_creature` 11, `axolotls` 1, `misc` **0**). A
//!   consumer that gains a category table should apply the rewrite there.
//! * **Vanilla's own positive-integer and `minCount <= maxCount` validation**
//!   is not enforced. These are embedded,
//!   generated assets, so a violation is a build-time defect rather than
//!   untrusted input, and the same convention every other parser in this crate
//!   follows.
//!
//! ## Dependencies
//!
//! `serde_json` only. Nothing in this module reads noise, RNG or the block grid.

use std::collections::BTreeMap;

use serde_json::Value;

/// Vanilla's own mob-category enum, in
/// declaration order — which is also its own encoded key order and the
/// order its per-category iteration follows, so a consumer that must match vanilla's
/// per-category iteration can rely on [`MobCategory::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MobCategory {
    /// `MONSTER("monster", "MO", 70, false, false, 128)`.
    Monster,
    /// `CREATURE("creature", "C", 10, true, true, 128)`.
    Creature,
    /// `AMBIENT("ambient", "AM", 15, true, false, 128)`.
    Ambient,
    /// `AXOLOTLS("axolotls", "AX", 5, true, false, 128)`.
    Axolotls,
    /// `UNDERGROUND_WATER_CREATURE("underground_water_creature", "UWC", 5, true, false, 128)`.
    UndergroundWaterCreature,
    /// `WATER_CREATURE("water_creature", "WC", 5, true, false, 128)`.
    WaterCreature,
    /// `WATER_AMBIENT("water_ambient", "WA", 20, true, false, 64)`.
    WaterAmbient,
    /// `MISC("misc", "MI", -1, true, true, 128)`.
    Misc,
}

impl MobCategory {
    /// Every category, in vanilla's declaration order.
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

    /// The JSON/`StringRepresentable` key — `MobCategory`'s first constructor
    /// argument.
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

    /// Vanilla's own per-category concurrent-mob cap (the `max` field).
    /// `MISC` is `-1`, i.e. uncapped — vanilla's own
    /// sentinel, kept rather than mapped to an `Option` so the value a consumer
    /// compares against is byte-for-byte the one in the jar.
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
    /// Panics on an unrecognised key — see this module's "How to change it".
    #[must_use]
    pub fn parse(key: &str) -> Self {
        match MobCategory::ALL.into_iter().find(|c| c.key() == key) {
            Some(category) => category,
            None => panic!("unsupported MobCategory key: {key}"),
        }
    }
}

/// One entry of one category's `spawners` list: a `WeightedList` weight plus the
/// `MobSpawnSettings.SpawnerData` it wraps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnerEntry {
    /// `type` — an entity id (`minecraft:sheep`). Kept as a string because this
    /// crate has no entity registry and must not grow one; a consumer resolves
    /// it against `lodestone-entity`.
    pub entity_type: String,
    /// The `WeightedList` weight, **not** a `SpawnerData` field — see this
    /// module's "How to change it".
    pub weight: i32,
    /// `minCount`.
    pub min_count: i32,
    /// `maxCount`.
    pub max_count: i32,
}

/// Vanilla's own spawn-cost record, holding an energy budget and a charge.
///
/// Field order in the record is `(energyBudget, charge)` while the JSON keys are
/// alphabetical (`charge` first). Both are read by name, so the order is inert
/// here — recorded only because transcribing a positional record from a JSON
/// sample is exactly how `DepthStencilState(…, 1.0F, 10.0F)` got reversed
/// (`CLAUDE.md`, "Re-verify before routing around").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MobSpawnCost {
    /// `energy_budget`.
    pub energy_budget: f64,
    /// `charge`.
    pub charge: f64,
}

/// One biome's whole `MobSpawnSettings`, minus `creature_spawn_probability`
/// (which is a `BiomeGenerationSettings` neighbour, not part of this field pair,
/// and which no bundled 26.2 overworld biome overrides).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BiomeSpawners {
    /// Per-category spawner lists in **declaration order** of the list as it
    /// appears in the document. A category with an empty list is stored as an
    /// empty entry rather than omitted, so [`Self::for_category`] cannot confuse
    /// "declared empty" with "absent".
    spawners: BTreeMap<MobCategory, Vec<SpawnerEntry>>,
    /// `spawn_costs`, keyed by entity id. Non-empty for exactly 5 of the 66
    /// bundled biomes (all Nether); every overworld biome ships `{}`.
    spawn_costs: BTreeMap<String, MobSpawnCost>,
}

impl BiomeSpawners {
    /// This category's entries, or an empty slice.
    #[must_use]
    pub fn for_category(&self, category: MobCategory) -> &[SpawnerEntry] {
        self.spawners.get(&category).map_or(&[], Vec::as_slice)
    }

    /// The spawn cost for an entity id, if this biome declares one.
    #[must_use]
    pub fn spawn_cost(&self, entity_type: &str) -> Option<MobSpawnCost> {
        self.spawn_costs.get(entity_type).copied()
    }

    /// Every declared spawn cost, entity id -> cost.
    #[must_use]
    pub fn spawn_costs(&self) -> &BTreeMap<String, MobSpawnCost> {
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

/// Parses one biome document's `spawners` and `spawn_costs`.
///
/// Both fields are optional: a document with neither yields
/// [`BiomeSpawners::default`], which [`BiomeSpawners::is_empty`] reports as
/// empty. That is the same "missing data means do nothing, never assume"
/// convention every other resolver-fed parser in this crate follows, and it is
/// why every fixture `Resolver` in this workspace keeps working unchanged.
///
/// # Panics
/// Panics on a malformed document (a non-object `spawners`, a non-string `type`,
/// a `spawn_costs` entry missing `charge`/`energy_budget`, or an unknown
/// category key). These are embedded generated assets — a shape error is a
/// build-time defect, not untrusted input.
#[must_use]
pub fn parse_biome_spawners(document: &Value) -> BiomeSpawners {
    let mut spawners: BTreeMap<MobCategory, Vec<SpawnerEntry>> = BTreeMap::new();
    if let Some(map) = document.get("spawners").and_then(Value::as_object) {
        for (key, list) in map {
            let category = MobCategory::parse(key);
            let entries = list
                .as_array()
                .expect("spawners category is an array")
                .iter()
                .map(|entry| SpawnerEntry {
                    entity_type: entry["type"]
                        .as_str()
                        .expect("spawner entry type is a string")
                        .to_owned(),
                    weight: i32::try_from(entry["weight"].as_i64().expect("spawner entry weight"))
                        .expect("spawner weight fits i32"),
                    min_count: i32::try_from(
                        entry["minCount"].as_i64().expect("spawner entry minCount"),
                    )
                    .expect("spawner minCount fits i32"),
                    max_count: i32::try_from(
                        entry["maxCount"].as_i64().expect("spawner entry maxCount"),
                    )
                    .expect("spawner maxCount fits i32"),
                })
                .collect();
            spawners.insert(category, entries);
        }
    }
    let mut spawn_costs = BTreeMap::new();
    if let Some(map) = document.get("spawn_costs").and_then(Value::as_object) {
        for (entity_type, cost) in map {
            spawn_costs.insert(
                entity_type.clone(),
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
        spawners,
        spawn_costs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vanilla_category_key_round_trips() {
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
        assert_eq!(parsed.spawn_cost("minecraft:pig"), None);
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
        assert_eq!(creature[0].entity_type, "minecraft:sheep");
        assert_eq!(creature[0].weight, 12);
        assert_eq!(creature[0].min_count, 4);
        assert_eq!(creature[0].max_count, 4);
        assert_eq!(creature[1].entity_type, "minecraft:pig");

        assert_eq!(
            parsed.for_category(MobCategory::Monster)[0],
            SpawnerEntry {
                entity_type: "minecraft:zombie".to_owned(),
                weight: 95,
                min_count: 1,
                max_count: 4,
            }
        );
        // A declared-but-empty category is empty, not absent-and-guessed.
        assert_eq!(parsed.for_category(MobCategory::Misc), &[]);

        let cost = parsed.spawn_cost("minecraft:skeleton").expect("skeleton cost");
        // `energy_budget` and `charge` are read by name; a positional
        // transcription of the record would swap these two.
        assert!((cost.energy_budget - 0.15).abs() < 1e-12);
        assert!((cost.charge - 0.7).abs() < 1e-12);
        assert_eq!(parsed.spawn_cost("minecraft:ghast"), None);
    }

    /// The `max` column, against vanilla's own per-category constructor arguments.
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
