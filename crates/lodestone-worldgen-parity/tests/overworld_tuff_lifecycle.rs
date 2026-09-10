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
fn source_context_rejects_target_only_diamond_witness() {
    const TARGET: (i32, i32) = (-7, -8);
    const SOURCE: (i32, i32) = (-7, -7);
    const WORLD: (i32, i32, i32) = (-107, -59, -113);
    const LOCAL: (i32, i32, i32) = (5, -59, 15);

    let source = overworld_chunk_source(42);
    let source_result = source.generator().parity_source_decoration_with_overrides(
        SOURCE.0,
        SOURCE.1,
        SOURCE.0,
        SOURCE.1,
        &[],
    );
    assert!(
        source_result
            .spills
            .iter()
            .all(|spill| spill.position != WORLD || spill.state != "minecraft:deepslate_diamond_ore"),
        "source-centred completion must not emit the target-only diamond spill at {WORLD:?}",
    );

    let admissions = (TARGET.1 - 2..=TARGET.1 + 2)
        .flat_map(|z| (TARGET.0 - 2..=TARGET.0 + 2).map(move |x| (x, z)))
        .collect::<Vec<_>>();
    let mut materializer = LifecycleMaterializer::new(source);
    materializer.prepare_lifecycle_replay(&admissions);
    materializer.admit_many_parallel(&admissions);
    materializer.begin_target(TARGET);
    for (sequence, source) in (TARGET.0 - 1..=TARGET.0 + 1)
        .flat_map(|x| (TARGET.1 - 1..=TARGET.1 + 1).map(move |z| (x, z)))
        .enumerate()
    {
        materializer.complete_for_target(
            TARGET,
            source,
            LifecycleCompletion::Features,
            sequence as u64,
        );
    }
    materializer.finish_target(TARGET);
    let snapshot = materializer.snapshot_for_packet(TARGET);
    assert_eq!(
        snapshot.block_state(LOCAL.0, LOCAL.1, LOCAL.2),
        "minecraft:deepslate[axis=y]",
        "source-centred replay must preserve the external deepslate at {WORLD:?}",
    );
}
