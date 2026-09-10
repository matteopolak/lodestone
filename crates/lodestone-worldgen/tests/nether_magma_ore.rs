//! External parity and a negative control for the Nether's `ore_magma` entry.
//!
//! `NetherFeatureCellsOracle` snapshots the target after carvers, admits the
//! nine-chunk FEATURES closure, and records changed magma cells. The Rust
//! control removes only `ore_magma`, keeping the mixed step's other raw indices
//! intact while proving this standard ore body reaches production columns.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::{NetherColumn, NetherGenerator};
use serde_json::Value;

const EXTERNAL: &str = include_str!("support/nether_magma_ore_external.txt");
const SEED: i64 = 42;
const CHUNK_X: i32 = 5;
const CHUNK_Z: i32 = 3;

struct Assets { root: PathBuf }

impl Assets {
    fn new() -> Self {
        Self { root: std::env::var_os("LODESTONE_WORLDGEN_ASSETS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")) }
    }
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())))
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        std::fs::read_to_string(&path).ok().map_or(Value::Null, |text| serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display())))
    }
}

impl Resolver for Assets {
    fn density_function(&self, id: &str) -> Value { self.read("density_function", id) }
    fn noise(&self, id: &str) -> NoiseParams {
        let value = self.read("noise", id);
        NoiseParams { first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32, amplitudes: value["amplitudes"].as_array().expect("amplitudes").iter().map(|x| x.as_f64().expect("amplitude")).collect() }
    }
    fn biome_parameters(&self) -> Value { self.read("biome_parameters", "nether") }
    fn biome_document(&self, id: &str) -> Value { self.try_read("biome", id) }
    fn configured_carver(&self, id: &str) -> Value { self.try_read("configured_carver", id) }
    fn configured_feature(&self, id: &str) -> Value { self.try_read("configured_feature", id) }
    fn placed_feature(&self, id: &str) -> Value { self.try_read("placed_feature", id) }
    fn block_tag(&self, id: &str) -> Value { self.try_read("tags/block", id) }
}

struct WithoutMagma(Assets);

impl Resolver for WithoutMagma {
    fn density_function(&self, id: &str) -> Value { self.0.density_function(id) }
    fn noise(&self, id: &str) -> NoiseParams { self.0.noise(id) }
    fn biome_parameters(&self) -> Value { self.0.biome_parameters() }
    fn biome_document(&self, id: &str) -> Value {
        let mut document = self.0.biome_document(id);
        if let Some(steps) = document.get_mut("features").and_then(Value::as_array_mut) {
            for step in steps {
                if let Some(entries) = step.as_array_mut() {
                    for entry in entries {
                        if entry.as_str() == Some("minecraft:ore_magma") {
                            *entry = Value::String("lodestone:withheld_ore_magma".to_owned());
                        }
                    }
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

fn magma_cells(column: &NetherColumn) -> BTreeSet<(usize, i32, usize)> {
    let mut cells = BTreeSet::new();
    for y in column.min_y()..column.min_y() + column.height() {
        for lz in 0..16 { for lx in 0..16 {
            if column.block_state(lx, y, lz) == "minecraft:magma_block" { cells.insert((lx, y, lz)); }
        }}
    }
    cells
}

fn external_cells() -> BTreeSet<(usize, i32, usize)> {
    let mut seed = None; let mut chunk = None; let mut block = None; let mut count = None; let mut oracle_count = None; let mut cells = BTreeSet::new();
    for line in EXTERNAL.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("seed") => seed = Some(fields.next().expect("seed").parse().unwrap()),
            Some("chunk") => chunk = Some((fields.next().expect("chunk x").parse().unwrap(), fields.next().expect("chunk z").parse().unwrap())),
            Some("block") => block = Some(fields.next().expect("block").to_owned()),
            Some("oracle-count") => oracle_count = Some(fields.next().expect("oracle count").parse().unwrap()),
            Some("count") => count = Some(fields.next().expect("count").parse().unwrap()),
            Some("cell") => { cells.insert((fields.next().expect("x").parse().unwrap(), fields.next().expect("y").parse().unwrap(), fields.next().expect("z").parse().unwrap())); }
            _ => {}
        }
    }
    assert_eq!(seed, Some(SEED)); assert_eq!(chunk, Some((CHUNK_X, CHUNK_Z))); assert_eq!(block.as_deref(), Some("minecraft:magma_block")); assert_eq!(oracle_count, Some(508)); assert_eq!(count, Some(10));
    assert_eq!(cells.len(), 10, "fixture stores a non-vacuous witness subset of the oracle's 508 changed cells");
    cells
}

#[test]
fn external_magma_witnesses_reach_production_column() {
    let assets = Assets::new();
    let column = NetherGenerator::new(SEED, &assets.read("noise_settings", "nether"), &assets).column(CHUNK_X, CHUNK_Z);
    let actual = magma_cells(&column);
    let expected = external_cells();
    assert!(!actual.is_empty(), "control: ore_magma must produce at least one magma block");
    assert!(actual.is_superset(&expected), "production lost external magma witnesses: missing {:?}", expected.difference(&actual).collect::<Vec<_>>());
}

#[test]
fn withholding_magma_entry_is_a_live_detector_control() {
    let assets = Assets::new();
    let settings = assets.read("noise_settings", "nether");
    let full = NetherGenerator::new(SEED, &settings, &assets).column(CHUNK_X, CHUNK_Z);
    let without = NetherGenerator::new(SEED, &settings, &WithoutMagma(Assets::new())).column(CHUNK_X, CHUNK_Z);
    let expected = external_cells();
    let full_cells = magma_cells(&full); let without_cells = magma_cells(&without);
    assert_ne!(full_cells, without_cells, "detector control: withholding ore_magma must change the column");
    assert!(expected.iter().any(|cell| full_cells.contains(cell) && !without_cells.contains(cell)), "detector control: no external magma witness disappeared");
}
