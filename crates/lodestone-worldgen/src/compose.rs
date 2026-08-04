//! Version-free composition glue for issue #295: resolves per-biome carver
//! lists, per-biome ore-feature lists, and block-tag closures from a
//! [`Resolver`], for [`crate::overworld::OverworldGenerator`] to consume when
//! composing carvers/features into the served chunk. Holds no data of its
//! own — everything here reads through the `Resolver` trait, matching every
//! other module in this crate (plan §3) — and every lookup degrades to "no
//! data for this id" rather than panicking, so a `Resolver` that only
//! supplies shape/surface data (most of this crate's own test fixtures) is
//! unaffected by this module existing.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::carver::CarverConfig;
use crate::density::Resolver;
use crate::feature::{PlacedOre, RuleTest, STEP_UNDERGROUND_ORES, parse_ore_config, parse_placements};

/// Recursively resolves a block tag's closure into a set of base block names.
/// Sub-tag references (`"#minecraft:..."`) recurse; plain ids are added
/// directly. Mirrors the resolution `carver_parity.rs`/`feature_parity.rs`'s
/// own hand-rolled `resolve_block_tag` test helpers already perform by
/// reading tag JSON straight off disk, just routed through
/// [`Resolver::block_tag`] so it also works against embedded server data
/// (and any other `Resolver`), not only a fixture directory.
///
/// A tag id with no data (`Resolver::block_tag`'s default `Value::Null`, or
/// a tag genuinely absent from the version's data) resolves to "no members"
/// rather than panicking — composition degrades gracefully, not loudly,
/// because an unrecognised tag is common (e.g. a `Resolver` that only ships
/// the shape/surface subset).
pub fn resolve_block_tag(
    resolver: &dyn Resolver,
    id: &str,
    out: &mut HashSet<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let doc = resolver.block_tag(id);
    let Some(values) = doc.get("values").and_then(Value::as_array) else {
        return;
    };
    for entry in values {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("id").and_then(Value::as_str).unwrap_or_default(),
            _ => continue,
        };
        if let Some(sub) = s.strip_prefix('#') {
            resolve_block_tag(resolver, sub, out, seen);
        } else if !s.is_empty() {
            out.insert(s.to_string());
        }
    }
}

/// Resolves one biome's `carvers` list into parsed [`CarverConfig`]s, in the
/// biome JSON's own declared order — `WorldgenRandom::set_large_feature_seed`'s
/// `index` (see [`crate::carver::apply_carvers`]) is the position in *this*
/// list, so order matters and must match vanilla's `BiomeGenerationSettings
/// .getCarvers()` (the JSON array's own order; nothing reorders it).
///
/// Empty if [`Resolver::biome_document`] has no data for `biome` — a biome
/// genuinely absent from the resolver's data carves nothing, matching the
/// pre-#295 behaviour for a `Resolver` that never implemented this method.
#[must_use]
pub fn build_biome_carvers(resolver: &dyn Resolver, biome: &str) -> Vec<CarverConfig> {
    let doc = resolver.biome_document(biome);
    let Some(carvers) = doc.get("carvers").and_then(Value::as_array) else {
        return Vec::new();
    };
    carvers
        .iter()
        .filter_map(Value::as_str)
        .map(|id| CarverConfig::parse(&resolver.configured_carver(id)))
        .collect()
}

/// Resolves one biome's `UNDERGROUND_ORES` decoration step
/// (`features[`[`STEP_UNDERGROUND_ORES`]`]`) into ordered [`PlacedOre`]s,
/// skipping non-ore entries in that step but **preserving their positions** —
/// the `index` `WorldgenRandom::set_feature_seed` uses is the entry's
/// position in the *raw* step array, not a count of ore-only entries, exactly
/// as `feature/mod.rs`'s own `build_plains_ores` test helper does for one
/// hardcoded biome file; this is the same logic against any biome a
/// [`Resolver`] supplies.
///
/// Empty if the resolver has no data for `biome`.
#[must_use]
pub fn build_biome_ores(resolver: &dyn Resolver, biome: &str) -> Vec<PlacedOre> {
    let doc = resolver.biome_document(biome);
    let Some(step) = doc
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(STEP_UNDERGROUND_ORES as usize))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut ores = Vec::new();
    for (i, entry) in step.iter().enumerate() {
        let Some(placed_id) = entry.as_str() else {
            continue;
        };
        let placed = resolver.placed_feature(placed_id);
        if placed.is_null() {
            continue;
        }
        let Some(cf_id) = placed.get("feature").and_then(Value::as_str) else {
            continue;
        };
        let configured = resolver.configured_feature(cf_id);
        if configured.get("type").and_then(Value::as_str) != Some("minecraft:ore") {
            // Vegetation/other feature kinds in the same step are not yet
            // ported (epic #404 Phase 3) — skipped, but the loop index above
            // still advances, keeping every later ore's `index` correct.
            continue;
        }
        ores.push(PlacedOre {
            index: i,
            placements: parse_placements(&placed),
            config: parse_ore_config(&configured["config"]),
        });
    }
    ores
}

/// Resolves one biome's `VEGETAL_DECORATION` decoration step
/// (`features[`[`crate::feature::STEP_VEGETAL_DECORATION`]`]`, issue #406)
/// into `(raw step index, resolved PlacedRef)` pairs — the same "preserve
/// the raw position" convention [`build_biome_ores`] establishes, so
/// [`crate::feature::vegetation::apply_vegetal_decoration_step`]'s
/// `setFeatureSeed` index matches vanilla's even if some entries here
/// resolve to [`crate::feature::vegetation::ConfiguredFeature::Unsupported`]
/// (which still consumes an index, just places nothing — see that module's
/// doc for why an unsupported entry must never be dropped from the list,
/// only made inert).
///
/// Empty if the resolver has no data for `biome`.
#[must_use]
pub fn build_biome_vegetation(
    resolver: &dyn Resolver,
    biome: &str,
) -> Vec<(usize, crate::feature::vegetation::PlacedRef)> {
    let doc = resolver.biome_document(biome);
    let Some(step) = doc
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(crate::feature::STEP_VEGETAL_DECORATION as usize))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, entry) in step.iter().enumerate() {
        let Some(id) = entry.as_str() else {
            continue;
        };
        if resolver.placed_feature(id).is_null() {
            continue;
        }
        let placed_ref =
            crate::feature::vegetation::resolve_placed_feature_ref(resolver, entry);
        out.push((i, placed_ref));
    }
    out
}

/// Resolves every block tag referenced by `ores`' [`RuleTest::TagMatch`]
/// targets into a `tag id -> member block set` map, for
/// [`crate::feature::OreInput::in_tag`].
#[must_use]
pub fn build_ore_tag_map(
    resolver: &dyn Resolver,
    ores: &[PlacedOre],
) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for ore in ores {
        for target in &ore.config.targets {
            if let RuleTest::TagMatch(tag) = &target.target {
                map.entry(tag.clone()).or_insert_with(|| {
                    let mut out = HashSet::new();
                    let mut seen = HashSet::new();
                    resolve_block_tag(resolver, tag, &mut out, &mut seen);
                    out
                });
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeResolver {
        tags: HashMap<&'static str, Value>,
        biomes: HashMap<&'static str, Value>,
        carvers: HashMap<&'static str, Value>,
        features: HashMap<&'static str, Value>,
        placed: HashMap<&'static str, Value>,
    }

    impl Resolver for FakeResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }
        fn noise(&self, _id: &str) -> crate::density::NoiseParams {
            unimplemented!("not needed by this test")
        }
        fn block_tag(&self, id: &str) -> Value {
            self.tags.get(id).cloned().unwrap_or(Value::Null)
        }
        fn biome_document(&self, id: &str) -> Value {
            self.biomes.get(id).cloned().unwrap_or(Value::Null)
        }
        fn configured_carver(&self, id: &str) -> Value {
            self.carvers.get(id).cloned().unwrap_or(Value::Null)
        }
        fn configured_feature(&self, id: &str) -> Value {
            self.features.get(id).cloned().unwrap_or(Value::Null)
        }
        fn placed_feature(&self, id: &str) -> Value {
            self.placed.get(id).cloned().unwrap_or(Value::Null)
        }
    }

    #[test]
    fn resolve_block_tag_follows_subtag_references() {
        let mut tags = HashMap::new();
        tags.insert(
            "minecraft:leaf",
            serde_json::json!({"values": ["minecraft:oak_log", "#minecraft:sub"]}),
        );
        tags.insert(
            "minecraft:sub",
            serde_json::json!({"values": ["minecraft:stone"]}),
        );
        let resolver = FakeResolver {
            tags,
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:leaf", &mut out, &mut seen);
        assert_eq!(
            out,
            HashSet::from([
                "minecraft:oak_log".to_string(),
                "minecraft:stone".to_string()
            ])
        );
    }

    #[test]
    fn resolve_block_tag_missing_id_is_empty_not_panic() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:does_not_exist", &mut out, &mut seen);
        assert!(out.is_empty());
    }

    #[test]
    fn build_biome_carvers_preserves_declared_order() {
        let mut biomes = HashMap::new();
        biomes.insert(
            "minecraft:test",
            serde_json::json!({"carvers": ["minecraft:a", "minecraft:b"]}),
        );
        let mut carvers = HashMap::new();
        let cave = |probability: f64| {
            serde_json::json!({
                "type": "minecraft:cave",
                "config": {
                    "probability": probability,
                    "y": {"type": "minecraft:uniform", "min_inclusive": {"absolute": 0}, "max_inclusive": {"absolute": 10}},
                    "yScale": 1.0,
                    "horizontal_radius_multiplier": 1.0,
                    "vertical_radius_multiplier": 1.0,
                    "floor_level": 0.0,
                    "lava_level": {"absolute": -54}
                }
            })
        };
        carvers.insert("minecraft:a", cave(0.1));
        carvers.insert("minecraft:b", cave(0.2));
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers,
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let list = build_biome_carvers(&resolver, "minecraft:test");
        assert_eq!(list.len(), 2);
        let probs: Vec<f32> = list
            .iter()
            .map(|c| match c {
                CarverConfig::Cave(c) => c.probability,
                CarverConfig::Canyon(c) => c.probability,
            })
            .collect();
        assert_eq!(probs, vec![0.1_f32, 0.2_f32]);
    }

    #[test]
    fn build_biome_carvers_unknown_biome_is_empty() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        assert!(build_biome_carvers(&resolver, "minecraft:nowhere").is_empty());
    }

    #[test]
    fn build_biome_ores_skips_non_ore_but_keeps_index() {
        let mut steps = vec![Value::Array(Vec::new()); 7];
        steps[STEP_UNDERGROUND_ORES as usize] = serde_json::json!([
            "minecraft:not_ore", // index 0, skipped (missing placed data)
            "minecraft:iron"     // index 1, must keep index 1, not 0
        ]);
        let mut biomes = HashMap::new();
        biomes.insert("minecraft:test", serde_json::json!({"features": steps}));

        let mut placed = HashMap::new();
        placed.insert(
            "minecraft:iron",
            serde_json::json!({"feature": "minecraft:iron_cf", "placement": []}),
        );
        let mut features = HashMap::new();
        features.insert(
            "minecraft:iron_cf",
            serde_json::json!({
                "type": "minecraft:ore",
                "config": {
                    "size": 9,
                    "discard_chance_on_air_exposure": 0.0,
                    "targets": []
                }
            }),
        );

        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers: HashMap::new(),
            features,
            placed,
        };
        let ores = build_biome_ores(&resolver, "minecraft:test");
        assert_eq!(ores.len(), 1, "the non-ore entry must not produce a PlacedOre");
        assert_eq!(ores[0].index, 1, "index must be the raw step position, not a count");
    }

    #[test]
    fn build_biome_vegetation_skips_missing_but_keeps_index() {
        let mut steps = vec![Value::Array(Vec::new()); 10];
        steps[crate::feature::STEP_VEGETAL_DECORATION as usize] = serde_json::json!([
            "minecraft:missing", // index 0, skipped (no placed_feature data)
            "minecraft:grass_patch" // index 1, must keep index 1, not 0
        ]);
        let mut biomes = HashMap::new();
        biomes.insert("minecraft:test", serde_json::json!({"features": steps}));

        let mut placed = HashMap::new();
        placed.insert(
            "minecraft:grass_patch",
            serde_json::json!({"feature": "minecraft:grass_cf", "placement": []}),
        );
        let mut features = HashMap::new();
        features.insert(
            "minecraft:grass_cf",
            serde_json::json!({
                "type": "minecraft:simple_block",
                "config": {
                    "to_place": {
                        "type": "minecraft:simple_state_provider",
                        "state": {"Name": "minecraft:short_grass"}
                    }
                }
            }),
        );

        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes,
            carvers: HashMap::new(),
            features,
            placed,
        };
        let veg = build_biome_vegetation(&resolver, "minecraft:test");
        assert_eq!(veg.len(), 1, "the missing entry must not produce a PlacedRef");
        assert_eq!(veg[0].0, 1, "index must be the raw step position, not a count");
        assert!(matches!(
            *veg[0].1.feature,
            crate::feature::vegetation::ConfiguredFeature::SimpleBlock(_)
        ));
    }

    #[test]
    fn build_biome_vegetation_unknown_biome_is_empty() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            carvers: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        assert!(build_biome_vegetation(&resolver, "minecraft:nowhere").is_empty());
    }
}
