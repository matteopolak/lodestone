//! Deterministic, shrinkable model check for the production scheduled-tick
//! queues.
//!
//! The world tick loop consumes the two queues behind
//! [`lodestone_server::ScheduledTickHandle`], while the save path consumes its
//! non-destructive column snapshots. This test drives that same handle with a
//! bounded action script and compares every result with an independent vector
//! model: deduplication, trigger-time/priority/insertion ordering, due caps,
//! cancellation, cross-chunk routing, and persisted insertion order.
//!
//! The script starts with two same-tick entries whose priorities differ, then
//! appends a shrinkable generated tail. A fixed ChaCha seed makes failures
//! reproducible, and the wrong-priority control proves that the expected-value
//! comparison is not an always-agreeing harness.

use std::cell::Cell;

use lodestone_server::{
    ChunkScheduledTickQueue, PersistedScheduledTick, ScheduledTickHandle, TickPriority,
};
use lodestone_server::region_source::ScheduledTickQueues;
use proptest::collection;
use proptest::prelude::*;
use proptest::sample;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestCaseError, TestError, TestRunner};

const CASES: u32 = 192;
const SEED: u64 = 0x53_43_48_45_44_55_4c;

const POSITIONS: [(i32, i32, i32); 7] = [
    (0, 64, 0),
    (15, 64, 0),
    (16, 64, 0),
    (-1, 64, 0),
    (-16, 64, 0),
    (31, 64, -17),
    (-17, 64, 16),
];
const KINDS: [&str; 3] = ["minecraft:water", "minecraft:redstone", "minecraft:fire"];
const LANES: [Lane; 2] = [Lane::Block, Lane::Fluid];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Block,
    Fluid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueOp {
    Schedule {
        lane: Lane,
        pos: usize,
        kind: usize,
        trigger_tick: u64,
        priority: TickPriority,
    },
    Drain {
        lane: Lane,
        current_tick: u64,
        max_to_process: usize,
    },
    Cancel {
        lane: Lane,
        pos: usize,
        kind: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TickView {
    pos: (i32, i32, i32),
    kind: String,
    trigger_tick: u64,
    priority: TickPriority,
}

impl From<&lodestone_server::ScheduledTick<String>> for TickView {
    fn from(tick: &lodestone_server::ScheduledTick<String>) -> Self {
        Self {
            pos: tick.pos,
            kind: tick.kind.clone(),
            trigger_tick: tick.trigger_tick,
            priority: tick.priority,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Scheduled { lane: Lane, accepted: bool },
    Drained { lane: Lane, ticks: Vec<TickView> },
    Cancelled { lane: Lane, tick: Option<TickView> },
}

#[derive(Debug, Clone)]
struct ReferenceTick {
    pos: (i32, i32, i32),
    kind: String,
    trigger_tick: u64,
    priority: TickPriority,
    insertion_order: u64,
}

#[derive(Debug, Default, Clone)]
struct ReferenceQueue {
    entries: Vec<ReferenceTick>,
    next_insertion_order: u64,
}

impl ReferenceQueue {
    fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: String,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        if self
            .entries
            .iter()
            .any(|entry| entry.pos == pos && entry.kind == kind)
        {
            return false;
        }
        let insertion_order = self.next_insertion_order;
        self.next_insertion_order += 1;
        self.entries.push(ReferenceTick {
            pos,
            kind,
            trigger_tick,
            priority,
            insertion_order,
        });
        true
    }

    fn drain_due(
        &mut self,
        current_tick: u64,
        max_to_process: usize,
        ignore_priority: bool,
    ) -> Vec<TickView> {
        let mut drained = Vec::new();
        while drained.len() < max_to_process {
            let Some(index) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.trigger_tick <= current_tick)
                .min_by_key(|(_, entry)| {
                    if ignore_priority {
                        // Intentional detector model: this is the plausible
                        // but wrong "insertion order wins ties" rule.
                        (entry.trigger_tick, 0_u8, entry.insertion_order)
                    } else {
                        (
                            entry.trigger_tick,
                            priority_rank(entry.priority),
                            entry.insertion_order,
                        )
                    }
                })
                .map(|(index, _)| index)
            else {
                break;
            };
            let entry = self.entries.remove(index);
            drained.push(TickView {
                pos: entry.pos,
                kind: entry.kind,
                trigger_tick: entry.trigger_tick,
                priority: entry.priority,
            });
        }
        drained
    }

    fn cancel(&mut self, pos: (i32, i32, i32), kind: &str) -> Option<TickView> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.pos == pos && entry.kind == kind)?;
        let entry = self.entries.remove(index);
        Some(TickView {
            pos: entry.pos,
            kind: entry.kind,
            trigger_tick: entry.trigger_tick,
            priority: entry.priority,
        })
    }

    fn pending_views(&self) -> Vec<TickView> {
        let mut views = self
            .entries
            .iter()
            .map(|entry| TickView {
                pos: entry.pos,
                kind: entry.kind.clone(),
                trigger_tick: entry.trigger_tick,
                priority: entry.priority,
            })
            .collect::<Vec<_>>();
        sort_views(&mut views);
        views
    }

    fn persisted_column(&self, column_x: i32, column_z: i32) -> Vec<PersistedScheduledTick> {
        let mut ticks = self
            .entries
            .iter()
            .filter(|entry| chunk_for(entry.pos) == (column_x, column_z))
            .map(|entry| PersistedScheduledTick {
                pos: entry.pos,
                kind: entry.kind.clone(),
                trigger_tick: entry.trigger_tick,
                priority: entry.priority,
                insertion_order: entry.insertion_order,
            })
            .collect::<Vec<_>>();
        ticks.sort_by_key(|tick| tick.insertion_order);
        ticks
    }
}

#[derive(Debug, Default)]
struct ReferenceWorld {
    block: ReferenceQueue,
    fluid: ReferenceQueue,
}

impl ReferenceWorld {
    fn queue(&self, lane: Lane) -> &ReferenceQueue {
        match lane {
            Lane::Block => &self.block,
            Lane::Fluid => &self.fluid,
        }
    }

    fn queue_mut(&mut self, lane: Lane) -> &mut ReferenceQueue {
        match lane {
            Lane::Block => &mut self.block,
            Lane::Fluid => &mut self.fluid,
        }
    }

    fn apply(&mut self, op: QueueOp, ignore_priority: bool) -> Event {
        match op {
            QueueOp::Schedule {
                lane,
                pos,
                kind,
                trigger_tick,
                priority,
            } => Event::Scheduled {
                lane,
                accepted: self.queue_mut(lane).schedule(
                    POSITIONS[pos],
                    KINDS[kind].to_owned(),
                    trigger_tick,
                    priority,
                ),
            },
            QueueOp::Drain {
                lane,
                current_tick,
                max_to_process,
            } => Event::Drained {
                lane,
                ticks: self
                    .queue_mut(lane)
                    .drain_due(current_tick, max_to_process, ignore_priority),
            },
            QueueOp::Cancel { lane, pos, kind } => Event::Cancelled {
                lane,
                tick: self.queue_mut(lane).cancel(POSITIONS[pos], KINDS[kind]),
            },
        }
    }
}

fn chunk_for(pos: (i32, i32, i32)) -> (i32, i32) {
    (pos.0.div_euclid(16), pos.2.div_euclid(16))
}

/// Independent numeric spelling of the externally documented priority order.
/// The production queue derives `Ord` on `TickPriority`; using this separate
/// rank in the reference model means a changed production enum order cannot
/// make both sides agree by construction.
fn priority_rank(priority: TickPriority) -> u8 {
    match priority {
        TickPriority::ExtremelyHigh => 0,
        TickPriority::VeryHigh => 1,
        TickPriority::High => 2,
        TickPriority::Normal => 3,
        TickPriority::Low => 4,
        TickPriority::VeryLow => 5,
        TickPriority::ExtremelyLow => 6,
    }
}

fn sort_views(views: &mut [TickView]) {
    views.sort_by(|left, right| {
        (
            left.pos,
            &left.kind,
            left.trigger_tick,
            left.priority,
        )
            .cmp(&(
                right.pos,
                &right.kind,
                right.trigger_tick,
                right.priority,
            ))
    });
}

fn lane_queue_mut(
    queues: &mut ScheduledTickQueues,
    lane: Lane,
) -> &mut ChunkScheduledTickQueue<String> {
    match lane {
        Lane::Block => &mut queues.block,
        Lane::Fluid => &mut queues.fluid,
    }
}

fn lane_queue(
    queues: &ScheduledTickQueues,
    lane: Lane,
) -> &ChunkScheduledTickQueue<String> {
    match lane {
        Lane::Block => &queues.block,
        Lane::Fluid => &queues.fluid,
    }
}

fn apply_production(handle: &ScheduledTickHandle, op: QueueOp) -> Event {
    handle.with(|queues| match op {
        QueueOp::Schedule {
            lane,
            pos,
            kind,
            trigger_tick,
            priority,
        } => Event::Scheduled {
            lane,
            accepted: lane_queue_mut(queues, lane).schedule(
                POSITIONS[pos],
                KINDS[kind].to_owned(),
                trigger_tick,
                priority,
            ),
        },
        QueueOp::Drain {
            lane,
            current_tick,
            max_to_process,
        } => Event::Drained {
            lane,
            ticks: lane_queue_mut(queues, lane)
                .drain_due(current_tick, max_to_process)
                .iter()
                .map(TickView::from)
                .collect(),
        },
        QueueOp::Cancel { lane, pos, kind } => Event::Cancelled {
            lane,
            tick: lane_queue_mut(queues, lane)
                .take_matching(POSITIONS[pos], |candidate| candidate == KINDS[kind])
                .as_ref()
                .map(TickView::from),
        },
    })
}

fn assert_state_matches(
    handle: &ScheduledTickHandle,
    expected: &ReferenceWorld,
) -> Result<(), String> {
    for lane in LANES {
        let actual = handle.with(|queues| {
            let queue = lane_queue(queues, lane);
            let mut views = queue.iter().map(TickView::from).collect::<Vec<_>>();
            sort_views(&mut views);
            (queue.len(), queue.is_empty(), views)
        });
        let reference = expected.queue(lane);
        if actual.0 != reference.entries.len()
            || actual.1 != reference.entries.is_empty()
            || actual.2 != reference.pending_views()
        {
            return Err(format!(
                "{lane:?} pending state differs: production={actual:?}, reference=(len={}, empty={}, views={:?})",
                reference.entries.len(),
                reference.entries.is_empty(),
                reference.pending_views(),
            ));
        }

        for (pos_index, &pos) in POSITIONS.iter().enumerate() {
            for (kind_index, &kind) in KINDS.iter().enumerate() {
                let actual_has = handle.with(|queues| lane_queue(queues, lane).has_scheduled(pos, &kind.to_owned()));
                let expected_has = reference
                    .entries
                    .iter()
                    .any(|entry| entry.pos == pos && entry.kind == kind);
                if actual_has != expected_has {
                    return Err(format!(
                        "{lane:?} has_scheduled differs for pos index {pos_index}, kind index {kind_index}: production={actual_has}, reference={expected_has}"
                    ));
                }
            }
        }
    }

    let mut columns = POSITIONS
        .iter()
        .map(|&pos| chunk_for(pos))
        .collect::<Vec<_>>();
    columns.sort_unstable();
    columns.dedup();
    for (column_x, column_z) in columns {
        let actual = handle.snapshot_column(column_x, column_z);
        let expected_snapshot = (
            expected.block.persisted_column(column_x, column_z),
            expected.fluid.persisted_column(column_x, column_z),
        );
        if actual != expected_snapshot {
            return Err(format!(
                "persisted column ({column_x},{column_z}) differs: production={actual:?}, reference={expected_snapshot:?}"
            ));
        }
    }
    Ok(())
}

fn production_and_reference_agree(script: &[QueueOp]) -> Result<(), String> {
    let handle = ScheduledTickHandle::new();
    let mut expected = ReferenceWorld::default();
    for (step, &op) in script.iter().enumerate() {
        let actual = apply_production(&handle, op);
        let reference = expected.apply(op, false);
        if actual != reference {
            return Err(format!(
                "step {step} {op:?} differs: production={actual:?}, reference={reference:?}"
            ));
        }
        assert_state_matches(&handle, &expected).map_err(|message| format!("step {step}: {message}"))?;
    }
    for lane in LANES {
        let op = QueueOp::Drain {
            lane,
            current_tick: u64::MAX,
            max_to_process: usize::MAX,
        };
        let actual = apply_production(&handle, op);
        let reference = expected.apply(op, false);
        if actual != reference {
            return Err(format!(
                "final {lane:?} drain differs: production={actual:?}, reference={reference:?}"
            ));
        }
        assert_state_matches(&handle, &expected)
            .map_err(|message| format!("final {lane:?} drain: {message}"))?;
    }
    Ok(())
}

fn reference_trace(script: &[QueueOp], ignore_priority: bool) -> Vec<Event> {
    let mut reference = ReferenceWorld::default();
    let mut events = Vec::with_capacity(script.len() + LANES.len());
    for &op in script {
        events.push(reference.apply(op, ignore_priority));
    }
    for lane in LANES {
        events.push(reference.apply(
            QueueOp::Drain {
                lane,
                current_tick: u64::MAX,
                max_to_process: usize::MAX,
            },
            ignore_priority,
        ));
    }
    events
}

fn runner() -> TestRunner {
    TestRunner::new(Config {
        cases: CASES,
        max_shrink_iters: 256,
        max_shrink_time: 0,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        ..Config::default()
    })
}

fn operation_strategy() -> impl Strategy<Value = QueueOp> {
    let lane = prop::bool::ANY.prop_map(|is_fluid| if is_fluid { Lane::Fluid } else { Lane::Block });
    let position = 0..POSITIONS.len();
    let kind = 0..KINDS.len();
    let priority = sample::select(vec![
        TickPriority::ExtremelyHigh,
        TickPriority::VeryHigh,
        TickPriority::High,
        TickPriority::Normal,
        TickPriority::Low,
        TickPriority::VeryLow,
        TickPriority::ExtremelyLow,
    ]);
    prop_oneof![
        (lane.clone(), position.clone(), kind.clone(), 0_u64..=32, priority).prop_map(
            |(lane, pos, kind, trigger_tick, priority)| QueueOp::Schedule {
                lane,
                pos,
                kind,
                trigger_tick,
                priority,
            }
        ),
        (lane.clone(), 0_u64..=32, 0_usize..=8).prop_map(
            |(lane, current_tick, max_to_process)| QueueOp::Drain {
                lane,
                current_tick,
                max_to_process,
            }
        ),
        (lane, position, kind).prop_map(|(lane, pos, kind)| QueueOp::Cancel { lane, pos, kind }),
    ]
}

fn script_strategy() -> impl Strategy<Value = Vec<QueueOp>> {
    collection::vec(operation_strategy(), 0..=32).prop_map(|tail| {
        // This prefix both exercises priority-before-insertion order and
        // guarantees the wrong-priority control has a minimal counterexample.
        let mut script = vec![
            QueueOp::Schedule {
                lane: Lane::Block,
                pos: 0,
                kind: 0,
                trigger_tick: 8,
                priority: TickPriority::Low,
            },
            QueueOp::Schedule {
                lane: Lane::Block,
                pos: 1,
                kind: 0,
                trigger_tick: 8,
                priority: TickPriority::High,
            },
        ];
        script.extend(tail);
        script
    })
}

#[test]
fn generated_queue_scripts_match_the_independent_model() {
    runner()
        .run(&script_strategy(), |script| {
            if let Err(message) = production_and_reference_agree(&script) {
                return Err(TestCaseError::fail(message));
            }
            Ok(())
        })
        .expect("the production scheduled-tick queues must match the independent model");
}

#[test]
fn wrong_priority_model_is_detected_and_shrunk() {
    let evaluations = Cell::new(0usize);
    let failure = runner()
        .run(&script_strategy(), |script| {
            evaluations.set(evaluations.get() + 1);
            let expected = reference_trace(&script, false);
            let intentionally_wrong = reference_trace(&script, true);
            prop_assert_eq!(intentionally_wrong, expected, "detector control");
            Ok(())
        })
        .expect_err("the wrong-priority model must disagree with the generated prefix");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(minimal.len() >= 2, "the fixed priority-conflict prefix must survive shrinking");
            assert_eq!(minimal[0], QueueOp::Schedule {
                lane: Lane::Block,
                pos: 0,
                kind: 0,
                trigger_tick: 8,
                priority: TickPriority::Low,
            });
            assert_eq!(minimal[1], QueueOp::Schedule {
                lane: Lane::Block,
                pos: 1,
                kind: 0,
                trigger_tick: 8,
                priority: TickPriority::High,
            });
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(evaluations.get() > 1, "the fixed-seed control must evaluate shrink candidates");
}
