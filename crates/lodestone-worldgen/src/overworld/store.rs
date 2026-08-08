//! The staged, sharded per-chunk store: worldgen's memoisation layer, with
//! **compute-exactly-once** as a structural property rather than a hope.
//!
//! # What it is
//!
//! A concurrent map from chunk position to a caller-defined *entry* holding one
//! [`StageSlot`] per intermediate product of that chunk (today: the pre-ore
//! world and the post-ore world; [`crate::overworld`] defines the shape).
//! Replaces the two `Mutex<HashMap + VecDeque>` FIFO caches
//! (`PreOreCache`/`PostOreCache`) that `overworld/mod.rs` carried until Unit 6
//! of `docs/plans/worldgen-rewrite.md`.
//!
//! # Why the old shape had to go, in one measurement
//!
//! `4307b59` is the revert that names it: *"Revert per-ring barrier removal —
//! cache contention with 289 concurrent generator calls."* A 289-column join
//! burst produced **~5,000 concurrent lock attempts on one `Arc<Mutex>`**, and
//! the fix at the time was to put a per-ring barrier back into
//! `lodestone-server` — a workaround for a workaround. Two properties of the old
//! caches caused it, and this module removes both:
//!
//! 1. **One global lock per cache.** Every lookup, every insert, and all the
//!    FIFO `VecDeque` bookkeeping went through a single mutex, so 289 workers
//!    serialised on it ~17 times each for pre-ore alone.
//! 2. **Racing misses recomputed.** Both caches deliberately released the lock
//!    across the computation (correct — never hold a lock across a pipeline) and
//!    therefore let two threads racing the same key both run the full pipeline.
//!    `pre_ore_stage`'s own comment admitted this: *"the work really was done
//!    twice, and is exactly the waste U6's store is meant to make impossible."*
//!
//! # How it works
//!
//! **Two levels of lock, and neither is global.**
//!
//! * **Shard level** — [`SHARD_COUNT`] independent `Mutex<HashMap<…>>`, selected
//!   by a mixing hash of the chunk position (see [`shard_of`]). A shard lock is
//!   held for exactly one `HashMap` probe, one `Arc` clone and one `u64` write:
//!   no allocation on the hit path, no computation ever. Adjacent chunks — the
//!   ones a 3×3 driver asks for together — land in *different* shards by
//!   construction, because the hash mixes both coordinates before masking.
//! * **Entry level** — each [`StageSlot`] is a `OnceLock<Arc<T>>`. A hit is an
//!   atomic load and an `Arc` bump with **no lock at all**. A miss runs the
//!   computation inside `OnceLock::get_or_init`, which guarantees the closure
//!   runs **exactly once** across all threads for the lifetime of the slot: a
//!   second thread arriving mid-computation *waits for the value* instead of
//!   computing its own copy. Two workers therefore contend only when they need
//!   the same chunk's same stage at the same instant, which is a real dependency
//!   edge in the generation graph, not incidental sharing.
//!
//! That is what turns the plan's acceptance criterion into an invariant: the
//! `pre_ore_computed`/`post_ore_computed` counters are bumped **inside** the
//! `get_or_init` closure, so over any sweep they equal the number of distinct
//! chunks reached at that stage, exactly — never more.
//!
//! # Eviction is view-scoped, and that is load-bearing
//!
//! The old caches evicted by **capacity FIFO** at 512. Capacity eviction is how
//! a "cache" silently starts recomputing, and the neighbouring failure —
//! two distinct chunks sharing one memoised value — is on the record in this
//! very crate: `pre_ore_stage`'s doc describes a *clamped-key* cache in
//! `FeatureOracle.java` that aliased two chunk coordinates onto one value and
//! hung a JVM oracle on a non-reentrant semaphore. So:
//!
//! * **Exact keys only.** [`ChunkPos`] is the literal `(cx, cz)` pair. Nothing
//!   here rounds, clamps, or merges a key, and there is no code path that could.
//! * **In-flight neighbourhoods are pinned, not merely favoured.** A top-level
//!   request opens a [`ViewScope`] over its own closure radius; every entry in
//!   that box is pinned for the request's lifetime and is *structurally
//!   ineligible* for eviction. Eviction therefore cannot cause a recompute
//!   inside a request — it is not a probability argument.
//! * **The retention ceiling is derived, not guessed.** See
//!   [`StagedStore::new`] and [`crate::overworld`]'s use of it: the floor is the
//!   pre-ore closure of the D4 join-burst scenario (a 17×17 = 289-column burst
//!   closes over 21×21 = 441 chunks), so neither that burst nor the 12×12
//!   parity sweep can evict anything. Beyond that ceiling — a session exploring
//!   a large world over hours, the case `PRE_ORE_CACHE_CAPACITY` was originally
//!   added for — the oldest **unpinned** entries are dropped, which is a memory
//!   bound on a cold tail rather than a policy the hot path can feel.
//!
//! Eviction is always *safe* regardless of policy: a slot's value is a pure
//! function of its key and the generator's own fixed state, so dropping one can
//! only ever cost a recompute, never a wrong answer. The counter is what makes
//! "never even costs a recompute" checkable.
//!
//! # How to change it
//!
//! * **To add a stage** (Unit 9's memoised per-source biome is the next one):
//!   add a [`StageSlot`] field to the entry type in `overworld/mod.rs`. Nothing
//!   in this file needs to know what the stages are — [`StagedStore`] is generic
//!   over the entry payload precisely so the store does not depend back on the
//!   generator.
//! * **Reentrancy is the one real trap.** `OnceLock::get_or_init` deadlocks if
//!   its own closure re-enters the *same slot*. The generator's call graph is
//!   strictly layered and must stay that way: `post_ore` may call `pre_ore`
//!   (any chunk), `pre_ore` calls nothing in the store, and no stage ever
//!   re-enters its own slot for its own chunk. If you add a stage, add it
//!   *above* the ones it consumes and never make a stage depend on itself.
//! * **Do not add a shared scratch pool here.** Any buffer reuse belongs in a
//!   `thread_local` free-list; a pool behind a lock would re-create precisely
//!   the contention this module exists to delete.
//!
//! # Configuration
//!
//! [`SHARD_COUNT`] (compile-time) and the retention ceiling passed to
//! [`StagedStore::new`]. Counters are the caller's business — this module bumps
//! nothing itself, so it stays independent of `crate::counters`.
//!
//! # Where this file lives, and why it is not `src/engine/store.rs`
//!
//! The rewrite plan's Unit 6 row names `src/engine/store.rs`, which would need a
//! `pub mod engine;` line in `lib.rs`. `lib.rs` was being rewritten by Unit 16's
//! concurrent leaf-crate extraction at the moment this landed, and `lib.rs` is a
//! choke point this repo has been burned on (CLAUDE.md: never rewrite a shared
//! file wholesale). Living under `overworld/` — a directory Unit 6 owns outright
//! — cost one line in a file this unit already had to edit and zero contention.
//!
//! Nothing about the code assumes this location: [`StagedStore`] names no
//! generator type. Relocating it under an `engine/` module once Unit 4 creates
//! one is a pure move plus a re-export, and `lodestone_worldgen::overworld::store`
//! is public today so Unit 10's server-side scheduler can drive it from here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// A chunk coordinate pair, used **exactly** as given — never rounded, clamped
/// or merged with a neighbour. See this module's doc on the clamped-key
/// aliasing incident that makes this worth a type alias and a sentence.
pub type ChunkPos = (i32, i32);

/// Number of independent shard locks.
///
/// Chosen as a power of two so [`shard_of`] can mask instead of divide, and
/// large enough that the D4 scenario's 289 concurrent columns spread thinly:
/// with 64 shards each column's ~26 store lookups hit an expected ~0.4 lookups
/// per shard per column. The critical section is a `HashMap` probe, so the
/// figure that matters is that it is *tens of nanoseconds against milliseconds
/// of generation* — raising this number cannot fix a design that holds a lock
/// across a computation, and this one does not.
pub const SHARD_COUNT: usize = 64;

/// Selects a shard for `pos`.
///
/// Mixes **both** coordinates through distinct odd multipliers and reads the
/// **high** bits, so the chunks a 3×3 or 5×5 driver requests together are
/// scattered across shards rather than sharing one. A naive
/// `(cx ^ cz) & mask` would map whole diagonals onto a single shard, which is
/// the access pattern this store is built for.
#[must_use]
pub fn shard_of(pos: ChunkPos) -> usize {
    // Fibonacci-hashing constants (odd, full-width); the multiply pushes entropy
    // upward, so the top bits are well mixed even for tiny coordinates.
    let x = (pos.0 as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = (pos.1 as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    let mixed = x ^ z.rotate_left(32);
    ((mixed >> 40) as usize) & (SHARD_COUNT - 1)
}

/// How often, and for how long, a [`StageSlot`] lookup **waited on another
/// thread** instead of hitting or computing — process-wide.
///
/// # What it separates
///
/// The store's once-only guarantee has a cost the `pre_ore_computed` /
/// `pre_ore_hits` pair structurally cannot show: a thread that arrives at an
/// empty slot while another thread is inside `get_or_init` is **parked** until
/// the value lands, and it is counted as a *hit* (deliberately — see
/// [`StageSlot::get_or_compute`]). Under a spatially contiguous generation
/// window that is the dominant shape: adjacent columns share 20 of their 25
/// pre-ore entries, so `window - 1` workers can all be parked on the one entry
/// the remaining worker is computing, and every counter in
/// `lodestone_worldgen::counters` reports a perfect once-only sweep while the
/// machine runs at 1× (`DESIGN.md` §12.131).
///
/// # Why a duration is legitimate here
///
/// `CLAUDE.md` prefers a counter to a duration, and [`Self::waits`] is that
/// counter. [`Self::wait_nanos`] is a *summed* blocked time across all workers,
/// not a wall-clock measurement of work, and its use is a **ratio** against
/// `workers × wall`: "what fraction of the parallel machine was parked". That
/// ratio is what a count alone cannot give, because one 400 ms wait and one 400 ns
/// wait are both one wait.
///
/// # Cost
///
/// Nothing here runs on the hit path. [`StageSlot::get_or_compute`] pre-checks
/// the `OnceLock` and returns before touching an `Instant`, so a warm column's
/// lookups are unchanged; the two `Instant::now()` calls and four `fetch_add`s
/// are paid only on a real miss, of which a 289-column burst has ~800.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaitStats {
    /// Lookups that found an empty slot and were parked while another thread
    /// computed it. **Exactly 0 in any single-threaded run** — that is this
    /// instrument's calibration.
    pub waits: u64,
    /// Summed nanoseconds spent parked across all threads.
    pub wait_nanos: u64,
    /// Lookups that ran the computation themselves.
    pub computes: u64,
    /// Summed nanoseconds spent inside those computations.
    pub compute_nanos: u64,
}

static WAITS: AtomicU64 = AtomicU64::new(0);
static WAIT_NANOS: AtomicU64 = AtomicU64::new(0);
static COMPUTES: AtomicU64 = AtomicU64::new(0);
static COMPUTE_NANOS: AtomicU64 = AtomicU64::new(0);

/// Zeroes the global [`WaitStats`]. Process-global, so a reset/read pair must
/// bracket work no other test is running concurrently.
pub fn reset_wait_stats() {
    for a in [&WAITS, &WAIT_NANOS, &COMPUTES, &COMPUTE_NANOS] {
        a.store(0, Ordering::Relaxed);
    }
}

/// Reads the global [`WaitStats`]. Exact — unlike
/// [`lodestone_worldgen_core::density::cache_2d_stats`] there is no per-thread
/// buffering, because the miss path is rare enough to afford a direct atomic.
#[must_use]
pub fn wait_stats() -> WaitStats {
    WaitStats {
        waits: WAITS.load(Ordering::Relaxed),
        wait_nanos: WAIT_NANOS.load(Ordering::Relaxed),
        computes: COMPUTES.load(Ordering::Relaxed),
        compute_nanos: COMPUTE_NANOS.load(Ordering::Relaxed),
    }
}

/// One chunk-stage's memoised product.
///
/// A hit costs an atomic load and an `Arc` bump — no lock. A miss runs `compute`
/// under `OnceLock`'s own initialisation guarantee, so the closure runs **once
/// per slot, ever**, across every thread. That once-only property is the whole
/// point: it is what makes "stage computations == chunks × stages" an invariant
/// instead of a measurement that happens to come out right.
///
/// See the module doc's reentrancy note before adding a stage.
#[derive(Debug)]
pub struct StageSlot<T> {
    cell: OnceLock<Arc<T>>,
}

impl<T> Default for StageSlot<T> {
    fn default() -> Self {
        Self {
            cell: OnceLock::new(),
        }
    }
}

impl<T> StageSlot<T> {
    /// Returns this slot's value, computing it exactly once.
    ///
    /// `outcome` is called with `true` if *this* call ran the computation and
    /// `false` otherwise, so a caller's hit/miss counters describe real work.
    /// It is deliberately reported from **inside** the once-guard rather than
    /// from a pre-check: a thread that loses the race to `get_or_init` returns
    /// somebody else's value and must be counted as a hit, or the counter
    /// over-reports computations under concurrency — exactly the ambiguity the
    /// old caches' `bump_pre_ore(true)`-then-compute shape had.
    pub fn get_or_compute(
        &self,
        outcome: impl FnOnce(bool),
        compute: impl FnOnce() -> T,
    ) -> Arc<T> {
        // Fast path first, so the instrument below never runs on a hit — a warm
        // column's ~26 lookups are all hits and must stay a bare atomic load.
        if let Some(value) = self.cell.get() {
            outcome(false);
            return Arc::clone(value);
        }
        let started = std::time::Instant::now();
        let computed = std::cell::Cell::new(false);
        let value = self.cell.get_or_init(|| {
            computed.set(true);
            Arc::new(compute())
        });
        let elapsed = started.elapsed().as_nanos() as u64;
        if computed.get() {
            COMPUTES.fetch_add(1, Ordering::Relaxed);
            COMPUTE_NANOS.fetch_add(elapsed, Ordering::Relaxed);
        } else {
            // The slot was empty when we looked and someone else's closure filled
            // it, so `get_or_init` parked this thread on `OnceLock`'s queue for
            // `elapsed`. See [`WaitStats`].
            WAITS.fetch_add(1, Ordering::Relaxed);
            WAIT_NANOS.fetch_add(elapsed, Ordering::Relaxed);
        }
        outcome(computed.get());
        Arc::clone(value)
    }

    /// The value if it has already been computed, without computing it.
    #[must_use]
    pub fn peek(&self) -> Option<&Arc<T>> {
        self.cell.get()
    }
}

/// One shard's map. `pins` and `last_epoch` live here, beside the entry, so
/// maintaining them costs nothing beyond the shard lock a lookup already takes —
/// no per-entry atomics, no separate recency structure.
struct Slot<E> {
    entry: Arc<E>,
    /// The epoch of the most recent request that touched this entry. Eviction
    /// order only; never consulted for correctness.
    last_epoch: u64,
    /// Number of live [`ViewScope`]s whose closure contains this entry. Non-zero
    /// means "an in-flight request needs this", and eviction skips it.
    pins: u32,
}

struct Shard<E> {
    slots: Mutex<HashMap<ChunkPos, Slot<E>>>,
}

impl<E> Default for Shard<E> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

/// The staged per-chunk store. Generic over the per-chunk entry payload `E` so
/// this module never depends back on the generator that defines the stages.
#[allow(missing_debug_implementations)]
pub struct StagedStore<E> {
    shards: Box<[Shard<E>]>,
    /// Live entry count across all shards. Read only on the *insert* path (a
    /// miss), so a steady-state all-hits column never touches it.
    total: AtomicUsize,
    /// Monotonic request counter; stamped onto touched entries as their recency
    /// key. Incremented once per [`ViewScope`] (i.e. once per top-level
    /// request), not once per lookup, so the hot path only ever *loads* it.
    epoch: AtomicU64,
    /// Ensures at most one thread runs a reclaim pass; the rest carry on.
    reclaiming: AtomicBool,
    /// Soft ceiling on live entries, above which unpinned entries are reclaimed
    /// oldest-first. Derived from real neighbourhood geometry by the caller.
    retention: usize,
    /// Entries dropped by reclamation, ever. A sweep asserting this is zero is
    /// the direct control for "eviction never made us recompute".
    evicted: AtomicUsize,
}

impl<E: Default> StagedStore<E> {
    /// Builds an empty store retaining up to `retention` entries.
    ///
    /// `retention` must be **derived from the geometry of the largest request
    /// pattern that has to be eviction-free**, never picked as a round number.
    /// See [`crate::overworld`]'s constant for the derivation actually used
    /// (the pre-ore closure of the 289-column D4 join burst).
    #[must_use]
    pub fn new(retention: usize) -> Self {
        Self {
            shards: (0..SHARD_COUNT).map(|_| Shard::default()).collect(),
            total: AtomicUsize::new(0),
            epoch: AtomicU64::new(0),
            reclaiming: AtomicBool::new(false),
            retention: retention.max(1),
            evicted: AtomicUsize::new(0),
        }
    }

    /// The entry for `pos`, creating an empty one if absent.
    ///
    /// This is the *only* lock a hit takes, and it is held for a `HashMap`
    /// probe, an `Arc` clone and a `u64` write. Never held across a stage
    /// computation — the returned `Arc` is what the caller computes into, after
    /// this function has returned and the shard lock is gone.
    pub fn entry(&self, pos: ChunkPos) -> Arc<E> {
        let epoch = self.epoch.load(Ordering::Relaxed);
        let mut inserted = false;
        let handle = {
            let shard = &self.shards[shard_of(pos)];
            let mut slots = shard.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let slot = slots.entry(pos).or_insert_with(|| {
                inserted = true;
                Slot {
                    entry: Arc::new(E::default()),
                    last_epoch: epoch,
                    pins: 0,
                }
            });
            slot.last_epoch = epoch;
            Arc::clone(&slot.entry)
        };
        if inserted {
            // Only a miss reaches the shared counter, and only a miss can push
            // the store over its ceiling.
            let total = self.total.fetch_add(1, Ordering::Relaxed) + 1;
            if total > self.retention {
                self.reclaim();
            }
        }
        handle
    }

    /// Opens a scope pinning every entry within Chebyshev `radius` of `centre`
    /// against eviction, for as long as the returned guard lives.
    ///
    /// This is what makes eviction *view-scoped* rather than capacity-guessed:
    /// an in-flight request's whole closure is ineligible for reclamation by
    /// construction, so no eviction policy — this one or a future one — can
    /// make a request recompute a stage it already computed.
    ///
    /// # This is an insertion path, and it is the one the game uses
    ///
    /// Pinning **creates** every slot in the box, so this — not
    /// [`Self::entry`] — is where a real session's entries come from.
    /// `OverworldGenerator::column` opens its pin first and only then touches a
    /// stage, so by the time `entry` runs the slot already exists, `entry`'s own
    /// `inserted` flag is always false, and its ceiling check is unreachable in
    /// the game. For one release that meant `reclaim` never ran at all outside
    /// tests and the store grew by 21 entries (~7.9 MiB) per chunk of travel,
    /// without bound: issue #503, `docs/worldgen-store-distance-leak.md`,
    /// DESIGN.md §12.108–§12.109. Hence the ceiling check below.
    ///
    /// # Why the check is after the loop and cannot go inside it
    ///
    /// A reclaim pass skips pinned entries, so once the **whole** box is pinned
    /// this scope's closure is ineligible by construction — the property that
    /// keeps eviction view-scoped rather than a capacity guess. Inside the loop
    /// that guarantee does not hold yet: at iteration *k* only *k* of the box's
    /// entries carry a pin, and the rest are typically the *oldest* unpinned
    /// entries in the store (a neighbouring column visited a moment ago), so
    /// they sort to the front of the candidate list. A pass there would evict
    /// slots this very request is about to compute into, turning a memory bound
    /// into a guaranteed recompute on the hot path. The ordering *is* the
    /// correctness argument; `reclaim` holds no lock, so there is no other
    /// reason to move it.
    #[must_use]
    pub fn open_view(&self, centre: ChunkPos, radius: i32) -> ViewScope<'_, E> {
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let mut fresh_inserts = 0usize;
        for pos in box_around(centre, radius) {
            let shard = &self.shards[shard_of(pos)];
            let mut slots = shard.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let fresh = !slots.contains_key(&pos);
            let slot = slots.entry(pos).or_insert_with(|| Slot {
                entry: Arc::new(E::default()),
                last_epoch: epoch,
                pins: 0,
            });
            slot.pins += 1;
            slot.last_epoch = epoch;
            drop(slots);
            if fresh {
                self.total.fetch_add(1, Ordering::Relaxed);
                fresh_inserts += 1;
            }
        }
        // Only a fresh insert can have pushed the store over its ceiling, so a
        // steady-state view that re-opens an already-resident neighbourhood
        // never reaches the shared counter — the same "misses only" rule
        // `entry` follows, and what keeps a hit free of anything but the shard
        // probe it already paid.
        if fresh_inserts > 0 && self.total.load(Ordering::Relaxed) > self.retention {
            self.reclaim();
        }
        ViewScope {
            store: self,
            centre,
            radius,
        }
    }

    /// Live entry count. Diagnostics and tests only.
    #[must_use]
    pub fn len(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// Whether the store is empty. Diagnostics and tests only.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Entries dropped by reclamation since construction.
    ///
    /// A sweep or burst that asserts this is **zero** has proved that its
    /// stage-computation counts cannot have been inflated by eviction — which is
    /// the control that separates "each stage computed once" from "each stage
    /// computed once, plus however many times we silently redid it".
    #[must_use]
    pub fn evicted(&self) -> usize {
        self.evicted.load(Ordering::Relaxed)
    }

    /// Drops the oldest unpinned entries until the live count is back inside
    /// `retention`.
    ///
    /// Runs on the insert path only, single-threaded (losers of the
    /// `reclaiming` swap carry on and let the winner do it), and takes one shard
    /// lock at a time — never two, and never one across a computation. Skips
    /// pinned entries and entries whose `Arc` is still held elsewhere, so it
    /// cannot throw away work a live request is about to publish.
    fn reclaim(&self) {
        if self
            .reclaiming
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Gather (epoch, pos) for every eviction-eligible entry, oldest first.
        // O(live entries), but only ever reached when a session has explored
        // past a whole join burst's closure — never during a sweep or a burst.
        let mut candidates: Vec<(u64, ChunkPos)> = Vec::new();
        for shard in self.shards.iter() {
            let slots = shard.slots.lock().unwrap_or_else(PoisonError::into_inner);
            for (pos, slot) in slots.iter() {
                if slot.pins == 0 && Arc::strong_count(&slot.entry) == 1 {
                    candidates.push((slot.last_epoch, *pos));
                }
            }
        }
        candidates.sort_unstable();
        let live = self.total.load(Ordering::Relaxed);
        let over = live.saturating_sub(self.retention);
        let mut dropped = 0usize;
        for (_, pos) in candidates.into_iter().take(over) {
            let shard = &self.shards[shard_of(pos)];
            let mut slots = shard.slots.lock().unwrap_or_else(PoisonError::into_inner);
            // Re-check under the lock: a request may have pinned or taken a
            // handle on this entry since it was gathered.
            let evictable = slots
                .get(&pos)
                .is_some_and(|slot| slot.pins == 0 && Arc::strong_count(&slot.entry) == 1);
            if evictable {
                slots.remove(&pos);
                dropped += 1;
            }
        }
        if dropped > 0 {
            self.total.fetch_sub(dropped, Ordering::Relaxed);
            self.evicted.fetch_add(dropped, Ordering::Relaxed);
        }
        self.reclaiming.store(false, Ordering::Release);
    }

    fn unpin(&self, pos: ChunkPos) {
        let shard = &self.shards[shard_of(pos)];
        let mut slots = shard.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get_mut(&pos) {
            slot.pins = slot.pins.saturating_sub(1);
        }
    }
}

/// Every chunk position within Chebyshev `radius` of `centre`, in a fixed order.
fn box_around(centre: ChunkPos, radius: i32) -> impl Iterator<Item = ChunkPos> {
    let (cx, cz) = centre;
    (-radius..=radius).flat_map(move |dx| (-radius..=radius).map(move |dz| (cx + dx, cz + dz)))
}

/// Pins one request's whole neighbourhood against eviction. See
/// [`StagedStore::open_view`].
///
/// Holds no allocation: the pinned set is re-derived from `centre`/`radius` on
/// drop, so a request's scope costs two short shard-lock visits per entry and
/// **no scratch buffer at all** — there is nothing here for a pool, shared or
/// per-thread, to own.
#[allow(missing_debug_implementations)]
pub struct ViewScope<'a, E: Default> {
    store: &'a StagedStore<E>,
    centre: ChunkPos,
    radius: i32,
}

impl<E: Default> Drop for ViewScope<'_, E> {
    fn drop(&mut self) {
        for pos in box_around(self.centre, self.radius) {
            self.store.unpin(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[derive(Default)]
    struct Stages {
        a: StageSlot<u64>,
        b: StageSlot<u64>,
    }

    /// Shard selection must scatter the 5×5 neighbourhood a single `column()`
    /// request touches, because those are the lookups that happen together.
    /// The failure this guards is a hash that maps a diagonal — or a whole
    /// neighbourhood — onto one shard, which would reproduce the single-mutex
    /// behaviour under a sharded name.
    ///
    /// The expectation is derived from outside the hash: 25 positions dropped
    /// into 64 uniform bins have an expected occupancy of 25 - 64*(1-(63/64)^25)
    /// ≈ 4.5 collisions, so ≥ 15 distinct shards is a bound a uniform hash
    /// clears comfortably and a degenerate one (1 shard, or 5 for a
    /// coordinate-ignoring hash) cannot.
    #[test]
    fn shard_selection_scatters_a_single_requests_neighbourhood() {
        for &(cx, cz) in &[(0, 0), (7, -3), (-120, -120), (1000, 1000), (-1, 1)] {
            let shards: std::collections::HashSet<usize> =
                box_around((cx, cz), 2).map(shard_of).collect();
            assert!(
                shards.len() >= 15,
                "5x5 around ({cx},{cz}) used only {} of {SHARD_COUNT} shards",
                shards.len()
            );
        }
    }

    /// Adjacent chunks specifically must not share a shard wholesale — the 3×3
    /// driver asks for all 9 at once.
    #[test]
    fn adjacent_chunks_do_not_pile_into_one_shard() {
        let shards: std::collections::HashSet<usize> = box_around((0, 0), 1).map(shard_of).collect();
        assert!(
            shards.len() >= 6,
            "3x3 around origin used only {} shards",
            shards.len()
        );
    }

    /// The core invariant: N threads racing the same slot run the computation
    /// **once**, and all N observe the same value.
    ///
    /// This is the property the old FIFO caches did not have — they released
    /// their lock across the computation and let every racing miss recompute,
    /// which is why `pre_ore_computed` could exceed the number of distinct
    /// chunks. The negative control is right below.
    #[test]
    fn concurrent_racers_on_one_slot_compute_exactly_once() {
        let store: Arc<StagedStore<Stages>> = Arc::new(StagedStore::new(1024));
        let runs = Arc::new(AtomicU32::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let store = Arc::clone(&store);
                let runs = Arc::clone(&runs);
                let observed = Arc::clone(&observed);
                std::thread::spawn(move || {
                    let entry = store.entry((3, 4));
                    let value = entry.a.get_or_compute(
                        |_| {},
                        || {
                            runs.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            42
                        },
                    );
                    observed.lock().unwrap().push(*value);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "16 racers must run the computation once, not once each"
        );
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 16);
        assert!(observed.iter().all(|v| *v == 42));
    }

    /// Negative control for the test above: the old cache shape (probe, release,
    /// compute, insert) really does recompute under the same race, so the
    /// `runs == 1` assertion above is measuring the new structure and not an
    /// artefact of a test too fast to race.
    #[test]
    fn the_old_probe_release_compute_shape_recomputes_under_the_same_race() {
        let cache: Arc<Mutex<HashMap<ChunkPos, Arc<u64>>>> = Arc::new(Mutex::new(HashMap::new()));
        let runs = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let runs = Arc::clone(&runs);
                std::thread::spawn(move || {
                    if let Some(hit) = cache.lock().unwrap().get(&(3, 4)) {
                        return Arc::clone(hit);
                    }
                    runs.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    let computed = Arc::new(42u64);
                    let mut guard = cache.lock().unwrap();
                    Arc::clone(guard.entry((3, 4)).or_insert(computed))
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(
            runs.load(Ordering::SeqCst) > 1,
            "control failed to race: the old shape must be observed recomputing, \
             otherwise the once-only test above proves nothing"
        );
    }

    /// Each stage of one chunk is independent: computing `a` must not mark `b`
    /// done. A slot-per-stage store that shared one flag would silently serve a
    /// stage that never ran.
    #[test]
    fn stages_within_one_entry_are_independent() {
        let store: StagedStore<Stages> = StagedStore::new(1024);
        let entry = store.entry((0, 0));
        let mut a_computed = false;
        let mut b_computed = false;
        entry.a.get_or_compute(|c| a_computed = c, || 1);
        entry.b.get_or_compute(|c| b_computed = c, || 2);
        assert!(a_computed && b_computed);
        assert_eq!(entry.a.peek().map(|v| **v), Some(1));
        assert_eq!(entry.b.peek().map(|v| **v), Some(2));
    }

    /// Keys are exact. `(1, 2)` and `(2, 1)` are different chunks and must never
    /// share an entry — the transposition case a symmetric `cx ^ cz` hash would
    /// merge if anything ever keyed on the shard instead of the position.
    #[test]
    fn distinct_positions_never_alias_including_transpositions() {
        let store: StagedStore<Stages> = StagedStore::new(1024);
        store.entry((1, 2)).a.get_or_compute(|_| {}, || 12);
        store.entry((2, 1)).a.get_or_compute(|_| {}, || 21);
        store.entry((-1, 2)).a.get_or_compute(|_| {}, || 90);
        assert_eq!(store.entry((1, 2)).a.peek().map(|v| **v), Some(12));
        assert_eq!(store.entry((2, 1)).a.peek().map(|v| **v), Some(21));
        assert_eq!(store.entry((-1, 2)).a.peek().map(|v| **v), Some(90));
    }

    /// An entry inside a live [`ViewScope`] cannot be evicted, even when the
    /// store is driven far past its retention ceiling.
    ///
    /// The magnitude matters, not just the direction: `retention = 4` with 400
    /// unrelated inserts is ~100× over the ceiling, so "survived" cannot be an
    /// accident of not having reclaimed yet — and the control below proves
    /// reclamation really does fire in that regime.
    #[test]
    fn a_pinned_neighbourhood_survives_massive_over_pressure() {
        let store: StagedStore<Stages> = StagedStore::new(4);
        let scope = store.open_view((0, 0), 2);
        store.entry((0, 0)).a.get_or_compute(|_| {}, || 7);
        for i in 100..500 {
            store.entry((i, i)).a.get_or_compute(|_| {}, || 0);
        }
        assert!(
            store.evicted() > 0,
            "control: reclamation must actually have run at 400 inserts over a \
             ceiling of 4, or the survival assertion below is vacuous"
        );
        assert_eq!(
            store.entry((0, 0)).a.peek().map(|v| **v),
            Some(7),
            "a pinned entry was evicted"
        );
        drop(scope);
    }

    /// The other half of the pin contract: once the scope is gone the entry is
    /// eligible again, so pinning is a real lifetime and not a permanent leak.
    #[test]
    fn unpinned_entries_are_reclaimable_once_the_scope_ends() {
        let store: StagedStore<Stages> = StagedStore::new(4);
        {
            let _scope = store.open_view((0, 0), 1);
            store.entry((0, 0)).a.get_or_compute(|_| {}, || 7);
        }
        let before = store.evicted();
        for i in 100..600 {
            store.entry((i, -i)).a.get_or_compute(|_| {}, || 0);
        }
        assert!(store.evicted() > before);
        assert!(
            store.len() <= store.retention + SHARD_COUNT,
            "live entries {} unbounded against retention {}",
            store.len(),
            store.retention
        );
    }

    /// Retention ceiling the walk gate runs against — `overworld::STORE_RETENTION`
    /// restated, because it is private to that module. Only ever compared against
    /// as an upper bound, so a stale value here weakens the gate rather than
    /// breaking it; re-read `overworld/mod.rs` before trusting it.
    const WALK_RETENTION: usize = 2048;

    /// `overworld::STRUCTURE_CLOSURE_RADIUS`, restated for the same reason. The
    /// 21×21 pin every `column()` call opens.
    ///
    /// **This was 2 and it had to move.** Issue #514 put a `structure_refs` walk
    /// (`REFS_RADIUS` = 8) upstream of `pre_ore`, so the real pin is `2 + 8`. A
    /// model at radius 2 is not a cheaper version of production's shape — it is a
    /// 59-entry-per-column shape where production is 1,243, and every number below
    /// is derived from it (DESIGN.md §12.130).
    const WALK_CLOSURE_RADIUS: i32 = 10;

    /// View radius the walk gate slides: 8 gives a 17×17 = 289-column view, the
    /// same render distance `STORE_RETENTION` was sized against.
    const WALK_VIEW_RADIUS: i32 = 8;

    /// Chunk steps the walk gate slides the view.
    ///
    /// **Derived from the leak's own slope, not picked.** A `+x` step exposes a
    /// leading strip of `2R + 1` columns whose pins add `2(R + closure) + 1` = 37
    /// entries at `R = 8`, `closure = 10`, so an unreclaimed store reaches
    /// `1,369 + 37 · steps`. 240 steps puts that at 10,249 — over 4× the ceiling
    /// of 2,489, which is the separation the magnitude assertion below needs and
    /// which that assertion now checks as a premise rather than assuming.
    const WALK_STEPS: i32 = 240;

    /// One `OverworldGenerator::column` call's store traffic, in its real shape.
    ///
    /// Faithful to `column()` rather than convenient: the 5×5 pin, then the
    /// pre-ore stage over that whole 5×5 (`ore_stage`'s 3×3-of-3×3 closure), then
    /// the post-ore stage over the 3×3 (`vegetation_stage`'s driver). Both stage
    /// loops go through [`StagedStore::entry`], exactly as production does —
    /// which is why the entries are all created by `open_view` first and why
    /// `entry`'s own ceiling check never sees `inserted == true` here either.
    fn visit_column(store: &StagedStore<Stages>, pos: ChunkPos) {
        let _view = store.open_view(pos, WALK_CLOSURE_RADIUS);
        for p in box_around(pos, WALK_CLOSURE_RADIUS) {
            store.entry(p).a.get_or_compute(|_| {}, || 1);
        }
        for p in box_around(pos, WALK_CLOSURE_RADIUS - 1) {
            store.entry(p).b.get_or_compute(|_| {}, || 2);
        }
    }

    /// **The gate for the `O(distance)` leak of `docs/worldgen-store-distance-leak.md`:
    /// a view walked across the world must not grow the store without bound.**
    ///
    /// Every other reclamation test in this file drives its inserts through
    /// [`StagedStore::entry`]. Production drives them through
    /// [`StagedStore::open_view`], because `column()` opens its pin *before*
    /// touching a stage, so by the time `entry` runs the slot already exists and
    /// `entry`'s ceiling check is unreachable. Both of those tests were green
    /// while reclamation was dead in the game: the *world* species — exemplary
    /// source, wrong input. **This gate exists to drive the production path, and
    /// nothing about it should be changed to route through `entry`.**
    ///
    /// Two hypotheses, both computed from constants outside the measurement, so
    /// this is not a direction-only assertion:
    ///
    /// | hypothesis | predicted final `len()` |
    /// |---|---|
    /// | reclamation reaches the `open_view` path | ≤ 2,048 + 441 (a pass's peak overshoot is one view's box) |
    /// | it does not (the shipped defect) | 1,369 + 37 × 240 = **10,249** |
    ///
    /// The measurement has to land on the first and be nowhere near the second.
    #[test]
    fn a_view_walked_across_the_world_stays_inside_the_retention_ceiling() {
        let store: StagedStore<Stages> = StagedStore::new(WALK_RETENTION);
        let r = WALK_VIEW_RADIUS;

        // The join view, as a connecting player produces it.
        for dz in -r..=r {
            for dx in -r..=r {
                visit_column(&store, (dx, dz));
            }
        }
        let join_len = store.len();

        // The closure of a 17×17 view is 37×37 = 1,369 (it was 21×21 = 441 before
        // issue #514 widened the pin). Asserting it here is what says this hermetic
        // model reproduces production's geometry rather than some cheaper shape
        // that could not leak in the first place — and it is under the ceiling, so
        // it holds on both arms.
        assert_eq!(
            join_len,
            (2 * (r + WALK_CLOSURE_RADIUS) + 1).pow(2) as usize,
            "the join view's closure is not the {} entries production reaches, so this \
             gate is not modelling `column()`",
            (2 * (r + WALK_CLOSURE_RADIUS) + 1).pow(2)
        );

        // A curve, not a point: the claim is about shape, so every sample is
        // checked and all of them are printed on failure.
        let mut curve: Vec<(i32, usize, usize)> = vec![(0, join_len, store.evicted())];
        for step in 1..=WALK_STEPS {
            let cx = step + r;
            for dz in -r..=r {
                visit_column(&store, (cx, dz));
            }
            if step % 20 == 0 {
                curve.push((step, store.len(), store.evicted()));
            }
        }

        let unreclaimed = join_len + (2 * (r + WALK_CLOSURE_RADIUS) + 1) as usize * WALK_STEPS as usize;
        let ceiling = WALK_RETENTION + (2 * WALK_CLOSURE_RADIUS + 1).pow(2) as usize;
        let shape = curve
            .iter()
            .map(|(step, len, evicted)| format!("step={step} len={len} evicted={evicted}"))
            .collect::<Vec<_>>()
            .join("; ");

        // **Premise of the magnitude assertion at the end**, checked rather than
        // assumed: the walk has to be long enough that the two hypotheses are far
        // apart. At radius 2 this held for free; at radius 10 the ceiling is 4.7×
        // larger and `WALK_STEPS` had to move with it.
        assert!(
            unreclaimed >= 4 * ceiling,
            "the walk is too short for the two hypotheses to be distinguishable: \
             unreclaimed {unreclaimed} is not 4x the ceiling {ceiling}. Raise \
             WALK_STEPS."
        );

        for &(step, len, _) in &curve {
            assert!(
                len <= ceiling,
                "store_len {len} at step {step} exceeds {ceiling} (retention \
                 {WALK_RETENTION} + one view box): the ceiling is not reached from the \
                 `open_view` path. Predicted {unreclaimed} if reclamation never runs. \
                 Curve: {shape}"
            );
        }

        // The detector: reclamation must actually have fired, or "bounded" above
        // would be satisfied by a walk that never reached the ceiling at all.
        assert!(
            store.evicted() > 0,
            "nothing was ever evicted over a {WALK_STEPS}-step walk, so the bound above \
             is vacuous. Curve: {shape}"
        );
        // Magnitude, not sign: the measurement must be far from the leaking
        // hypothesis, not merely smaller than it.
        assert!(
            store.len() * 4 < unreclaimed,
            "store_len {} is within 4x of the {unreclaimed} entries the unreclaimed \
             hypothesis predicts. Curve: {shape}",
            store.len()
        );
        println!("view walk, {WALK_STEPS} steps at R={r}: {shape}");
    }

    /// No eviction at all below the ceiling — the regime every parity sweep and
    /// the 289-column burst run in. This is the property that makes the
    /// stage-computation counter equal to `chunks x stages` exactly.
    #[test]
    fn nothing_is_evicted_below_the_retention_ceiling() {
        let store: StagedStore<Stages> = StagedStore::new(WALK_RETENTION);
        // The full closure of a 17x17 join burst: 37x37 = 1,369 chunks, since
        // `column()`'s pin is `COLUMN_CLOSURE_RADIUS + REFS_RADIUS` = 10 deep.
        // This used to read 21x21 = 441 against a 512 ceiling; both numbers moved
        // together, and the point of the test is that the second still exceeds the
        // first.
        for cx in -10..27 {
            for cz in -10..27 {
                store.entry((cx, cz)).a.get_or_compute(|_| {}, || 1);
            }
        }
        assert_eq!(store.len(), 1369);
        assert!(
            WALK_RETENTION > 1369,
            "the retention ceiling {WALK_RETENTION} no longer exceeds the D4 burst \
             closure of 1,369, so the assertion below would be trivially false rather \
             than a property of the store"
        );
        assert_eq!(store.evicted(), 0, "the D4 burst closure must not evict");
    }
}
