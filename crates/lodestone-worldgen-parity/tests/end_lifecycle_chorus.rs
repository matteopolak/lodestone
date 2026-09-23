use lodestone_data::{block::Block, block_states::StateId};
use lodestone_server::{end_chunk_source, ChunkSource};
use lodestone_server::worldgen_session::{GenerationRequest, GenerationSession};
use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
};

const SEED: i64 = 42;
const TARGET: (i32, i32) = (128, 130);
const CHORUS_SOURCE: (i32, i32) = (128, 131);
const FOCUS_LOCAL: (i32, i32, i32) = (4, 67, 15);
const CAPTURE: &str = include_str!("fixtures/end-chorus-128-130.txt");

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
#[ignore = "known production End mismatch against the authenticated packet fixture"]
fn production_end_request_matches_the_captured_packet_cell() {
    let field = |name: &str| {
        CAPTURE
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("capture fixture lacks {name}"))
    };
    assert_eq!(field("protocol"), "776");
    assert_eq!(field("dimension"), "end");
    assert_eq!(field("seed"), SEED.to_string());
    assert_eq!(field("target"), format!("{},{}", TARGET.0, TARGET.1));
    assert_eq!(field("focus_absolute"), "2052,67,2095");
    assert_eq!(field("expected"), "minecraft:chorus_plant");
    assert_eq!(
        field("source_order"),
        "127,129;127,130;127,131;128,129;128,130;128,131;129,129;129,130;129,131"
    );

    let source = end_chunk_source(SEED);
    let request = GenerationRequest::new(Dimension::End, TARGET, GenerationTarget::Full, 1);
    let mut session = GenerationSession::new(request);
    let packet = source
        .request_stage_driver()
        .expect("End source uses the production request driver")
        .generate(&mut session)
        .expect("production End request completes");
    let focus = packet
        .column()
        .block_state_id(FOCUS_LOCAL.0, FOCUS_LOCAL.1, FOCUS_LOCAL.2);
    assert_eq!(
        focus.block(),
        Block::ChorusPlant,
        "production yielded {focus:?}; captured packet SHA-256 is {}",
        field("packet_sha256")
    );
}

#[test]
#[ignore = "target-scoped replay currently differs from the authenticated packet fixture"]
fn lifecycle_materialization_matches_the_packet_cell() {
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
        "target-scoped replay yielded {focus:?}; captured packet SHA-256 is {}",
        CAPTURE
            .lines()
            .find_map(|line| line.strip_prefix("packet_sha256="))
            .expect("fixture packet digest"),
    );
}

#[test]
fn source_centered_synthetic_replay_has_a_live_negative_control() {
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
