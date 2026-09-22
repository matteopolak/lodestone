//! End lifecycle replay keeps a source's decoration context source-centred.

use lodestone_data::{block::Block, block_states::StateId};
use lodestone_server::end_chunk_source;
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
};

const SEED: i64 = 42;
const TARGET: (i32, i32) = (128, 130);
const CHORUS_SOURCE: (i32, i32) = (128, 131);
const FOCUS_LOCAL: (i32, i32, i32) = (4, 67, 15);

fn source_window() -> Vec<(i32, i32)> {
    (TARGET.0 - 1..=TARGET.0 + 1)
        .flat_map(|x| (TARGET.1 - 1..=TARGET.1 + 1).map(move |z| (x, z)))
        .collect()
}

fn focus_after_direct_sources(sources: &[(i32, i32)]) -> StateId {
    let admitted = source_window();
    let mut materializer = LifecycleMaterializer::new(end_chunk_source(SEED));
    for chunk in admitted {
        materializer.admit(chunk);
    }
    for (sequence, &source) in sources.iter().enumerate() {
        materializer.complete(source, LifecycleCompletion::Features, sequence as u64);
    }
    let snapshot = materializer.snapshot_for_packet(TARGET);
    snapshot
        .block_state_id(FOCUS_LOCAL.0, FOCUS_LOCAL.1, FOCUS_LOCAL.2)
}

#[test]
fn authenticated_end_replay_uses_the_source_context_for_chorus() {
    let admissions = source_window();
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
        .expect("the complete End packet domain must be replayable");

    let mut materializer = LifecycleMaterializer::new(end_chunk_source(SEED));
    materializer.replay_plan(&plan);
    let snapshot = materializer.snapshot_for_packet(TARGET);
    let focus = snapshot.block_state_id(FOCUS_LOCAL.0, FOCUS_LOCAL.1, FOCUS_LOCAL.2);
    assert_eq!(
        focus.block(),
        Block::ChorusPlant,
        "source {CHORUS_SOURCE:?} must write the target focus cell, got {focus:?}",
    );
}

#[test]
fn withholding_the_chorus_source_is_a_live_negative_control() {
    let admissions = source_window();
    let complete = admissions.clone();
    let withheld = admissions
        .into_iter()
        .filter(|&source| source != CHORUS_SOURCE)
        .collect::<Vec<_>>();

    let complete_focus = focus_after_direct_sources(&complete);
    let withheld_focus = focus_after_direct_sources(&withheld);

    assert_eq!(complete_focus.block(), Block::ChorusPlant);
    assert_eq!(withheld_focus, Block::Air.default_state());
}
