//! Production-connected replay of the externally measured redstone timing
//! trace, including a delayed repeater path across chunk seams.
//!
//! The positive case compares `RedstoneModelOracle` with the recorded state
//! for every tick. The negative case corrupts the delayed probe at its measured
//! arrival tick and proves first-divergence reporting remains live.

#[path = "support/tick_corpus.rs"]
mod tick_corpus;

#[path = "contraption/mod.rs"]
mod contraption;

use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::DifferentialOutcome;
use tick_corpus::TickCorpus;

const CAPTURED_REDSTONE: &str = include_str!("fixtures/tick_corpus_26_2_redstone.json");
const REPLAY_SEED: u64 = 0x5490_0002;

fn captured_replay() -> (TickCorpus, lodestone_fuzz::differential::FixedActionReplay) {
    let corpus = TickCorpus::from_json(CAPTURED_REDSTONE)
        .expect("the redstone trace must retain real-server provenance");
    let replay = corpus
        .fixed_replay(REPLAY_SEED)
        .expect("the captured redstone trace must fit fixed replay bounds");
    (corpus, replay)
}

fn production_oracle() -> RedstoneModelOracle {
    let mut ours = RedstoneModelOracle::new(
        contraption::origin_on_lane(0),
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    );
    for (pos, state) in contraption::components() {
        ours.place_static(pos, &state);
    }
    ours
}

#[test]
fn captured_redstone_trace_reaches_the_production_model_tick_by_tick() {
    let (corpus, replay) = captured_replay();
    let mut recorded = corpus.recorded_oracle();
    let report = replay.run(&mut production_oracle(), &mut recorded);

    assert!(matches!(report.outcome, DifferentialOutcome::Agreed));
}

#[test]
fn captured_redstone_control_reports_the_delayed_probe_divergence() {
    let (corpus, replay) = captured_replay();
    let mut production = production_oracle();
    let mut recorded = corpus.recorded_oracle().corrupt_at(9);
    let report = replay.run(&mut production, &mut recorded);

    let DifferentialOutcome::Diverged(divergence) = report.outcome else {
        panic!("the deliberately corrupted redstone trace must diverge");
    };
    assert_eq!(divergence.tick, 9);
    assert_eq!(divergence.pos, (16, 0, 0));
    assert_eq!(divergence.left.as_deref(), Some("minecraft:redstone_wire[power=8]"));
    assert_eq!(divergence.right, None);
}
