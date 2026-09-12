//! Contract tests for the typed End schedule and its chunk-level frontier.

use lodestone_worldgen::stage_schedule::{
    BarrierPolicy, CancellationPolicy, ColumnStage, Dimension, GenerationLevel,
    GenerationTarget, ResourceKey, SeedScope, SidecarKey, StageFrontier, StageKey,
    StageRecord, END, STAGE_SCHEDULE_VERSION,
};

fn fingerprint(value: u8) -> [u8; 32] {
    [value; 32]
}

#[test]
fn end_schedule_is_the_single_order_for_targets_and_levels() {
    END.validate();
    assert_eq!(END.dimension(), Dimension::End);
    assert_eq!(END.stages_for(GenerationTarget::Shaped).len(), 6);
    assert_eq!(
        END.target_stage(GenerationTarget::Shaped),
        ColumnStage::StructurePlacement
    );
    assert_eq!(
        END.stages_for_level(GenerationLevel::Terrain),
        Some(&END.stages()[..3])
    );
    assert_eq!(
        END.stages_for_level(GenerationLevel::Structures),
        Some(&END.stages()[..6])
    );
    assert_eq!(
        END.stages_for_level(GenerationLevel::Decorated),
        Some(&END.stages()[..7])
    );
    assert_eq!(END.target_stage(GenerationTarget::Full), ColumnStage::Output);

    let stages: Vec<_> = END
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.key().stage())
        .collect();
    assert_eq!(stages, END.stages());
}

#[test]
fn end_descriptor_contract_covers_structure_and_outer_island_transitions() {
    let placement = END
        .descriptor(ColumnStage::StructurePlacement)
        .expect("structure placement descriptor");
    assert_eq!(placement.key().dimension(), Dimension::End);
    assert_eq!(placement.read_radius().chunks_value(), 16);
    assert_eq!(placement.write_radius().chunks_value(), 0);
    assert_eq!(placement.seed_scope(), SeedScope::StructureChunk);
    assert_eq!(placement.barrier(), BarrierPolicy::Pure);
    assert!(placement
        .outputs()
        .contains(&ResourceKey::StructureBlocks));
    assert!(placement
        .retained_sidecars()
        .contains(&SidecarKey::BlockEntityEvents));

    let features = END
        .descriptor(ColumnStage::Features)
        .expect("feature descriptor");
    assert_eq!(features.read_radius().chunks_value(), 1);
    assert_eq!(features.write_radius().chunks_value(), 1);
    assert_eq!(features.seed_scope(), SeedScope::SourceFeature);
    assert_eq!(features.barrier(), BarrierPolicy::SourceOrdered);
    assert_eq!(
        features.cancellation(),
        CancellationPolicy::TransactionBoundary
    );
    assert!(features
        .retained_sidecars()
        .contains(&SidecarKey::DecorationSpills));
    assert!(features.retained_sidecars().contains(&SidecarKey::Gateways));
}

#[test]
fn end_frontier_resumes_only_at_committed_boundaries() {
    let mut frontier = StageFrontier::new(END, (-7, 11));
    assert_eq!(frontier.next_stage(), Some(StageKey::new(Dimension::End, ColumnStage::Fill)));
    frontier
        .commit_stage(ColumnStage::Fill, fingerprint(0), fingerprint(1), 26)
        .expect("fill commit");
    assert_eq!(frontier.completed_stage_mask(), 1);
    assert_eq!(frontier.highest_level(), None);

    let wrong = StageRecord::for_descriptor(
        END.descriptor(ColumnStage::Surface).expect("surface descriptor"),
        fingerprint(2),
        fingerprint(3),
        26,
    );
    let before = frontier.records().to_vec();
    assert!(matches!(
        frontier.commit(wrong),
        Err(lodestone_worldgen::stage_schedule::FrontierError::OutOfOrder { .. })
    ));
    assert_eq!(frontier.records(), before.as_slice());

    for (index, stage) in END.stages().iter().copied().enumerate().skip(1) {
        frontier
            .commit_stage(stage, fingerprint(index as u8), fingerprint((index + 1) as u8), 26)
            .expect("ordered End commit");
    }
    assert!(frontier.is_complete());
    assert_eq!(frontier.highest_level(), Some(GenerationLevel::Output));
    assert!(frontier.is_complete_at_level(GenerationLevel::Terrain));
    assert!(frontier.is_complete_at_level(GenerationLevel::Structures));
    assert!(frontier.is_complete_at_level(GenerationLevel::Decorated));
    assert_eq!(frontier.completed_stage_mask(), 0xff);

    let restored = StageFrontier::from_records(
        END,
        (-7, 11),
        STAGE_SCHEDULE_VERSION,
        frontier.records().to_vec(),
    )
    .expect("restore frontier");
    assert_eq!(restored, frontier);
}

#[test]
fn end_frontier_rejects_missing_products_and_foreign_records() {
    let fill_key = StageKey::new(Dimension::End, ColumnStage::Fill);
    let missing = StageRecord::new(
        fill_key,
        fingerprint(0),
        fingerprint(1),
        Vec::new(),
        Vec::new(),
        26,
    );
    let mut frontier = StageFrontier::new(END, (0, 0));
    assert_eq!(
        frontier.commit(missing),
        Err(lodestone_worldgen::stage_schedule::FrontierError::MissingProduct {
            stage: fill_key,
            product: ResourceKey::DensityField,
        })
    );
    assert!(frontier.records().is_empty());

    let foreign_key = StageKey::new(Dimension::Nether, ColumnStage::Fill);
    let foreign_descriptor = lodestone_worldgen::stage_schedule::StageDescriptor::new(
        foreign_key,
        &[],
        lodestone_worldgen::stage_schedule::Radius2d::chunks(0),
        lodestone_worldgen::stage_schedule::Radius2d::chunks(0),
        &[],
        &[ResourceKey::DensityField],
        &[],
        SeedScope::TerrainChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    );
    let foreign = StageRecord::for_descriptor(
        foreign_descriptor,
        fingerprint(0),
        fingerprint(1),
        26,
    );
    assert!(matches!(
        frontier.commit(foreign),
        Err(lodestone_worldgen::stage_schedule::FrontierError::ForeignDimension { .. })
    ));
    assert_eq!(
        StageFrontier::from_records(END, (0, 0), STAGE_SCHEDULE_VERSION + 1, Vec::new()),
        Err(lodestone_worldgen::stage_schedule::FrontierError::ScheduleVersionMismatch {
            expected: STAGE_SCHEDULE_VERSION,
            found: STAGE_SCHEDULE_VERSION + 1,
        })
    );
}
