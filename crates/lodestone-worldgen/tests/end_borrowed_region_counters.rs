//! One process-local counter gate for End source replay's shared base window.

#![cfg(feature = "gen-counters")]

use std::path::{Path, PathBuf};

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_worldgen::counters::{self, MemoryBoundary};
use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::end::EndGenerator;
use serde_json::Value;

struct EndAssets(PathBuf);

impl EndAssets {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.0.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }

    fn optional(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        std::fs::read_to_string(self.0.join(kind).join(format!("{name}.json")))
            .ok().and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    }
}

impl Resolver for EndAssets {
    fn density_function(&self, id: &str) -> Value { self.read("density_function", id) }

    fn noise(&self, id: &str) -> NoiseParams {
        let value = self.read("noise", id);
        NoiseParams {
            first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: value["amplitudes"].as_array().expect("amplitudes")
                .iter().map(|value| value.as_f64().expect("amplitude")).collect(),
        }
    }

    fn block_tag(&self, id: &str) -> Value { self.optional("tags/block", id) }
    fn biome_document(&self, id: &str) -> Value { self.optional("biome", id) }
    fn configured_feature(&self, id: &str) -> Value { self.optional("configured_feature", id) }
    fn placed_feature(&self, id: &str) -> Value { self.optional("placed_feature", id) }
}

fn detect_base_copy(snapshot: &counters::Snapshot) -> Result<(), &'static str> {
    let base = MemoryBoundary::EndReplayBase as usize;
    if snapshot.logical_writes[base] != 0 {
        return Err("End replay stitched base cells");
    }
    if snapshot.logical_write_bytes[base] != 0 {
        return Err("End replay copied base bytes");
    }
    Ok(())
}

#[test]
fn end_replay_shares_base_cells_and_records_only_transient_entries() {
    let base_boundary = MemoryBoundary::EndReplayBase as usize;
    let overlay_boundary = MemoryBoundary::EndReplayOverlay as usize;
    let mut stitched = DenseBlockGrid::with_default(80, 0, -16, 48, 256, 48, StateId::AIR);
    let bases: [_; 9] = std::array::from_fn(|slot| {
        DenseBlockGrid::with_default(
            80 + (slot / 3) as i32 * 16, 0, -16 + (slot % 3) as i32 * 16,
            16, 256, 16, StateId::AIR,
        )
    });
    counters::reset();
    for base in &bases {
        let (x, y, z, _, height, _) = base.bounds();
        stitched.copy_box_from(base, x, y, z, x, y, z, 16, height, 16);
        counters::bump_end_region_base_copy(16 * height as u64 * 16);
    }
    let control = counters::snapshot();
    assert_eq!(control.logical_writes[base_boundary], 9 * 16 * 256 * 16);
    assert_eq!(control.logical_write_bytes[base_boundary], 1_179_648);
    assert_eq!(detect_base_copy(&control), Err("End replay stitched base cells"));
    drop((stitched, bases));

    let assets = EndAssets(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-server/assets/worldgen"));
    let generator = EndGenerator::new(42, &assets.read("noise_settings", "end"), &assets);
    let gold = Block::GoldBlock.default_state();
    for (target, platform_columns) in [((6, 0), 25u64), ((7, 1), 15u64)] {
        for cx in target.0 - 1..=target.0 + 1 {
            for cz in target.1 - 1..=target.1 + 1 {
                generator.base_world_for_batch(cx, cz);
            }
        }
        let untouched = (target.0 * 16 + 31, 73, target.1 * 16 + 31);
        counters::reset();
        let result = generator.parity_source_decoration_for_target_with_overrides(
            target.0, target.1, 6, 0,
            &[
                (100, 48, 0, gold),
                (untouched.0, untouched.1, untouched.2, gold),
                ((target.0 + 2) * 16, 73, target.1 * 16 + 7, gold),
            ],
        );
        let actual = counters::snapshot();
        assert_eq!(detect_base_copy(&actual), Ok(()));
        // Each platform column writes one base row and clears three rows.
        // Capture and base comparison each read once, except the seeded cell;
        // the two accepted overrides themselves each read their base once.
        assert_eq!(actual.logical_reads[base_boundary], platform_columns * 4 * 2 + 1);
        assert_eq!(actual.logical_read_bytes[base_boundary], actual.logical_reads[base_boundary] * 2);
        assert_eq!(actual.logical_writes[overlay_boundary], platform_columns + 1);
        assert_eq!(actual.logical_write_bytes[overlay_boundary], (platform_columns + 1) * 4);
        assert_eq!(result.spills.len() as u64, platform_columns);
        assert!(result.spills.iter().any(|spill| {
            spill.position == (100, 48, 0) && spill.state == Block::Obsidian.default_state()
        }));
        assert!(result.spills.iter().all(|spill| spill.position != untouched));
        assert!(result.structure_blocks.mutations().is_empty());
    }
}
