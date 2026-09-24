use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use lodestone_worldgen::rng::{RandomSource, WorldgenRandom, XoroshiroRandomSource};

fn feature_path(root: &Path, kind: &str, id: &str) -> PathBuf {
    root.join(kind)
        .join(format!("{}.json", id.strip_prefix("minecraft:").unwrap_or(id)))
}

fn document_has_clamped_normal(root: &Path, kind: &str, id: &str) -> bool {
    fs::read_to_string(feature_path(root, kind, id))
        .expect("worldgen asset must exist")
        .contains("clamped_normal")
}

fn nether_feature_documents_have_no_gaussian(root: &Path) -> bool {
    let parameters: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("biome_parameters/nether.json"))
            .expect("Nether biome parameters must exist"),
    )
    .expect("Nether biome parameters must be JSON");
    let mut pending = Vec::new();
    for entry in parameters.as_array().expect("Nether parameters are a list") {
        let biome = entry
            .as_array()
            .and_then(|values| values.last())
            .and_then(serde_json::Value::as_str)
            .expect("Nether parameter entry must name a biome");
        let biome: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("biome").join(format!(
                "{}.json",
                biome.strip_prefix("minecraft:").unwrap_or(biome)
            )))
            .expect("Nether biome asset must exist"),
        )
        .expect("Nether biome must be JSON");
        for feature in biome["features"]
            .as_array()
            .expect("Nether biome features are a list")
            .iter()
            .flat_map(|step| step.as_array().into_iter().flatten())
            .filter_map(serde_json::Value::as_str)
        {
            pending.push(("placed_feature", feature.to_owned()));
        }
    }

    let mut visited = HashSet::new();
    while let Some((kind, id)) = pending.pop() {
        if !visited.insert((kind, id.clone())) {
            continue;
        }
        let document: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(feature_path(root, kind, &id))
                .expect("referenced Nether feature asset must exist"),
        )
        .expect("referenced Nether feature must be JSON");
        if document.to_string().contains("clamped_normal") {
            return false;
        }
        let mut values = vec![&document];
        while let Some(value) = values.pop() {
            match value {
                serde_json::Value::String(id) => {
                    for candidate_kind in ["placed_feature", "configured_feature"] {
                        if feature_path(root, candidate_kind, id).is_file() {
                            pending.push((candidate_kind, id.clone()));
                        }
                    }
                }
                serde_json::Value::Array(entries) => values.extend(entries),
                serde_json::Value::Object(fields) => values.extend(fields.values()),
                _ => {}
            }
        }
    }
    true
}

#[test]
fn nether_feature_assets_do_not_reach_gaussian_consumers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen");

    assert!(document_has_clamped_normal(&root, "placed_feature", "minecraft:sulfur_spike"));
    assert!(nether_feature_documents_have_no_gaussian(&root));
}

#[test]
fn feature_reseed_discriminates_shared_gaussian_cache_from_new_wrapper() {
    let mut shared = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let decoration_seed = shared.set_decoration_seed(42, -4_000, -4_000);
    shared.set_feature_seed(decoration_seed, 0, 7);
    let _first = shared.next_gaussian();
    shared.set_feature_seed(decoration_seed, 9, 7);
    let cached = shared.next_gaussian();
    let shared_following = shared.next_int();
    assert!((cached - 0.6972780646791461).abs() < 1e-12);
    assert_eq!(shared_following, 510_353_045);
    assert_eq!(shared.count(), 9);

    let mut separated = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let decoration_seed = separated.set_decoration_seed(42, -4_000, -4_000);
    separated.set_feature_seed(decoration_seed, 9, 7);
    let fresh = separated.next_gaussian();
    let separated_following = separated.next_int();

    assert!((fresh + 0.4586782596500431).abs() < 1e-12);
    assert_eq!(separated_following, 2_042_154_790);
    assert_eq!(separated.count(), 9);
    assert_ne!(cached, fresh);
    assert_ne!(shared_following, separated_following);
}
