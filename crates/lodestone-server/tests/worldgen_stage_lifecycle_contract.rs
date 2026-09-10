//! Keeps the server's coarse generation tiers tied to each dimension's named
//! worldgen schedule.
//!
//! `ChunkGenerationStage::Shaped` intentionally stops at a different named
//! boundary in each dimension.  These exact typed prefixes make that choice
//! reviewable and prevent a schedule edit from silently moving work across the
//! gameplay/streaming boundary.

use lodestone_server::ChunkGenerationStage;
use lodestone_worldgen::{
    end::EndGenerator,
    nether::NetherGenerator,
    overworld::OverworldGenerator,
    stage_schedule::ColumnStage::{
        self, Biomes, Carvers, Features, Fill, Materialize, Output, StructureInfluence,
        StructurePlacement, StructureReferences, StructureStarts, Surface, TopLayer,
    },
};

const OVERWORLD_SHAPED: &[ColumnStage] = &[
    StructureStarts,
    StructureReferences,
    StructureInfluence,
    Fill,
    Biomes,
    Surface,
    Materialize,
    Carvers,
    StructurePlacement,
];

const NETHER_SHAPED: &[ColumnStage] = &[
    StructureStarts,
    StructureReferences,
    StructureInfluence,
    Fill,
    Biomes,
    Surface,
    Materialize,
    Carvers,
];

const END_SHAPED: &[ColumnStage] = &[
    Fill,
    Biomes,
    Surface,
    Materialize,
    StructureStarts,
    StructurePlacement,
];

fn assert_tier_contract(
    dimension: &str,
    actual: &[ColumnStage],
    shaped: &[ColumnStage],
    full_suffix: &[ColumnStage],
) {
    assert_eq!(
        actual.len(),
        shaped.len() + full_suffix.len(),
        "{dimension} named schedule changed length; classify every pass as Shaped or Full",
    );
    assert_eq!(
        &actual[..shaped.len()],
        shaped,
        "{dimension} Shaped prefix skipped or reordered a required named pass",
    );
    assert_eq!(
        &actual[shaped.len()..],
        full_suffix,
        "{dimension} Full suffix skipped or reordered a required named pass",
    );
}

#[test]
fn every_dimension_maps_its_complete_named_schedule_to_server_tiers() {
    let _server_tiers = [ChunkGenerationStage::Shaped, ChunkGenerationStage::Full];

    assert_tier_contract(
        "Overworld",
        OverworldGenerator::stage_schedule().stages(),
        OVERWORLD_SHAPED,
        &[Features, TopLayer, Output],
    );
    assert_tier_contract(
        "Nether",
        NetherGenerator::stage_schedule().stages(),
        NETHER_SHAPED,
        &[StructurePlacement, Features, Output],
    );
    assert_tier_contract(
        "End",
        EndGenerator::stage_schedule().stages(),
        END_SHAPED,
        &[Features, Output],
    );
}
