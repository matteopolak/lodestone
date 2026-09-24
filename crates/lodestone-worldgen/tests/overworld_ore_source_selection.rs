//! External control for an Overworld source-biome selection boundary.
//!
//! `support/overworld_ore_ne_250_neg250_oracle.txt` is derived from two
//! independent read-only packet exports of the same frozen seed-42 world. The
//! witness cells are on chunk `(250,-250)`'s positive-Z edge. They include a
//! neighboring source body's diorite spill into the target column.

use std::path::{Path, PathBuf};

use lodestone_data::block_states::StateId;
use lodestone_worldgen::density::{NoiseParams, Resolver};
#[cfg(feature = "gen-counters")]
use lodestone_worldgen::feature::vegetation::census;
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
fn scalar_column_includes_admitted_neighbour_source_spill() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen");
    let assets = Assets { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
            .expect("read overworld settings"),
    )
    .expect("parse overworld settings");
    let generator = OverworldGenerator::new(42, &settings, &assets, "minecraft:plains", false);

    let target_owned = generator.direct_decoration_with_overrides(250, -250, &[]);
    assert_eq!(
        target_owned.column.block_state_id(11, 34, 14),
        StateId::from_state_str("minecraft:stone").expect("known state"),
        "one-source lifecycle control must not execute the neighboring source body",
    );

    let column = generator.column(250, -250);
    assert_eq!(
        column.block_state_id(11, 34, 14),
        StateId::from_state_str("minecraft:diorite").expect("known state"),
        "the scalar composition must include the positive-Z source body",
    );
    let expected = fixture();
    assert_eq!(
        expected.len(),
        20,
        "external packet control must retain every positive-Z witness"
    );

    for (x, y, z, state) in expected {
        let expected = StateId::from_state_str(state).expect("known oracle state");
        assert_eq!(
            column.block_state_id(x, y, z),
            expected,
            "external packet control differs at chunk (250,-250), local ({x},{y},{z})",
        );
    }
}

#[cfg(feature = "gen-counters")]
#[test]
fn standard_direct_features_stay_inside_the_target_radius_one_context() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen");
    let assets = Assets { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
            .expect("read overworld settings"),
    )
    .expect("parse overworld settings");
    let generator = OverworldGenerator::new(42, &settings, &assets, "minecraft:plains", false);

    for (target_x, target_z) in [(0, 0), (37, -29)] {
        census::reset();
        let _ = generator.direct_decoration_with_overrides(target_x, target_z, &[]);
        let mask = census::source_slot_mask();
        println!("direct source slots target=({target_x},{target_z}) mask={mask:#x}");
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                if dx.abs() > 1 || dz.abs() > 1 {
                    let slot = ((dx + 2) * 5 + (dz + 2)) as usize;
                    assert_eq!(
                        mask & (1u32 << slot),
                        0,
                        "target ({target_x},{target_z}) read radius-two source ({dx},{dz}); mask={mask:#x}",
                    );
                }
            }
        }
    }
}
