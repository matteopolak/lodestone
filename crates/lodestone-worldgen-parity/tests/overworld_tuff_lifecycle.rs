//! Seeded Overworld lifecycle witness for the requested-target dispatch seam.

use lodestone_data::block::Block;
use lodestone_server::overworld_chunk_source;
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
};

const TARGET: (i32, i32) = (2, 0);
const LOCAL: (i32, i32, i32) = (5, -62, 0);

#[test]
fn requested_target_replay_preserves_target_owned_witness() {
    let admissions = (-1..=1)
        .flat_map(|z| (1..=3).map(move |x| (x, z)))
        .collect::<Vec<_>>();
    let events = vec![LifecycleReplayEvent {
        source: TARGET,
        stage: LifecycleCompletion::Features,
        sequence: 0,
        resident_transitions: Vec::new(),
    }];
    let plan = LifecycleReplayPlan::for_target(TARGET, &admissions, &events)
        .expect("the 3x3 capture is a complete target replay domain");
    assert_eq!(plan.feature_events().len(), 1);

    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    materializer.replay_plan(&plan);

    assert_eq!(
        materializer
            .snapshot_for_packet(TARGET)
            .block_state_id(LOCAL.0, LOCAL.1, LOCAL.2),
        Block::Deepslate.default_state(),
    );
}

#[test]
#[should_panic(expected = "target-owned FEATURES replay requires exactly one target event")]
fn target_owned_replay_rejects_nine_source_events() {
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
        .expect("the nine-source control must be structurally valid before dispatch");
    assert_eq!(plan.feature_events().len(), 9);

    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    materializer.replay_plan(&plan);
}

#[test]
fn streamed_adjacent_targets_retain_the_prior_target_owned_spill() {
    let target = (1, 0);
    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    for x in -1..=2 {
        for z in -1..=1 {
            materializer.admit((x, z));
        }
    }
    materializer.complete_target_features_observing((0, 0), 0, |_| {});
    materializer.finish_target((0, 0));
    materializer.complete_target_features_observing(target, 1, |_| {});
    materializer.finish_target(target);

    assert_eq!(
        materializer.snapshot_for_packet(target).block_state_id(1, 27, 14),
        Block::Stone.default_state(),
        "the preceding target-owned completion retains its border write for streamed (1,0)",
    );
}

#[test]
fn target_owned_dispatch_rejects_a_nine_source_replay() {
    let source = overworld_chunk_source(42);
    let cell = (17, 27, 14);
    let source_spill = source
        .generator()
        .parity_source_decoration_with_overrides(1, 0, 0, 0, &[])
        .spills
        .into_iter()
        .find(|spill| spill.position == cell)
        .map(|spill| spill.state);
    let target_owned = source
        .generator()
        .parity_features_with_overrides(1, 0, &[])
        .spills
        .into_iter()
        .find(|spill| spill.position == cell)
        .map(|spill| spill.state);
    assert_eq!(source_spill, Some(Block::Granite.default_state()));
    assert_eq!(target_owned, None);
    assert_ne!(source_spill, target_owned);
}
