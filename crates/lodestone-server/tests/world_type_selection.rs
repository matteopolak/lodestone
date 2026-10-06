//! [`WorldType`] reaches the generator: at the same seed and the same chunks, Amplified builds
//! terrain the default world cannot, and Large Biomes changes biome far less often.
//!
//! "Terrain differs" would pass for any bug that also changes terrain, so each gate measures
//! the statistic its world type exists to change:
//!
//! * **Amplified** scales the terrain's depth and height, so its highest column top over a
//!   patch of chunks clears the default world's by a wide margin.
//! * **Large Biomes** zooms the climate noise, so a 120-chunk strip crosses far fewer biome
//!   boundaries.
//!
//! Both read shaped columns (terrain and biomes, before decoration), which is all either
//! statistic needs.

use lodestone_server::{ChunkGenerationStage, ChunkSource, WorldType, overworld_chunk_source_of_type};
use std::collections::HashSet;

const SEED: i64 = 4242;

fn shaped(world_type: WorldType, cx: i32, cz: i32) -> lodestone_server::ChunkColumn {
    overworld_chunk_source_of_type(SEED, world_type).column_at(cx, cz, ChunkGenerationStage::Shaped)
}

/// The highest non-air block over the 4x4 chunks at the origin.
fn highest_top(world_type: WorldType) -> i32 {
    let source = overworld_chunk_source_of_type(SEED, world_type);
    let mut top = i32::MIN;
    for cz in 0..4 {
        for cx in 0..4 {
            let column = source.column_at(cx, cz, ChunkGenerationStage::Shaped);
            for z in (0..16).step_by(4) {
                for x in (0..16).step_by(4) {
                    if let Some(y) = (column.min_y..column.min_y + column.height)
                        .rev()
                        .find(|&y| !matches!(column.block_state_id(x, y, z).block(), lodestone_data::block::Block::Air | lodestone_data::block::Block::CaveAir | lodestone_data::block::Block::VoidAir))
                    {
                        top = top.max(y);
                    }
                }
            }
        }
    }
    top
}

#[test]
fn amplified_rises_far_above_the_default_world_over_the_same_chunks() {
    let normal = highest_top(WorldType::Overworld);
    let amplified = highest_top(WorldType::Amplified);
    assert!(
        amplified - normal > 40,
        "amplified's highest top ({amplified}) is not decisively above the default world's ({normal})"
    );
}

#[test]
fn large_biomes_cross_far_fewer_biome_boundaries_over_the_same_strip() {
    let (normal_changes, _) = biome_transitions(WorldType::Overworld);
    let (large_changes, large_distinct) = biome_transitions(WorldType::LargeBiomes);
    assert!(normal_changes >= 8, "the default strip crossed only {normal_changes} boundaries");
    assert!(
        normal_changes >= large_changes * 4,
        "large biomes ({large_changes} changes, {large_distinct} biomes) is not decisively sparser than \
         the default world ({normal_changes} changes)"
    );
}

/// Walks chunks `0..120` along `z = 0`, sampling the biome at each chunk's corner, and returns
/// `(boundaries crossed, distinct biomes)`.
fn biome_transitions(world_type: WorldType) -> (usize, usize) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut changes = 0usize;
    let mut prev: Option<String> = None;
    for cx in 0..120 {
        let biome = shaped(world_type, cx, 0).biome_state(0, 0).to_string();
        seen.insert(biome.clone());
        if prev.as_ref().is_some_and(|p| *p != biome) {
            changes += 1;
        }
        prev = Some(biome);
    }
    (changes, seen.len())
}
