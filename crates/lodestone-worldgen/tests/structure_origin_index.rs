use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::structure::{HeightmapKind, StartContext, StructureRegistry};
use serde_json::Value;

const SEED: i64 = -195_764_831;

struct PlacementResolver;

impl Resolver for PlacementResolver {
    fn density_function(&self, _id: &str) -> Value {
        Value::Null
    }

    fn noise(&self, _id: &str) -> NoiseParams {
        NoiseParams {
            first_octave: 0,
            amplitudes: Vec::new(),
        }
    }

    fn structure_set_ids(&self) -> Vec<String> {
        vec!["test:spread".to_owned()]
    }

    fn structure_set(&self, id: &str) -> Value {
        if id == "test:spread" {
            serde_json::json!({
                "placement": {
                    "type": "minecraft:random_spread",
                    "spacing": 24,
                    "separation": 4,
                    "salt": 165745295,
                    "frequency": 0.5
                },
                "structures": []
            })
        } else {
            Value::Null
        }
    }
}

struct NoWorld;

impl StartContext for NoWorld {
    fn first_occupied_height(&self, _x: i32, _z: i32, _heightmap: HeightmapKind) -> i32 {
        63
    }

    fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
        "test:biome".to_owned()
    }

    fn sea_level(&self) -> i32 {
        63
    }
}

#[test]
fn indexed_origins_match_exhaustive_random_spread_walk() {
    let registry = StructureRegistry::new(SEED, &PlacementResolver);
    let placement = &registry.sets()[0].placement;
    let (min_x, max_x, min_z, max_z) = (-19, 20, -21, 18);
    let indexed = registry.origin_candidates_in(min_x, max_x, min_z, max_z, &NoWorld);

    let mut exhaustive = Vec::new();
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            // The index returns potential origins before the full structure
            // gate. `starts_at` applies frequency (and exclusion) after this
            // candidate enumeration, so this arm must compare placement
            // membership only.
            if placement.is_placement_chunk(SEED, x, z) {
                exhaustive.push((x, z));
            }
        }
    }
    exhaustive.sort_unstable();
    assert_eq!(indexed, exhaustive);

    let mut truncated_cell_walk = Vec::new();
    for cell_x in min_x / 24..=max_x / 24 {
        for cell_z in min_z / 24..=max_z / 24 {
            let Some(origin) = placement.potential_structure_chunk(SEED, cell_x * 24, cell_z * 24) else {
                continue;
            };
            if (min_x..=max_x).contains(&origin.0) && (min_z..=max_z).contains(&origin.1) {
                truncated_cell_walk.push(origin);
            }
        }
    }
    truncated_cell_walk.sort_unstable();
    truncated_cell_walk.dedup();
    assert_ne!(
        indexed, truncated_cell_walk,
        "the negative control must miss an origin from a neighbouring floor-divided cell"
    );
}
