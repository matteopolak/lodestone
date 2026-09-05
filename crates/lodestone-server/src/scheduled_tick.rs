//! The scheduled-tick queue (the first half of scheduled block/fluid processing): the
//! per-block and per-fluid tick machinery, collapsed to one generic
//! [`ScheduledTickQueue<T>`] the tick loop instantiates twice.
//!
//! # Where this comes from in the real engine
//!
//! The real per-world state keeps exactly two of these tick queues, one
//! keyed by block type and one by fluid type, and drains both once per
//! world tick, **block before fluid**, each capped at a `65536`
//! ticks-per-tick ceiling — see [`ScheduledTickQueue::drain_due`]'s
//! `max_to_process` parameter for that same cap.
//!
//! The real engine partitions each queue further, per chunk, via a
//! per-world container holding one per-chunk tick container
//! per loaded chunk, so that draining one very full
//! chunk cannot starve every other chunk's due ticks in the same pass. This
//! crate has no per-chunk tick-container registry yet (no chunk-load/unload
//! lifecycle for blocks — see `crate::chunk`'s own module doc), so
//! [`ScheduledTickQueue`] is the **single-container reduction** of the real
//! design: with exactly one container, the real per-world container's own
//! cross-container
//! selection (comparing containers by their own intra-tick drain order)
//! never has
//! a second container to compare against, and the whole algorithm collapses
//! to draining the single container's own queue in
//! the real drain order — which is exactly what this type does. This
//! is not an invented simplification: it is the real algorithm evaluated at
//! the case this crate is actually in today. If a per-chunk tick-container
//! registry is ever added, promote this to one `ScheduledTickQueue` per
//! chunk plus the per-world container's cross-container merge; the ordering
//! contract below does not change.
//!
//! # The ordering contract, transcribed from the real engine
//!
//! The real drain-order comparator, transcribed as the rule it implements:
//! compare by trigger tick first; if those are equal, compare by priority;
//! if those are also equal, compare by insertion order.
//!
//! i.e. **trigger tick, then priority, then insertion order** — see
//! [`TickPriority`] for the seven priorities and
//! [`ScheduledTickQueue::drain_due`] for the collect-then-run split that
//! keeps a tick scheduled *during* processing out of the pass currently
//! running (the real per-chunk-container drain fully
//! collects everything due before running any of the collected ticks' own
//! callback, even once).
//!
//! # The per-position dedup, transcribed from the real engine
//!
//! The real per-chunk container's schedule call, transcribed as the rule it
//! implements: only actually enqueue the new tick if adding it to the
//! per-position set reports that it was not already present.
//!
//! That per-position set hashes/compares only on `(pos, type)` — a second
//! `schedule` for a position/kind pair that already has one pending is
//! silently dropped, regardless of the new call's `trigger_tick` or
//! `priority`. [`ScheduledTickQueue::schedule`] mirrors this exactly.

use std::collections::{BTreeMap, BinaryHeap, HashSet};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Mirrors the real per-tick priority enum, in its real declared order:
/// extremely high, very high, high, normal, low, very low, extremely low —
/// each also carrying a signed numeric value (`-3` through `3`) that this
/// port does not need to reproduce.
///
/// Declared in that exact order deliberately: the real drain-order
/// comparator
/// compares priorities by their enum ordinal (declaration order), not the
/// `-3..3` values mentioned above. Rust's derived
/// [`Ord`] on an enum is likewise declaration-order, so listing the variants
/// `ExtremelyHigh` first reproduces that comparison with no explicit value
/// mapping needed — a smaller `TickPriority` here is exactly a smaller
/// `TickPriority` there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TickPriority {
    ExtremelyHigh,
    VeryHigh,
    High,
    Normal,
    Low,
    VeryLow,
    ExtremelyLow,
}

impl Default for TickPriority {
    /// The real 3-arg scheduled-tick constructor
    /// defaults to `NORMAL` for callers that
    /// don't care about priority.
    fn default() -> Self {
        TickPriority::Normal
    }
}

/// One scheduled tick, mirroring the real per-tick record: a type, a
/// position, a trigger tick, a priority, and a sub-tick insertion order.
///
/// `sub_tick_order` is a queue-assigned monotonic counter (the real
/// implementation's own
/// field of the same name) — the final tiebreaker when two ticks share both
/// `trigger_tick` and `priority`, and it is what makes
/// [`ScheduledTickQueue::drain_due`]'s output order a pure function of
/// *schedule call order*, not of a `HashMap`/`HashSet`'s iteration order
/// (CLAUDE.md's own warning: this queue never iterates a hash collection
/// where output order matters — the dedup set below is membership-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTick<T> {
    pub pos: (i32, i32, i32),
    pub kind: T,
    pub trigger_tick: u64,
    pub priority: TickPriority,
    sub_tick_order: u64,
}

/// One pending tick in the native-storage handoff.
///
/// Unlike [`ScheduledTick`], this record deliberately exposes the global
/// insertion sequence. A native reopen must restore that value rather than
/// assigning a fresh one, because it is the final tie-breaker when entries in
/// different chunk-owned queues share a trigger tick and priority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedScheduledTick {
    pub pos: (i32, i32, i32),
    pub kind: String,
    pub trigger_tick: u64,
    pub priority: TickPriority,
    pub insertion_order: u64,
}

/// `BinaryHeap` wrapper implementing the real drain order
/// with the comparison inverted, so
/// `BinaryHeap` (a max-heap) pops the real-engine-*smallest* entry first — i.e.
/// a genuine min-heap by `(trigger_tick, priority, sub_tick_order)`.
#[derive(Debug)]
struct HeapEntry<T>(ScheduledTick<T>);

impl<T> PartialEq for HeapEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.trigger_tick == other.0.trigger_tick
            && self.0.priority == other.0.priority
            && self.0.sub_tick_order == other.0.sub_tick_order
    }
}
impl<T> Eq for HeapEntry<T> {}

impl<T> PartialOrd for HeapEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for HeapEntry<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_key = (self.0.trigger_tick, self.0.priority, self.0.sub_tick_order);
        let other_key = (other.0.trigger_tick, other.0.priority, other.0.sub_tick_order);
        // Reversed on purpose (`other` compared against `self`): see the
        // struct doc comment above for why this turns `BinaryHeap`'s
        // max-heap into the real drain order's min-first semantics.
        other_key.cmp(&self_key)
    }
}

/// The single-container reduction of the real per-world tick container —
/// see this
/// module's own doc comment for why one container is the faithful case to
/// model today, and what changes (nothing about the ordering contract) if a
/// per-chunk registry is added later.
///
/// `T` is the tick's payload — the block/fluid *kind* being ticked (this
/// crate keys it by canonical block-state-name `String`, matching
/// `ChunkColumn`'s own block representation; the real engine keys by the
/// block/fluid
/// registry object). `T: Eq + Hash + Clone` is required for the
/// `(pos, kind)` dedup set — see [`schedule`](Self::schedule).
#[derive(Debug)]
pub struct ScheduledTickQueue<T> {
    heap: BinaryHeap<HeapEntry<T>>,
    scheduled: HashSet<((i32, i32, i32), T)>,
    next_sub_tick_order: u64,
}

impl<T> Default for ScheduledTickQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ScheduledTickQueue<T> {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            scheduled: HashSet::new(),
            next_sub_tick_order: 0,
        }
    }

    /// Number of ticks currently pending (not yet drained).
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// `true` iff nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

impl<T: Eq + Hash + Clone> ScheduledTickQueue<T> {
    /// Schedules `kind` at `pos` to run at `trigger_tick`, at `priority`.
    ///
    /// Returns `false` (a no-op) if a tick for the same `(pos, kind)` is
    /// already pending — mirrors `LevelChunkTicks::schedule`'s
    /// `ticksPerPosition.add(tick)` dedup keyed on `(pos, type)` only, per
    /// this module's own doc comment. The *new* call's `trigger_tick`/
    /// `priority` are discarded in that case, exactly like vanilla: the
    /// tick already in the queue keeps its original scheduling.
    pub fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        let key = (pos, kind.clone());
        if !self.scheduled.insert(key) {
            return false;
        }
        let sub_tick_order = self.next_sub_tick_order;
        self.next_sub_tick_order += 1;
        self.heap.push(HeapEntry(ScheduledTick {
            pos,
            kind,
            trigger_tick,
            priority,
            sub_tick_order,
        }));
        true
    }

    /// Inserts a tick with an order assigned by a containing queue.
    ///
    /// A chunk-local container must not restart the final drain-order
    /// tiebreaker for each chunk. Its world-level owner supplies one sequence
    /// shared by every contained queue through this internal entry point.
    fn schedule_with_sub_tick_order(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
        sub_tick_order: u64,
    ) -> bool {
        let key = (pos, kind.clone());
        if !self.scheduled.insert(key) {
            return false;
        }
        self.heap.push(HeapEntry(ScheduledTick {
            pos,
            kind,
            trigger_tick,
            priority,
            sub_tick_order,
        }));
        true
    }

    /// `true` iff a tick for `(pos, kind)` is currently pending — mirrors
    /// the real per-chunk container's own has-scheduled-tick query.
    #[must_use]
    pub fn has_scheduled(&self, pos: (i32, i32, i32), kind: &T) -> bool {
        self.scheduled.contains(&(pos, kind.clone()))
    }

    /// Every pending tick, in **heap order — not due order**.
    ///
    /// **Non-destructive**, which is the entire reason it exists: [`drain_due`]
    /// is the only other way to see the contents and it *removes* them, so
    /// saving the world through it would desync the running server from what
    /// lands on disk. This accessor completes the persistence handoff.
    ///
    /// Two schema traps for whoever serialises these, both measured against
    /// 4,023 real chunks read with an independent parser, and both of
    /// which the decompiled source reads misleadingly:
    ///
    /// * the real save format's `p` field is an `Int` carrying the priority **value** in `-3..3`,
    ///   **not** an ordinal. Our [`TickPriority`] is declaration-ordered so its
    ///   `Ord` matches the real engine's own priority comparison, which makes `Normal`'s ordinal `3`
    ///   and its value `0` — writing the ordinal would silently turn every
    ///   normal tick into `EXTREMELY_LOW`.
    /// * the real save format's `t` field is a delay **relative to game time at save, and can be
    ///   negative** (`-33` observed for an overdue lava tick). Loading is
    ///   `trigger_tick = game_time_at_load + delay`; an unsigned conversion
    ///   panics or wraps.
    ///
    /// [`drain_due`]: Self::drain_due
    pub fn iter(&self) -> impl Iterator<Item = &ScheduledTick<T>> {
        self.heap.iter().map(|entry| &entry.0)
    }

    /// Drains every tick due at or before `current_tick` (`trigger_tick <=
    /// current_tick`), in the real drain order, up to `max_to_process` entries —
    /// mirrors the real per-world tick drain's own `65536` cap.
    ///
    /// Every returned entry is removed from the queue (and its dedup key)
    /// **before** this function returns — the whole `Vec` is popped from the
    /// heap in one pass, mirroring the real per-world container's own collect-then-run split
    /// (collect everything due before running any of it). So a tick the caller schedules while
    /// iterating the returned `Vec` (a common shape: "processing this
    /// scheduled tick causes a fresh one to be scheduled") is invisible to
    /// *this* call regardless of its `trigger_tick` — it can only be seen by
    /// a subsequent `drain_due` call. This is the "ticks scheduled during
    /// this tick's processing must not be processed in the same pass"
    /// invariant required by the queue's processing contract.
    pub fn drain_due(&mut self, current_tick: u64, max_to_process: usize) -> Vec<ScheduledTick<T>> {
        let mut out = Vec::new();
        while out.len() < max_to_process {
            match self.heap.peek() {
                Some(top) if top.0.trigger_tick <= current_tick => {
                    let entry = self.heap.pop().expect("just peeked Some").0;
                    self.scheduled.remove(&(entry.pos, entry.kind.clone()));
                    out.push(entry);
                }
                _ => break,
            }
        }
        out
    }

    fn peek_due(&self, current_tick: u64) -> Option<&ScheduledTick<T>> {
        self.heap
            .peek()
            .map(|entry| &entry.0)
            .filter(|tick| tick.trigger_tick <= current_tick)
    }

    fn pop_due(&mut self, current_tick: u64) -> Option<ScheduledTick<T>> {
        let due = self.peek_due(current_tick).is_some();
        due.then(|| {
            let entry = self.heap.pop().expect("just observed a due scheduled tick").0;
            self.scheduled.remove(&(entry.pos, entry.kind.clone()));
            entry
        })
    }

    /// Removes and returns the first pending tick at `pos` whose `kind`
    /// satisfies `matches`, regardless of `trigger_tick` — i.e. **interrupts**
    /// a pending tick rather than waiting for it to come due.
    ///
    /// This is the queue-side half of the real moving-piston block entity's
    /// own final-tick and pre-remove-side-effects hooks reaching a moving block entity
    /// *before* its own scheduled commit fires: `crate::piston`'s pending
    /// commit lives in this queue (see `piston::finish_kind`'s own doc
    /// comment — there is no per-position block-entity map on this reaction
    /// surface, so "read the moving block entity at this cell" already means
    /// "find the pending commit at this cell"), so interrupting it means
    /// removing that entry from *here*, not from a store that does not
    /// exist. `kind` is matched by predicate rather than equality because a
    /// piston's finish kind is a full serialised record
    /// ([`super::piston::finish_kind`]), not a fixed string — the same
    /// reason [`Self::has_scheduled`] cannot be reused for this lookup.
    ///
    /// `BinaryHeap` has no O(log n) removal for an interior element, so this
    /// is O(n) in the number of pending ticks — bounded by how much redstone
    /// is mid-flight, never by world size, and the same cost class
    /// `drain_due`'s own `Vec` pop-and-rebuild already pays per call.
    pub fn take_matching(
        &mut self,
        pos: (i32, i32, i32),
        mut matches: impl FnMut(&T) -> bool,
    ) -> Option<ScheduledTick<T>> {
        let entries: Vec<ScheduledTick<T>> = std::mem::take(&mut self.heap)
            .into_vec()
            .into_iter()
            .map(|e| e.0)
            .collect();
        let mut found = None;
        let mut rebuilt = Vec::with_capacity(entries.len());
        for entry in entries {
            if found.is_none() && entry.pos == pos && matches(&entry.kind) {
                self.scheduled.remove(&(entry.pos, entry.kind.clone()));
                found = Some(entry);
            } else {
                rebuilt.push(HeapEntry(entry));
            }
        }
        self.heap = BinaryHeap::from(rebuilt);
        found
    }
}

/// A scheduled-tick sink used by a tick body that may schedule its own next
/// pass. `ScheduledTickQueue` is the single-container reduction used by block
/// ticks; [`ChunkScheduledTickQueue`] is the chunk-local fluid implementation.
pub trait ScheduledTickSink<T> {
    /// Schedules one pending tick, returning `false` for the normal
    /// per-position deduplication case.
    fn schedule_tick(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool;
}

/// The scheduled-block-tick operations a reaction may need from the owner of
/// the position it is changing.
///
/// Block reactions are allowed to schedule, inspect, and cancel pending work:
/// the latter is how a reversing piston removes its own uncommitted arm while
/// leaving the carried block's finish tick intact.  Keeping those operations
/// behind this boundary lets the world queue route each record to its column
/// without making redstone or piston code know which column owns it.
///
/// The world still assigns the insertion sequence.  A reaction therefore
/// hands work back to this interface instead of retaining another column's
/// queue or choosing a local order itself.
pub trait ScheduledTickQueueAccess<T> {
    /// Schedules one tick, returning `false` for the normal deduplication
    /// case.
    fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool;

    /// Whether `pos` owns a pending tick of `kind`.
    fn has_scheduled(&self, pos: (i32, i32, i32), kind: &T) -> bool;

    /// Finds a pending tick at `pos` without exposing its owner's whole queue.
    fn matching_at(
        &self,
        pos: (i32, i32, i32),
        matches: impl FnMut(&T) -> bool,
    ) -> Option<&ScheduledTick<T>>;

    /// Removes the first pending tick at `pos` whose kind matches.
    fn take_matching(
        &mut self,
        pos: (i32, i32, i32),
        matches: impl FnMut(&T) -> bool,
    ) -> Option<ScheduledTick<T>>;
}

impl<T: Eq + Hash + Clone> ScheduledTickSink<T> for ScheduledTickQueue<T> {
    fn schedule_tick(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        self.schedule(pos, kind, trigger_tick, priority)
    }
}

impl<T: Eq + Hash + Clone> ScheduledTickQueueAccess<T> for ScheduledTickQueue<T> {
    fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        Self::schedule(self, pos, kind, trigger_tick, priority)
    }

    fn has_scheduled(&self, pos: (i32, i32, i32), kind: &T) -> bool {
        Self::has_scheduled(self, pos, kind)
    }

    fn matching_at(
        &self,
        pos: (i32, i32, i32),
        mut matches: impl FnMut(&T) -> bool,
    ) -> Option<&ScheduledTick<T>> {
        self.iter().find(|tick| tick.pos == pos && matches(&tick.kind))
    }

    fn take_matching(
        &mut self,
        pos: (i32, i32, i32),
        matches: impl FnMut(&T) -> bool,
    ) -> Option<ScheduledTick<T>> {
        Self::take_matching(self, pos, matches)
    }
}

/// A world-owned collection of chunk-local scheduled-tick queues.
///
/// Every position is routed to the queue for its containing chunk. The outer
/// owner assigns the sub-tick sequence, then selects the earliest due head
/// across all chunk queues. That preserves the established global order
/// `(trigger tick, priority, insertion order)` even when a fluid update in one
/// column schedules its next update across a chunk border. The current tick
/// task still executes that selected order serially; this is a storage and
/// hand-off boundary, not permission to run columns concurrently.
#[derive(Debug)]
pub struct ChunkScheduledTickQueue<T> {
    queues: BTreeMap<(i32, i32), ScheduledTickQueue<T>>,
    next_sub_tick_order: u64,
}

impl<T> Default for ChunkScheduledTickQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ChunkScheduledTickQueue<T> {
    /// An empty world-owned collection of chunk queues.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
            next_sub_tick_order: 0,
        }
    }

    /// Number of pending ticks across all local queues.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queues.values().map(ScheduledTickQueue::len).sum()
    }

    /// `true` iff no chunk owns a pending tick.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    fn chunk_for(pos: (i32, i32, i32)) -> (i32, i32) {
        (pos.0.div_euclid(16), pos.2.div_euclid(16))
    }
}

impl<T: Eq + Hash + Clone> ChunkScheduledTickQueue<T> {
    /// Routes a tick to the queue for `pos`'s chunk while assigning an order
    /// from the single world-wide sequence.
    pub fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        let chunk = Self::chunk_for(pos);
        let queue = self.queues.entry(chunk).or_default();
        let order = self.next_sub_tick_order;
        let scheduled = queue.schedule_with_sub_tick_order(pos, kind, trigger_tick, priority, order);
        if scheduled {
            self.next_sub_tick_order += 1;
        }
        scheduled
    }

    fn restore(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
        insertion_order: u64,
    ) -> bool {
        let chunk = Self::chunk_for(pos);
        let scheduled = self.queues.entry(chunk).or_default().schedule_with_sub_tick_order(
            pos,
            kind,
            trigger_tick,
            priority,
            insertion_order,
        );
        if scheduled {
            self.next_sub_tick_order = self
                .next_sub_tick_order
                .max(insertion_order.saturating_add(1));
        }
        scheduled
    }

    /// `true` iff `pos`'s local queue holds a pending tick of `kind`.
    #[must_use]
    pub fn has_scheduled(&self, pos: (i32, i32, i32), kind: &T) -> bool {
        self.queues
            .get(&Self::chunk_for(pos))
            .is_some_and(|queue| queue.has_scheduled(pos, kind))
    }

    /// Every pending tick across every local queue. The iterator order is not
    /// drain order; callers that need execution order must use
    /// [`drain_due`](Self::drain_due).
    pub fn iter(&self) -> impl Iterator<Item = &ScheduledTick<T>> {
        self.queues.values().flat_map(ScheduledTickQueue::iter)
    }

    /// Drains due ticks in the world-wide ordering contract, while keeping the
    /// pending records physically owned by their chunk until this point.
    pub fn drain_due(&mut self, current_tick: u64, max_to_process: usize) -> Vec<ScheduledTick<T>> {
        let mut out = Vec::new();
        while out.len() < max_to_process {
            let next_chunk = self
                .queues
                .iter()
                .filter_map(|(&chunk, queue)| queue.peek_due(current_tick).map(|tick| (chunk, tick)))
                .min_by_key(|(_, tick)| (tick.trigger_tick, tick.priority, tick.sub_tick_order))
                .map(|(chunk, _)| chunk);
            let Some(chunk) = next_chunk else {
                break;
            };
            let (tick, empty) = {
                let queue = self
                    .queues
                    .get_mut(&chunk)
                    .expect("selected chunk must still have a scheduled queue");
                let tick = queue
                    .pop_due(current_tick)
                    .expect("selected chunk must still have the due head");
                (tick, queue.is_empty())
            };
            if empty {
                self.queues.remove(&chunk);
            }
            out.push(tick);
        }
        out
    }

    /// Removes the first matching tick in `pos`'s local queue.
    pub fn take_matching(
        &mut self,
        pos: (i32, i32, i32),
        matches: impl FnMut(&T) -> bool,
    ) -> Option<ScheduledTick<T>> {
        let chunk = Self::chunk_for(pos);
        let (tick, empty) = {
            let queue = self.queues.get_mut(&chunk)?;
            let tick = queue.take_matching(pos, matches);
            (tick, queue.is_empty())
        };
        if empty {
            self.queues.remove(&chunk);
        }
        tick
    }
}

impl<T: Eq + Hash + Clone> ScheduledTickSink<T> for ChunkScheduledTickQueue<T> {
    fn schedule_tick(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        self.schedule(pos, kind, trigger_tick, priority)
    }
}

impl<T: Eq + Hash + Clone> ScheduledTickQueueAccess<T> for ChunkScheduledTickQueue<T> {
    fn schedule(
        &mut self,
        pos: (i32, i32, i32),
        kind: T,
        trigger_tick: u64,
        priority: TickPriority,
    ) -> bool {
        Self::schedule(self, pos, kind, trigger_tick, priority)
    }

    fn has_scheduled(&self, pos: (i32, i32, i32), kind: &T) -> bool {
        Self::has_scheduled(self, pos, kind)
    }

    fn matching_at(
        &self,
        pos: (i32, i32, i32),
        mut matches: impl FnMut(&T) -> bool,
    ) -> Option<&ScheduledTick<T>> {
        self.queues
            .get(&Self::chunk_for(pos))?
            .iter()
            .find(|tick| tick.pos == pos && matches(&tick.kind))
    }

    fn take_matching(
        &mut self,
        pos: (i32, i32, i32),
        matches: impl FnMut(&T) -> bool,
    ) -> Option<ScheduledTick<T>> {
        Self::take_matching(self, pos, matches)
    }
}

/// The world's two scheduled-tick queues, plus the game tick their
/// `trigger_tick`s are measured against — shared, so the save path can read
/// them.
///
/// # Why this type exists rather than the queues being locals
///
/// `tick::run_tick_loop` owned both queues as local `let mut` bindings, which
/// made them unreachable from anywhere else: the only non-destructive way to
/// see a queue's contents is [`ScheduledTickQueue::iter`], and no reference to
/// the queue escaped the function. So `chunk_nbt` wrote an empty `block_ticks`
/// list for every chunk and a pending redstone or fluid tick was lost on quit.
///
/// # The game tick lives here, and that is deliberate
///
/// Saving a tick means writing `delay = trigger_tick - game_time_at_save`, so
/// the save path needs the same counter the queues were scheduled against.
/// Taking it from a second source is `SET_TIME`'s scar exactly — it decoded,
/// darkened the sky, and carried wall-clock elapsed-since-join while
/// `tick.rs`'s real counter never reached the encoder, with every link in the
/// wire green. So the tick loop stores its own `game_tick` here, one relaxed
/// atomic store per tick, and the save path reads that.
///
/// # Why it lives here and not in `region_source`
///
/// It used to live beside the Anvil save path that reads it, which put a
/// **portable** type — two `Arc`s over the queues in this module, no I/O and no
/// clock — inside a `cfg(not(target_arch = "wasm32"))` module. `ChunkSource`'s
/// `world_registries` accessor names it in an ungated struct field, so the
/// browser build stopped compiling the moment anything referenced it. The
/// native-only half is the *store behind* the handle, not the handle: the two
/// methods that speak `chunk_nbt::SavedTick`
/// ([`saved_ticks_for`](crate::region_source) and its siblings) stay in
/// `region_source` as a second inherent `impl` block, which is where the save
/// format is. `region_source` re-exports this type, so every native call site
/// that spells `crate::region_source::ScheduledTickHandle` is unchanged.
#[derive(Debug, Clone, Default)]
pub struct ScheduledTickHandle {
    queues: Arc<Mutex<ScheduledTickQueues>>,
    game_tick: Arc<AtomicU64>,
    /// Ticks read off a chunk that has not been merged into `queues` yet. See
    /// [`ScheduledTickHandle::stage`] — this exists so a chunk load can hand
    /// its ticks over **without taking `queues`**, because the thread doing
    /// the load is very often the tick thread, already inside [`Self::with`].
    staged: Arc<Mutex<Vec<StagedTick>>>,
}

/// One tick handed to [`ScheduledTickHandle::stage`] by a chunk load, waiting
/// for the next [`ScheduledTickHandle::with`] to merge it into the live queues.
///
/// Carries an **absolute** `trigger_tick`, already rebased by the loader, so
/// merging is a plain `schedule` call with no clock reading — the staging delay
/// therefore cannot shift when a restored tick fires.
#[derive(Debug, Clone)]
pub struct StagedTick {
    /// Block position, as `ScheduledTick::pos`.
    pub pos: (i32, i32, i32),
    /// The tick kind, as `ScheduledTick::kind`.
    pub kind: String,
    /// Absolute trigger tick, already rebased onto the loading session's clock.
    pub trigger_tick: u64,
    /// As `ScheduledTick::priority`.
    pub priority: TickPriority,
    /// `true` for the real per-world fluid-tick queue, `false` for the
    /// block-tick queue.
    pub fluid: bool,
    /// The saved global insertion sequence, when this tick came from the
    /// typed native record path. Anvil restores have no such field and are
    /// deliberately assigned a fresh sequence at the next merge.
    pub insertion_order: Option<u64>,
}

/// The pair of queues [`ScheduledTickHandle`] guards, mirroring the real
/// per-world block-tick and fluid-tick queues.
#[derive(Debug, Default)]
pub struct ScheduledTickQueues {
    /// Column-owned block ticks, merged in the one world-wide drain order.
    ///
    /// Reactions use [`ScheduledTickQueueAccess`] for their explicit hand-off
    /// back to this owner, preserving piston cancellation as well as ordinary
    /// redstone rescheduling across a column boundary.
    pub block: ChunkScheduledTickQueue<String>,
    /// The real per-world fluid-tick queue.
    pub fluid: ChunkScheduledTickQueue<String>,
}

impl ScheduledTickHandle {
    /// A handle onto a fresh, empty pair of queues.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` against both locked queues, returning its result.
    ///
    /// Both at once, and **synchronously**, for two reasons. Handing over both
    /// lets `tick::run_tick_loop` wrap its existing scheduled-tick section in
    /// one closure with every use site textually unchanged, so the wiring diff
    /// is a wrapper rather than a rewrite of code the redstone work is actively
    /// editing. And a closure cannot contain an `.await`, so the compiler — not
    /// a reviewer — guarantees the guard is never held across a suspension
    /// point, which would make the tick task non-`Send`.
    ///
    /// Same shape as [`crate::BlockEntityHandle::with`], and a poisoned lock is
    /// a bug rather than a recoverable condition, for the same reason.
    ///
    /// **Merges [`stage`](Self::stage)d ticks first**, so nothing can observe
    /// the queues in a state where a loaded chunk's ticks are staged but not
    /// scheduled: every read of the queues in the crate goes through here.
    pub fn with<R>(&self, f: impl FnOnce(&mut ScheduledTickQueues) -> R) -> R {
        let _queues_order =
            crate::lock_order::acquire(crate::lock_order::LockClass::ScheduledQueues);
        let mut guard = self.queues.lock().expect("scheduled tick lock poisoned");
        // Lock order is always `queues` then `staged`, never the reverse —
        // `stage` takes `staged` alone. Inverting it here would reintroduce a
        // deadlock of a different shape.
        let staged: Vec<StagedTick> = {
            let _staged_order =
                crate::lock_order::acquire(crate::lock_order::LockClass::ScheduledStaged);
            let mut pending = self.staged.lock().expect("staged tick lock poisoned");
            if pending.is_empty() {
                Vec::new()
            } else {
                std::mem::take(&mut *pending)
            }
        };
        for tick in staged {
            if tick.fluid {
                if let Some(order) = tick.insertion_order {
                    guard
                        .fluid
                        .restore(tick.pos, tick.kind, tick.trigger_tick, tick.priority, order);
                } else {
                    guard
                        .fluid
                        .schedule(tick.pos, tick.kind, tick.trigger_tick, tick.priority);
                }
            } else {
                if let Some(order) = tick.insertion_order {
                    guard
                        .block
                        .restore(tick.pos, tick.kind, tick.trigger_tick, tick.priority, order);
                } else {
                    guard
                        .block
                        .schedule(tick.pos, tick.kind, tick.trigger_tick, tick.priority);
                }
            }
        }
        f(&mut guard)
    }

    /// Hands `ticks` over to the queues **without locking them**, returning how
    /// many were staged. They become visible at the next [`Self::with`].
    ///
    /// # Why a chunk load may not simply take the lock
    ///
    /// `tick::run_tick_loop` holds the queues for its entire scheduled-tick and
    /// random-tick section, and that section calls `world.column`,
    /// `world.block_state` and `world.set_block`. On a persistent world any of
    /// those can reach `region_source::RegionChunkSource::load`, which restores
    /// the loaded chunk's saved ticks — so the tick thread would re-enter its
    /// own [`Self::with`] and a `std::sync::Mutex` is not reentrant. That is a
    /// **self**-deadlock: deterministic, total, and reached the moment the world
    /// tick first touches a column that exists on disk. A world with no region
    /// files never reaches it, which is why only *saved* worlds hung.
    ///
    /// Deferring rather than `try_lock`-ing is deliberate: a `try_lock` fast
    /// path would make the merge point depend on lock contention, so the same
    /// world would restore its ticks at a different moment from run to run.
    pub fn stage(&self, ticks: Vec<StagedTick>) -> u64 {
        if ticks.is_empty() {
            return 0;
        }
        let count = ticks.len() as u64;
        let _order = crate::lock_order::acquire(crate::lock_order::LockClass::ScheduledStaged);
        self.staged
            .lock()
            .expect("staged tick lock poisoned")
            .extend(ticks);
        count
    }

    /// Snapshots both queues' pending records belonging to one chunk column.
    ///
    /// The result is sorted by the one world-wide insertion sequence rather
    /// than container or heap iteration order. This is non-destructive and is
    /// the only native-storage read boundary for scheduled ticks.
    #[must_use]
    pub fn snapshot_column(
        &self,
        column_x: i32,
        column_z: i32,
    ) -> (Vec<PersistedScheduledTick>, Vec<PersistedScheduledTick>) {
        self.with(|queues| {
            let snapshot = |queue: &ChunkScheduledTickQueue<String>| {
                let mut ticks: Vec<_> = queue
                    .iter()
                    .filter(|tick| {
                        (tick.pos.0.div_euclid(16), tick.pos.2.div_euclid(16))
                            == (column_x, column_z)
                    })
                    .map(|tick| PersistedScheduledTick {
                        pos: tick.pos,
                        kind: tick.kind.clone(),
                        trigger_tick: tick.trigger_tick,
                        priority: tick.priority,
                        insertion_order: tick.sub_tick_order,
                    })
                    .collect();
                ticks.sort_by_key(|tick| tick.insertion_order);
                ticks
            };
            (snapshot(&queues.block), snapshot(&queues.fluid))
        })
    }

    /// Stages typed native records for their next queue merge without changing
    /// their stored global insertion order.
    pub fn stage_persisted(
        &self,
        block: Vec<PersistedScheduledTick>,
        fluid: Vec<PersistedScheduledTick>,
    ) -> u64 {
        let ticks = block
            .into_iter()
            .map(|tick| StagedTick {
                pos: tick.pos,
                kind: tick.kind,
                trigger_tick: tick.trigger_tick,
                priority: tick.priority,
                fluid: false,
                insertion_order: Some(tick.insertion_order),
            })
            .chain(fluid.into_iter().map(|tick| StagedTick {
                pos: tick.pos,
                kind: tick.kind,
                trigger_tick: tick.trigger_tick,
                priority: tick.priority,
                fluid: true,
                insertion_order: Some(tick.insertion_order),
            }))
            .collect();
        self.stage(ticks)
    }

    /// Records the tick the queues' `trigger_tick`s are relative to.
    ///
    /// Called once per tick by `tick::run_tick_loop`. A single relaxed atomic
    /// store — the cost on the tick thread is a count of one, no I/O and no
    /// encoding, which is the property `docs/world-open-latency.md` exists to
    /// protect.
    pub fn set_game_tick(&self, tick: u64) {
        self.game_tick.store(tick, Ordering::Relaxed);
    }

    /// The last tick recorded by [`set_game_tick`](Self::set_game_tick), or `0`
    /// if the loop has not run — which is correct for a world that is saved
    /// before its first tick.
    #[must_use]
    pub fn game_tick(&self) -> u64 {
        self.game_tick.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Predicted drain order for a hand-built set of five ticks spanning
    /// every tiebreaker `DRAIN_ORDER` defines: trigger tick first (so the
    /// tick due at 10 always precedes the ties due at 20 regardless of
    /// priority), then priority (`ExtremelyHigh` before `Normal` before
    /// `Low` among the trigger-tick-20 group), then insertion order for the
    /// two `Normal`-at-20 entries (B before D, since B was scheduled
    /// first).
    #[test]
    fn drains_in_trigger_tick_then_priority_then_insertion_order() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        q.schedule((0, 0, 0), "A-tick10-normal", 10, TickPriority::Normal);
        q.schedule((1, 0, 0), "B-tick20-normal", 20, TickPriority::Normal);
        q.schedule((2, 0, 0), "C-tick20-high", 20, TickPriority::ExtremelyHigh);
        q.schedule((3, 0, 0), "D-tick20-normal", 20, TickPriority::Normal);
        q.schedule((4, 0, 0), "E-tick20-low", 20, TickPriority::Low);

        let drained = q.drain_due(20, 100);
        let order: Vec<&str> = drained.iter().map(|t| t.kind).collect();
        assert_eq!(
            order,
            vec![
                "A-tick10-normal",
                "C-tick20-high",
                "B-tick20-normal",
                "D-tick20-normal",
                "E-tick20-low",
            ],
            "expected trigger_tick, then priority, then insertion order"
        );
    }

    /// Negative control for the assertion above: nothing due later than
    /// `current_tick` may be drained, proving `drain_due` isn't simply
    /// draining the whole heap regardless of `trigger_tick`.
    #[test]
    fn drain_due_never_returns_a_tick_scheduled_for_the_future() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        q.schedule((0, 0, 0), "due-now", 5, TickPriority::Normal);
        q.schedule((1, 0, 0), "not-due-yet", 6, TickPriority::Normal);

        let drained = q.drain_due(5, 100);
        assert_eq!(drained.len(), 1, "only the tick due at or before tick 5 may drain");
        assert_eq!(drained[0].kind, "due-now");
        assert_eq!(q.len(), 1, "the not-yet-due tick must remain queued");
    }

    /// Pins the dedup rule transcribed from the real per-chunk container: a second
    /// `schedule` for the same `(pos, kind)` while one is already pending is
    /// dropped, and the *original* scheduling (trigger tick, priority) wins.
    #[test]
    fn duplicate_schedule_for_same_pos_and_kind_is_a_no_op() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        assert!(q.schedule((0, 0, 0), "fire", 100, TickPriority::Normal));
        // Second call: same pos, same kind, different (earlier, higher-priority)
        // scheduling — must be rejected, and the ORIGINAL trigger_tick/priority
        // must be what actually drains.
        assert!(!q.schedule((0, 0, 0), "fire", 1, TickPriority::ExtremelyHigh));
        assert_eq!(q.len(), 1, "the duplicate must not have been added");

        // The original scheduling (tick 100) is what wins, not the rejected
        // one (tick 1) — draining at tick 1 must find nothing.
        assert!(q.drain_due(1, 100).is_empty());
        let drained = q.drain_due(100, 100);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].priority, TickPriority::Normal, "original priority must survive");
    }

    /// [`ScheduledTickQueue::take_matching`] removes only the entry whose
    /// `pos` **and** predicate both match, leaves every other pending tick —
    /// including a same-position tick of a *different* kind and a
    /// same-kind-shaped tick at a *different* position — untouched, and the
    /// removed entry no longer drains later. All three are collected and
    /// asserted together rather than three separate `assert!`s, since a
    /// version that also deletes a neighbour would only be caught by
    /// checking what is left, not by checking what was returned.
    #[test]
    fn take_matching_removes_only_the_one_entry_it_names() {
        let mut q: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        q.schedule((0, 0, 0), "redstone:piston_finish|south|true|true|x".to_string(), 50, TickPriority::Normal);
        q.schedule((0, 0, 0), "redstone:repeater".to_string(), 60, TickPriority::Normal);
        q.schedule((1, 0, 0), "redstone:piston_finish|south|true|true|x".to_string(), 70, TickPriority::Normal);

        let taken = q.take_matching((0, 0, 0), |k| k.starts_with("redstone:piston_finish"));
        assert!(taken.is_some(), "must find the matching entry at (0,0,0)");
        let taken = taken.unwrap();
        assert_eq!(taken.pos, (0, 0, 0));
        assert_eq!(taken.trigger_tick, 50);

        let mut remaining: Vec<((i32, i32, i32), String)> =
            q.iter().map(|t| (t.pos, t.kind.clone())).collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                ((0, 0, 0), "redstone:repeater".to_string()),
                ((1, 0, 0), "redstone:piston_finish|south|true|true|x".to_string()),
            ],
            "the same-position different-kind entry and the same-kind different-position entry \
             must both survive"
        );

        // The taken entry must no longer be reachable through the ordinary
        // drain path either -- proves this is a removal, not a copy.
        let drained = q.drain_due(1000, 100);
        assert_eq!(drained.len(), 2, "only the two untaken entries may still drain");
        assert!(
            drained.iter().all(|t| t.trigger_tick != 50),
            "the taken entry's trigger_tick must not appear in a later drain"
        );
    }

    /// Negative control: a predicate that matches nothing, or a position
    /// that holds nothing, returns `None` and leaves the queue exactly as it
    /// was -- proving `take_matching` cannot silently remove the wrong
    /// entry when its own premise (something is actually there) is false.
    #[test]
    fn take_matching_returns_none_and_leaves_the_queue_intact_when_nothing_matches() {
        let mut q: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        q.schedule((0, 0, 0), "redstone:repeater".to_string(), 50, TickPriority::Normal);

        assert!(q.take_matching((0, 0, 0), |k| k.starts_with("redstone:piston_finish")).is_none());
        assert!(q.take_matching((5, 5, 5), |_| true).is_none());
        assert_eq!(q.len(), 1, "the untouched entry must still be queued");
        assert!(q.has_scheduled((0, 0, 0), &"redstone:repeater".to_string()));
    }

    /// Negative control for the dedup test above, proving the detector
    /// actually discriminates on `kind`, not merely on `pos`: a second
    /// `schedule` at the **same position** but a **different** kind must
    /// succeed independently.
    #[test]
    fn different_kind_at_the_same_position_is_not_deduplicated() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        assert!(q.schedule((0, 0, 0), "block-kind", 10, TickPriority::Normal));
        assert!(
            q.schedule((0, 0, 0), "fluid-kind", 10, TickPriority::Normal),
            "a different kind at the same position must schedule independently — \
             dedup is keyed on (pos, kind), not pos alone"
        );
        assert_eq!(q.drain_due(10, 100).len(), 2);
    }

    /// The core "not in the same pass" control: draining once, then
    /// scheduling a fresh tick (already due) for each drained entry — as a
    /// real tick handler that reschedules itself would — must NOT have
    /// grown the `Vec` this call already returned. A queue that drained
    /// live (checking the heap after every push rather than snapshotting
    /// first) would fail this by returning more than the initial count.
    #[test]
    fn ticks_scheduled_while_processing_a_drain_do_not_appear_in_it() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        q.schedule((0, 0, 0), "seed-a", 10, TickPriority::Normal);
        q.schedule((1, 0, 0), "seed-b", 10, TickPriority::Normal);

        let drained = q.drain_due(10, 100);
        assert_eq!(drained.len(), 2, "setup: exactly the two seeded ticks must drain");

        // Simulate "processing seed-a re-schedules itself, already due" —
        // this must land in a FUTURE drain_due call, never retroactively in
        // the Vec above (already returned, already immutable).
        for tick in &drained {
            q.schedule(tick.pos, "rescheduled", 10, TickPriority::Normal);
        }
        assert_eq!(drained.len(), 2, "the already-returned Vec cannot grow");

        // And the rescheduled ticks are real, not lost: they drain on the
        // *next* call.
        let second_pass = q.drain_due(10, 100);
        assert_eq!(second_pass.len(), 2, "both rescheduled ticks must surface on the next pass");
    }

    /// Magnitude check on `max_to_process`, not just a sign check: five due
    /// ticks, capped at two, must yield *exactly* two — proving the cap is a
    /// hard limit, not merely "fewer than five" (which a wrong
    /// implementation returning e.g. four would also satisfy).
    #[test]
    fn max_to_process_is_a_hard_cap_not_a_loose_bound() {
        let mut q: ScheduledTickQueue<&'static str> = ScheduledTickQueue::new();
        for i in 0..5 {
            q.schedule((i, 0, 0), "x", 1, TickPriority::Normal);
        }
        let first = q.drain_due(1, 2);
        assert_eq!(first.len(), 2, "cap must be exact, not approximate");
        assert_eq!(q.len(), 3, "the other three remain queued");
        let second = q.drain_due(1, 2);
        assert_eq!(second.len(), 2);
        let third = q.drain_due(1, 2);
        assert_eq!(third.len(), 1, "the last one drains once the queue is nearly empty");
    }

    /// `TickPriority`'s declared order must match the real engine's `-3..3`
    /// value
    /// order — the property `HeapEntry::cmp`
    /// relies on via `#[derive(Ord)]`.
    #[test]
    fn tick_priority_declaration_order_matches_real_value_order() {
        assert!(TickPriority::ExtremelyHigh < TickPriority::VeryHigh);
        assert!(TickPriority::VeryHigh < TickPriority::High);
        assert!(TickPriority::High < TickPriority::Normal);
        assert!(TickPriority::Normal < TickPriority::Low);
        assert!(TickPriority::Low < TickPriority::VeryLow);
        assert!(TickPriority::VeryLow < TickPriority::ExtremelyLow);
    }

    /// Determinism control (CLAUDE.md's own warning: a determinism gate that
    /// calls the *same* instance twice proves memoisation, not determinism).
    /// Builds two genuinely independent queues from the same schedule script
    /// and asserts identical drain order from both.
    #[test]
    fn two_independently_built_queues_drain_identically() {
        fn build() -> ScheduledTickQueue<&'static str> {
            let mut q = ScheduledTickQueue::new();
            q.schedule((0, 0, 0), "a", 5, TickPriority::Low);
            q.schedule((1, 0, 0), "b", 5, TickPriority::High);
            q.schedule((2, 0, 0), "c", 3, TickPriority::Normal);
            q
        }
        let mut q1 = build();
        let mut q2 = build();
        let d1 = q1.drain_due(5, 100);
        let d2 = q2.drain_due(5, 100);
        let k1: Vec<&str> = d1.iter().map(|t| t.kind).collect();
        let k2: Vec<&str> = d2.iter().map(|t| t.kind).collect();
        assert_eq!(k1, k2);
        assert_eq!(k1, vec!["c", "b", "a"]);
    }

    /// The local queues must not let their map order replace the world-wide
    /// insertion tiebreaker. The first entry belongs to chunk `(1, 0)`, while
    /// the second belongs to `(0, 0)`; a drain ordered by chunk key would
    /// reverse them.
    #[test]
    fn chunk_local_queues_keep_global_insertion_order_across_a_column_border() {
        let mut q: ChunkScheduledTickQueue<&'static str> = ChunkScheduledTickQueue::new();
        assert!(q.schedule((16, 0, 0), "east-first", 10, TickPriority::Normal));
        assert!(q.schedule((15, 0, 0), "west-second", 10, TickPriority::Normal));

        let drained = q.drain_due(10, usize::MAX);
        let kinds: Vec<_> = drained.iter().map(|tick| tick.kind).collect();
        assert_eq!(kinds, ["east-first", "west-second"]);
    }

    /// Priority remains ahead of insertion order even when the contenders are
    /// physically held by different chunks. This is the second part of the
    /// cross-column ordering contract, distinct from the insertion control.
    #[test]
    fn chunk_local_queues_compare_priority_before_cross_column_insertion_order() {
        let mut q: ChunkScheduledTickQueue<&'static str> = ChunkScheduledTickQueue::new();
        assert!(q.schedule((16, 0, 0), "normal-first", 10, TickPriority::Normal));
        assert!(q.schedule((15, 0, 0), "high-second", 10, TickPriority::High));

        let drained = q.drain_due(10, usize::MAX);
        let kinds: Vec<_> = drained.iter().map(|tick| tick.kind).collect();
        assert_eq!(kinds, ["high-second", "normal-first"]);
    }

    /// Block reactions can hand work to either side of a column boundary,
    /// including across the negative-coordinate boundary, without selecting a
    /// local order. The world sequence remains the final tiebreaker even
    /// though the records are physically held by four different owners.
    #[test]
    fn cross_owner_block_handoff_keeps_global_order_on_both_sides_of_zero() {
        let mut q: ChunkScheduledTickQueue<&'static str> = ChunkScheduledTickQueue::new();
        assert_eq!(ChunkScheduledTickQueue::<&str>::chunk_for((16, 0, 0)), (1, 0));
        assert_eq!(ChunkScheduledTickQueue::<&str>::chunk_for((-1, 0, 0)), (-1, 0));
        assert_eq!(ChunkScheduledTickQueue::<&str>::chunk_for((-16, 0, 0)), (-1, 0));

        assert!(q.schedule((16, 0, 0), "east-first", 10, TickPriority::Normal));
        assert!(q.schedule((-1, 0, 0), "negative-second", 10, TickPriority::Normal));
        assert!(q.schedule((-16, 0, 0), "negative-third", 10, TickPriority::Normal));
        assert!(q.schedule((15, 0, 0), "home-fourth", 10, TickPriority::Normal));

        let drained = q.drain_due(10, usize::MAX);
        let kinds: Vec<_> = drained.iter().map(|tick| tick.kind).collect();
        assert_eq!(kinds, ["east-first", "negative-second", "negative-third", "home-fourth"]);
    }

    /// Positive and negative controls for the cancellation half of the block
    /// hand-off. A reversing piston removes only its matching pending arm from
    /// its position's owner; an unmatched cancellation must not erase the
    /// tick held by the neighbouring negative column.
    #[test]
    fn cross_owner_block_handoff_preserves_piston_cancellation_boundaries() {
        let mut q: ChunkScheduledTickQueue<String> = ChunkScheduledTickQueue::new();
        let arm = "redstone:piston_finish|east|true|true|x".to_string();
        let neighbour = "redstone:repeater".to_string();
        assert!(q.schedule((15, 5, 0), arm.clone(), 10, TickPriority::Normal));
        assert!(q.schedule((-1, 5, 0), neighbour.clone(), 10, TickPriority::Normal));

        assert!(q
            .take_matching((15, 5, 0), |kind| kind.starts_with("redstone:piston_finish"))
            .is_some());
        assert!(q.has_scheduled((-1, 5, 0), &neighbour));
        assert!(q
            .take_matching((-1, 5, 0), |kind| kind.starts_with("redstone:piston_finish"))
            .is_none());
        assert!(q.has_scheduled((-1, 5, 0), &neighbour));
        assert!(q.drain_due(10, usize::MAX).iter().all(|tick| tick.kind == neighbour));
    }
}
