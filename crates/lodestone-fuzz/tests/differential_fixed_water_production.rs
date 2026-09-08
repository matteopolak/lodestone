//! Production-connected replay of an externally captured four-way water trace.
//!
//! The positive case compares `FluidModelOracle` with state observations from
//! a real 26.2 Java server. The negative case corrupts the recorded side when
//! the first flowing cells arrive and proves that the shared runner reports
//! that exact tick and probe.

#[path = "support/tick_corpus.rs"]
mod tick_corpus;

use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::DifferentialOutcome;
use tick_corpus::TickCorpus;

const CAPTURED_WATER_SPREAD: &str =
    include_str!("fixtures/tick_corpus_26_2_water_spread.json");
const REPLAY_SEED: u64 = 0x5490_0003;

fn captured_replay() -> (TickCorpus, lodestone_fuzz::differential::FixedActionReplay) {
    let corpus = TickCorpus::from_json(CAPTURED_WATER_SPREAD)
        .expect("the water trace must retain real-server provenance");
    let replay = corpus
        .fixed_replay(REPLAY_SEED)
        .expect("the captured water trace must fit fixed replay bounds");
    (corpus, replay)
}

fn production_oracle() -> FluidModelOracle {
    // The capture lays a stone floor under the source and four first-step
    // probes. The model's matching flat rig supplies that same floor at every
    // unedited cell, while the captured SetBlock actions still exercise the
    // production edit and scheduling path.
    FluidModelOracle::new((0, 0, 0), -1, "minecraft:stone")
}

#[test]
fn captured_water_trace_reaches_the_production_model_tick_by_tick() {
    let (corpus, replay) = captured_replay();
    let mut recorded = corpus.recorded_oracle();
    let report = replay.run(&mut production_oracle(), &mut recorded);

    assert!(matches!(report.outcome, DifferentialOutcome::Agreed));
}

#[test]
fn captured_water_control_reports_the_first_flowing_tick_and_probe() {
    let (corpus, replay) = captured_replay();
    let mut production = production_oracle();
    let mut recorded = corpus.recorded_oracle().corrupt_at(4);
    let report = replay.run(&mut production, &mut recorded);

    let DifferentialOutcome::Diverged(divergence) = report.outcome else {
        panic!("the deliberately corrupted water trace must diverge");
    };
    assert_eq!(divergence.tick, 4);
    assert_eq!(divergence.pos, (32001, 0, 32000));
    assert_eq!(divergence.left.as_deref(), Some("minecraft:water"));
    assert_eq!(divergence.right, None);
}
