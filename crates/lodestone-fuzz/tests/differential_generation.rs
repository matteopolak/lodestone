//! Hermetic controls for generated differential scripts and their shrinker.
//!
//! These tests use tiny in-memory worlds so generation and shrinking have no
//! network, server, container, or wall-clock dependency. The deliberately
//! faulty world is a detector control, not an oracle for gameplay behaviour.

#[path = "support/differential_generation.rs"]
mod differential_generation;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::rc::Rc;

use differential_generation::{
    GenerationDomain, MAX_ORACLE_TICKS, ReplayCase, SearchBudget, SearchOutcome, sample_scripts,
    search_and_shrink,
};
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};

const AIR: &str = "minecraft:air";
const STONE: &str = "minecraft:stone";
const WATER: &str = "minecraft:water[level=0]";
const TARGET: (i32, i32, i32) = (0, 0, 0);
const OTHER_TARGET: (i32, i32, i32) = (1, 0, 0);

fn domain() -> GenerationDomain {
    GenerationDomain::new(
        vec![TARGET, (1, 0, 0), (2, 0, 0)],
        vec![AIR.to_owned(), STONE.to_owned(), WATER.to_owned()],
        8,
        5,
    )
    .expect("the test domain is valid")
}

fn search_budget() -> SearchBudget {
    SearchBudget {
        seed: 0x5e_ed_54_9,
        cases: 64,
        shrink_attempts: 256,
    }
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    vec![(TARGET, vec![AIR.to_owned(), STONE.to_owned(), WATER.to_owned()])]
}

fn assert_script_eq(actual: &Script, expected: &Script) {
    assert_eq!(actual.steps.len(), expected.steps.len());
    for (actual, expected) in actual.steps.iter().zip(&expected.steps) {
        assert_eq!(actual.tick, expected.tick);
        assert_eq!(actual.action, expected.action);
    }
}

#[derive(Default)]
struct FakeWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    fault: Option<Fault>,
}

#[derive(Clone, Copy)]
enum Fault {
    WaterAtTargetBecomesStone,
}

impl WorldOracle for FakeWorld {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        match action {
            Action::SetBlock { pos, state } => {
                self.blocks.insert(*pos, state.clone());
            }
            Action::RunCommand(_) => unreachable!("generated scripts never contain raw commands"),
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        if matches!(self.fault, Some(Fault::WaterAtTargetBecomesStone))
            && self.blocks.get(&TARGET).is_some_and(|state| state == WATER)
        {
            self.blocks.insert(TARGET, STONE.to_owned());
        }
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self.blocks.get(&pos).map_or(AIR, String::as_str);
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

struct FailingWorld;

impl WorldOracle for FailingWorld {
    type Error = &'static str;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Err("instrument disconnected")
    }

    fn block_state(
        &mut self,
        _pos: (i32, i32, i32),
        _candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }
}

struct DivergenceClassWorld {
    applied: usize,
    faulty: bool,
    observed_classes: Rc<RefCell<Vec<(i32, i32, i32)>>>,
}

impl WorldOracle for DivergenceClassWorld {
    type Error = Infallible;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        self.applied += 1;
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let divergent_pos = if self.applied >= 2 { TARGET } else { OTHER_TARGET };
        let actual = if self.faulty && pos == divergent_pos {
            self.observed_classes.borrow_mut().push(pos);
            STONE
        } else {
            AIR
        };
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

struct SingleEvaluationWorld {
    generation_id: u32,
    finished: bool,
    faulty: bool,
    observed_generations: Rc<RefCell<Vec<u32>>>,
}

impl WorldOracle for SingleEvaluationWorld {
    type Error = &'static str;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        if self.finished {
            return Err("oracle instance was reused for another candidate");
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        if self.finished {
            return Err("oracle instance was reused for another candidate");
        }
        self.finished = true;
        if self.faulty {
            self.observed_generations.borrow_mut().push(self.generation_id);
        }
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = if self.faulty && pos == TARGET { STONE } else { AIR };
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

#[test]
fn a_fixed_seed_produces_the_same_scripts_without_wall_clock_input() {
    let first = sample_scripts(&domain(), search_budget()).expect("sample scripts");
    let second = sample_scripts(&domain(), search_budget()).expect("sample scripts again");
    assert_eq!(first.len(), second.len());
    for (first, second) in first.iter().zip(&second) {
        assert_script_eq(first, second);
    }

    let different = sample_scripts(
        &domain(),
        SearchBudget {
            seed: search_budget().seed + 1,
            ..search_budget()
        },
    )
    .expect("sample scripts with a different seed");
    assert!(
        first.iter().zip(&different).any(|(first, different)| {
            first.steps.len() != different.steps.len()
                || first.steps.iter().zip(&different.steps).any(|(first, different)| {
                    first.tick != different.tick || first.action != different.action
                })
        }),
        "the control seed must exercise a different stream"
    );
}

#[test]
fn generated_scripts_are_bounded_ordered_and_set_block_only() {
    let domain = domain();
    let scripts = sample_scripts(&domain, search_budget()).expect("sample scripts");

    for script in scripts {
        assert!(!script.steps.is_empty());
        assert!(script.steps.len() <= domain.max_steps());
        assert_eq!(script.steps[0].tick, 0);
        assert!(script.steps.windows(2).all(|pair| pair[0].tick <= pair[1].tick));
        assert!(
            script
                .steps
                .windows(2)
                .all(|pair| pair[1].tick - pair[0].tick <= domain.max_tick_gap())
        );
        for step in script.steps {
            match step.action {
                Action::SetBlock { pos, state } => {
                    assert!(domain.positions().contains(&pos));
                    assert!(domain.states().contains(&state));
                }
                Action::RunCommand(command) => {
                    panic!("generated a raw command instead of SetBlock: {command:?}")
                }
            }
        }
    }
}

#[test]
fn the_fault_control_shrinks_to_one_replayable_trigger_at_tick_zero() {
    let mut factory_calls = 0_u32;
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        factory_calls += 1;
        (
            FakeWorld::default(),
            FakeWorld {
                fault: Some(Fault::WaterAtTargetBecomesStone),
                ..FakeWorld::default()
            },
        )
    });

    let SearchOutcome::Found(found) = outcome else {
        panic!("the deliberately faulty world must be found, got {outcome:?}");
    };
    assert!(factory_calls > 1, "each search and shrink candidate needs fresh worlds");
    assert!(found.case_index < search_budget().cases);
    assert!(!found.original_script.steps.is_empty());
    assert!(found.shrink_attempts > 0);
    assert!(found.shrink_attempts <= search_budget().shrink_attempts);
    assert_script_eq(
        &found.minimal_script,
        &Script::new(vec![lodestone_fuzz::differential::ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: TARGET,
                state: WATER.to_owned(),
            },
        }]),
    );
    assert_eq!(found.original_divergence.pos, found.minimal_divergence.pos);
    assert_eq!(found.original_divergence.left, found.minimal_divergence.left);
    assert_eq!(found.original_divergence.right, found.minimal_divergence.right);

    let replay = ReplayCase::from_found(
        "fault-control-v1",
        search_budget().seed,
        0,
        region(),
        &found,
    );
    let json = replay.to_json_pretty().expect("serialize replay");
    let decoded = ReplayCase::from_json(&json).expect("deserialize replay");
    assert_eq!(decoded, replay);
    assert_script_eq(&decoded.script(), &found.minimal_script);

    let replayed = decoded
        .replay(|| {
            (
                FakeWorld::default(),
                FakeWorld {
                    fault: Some(Fault::WaterAtTargetBecomesStone),
                    ..FakeWorld::default()
                },
            )
        })
        .expect("the minimized replay has a bounded tick horizon");
    let DifferentialOutcome::Diverged(replayed_divergence) = replayed else {
        panic!("the decoded replay must reproduce its recorded divergence: {replayed:?}");
    };
    assert_eq!(replayed_divergence, decoded.expected_divergence());
}

#[test]
fn shrink_candidates_with_a_different_divergence_class_are_rejected() {
    let domain = GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 8, 0).expect("valid domain");
    let seed = 0;
    let original = sample_scripts(
        &domain,
        SearchBudget {
            seed,
            cases: 1,
            shrink_attempts: 0,
        },
    )
    .expect("sample one script");
    assert!(original[0].steps.len() >= 2, "the control seed must begin in the first class");
    let observed_classes = Rc::new(RefCell::new(Vec::new()));
    let outcome = search_and_shrink(
        &domain,
        SearchBudget {
            seed,
            cases: 1,
            shrink_attempts: 256,
        },
        &[
            (TARGET, vec![AIR.to_owned(), STONE.to_owned()]),
            (OTHER_TARGET, vec![AIR.to_owned(), STONE.to_owned()]),
        ],
        0,
        || {
            (
                DivergenceClassWorld {
                    applied: 0,
                    faulty: false,
                    observed_classes: Rc::clone(&observed_classes),
                },
                DivergenceClassWorld {
                    applied: 0,
                    faulty: true,
                    observed_classes: Rc::clone(&observed_classes),
                },
            )
        },
    );

    let SearchOutcome::Found(found) = outcome else {
        panic!("the two-class detector must find a divergence: {outcome:?}");
    };
    assert!(
        observed_classes.borrow().contains(&OTHER_TARGET),
        "the shrinker must actually evaluate a candidate in the other divergence class"
    );
    assert_eq!(found.original_divergence.pos, TARGET);
    assert_eq!(found.minimal_divergence.pos, TARGET);
    assert_eq!(found.original_divergence.left, found.minimal_divergence.left);
    assert_eq!(found.original_divergence.right, found.minimal_divergence.right);
    assert!(found.minimal_script.steps.len() >= 2);
}

#[test]
fn zero_and_one_attempt_budgets_are_exact_hard_cutoffs() {
    for expected_attempts in [0, 1] {
        let mut factory_calls = 0_u32;
        let outcome = search_and_shrink(
            &domain(),
            SearchBudget {
                shrink_attempts: expected_attempts,
                ..search_budget()
            },
            &region(),
            0,
            || {
                factory_calls += 1;
                (
                    FakeWorld::default(),
                    FakeWorld {
                        fault: Some(Fault::WaterAtTargetBecomesStone),
                        ..FakeWorld::default()
                    },
                )
            },
        );

        let SearchOutcome::Found(found) = outcome else {
            panic!("the fault control must be found with budget {expected_attempts}: {outcome:?}");
        };
        assert_eq!(found.shrink_attempts, expected_attempts);
        assert_eq!(factory_calls, found.case_index + 1 + expected_attempts);
        if expected_attempts == 0 {
            assert_script_eq(&found.minimal_script, &found.original_script);
        }
    }
}

#[test]
fn the_exact_oracle_tick_horizon_cap_is_accepted() {
    let domain = GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 1, 0).expect("valid domain");
    let mut factory_calls = 0;
    let outcome = search_and_shrink(
        &domain,
        SearchBudget {
            seed: 0,
            cases: 1,
            shrink_attempts: 0,
        },
        &[(TARGET, vec![AIR.to_owned(), STONE.to_owned()])],
        MAX_ORACLE_TICKS - 1,
        || {
            factory_calls += 1;
            (
                SingleEvaluationWorld {
                    generation_id: factory_calls,
                    finished: false,
                    faulty: false,
                    observed_generations: Rc::new(RefCell::new(Vec::new())),
                },
                SingleEvaluationWorld {
                    generation_id: factory_calls,
                    finished: false,
                    faulty: true,
                    observed_generations: Rc::new(RefCell::new(Vec::new())),
                },
            )
        },
    );

    assert!(matches!(outcome, SearchOutcome::Found(_)), "the exact cap is valid: {outcome:?}");
    assert_eq!(factory_calls, 1);
}

#[test]
fn a_horizon_above_the_cap_is_rejected_before_oracle_creation() {
    let domain = GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 1, 0).expect("valid domain");
    let mut factory_calls = 0;
    let outcome = search_and_shrink(
        &domain,
        SearchBudget {
            seed: 0,
            cases: 1,
            shrink_attempts: 0,
        },
        &region(),
        MAX_ORACLE_TICKS,
        || {
            factory_calls += 1;
            (FakeWorld::default(), FakeWorld::default())
        },
    );

    let SearchOutcome::InvalidConfiguration { message } = outcome else {
        panic!("an over-cap horizon must be rejected explicitly: {outcome:?}");
    };
    assert_eq!(factory_calls, 0);
    assert!(message.contains("exceeds the 4096-tick cap"), "unexpected error: {message}");
}

#[test]
fn overflowing_gap_or_settle_horizons_are_rejected_before_oracle_creation() {
    let cases = [
        (
            GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 2, u64::MAX)
                .expect("structurally valid domain"),
            0,
        ),
        (
            GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 1, 0)
                .expect("structurally valid domain"),
            u64::MAX,
        ),
    ];

    for (domain, settle_ticks) in cases {
        let mut factory_calls = 0;
        let outcome = search_and_shrink(
            &domain,
            SearchBudget {
                seed: 0,
                cases: 1,
                shrink_attempts: 0,
            },
            &region(),
            settle_ticks,
            || {
                factory_calls += 1;
                (FakeWorld::default(), FakeWorld::default())
            },
        );

        let SearchOutcome::InvalidConfiguration { message } = outcome else {
            panic!("an overflowing horizon must be rejected explicitly: {outcome:?}");
        };
        assert_eq!(factory_calls, 0);
        assert!(message.contains("overflows u64"), "unexpected error: {message}");
    }
}

#[test]
fn every_candidate_evaluation_receives_new_generation_tagged_oracles() {
    let domain = GenerationDomain::new(vec![TARGET], vec![AIR.to_owned()], 8, 0).expect("valid domain");
    let reuse_observations = Rc::new(RefCell::new(Vec::new()));
    let mut reused_left = SingleEvaluationWorld {
        generation_id: 99,
        finished: false,
        faulty: false,
        observed_generations: Rc::clone(&reuse_observations),
    };
    let mut reused_right = SingleEvaluationWorld {
        generation_id: 99,
        finished: false,
        faulty: true,
        observed_generations: Rc::clone(&reuse_observations),
    };
    let one_tick_script = Script::new(vec![ScriptStep {
        tick: 0,
        action: Action::SetBlock {
            pos: TARGET,
            state: AIR.to_owned(),
        },
    }]);
    let one_probe = [(TARGET, vec![AIR.to_owned(), STONE.to_owned()])];
    assert!(matches!(
        run_differential(&one_tick_script, &one_probe, &mut reused_left, &mut reused_right, 0),
        DifferentialOutcome::Diverged(_)
    ));
    let reused = run_differential(
        &one_tick_script,
        &one_probe,
        &mut reused_left,
        &mut reused_right,
        0,
    );
    let DifferentialOutcome::OracleFailed(failure) = reused else {
        panic!("the freshness instrument must reject reuse: {reused:?}");
    };
    assert_eq!(failure.message, "oracle instance was reused for another candidate");

    let observed_generations = Rc::new(RefCell::new(Vec::new()));
    let mut next_generation = 0_u32;
    let outcome = search_and_shrink(
        &domain,
        SearchBudget {
            seed: 7,
            cases: 1,
            shrink_attempts: 8,
        },
        &[(TARGET, vec![AIR.to_owned(), STONE.to_owned()])],
        0,
        || {
            next_generation += 1;
            (
                SingleEvaluationWorld {
                    generation_id: next_generation,
                    finished: false,
                    faulty: false,
                    observed_generations: Rc::clone(&observed_generations),
                },
                SingleEvaluationWorld {
                    generation_id: next_generation,
                    finished: false,
                    faulty: true,
                    observed_generations: Rc::clone(&observed_generations),
                },
            )
        },
    );

    let SearchOutcome::Found(found) = outcome else {
        panic!("fresh generation-tagged worlds must find the control divergence: {outcome:?}");
    };
    assert!(found.shrink_attempts > 0);
    let observed = observed_generations.borrow();
    assert_eq!(observed.len() as u32, next_generation);
    assert_eq!(observed.iter().copied().collect::<HashSet<_>>().len(), observed.len());
    assert_eq!(observed.as_slice(), (1..=next_generation).collect::<Vec<_>>());
}

#[test]
fn an_unsupported_replay_format_version_is_rejected() {
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        (
            FakeWorld::default(),
            FakeWorld {
                fault: Some(Fault::WaterAtTargetBecomesStone),
                ..FakeWorld::default()
            },
        )
    });
    let SearchOutcome::Found(found) = outcome else {
        panic!("the deliberately faulty world must be found: {outcome:?}");
    };
    let replay = ReplayCase::from_found("version-control", search_budget().seed, 0, region(), &found);
    let mut json: serde_json::Value =
        serde_json::from_str(&replay.to_json_pretty().expect("serialize replay")).expect("parse JSON value");
    json["format_version"] = serde_json::json!(2);

    let error = ReplayCase::from_json(&serde_json::to_string(&json).expect("serialize changed JSON"))
        .expect_err("an unknown format version must not be guessed at");
    assert_eq!(error, "unsupported differential replay format 2, expected 1");
}

#[test]
fn an_overflowing_replay_horizon_is_rejected_before_oracle_creation() {
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        (
            FakeWorld::default(),
            FakeWorld {
                fault: Some(Fault::WaterAtTargetBecomesStone),
                ..FakeWorld::default()
            },
        )
    });
    let SearchOutcome::Found(found) = outcome else {
        panic!("the deliberately faulty world must be found: {outcome:?}");
    };
    let replay = ReplayCase::from_found("horizon-control", search_budget().seed, 0, region(), &found);
    let mut json: serde_json::Value =
        serde_json::from_str(&replay.to_json_pretty().expect("serialize replay")).expect("parse JSON value");
    json["settle_ticks"] = serde_json::json!(u64::MAX);
    let decoded = ReplayCase::from_json(&serde_json::to_string(&json).expect("serialize changed JSON"))
        .expect("the replay format remains valid");
    let mut factory_calls = 0;

    let error = decoded
        .replay(|| {
            factory_calls += 1;
            (FakeWorld::default(), FakeWorld::default())
        })
        .expect_err("an overflowing replay horizon must be rejected explicitly");
    assert_eq!(factory_calls, 0);
    assert_eq!(error, "replay differential tick horizon overflows u64");
}

#[test]
fn a_non_overflowing_replay_above_the_cap_is_rejected_before_oracle_creation() {
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        (
            FakeWorld::default(),
            FakeWorld {
                fault: Some(Fault::WaterAtTargetBecomesStone),
                ..FakeWorld::default()
            },
        )
    });
    let SearchOutcome::Found(found) = outcome else {
        panic!("the deliberately faulty world must be found: {outcome:?}");
    };
    assert_eq!(found.minimal_script.last_tick(), 0, "the control replay must start at tick zero");
    let replay = ReplayCase::from_found("horizon-cap-control", search_budget().seed, 0, region(), &found);
    let mut json: serde_json::Value =
        serde_json::from_str(&replay.to_json_pretty().expect("serialize replay")).expect("parse JSON value");
    json["settle_ticks"] = serde_json::json!(MAX_ORACLE_TICKS);
    let decoded = ReplayCase::from_json(&serde_json::to_string(&json).expect("serialize changed JSON"))
        .expect("the replay format remains valid");
    let mut factory_calls = 0;

    let error = decoded
        .replay(|| {
            factory_calls += 1;
            (FakeWorld::default(), FakeWorld::default())
        })
        .expect_err("a replay above the tick cap must be rejected explicitly");
    assert_eq!(factory_calls, 0);
    assert_eq!(
        error,
        "replay differential horizon of 4097 ticks exceeds the 4096-tick cap"
    );
}

#[test]
fn the_same_generated_stream_agrees_when_the_fault_is_absent() {
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        (FakeWorld::default(), FakeWorld::default())
    });

    assert!(
        matches!(outcome, SearchOutcome::NoDivergence { cases_run: 64 }),
        "two fresh correct fakes must agree for the complete fixed case budget: {outcome:?}"
    );
}

#[test]
fn an_oracle_failure_aborts_instead_of_becoming_a_counterexample() {
    let outcome = search_and_shrink(&domain(), search_budget(), &region(), 0, || {
        (FakeWorld::default(), FailingWorld)
    });

    match outcome {
        SearchOutcome::OracleFailed {
            case_index,
            during_shrink,
            failure,
        } => {
            assert_eq!(case_index, 0);
            assert!(!during_shrink);
            assert_eq!(failure.message, "instrument disconnected");
        }
        other => panic!("an instrument failure must abort the search, got {other:?}"),
    }
}
