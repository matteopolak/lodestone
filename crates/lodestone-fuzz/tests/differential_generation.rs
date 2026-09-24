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
    retry_oracle_timeouts, search_and_shrink, search_and_shrink_with,
};
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, OracleFailure, OracleFailureKind, Script, ScriptStep, Side,
    WorldOracle, run_differential,
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

#[test]
fn empty_search_work_is_rejected_before_evaluation() {
    let valid_region = region();
    for (cases, probes, expected) in [
        (0, valid_region.clone(), "a differential search needs at least one case"),
        (1, vec![], "a differential comparison needs at least one probe"),
        (
            1,
            vec![(TARGET, vec![])],
            "differential probe 0 needs at least one candidate state",
        ),
    ] {
        let mut evaluations = 0;
        let outcome = search_and_shrink_with(
            &domain(),
            SearchBudget { cases, ..search_budget() },
            &probes,
            0,
            |_, _, _| {
                evaluations += 1;
                DifferentialOutcome::Agreed
            },
        );
        match outcome {
            SearchOutcome::InvalidConfiguration { message } => assert_eq!(message, expected),
            other => panic!("empty search work must not report agreement: {other:?}"),
        }
        assert_eq!(evaluations, 0, "invalid coverage must be rejected before oracle setup");
    }

    let mut evaluations = 0;
    let outcome = search_and_shrink_with(
        &domain(),
        SearchBudget { cases: 1, ..search_budget() },
        &valid_region,
        0,
        |_, _, _| {
            evaluations += 1;
            DifferentialOutcome::Agreed
        },
    );
    assert!(matches!(outcome, SearchOutcome::NoDivergence { cases_run: 1 }));
    assert_eq!(evaluations, 1, "the valid control must execute its evaluator");
}

#[test]
fn empty_replay_probes_are_rejected_before_evaluation() {
    for (probes, expected) in [
        (serde_json::json!([]), "a differential comparison needs at least one probe"),
        (
            serde_json::json!([{ "pos": [0, 0, 0], "candidates": [] }]),
            "differential probe 0 needs at least one candidate state",
        ),
    ] {
        let replay = ReplayCase::from_json(&serde_json::json!({
            "format_version": 1,
            "scenario": "empty-probe-control",
            "seed": 1,
            "case_index": 0,
            "settle_ticks": 0,
            "region": probes,
            "steps": [{ "tick": 0, "action": {
                "kind": "set_block", "pos": [0, 0, 0], "state": STONE
            }}],
            "divergence": { "tick": 0, "pos": [0, 0, 0], "left": AIR, "right": STONE }
        }).to_string()).expect("valid replay format");
        let mut evaluations = 0;
        let error = replay.replay_with(|_, _, _| {
            evaluations += 1;
            DifferentialOutcome::Agreed
        }).expect_err("empty replay coverage must fail before oracle setup");
        assert_eq!(error, expected);
        assert_eq!(evaluations, 0);
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
fn the_fault_control_shrinks_to_a_replayable_trigger_at_the_original_tick() {
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
    assert!(
        found.minimal_script.steps.iter().any(|step| {
            step.tick == found.original_divergence.tick
                && step.action
                    == (Action::SetBlock {
                        pos: TARGET,
                        state: WATER.to_owned(),
                    })
        }),
        "the minimized script must retain the triggering edit"
    );
    assert_eq!(found.original_divergence.tick, found.minimal_divergence.tick);
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

    let replayed_through_owned_setup = decoded
        .replay_with(|script, region, settle_ticks| {
            let mut left = FakeWorld::default();
            let mut right = FakeWorld {
                fault: Some(Fault::WaterAtTargetBecomesStone),
                ..FakeWorld::default()
            };
            run_differential(script, region, &mut left, &mut right, settle_ticks)
        })
        .expect("the minimized live-shaped replay has a bounded tick horizon");
    let DifferentialOutcome::Diverged(replayed_divergence) = replayed_through_owned_setup else {
        panic!("the caller-owned evaluator must reproduce the replay: {replayed_through_owned_setup:?}");
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
fn shrinking_preserves_the_first_divergence_tick_as_part_of_the_failure_class() {
    let domain = GenerationDomain::new(
        vec![OTHER_TARGET, TARGET],
        vec![WATER.to_owned()],
        8,
        5,
    )
    .expect("valid domain");
    let (seed, original_tick) = (0..10_000)
        .find_map(|seed| {
            let scripts = sample_scripts(
                &domain,
                SearchBudget {
                    seed,
                    cases: 1,
                    shrink_attempts: 0,
                },
            )
            .expect("sample one script");
            let script = &scripts[0];
            let first_target = script.steps.iter().find(|step| {
                matches!(step.action, Action::SetBlock { pos: TARGET, .. })
            })?;
            (first_target.tick > 0).then_some((seed, first_target.tick))
        })
        .expect("the finite seed search must find a delayed target edit");

    let outcome = search_and_shrink(
        &domain,
        SearchBudget {
            seed,
            cases: 1,
            shrink_attempts: 256,
        },
        &region(),
        0,
        || {
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
        panic!("the delayed fault must be found, got {outcome:?}");
    };
    assert_eq!(found.original_divergence.tick, original_tick);
    assert_eq!(
        found.minimal_divergence.tick, found.original_divergence.tick,
        "a minimized replay must preserve the first-divergence tick, not only its states"
    );
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
fn live_replay_policy_rejects_untrusted_fields_before_oracle_setup() {
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
    let expected_scenario = "live-fluid-policy-control";
    let expected_region = region();
    let replay = ReplayCase::from_found(
        expected_scenario,
        search_budget().seed,
        0,
        expected_region.clone(),
        &found,
    );
    let original: serde_json::Value = serde_json::from_str(
        &replay.to_json_pretty().expect("serialize replay"),
    )
    .expect("parse replay JSON");

    let mut mutations = Vec::new();
    let mut wrong_scenario = original.clone();
    wrong_scenario["scenario"] = serde_json::json!("another-scenario");
    mutations.push((wrong_scenario, "scenario"));

    let mut wrong_region = original.clone();
    wrong_region["region"][0]["pos"] = serde_json::json!([99, 0, 0]);
    mutations.push((wrong_region, "probe region"));

    let mut wrong_settle = original.clone();
    wrong_settle["settle_ticks"] = serde_json::json!(1);
    mutations.push((wrong_settle, "settle ticks"));

    let mut command = original.clone();
    command["steps"][0]["action"] = serde_json::json!({
        "kind": "run_command",
        "command": "fill 0 0 0 100 100 100 minecraft:lava"
    });
    mutations.push((command, "RunCommand"));

    let mut out_of_lane = original.clone();
    out_of_lane["steps"][0]["action"]["pos"] = serde_json::json!([99, 0, 0]);
    mutations.push((out_of_lane, "position"));

    let mut wrong_state = original.clone();
    wrong_state["steps"][0]["action"]["state"] = serde_json::json!("minecraft:lava");
    mutations.push((wrong_state, "state"));

    let mut wrong_divergence = original;
    wrong_divergence["divergence"]["pos"] = serde_json::json!([99, 0, 0]);
    mutations.push((wrong_divergence, "divergence position"));

    let mut oracle_setups = 0;
    for (json, expected_error_fragment) in mutations {
        let decoded = ReplayCase::from_json(&serde_json::to_string(&json).expect("serialize mutation"))
            .expect("the malicious input still uses replay format v1");
        let error = decoded
            .replay_generated_with(
                expected_scenario,
                &domain(),
                &expected_region,
                0,
                |_script, _region, _settle_ticks| {
                    oracle_setups += 1;
                    DifferentialOutcome::Agreed
                },
            )
            .expect_err("untrusted live replay content must be rejected");
        assert!(
            error.contains(expected_error_fragment),
            "unexpected policy error {error:?} for {expected_error_fragment}"
        );
    }
    assert_eq!(oracle_setups, 0, "rejected replay input must not reach RCON setup");

    let accepted = replay
        .replay_generated_with(
            expected_scenario,
            &domain(),
            &expected_region,
            0,
            |_script, _region, _settle_ticks| {
                oracle_setups += 1;
                DifferentialOutcome::Agreed
            },
        )
        .expect("the matching generated replay policy must admit its own artifact");
    assert!(matches!(accepted, DifferentialOutcome::Agreed));
    assert_eq!(
        oracle_setups, 1,
        "the valid control must prove the evaluator is reachable"
    );
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
    let replay = ReplayCase::from_found("horizon-cap-control", search_budget().seed, 0, region(), &found);
    let mut json: serde_json::Value =
        serde_json::from_str(&replay.to_json_pretty().expect("serialize replay")).expect("parse JSON value");
    json["settle_ticks"] = serde_json::json!(MAX_ORACLE_TICKS - found.minimal_script.last_tick());
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
fn evaluator_setup_failure_aborts_before_it_can_become_a_counterexample() {
    let mut evaluations = 0;
    let outcome = search_and_shrink_with(
        &domain(),
        search_budget(),
        &region(),
        0,
        |_script, _region, _settle_ticks| {
            evaluations += 1;
            DifferentialOutcome::OracleFailed(OracleFailure {
                tick: 0,
                side: Side::Right,
                kind: OracleFailureKind::Failure,
                message: "candidate reset failed".to_owned(),
            })
        },
    );

    let SearchOutcome::OracleFailed {
        case_index,
        during_shrink,
        failure,
    } = outcome
    else {
        panic!("setup failure must stop the generated search, got {outcome:?}");
    };
    assert_eq!(evaluations, 1);
    assert_eq!(case_index, 0);
    assert!(!during_shrink);
    assert_eq!(failure.kind, OracleFailureKind::Failure);
    assert_eq!(failure.message, "candidate reset failed");
}

#[test]
fn only_timing_failures_are_retried_and_each_attempt_is_bounded() {
    let divergence = lodestone_fuzz::differential::Divergence {
        tick: 3,
        pos: TARGET,
        left: Some(AIR.to_owned()),
        right: Some(STONE.to_owned()),
    };
    let mut attempts = 0;
    let recovered = retry_oracle_timeouts(3, || {
        attempts += 1;
        if attempts == 1 {
            DifferentialOutcome::OracleFailed(OracleFailure {
                tick: 3,
                side: Side::Right,
                kind: OracleFailureKind::Timeout,
                message: "missed a tick boundary".to_owned(),
            })
        } else {
            DifferentialOutcome::Diverged(divergence.clone())
        }
    });
    assert_eq!(attempts, 2);
    assert!(matches!(recovered, DifferentialOutcome::Diverged(found) if found == divergence));

    let mut failures = 0;
    let not_retried = retry_oracle_timeouts(3, || {
        failures += 1;
        DifferentialOutcome::OracleFailed(OracleFailure {
            tick: 0,
            side: Side::Right,
            kind: OracleFailureKind::Failure,
            message: "authentication failed".to_owned(),
        })
    });
    assert_eq!(failures, 1);
    assert!(matches!(
        not_retried,
        DifferentialOutcome::OracleFailed(OracleFailure {
            kind: OracleFailureKind::Failure,
            ..
        })
    ));

    let mut timeouts = 0;
    let exhausted = retry_oracle_timeouts(3, || {
        timeouts += 1;
        DifferentialOutcome::OracleFailed(OracleFailure {
            tick: 1,
            side: Side::Right,
            kind: OracleFailureKind::Timeout,
            message: "still overloaded".to_owned(),
        })
    });
    assert_eq!(timeouts, 3);
    assert!(matches!(
        exhausted,
        DifferentialOutcome::OracleFailed(OracleFailure {
            kind: OracleFailureKind::Timeout,
            ..
        })
    ));
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
            assert_eq!(failure.kind, OracleFailureKind::Failure);
            assert_eq!(failure.message, "instrument disconnected");
        }
        other => panic!("an instrument failure must abort the search, got {other:?}"),
    }
}
