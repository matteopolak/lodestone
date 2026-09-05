//! Bounded, fixed-seed model checking for the server composter state machine.
//!
//! The production consumer combines item eligibility, the special first-fill
//! rule, chance-based level advances, a delayed level-7 transition, and ready
//! extraction. This test drives those public operations with a finite,
//! shrinkable action script and compares every observable state field with an
//! independent model. The fixed prefix includes a failed roll, the exact
//! twenty-tick delay, and a ready-to-empty cycle.
//!
//! The detector control intentionally leaves a ready composter full after an
//! extraction. Its fixed prefix must diverge, and the fixed-seed shrink run
//! proves that a comparison which accepts every trace cannot pass.

use std::cell::Cell;

use lodestone_server::{Composter, ComposterInsertOutcome};
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 160;
const SEED: u64 = 0x43_4f_4d_50_4f_53_54;
const MAX_SHRINK_ITERS: u32 = 256;

// These values are the model's own expected-value table. They are deliberately
// not read from `lodestone_server::compostable_chance` or its exported timing
// constants: agreement must come from the observed production state, not two
// calls into the same implementation.
const MODEL_MAX_INSERT_LEVEL: u8 = 7;
const MODEL_READY_LEVEL: u8 = 8;
const MODEL_READY_DELAY: u8 = 20;
const ROLL_SCALE: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Leaves,
    SugarCane,
    Wheat,
    Hay,
    Cake,
    Junk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposterOp {
    Insert { item: Item, roll: u32 },
    Tick,
    Extract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scenario {
    tail: Vec<ComposterOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefInsertOutcome {
    NotCompostable,
    NotAccepting,
    Consumed { level_increased: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RefState {
    level: u8,
    ticks_until_ready: Option<u8>,
}

impl Default for RefState {
    fn default() -> Self {
        Self {
            level: 0,
            ticks_until_ready: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefObservation {
    insert: Option<RefInsertOutcome>,
    tick_ready: Option<bool>,
    extracted: Option<bool>,
    level: u8,
    ticks_until_ready: Option<u8>,
    ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    insert: Option<RefInsertOutcome>,
    tick_ready: Option<bool>,
    extracted: Option<bool>,
    level: u8,
    ticks_until_ready: Option<u8>,
    ready: bool,
}

fn item_name(item: Item) -> &'static str {
    match item {
        Item::Leaves => "minecraft:jungle_leaves",
        Item::SugarCane => "minecraft:sugar_cane",
        Item::Wheat => "minecraft:wheat",
        Item::Hay => "minecraft:hay_block",
        Item::Cake => "minecraft:cake",
        Item::Junk => "minecraft:stone",
    }
}

/// The model's independently stated compost table, in millionths of a unit.
/// The selected entries cover four nontrivial thresholds plus the guaranteed
/// item, while `Junk` exercises the rejected-item branch.
fn model_chance(item: Item) -> Option<u32> {
    match item {
        Item::Leaves => Some(300_000),
        Item::SugarCane => Some(500_000),
        Item::Wheat => Some(650_000),
        Item::Hay => Some(850_000),
        Item::Cake => Some(1_000_000),
        Item::Junk => None,
    }
}

impl RefState {
    fn insert(&mut self, item: Item, roll: u32) -> RefInsertOutcome {
        if self.level >= MODEL_MAX_INSERT_LEVEL {
            return RefInsertOutcome::NotAccepting;
        }
        let Some(chance) = model_chance(item) else {
            return RefInsertOutcome::NotCompostable;
        };

        // The first accepted item advances without consulting its chance.
        let level_increased = self.level == 0 || roll < chance;
        if level_increased {
            self.level += 1;
            if self.level == MODEL_MAX_INSERT_LEVEL {
                self.ticks_until_ready = Some(MODEL_READY_DELAY);
            }
        }
        RefInsertOutcome::Consumed { level_increased }
    }

    fn tick(&mut self) -> bool {
        match self.ticks_until_ready {
            Some(1) => {
                self.level = MODEL_READY_LEVEL;
                self.ticks_until_ready = None;
                true
            }
            Some(n) => {
                self.ticks_until_ready = Some(n - 1);
                false
            }
            None => false,
        }
    }

    fn extract(&mut self) -> bool {
        if self.level == MODEL_READY_LEVEL {
            self.level = 0;
            true
        } else {
            false
        }
    }
}

fn model_observation(state: RefState, insert: Option<RefInsertOutcome>, tick_ready: Option<bool>, extracted: Option<bool>) -> RefObservation {
    RefObservation {
        insert,
        tick_ready,
        extracted,
        level: state.level,
        ticks_until_ready: state.ticks_until_ready,
        ready: state.level == MODEL_READY_LEVEL,
    }
}

fn production_observation(composter: &Composter, insert: Option<RefInsertOutcome>, tick_ready: Option<bool>, extracted: Option<bool>) -> Observation {
    Observation {
        insert,
        tick_ready,
        extracted,
        level: composter.level(),
        ticks_until_ready: composter.ticks_until_ready(),
        ready: composter.is_ready(),
    }
}

fn production_insert_outcome(outcome: ComposterInsertOutcome) -> RefInsertOutcome {
    match outcome {
        ComposterInsertOutcome::NotCompostable => RefInsertOutcome::NotCompostable,
        ComposterInsertOutcome::NotAccepting => RefInsertOutcome::NotAccepting,
        ComposterInsertOutcome::Consumed { level_increased } => {
            RefInsertOutcome::Consumed { level_increased }
        }
    }
}

fn roll_value(roll: u32) -> f64 {
    f64::from(roll) / f64::from(ROLL_SCALE)
}

fn prefix() -> Vec<ComposterOp> {
    let mut ops = vec![
        // At level zero, even a high roll advances the first compostable item.
        ComposterOp::Insert {
            item: Item::Leaves,
            roll: 999_999,
        },
        // Leaves have a 0.3 chance; this item is consumed but does not advance.
        ComposterOp::Insert {
            item: Item::Leaves,
            roll: 900_000,
        },
    ];
    // Six guaranteed advances reach level seven and arm the delayed tick.
    ops.extend(std::iter::repeat_n(
        ComposterOp::Insert {
            item: Item::Cake,
            roll: 0,
        },
        6,
    ));
    // The full-but-not-ready state rejects inserts and keeps its countdown.
    ops.push(ComposterOp::Insert {
        item: Item::Cake,
        roll: 0,
    });
    ops.extend(std::iter::repeat_n(ComposterOp::Tick, 20));
    // Ready extraction returns one item and resets the state for another cycle.
    ops.push(ComposterOp::Extract);
    // The rejected-item branch is now visible at level zero.
    ops.push(ComposterOp::Insert {
        item: Item::Junk,
        roll: 0,
    });
    ops
}

fn roll_strategy() -> impl Strategy<Value = u32> {
    (0..ROLL_SCALE).prop_filter("avoid float threshold boundaries", |roll| {
        !matches!(roll, 300_000 | 500_000 | 650_000 | 850_000)
    })
}

fn operation_strategy() -> impl Strategy<Value = ComposterOp> {
    prop_oneof![
        (prop::sample::select(&[
            Item::Leaves,
            Item::SugarCane,
            Item::Wheat,
            Item::Hay,
            Item::Cake,
            Item::Junk,
        ]), roll_strategy())
            .prop_map(|(item, roll)| ComposterOp::Insert { item, roll }),
        Just(ComposterOp::Tick),
        Just(ComposterOp::Extract),
    ]
}

fn scenario_strategy() -> impl Strategy<Value = Scenario> {
    collection::vec(operation_strategy(), 1..=24).prop_map(|tail| Scenario { tail })
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        max_shrink_iters: MAX_SHRINK_ITERS,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    })
}

fn production_trace(scenario: &Scenario) -> Vec<Observation> {
    let mut composter = Composter::new();
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.tail.iter().copied());
    for op in ops {
        let (insert, tick_ready, extracted) = match op {
            ComposterOp::Insert { item, roll } => (
                Some(production_insert_outcome(composter.insert(item_name(item), roll_value(roll)))),
                None,
                None,
            ),
            ComposterOp::Tick => (None, Some(composter.tick()), None),
            ComposterOp::Extract => (None, None, Some(composter.extract())),
        };
        trace.push(production_observation(&composter, insert, tick_ready, extracted));
    }
    trace
}

fn model_trace(scenario: &Scenario, reset_on_extract: bool) -> Vec<RefObservation> {
    let mut state = RefState::default();
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.tail.iter().copied());
    for op in ops {
        let (insert, tick_ready, extracted) = match op {
            ComposterOp::Insert { item, roll } => (Some(state.insert(item, roll)), None, None),
            ComposterOp::Tick => (None, Some(state.tick()), None),
            ComposterOp::Extract => {
                let extracted = state.level == MODEL_READY_LEVEL;
                if reset_on_extract {
                    state.extract();
                }
                (None, None, Some(extracted))
            }
        };
        trace.push(model_observation(state, insert, tick_ready, extracted));
    }
    trace
}

fn mismatch_index(actual: &[Observation], expected: &[RefObservation]) -> Option<usize> {
    if actual.len() != expected.len() {
        return Some(actual.len().min(expected.len()));
    }
    actual.iter().zip(expected).position(|(actual, expected)| {
        actual.insert != expected.insert
            || actual.tick_ready != expected.tick_ready
            || actual.extracted != expected.extracted
            || actual.level != expected.level
            || actual.ticks_until_ready != expected.ticks_until_ready
            || actual.ready != expected.ready
    })
}

#[test]
fn generated_composter_traces_match_the_independent_model() {
    runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            let actual = production_trace(&scenario);
            let expected = model_trace(&scenario, true);
            let mismatch = mismatch_index(&actual, &expected);
            prop_assert!(
                mismatch.is_none(),
                "composter trace mismatch at {mismatch:?}: actual={:?} expected={:?}",
                mismatch.and_then(|index| actual.get(index)),
                mismatch.and_then(|index| expected.get(index)),
            );
            Ok(())
        })
        .expect("the production composter state machine must match the independent model");
}

#[test]
fn detector_control_rejects_a_model_that_does_not_empty_ready_composters() {
    let evaluations = Cell::new(0usize);
    let failure = runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            evaluations.set(evaluations.get() + 1);
            let actual = production_trace(&scenario);
            let intentionally_wrong = model_trace(&scenario, false);
            prop_assert!(
                mismatch_index(&actual, &intentionally_wrong).is_none(),
                "detector control that leaves extracted composters full must disagree"
            );
            Ok(())
        })
        .expect_err("a model that ignores ready extraction must be rejected");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(minimal.tail.len() <= 24, "the shrunk control must remain bounded");
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed control must evaluate shrink candidates after detecting a mismatch"
    );
}
