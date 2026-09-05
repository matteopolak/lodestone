//! Hermetic protocol and detector checks for externally captured tick corpora.
//!
//! A real capture is deliberately not committed until it is produced by the
//! Java-oracle recorder. These tests establish that the consumer rejects a
//! self-authored expectation and that a known bad Rust-side reader is seen at
//! the exact recorded tick.

#[path = "support/tick_corpus.rs"]
mod tick_corpus;

use std::collections::HashMap;
use std::convert::Infallible;

use lodestone_fuzz::differential::{Action, WorldOracle};
use tick_corpus::{CorpusOutcome, TickCorpus};

const CORPUS: &str = r#"{
  "format_version": 1,
  "scenario": "protocol-detector-control",
  "provenance": {
    "source": "real-java-rcon",
    "minecraft_version": "26.2",
    "capture_command": "capture-differential-ticks.py --spec protocol-detector-control.json"
  },
  "settle_ticks": 1,
  "region": [{ "pos": [0, 0, 0], "candidates": ["minecraft:air", "minecraft:stone"] }],
  "steps": [{ "tick": 0, "action": { "kind": "set_block", "pos": [0, 0, 0], "state": "minecraft:stone" } }],
  "observations": [
    { "tick": 0, "game_time": 9001, "states": ["minecraft:stone"] },
    { "tick": 1, "game_time": 9002, "states": ["minecraft:stone"] }
  ]
}"#;

#[derive(Default)]
struct CorpusWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    corrupt_on_tick: Option<u64>,
    ticks: u64,
}

struct FailingWorld;

impl WorldOracle for CorpusWorld {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            unreachable!("tick corpus permits SetBlock only");
        };
        self.blocks.insert(*pos, state.clone());
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.ticks += 1;
        if self.corrupt_on_tick == Some(self.ticks) {
            self.blocks.insert((0, 0, 0), "minecraft:air".to_owned());
        }
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        Ok(self
            .blocks
            .get(&pos)
            .filter(|state| candidates.contains(*state))
            .cloned())
    }
}

impl WorldOracle for FailingWorld {
    type Error = &'static str;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Err("model setup failed")
    }

    fn block_state(
        &mut self,
        _pos: (i32, i32, i32),
        _candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }
}

#[test]
fn external_tick_observations_replay_without_a_live_server() {
    let corpus = TickCorpus::from_json(CORPUS).expect("bounded externally shaped corpus");
    assert_eq!(corpus.scenario(), "protocol-detector-control");
    assert!(matches!(
        corpus.compare(&mut CorpusWorld::default()),
        CorpusOutcome::Agreed
    ));
}

#[test]
fn detector_control_reports_the_first_wrong_tick_and_expected_value() {
    let corpus = TickCorpus::from_json(CORPUS).expect("bounded externally shaped corpus");
    let outcome = corpus.compare(&mut CorpusWorld {
        corrupt_on_tick: Some(2),
        ..CorpusWorld::default()
    });
    let CorpusOutcome::Diverged {
        tick,
        pos,
        expected,
        actual,
    } = outcome
    else {
        panic!("the deliberate wrong reader must diverge at the recorded tick");
    };
    assert_eq!(tick, 1);
    assert_eq!(pos, (0, 0, 0));
    assert_eq!(expected.as_deref(), Some("minecraft:stone"));
    assert_eq!(actual.as_deref(), Some("minecraft:air"));
}

#[test]
fn self_authored_or_skipped_tick_corpora_are_rejected_before_model_execution() {
    for (from, to) in [
        ("real-java-rcon", "rust-model"),
        ("\"game_time\": 9002", "\"game_time\": 9003"),
    ] {
        let input = CORPUS.replacen(from, to, 1);
        assert!(TickCorpus::from_json(&input).is_err(), "invalid corpus must not execute");
    }
}

#[test]
fn a_model_failure_is_not_misreported_as_a_missing_external_state() {
    let corpus = TickCorpus::from_json(CORPUS).expect("bounded externally shaped corpus");
    let CorpusOutcome::OracleFailed(failure) = corpus.compare(&mut FailingWorld) else {
        panic!("a model failure must not become agreement or a corpus divergence");
    };
    assert_eq!(failure.tick, 0);
    assert_eq!(failure.message, "model setup failed");
}
