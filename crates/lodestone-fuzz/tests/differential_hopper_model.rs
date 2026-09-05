//! Bounded, fixed-seed model checking for the server-side hopper transfer
//! chain.
//!
//! The production consumer is [`lodestone_server::hopper::Hopper::tick`]. It
//! combines cooldown handling, redstone locking, one-item ejection, and
//! one-item suction in a single ready-tick path. This test drives that path
//! with a finite script and compares every observable slot and cooldown value
//! with a small model that owns its own stack representation and transfer
//! rules.
//!
//! The fixed prefix exercises both directions on one ready tick and then the
//! exact eight-tick retry cadence. The detector control deliberately removes
//! the ejection half of the model; its first tick must disagree and the
//! shrinker must still evaluate candidates after finding that mismatch.

use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

use lodestone_model::ItemStack;
use lodestone_server::{Hopper, HopperTick};

const CASES: u32 = 160;
const SEED: u64 = 0x48_4f_50_50_45_52_31;
const MAX_SHRINK_ITERS: u32 = 256;

// These are the transfer contract's external domain facts, kept here rather
// than imported from the implementation under test. The generated stacks all
// fit the ordinary item-stack bound and the fixed cooldown witness exercises
// the exact retry interval.
const MODEL_STACK_CAP: u32 = 64;
const MODEL_TRANSFER_COOLDOWN: i32 = 8;
const SLOT_COUNT: usize = 5;
const NEIGHBOUR_SLOTS: usize = 3;
const ITEM_NAMES: [&str; 3] = ["test:alpha", "test:beta", "test:gamma"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RefStack {
    item: usize,
    count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HopperOp {
    SetOwn {
        slot: usize,
        stack: Option<RefStack>,
    },
    SetAbove {
        slot: usize,
        stack: Option<RefStack>,
    },
    SetBelow {
        slot: usize,
        stack: Option<RefStack>,
    },
    Tick { enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefTick {
    ejected: bool,
    sucked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefObservation {
    tick: Option<RefTick>,
    own: [Option<RefStack>; SLOT_COUNT],
    above: [Option<RefStack>; NEIGHBOUR_SLOTS],
    below: [Option<RefStack>; NEIGHBOUR_SLOTS],
    cooldown: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    tick: Option<HopperTick>,
    own: [Option<RefStack>; SLOT_COUNT],
    above: [Option<RefStack>; NEIGHBOUR_SLOTS],
    below: [Option<RefStack>; NEIGHBOUR_SLOTS],
    cooldown: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scenario {
    tail: Vec<HopperOp>,
}

fn fixed_own() -> [Option<RefStack>; SLOT_COUNT] {
    [
        Some(RefStack { item: 0, count: 2 }),
        None,
        None,
        None,
        None,
    ]
}

fn fixed_above() -> [Option<RefStack>; NEIGHBOUR_SLOTS] {
    [Some(RefStack { item: 1, count: 3 }), None, None]
}

fn fixed_below() -> [Option<RefStack>; NEIGHBOUR_SLOTS] {
    [Some(RefStack { item: 0, count: 63 }), Some(RefStack { item: 1, count: 1 }), None]
}

fn prefix() -> Vec<HopperOp> {
    // First ready tick: alpha merges into below[0], while beta lands in own[1].
    // Seven cooldown ticks do no work; the ninth tick is the next ready tick.
    let mut ops = vec![HopperOp::Tick { enabled: true }];
    ops.extend(std::iter::repeat_n(HopperOp::Tick { enabled: true }, 7));
    ops.push(HopperOp::Tick { enabled: true });
    ops
}

fn stack_strategy() -> impl Strategy<Value = Option<RefStack>> {
    prop_oneof![
        Just(None),
        (0..ITEM_NAMES.len(), 1..=MODEL_STACK_CAP)
            .prop_map(|(item, count)| Some(RefStack { item, count })),
    ]
}

fn operation_strategy() -> impl Strategy<Value = HopperOp> {
    prop_oneof![
        (0..SLOT_COUNT, stack_strategy()).prop_map(|(slot, stack)| HopperOp::SetOwn { slot, stack }),
        (0..NEIGHBOUR_SLOTS, stack_strategy())
            .prop_map(|(slot, stack)| HopperOp::SetAbove { slot, stack }),
        (0..NEIGHBOUR_SLOTS, stack_strategy())
            .prop_map(|(slot, stack)| HopperOp::SetBelow { slot, stack }),
        any::<bool>().prop_map(|enabled| HopperOp::Tick { enabled }),
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

fn initial_model() -> ([Option<RefStack>; SLOT_COUNT], [Option<RefStack>; NEIGHBOUR_SLOTS], [Option<RefStack>; NEIGHBOUR_SLOTS], i32) {
    (fixed_own(), fixed_above(), fixed_below(), -1)
}

fn model_move_one(
    from: &mut [Option<RefStack>],
    to: &mut [Option<RefStack>],
) -> bool {
    for source in from.iter_mut() {
        let Some(source_stack) = source else { continue };
        for destination in to.iter_mut() {
            match destination {
                None => {
                    *destination = Some(RefStack {
                        item: source_stack.item,
                        count: 1,
                    });
                    source_stack.count -= 1;
                    if source_stack.count == 0 {
                        *source = None;
                    }
                    return true;
                }
                Some(destination_stack)
                    if destination_stack.item == source_stack.item
                        && destination_stack.count < MODEL_STACK_CAP =>
                {
                    destination_stack.count += 1;
                    source_stack.count -= 1;
                    if source_stack.count == 0 {
                        *source = None;
                    }
                    return true;
                }
                Some(_) => {}
            }
        }
    }
    false
}

fn model_is_empty(own: &[Option<RefStack>; SLOT_COUNT]) -> bool {
    own.iter().all(Option::is_none)
}

fn model_is_full(own: &[Option<RefStack>; SLOT_COUNT]) -> bool {
    own.iter()
        .all(|stack| matches!(stack, Some(stack) if stack.count >= MODEL_STACK_CAP))
}

fn model_tick(
    own: &mut [Option<RefStack>; SLOT_COUNT],
    above: &mut [Option<RefStack>; NEIGHBOUR_SLOTS],
    below: &mut [Option<RefStack>; NEIGHBOUR_SLOTS],
    cooldown: &mut i32,
    enabled: bool,
    eject: bool,
) -> RefTick {
    *cooldown -= 1;
    if *cooldown > 0 {
        return RefTick {
            ejected: false,
            sucked: false,
        };
    }
    *cooldown = 0;
    if !enabled {
        return RefTick {
            ejected: false,
            sucked: false,
        };
    }

    let ejected = eject && !model_is_empty(own) && model_move_one(own, below);
    let sucked = !model_is_full(own) && model_move_one(above, own);
    if ejected || sucked {
        *cooldown = MODEL_TRANSFER_COOLDOWN;
    }
    RefTick { ejected, sucked }
}

fn model_trace(scenario: &Scenario, eject: bool) -> Vec<RefObservation> {
    let (mut own, mut above, mut below, mut cooldown) = initial_model();
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.tail.iter().copied());
    for op in ops {
        let tick = match op {
            HopperOp::SetOwn { slot, stack } => {
                own[slot] = stack;
                None
            }
            HopperOp::SetAbove { slot, stack } => {
                above[slot] = stack;
                None
            }
            HopperOp::SetBelow { slot, stack } => {
                below[slot] = stack;
                None
            }
            HopperOp::Tick { enabled } => Some(model_tick(
                &mut own,
                &mut above,
                &mut below,
                &mut cooldown,
                enabled,
                eject,
            )),
        };
        trace.push(RefObservation {
            tick,
            own,
            above,
            below,
            cooldown,
        });
    }
    trace
}

fn production_stack(stack: Option<RefStack>) -> Option<ItemStack> {
    stack.map(|stack| {
        ItemStack::new(
            ITEM_NAMES[stack.item]
                .parse()
                .expect("test item names are valid resource keys"),
            stack.count,
        )
    })
}

fn production_stack_view(stack: Option<&ItemStack>) -> Option<RefStack> {
    stack.map(|stack| {
        let item = ITEM_NAMES
            .iter()
            .position(|name| *name == stack.item.to_string())
            .expect("production must only contain generated item names");
        RefStack {
            item,
            count: stack.count,
        }
    })
}

fn production_views<const N: usize>(
    slots: &[Option<ItemStack>; N],
) -> [Option<RefStack>; N] {
    std::array::from_fn(|index| production_stack_view(slots[index].as_ref()))
}

fn production_trace(scenario: &Scenario) -> Vec<Observation> {
    let (own, above, below, _) = initial_model();
    let mut hopper = Hopper::new();
    for (slot, stack) in own.into_iter().enumerate() {
        hopper.set_slot(slot, production_stack(stack));
    }
    let mut above: [Option<ItemStack>; NEIGHBOUR_SLOTS] = above.map(production_stack);
    let mut below: [Option<ItemStack>; NEIGHBOUR_SLOTS] = below.map(production_stack);
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.tail.iter().copied());
    for op in ops {
        let tick = match op {
            HopperOp::SetOwn { slot, stack } => {
                hopper.set_slot(slot, production_stack(stack));
                None
            }
            HopperOp::SetAbove { slot, stack } => {
                above[slot] = production_stack(stack);
                None
            }
            HopperOp::SetBelow { slot, stack } => {
                below[slot] = production_stack(stack);
                None
            }
            HopperOp::Tick { enabled } => Some(hopper.tick(
                enabled,
                Some(&mut below[..]),
                Some(&mut above[..]),
            )),
        };
        trace.push(Observation {
            tick,
            own: production_views(hopper.slots()),
            above: production_views(&above),
            below: production_views(&below),
            cooldown: hopper.cooldown(),
        });
    }
    trace
}

fn equivalent(actual: &[Observation], expected: &[RefObservation]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.tick.as_ref().map(|tick| (tick.ejected, tick.sucked))
                == expected.tick.as_ref().map(|tick| (tick.ejected, tick.sucked))
                && actual.own == expected.own
                && actual.above == expected.above
                && actual.below == expected.below
                && actual.cooldown == expected.cooldown
        })
}

#[test]
fn generated_hopper_ticks_match_the_independent_model() {
    runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            let actual = production_trace(&scenario);
            let expected = model_trace(&scenario, true);
            prop_assert!(equivalent(&actual, &expected), "hopper trace mismatch\nactual: {actual:#?}\nexpected: {expected:#?}");
            Ok(())
        })
        .expect("the production hopper chain must match the independent model");
}

#[test]
fn detector_control_rejects_missing_hopper_ejection() {
    let evaluations = std::cell::Cell::new(0usize);
    let failure = runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            evaluations.set(evaluations.get() + 1);
            let actual = production_trace(&scenario);
            let intentionally_wrong = model_trace(&scenario, false);
            prop_assert!(
                equivalent(&actual, &intentionally_wrong),
                "detector control that omits ejection must disagree: {scenario:?}"
            );
            Ok(())
        })
        .expect_err("a hopper model without ejection must be rejected");

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
