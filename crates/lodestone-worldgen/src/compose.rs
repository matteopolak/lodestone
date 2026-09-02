//! Version-free composition glue: resolves per-biome carver
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
/// earlier behaviour for a `Resolver` that never implemented this method.
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
            // ported — skipped, but the loop index above
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
/// (`features[`[`crate::feature::STEP_VEGETAL_DECORATION`]`]`)
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

/// Every decoration step the [`crate::feature::vegetation`] engine
/// drives, not just `VEGETAL_DECORATION`.
///
/// Returns `(step, raw index within that step, resolved feature)` in **step
/// order**, which is the order `Biome.generate` runs them. The index is the
/// entry's position in its own step array, because that is what
/// `WorldgenRandom::set_feature_seed(seed, index, step)` takes — the *pair*
/// identifies a feature's RNG stream, so a step must never be flattened into a
/// single running index.
///
/// # Which steps, and the one deliberate deviation
///
/// [`DRIVEN_STEPS`] is the list. It omits:
///
/// * step 6 `UNDERGROUND_ORES` — a separate engine
///   ([`crate::feature::apply_ore_step_3x3_per_source`]) with its own region
///   view, already correct, and merging the two is not this issue's scope.
/// * step 10 `TOP_LAYER_MODIFICATION` — [`crate::feature::top_layer`].
/// * step 5 `STRONGHOLDS` — zero entries across all 66 bundled biomes.
///
/// **The deviation:** because ore runs as its own earlier stage, steps 0-4 run
/// *after* ores here and *before* them in vanilla. Nothing in steps 0-4 reads a
/// block that ore placement writes (ore replaces stone with ore in place, so
/// every solidity/air question those steps ask answers the same either way), so
/// this is a real ordering difference with no known observable consequence —
/// stated rather than hidden, because "no known consequence" is not "none".
#[must_use]
pub fn build_biome_decoration(
    resolver: &dyn Resolver,
    biome: &str,
) -> Vec<(i32, usize, crate::feature::vegetation::PlacedRef)> {
    let doc = resolver.biome_document(biome);
    let Some(steps) = doc.get("features").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &step in DRIVEN_STEPS {
        let Some(entries) = steps.get(step as usize).and_then(Value::as_array) else {
            continue;
        };
        for (i, entry) in entries.iter().enumerate() {
            let Some(id) = entry.as_str() else {
                continue;
            };
            if resolver.placed_feature(id).is_null() {
                continue;
            }
            out.push((
                step,
                i,
                crate::feature::vegetation::resolve_placed_feature_ref(resolver, entry),
            ));
        }
    }
    out
}

/// The `GenerationStep.Decoration` indices [`build_biome_decoration`] drives, in
/// vanilla's own order. See that function's doc for what is missing and why.
pub const DRIVEN_STEPS: &[i32] = &[
    0, // RAW_GENERATION
    1, // LAKES
    2, // LOCAL_MODIFICATIONS
    3, // UNDERGROUND_STRUCTURES  (monster_room/fossil are plain features)
    4, // SURFACE_STRUCTURES
    7, // UNDERGROUND_DECORATION
    8, // FLUID_SPRINGS
    crate::feature::STEP_VEGETAL_DECORATION,
];

/// Whether a biome document lists `minecraft:freeze_top_layer` in its
/// `TOP_LAYER_MODIFICATION` step.
///
/// In vanilla 26.2 **every** biome does — vanilla's own default per-biome
/// feature registration adds it
/// from a shared tail, so the step self-gates on temperature rather than on
/// biome membership, and `docs/plans/worldgen-parity.md`'s census row 6i verified
/// that across `assets/worldgen/biome/*.json`. This function exists anyway
/// because "every biome" is a property of the *data*, not of the engine: a
/// trimmed or modified datapack that omits the entry must produce a snow-free
/// world rather than snow the engine assumed.
///
/// Unlike [`build_biome_vegetation`] this does not consult
/// [`Resolver::placed_feature`]: `placed_feature/freeze_top_layer.json` carries
/// nothing the engine reads (its whole placement is `[{"type":
/// "minecraft:biome"}]`, and `configured_feature/freeze_top_layer.json`'s config
/// is `{}` — `NoneFeatureConfiguration`), so requiring it to resolve would gate
/// the step on an asset with no content.
#[must_use]
pub fn biome_lists_freeze_top_layer(document: &Value) -> bool {
    document
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps.get(crate::feature::top_layer::STEP_TOP_LAYER_MODIFICATION as usize)
        })
        .and_then(Value::as_array)
        .is_some_and(|step| {
            step.iter().any(|entry| {
                entry.as_str().is_some_and(|id| {
                    id.strip_prefix("minecraft:").unwrap_or(id) == "freeze_top_layer"
                })
            })
        })
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

/// Where chunk-local `(lx, ly, lz)` lands in a column field built by
/// [`fill_column`] or read back by [`materialize_column`].
///
/// **Every caller must go through this rather than restating
/// `((ly * 16 + lz) * 16 + lx)`.** A restated index that transposed `lx` and `lz`
/// reads a mirrored column and reports a plausible-looking wrong answer, and the
/// two spellings could then drift apart independently — which is the same argument
/// `OverworldGenerator::shape_index` makes for forwarding to its own private `idx`.
#[must_use]
pub fn column_index(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
    debug_assert!((0..height).contains(&ly));
    ((ly * 16 + lz) * 16 + lx) as usize
}

/// `fillFromNoise` for one chunk of a **disabled-aquifer** dimension: the
/// interpolated `final_density` plus the beard term, mapped to
/// [`BlockKind`](crate::aquifer::BlockKind).
///
/// Shared by [`crate::nether::NetherGenerator`] and
/// [`crate::end::EndGenerator`], which differ only in the settings they hand the
/// aquifer. The Overworld keeps its own copy because its fill is instrumented by
/// the allocation-attribution bench and carries a `StageGuard` this one must not.
///
/// # The two loops are a correctness property, not a micro-optimisation
///
/// An **empty** beardifier takes the loop that calls
/// [`AquiferSystem::block_at`](crate::aquifer::AquiferSystem::block_at) with no
/// addition at all. Adding `0.0` is the identity for every finite `f64` *except*
/// `-0.0`, whose sign bit it flips; nothing downstream distinguishes the two today
/// (`compute_substance` only asks `density > 0.0`), and the branch means that claim
/// about the rest of the pipeline never has to be made. It is also what keeps a
/// dimension with no adaptation-bearing structure bit-identical to the same
/// dimension before structures existed.
#[must_use]
pub fn fill_column(
    aquifer: &crate::aquifer::AquiferSystem,
    base_x: i32,
    base_z: i32,
    min_y: i32,
    height: i32,
    beard: &crate::structure::beardifier::Beardifier,
) -> Vec<crate::aquifer::BlockKind> {
    use crate::aquifer::BlockKind;
    let mut field = vec![BlockKind::Air; 16 * 16 * height as usize];
    if beard.is_empty() {
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..height {
                    field[column_index(lx, ly, lz, height)] =
                        aquifer.block_at(base_x + lx, min_y + ly, base_z + lz);
                }
            }
        }
        return field;
    }
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..height {
                let (wx, wy, wz) = (base_x + lx, min_y + ly, base_z + lz);
                field[column_index(lx, ly, lz, height)] =
                    aquifer.block_at_beard(wx, wy, wz, beard.compute(wx, wy, wz));
            }
        }
    }
    field
}

/// The heightmap the biome and surface stages consume: the highest local
/// `(lx, lz)` position whose block is *solid* (`BlockKind::Stone` — non-air,
/// non-fluid), floored at `sea_level - 1`.
///
/// This is `ComposedChunkOracle.java`'s `solidTop`, same definition and same
/// fallback, which is why biome sampling agrees between the two languages.
///
/// The floor matters in a dimension with no sea: the End's `sea_level` is `0` and
/// its `min_y` is `0`, so `sea_level - 1` is `min_y - 1` — the same "nothing solid
/// in this column" sentinel the loop already produces, rather than a spurious
/// clamp to a water line that does not exist.
#[must_use]
pub fn solid_top_heights(
    field: &[crate::aquifer::BlockKind],
    min_y: i32,
    height: i32,
    sea_level: i32,
) -> [i32; 256] {
    use crate::aquifer::BlockKind;
    let mut heights = [i32::MIN; 256];
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            let mut top = min_y - 1;
            for ly in (0..height).rev() {
                if field[column_index(lx, ly, lz, height)] == BlockKind::Stone {
                    top = min_y + ly;
                    break;
                }
            }
            heights[(lz * 16 + lx) as usize] = top.max(sea_level - 1);
        }
    }
    heights
}

/// Turns a [`fill_column`] field plus a surface diff into the working block grid
/// the carve and structure stages mutate.
///
/// # The loop order is the specification
///
/// A [`crate::dense_grid::DenseBlockGrid`]'s palette is built in `set` order, and
/// `surface_diff` is a hash map whose iteration order is not stable even across two
/// separately constructed maps with identical content. So the diff is consulted by
/// **point lookup inside this fixed `(lz, lx, ly)` loop** and never iterated:
/// iterating it made two independently built generators produce the same terrain
/// with different bytes, which is a real bug this repo shipped once (see
/// `overworld/mod.rs`'s own note).
#[must_use]
pub fn materialize_column(
    interner: &std::sync::Arc<crate::interner::StateInterner>,
    field: &[crate::aquifer::BlockKind],
    surface_diff: &crate::surface::SurfaceDiff,
    base_x: i32,
    base_z: i32,
    min_y: i32,
    height: i32,
    solid: crate::interner::StateId,
    fluid: crate::interner::StateId,
) -> crate::dense_grid::DenseBlockGrid {
    use crate::aquifer::BlockKind;
    use crate::interner::StateId;
    let mut world = crate::dense_grid::DenseBlockGrid::with_interner(
        std::sync::Arc::clone(interner),
        base_x,
        min_y,
        base_z,
        16,
        height,
        16,
        StateId::AIR,
    );
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..height {
                let y = min_y + ly;
                let base = match field[column_index(lx, ly, lz, height)] {
                    BlockKind::Stone => solid,
                    BlockKind::Water | BlockKind::Lava => fluid,
                    BlockKind::Air => StateId::AIR,
                };
                let state = surface_diff.get(&(lx, y, lz)).copied().unwrap_or(base);
                world.set_id(base_x + lx, y, base_z + lz, state);
            }
        }
    }
    world
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
