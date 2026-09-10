//! Registry-consumption census for configured and placed worldgen features.
//!
//! This is deliberately a data-only gate. It walks the three dimension biome
//! sets in the bundled registry, resolves every placed/configured holder (also
//! following inline selector branches), and checks that the parser consumes
//! each placement modifier in declaration order. A parser that silently drops
//! a new modifier would otherwise leave a plausible feature body running with
//! a different position stream.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::feature::IntProvider;
use lodestone_worldgen::feature::vegetation::{
    collect_unsupported, resolve_placed_feature_ref, ConfiguredFeature, PlacedRef, VegPlacement,
};
use serde_json::Value;

const PLACEMENT_TYPES: &[&str] = &[
    "minecraft:biome",
    "minecraft:block_predicate_filter",
    "minecraft:count",
    "minecraft:count_on_every_layer",
    "minecraft:environment_scan",
    "minecraft:fixed_placement",
    "minecraft:height_range",
    "minecraft:heightmap",
    "minecraft:in_square",
    "minecraft:noise_based_count",
    "minecraft:noise_threshold_count",
    "minecraft:random_offset",
    "minecraft:rarity_filter",
    "minecraft:surface_relative_threshold_filter",
    "minecraft:surface_water_depth_filter",
];

/// These configured types are intentionally outside the common vegetal
/// parser. Their consumers are separate dimension/stage paths, or they are
/// the currently explicit registry gaps. Keeping this list in the census
/// makes a newly reachable body fail loudly instead of becoming an unnoticed
/// `ConfiguredFeature::Unsupported` no-op.
const OUT_OF_BAND_CONFIGURED_TYPES: &[&str] = &[
    "minecraft:chorus_plant",
    "minecraft:coral_claw",
    "minecraft:coral_mushroom",
    "minecraft:coral_tree",
    "minecraft:end_gateway",
    "minecraft:end_island",
    "minecraft:end_platform",
    "minecraft:end_spike",
    "minecraft:fossil",
    "minecraft:freeze_top_layer",
    "minecraft:iceberg",
    "minecraft:ore",
    "minecraft:root_system",
    "minecraft:scattered_ore",
    "minecraft:template",
];

/// The parser arms currently expected to be usable for every reachable
/// document of that type. This is intentionally separate from
/// `OUT_OF_BAND_CONFIGURED_TYPES`: a type can be parsed by the common
/// interpreter while still carrying a known composed-coverage gap (for
/// example, coral or root-system bodies).
const COMMON_CONFIGURED_TYPES: &[&str] = &[
    "minecraft:bamboo",
    "minecraft:basalt_columns",
    "minecraft:basalt_pillar",
    "minecraft:block_blob",
    "minecraft:block_column",
    "minecraft:block_pile",
    "minecraft:blue_ice",
    "minecraft:delta_feature",
    "minecraft:desert_well",
    "minecraft:disk",
    "minecraft:fallen_tree",
    "minecraft:geode",
    "minecraft:large_dripstone",
    "minecraft:glowstone_blob",
    "minecraft:huge_brown_mushroom",
    "minecraft:huge_fungus",
    "minecraft:huge_red_mushroom",
    "minecraft:kelp",
    "minecraft:lake",
    "minecraft:monster_room",
    "minecraft:multiface_growth",
    "minecraft:nether_forest_vegetation",
    "minecraft:netherrack_replace_blobs",
    "minecraft:no_op",
    "minecraft:ore",
    "minecraft:random_boolean_selector",
    "minecraft:random_selector",
    "minecraft:root_system",
    "minecraft:sculk_patch",
    "minecraft:sea_pickle",
    "minecraft:seagrass",
    "minecraft:sequence",
    "minecraft:simple_block",
    "minecraft:simple_random_selector",
    "minecraft:speleothem",
    "minecraft:speleothem_cluster",
    "minecraft:spring_feature",
    "minecraft:spike",
    "minecraft:tree",
    "minecraft:twisting_vines",
    "minecraft:underwater_magma",
    "minecraft:vegetation_patch",
    "minecraft:vines",
    "minecraft:waterlogged_vegetation_patch",
    "minecraft:weeping_vines",
    "minecraft:weighted_random_selector",
];

#[derive(Debug)]
struct Bundle {
    root: PathBuf,
    biomes: BTreeMap<String, Value>,
    placed: BTreeMap<String, Value>,
    configured: BTreeMap<String, Value>,
}

impl Bundle {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen");
        Self {
            biomes: load_dir(&root, "biome"),
            placed: load_dir(&root, "placed_feature"),
            configured: load_dir(&root, "configured_feature"),
            root,
        }
    }

    fn biome_names(&self, tag: &str) -> BTreeSet<String> {
        let path = self.root.join("tags/worldgen/biome").join(format!("{tag}.json"));
        let document = read_json(&path);
        document["values"]
            .as_array()
            .expect("biome tag values")
            .iter()
            .map(|value| value.as_str().expect("biome tag entry").to_owned())
            .collect()
    }

    fn overworld_names(&self) -> BTreeSet<String> {
        let path = self.root.join("biome_parameters/overworld.json");
        let document = read_json(&path);
        document
            .as_array()
            .expect("overworld biome parameter table")
            .iter()
            .map(|row| row[13].as_str().expect("overworld biome parameter name").to_owned())
            .collect()
    }
}

impl Resolver for Bundle {
    fn density_function(&self, id: &str) -> Value {
        panic!("registry census does not resolve density function {id}");
    }

    fn noise(&self, id: &str) -> NoiseParams {
        panic!("registry census does not resolve noise {id}");
    }

    fn biome_document(&self, id: &str) -> Value {
        lookup(&self.biomes, id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        lookup(&self.configured, id)
    }

    fn placed_feature(&self, id: &str) -> Value {
        lookup(&self.placed, id)
    }

    fn block_tag(&self, id: &str) -> Value {
        let path = self
            .root
            .join("tags/block")
            .join(format!("{}.json", short_id(id)));
        fs::read_to_string(path)
            .ok()
            .map(|text| serde_json::from_str(&text).expect("block tag JSON"))
            .unwrap_or(Value::Null)
    }
}

fn short_id(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

fn lookup(table: &BTreeMap<String, Value>, id: &str) -> Value {
    table.get(short_id(id)).cloned().unwrap_or(Value::Null)
}

fn load_dir(root: &Path, kind: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let dir = root.join(kind);
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
        let path = entry.expect("worldgen asset entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .expect("worldgen asset filename")
            .to_owned();
        let previous = out.insert(name, read_json(&path));
        assert!(previous.is_none(), "duplicate {kind} asset: {}", path.display());
    }
    out
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn placement_type(placement: &VegPlacement) -> &'static str {
    match placement {
        VegPlacement::Count(_) => "minecraft:count",
        VegPlacement::InSquare => "minecraft:in_square",
        VegPlacement::Heightmap(_) => "minecraft:heightmap",
        VegPlacement::Biome => "minecraft:biome",
        VegPlacement::RarityFilter(_) => "minecraft:rarity_filter",
        VegPlacement::SurfaceWaterDepthFilter(_) => "minecraft:surface_water_depth_filter",
        VegPlacement::NoiseThresholdCount { .. } => "minecraft:noise_threshold_count",
        VegPlacement::RandomOffset { .. } => "minecraft:random_offset",
        VegPlacement::BlockPredicateFilter(_) => "minecraft:block_predicate_filter",
        VegPlacement::HeightRange(_) => "minecraft:height_range",
        VegPlacement::CountOnEveryLayer(_) => "minecraft:count_on_every_layer",
        VegPlacement::EnvironmentScan { .. } => "minecraft:environment_scan",
        VegPlacement::NoiseBasedCount { .. } => "minecraft:noise_based_count",
        VegPlacement::SurfaceRelativeThresholdFilter { .. } => {
            "minecraft:surface_relative_threshold_filter"
        }
        VegPlacement::FixedPlacement(_) => "minecraft:fixed_placement",
    }
}

fn feature_type(feature: &ConfiguredFeature) -> Option<&str> {
    Some(match feature {
        ConfiguredFeature::SimpleBlock(_) => "minecraft:simple_block",
        ConfiguredFeature::Tree(_) => "minecraft:tree",
        ConfiguredFeature::BlockColumn(_) => "minecraft:block_column",
        ConfiguredFeature::FallenTree(_) => "minecraft:fallen_tree",
        ConfiguredFeature::RootSystem(_) => "minecraft:root_system",
        // The concrete enum lives in a private parser submodule. Its exact
        // kind is still reported by `collect_unsupported`, which is the
        // coverage signal this census needs.
        ConfiguredFeature::Coral(_) => "minecraft:coral",
        ConfiguredFeature::RandomSelector { .. } => "minecraft:random_selector",
        ConfiguredFeature::SimpleRandomSelector(_) => "minecraft:simple_random_selector",
        ConfiguredFeature::Spring(_) => "minecraft:spring_feature",
        ConfiguredFeature::UnderwaterMagma(_) => "minecraft:underwater_magma",
        ConfiguredFeature::Disk(_) => "minecraft:disk",
        ConfiguredFeature::BlockPile(_) => "minecraft:block_pile",
        ConfiguredFeature::NetherForestVegetation(_) => "minecraft:nether_forest_vegetation",
        ConfiguredFeature::BlockBlob(_) => "minecraft:block_blob",
        ConfiguredFeature::Delta(_) => "minecraft:delta_feature",
        ConfiguredFeature::BasaltColumns(_) => "minecraft:basalt_columns",
        ConfiguredFeature::ReplaceBlobs(_) => "minecraft:netherrack_replace_blobs",
        ConfiguredFeature::GlowstoneBlob => "minecraft:glowstone_blob",
        ConfiguredFeature::BasaltPillar => "minecraft:basalt_pillar",
        ConfiguredFeature::DesertWell => "minecraft:desert_well",
        ConfiguredFeature::BlueIce => "minecraft:blue_ice",
        ConfiguredFeature::Kelp => "minecraft:kelp",
        ConfiguredFeature::SeaPickle(_) => "minecraft:sea_pickle",
        ConfiguredFeature::Seagrass(_) => "minecraft:seagrass",
        ConfiguredFeature::Vines => "minecraft:vines",
        ConfiguredFeature::TwistingVines(_) => "minecraft:twisting_vines",
        ConfiguredFeature::WeepingVines => "minecraft:weeping_vines",
        ConfiguredFeature::MultifaceGrowth(_) => "minecraft:multiface_growth",
        ConfiguredFeature::Speleothem(_) => "minecraft:speleothem",
        ConfiguredFeature::SpeleothemCluster(_) => "minecraft:speleothem_cluster",
        ConfiguredFeature::Lake(_) => "minecraft:lake",
        ConfiguredFeature::MonsterRoom => "minecraft:monster_room",
        ConfiguredFeature::HugeMushroom(_) => "minecraft:huge_mushroom",
        ConfiguredFeature::HugeFungus(_) => "minecraft:huge_fungus",
        ConfiguredFeature::Bamboo(_) => "minecraft:bamboo",
        ConfiguredFeature::VegetationPatch(_) => "minecraft:vegetation_patch",
        ConfiguredFeature::SculkPatch(_) => "minecraft:sculk_patch",
        ConfiguredFeature::RandomBooleanSelector { .. } => "minecraft:random_boolean_selector",
        ConfiguredFeature::WeightedRandomSelector(_) => "minecraft:weighted_random_selector",
        ConfiguredFeature::Sequence(_) => "minecraft:sequence",
        ConfiguredFeature::Geode(_) => "minecraft:geode",
        ConfiguredFeature::Fossil(_) => "minecraft:fossil",
        ConfiguredFeature::IceSpike(_) => "minecraft:spike",
        ConfiguredFeature::LargeDripstone(_) => "minecraft:large_dripstone",
        ConfiguredFeature::NoOp => "minecraft:no_op",
        ConfiguredFeature::Unsupported(_) => return None,
    })
}

fn collect_feature_types(feature: &ConfiguredFeature, out: &mut BTreeSet<String>) {
    if let Some(kind) = feature_type(feature) {
        out.insert(kind.to_owned());
    }
    match feature {
        ConfiguredFeature::RandomSelector { default, options } => {
            collect_placed_types(default, out);
            for (_, option) in options {
                collect_placed_types(option, out);
            }
        }
        ConfiguredFeature::SimpleRandomSelector(options)
        | ConfiguredFeature::Sequence(options) => {
            for option in options {
                collect_placed_types(option, out);
            }
        }
        ConfiguredFeature::RandomBooleanSelector { yes, no } => {
            collect_placed_types(yes, out);
            collect_placed_types(no, out);
        }
        ConfiguredFeature::WeightedRandomSelector(options) => {
            for (_, option) in options {
                collect_placed_types(option, out);
            }
        }
        ConfiguredFeature::VegetationPatch(config) => {
            collect_placed_types(&config.vegetation_feature, out);
        }
        ConfiguredFeature::RootSystem(config) => collect_placed_types(&config.feature, out),
        _ => {}
    }
}

fn collect_placed_types(placed: &PlacedRef, out: &mut BTreeSet<String>) {
    collect_feature_types(&placed.feature, out);
}

fn collect_reachable_types(bundle: &Bundle, biomes: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for biome in biomes {
        let document = bundle
            .biomes
            .get(short_id(biome))
            .unwrap_or_else(|| panic!("missing biome document {biome}"));
        let steps = document["features"]
            .as_array()
            .unwrap_or_else(|| panic!("{biome} has no feature-step array"));
        for entry in steps.iter().flat_map(Value::as_array).flatten() {
            let id = entry
                .as_str()
                .unwrap_or_else(|| panic!("{biome} has non-string feature holder: {entry}"));
            let placed = resolve_placed_feature_ref(bundle, &Value::String(id.to_owned()));
            collect_placed_types(&placed, &mut out);
            for reason in collect_unsupported(&placed) {
                out.insert(format!("unsupported:{reason}"));
            }
        }
    }
    out
}

fn collect_raw_configured_types(bundle: &Bundle, biomes: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for biome in biomes {
        let document = bundle.biomes.get(short_id(biome)).expect("biome document");
        for entry in document["features"]
            .as_array()
            .expect("feature steps")
            .iter()
            .flat_map(Value::as_array)
            .flatten()
        {
            collect_raw_placed_holder(bundle, entry, &mut out);
        }
    }
    out
}

fn collect_raw_placed(bundle: &Bundle, id: &str, out: &mut BTreeSet<String>) {
    let document = bundle.placed.get(short_id(id)).expect("placed feature document");
    collect_raw_configured(bundle, &document["feature"], out);
}

fn collect_raw_placed_holder(bundle: &Bundle, value: &Value, out: &mut BTreeSet<String>) {
    if let Some(id) = value.as_str() {
        collect_raw_placed(bundle, id, out);
    } else {
        let feature = value
            .get("feature")
            .filter(|feature| !feature.is_null())
            .unwrap_or_else(|| panic!("inline placed holder has no feature: {value}"));
        collect_raw_configured(bundle, feature, out);
    }
}

fn collect_raw_configured(bundle: &Bundle, value: &Value, out: &mut BTreeSet<String>) {
    let document = if let Some(id) = value.as_str() {
        bundle.configured.get(short_id(id)).expect("configured feature document")
    } else {
        value
    };
    let kind = document["type"].as_str().expect("configured feature type");
    out.insert(kind.to_owned());
    let config = &document["config"];
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "random_selector" => {
            collect_raw_placed_holder(bundle, &config["default"], out);
            for option in config["features"].as_array().expect("selector features") {
                collect_raw_placed_holder(bundle, &option["feature"], out);
            }
        }
        "simple_random_selector" => {
            for option in config["features"].as_array().expect("simple selector features") {
                collect_raw_placed_holder(bundle, option, out);
            }
        }
        "random_boolean_selector" => {
            for key in ["feature_true", "feature_false"] {
                collect_raw_placed_holder(bundle, &config[key], out);
            }
        }
        "weighted_random_selector" => {
            for option in config["features"].as_array().expect("weighted selector features") {
                let data = &option["data"];
                collect_raw_placed_holder(bundle, data, out);
            }
        }
        "sequence" => {
            for option in config["features"].as_array().expect("sequence features") {
                collect_raw_placed_holder(bundle, option, out);
            }
        }
        "vegetation_patch" | "waterlogged_vegetation_patch" => {
            collect_raw_placed_holder(bundle, &config["vegetation_feature"], out);
        }
        "root_system" => {
            collect_raw_placed_holder(bundle, &config["feature"], out);
        }
        _ => {}
    }
}

#[test]
fn all_three_dimension_registries_are_consumed_without_modifier_loss() {
    let bundle = Bundle::new();
    let dimensions = [
        ("overworld", bundle.overworld_names()),
        ("nether", bundle.biome_names("is_nether")),
        ("end", bundle.biome_names("is_end")),
    ];

    let mut seen_biomes = BTreeSet::new();
    let mut actual_placement_types = BTreeSet::new();
    let mut actual_configured_types = BTreeMap::<&str, BTreeSet<String>>::new();
    for (dimension, biomes) in dimensions {
        assert_eq!(
            biomes.len(),
            match dimension {
                "overworld" => 55,
                "nether" | "end" => 5,
                _ => unreachable!(),
            },
            "unexpected {dimension} biome set"
        );
        assert!(
            seen_biomes.is_disjoint(&biomes),
            "dimension biome sets overlap: {dimension}"
        );
        seen_biomes.extend(biomes.iter().cloned());

        let raw_types = collect_raw_configured_types(&bundle, &biomes);
        let resolved_types = collect_reachable_types(&bundle, &biomes);
        actual_configured_types.insert(dimension, resolved_types);

        let unknown_types: Vec<_> = raw_types
            .iter()
            .filter(|kind| {
                !COMMON_CONFIGURED_TYPES.contains(&kind.as_str())
                    && !OUT_OF_BAND_CONFIGURED_TYPES.contains(&kind.as_str())
            })
            .collect();
        assert!(
            unknown_types.is_empty(),
            "{dimension} reaches configured feature types absent from the parser census: {unknown_types:?}"
        );

        for biome in &biomes {
            let document = bundle.biomes.get(short_id(biome)).expect("biome document");
            let steps = document["features"]
                .as_array()
                .unwrap_or_else(|| panic!("{biome} has no feature-step array"));
            for (step, entries) in steps.iter().enumerate() {
                for entry in entries.as_array().expect("feature-step entries") {
                    let id = entry.as_str().expect("placed-feature holder");
                    let raw = bundle
                        .placed
                        .get(short_id(id))
                        .unwrap_or_else(|| panic!("{dimension}/{biome} references missing {id}"));
                    let raw_modifiers = raw["placement"].as_array().expect("placement array");
                    let parsed = resolve_placed_feature_ref(&bundle, entry);
                    assert_eq!(
                        parsed.placements.len(),
                        raw_modifiers.len(),
                        "{dimension}/{biome} step {step} placed feature {id} dropped a placement modifier"
                    );
                    for (index, (raw_modifier, parsed_modifier)) in
                        raw_modifiers.iter().zip(parsed.placements.iter()).enumerate()
                    {
                        let raw_type = raw_modifier["type"]
                            .as_str()
                            .unwrap_or_else(|| panic!("{id} modifier {index} has no type"));
                        actual_placement_types.insert(raw_type.to_owned());
                        assert_eq!(
                            placement_type(parsed_modifier),
                            raw_type,
                            "{dimension}/{biome} step {step} placed feature {id} modifier {index} was parsed as the wrong kind"
                        );
                    }
                }
            }
        }
    }

    assert_eq!(seen_biomes.len(), 65, "dimension biome sets did not partition the bundle");
    assert_eq!(
        actual_placement_types,
        PLACEMENT_TYPES.iter().copied().map(str::to_owned).collect(),
        "reachable placement-modifier registry surface changed"
    );

    // Keep the diagnostic census in assertion output. This is useful when a
    // new registry file appears: the failure names the dimension and the
    // resolved parser reason rather than only reporting a count mismatch.
    for (dimension, types) in actual_configured_types {
        let unexpected: Vec<_> = types
            .iter()
            .filter(|kind| {
                kind.starts_with("unsupported:")
                    && !OUT_OF_BAND_CONFIGURED_TYPES.iter().any(|allowed| {
                        kind.strip_prefix("unsupported:")
                            .is_some_and(|reason| reason == allowed.strip_prefix("minecraft:").unwrap())
                    })
            })
            .collect();
        assert!(
            unexpected.is_empty(),
            "{dimension} parser returned an unlisted configured-feature gap: {unexpected:?}"
        );
    }
}

#[test]
fn nested_clamped_count_provider_is_consumed_as_one_modifier() {
    let bundle = Bundle::new();
    let placed = resolve_placed_feature_ref(
        &bundle,
        &Value::String("minecraft:forest_flowers".to_owned()),
    );
    let VegPlacement::Count(IntProvider::Clamped { source, min, max }) =
        placed.placements.get(3).expect("forest flowers count")
    else {
        panic!("forest flowers count must preserve its nested clamped provider");
    };
    assert_eq!((*min, *max), (0, 1));
    assert!(matches!(source.as_ref(), IntProvider::Uniform { min: -3, max: 1 }));
}
