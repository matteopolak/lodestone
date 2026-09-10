//! Production-boundary controls for the packed-ice spike's typed JSON model.
//!
//! The unit tests in the feature module exercise the closed Serde shape. These
//! controls keep the resolver/catalog path in the loop as well: the bundled
//! asset must reach the real feature body, while a future field must demote
//! the record instead of being silently ignored.

use std::path::{Path, PathBuf};

use lodestone_worldgen::compose::build_decoration_catalog;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::feature::vegetation::ConfiguredFeature;
use serde_json::Value;

struct AssetResolver {
    root: PathBuf,
}

impl AssetResolver {
    fn json(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self
            .root
            .join("worldgen")
            .join(kind)
            .join(format!("{name}.json"));
        serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }
}

impl Resolver for AssetResolver {
    fn density_function(&self, _id: &str) -> Value {
        Value::Null
    }

    fn noise(&self, _id: &str) -> NoiseParams {
        unreachable!("the spike configuration does not use noise")
    }

    fn block_tag(&self, id: &str) -> Value {
        self.json("tags/block", id)
    }

    fn biome_document(&self, id: &str) -> Value {
        self.json("biome", id)
    }

    fn configured_feature(&self, id: &str) -> Value {
        self.json("configured_feature", id)
    }

    fn placed_feature(&self, id: &str) -> Value {
        self.json("placed_feature", id)
    }
}

#[test]
fn bundled_spike_reaches_the_catalog_and_real_feature_body() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
    let resolver = AssetResolver { root };
    let catalog = build_decoration_catalog(&resolver, &["minecraft:ice_spikes".to_owned()]);
    let selected = catalog
        .select(["minecraft:ice_spikes"])
        .into_iter()
        .find(|(_, _, placed)| placed.registry_id.as_deref() == Some("minecraft:ice_spike"))
        .expect("ice_spikes must select its placed feature");
    assert!(matches!(
        selected.2.feature.as_ref(),
        ConfiguredFeature::IceSpike(_)
    ));
}

struct MalformedResolver;

impl Resolver for MalformedResolver {
    fn density_function(&self, _id: &str) -> Value {
        Value::Null
    }

    fn noise(&self, _id: &str) -> NoiseParams {
        unreachable!("the malformed spike fixture does not use noise")
    }

    fn biome_document(&self, _id: &str) -> Value {
        let mut features = vec![Value::Array(Vec::new()); 10];
        features[7] = serde_json::json!(["minecraft:test_spike"]);
        serde_json::json!({"features": features})
    }

    fn placed_feature(&self, id: &str) -> Value {
        match id {
            "minecraft:test_spike" => {
                serde_json::json!({"feature":"minecraft:test_spike_config", "placement":[]})
            }
            _ => Value::Null,
        }
    }

    fn configured_feature(&self, id: &str) -> Value {
        match id {
            "minecraft:test_spike_config" => serde_json::json!({
                "type":"minecraft:spike",
                "config": {
                    "can_place_on": {"type":"minecraft:matching_blocks", "blocks":"minecraft:snow_block"},
                    "can_replace": {"type":"minecraft:matching_block_tag", "tag":"minecraft:ice_spike_replaceable"},
                    "state": {"Name":"minecraft:packed_ice", "future_field":true}
                }
            }),
            _ => Value::Null,
        }
    }

    fn block_tag(&self, _id: &str) -> Value {
        serde_json::json!({"values": ["minecraft:snow_block"]})
    }
}

#[test]
fn malformed_spike_schema_is_not_promoted_to_a_feature() {
    let resolver = MalformedResolver;
    let catalog = build_decoration_catalog(&resolver, &["minecraft:test".to_owned()]);
    let selected = catalog
        .select(["minecraft:test"])
        .into_iter()
        .find(|(_, _, placed)| placed.registry_id.as_deref() == Some("minecraft:test_spike"))
        .expect("test biome must retain the placed-feature identity");
    assert!(matches!(
        selected.2.feature.as_ref(),
        ConfiguredFeature::Unsupported(reason) if reason.contains("spike")
    ));
}
