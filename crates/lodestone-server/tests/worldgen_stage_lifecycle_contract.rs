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
    stage_schedule::{ChunkRequest, LifecyclePhase, LIFECYCLE},
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
        &[Features, Output],
    );
    assert_tier_contract(
        "End",
        EndGenerator::stage_schedule().stages(),
        END_SHAPED,
        &[Features, Output],
    );

    assert_eq!(
        LIFECYCLE.phases().last().map(|contract| contract.phase()),
        Some(LifecyclePhase::PacketFinalization),
        "packet finalization must remain after every generation and lighting phase",
    );
}

#[test]
fn task_mutation_and_packet_light_radii_keep_distinct_contracts() {
    let request = ChunkRequest::single(0, 0, 2);
    assert_eq!(request.admission_radius(), 2);

    let starts = OverworldGenerator::stage_schedule()
        .descriptor(StructureStarts)
        .expect("Overworld structure-start task");
    assert_eq!(starts.task_read_radius().chunks_value(), 0);
    assert_eq!(starts.mutable_write_radius().chunks_value(), 0);

    let structure_references = LIFECYCLE
        .contract(LifecyclePhase::StructureReferences)
        .expect("structure-reference lifecycle phase");
    assert!(structure_references.dependencies().iter().any(|dependency| {
        dependency.phase() == LifecyclePhase::StructureStarts
            && dependency.dependency_radius() == 8
    }));
    assert_eq!(
        LIFECYCLE.immediate_reverse_dependency_radius(LifecyclePhase::StructureStarts),
        Some(8)
    );
    assert_eq!(
        LIFECYCLE.reverse_dependency_radius(LifecyclePhase::StructureStarts),
        Some(9)
    );

    let carvers = OverworldGenerator::stage_schedule()
        .descriptor(Carvers)
        .expect("Overworld carver task");
    assert_eq!(carvers.task_read_radius().chunks_value(), 8);
    assert_eq!(carvers.mutable_write_radius().chunks_value(), 0);

    let features = LIFECYCLE
        .contract(LifecyclePhase::Features)
        .expect("feature lifecycle phase");
    assert_eq!(features.block_write_radius(), 1);
    assert!(features.dependencies().iter().any(|dependency| {
        dependency.phase() == LifecyclePhase::Carvers
            && dependency.dependency_radius() == 1
    }));

    let packet = LIFECYCLE
        .contract(LifecyclePhase::PacketFinalization)
        .expect("packet lifecycle phase");
    assert!(packet.dependencies().iter().any(|dependency| {
        dependency.phase() == LifecyclePhase::Light && dependency.dependency_radius() == 1
    }));
    assert_eq!(LIFECYCLE.packet_target_radius(), Some(0));
    assert_eq!(LIFECYCLE.packet_light_radius(), Some(1));
}
