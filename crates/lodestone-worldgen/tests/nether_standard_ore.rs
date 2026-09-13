//! Focused production coverage for the Nether's biome-specific soul-sand ore.
//!
//! The configured feature is an ordinary connected ore body, but its placed
//! entry is unique to the soul-sand valley. The test keeps the expected cells
//! in an external fixture and compares the complete production column with a
//! resolver that withholds only this entry.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::{NetherColumn, NetherGenerator};
use serde_json::Value;

const EXTERNAL: &str = include_str!("support/nether_soul_sand_ore_external.txt");
const SEED: i64 = 42;
const CHUNK_X: i32 = 16;
const CHUNK_Z: i32 = 24;

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

/// Keep every production input except the one placed entry under test.
struct WithoutSoulSandOre(ExternalAssets);

impl Resolver for WithoutSoulSandOre {
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
        let mut document = self.0.biome_document(id);
        let Some(entries) = document
            .get_mut("features")
            .and_then(Value::as_array_mut)
            .and_then(|steps| steps.get_mut(7))
            .and_then(Value::as_array_mut)
        else {
            return document;
        };
        for entry in entries {
            if entry.as_str() == Some("minecraft:ore_soul_sand") {
                *entry = Value::String("lodestone:withheld_soul_sand_ore".to_owned());
            }
        }
        document
    }

    fn configured_carver(&self, id: &str) -> Value {
        self.0.configured_carver(id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        self.0.configured_feature(id)
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

fn soul_sand_cells(column: &NetherColumn) -> BTreeSet<(usize, i32, usize)> {
    let mut cells = BTreeSet::new();
    for y in column.min_y()..column.min_y() + column.height() {
        for lz in 0..16 {
            for lx in 0..16 {
                if column.block_state(lx, y, lz) == "minecraft:soul_sand" {
                    cells.insert((lx, y, lz));
                }
            }
        }
    }
    cells
}

struct ExternalFixture {
    seed: i64,
    chunk: (i32, i32),
    step: i32,
    scope: String,
    feature: String,
    block: String,
    region: String,
    freeze_sha256: String,
    bounds: (i32, i32, i32, i32),
    cells: BTreeSet<(usize, i32, usize)>,
}

fn external_fixture() -> ExternalFixture {
    let mut seed = None;
    let mut chunk = None;
    let mut step = None;
    let mut scope = None;
    let mut feature = None;
    let mut block = None;
    let mut region = None;
    let mut freeze_sha256 = None;
    let mut bounds = None;
    let mut count: Option<usize> = None;
    let mut cells = BTreeSet::new();
    for line in EXTERNAL.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("seed") => seed = Some(fields.next().expect("seed").parse().unwrap()),
            Some("chunk") => {
                chunk = Some((
                    fields.next().expect("chunk x").parse().unwrap(),
                    fields.next().expect("chunk z").parse().unwrap(),
                ));
            }
            Some("step") => step = Some(fields.next().expect("step").parse().unwrap()),
            Some("scope") => scope = Some(fields.next().expect("scope").to_owned()),
            Some("feature") => feature = Some(fields.next().expect("feature").to_owned()),
            Some("block") => block = Some(fields.next().expect("block").to_owned()),
            Some("region") => region = Some(fields.next().expect("region").to_owned()),
            Some("freeze-sha256") => {
                freeze_sha256 = Some(fields.next().expect("freeze digest").to_owned());
            }
            Some("bounds") => {
                bounds = Some((
                    fields.next().expect("min x").parse().unwrap(),
                    fields.next().expect("max x").parse().unwrap(),
                    fields.next().expect("min z").parse().unwrap(),
                    fields.next().expect("max z").parse().unwrap(),
                ));
            }
            Some("count") => count = Some(fields.next().expect("count").parse().unwrap()),
            Some("cell") => {
                cells.insert((
                    fields.next().expect("cell x").parse().unwrap(),
                    fields.next().expect("cell y").parse().unwrap(),
                    fields.next().expect("cell z").parse().unwrap(),
                ));
            }
            _ => {}
        }
    }
    let fixture = ExternalFixture {
        seed: seed.expect("seed"),
        chunk: chunk.expect("chunk"),
        step: step.expect("step"),
        scope: scope.expect("scope"),
        feature: feature.expect("feature"),
        block: block.expect("block"),
        region: region.expect("region"),
        freeze_sha256: freeze_sha256.expect("freeze digest"),
        bounds: bounds.expect("bounds"),
        cells,
    };
    assert_eq!(fixture.seed, SEED);
    assert_eq!(fixture.chunk, (CHUNK_X, CHUNK_Z));
    assert_eq!(fixture.step, 7);
    assert_eq!(fixture.scope, "full-column");
    assert_eq!(fixture.feature, "minecraft:ore_soul_sand");
    assert_eq!(fixture.block, "minecraft:soul_sand");
    assert_eq!(fixture.region, "authenticated-nether-51x51");
    assert_eq!(
        fixture.freeze_sha256,
        "8e3156fc42dff5e344520f75ba08b5e29a2e08eacdf839bb33ab2cce2ef9a257"
    );
    assert_eq!(fixture.bounds, (-26, 26, -26, 26));
    assert_eq!(fixture.cells.len(), 65);
    assert_eq!(count.expect("count"), fixture.cells.len());
    fixture
}

#[test]
fn soul_sand_ore_config_is_present_in_the_bundled_nether_data() {
    let assets = ExternalAssets::new();
    let biome = assets.read("biome", "minecraft:soul_sand_valley");
    assert!(
        biome["features"][7]
            .as_array()
            .expect("step 7")
            .iter()
            .any(|entry| entry.as_str() == Some("minecraft:ore_soul_sand")),
        "the soul-sand valley must retain the ore's raw step-7 entry"
    );
    let placed = assets.read("placed_feature", "minecraft:ore_soul_sand");
    assert_eq!(placed["feature"], "minecraft:ore_soul_sand");
    let configured = assets.read("configured_feature", "minecraft:ore_soul_sand");
    assert_eq!(configured["type"], "minecraft:ore");
    assert_eq!(configured["config"]["size"], 12);
    assert_eq!(
        configured["config"]["targets"][0]["state"]["Name"],
        "minecraft:soul_sand"
    );
}

#[test]
fn external_soul_sand_ore_cells_reach_the_production_nether_column() {
    let fixture = external_fixture();
    let assets = ExternalAssets::new();
    let settings = settings(&assets);
    let with_ore = NetherGenerator::new(SEED, &settings, &assets);
    let without_ore = NetherGenerator::new(
        SEED,
        &settings,
        &WithoutSoulSandOre(ExternalAssets::new()),
    );
    let with_column = with_ore.column(CHUNK_X, CHUNK_Z);
    let with = soul_sand_cells(&with_column);
    let without = soul_sand_cells(&without_ore.column(CHUNK_X, CHUNK_Z));
    let produced: BTreeSet<_> = with.difference(&without).copied().collect();
    assert_eq!(
        produced, fixture.cells,
        "the production column's feature-only soul-sand cells differ from the authenticated external capture"
    );
    assert!(
        !produced.is_empty(),
        "the selected production column must exercise the ore consumer"
    );
    assert!(
        fixture
            .cells
            .iter()
            .all(|&(x, y, z)| with_column.block_state(x, y, z) == "minecraft:soul_sand"),
        "every captured cell must be the configured target state in the production column"
    );
}
