//! External witness coverage for the Nether's standard gold-ore entry.
//!
//! The witness comes from the light-free external stream, while the test
//! exercises the production source-completion spill API. A resolver control
//! removes only `ore_gold_nether`.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::NetherGenerator;
use lodestone_worldgen::stage_schedule::{DecorationStep, Dimension, NETHER_FEATURES};
use serde_json::Value;

const EXTERNAL: &str = include_str!("support/nether_gold_ore_external.txt");
const SEED: i64 = 42;
const TARGET: (i32, i32) = (-1, -10);
const SOURCE: (i32, i32) = (-1, -10);

struct Assets {
    root: PathBuf,
}

impl Assets {
    fn new() -> Self {
        Self {
            root: std::env::var_os("LODESTONE_WORLDGEN_ASSETS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")
                }),
        }
    }

    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        std::fs::read_to_string(&path).ok().map_or(Value::Null, |text| {
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        })
    }
}

impl Resolver for Assets {
    fn density_function(&self, id: &str) -> Value { self.read("density_function", id) }
    fn noise(&self, id: &str) -> NoiseParams {
        let value = self.read("noise", id);
        NoiseParams {
            first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: value["amplitudes"].as_array().expect("amplitudes").iter()
                .map(|amplitude| amplitude.as_f64().expect("amplitude")).collect(),
        }
    }
    fn biome_parameters(&self) -> Value { self.read("biome_parameters", "nether") }
    fn biome_document(&self, id: &str) -> Value { self.try_read("biome", id) }
    fn configured_carver(&self, id: &str) -> Value { self.try_read("configured_carver", id) }
    fn configured_feature(&self, id: &str) -> Value { self.try_read("configured_feature", id) }
    fn placed_feature(&self, id: &str) -> Value { self.try_read("placed_feature", id) }
    fn block_tag(&self, id: &str) -> Value { self.try_read("tags/block", id) }
}

struct WithoutGoldOre(Assets);

impl Resolver for WithoutGoldOre {
    fn density_function(&self, id: &str) -> Value { self.0.density_function(id) }
    fn noise(&self, id: &str) -> NoiseParams { self.0.noise(id) }
    fn biome_parameters(&self) -> Value { self.0.biome_parameters() }
    fn biome_document(&self, id: &str) -> Value {
        let mut document = self.0.biome_document(id);
        if let Some(entries) = document.get_mut("features").and_then(Value::as_array_mut)
            .and_then(|steps| steps.get_mut(7)).and_then(Value::as_array_mut)
        {
            for entry in entries {
                if entry.as_str() == Some("minecraft:ore_gold_nether") {
                    *entry = Value::String("lodestone:withheld_ore_gold_nether".to_owned());
                }
            }
        }
        document
    }
    fn configured_carver(&self, id: &str) -> Value { self.0.configured_carver(id) }
    fn configured_feature(&self, id: &str) -> Value { self.0.configured_feature(id) }
    fn placed_feature(&self, id: &str) -> Value { self.0.placed_feature(id) }
    fn block_tag(&self, id: &str) -> Value { self.0.block_tag(id) }
}

fn external_cell() -> (i32, i32, i32) {
    let mut cell = None;
    let mut stream_sha256 = None;
    let mut seed = None;
    let mut target = None;
    let mut source = None;
    let mut step = None;
    let mut feature_index = None;
    let mut feature = None;
    let mut block = None;
    let mut scope = None;
    for line in EXTERNAL.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("stream-sha256") => stream_sha256 = fields.next(),
            Some("seed") => seed = fields.next().and_then(|value| value.parse().ok()),
            Some("target") => target = Some((fields.next().unwrap().parse().unwrap(), fields.next().unwrap().parse().unwrap())),
            Some("source") => source = Some((fields.next().unwrap().parse().unwrap(), fields.next().unwrap().parse().unwrap())),
            Some("step") => step = fields.next().and_then(|value| value.parse().ok()),
            Some("feature-index") => feature_index = fields.next().and_then(|value| value.parse().ok()),
            Some("feature") => feature = fields.next(),
            Some("block") => block = fields.next(),
            Some("scope") => scope = fields.next(),
            Some("cell") => cell = Some((fields.next().unwrap().parse().unwrap(), fields.next().unwrap().parse().unwrap(), fields.next().unwrap().parse().unwrap())),
            _ => {}
        }
    }
    assert_eq!(stream_sha256.map(str::len), Some(64));
    assert_eq!(seed, Some(SEED));
    assert_eq!(target, Some(TARGET));
    assert_eq!(source, Some(SOURCE));
    assert_eq!(step, Some(DecorationStep::UndergroundDecoration.ordinal()));
    assert_eq!(feature_index, Some(19));
    assert_eq!(feature, Some("minecraft:ore_gold_nether"));
    assert_eq!(block, Some("minecraft:nether_gold_ore"));
    assert_eq!(scope, Some("source-spill"));
    assert_eq!(cell, Some((6, 18, 0)));
    cell.unwrap()
}

fn gold_spills(generator: &NetherGenerator) -> Vec<lodestone_worldgen::nether::ParityDecorationSpill> {
    generator.parity_source_spills_with_overrides(TARGET.0, TARGET.1, SOURCE.0, SOURCE.1, &[])
}

#[test]
fn external_gold_witness_has_typed_step_seven_attribution() {
    let cell = external_cell();
    assert_eq!(NETHER_FEATURES.dimension(), Dimension::Nether);
    assert!(NETHER_FEATURES.steps().contains(&DecorationStep::UndergroundDecoration));
    assert_eq!(DecorationStep::UndergroundDecoration.ordinal(), 7);
    assert_eq!(cell, (6, 18, 0));
}

#[test]
fn external_gold_witness_reaches_production_source_spill() {
    let (lx, y, lz) = external_cell();
    let assets = Assets::new();
    let settings = assets.read("noise_settings", "nether");
    let generator = NetherGenerator::new(SEED, &settings, &assets);
    let absolute = (TARGET.0 * 16 + lx, y, TARGET.1 * 16 + lz);
    assert!(gold_spills(&generator).iter().any(|spill| {
        spill.position == absolute && spill.state == "minecraft:nether_gold_ore" && !spill.transient
    }), "production source spill lost external gold witness {absolute:?}");
}

#[test]
fn withholding_gold_entry_is_a_live_detector_control() {
    let (lx, y, lz) = external_cell();
    let assets = Assets::new();
    let settings = assets.read("noise_settings", "nether");
    let full = NetherGenerator::new(SEED, &settings, &assets);
    let without = NetherGenerator::new(SEED, &settings, &WithoutGoldOre(Assets::new()));
    let absolute = (TARGET.0 * 16 + lx, y, TARGET.1 * 16 + lz);
    assert!(gold_spills(&full).iter().any(|spill| spill.position == absolute && spill.state == "minecraft:nether_gold_ore"));
    assert!(gold_spills(&without).iter().all(|spill| spill.position != absolute || spill.state != "minecraft:nether_gold_ore"), "detector control: withholding ore_gold_nether must remove the external witness");
}
