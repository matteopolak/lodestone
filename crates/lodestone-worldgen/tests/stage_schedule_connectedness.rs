use lodestone_worldgen::{
    end::EndGenerator,
    nether::NetherGenerator,
    overworld::OverworldGenerator,
    stage_schedule::{Dimension, ColumnStage},
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
