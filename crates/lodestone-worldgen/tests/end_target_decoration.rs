//! Target/source separation at the End lifecycle decoration seam.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::end::EndGenerator;
use serde_json::Value;

struct EndAssets {
    root: PathBuf,
}

impl EndAssets {
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
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    }
}

impl Resolver for EndAssets {
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

    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }

    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        self.try_read("configured_feature", id)
    }

    fn placed_feature(&self, id: &str) -> Value {
        self.try_read("placed_feature", id)
    }

    fn structure_set_ids(&self) -> Vec<String> {
        vec!["minecraft:end_cities".to_owned()]
    }

    fn structure_set(&self, id: &str) -> Value {
        self.read("structure_set", id)
    }

    fn structure(&self, id: &str) -> Value {
        self.read("structure", id)
    }

    fn biome_tag(&self, id: &str) -> Value {
        self.try_read("tags/worldgen/biome", id)
    }

    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        std::fs::read(self.root.parent()?.join("structure").join(format!("{name}.nbt"))).ok()
    }
}

#[test]
fn target_aware_source_replay_reads_the_target_window() {
    let assets = EndAssets::new();
    let generator = EndGenerator::new(42, &assets.read("noise_settings", "end"), &assets);
    let target = (134, 137);
    let source = (133, 136);
    let target_only_cell = (target.0 * 16 + 31, 70, target.1 * 16 + 31);
    let override_state = "minecraft:diamond_block".to_owned();

    let target_result = generator.parity_source_decoration_for_target_with_overrides(
        target.0,
        target.1,
        source.0,
        source.1,
        &[(
            target_only_cell.0,
            target_only_cell.1,
            target_only_cell.2,
            override_state.clone(),
        )],
    );
    assert!(
        target_result.spills.iter().all(|spill| spill.position != target_only_cell),
        "an installed target-window override is part of the read baseline",
    );

    let source_result = generator.parity_source_decoration_with_overrides(
        source.0,
        source.1,
        &[(target_only_cell.0, target_only_cell.1, target_only_cell.2, override_state)],
    );
    assert_eq!(
        source_result,
        generator.parity_source_decoration_with_overrides(source.0, source.1, &[]),
        "the source-centred control must ignore an override outside its read window",
    );
}
