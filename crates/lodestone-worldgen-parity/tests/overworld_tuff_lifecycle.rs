//! Seeded Overworld lifecycle witness for the requested-target dispatch seam.

use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
};

const TARGET: (i32, i32) = (2, 0);
const LOCAL: (i32, i32, i32) = (5, -62, 0);

#[test]
fn requested_target_replay_preserves_tuff_witness() {
    let admissions = (-1..=1)
        .flat_map(|z| (1..=3).map(move |x| (x, z)))
        .collect::<Vec<_>>();
    let events = admissions
        .iter()
        .enumerate()
        .map(|(sequence, &source)| LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence: sequence as u64,
            resident_transitions: Vec::new(),
        })
        .collect::<Vec<_>>();
    let plan = LifecycleReplayPlan::for_target(TARGET, &admissions, &events)
        .expect("the 3x3 capture is a complete target replay domain");
    assert_eq!(plan.feature_events().len(), admissions.len());

    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    materializer.replay_plan(&plan);

    assert_eq!(
        materializer
            .snapshot_for_packet(TARGET)
            .block_state(LOCAL.0, LOCAL.1, LOCAL.2),
        "minecraft:tuff",
    );
}

#[test]
fn streamed_adjacent_targets_retain_the_prior_source_spill() {
    let target = (1, 0);
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    for x in -1..=2 {
        for z in -1..=1 {
            materializer.admit((x, z));
        }
    }
    materializer.complete_for_target((0, 0), (0, 0), LifecycleCompletion::Features, 0);
    materializer.finish_target((0, 0));
    materializer.complete_for_target(target, target, LifecycleCompletion::Features, 1);
    materializer.finish_target(target);

    assert_eq!(
        materializer.snapshot_for_packet(target).block_state(1, 27, 14),
        "minecraft:granite",
        "the preceding (0,0) source owns the border write observed by streamed (1,0)",
    );
}

#[test]
fn fused_dispatch_is_a_discriminating_negative_control_for_stream_replay() {
    let source = overworld_chunk_source(42);
    let cell = (17, 27, 14);
    let source_spill = source
        .generator()
        .parity_source_decoration_with_overrides(1, 0, 0, 0, &[])
        .spills
        .into_iter()
        .find(|spill| spill.position == cell)
        .map(|spill| spill.state);
    let fused = source
        .generator()
        .parity_features_with_overrides(1, 0, &[])
        .spills
        .into_iter()
        .find(|spill| spill.position == cell)
        .map(|spill| spill.state);
    assert_eq!(source_spill, Some("minecraft:granite".to_owned()));
    assert_eq!(fused, Some("minecraft:moss_block".to_owned()));
}
