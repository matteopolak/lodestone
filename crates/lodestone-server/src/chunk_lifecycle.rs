//! Deterministic ownership and source acknowledgement for chunk lifecycle work.
//!
//! Ticket transitions and LRU eviction both eventually release a column through
//! [`crate::chunk::ChunkSource::unload`]. This module makes the owner of that
//! release explicit before a future region executor can move it off the
//! current task. It deliberately starts no worker: the caller executes every
//! assignment serially in canonical chunk-coordinate order. The acknowledgement
//! batch is nevertheless explicit, so a later region worker cannot make a
//! source release visible before the matching owner has reported both the
//! source hand-off and the persistence hand-off complete.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// Which source-side transition an owner is allowed to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkLifecycleAction {
    /// Obtain the complete or shaped column from the source.
    Load,
    /// Release the source's resident/persisted column after cache removal.
    Unload,
}

/// Which side of the lifecycle boundary is acknowledging a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkLifecycleAckPhase {
    /// The source has accepted the selected load or release command.
    Source,
    /// The persistence owner has accepted the source result.
    Persistence,
}

/// The smallest owner that can load or unload one chunk column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkLifecycleOwner {
    /// The column at `(cx, cz)`, including negative coordinates unchanged.
    Chunk { cx: i32, cz: i32 },
}

/// One load or unload action and the chunk owner responsible for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChunkLifecycleAssignment {
    /// The sole owner of this lifecycle action.
    pub owner: ChunkLifecycleOwner,
    /// The target chunk coordinate.
    pub chunk: (i32, i32),
}

/// A capability to acknowledge exactly one selected source transition.
///
/// The token includes the bounded batch and slot rather than only a coordinate:
/// a delayed acknowledgement for an old release must never complete a later
/// load of the same negative or positive chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChunkLifecycleAck {
    batch: u64,
    slot: usize,
    action: ChunkLifecycleAction,
    chunk: (i32, i32),
    phase: ChunkLifecycleAckPhase,
}

/// The result of presenting a lifecycle acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkLifecycleAckResult {
    /// The source/persistence operation completed for this still-pending slot.
    Accepted,
    /// The same slot was already accepted; it cannot complete another action.
    Duplicate,
    /// The token names a different batch, slot, action, or coordinate.
    Stale,
}

/// A bounded, duplicate-free lifecycle hand-off for columns already selected
/// by the current cache operation.
///
/// The plan owns no columns and has no queue. Its size is bounded by the
/// caller's selected inputs: an on-demand load has one assignment, while an
/// eviction batch contains at most the cache's current resident entries. The
/// order is `(cx, cz)`, so a `HashMap`-derived ticket delta cannot change the
/// serial source-unload order from one process run to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkLifecyclePlan {
    action: ChunkLifecycleAction,
    assignments: Vec<ChunkLifecycleAssignment>,
}

impl ChunkLifecyclePlan {
    /// Plans an on-demand load without changing demand-driven generation.
    #[must_use]
    pub(crate) fn load(chunk: (i32, i32)) -> Self {
        Self::from_chunks(ChunkLifecycleAction::Load, [chunk])
    }

    /// Plans a cache-release batch after ticket or LRU selection.
    #[must_use]
    pub(crate) fn unload(chunks: impl IntoIterator<Item = (i32, i32)>) -> Self {
        Self::from_chunks(ChunkLifecycleAction::Unload, chunks)
    }

    fn from_chunks(
        action: ChunkLifecycleAction,
        chunks: impl IntoIterator<Item = (i32, i32)>,
    ) -> Self {
        let mut chunks: Vec<_> = chunks.into_iter().collect();
        chunks.sort_unstable();
        chunks.dedup();
        Self {
            action,
            assignments: chunks
                .into_iter()
                .map(|(cx, cz)| ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx, cz },
                    chunk: (cx, cz),
                })
                .collect(),
        }
    }

    /// The serial, canonical owner hand-off the current task consumes.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn assignments(&self) -> &[ChunkLifecycleAssignment] {
        &self.assignments
    }
}

/// One bounded hand-off batch. It is intentionally not a process-wide history:
/// after the caller finishes or drops this batch, every one of its tokens is
/// stale by construction rather than accumulating acknowledgement state for
/// every chunk the server has ever visited.
#[derive(Debug)]
pub(crate) struct ChunkLifecycleBatch {
    id: u64,
    plan: ChunkLifecyclePlan,
    slots: Vec<ChunkLifecycleSlotState>,
}

/// The only legal states for one bounded lifecycle slot.
///
/// The state names are intentionally directional. A persistence reply cannot
/// move a slot that has not first received its source reply, and a source
/// reply cannot be presented twice after the hand-off has moved forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkLifecycleSlotState {
    SourceReady,
    SourceInFlight,
    PersistenceReady,
    PersistenceInFlight,
    Complete,
}

impl ChunkLifecycleBatch {
    fn new(id: u64, plan: ChunkLifecyclePlan) -> Self {
        let slots = vec![ChunkLifecycleSlotState::SourceReady; plan.assignments.len()];
        Self {
            id,
            plan,
            slots,
        }
    }

    fn acknowledgement(&self, slot: usize, phase: ChunkLifecycleAckPhase) -> ChunkLifecycleAck {
        let assignment = self.plan.assignments[slot];
        ChunkLifecycleAck {
            batch: self.id,
            slot,
            action: self.plan.action,
            chunk: assignment.chunk,
            phase,
        }
    }

    /// Starts the selected source operation and returns its acknowledgement.
    ///
    /// A slot can be dispatched only once. Returning `None` for a duplicate or
    /// out-of-range slot is the fail-closed behaviour: callers cannot create a
    /// second source operation by retrying a command lookup.
    #[must_use]
    pub(crate) fn command(
        &mut self,
        slot: usize,
    ) -> Option<(ChunkLifecycleAssignment, ChunkLifecycleAck)> {
        if self.slots.get(slot).copied()? != ChunkLifecycleSlotState::SourceReady {
            return None;
        }
        self.slots[slot] = ChunkLifecycleSlotState::SourceInFlight;
        let assignment = self.plan.assignments[slot];
        Some((assignment, self.acknowledgement(slot, ChunkLifecycleAckPhase::Source)))
    }

    /// Starts the persistence hand-off after the source acknowledged it.
    ///
    /// Persistence is deliberately a second command, rather than an implicit
    /// boolean on the source command. A future worker can retain this bounded
    /// batch between these calls while it waits for its durable writer.
    #[must_use]
    pub(crate) fn persistence_command(
        &mut self,
        slot: usize,
    ) -> Option<(ChunkLifecycleAssignment, ChunkLifecycleAck)> {
        if self.slots.get(slot).copied()? != ChunkLifecycleSlotState::PersistenceReady {
            return None;
        }
        self.slots[slot] = ChunkLifecycleSlotState::PersistenceInFlight;
        let assignment = self.plan.assignments[slot];
        Some((
            assignment,
            self.acknowledgement(slot, ChunkLifecycleAckPhase::Persistence),
        ))
    }

    /// Records one source or persistence acknowledgement without accepting
    /// stale, out-of-order, or duplicated replies.
    pub(crate) fn acknowledge(&mut self, ack: ChunkLifecycleAck) -> ChunkLifecycleAckResult {
        if ack.batch != self.id || ack.action != self.plan.action {
            return ChunkLifecycleAckResult::Stale;
        }
        let Some(assignment) = self.plan.assignments.get(ack.slot) else {
            return ChunkLifecycleAckResult::Stale;
        };
        if assignment.chunk != ack.chunk {
            return ChunkLifecycleAckResult::Stale;
        }
        let state = &mut self.slots[ack.slot];
        match (ack.phase, *state) {
            (ChunkLifecycleAckPhase::Source, ChunkLifecycleSlotState::SourceInFlight) => {
                *state = ChunkLifecycleSlotState::PersistenceReady;
                ChunkLifecycleAckResult::Accepted
            }
            (
                ChunkLifecycleAckPhase::Persistence,
                ChunkLifecycleSlotState::PersistenceInFlight,
            ) => {
                *state = ChunkLifecycleSlotState::Complete;
                ChunkLifecycleAckResult::Accepted
            }
            (_, ChunkLifecycleSlotState::Complete)
            | (ChunkLifecycleAckPhase::Source, ChunkLifecycleSlotState::PersistenceReady)
            | (ChunkLifecycleAckPhase::Source, ChunkLifecycleSlotState::PersistenceInFlight) => {
                ChunkLifecycleAckResult::Duplicate
            }
            _ => ChunkLifecycleAckResult::Stale,
        }
    }

    #[must_use]
    fn len(&self) -> usize {
        self.plan.assignments.len()
    }

    #[must_use]
    fn is_complete(&self) -> bool {
        self.slots
            .iter()
            .all(|state| *state == ChunkLifecycleSlotState::Complete)
    }
}

/// Serial source ownership for bounded lifecycle batches.
///
/// The cache changes its resident set before it asks the source to unload, and
/// this hand-off then owns the source call until it accepts its acknowledgement.
/// Different chunks retain their independent source calls, while repeated work
/// for the *same* chunk cannot invert a load behind an earlier unload. Weak
/// per-chunk gates disappear once no caller holds them, so this coordination
/// boundary does not become a second unbounded resident-chunk map.
#[derive(Debug, Default)]
pub(crate) struct ChunkLifecycleHandoff {
    state: Mutex<ChunkLifecycleHandoffState>,
}

#[derive(Debug, Default)]
struct ChunkLifecycleHandoffState {
    next_batch: u64,
    source_gates: HashMap<(i32, i32), Weak<Mutex<()>>>,
}

impl ChunkLifecycleHandoff {
    /// Executes an already-bounded plan through the source and persistence
    /// hand-offs. The current source contract has no durable-write callback,
    /// so this convenience path treats the source result as the immediate
    /// persistence acknowledgement. A future region worker should use the
    /// batch-level command/acknowledgement methods and retain its batch until
    /// the real writer reports completion.
    pub(crate) fn execute<R>(
        &self,
        plan: ChunkLifecyclePlan,
        mut source: impl FnMut(ChunkLifecycleAssignment) -> R,
    ) -> Vec<R> {
        self.execute_with_persistence(plan, &mut source, |_, _| {})
    }

    /// Executes an already-bounded plan with an explicit persistence callback.
    ///
    /// The source gate remains held from source dispatch through persistence
    /// acknowledgement. This prevents a same-coordinate load from overtaking
    /// a release whose persistence hand-off is still pending, while unrelated
    /// coordinates retain independent gates.
    pub(crate) fn execute_with_persistence<R>(
        &self,
        plan: ChunkLifecyclePlan,
        mut source: impl FnMut(ChunkLifecycleAssignment) -> R,
        mut persistence: impl FnMut(ChunkLifecycleAssignment, &R),
    ) -> Vec<R> {
        let mut batch = self.open(plan);
        let mut completed = Vec::with_capacity(batch.len());
        for slot in 0..batch.len() {
            let (assignment, ack) = batch
                .command(slot)
                .expect("bounded lifecycle batch has a command for every slot");
            let gate = self.source_gate(assignment.chunk);
            let owner = gate.lock().expect("chunk lifecycle source gate poisoned");
            let result = source(assignment);
            assert_eq!(
                batch.acknowledge(ack),
                ChunkLifecycleAckResult::Accepted,
                "a fresh lifecycle source acknowledgement must be accepted",
            );
            let (persistence_assignment, persistence_ack) = batch
                .persistence_command(slot)
                .expect("a source acknowledgement must authorize persistence");
            debug_assert_eq!(persistence_assignment, assignment);
            persistence(persistence_assignment, &result);
            assert_eq!(
                batch.acknowledge(persistence_ack),
                ChunkLifecycleAckResult::Accepted,
                "a fresh lifecycle persistence acknowledgement must be accepted",
            );
            drop(owner);
            self.release_source_gate(assignment.chunk, &gate);
            completed.push(result);
        }
        assert!(
            batch.is_complete(),
            "lifecycle hand-off cannot finish with an unacknowledged slot"
        );
        completed
    }

    /// Opens a bounded batch for a worker that needs to defer persistence.
    ///
    /// The returned batch owns all acknowledgement state for this selection;
    /// callers must retain it until every slot reaches `Complete`. No batch is
    /// registered globally, so dropping it cannot leave an acknowledgement
    /// history behind.
    pub(crate) fn open(&self, plan: ChunkLifecyclePlan) -> ChunkLifecycleBatch {
        let mut state = self.state.lock().expect("chunk lifecycle hand-off poisoned");
        state.next_batch = state
            .next_batch
            .checked_add(1)
            .expect("chunk lifecycle batch id space exhausted");
        ChunkLifecycleBatch::new(state.next_batch, plan)
    }

    fn source_gate(&self, chunk: (i32, i32)) -> Arc<Mutex<()>> {
        let mut state = self.state.lock().expect("chunk lifecycle hand-off poisoned");
        // A callback panic can drop the last strong gate before the normal
        // release path runs. Prune those abandoned weak records before adding
        // another coordinate, keeping this coordination map bounded even
        // across a failed operation.
        state.source_gates.retain(|_, gate| gate.strong_count() != 0);
        if let Some(gate) = state.source_gates.get(&chunk).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        state.source_gates.insert(chunk, Arc::downgrade(&gate));
        gate
    }

    fn release_source_gate(&self, chunk: (i32, i32), gate: &Arc<Mutex<()>>) {
        let mut state = self.state.lock().expect("chunk lifecycle hand-off poisoned");
        let Some(recorded) = state.source_gates.get(&chunk) else {
            return;
        };
        if recorded
            .upgrade()
            .is_some_and(|recorded_gate| Arc::ptr_eq(&recorded_gate, gate))
            && Arc::strong_count(gate) == 1
        {
            state.source_gates.remove(&chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn lifecycle_assignments_canonically_order_negative_chunks_and_deduplicate() {
        let plan = ChunkLifecyclePlan::unload([(0, 0), (-1, 2), (0, -1), (-1, 2), (-2, 0)]);

        assert_eq!(
            plan.assignments(),
            [
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: -2, cz: 0 },
                    chunk: (-2, 0),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: -1, cz: 2 },
                    chunk: (-1, 2),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: 0, cz: -1 },
                    chunk: (0, -1),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: 0, cz: 0 },
                    chunk: (0, 0),
                },
            ]
        );
    }

    #[test]
    fn an_on_demand_load_has_exactly_its_target_chunk_owner() {
        let plan = ChunkLifecyclePlan::load((-1, -1));

        assert_eq!(
            plan.assignments(),
            [ChunkLifecycleAssignment {
                owner: ChunkLifecycleOwner::Chunk { cx: -1, cz: -1 },
                chunk: (-1, -1),
            }]
        );
    }

    #[test]
    fn acknowledgements_reject_duplicates_and_old_batches() {
        let handoff = ChunkLifecycleHandoff::default();
        let mut first = handoff.open(ChunkLifecyclePlan::unload([(-1, -2)]));
        let (_, ack) = first.command(0).expect("one release command");
        assert_eq!(first.acknowledge(ack), ChunkLifecycleAckResult::Accepted);
        assert_eq!(first.acknowledge(ack), ChunkLifecycleAckResult::Duplicate);
        let (_, persistence_ack) = first
            .persistence_command(0)
            .expect("source acknowledgement authorizes persistence");
        assert_eq!(
            first.acknowledge(persistence_ack),
            ChunkLifecycleAckResult::Accepted
        );

        let mut second = handoff.open(ChunkLifecyclePlan::load((-1, -2)));
        assert_eq!(second.acknowledge(ack), ChunkLifecycleAckResult::Stale);
        let (_, current) = second.command(0).expect("one load command");
        assert_eq!(second.acknowledge(current), ChunkLifecycleAckResult::Accepted);
    }

    #[test]
    fn lifecycle_slots_fail_closed_until_each_typed_transition_is_started() {
        let handoff = ChunkLifecycleHandoff::default();
        let mut batch = handoff.open(ChunkLifecyclePlan::load((4, -7)));
        assert!(
            batch.persistence_command(0).is_none(),
            "persistence cannot start before the source acknowledgement"
        );

        let (_, source_ack) = batch.command(0).expect("one load command");
        assert!(
            batch.command(0).is_none(),
            "a source command cannot be dispatched twice"
        );
        let wrong_action = ChunkLifecycleAck {
            action: ChunkLifecycleAction::Unload,
            ..source_ack
        };
        assert_eq!(
            batch.acknowledge(wrong_action),
            ChunkLifecycleAckResult::Stale,
            "an unload reply cannot acknowledge a load"
        );
        let wrong_coordinate = ChunkLifecycleAck {
            chunk: (4, -6),
            ..source_ack
        };
        assert_eq!(
            batch.acknowledge(wrong_coordinate),
            ChunkLifecycleAckResult::Stale,
            "a reply for a neighbouring chunk cannot advance this slot"
        );
        let premature_persistence_ack = ChunkLifecycleAck {
            phase: ChunkLifecycleAckPhase::Persistence,
            ..source_ack
        };
        assert_eq!(
            batch.acknowledge(premature_persistence_ack),
            ChunkLifecycleAckResult::Stale,
            "a persistence reply before source completion must not advance the slot"
        );

        assert_eq!(batch.acknowledge(source_ack), ChunkLifecycleAckResult::Accepted);
        let (_, persistence_ack) = batch
            .persistence_command(0)
            .expect("source completion opens persistence");
        assert!(
            batch.persistence_command(0).is_none(),
            "a persistence command cannot be dispatched twice"
        );
        assert_eq!(
            batch.acknowledge(persistence_ack),
            ChunkLifecycleAckResult::Accepted
        );
        assert_eq!(
            batch.acknowledge(persistence_ack),
            ChunkLifecycleAckResult::Duplicate
        );
        assert!(batch.is_complete(), "both hand-off phases must complete");
    }

    #[test]
    fn persistence_callback_is_after_source_ack_and_before_next_coordinate() {
        let handoff = ChunkLifecycleHandoff::default();
        let events = std::cell::RefCell::new(Vec::new());
        let seen = handoff.execute_with_persistence(
            ChunkLifecyclePlan::unload([(1, 0), (-1, 0)]),
            |assignment| {
                events.borrow_mut().push(("source", assignment.chunk));
                assignment.chunk
            },
            |assignment, _| events.borrow_mut().push(("persistence", assignment.chunk)),
        );
        assert_eq!(seen, [(-1, 0), (1, 0)]);
        assert_eq!(
            *events.borrow(),
            [
                ("source", (-1, 0)),
                ("persistence", (-1, 0)),
                ("source", (1, 0)),
                ("persistence", (1, 0)),
            ]
        );
    }

    #[test]
    fn handoff_keeps_negative_release_order_and_only_bounded_batch_state() {
        let handoff = ChunkLifecycleHandoff::default();
        let seen = handoff.execute(
            ChunkLifecyclePlan::unload([(0, 0), (-1, 2), (-2, 0), (-1, 2)]),
            |assignment| assignment.chunk,
        );
        assert_eq!(seen, [(-2, 0), (-1, 2), (0, 0)]);
        assert!(
            handoff
                .state
                .lock()
                .expect("chunk lifecycle hand-off poisoned")
                .source_gates
                .is_empty(),
            "completed source ownership gates must not retain every historical chunk"
        );
    }

    #[test]
    fn a_negative_chunk_load_waits_for_its_prior_release_acknowledgement() {
        let handoff = Arc::new(ChunkLifecycleHandoff::default());
        let (release_entered_tx, release_entered_rx) = mpsc::channel();
        let (allow_release_tx, allow_release_rx) = mpsc::channel();
        let release_handoff = Arc::clone(&handoff);
        let release = thread::spawn(move || {
            release_handoff.execute_with_persistence(
                ChunkLifecyclePlan::unload([(-1, 0)]),
                |_| {
                    release_entered_tx.send(()).expect("test must observe release");
                },
                |_, _| allow_release_rx.recv().expect("test must finish persistence"),
            );
        });

        release_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("release must own the negative chunk source gate");
        let (load_entered_tx, load_entered_rx) = mpsc::channel();
        let load_handoff = Arc::clone(&handoff);
        let load = thread::spawn(move || {
            load_handoff.execute(ChunkLifecyclePlan::load((-1, 0)), |_| {
                load_entered_tx.send(()).expect("load source call must be observed");
            });
        });

        assert!(
            load_entered_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "a same-coordinate load entered the source before persistence acknowledged"
        );
        allow_release_tx.send(()).expect("release thread must still wait");
        release.join().expect("release thread must finish");
        load_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("load must enter only after the release acknowledgement");
        load.join().expect("load thread must finish");
    }
}
