//! End lifecycle replay keeps a source's decoration context source-centred.

use lodestone_server::{end_chunk_source, ChunkSource};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
};

const SEED: i64 = 42;
const TARGET: (i32, i32) = (128, 130);
const BANNER_TARGET: (i32, i32) = (283, 81);
const CHORUS_SOURCE: (i32, i32) = (128, 131);
const FOCUS_LOCAL: (i32, i32, i32) = (4, 67, 15);
const CROSS_CHUNK_TARGET: (i32, i32) = (285, 79);
const CROSS_CHUNK_CHORUS_SOURCE: (i32, i32) = (285, 79);
const CROSS_CHUNK_FOCUS_LOCAL: (i32, i32, i32) = (13, 65, 15);
// Captured independently from the external 26.2 End lifecycle stream.
const CROSS_CHUNK_EXPECTED: &str =
    "minecraft:chorus_plant[down=true,east=false,north=true,south=true,up=false,west=true]";

fn source_window() -> Vec<(i32, i32)> {
    (TARGET.0 - 1..=TARGET.0 + 1)
        .flat_map(|x| (TARGET.1 - 1..=TARGET.1 + 1).map(move |z| (x, z)))
        .collect()
}

fn banner_source_window() -> Vec<(i32, i32)> {
    (BANNER_TARGET.0 - 1..=BANNER_TARGET.0 + 1)
        .flat_map(|x| (BANNER_TARGET.1 - 1..=BANNER_TARGET.1 + 1).map(move |z| (x, z)))
        .collect()
}

fn focus_after_direct_sources(sources: &[(i32, i32)]) -> String {
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
        .block_state(FOCUS_LOCAL.0, FOCUS_LOCAL.1, FOCUS_LOCAL.2)
        .to_owned()
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
    let focus = snapshot.block_state(FOCUS_LOCAL.0, FOCUS_LOCAL.1, FOCUS_LOCAL.2);
    assert!(
        focus.starts_with("minecraft:chorus_plant["),
        "source {CHORUS_SOURCE:?} must write the target focus cell, got {focus}",
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

    assert!(complete_focus.starts_with("minecraft:chorus_plant["));
    assert_eq!(withheld_focus, "minecraft:air");
}

fn cross_chunk_source_window() -> Vec<(i32, i32)> {
    (CROSS_CHUNK_TARGET.0 - 1..=CROSS_CHUNK_TARGET.0 + 1)
        .flat_map(|x| {
            (CROSS_CHUNK_TARGET.1 - 1..=CROSS_CHUNK_TARGET.1 + 1).map(move |z| (x, z))
        })
        .collect()
}

fn cross_chunk_focus_after_target_replay(sources: &[(i32, i32)]) -> String {
    let mut materializer = LifecycleMaterializer::new(end_chunk_source(SEED));
    for chunk in cross_chunk_source_window() {
        materializer.admit(chunk);
    }
    materializer.begin_target(CROSS_CHUNK_TARGET);
    for (sequence, &source) in sources.iter().enumerate() {
        materializer.complete_for_target(
            CROSS_CHUNK_TARGET,
            source,
            LifecycleCompletion::Features,
            sequence as u64,
        );
    }
    materializer.finish_target(CROSS_CHUNK_TARGET);
    materializer
        .snapshot_for_packet(CROSS_CHUNK_TARGET)
        .block_state(
            CROSS_CHUNK_FOCUS_LOCAL.0,
            CROSS_CHUNK_FOCUS_LOCAL.1,
            CROSS_CHUNK_FOCUS_LOCAL.2,
        )
        .to_owned()
}

#[test]
fn exact_end_cross_chunk_chorus_connection_uses_source_context() {
    let sources = cross_chunk_source_window();
    let focus = cross_chunk_focus_after_target_replay(&sources);
    assert_eq!(
        focus, CROSS_CHUNK_EXPECTED,
        "the target's south connection must survive the source-centred End replay",
    );
}

#[test]
fn exact_end_cross_chunk_chorus_source_withheld_is_a_negative_control() {
    let sources = cross_chunk_source_window();
    let withheld = sources
        .iter()
        .copied()
        .filter(|&source| source != CROSS_CHUNK_CHORUS_SOURCE)
        .collect::<Vec<_>>();
    let complete_focus = cross_chunk_focus_after_target_replay(&sources);
    let withheld_focus = cross_chunk_focus_after_target_replay(&withheld);
    assert_eq!(complete_focus, CROSS_CHUNK_EXPECTED);
    assert_eq!(withheld_focus, "minecraft:air");
}

#[test]
fn end_lifecycle_packet_snapshot_omits_structure_sidecars() {
    let admissions = banner_source_window();
    let source = end_chunk_source(SEED);
    let direct = source.column(BANNER_TARGET.0, BANNER_TARGET.1);
    assert_eq!(
        direct
            .block_entities()
            .iter()
            .filter(|(_, entity)| entity.type_id() == "minecraft:banner")
            .count(),
        4,
        "the direct End source retains the four patterned banner sidecars"
    );

    let mut materializer = LifecycleMaterializer::new(source);
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
    let plan = LifecycleReplayPlan::for_target(BANNER_TARGET, &admissions, &events)
        .expect("the complete End lifecycle wavefront must be authenticated");
    materializer.replay_plan(&plan);

    let snapshot = materializer.snapshot_for_packet(BANNER_TARGET);
    assert!(
        snapshot.block_entities().is_empty(),
        "the external End lifecycle packet boundary carries no structure sidecars"
    );
}
