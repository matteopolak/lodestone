//! Production-connected replay of a short, externally captured tick trace.
//!
//! The positive case compares `FluidModelOracle` with the recorded state from
//! a real Java server. The negative case corrupts only the recorded side and
//! proves that the shared runner reports the first tick instead of silently
//! accepting a mismatch.

#[path = "support/tick_corpus.rs"]
mod tick_corpus;

use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::DifferentialOutcome;
use tick_corpus::TickCorpus;

const CAPTURED_STONE: &str = include_str!("fixtures/tick_corpus_26_2_stone.json");
const REPLAY_SEED: u64 = 0x549;

fn captured_replay() -> (TickCorpus, lodestone_fuzz::differential::FixedActionReplay) {
    let corpus = TickCorpus::from_json(CAPTURED_STONE)
        .expect("the checked-in trace must retain real-server provenance");
    let replay = corpus
        .fixed_replay(REPLAY_SEED)
        .expect("the captured trace must fit the fixed replay bounds");
    (corpus, replay)
}

fn production_oracle() -> FluidModelOracle {
    // The fixture writes a static block above a solid floor. This is the same
    // sparse production fluid rig used by the live differential lane, but it
    // can advance exactly without wall-clock timing in this focused test.
    FluidModelOracle::new((0, 0, 0), -1, "minecraft:stone")
}

#[test]
fn captured_trace_reaches_the_production_model_through_fixed_replay() {
    let (corpus, replay) = captured_replay();
    let mut recorded = corpus.recorded_oracle();
    let report = replay.run(&mut production_oracle(), &mut recorded);

    assert!(matches!(report.outcome, DifferentialOutcome::Agreed));
}

#[test]
fn captured_trace_control_reports_the_first_wrong_tick_and_probe() {
    let (corpus, replay) = captured_replay();
    let mut production = production_oracle();
    let mut recorded = corpus.recorded_oracle().corrupt_at(0);
    let report = replay.run(&mut production, &mut recorded);

    let DifferentialOutcome::Diverged(divergence) = report.outcome else {
        panic!("the deliberately corrupted captured side must diverge");
    };
    assert_eq!(divergence.tick, 0);
    assert_eq!(divergence.pos, (20000, 200, 20000));
    assert_eq!(divergence.left.as_deref(), Some("minecraft:stone"));
    assert_eq!(divergence.right, None);
}
