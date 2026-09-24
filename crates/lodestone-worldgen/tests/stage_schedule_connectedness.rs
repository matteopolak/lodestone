use lodestone_worldgen::{
    end::EndGenerator,
    nether::NetherGenerator,
    overworld::OverworldGenerator,
    stage_schedule::{ColumnStage, Dimension, GenerationTarget, ResourceKey},
};

#[test]
fn each_generator_exposes_its_named_column_schedule() {
    assert_eq!(OverworldGenerator::stage_schedule().dimension(), Dimension::Overworld);
    assert_eq!(NetherGenerator::stage_schedule().dimension(), Dimension::Nether);
    assert_eq!(EndGenerator::stage_schedule().dimension(), Dimension::End);

    for schedule in [
        OverworldGenerator::stage_schedule(),
        NetherGenerator::stage_schedule(),
        EndGenerator::stage_schedule(),
    ] {
        assert!(schedule.index_of(ColumnStage::Fill).is_some());
        assert!(schedule.index_of(ColumnStage::Features).is_some());
        assert!(schedule.index_of(ColumnStage::Output).is_some());
    }
}

#[test]
fn task_read_and_mutable_write_radii_have_discriminating_boundaries() {
    for schedule in [
        OverworldGenerator::stage_schedule(),
        NetherGenerator::stage_schedule(),
        EndGenerator::stage_schedule(),
    ] {
        let starts = schedule
            .descriptor(ColumnStage::StructureStarts)
            .expect("every dimension computes structure starts");
        assert_eq!(starts.task_read_radius().chunks_value(), 0);
        assert_eq!(starts.mutable_write_radius().chunks_value(), 0);
        assert!(starts.task_read_radius().contains_offset(0, 0));
        assert!(!starts.task_read_radius().contains_offset(1, 0));
        assert!(starts.mutable_write_radius().contains_offset(0, 0));
        assert!(!starts.mutable_write_radius().contains_offset(0, 1));
    }

    for schedule in [
        OverworldGenerator::stage_schedule(),
        NetherGenerator::stage_schedule(),
    ] {
        let carvers = schedule
            .descriptor(ColumnStage::Carvers)
            .expect("terrain dimensions run carvers");
        assert_eq!(carvers.task_read_radius().chunks_value(), 8);
        assert_eq!(carvers.mutable_write_radius().chunks_value(), 0);
        assert!(carvers.task_read_radius().contains_offset(8, -8));
        assert!(!carvers.task_read_radius().contains_offset(9, -8));
        assert!(carvers.mutable_write_radius().contains_offset(0, 0));
        assert!(!carvers.mutable_write_radius().contains_offset(-1, 0));
    }
}

#[test]
fn nether_features_owns_the_interleaved_structure_product() {
    let schedule = NetherGenerator::stage_schedule();
    assert!(schedule.index_of(ColumnStage::StructurePlacement).is_none());
    assert_eq!(
        schedule.target_stage(GenerationTarget::Shaped),
        ColumnStage::Carvers
    );
    let features = schedule
        .descriptor(ColumnStage::Features)
        .expect("Nether feature descriptor");
    assert_eq!(
        features.inputs(),
        &[
            ResourceKey::MaterializedWorld,
            ResourceKey::BiomeQuarts,
            ResourceKey::StructureStarts,
        ]
    );
    assert_eq!(
        features.outputs(),
        &[ResourceKey::StructureBlocks, ResourceKey::ResidentOverlay]
    );
}
