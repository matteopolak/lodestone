//! Bounded, fixed-seed model checking for the server-side container click
//! state machine.
//!
//! The production consumer is `lodestone_server::container_click::do_click`.
//! This test drives it through a three-slot generic container and compares every
//! click with an independent model that stores only item kind, count, cursor and
//! dropped stacks. The fixed prefix exercises merge-before-place quick-move and
//! a multi-slot drag; the generated tail is finite and shrinkable.
//!
//! The detector control deliberately removes quick-move's merge pass. Its fixed
//! partial-stack witness must fail, so an accidentally inert comparison cannot
//! make this lane green.

use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestCaseError, TestError, TestRunner};

use lodestone_model::{Identifier, ItemStack};
use lodestone_server::container_click::{Click, ClickState, MenuLayout, do_click};

const CASES: u32 = 160;
const SEED: u64 = 0x43_4c_49_43_4b_35_49;
const MAX_SHRINK_ITERS: u32 = 256;
const MENU_LEN: usize = 39;
const OWN_SLOT_COUNT: usize = 3;
const HOTBAR_START: usize = 30;

// These deliberately custom item ids are outside the generated prototype table.
// The model's 64-item cap is therefore an input-domain rule, not a call back into
// the production cap lookup. Components remain empty on both sides.
const ITEM_NAMES: [&str; 3] = ["test:alpha", "test:beta", "test:gamma"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RefStack {
    item: usize,
    count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickOp {
    Click {
        slot: i32,
        button: i8,
        click_type: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefDrag {
    status: i32,
    kind: i32,
    slots: Vec<usize>,
}

impl Default for RefDrag {
    fn default() -> Self {
        Self {
            status: 0,
            kind: 0,
            slots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefState {
    slots: Vec<Option<RefStack>>,
    carried: Option<RefStack>,
    dropped: Vec<RefStack>,
    drag: RefDrag,
}

impl RefState {
    fn new() -> Self {
        let mut slots = vec![None; MENU_LEN];
        // The first witness is independent of the generated tail: a quick move
        // must merge four items into the partial stack at the end of the range,
        // then place the sixteen-item remainder in the next empty slot.
        slots[0] = Some(RefStack { item: 0, count: 20 });
        slots[1] = Some(RefStack { item: 1, count: 8 });
        slots[HOTBAR_START + 8] = Some(RefStack { item: 0, count: 60 });
        Self {
            slots,
            carried: None,
            dropped: Vec::new(),
            drag: RefDrag::default(),
        }
    }

    fn apply(&mut self, op: ClickOp, merge_pass: bool) {
        self.dropped.clear();
        let ClickOp::Click {
            slot,
            button,
            click_type,
        } = op;
        let index = usize::try_from(slot).ok();

        if click_type == 5 {
            self.quick_craft(index, button, false);
            return;
        }
        // A click outside an in-progress drag cancels the drag and does not also
        // perform the click's ordinary action.
        if self.drag.status != 0 {
            self.drag = RefDrag::default();
            return;
        }

        match click_type {
            0 | 1 if button == 0 || button == 1 => {
                if slot == -999 {
                    if let Some(mut carried) = self.carried {
                        if button == 0 {
                            self.dropped.push(carried);
                            self.carried = None;
                        } else {
                            self.dropped.push(RefStack {
                                item: carried.item,
                                count: 1,
                            });
                            carried.count -= 1;
                            self.carried = (carried.count > 0).then_some(carried);
                        }
                    }
                } else if let Some(index) = index.filter(|&index| index < MENU_LEN) {
                    if click_type == 1 {
                        self.quick_move(index, merge_pass);
                    } else {
                        self.pickup(index, button == 0);
                    }
                }
            }
            2 if (0..=8).contains(&button) => {
                if let Some(index) = index.filter(|&index| index < MENU_LEN) {
                    self.swap(index, button as usize);
                }
            }
            4 if self.carried.is_none() && (button == 0 || button == 1) => {
                if let Some(index) = index.filter(|&index| index < MENU_LEN) {
                    let amount = if button == 1 {
                        self.slots[index].map_or(0, |stack| stack.count)
                    } else {
                        1
                    };
                    if amount > 0 {
                        if let Some(taken) = self.take(index, amount) {
                            self.dropped.push(taken);
                            // Generic containers have no derived result slot, so
                            // the repeat arm has no second iteration.
                        }
                    }
                }
            }
            6 => {
                if let Some(index) = index.filter(|&index| index < MENU_LEN) {
                    self.pickup_all(index, button == 0);
                }
            }
            _ => {}
        }
    }

    fn quick_craft(&mut self, index: Option<usize>, button: i8, creative: bool) {
        let expected = self.drag.status;
        self.drag.status = i32::from(button) & 3;
        if (expected != 1 || self.drag.status != 2) && expected != self.drag.status {
            self.drag = RefDrag::default();
        } else if self.carried.is_none() {
            self.drag = RefDrag::default();
        } else if self.drag.status == 0 {
            self.drag.kind = (i32::from(button) >> 2) & 3;
            if self.drag.kind == 2 && !creative {
                self.drag = RefDrag::default();
            } else {
                self.drag.status = 1;
                self.drag.slots.clear();
            }
        } else if self.drag.status == 1 {
            let carried = self.carried.expect("checked non-empty above");
            if let Some(index) = index.filter(|&index| index < MENU_LEN) {
                let replaceable = self.slots[index].is_none_or(|existing| {
                    existing.item == carried.item && existing.count <= 64
                });
                if replaceable
                    && (self.drag.kind == 2 || carried.count > self.drag.slots.len() as u32)
                    && !self.drag.slots.contains(&index)
                {
                    self.drag.slots.push(index);
                }
            }
        } else if self.drag.status == 2 {
            self.finish_drag();
            self.drag = RefDrag::default();
        } else {
            self.drag = RefDrag::default();
        }
    }

    fn finish_drag(&mut self) {
        if self.drag.slots.is_empty() {
            return;
        }
        let Some(source) = self.carried else { return };
        if self.drag.slots.len() == 1 {
            let index = self.drag.slots[0];
            let primary = self.drag.kind == 0;
            self.pickup(index, primary);
            return;
        }

        let mut remaining = source.count;
        let selected = self.drag.slots.clone();
        for index in selected {
            let replaceable = self.slots[index].is_none_or(|existing| {
                existing.item == source.item && existing.count <= 64
            });
            if !replaceable {
                continue;
            }
            if self.drag.kind != 2 && self.carried.is_some_and(|carried| carried.count < self.drag.slots.len() as u32) {
                continue;
            }
            let held = self.slots[index].map_or(0, |stack| stack.count);
            let per_slot = match self.drag.kind {
                0 => source.count / self.drag.slots.len().max(1) as u32,
                1 => 1,
                2 => 64,
                _ => source.count,
            };
            let new_count = (per_slot + held).min(64);
            remaining = remaining.saturating_sub(new_count - held);
            self.slots[index] = Some(RefStack {
                item: source.item,
                count: new_count,
            });
        }
        self.carried = (remaining > 0).then_some(RefStack {
            item: source.item,
            count: remaining,
        });
    }

    fn pickup(&mut self, index: usize, primary: bool) {
        match (self.slots[index], self.carried) {
            (None, Some(carried)) => {
                let amount = if primary { carried.count } else { 1 };
                self.insert(index, carried, amount);
            }
            (Some(clicked), None) => {
                let amount = if primary { clicked.count } else { clicked.count.div_ceil(2) };
                self.carried = self.take(index, amount);
            }
            (Some(clicked), Some(carried)) if clicked.item == carried.item => {
                let amount = if primary { carried.count } else { 1 };
                self.insert(index, carried, amount);
            }
            (Some(clicked), Some(carried)) if carried.count <= 64 => {
                self.slots[index] = Some(carried);
                self.carried = Some(clicked);
            }
            _ => {}
        }
    }

    fn insert(&mut self, index: usize, mut stack: RefStack, amount: u32) {
        let held = self.slots[index].map_or(0, |existing| {
            if existing.item == stack.item { existing.count } else { return 0 }
        });
        if self.slots[index].is_some_and(|existing| existing.item != stack.item) {
            return;
        }
        let moved = amount.min(stack.count).min(64 - held);
        if moved == 0 {
            return;
        }
        self.slots[index] = Some(RefStack {
            item: stack.item,
            count: held + moved,
        });
        stack.count -= moved;
        self.carried = (stack.count > 0).then_some(stack);
    }

    fn take(&mut self, index: usize, amount: u32) -> Option<RefStack> {
        let existing = self.slots[index]?;
        let taken = amount.min(existing.count);
        if taken == 0 {
            return None;
        }
        self.slots[index] = (existing.count > taken).then_some(RefStack {
            item: existing.item,
            count: existing.count - taken,
        });
        Some(RefStack {
            item: existing.item,
            count: taken,
        })
    }

    fn quick_move(&mut self, index: usize, merge_pass: bool) {
        let Some(source) = self.slots[index] else { return };
        let mut stack = source;
        let (start, end, backwards) = if index < OWN_SLOT_COUNT {
            (OWN_SLOT_COUNT, MENU_LEN, true)
        } else {
            (0, OWN_SLOT_COUNT, false)
        };
        let mut order: Vec<usize> = (start..end).collect();
        if backwards {
            order.reverse();
        }
        if merge_pass {
            for &target in &order {
                if stack.count == 0 || target == index {
                    continue;
                }
                let Some(existing) = self.slots[target] else { continue };
                if existing.item != stack.item {
                    continue;
                }
                let moved = stack.count.min(64 - existing.count);
                if moved > 0 {
                    self.slots[target] = Some(RefStack {
                        item: existing.item,
                        count: existing.count + moved,
                    });
                    stack.count -= moved;
                }
            }
        }
        if stack.count > 0 {
            for &target in &order {
                if target == index || self.slots[target].is_some() {
                    continue;
                }
                self.slots[target] = Some(stack);
                stack.count = 0;
                break;
            }
        }
        if stack.count == source.count {
            return;
        }
        self.slots[index] = (stack.count > 0).then_some(stack);
    }

    fn swap(&mut self, index: usize, hotbar: usize) {
        let source_index = HOTBAR_START + hotbar;
        if source_index == index {
            return;
        }
        let source = self.slots[source_index];
        let target = self.slots[index];
        match (source, target) {
            (None, None) => {}
            (None, Some(target)) => {
                self.slots[index] = None;
                self.slots[source_index] = Some(target);
            }
            (Some(source), None) => {
                if source.count <= 64 {
                    self.slots[source_index] = None;
                    self.slots[index] = Some(source);
                } else {
                    self.slots[index] = Some(RefStack { item: source.item, count: 64 });
                    self.slots[source_index] = Some(RefStack { item: source.item, count: source.count - 64 });
                }
            }
            (Some(source), Some(target)) => {
                if source.count <= 64 {
                    self.slots[source_index] = Some(target);
                    self.slots[index] = Some(source);
                } else {
                    self.slots[index] = Some(RefStack { item: source.item, count: 64 });
                    self.slots[source_index] = Some(RefStack { item: source.item, count: source.count - 64 });
                    self.dropped.push(target);
                }
            }
        }
    }

    fn pickup_all(&mut self, index: usize, forwards: bool) {
        let Some(mut carried) = self.carried else { return };
        if self.slots[index].is_some() {
            return;
        }
        let mut order: Vec<usize> = (0..MENU_LEN).collect();
        if !forwards {
            order.reverse();
        }
        for pass in 0..2 {
            for target in &order {
                if carried.count >= 64 {
                    break;
                }
                let Some(existing) = self.slots[*target] else { continue };
                if existing.item != carried.item || (pass == 0 && existing.count == 64) {
                    continue;
                }
                let room = 64 - carried.count;
                if let Some(taken) = self.take(*target, existing.count.min(room)) {
                    carried.count += taken.count;
                }
            }
        }
        self.carried = Some(carried);
    }
}

fn production_stack(stack: RefStack) -> ItemStack {
    ItemStack::new(
        ITEM_NAMES[stack.item]
            .parse::<Identifier>()
            .expect("the test item identifier is valid"),
        stack.count,
    )
}

fn production_snapshot(slots: &[Option<ItemStack>], state: &ClickState) -> (Vec<Option<RefStack>>, Option<RefStack>) {
    let slots = slots
        .iter()
        .map(|stack| {
            stack.as_ref().map(|stack| RefStack {
                item: ITEM_NAMES
                    .iter()
                    .position(|name| *name == stack.item.to_string())
                    .expect("production must preserve the generated item alphabet"),
                count: stack.count,
            })
        })
        .collect();
    let carried = state.carried.as_ref().map(|stack| RefStack {
        item: ITEM_NAMES
            .iter()
            .position(|name| *name == stack.item.to_string())
            .expect("production must preserve the generated item alphabet"),
        count: stack.count,
    });
    (slots, carried)
}

fn runner() -> TestRunner {
    TestRunner::new(Config {
        cases: CASES,
        max_shrink_iters: MAX_SHRINK_ITERS,
        max_shrink_time: 0,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        ..Config::default()
    })
}

fn click_strategy() -> impl Strategy<Value = ClickOp> {
    let slot = prop_oneof![Just(-999), -2_i32..=40_i32];
    prop_oneof![
        (slot.clone(), 0_i8..=1).prop_map(|(slot, button)| ClickOp::Click { slot, button, click_type: 0 }),
        (slot.clone(), 0_i8..=1).prop_map(|(slot, button)| ClickOp::Click { slot, button, click_type: 1 }),
        (slot.clone(), 0_i8..=8).prop_map(|(slot, button)| ClickOp::Click { slot, button, click_type: 2 }),
        (slot.clone(), 0_i8..=1).prop_map(|(slot, button)| ClickOp::Click { slot, button, click_type: 4 }),
        (slot.clone(), 0_i8..=1).prop_map(|(slot, button)| ClickOp::Click { slot, button, click_type: 6 }),
        (slot, prop::bool::ANY, 0_i8..=2).prop_map(|(slot, kind, stage)| ClickOp::Click {
            slot,
            button: (if kind { 4 } else { 0 }) | stage,
            click_type: 5,
        }),
    ]
}

fn script_strategy() -> impl Strategy<Value = Vec<ClickOp>> {
    collection::vec(click_strategy(), 0..=32).prop_map(|tail| {
        let mut script = vec![
            // Partial-stack merge witness.
            ClickOp::Click { slot: 0, button: 0, click_type: 1 },
            // Pick up beta, then distribute it over three empty slots.
            ClickOp::Click { slot: 1, button: 0, click_type: 0 },
            ClickOp::Click { slot: -999, button: 0, click_type: 5 },
            ClickOp::Click { slot: 2, button: 1, click_type: 5 },
            ClickOp::Click { slot: 3, button: 1, click_type: 5 },
            ClickOp::Click { slot: 4, button: 1, click_type: 5 },
            ClickOp::Click { slot: -999, button: 2, click_type: 5 },
        ];
        script.extend(tail);
        script
    })
}

fn production_and_reference_agree(script: &[ClickOp]) -> Result<(), String> {
    let layout = MenuLayout::container(OWN_SLOT_COUNT);
    let mut slots = RefState::new()
        .slots
        .iter()
        .map(|stack| stack.map(production_stack))
        .collect::<Vec<_>>();
    let mut actual_state = ClickState::default();
    let mut expected = RefState::new();

    for (step, &op) in script.iter().enumerate() {
        let ClickOp::Click { slot, button, click_type } = op;
        let dropped = do_click(
            &layout,
            &mut slots,
            &mut actual_state,
            Click { slot, button, click_type },
            false,
        );
        expected.apply(op, true);
        let actual = production_snapshot(&slots, &actual_state);
        let expected_view = (expected.slots.clone(), expected.carried);
        if actual != expected_view {
            return Err(format!("step {step} {op:?} state differs: production={actual:?}, reference={expected_view:?}"));
        }
        let actual_dropped = dropped
            .iter()
            .map(|stack| {
                ITEM_NAMES
                    .iter()
                    .position(|name| *name == stack.item.to_string())
                    .map(|item| RefStack { item, count: stack.count })
                    .ok_or_else(|| format!("step {step}: unknown dropped item {}", stack.item))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if actual_dropped != expected.dropped {
            return Err(format!("step {step} {op:?} drops differ: production={actual_dropped:?}, reference={:?}", expected.dropped));
        }
    }
    Ok(())
}

#[test]
fn generated_container_click_scripts_match_the_independent_model() {
    runner()
        .run(&script_strategy(), |script| {
            if let Err(message) = production_and_reference_agree(&script) {
                return Err(TestCaseError::fail(message));
            }
            Ok(())
        })
        .expect("the production container click state machine must match the independent model");
}

#[test]
fn detector_control_rejects_a_quick_move_without_merge() {
    let evaluations = std::cell::Cell::new(0usize);
    let failure = runner()
        .run(&script_strategy(), |script| {
            evaluations.set(evaluations.get() + 1);
            let mut expected = RefState::new();
            let layout = MenuLayout::container(OWN_SLOT_COUNT);
            let mut slots = expected
                .slots
                .iter()
                .map(|stack| stack.map(production_stack))
                .collect::<Vec<_>>();
            let mut state = ClickState::default();
            for &op in &script {
                let ClickOp::Click { slot, button, click_type } = op;
                do_click(
                    &layout,
                    &mut slots,
                    &mut state,
                    Click { slot, button, click_type },
                    false,
                );
                expected.apply(op, false);
                let actual = production_snapshot(&slots, &state);
                let wrong = (expected.slots.clone(), expected.carried);
                prop_assert_eq!(wrong, actual, "detector control");
            }
            Ok(())
        })
        .expect_err("the wrong quick-move model must disagree with the fixed witness");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(minimal.len() >= 1, "the fixed merge witness must survive shrinking");
            assert_eq!(minimal[0], ClickOp::Click { slot: 0, button: 0, click_type: 1 });
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(evaluations.get() > 1, "the fixed-seed control must evaluate shrink candidates");
}
