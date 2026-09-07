//! Independent direct step-7 controls for the Nether's scattered debris feature.
//!
//! The expected cells are from a separately captured feature trace starting at
//! the post-ore terrain. The second test replaces only the configured body type
//! with the standard ore body, so it exercises the same terrain, placement
//! modifiers, and source index while proving that the scattered body is the
//! path producing the traced cells. The sealed packet remains an end-to-end
//! integration gate until source-spill lifecycle state is modeled here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::{NetherColumn, NetherGenerator};
use serde_json::Value;

const SEED: i64 = 42;
const CHUNK_X: i32 = -8;
const CHUNK_Z: i32 = -8;
const EXTERNAL: &str = include_str!("support/nether_scattered_ore_external.txt");

struct ExternalAssets {
    root: PathBuf,
}

impl ExternalAssets {
    fn new() -> Self {
        Self {
            root: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-server/assets/worldgen"),
        }
    }

    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display())),
            Err(_) => Value::Null,
        }
    }
}

impl Resolver for ExternalAssets {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }

    fn noise(&self, id: &str) -> NoiseParams {
        let value = self.read("noise", id);
        NoiseParams {
            first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: value["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                .collect(),
        }
    }

    fn biome_parameters(&self) -> Value {
        self.read("biome_parameters", "nether")
    }

    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }

    fn configured_carver(&self, id: &str) -> Value {
        self.try_read("configured_carver", id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        self.try_read("configured_feature", id)
    }

    fn placed_feature(&self, id: &str) -> Value {
        self.try_read("placed_feature", id)
    }

    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }
}

/// The negative-control resolver preserves every input except the scattered
/// feature body, making a standard connected placement run at the same index.
struct StandardOreControl(ExternalAssets);

impl Resolver for StandardOreControl {
    fn density_function(&self, id: &str) -> Value {
        self.0.density_function(id)
    }

    fn noise(&self, id: &str) -> NoiseParams {
        self.0.noise(id)
    }

    fn biome_parameters(&self) -> Value {
        self.0.biome_parameters()
    }

    fn biome_document(&self, id: &str) -> Value {
        self.0.biome_document(id)
    }

    fn configured_carver(&self, id: &str) -> Value {
        self.0.configured_carver(id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        let mut value = self.0.configured_feature(id);
        if value["type"].as_str() == Some("minecraft:scattered_ore") {
            value["type"] = Value::String("minecraft:ore".to_owned());
        }
        value
    }

    fn placed_feature(&self, id: &str) -> Value {
        self.0.placed_feature(id)
    }

    fn block_tag(&self, id: &str) -> Value {
        self.0.block_tag(id)
    }
}

fn settings(assets: &ExternalAssets) -> Value {
    assets.read("noise_settings", "nether")
}

fn debris_cells(column: &NetherColumn) -> BTreeSet<(usize, i32, usize)> {
    let mut cells = BTreeSet::new();
    for y in column.min_y()..column.min_y() + column.height() {
        for lz in 0..16 {
            for lx in 0..16 {
                if column.block_state(lx, y, lz) == "minecraft:ancient_debris" {
                    cells.insert((lx, y, lz));
                }
            }
        }
    }
    cells
}

fn external_cells() -> (usize, BTreeSet<(usize, i32, usize)>) {
    let mut seed = None;
    let mut chunk = None;
    let mut step = None;
    let mut count = None;
    let mut cells = BTreeSet::new();
    for line in EXTERNAL.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("seed") => seed = Some(fields.next().expect("seed value").parse().unwrap()),
            Some("chunk") => {
                let x = fields.next().expect("chunk x").parse().unwrap();
                let z = fields.next().expect("chunk z").parse().unwrap();
                chunk = Some((x, z));
            }
            Some("step") => step = Some(fields.next().expect("step value").parse().unwrap()),
            Some("count") => count = Some(fields.next().expect("count value").parse().unwrap()),
            Some("cell") => {
                let x = fields.next().expect("cell x").parse().unwrap();
                let y = fields.next().expect("cell y").parse().unwrap();
                let z = fields.next().expect("cell z").parse().unwrap();
                cells.insert((x, y, z));
            }
            _ => {}
        }
    }
    assert_eq!(seed, Some(SEED), "direct trace seed must match the test input");
    assert_eq!(chunk, Some((CHUNK_X, CHUNK_Z)), "direct trace chunk must match the test input");
    assert_eq!(step, Some(7), "direct trace must cover Nether step 7");
    let count = count.expect("external count");
    assert_eq!(count, cells.len(), "external count must match its cells");
    (count, cells)
}

#[test]
fn direct_step7_trace_cells_and_count_reach_production_nether() {
    let (expected_count, expected_cells) = external_cells();
    let assets = ExternalAssets::new();
    let generator = NetherGenerator::new(SEED, &settings(&assets), &assets);
    let column = generator.column(CHUNK_X, CHUNK_Z);
    let actual = debris_cells(&column);
    assert_eq!(
        actual.len(),
        expected_count,
        "ancient-debris count at chunk ({CHUNK_X},{CHUNK_Z}) differs from the direct step-7 trace"
    );
    assert_eq!(
        actual, expected_cells,
        "ancient-debris cells at chunk ({CHUNK_X},{CHUNK_Z}) differ from the direct step-7 trace"
    );
}

#[test]
fn standard_ore_negative_control_does_not_match_scattered_trace() {
    let (expected_count, expected_cells) = external_cells();
    let assets = ExternalAssets::new();
    let generator = NetherGenerator::new(
        SEED,
        &settings(&assets),
        &StandardOreControl(assets),
    );
    let actual = debris_cells(&generator.column(CHUNK_X, CHUNK_Z));
    assert_ne!(
        (actual.len(), actual),
        (expected_count, expected_cells),
        "standard connected ore placement unexpectedly reproduced the scattered direct trace"
    );
}
