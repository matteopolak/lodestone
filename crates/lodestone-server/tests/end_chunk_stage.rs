//! External control for the End source's bounded generation stages.
//!
//! The shaped source must stop before End decoration and output. The full
//! source is the packet/save product and must include the feature writes that
//! can cross a chunk boundary.

use lodestone_server::{end_chunk_source, ChunkGenerationStage, ChunkSource};

const SEED: i64 = -195_764_831;
// The independently captured fixed platform includes world cell (100, 48, -1),
// in source chunk (6, -1).
const PLATFORM_CELL: (i32, i32, i32) = (100, 48, -1);

#[test]
fn end_shaped_stops_before_features_and_full_runs_the_feature_output_suffix() {
    let source = end_chunk_source(SEED);
    let (x, y, z) = PLATFORM_CELL;
    let cx = x.div_euclid(16);
    let cz = z.div_euclid(16);

    let shaped = source.column_at(cx, cz, ChunkGenerationStage::Shaped);
    assert_eq!(
        shaped.generation_stage(),
        ChunkGenerationStage::Shaped,
        "the End shaped path must retain its partial-column stage"
    );
    assert_eq!(
        shaped.block_state(x.rem_euclid(16), y, z.rem_euclid(16)),
        "minecraft:air",
        "a shaped End column must not run the fixed-platform FEATURES write"
    );

    let full = source.column_at(cx, cz, ChunkGenerationStage::Full);
    assert_eq!(full.generation_stage(), ChunkGenerationStage::Full);
    assert_eq!(
        full.block_state(x.rem_euclid(16), y, z.rem_euclid(16)),
        "minecraft:obsidian",
        "the FULL End column must include the fixed-platform FEATURES write"
    );
    assert_eq!(
        source.packet_generation_stage(ChunkGenerationStage::Shaped),
        Some(ChunkGenerationStage::Full),
        "cross-column End decoration requires a FULL packet product"
    );
}
