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
    let events = vec![LifecycleReplayEvent {
        source: TARGET,
        stage: LifecycleCompletion::Features,
        sequence: 0,
        resident_transitions: Vec::new(),
    }];
    let plan = LifecycleReplayPlan::for_target(TARGET, &admissions, &events)
        .expect("the 3x3 capture is a complete target replay domain");
    assert_eq!(plan.feature_events().len(), 1, "Overworld FEATURES has one centre completion");
    assert_eq!(plan.feature_events()[0].source, TARGET);

    let mut materializer = LifecycleMaterializer::new(overworld_chunk_source(42));
    materializer.replay_plan(&plan);

    assert_eq!(
        materializer
            .snapshot_for_packet(TARGET)
            .block_state(LOCAL.0, LOCAL.1, LOCAL.2),
        "minecraft:tuff",
    );
}
