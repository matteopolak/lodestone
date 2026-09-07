//! External control for an Overworld source-biome selection boundary.
//!
//! `support/overworld_ore_ne_250_neg250_oracle.txt` is derived from two
//! independent read-only packet exports of the same frozen seed-42 world. The
//! witness cells are on chunk `(250,-250)`'s positive-Z edge. Treating the
//! surrounding eight biome containers as part of this chunk's feature list
//! produced an extra copper source there; a source owns only the full section
//! biome container stored by that source chunk. The nine-source driver still
//! provides neighbouring read/write context.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

const ORACLE: &str = include_str!("support/overworld_ore_ne_250_neg250_oracle.txt");

struct Assets {
    root: PathBuf,
}

impl Assets {
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
        std::fs::read_to_string(&path)
            .ok()
            .map(|text| {
                serde_json::from_str(&text)
                    .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
            })
            .unwrap_or(Value::Null)
    }
}

impl Resolver for Assets {
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
        self.read("biome_parameters", "overworld")
    }

    fn biome_temperatures(&self) -> Value {
        self.read("biome_parameters", "overworld_temperature")
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

fn fixture() -> Vec<(usize, i32, usize, &'static str)> {
    ORACLE
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (coordinates, state) = line.split_once(' ').expect("oracle row");
            let mut values = coordinates.split(',');
            let x = values.next().expect("x").parse().expect("integer x");
            let y = values.next().expect("y").parse().expect("integer y");
            let z = values.next().expect("z").parse().expect("integer z");
            assert!(values.next().is_none(), "only x,y,z in oracle row");
            (x, y, z, state)
        })
        .collect()
}

#[test]
fn source_biome_selection_does_not_import_neighbour_copper_into_the_ne_edge() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen");
    let assets = Assets { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
            .expect("read overworld settings"),
    )
    .expect("parse overworld settings");
    let generator = OverworldGenerator::new(42, &settings, &assets, "minecraft:plains", false);
    let column = generator.column(250, -250);
    let expected = fixture();
    assert_eq!(
        expected.len(),
        20,
        "external packet control must retain every positive-Z witness"
    );

    for (x, y, z, state) in expected {
        assert_eq!(
            column.block_state(x, y, z),
            state,
            "external packet control differs at chunk (250,-250), local ({x},{y},{z})",
        );
    }
}
