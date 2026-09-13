//! Hermetic coverage for fixed differential action-script replays.
//!
//! The fake oracle is deliberately small: this test proves the replay wrapper
//! preserves the existing tick-aligned comparison contract without depending
//! on a server container. Live callers use the same `WorldOracle` seam through
//! `RconOracle`.

use std::collections::HashMap;
use std::convert::Infallible;

use lodestone_fuzz::differential::{
    Action, BlockStateProbe, BlockStateRegion, DifferentialOutcome, FixedActionReplay,
    FixedReplayError, MAX_FIXED_REPLAY_TICKS, Script, ScriptStep, WorldOracle,
};

const TARGET: (i32, i32, i32) = (0, 0, 0);
const AIR: &str = "minecraft:air";
const STONE: &str = "minecraft:stone";

#[derive(Default)]
struct FakeOracle {
    blocks: HashMap<(i32, i32, i32), String>,
    ticks: u64,
    diverge_on_tick: Option<u64>,
}

impl WorldOracle for FakeOracle {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            unreachable!("the fixed replay fixture uses SetBlock only");
        };
        self.blocks.insert(*pos, state.clone());
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.ticks += 1;
        if self.diverge_on_tick == Some(self.ticks) {
            self.blocks.insert(TARGET, AIR.to_owned());
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

fn region() -> BlockStateRegion {
    BlockStateRegion::new(vec![BlockStateProbe {
        pos: TARGET,
        candidates: vec![AIR.to_owned(), STONE.to_owned()],
    }])
    .expect("the fixture region is bounded and nonempty")
}

fn replay() -> FixedActionReplay {
    FixedActionReplay::new(
        0x549,
        Script::new(vec![ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: TARGET,
                state: STONE.to_owned(),
            },
        }]),
        region(),
        2,
    )
    .expect("the fixture replay is bounded")
}

#[test]
fn fixed_replay_agreement_uses_the_same_hermetic_oracle_seam() {
    let replay = replay();
    let report = replay.run(&mut FakeOracle::default(), &mut FakeOracle::default());

    assert_eq!(report.replay, replay);
    assert!(matches!(report.outcome, DifferentialOutcome::Agreed));
}

#[test]
fn fixed_replay_reports_the_first_divergence_with_the_replayable_case() {
    let replay = replay();
    let mut left = FakeOracle::default();
    let mut right = FakeOracle {
        // The second advance happens while outer loop tick 1 is being
        // compared, proving the wrapper does not defer comparison to the
        // script's settle boundary.
        diverge_on_tick: Some(2),
        ..FakeOracle::default()
    };

    let report = replay.run(&mut left, &mut right);

    assert_eq!(report.replay, replay);
    assert_eq!(report.replay.seed, 0x549);
    let DifferentialOutcome::Diverged(divergence) = report.outcome else {
        panic!("the deliberately faulty fake must diverge");
    };
    assert_eq!(divergence.tick, 1);
    assert_eq!(divergence.pos, TARGET);
    assert_eq!(divergence.left.as_deref(), Some(STONE));
    assert_eq!(divergence.right.as_deref(), Some(AIR));
}

#[test]
fn fixed_replay_rejects_unbounded_or_ambiguous_work_before_oracle_setup() {
    let duplicate = BlockStateRegion::new(vec![
        BlockStateProbe {
            pos: TARGET,
            candidates: vec![AIR.to_owned()],
        },
        BlockStateProbe {
            pos: TARGET,
            candidates: vec![STONE.to_owned()],
        },
    ]);
    assert!(matches!(
        duplicate,
        Err(FixedReplayError::DuplicateProbe { pos: TARGET })
    ));

    let out_of_order = FixedActionReplay::new(
        7,
        Script::new(vec![
            ScriptStep {
                tick: 2,
                action: Action::SetBlock {
                    pos: TARGET,
                    state: AIR.to_owned(),
                },
            },
            ScriptStep {
                tick: 1,
                action: Action::SetBlock {
                    pos: TARGET,
                    state: STONE.to_owned(),
                },
            },
        ]),
        region(),
        0,
    );
    assert!(matches!(
        out_of_order,
        Err(FixedReplayError::StepsOutOfOrder { step: 1, .. })
    ));

    let beyond_horizon = FixedActionReplay::new(
        8,
        Script::new(vec![ScriptStep {
            tick: MAX_FIXED_REPLAY_TICKS,
            action: Action::SetBlock {
                pos: TARGET,
                state: STONE.to_owned(),
            },
        }]),
        region(),
        0,
    );
    assert!(matches!(
        beyond_horizon,
        Err(FixedReplayError::TickHorizonTooLong { .. })
    ));
}
