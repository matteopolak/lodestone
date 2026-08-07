//! A bounded cache of generated chunk columns (`docs/plans/chunk-lifecycle.md`
//! unit **U3**, issue #289 part 1).
//!
//! # What it is
//!
//! [`ChunkStore`] wraps any [`ChunkSource`] and *is* a [`ChunkSource`], so it
//! drops in wherever a source is constructed with no call-site changes
//! anywhere. It retains the columns it has been asked for, evicting the
//! least-recently-used one past a capacity bound, so a column is generated
//! **once** and thereafter read.
//!
//! # Why it exists: this was a correctness bug, not a performance gap
//!
//! [`crate::chunk::OverworldChunkSource`] retains **only edited** columns —
//! its own doc comment says so, and says regenerating an unedited column on
//! every request is deliberate because the generator is deterministic, making
//! "regenerate" and "cache forever" observationally identical. That reasoning
//! was sound and is now false, because it was arithmetic about a *cheap*
//! generator. [`crate::tick::run_tick_loop`]'s doc comment drew the explicit
//! conclusion — regeneration every tick is *"a real, documented performance
//! gap … not a correctness one"*. Both went stale the same way, and this is a
//! textbook instance of CLAUDE.md's rule 2: the claim was true and evidenced
//! when written.
//!
//! What changed underneath it is that generation composed in carvers, ores and
//! vegetation, of which vegetation is ~62% of the cost and ore ~18%. Measured
//! here in release, four **cold** columns from four independently constructed
//! sources (so the generator's memo cache cannot absorb any of them), on a box
//! at load average 3.7:
//!
//! ```text
//! column 0: 803.0ms   column 1: 840.8ms   column 2: 1.001s   column 3: 991.4ms
//! mean: 909.2ms
//! ```
//!
//! A 20 Hz tick has a **50 ms** budget, so *one* regeneration is ~18 tick
//! budgets. `measure_real_column_generation_cost` below reproduces this.
//!
//! Two independent consumers were paying that per-column cost on a repeating
//! timer, and they starve two *different* tasks — which is why the owner's
//! report had four symptoms and not one:
//!
//! | site | cadence | columns per firing | task starved |
//! |---|---|---|---|
//! | [`crate::tick::run_tick_loop`]'s random-tick loop | every tick (50 ms) | the whole `tick_area` — **49** at the shell's `mob_radius.clamp(1, 3)` | the world tick |
//! | `crate::server`'s `vitals_tick` submersion probe | every 50 ms, once `player_pos` is `Some` | 1, to read a **single block** | the *connection*, i.e. chunk streaming |
//!
//! At 909 ms per column the world tick was therefore spending ~44.5 s of
//! generation per 50 ms of budget — about **0.022 TPS** — and the connection
//! task ~909 ms per 50 ms. That single number explains all four of the symptoms
//! reported against singleplayer: the server barely ticks, so chunk streaming
//! starves ("takes forever to load"); the connection task is saturated from the
//! first movement packet onward ("stops generating chunks after the first
//! load"); the view never recenters ("chunks not close to me"); and the client
//! freezes the player rather than falling into an unloaded column ("stuck in
//! the air" — `lodestone-shell/src/sim/collide.rs`'s
//! `is_chunk_loaded` early return, via `PlayerCollision::Pending`).
//!
//! The second one is the more surprising of the pair and it is not a call-site
//! mistake: until issue #440 made it a **required** method, `block_state`'s
//! default implementation was `self.column(cx, cz).block_state(..)`, so
//! reading one block regenerated a whole 16×384×16 column. It fires only once
//! the client has sent a position, which lines up exactly with *"it seems to
//! stop generating chunks after the first load"* — the connection task streams
//! chunks fine during the join burst and is saturated from the first movement
//! packet onward.
//!
//! [`ChunkStore`] therefore overrides `block_state` as well as `column`; the
//! override reads one cell out of the retained column and clones nothing, and
//! it is the model for the post-#440 required method: a source that retains
//! columns should read a cell from them rather than regenerate.
//!
//! # How it works
//!
//! One `Mutex<Cache>` holding a `HashMap<(i32, i32), Entry>` plus a monotonic
//! use-stamp per entry. Three properties are load-bearing:
//!
//! - **Generation happens with the lock released.** A miss unlocks, calls
//!   `source.column()`, then re-locks to insert. Holding the lock across an
//!   ~909 ms generation would serialise
//!   [`crate::chunk::generate_columns_parallel`]'s whole scoped fan-out and
//!   undo issue #414.
//! - **An insert after that window never overwrites.** In the unlocked
//!   interval another thread may have inserted, and its entry may carry a
//!   [`set_block`](ChunkSource::set_block) edit that this thread's freshly
//!   generated column does not. First writer wins; the loser's column is
//!   dropped. (Both are otherwise byte-identical — generation is deterministic
//!   per chunk, see `generate_columns_parallel`'s doc comment.)
//! - **Eviction is lossless, so the bound needs no exception for edits.** A
//!   `set_block` is forwarded to the inner source *before* the cache is
//!   touched, and `OverworldChunkSource::edits` retains it there permanently.
//!   Dropping a cache entry therefore costs a regeneration and never a block:
//!   the regeneration goes back through `OverworldChunkSource::column`, which
//!   consults `edits` first. This is the single property that lets the store be
//!   bounded at all — `docs/plans/chunk-lifecycle.md`'s U6 needs a much more
//!   careful rule ("refuse to drop an edited column") because *it* drops the
//!   authoritative copy, where this only drops a cache.
//!
//! # The memory this costs, and how to change it
//!
//! `ChunkColumn` is a dense `Vec<u16>` over full world height
//! (`crate::chunk`), i.e. `16 × 384 × 16 × 2 B` ≈ **192 KiB** per column —
//! free today precisely *because* nothing retained it. Retention turns that
//! into real resident memory, which is
//! `docs/plans/chunk-lifecycle.md`'s top risk and the reason this type is
//! bounded rather than a plain `HashMap`.
//!
//! **Measured, not assumed** (the plan's U2 question, answered here because
//! this is the unit that creates the cost). `/usr/bin/time -l` on the release
//! test binary, one arm per configuration:
//!
//! | arm | peak RSS |
//! |---|---|
//! | 512 columns retained | 105.4 MiB |
//! | same 512 touched, retention off | 7.8 MiB |
//! | **delta** | **97.6 MiB**, i.e. 195.5 KiB per column |
//!
//! The delta lands within 2% of the 192 KiB arithmetic, the remainder being the
//! palette and biome `String`s and the map itself. The two arms are also each
//! other's control: a delta near zero would mean the columns were dropped in
//! both arms, or that the pages were never faulted in, and the run would be a
//! failure to measure rather than evidence that residency is free. See
//! `touched_column`, which exists because `alloc_zeroed` pages that are never
//! written do not show up in RSS.
//!
//! [`DEFAULT_CAPACITY`] is the knob, and 97.6 MiB is what it currently buys.
//! Lowering it to 128 (~24 MiB) **still fixes the reported bug completely** —
//! the starvation fix needs only the 49-column `tick_area` resident, and
//! everything beyond that is avoided *re*-generation as a player walks back
//! over ground they have seen. 512 was chosen to also cover the default
//! streamed view (`render_distance` 8 ⇒ `view_radius` 9 ⇒ 361 columns) so that
//! walking in a circle does not pay 909 ms per column again.
//!
//! To reduce the cost rather than the count, the prior art is
//! `lodestone-world`'s `PalettedContainer` over `PackedArray` plus
//! `Arc<ChunkSection>` copy-on-write sections — that is unit **U8** of the
//! plan, deliberately gated on a *measurement* rather than on the arithmetic
//! above.
//!
//! # The clone this keeps, deliberately
//!
//! [`ChunkSource::column`] returns a `ChunkColumn` **by value**, so a store
//! read is a ~192 KiB `memcpy` (measured in the gate below, tens of
//! microseconds) rather than a refcount bump. Handing back
//! `Arc<ChunkColumn>` instead — which the plan asks for, and which U8 wants —
//! cannot be done without either changing that signature or lending `&mut`
//! from inside the lock.
//!
//! Lending `&mut` is the trap, and it is worth writing down because it looks
//! like the obvious design: `run_tick_loop` mutates its column
//! (`random_tick::tick_chunk` takes `&mut ChunkColumn`) **and** calls
//! `world.set_block` for the same chunk in the same breath, to persist through
//! to the source. A `with_column_mut(cx, cz, f)` that holds the cache lock
//! across `f` therefore **deadlocks** on that nested `set_block`, and the
//! `try_lock` workaround silently skips a cache update on genuine contention,
//! which serves a stale block. So the closure API is not exposed even
//! privately in a re-entrant shape.
//!
//! The trade is not close: the clone is 3.1 µs (measured below) against the 909
//! ms it removes, and it needs **zero edits to `tick.rs`** — the most
//! contended file in this cluster, with concurrent redstone work in it.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::Mutex;

use crate::chunk::{ChunkColumn, ChunkSource};

/// How many columns a [`ChunkStore::new`] store retains before evicting the
/// least-recently-used one.
///
/// 512 dense full-height columns measured **97.6 MiB** of resident memory (see
/// this module's memory section for the paired `/usr/bin/time -l` arms — that is
/// a measurement, not the 96 MiB arithmetic). Chosen to hold `run_tick_loop`'s 49-column
/// `tick_area` plus a typical streamed view with room to spare, not to hold a
/// whole large view: retaining the tick area is what removes the per-tick
/// regeneration, and eviction is lossless, so overshooting the view costs a
/// regeneration and never a block.
pub(crate) const DEFAULT_CAPACITY: usize = 512;

/// One retained column plus the stamp that orders eviction.
struct Entry {
    column: ChunkColumn,
    /// Value of `Cache::stamp` at this entry's most recent read or write.
    /// Smallest wins eviction.
    last_used: u64,
}

struct Cache {
    columns: HashMap<(i32, i32), Entry>,
    /// Monotonic counter handed out by [`Cache::next_stamp`]. Not a tick count
    /// and not comparable to anything outside this struct.
    stamp: u64,
    /// Cumulative count of calls that reached `source.column()`. This is a
    /// store-lifetime accumulator, so a gate must read it as a **delta** or
    /// against a freshly constructed store — see [`ChunkStore::generated`].
    generated: u64,
    /// Cumulative count of evictions, same accumulator caveat.
    evicted: u64,
}

impl Cache {
    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// Drops least-recently-used entries until `len() <= capacity`.
    ///
    /// Linear scan per eviction rather than an intrusive LRU list: it runs only
    /// on a **miss**, which has just paid a generation three to four orders of
    /// magnitude more expensive than 512 integer comparisons. A real LRU here
    /// would be optimising the cheap half.
    /// Returns the evicted coordinates so the caller can pass them to
    /// [`ChunkSource::unload`] **after releasing the cache lock**. Notifying
    /// from in here would call out into the source while holding this mutex,
    /// which is both a lock-ordering hazard and a way to put the source's own
    /// work on the critical section every miss pays.
    fn evict_down_to(&mut self, capacity: usize) -> Vec<(i32, i32)> {
        let mut evicted = Vec::new();
        while self.columns.len() > capacity {
            let Some(victim) = self
                .columns
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(&key, _)| key)
            else {
                break;
            };
            self.columns.remove(&victim);
            self.evicted += 1;
            evicted.push(victim);
        }
        evicted
    }
}

/// A [`ChunkSource`] that retains what it generates. See the module docs.
pub(crate) struct ChunkStore<S> {
    source: S,
    capacity: usize,
    cache: Mutex<Cache>,
}

impl<S> ChunkStore<S> {
    /// Wraps `source`, retaining up to [`DEFAULT_CAPACITY`] columns.
    pub(crate) fn new(source: S) -> Self {
        Self::with_capacity(source, DEFAULT_CAPACITY)
    }

    /// Wraps `source` with an explicit capacity. A capacity of 0 disables
    /// retention entirely, which is the pre-store behaviour and is what the
    /// gate below uses as its negative control.
    pub(crate) fn with_capacity(source: S, capacity: usize) -> Self {
        Self {
            source,
            capacity,
            cache: Mutex::new(Cache {
                columns: HashMap::new(),
                stamp: 0,
                generated: 0,
                evicted: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("chunk store lock poisoned")
    }

    // The four accessors below are `#[cfg(test)]` rather than
    // `#[allow(dead_code)]`: nothing in production reads them, and pretending
    // otherwise is how dead code accumulates. Production observability goes
    // through the `Debug` impl, which reports all four. Units U6 (unloading)
    // and U8 (sectioned storage) of `docs/plans/chunk-lifecycle.md` are the
    // ones that will want them for real; drop the `cfg` then.

    /// Columns currently retained. Never exceeds [`capacity`](Self::capacity).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().columns.len()
    }

    /// The eviction bound this store was built with.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Cumulative calls that reached the inner source's `column()`.
    ///
    /// A store-lifetime accumulator: read it as a delta, or from a store
    /// constructed inside the gate. It is a convenience cross-check only — the
    /// gate below counts on its own hand-written source instead, because the
    /// real `OverworldGenerator` carries a 512-entry memo cache that would
    /// absorb a second request and make any count measured *above* it vacuous.
    #[cfg(test)]
    pub(crate) fn generated(&self) -> u64 {
        self.lock().generated
    }

    /// Cumulative evictions. Same accumulator caveat as
    /// [`generated`](Self::generated).
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> u64 {
        self.lock().evicted
    }
}

impl<S> std::fmt::Debug for ChunkStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = self.lock();
        f.debug_struct("ChunkStore")
            .field("resident", &cache.columns.len())
            .field("capacity", &self.capacity)
            .field("generated", &cache.generated)
            .field("evicted", &cache.evicted)
            .finish_non_exhaustive()
    }
}

impl<S: ChunkSource> ChunkStore<S> {
    /// Makes `(cx, cz)` resident, generating it **with the lock released** if
    /// it is not. See the module docs for why that matters and why the insert
    /// does not overwrite.
    ///
    /// Returns `Some(column)` only when retention is disabled
    /// (`capacity == 0`), handing the freshly generated column straight back so
    /// the caller does not generate it a **second** time. That double
    /// generation is what the first draft of this did, and the negative control
    /// below caught it: the control measured `49 × 12 × 2` where the predicted
    /// pre-store figure is `49 × 12`. Harmless in production (capacity is never
    /// 0 there) but it made the control a 2× overstatement of the bug rather
    /// than an exact reproduction of it.
    ///
    /// For any `capacity >= 1` the just-inserted entry carries the highest
    /// `last_used` in the map, so [`Cache::evict_down_to`] can never choose it
    /// and the following [`read`](Self::read) is guaranteed to hit.
    fn ensure(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                entry.last_used = stamp;
                return None;
            }
        }

        // Lock released: a ~909 ms generation must not serialise
        // `generate_columns_parallel`'s scoped fan-out.
        let fresh = self.source.column(cx, cz);

        let mut guard = self.lock();
        let cache = &mut *guard;
        cache.generated += 1;
        let stamp = cache.next_stamp();
        if self.capacity == 0 {
            return Some(fresh);
        }
        match cache.columns.entry((cx, cz)) {
            // Another thread won the race while this one generated. Keep
            // theirs: it may carry a `set_block` edit this column predates.
            MapEntry::Occupied(mut occupied) => occupied.get_mut().last_used = stamp,
            MapEntry::Vacant(vacant) => {
                vacant.insert(Entry {
                    column: fresh,
                    last_used: stamp,
                });
            }
        }
        let evicted = cache.evict_down_to(self.capacity);
        drop(guard);
        // Outside the lock, deliberately: see `evict_down_to`. This is what
        // lets the layer beneath release a column it has already written, so
        // the edit map is no longer the process's real memory bound for a
        // heavily-built world.
        for (vx, vz) in evicted {
            self.source.unload(vx, vz);
        }
        None
    }

    /// Reads a retained column in place, without cloning it. `None` if it is
    /// not resident (a capacity of 0, or an eviction in the window since
    /// [`ensure`](Self::ensure)).
    fn read<R>(&self, cx: i32, cz: i32, f: impl FnOnce(&ChunkColumn) -> R) -> Option<R> {
        let mut guard = self.lock();
        let cache = &mut *guard;
        let stamp = cache.next_stamp();
        let entry = cache.columns.get_mut(&(cx, cz))?;
        entry.last_used = stamp;
        Some(f(&entry.column))
    }
}

impl<S: ChunkSource> ChunkSource for ChunkStore<S> {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        // `Some` means retention is off (the negative-control configuration) —
        // the column was just generated and there is nothing to read it from.
        if let Some(fresh) = self.ensure(cx, cz) {
            return fresh;
        }
        // The fallback below is reachable only if another thread evicted this
        // entry in the window since `ensure` inserted it, which needs a
        // capacity-worth of concurrent misses. Correct rather than dead, and it
        // costs a regeneration, never a wrong block.
        self.read(cx, cz, ChunkColumn::clone)
            .unwrap_or_else(|| self.source.column(cx, cz))
    }

    /// One block, without regenerating or cloning a column.
    ///
    /// Overriding this is half the fix, not an optimisation: the
    /// column-regenerating form (`self.column(cx, cz).block_state(..)`, once
    /// `block_state`'s default and now each non-retaining implementor's
    /// explicit choice) regenerates a whole column per probe, and
    /// `crate::server`'s `vitals_tick` calls this every 50 ms on the
    /// connection task. See the module docs.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        if let Some(fresh) = self.ensure(cx, cz) {
            return fresh.block_state(lx, y, lz).to_string();
        }
        self.read(cx, cz, |column| column.block_state(lx, y, lz).to_string())
            .unwrap_or_else(|| self.source.block_state(x, y, z))
    }

    /// Writes through to the inner source **first**, then to the retained
    /// column if one is resident.
    ///
    /// That order is what makes eviction lossless — see the module docs. If no
    /// entry is resident this deliberately does not create one: the next read
    /// regenerates through the inner source, which for
    /// [`crate::chunk::OverworldChunkSource`] consults its `edits` map and so
    /// returns the edited column.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.source.set_block(x, y, z, name);

        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut guard = self.lock();
        let cache = &mut *guard;
        let stamp = cache.next_stamp();
        if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
            // A `y` outside the column's vertical extent is a no-op rather than
            // an index panic. `ChunkColumn::set_block` indexes unguarded, and
            // the inner source's own `set_block` may have accepted the edit (or
            // rejected it its own way) without this retained column being able
            // to hold it — so the store guards its own update rather than
            // relying on the source to reject out-of-range `y`.
            if y >= entry.column.min_y && y < entry.column.min_y + entry.column.height {
                entry.column.set_block(lx, y, lz, name);
                entry.last_used = stamp;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use lodestone_model::BlockPos;

    use super::*;
    use crate::block_entities::BlockEntityHandle;
    use crate::mobs::{ChunkWorld, MobHandle};
    use crate::tick::{
        BlockTickFeed, ExplosionFeed, INITIAL_RANDOM_TICK_DEFERRAL_TICKS, TICK_PERIOD, TickClock,
        run_tick_loop,
    };

    /// The world height a real overworld column has, so the clone-cost
    /// measurement below is about the representation production actually pays
    /// for rather than a toy.
    const REAL_MIN_Y: i32 = -64;
    const REAL_HEIGHT: i32 = 384;

    /// A [`ChunkSource`] that counts `column()` calls and nothing else.
    ///
    /// **Hand-written on purpose, and this is the anti-vacuity property of
    /// every count below.** The real `OverworldGenerator` carries a per-instance
    /// 512-entry memo cache keyed on exact `(cx, cz)`, so a generation-count
    /// gate built on `crate::overworld_chunk_source` passes *even with a
    /// completely broken store* — the memo absorbs the second call. That exact
    /// vacuity was found and fixed once already in `crate::chunk`'s
    /// `parallel_generation_is_deterministic_and_matches_serial`. This source
    /// has no cache of any kind, so every call it is asked for, it counts.
    struct CountingSource {
        calls: Arc<AtomicU64>,
        /// Recorded per coordinate too, so a failure can say *which* chunk was
        /// regenerated rather than only that the total was wrong — per
        /// CLAUDE.md's "make failure output say *where*". Shared by `Arc` so a
        /// gate can keep reading it after the source is moved into the store.
        per_chunk: PerChunk,
        min_y: i32,
        height: i32,
    }

    type PerChunk = Arc<Mutex<HashMap<(i32, i32), u64>>>;

    impl CountingSource {
        fn new() -> Self {
            Self::sized(0, 16)
        }

        /// Same shape, but full overworld height — used where the *size* of a
        /// column matters (the clone-cost and residency measurements).
        fn full_height() -> Self {
            Self::sized(REAL_MIN_Y, REAL_HEIGHT)
        }

        fn sized(min_y: i32, height: i32) -> Self {
            Self {
                calls: Arc::new(AtomicU64::new(0)),
                per_chunk: Arc::new(Mutex::new(HashMap::new())),
                min_y,
                height,
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    /// The worst per-coordinate generation count, with its coordinate — the
    /// figure that distinguishes "generated once each" from "regenerated every
    /// tick" without depending on how many chunks the loop happened to visit.
    fn worst_chunk(per_chunk: &PerChunk) -> ((i32, i32), u64) {
        per_chunk
            .lock()
            .expect("per-chunk map poisoned")
            .iter()
            .max_by_key(|&(_, &n)| n)
            .map(|(&k, &n)| (k, n))
            .unwrap_or(((0, 0), 0))
    }

    /// How many distinct coordinates were ever generated.
    fn distinct_chunks(per_chunk: &PerChunk) -> usize {
        per_chunk.lock().expect("per-chunk map poisoned").len()
    }

    impl ChunkSource for CountingSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self
                .per_chunk
                .lock()
                .expect("per-chunk map poisoned")
                .entry((cx, cz))
                .or_insert(0) += 1;
            ChunkColumn::new(self.min_y, self.height)
        }

        // Goes through `column()` on purpose: the control half of
        // `repeated_single_block_probes_generate_one_column_not_forty` relies
        // on one probe costing exactly one generation. This is the explicit,
        // column-regenerating form that used to be `block_state`'s default.
        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // `run_tick_loop` forwards random-tick and grazing mutations through
        // the store to the inner source, so this must not panic; but the
        // source has no storage (its `column()` is a fresh blank column plus a
        // counter), so the edit is deliberately discarded. Explicit rather than
        // inherited — the point of issue #440.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this counting stub.
        }
    }

    /// The `tick_area` the shell actually produces for singleplayer:
    /// `crates/lodestone-shell/src/net.rs` passes
    /// `mob_radius = view_radius.clamp(1, 3)`, so at any real view radius this
    /// is `-3..=3` on both axes — **49** columns, not the 9 a 3×3 reading
    /// suggests. Transcribed from that call site rather than invented, because
    /// the whole magnitude of the bug is this number times the per-column cost.
    const SHELL_TICK_RADIUS: i32 = 3;

    fn shell_tick_area() -> (RangeInclusive<i32>, RangeInclusive<i32>) {
        (
            -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS,
            -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS,
        )
    }

    const EXPECTED_TICK_AREA_COLUMNS: usize =
        ((2 * SHELL_TICK_RADIUS + 1) * (2 * SHELL_TICK_RADIUS + 1)) as usize;

    /// How many random-tick *passes* the gates below observe — **not** how many
    /// ticks they drive.
    ///
    /// [`crate::tick::run_tick_loop`] is the only thing that calls
    /// `world.column()` here, and since issue #481 it skips its random-tick pass
    /// while `game_tick <= INITIAL_RANDOM_TICK_DEFERRAL_TICKS`. `game_tick` is
    /// incremented at the top of each iteration, so driving [`TICKS`] periods
    /// yields passes on ticks `INITIAL_RANDOM_TICK_DEFERRAL_TICKS + 1 ..= TICKS`,
    /// i.e. exactly this many.
    ///
    /// 12 rather than some other number because it is the figure this module's
    /// own doc comment and the negative control's `49 × 12 = 588` observation
    /// were recorded against — the *passes* count is what those numbers were
    /// always about; only the tick count had to move.
    const RANDOM_TICK_PASSES: u32 = 12;

    /// Tick periods to drive, derived from the deferral rather than restated:
    /// the deferral is a production knob, and a gate that hardcoded a tick
    /// count went to **zero** observed generations when it was introduced.
    const TICKS: u32 = INITIAL_RANDOM_TICK_DEFERRAL_TICKS as u32 + RANDOM_TICK_PASSES;

    // The deferral must not swallow the whole window, or both gates below
    // measure nothing while still reading as rigorous. Checked at compile time
    // so raising `INITIAL_RANDOM_TICK_DEFERRAL_TICKS` past `TICKS` is a build
    // failure rather than a silent pair of zeroes.
    const _: () = assert!(RANDOM_TICK_PASSES > 0);
    const _: () = assert!(TICKS as u64 > INITIAL_RANDOM_TICK_DEFERRAL_TICKS);

    /// Drives `run_tick_loop` for `ticks` virtual tick periods against `world`,
    /// returning nothing — the caller reads its own counter afterwards.
    ///
    /// Virtual time (`start_paused`), so this is immune to the box's load. The
    /// `yield_now` before and after are both required, not defensive: see
    /// `crate::tick`'s own tests for why the first one (the spawned task must
    /// reach its `Instant::now()` baseline before the first `advance`) and the
    /// second (the woken task must actually run its synchronous body).
    async fn drive_tick_loop<W: ChunkSource + 'static>(
        world: Arc<W>,
        area: (RangeInclusive<i32>, RangeInclusive<i32>),
        ticks: u32,
    ) -> Arc<TickClock> {
        drive_tick_loop_with_block_entities(world, area, ticks, BlockEntityHandle::default()).await
    }

    /// [`drive_tick_loop`], with the registry supplied by the caller — the arm
    /// the block-entity gates below need, since the whole question there is what
    /// a *populated* registry makes the tick loop ask the store for.
    ///
    /// A separate entry point rather than a fourth parameter on
    /// [`drive_tick_loop`] so the two column-generation gates above keep the
    /// exact call they were measured with.
    async fn drive_tick_loop_with_block_entities<W: ChunkSource + 'static>(
        world: Arc<W>,
        area: (RangeInclusive<i32>, RangeInclusive<i32>),
        ticks: u32,
        block_entities: BlockEntityHandle,
    ) -> Arc<TickClock> {
        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(
            MobHandle::new(ChunkWorld::new(REAL_MIN_Y, REAL_HEIGHT)),
            crate::mobs::LiveMobSource::default(),
            block_entities,
            Arc::clone(&clock),
            world,
            BlockTickFeed::default(),
            area,
            ExplosionFeed::default(),
            // Issue #468: this gate measures column generation, not persistence,
            // so a fresh handle -- behaviourally the locals this replaced.
            crate::region_source::ScheduledTickHandle::default(),
        ));
        tokio::task::yield_now().await;
        for _ in 0..ticks {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
        clock
    }

    /// **The load-bearing gate.** A column is generated exactly **once**, no
    /// matter how many ticks run over it.
    ///
    /// # Why a count and not a duration
    ///
    /// Counts are immune to machine load; durations are not — a 2.3× spread
    /// was measured on an identical release binary from load alone, while every
    /// count stayed byte-identical. So the assertion is
    /// `generated == distinct chunks`, never "the tick got faster".
    ///
    /// # Predicting the value, not the sign
    ///
    /// The two competing hypotheses are computed rather than compared: with a
    /// store, `RANDOM_TICK_PASSES × 49` visits produce **49** generations;
    /// without one they produce **`RANDOM_TICK_PASSES × 49`**. Those are not
    /// "more" and "less", they are two exact numbers a factor of
    /// [`RANDOM_TICK_PASSES`] apart, and the negative control below lands on the
    /// second.
    ///
    /// # Duration species
    ///
    /// `CountingSource` is constructed inside this test, so its counter has no
    /// life outside the gate. `TickClock` would have been the wrong instrument:
    /// it accumulates over a whole server lifetime. It is read here only as a
    /// precondition (did the loop actually run?), never as the measurement.
    #[tokio::test(start_paused = true)]
    async fn the_store_generates_each_column_exactly_once_across_many_ticks() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::new(counting));

        let clock = drive_tick_loop(Arc::clone(&store), shell_tick_area(), TICKS).await;

        // Precondition, failing rather than skipping: if the loop did not
        // really run many ticks, "generated once" is trivially true and this
        // gate measures nothing.
        assert!(
            clock.tick_count() >= u64::from(TICKS) - 1,
            "the tick loop only advanced {} ticks of {TICKS}; the count below would be \
             trivially satisfied",
            clock.tick_count()
        );

        let generated = calls.load(Ordering::Relaxed);
        assert_eq!(
            distinct_chunks(&per_chunk),
            EXPECTED_TICK_AREA_COLUMNS,
            "precondition: the loop must have visited the whole tick area, or the total \
             below could be right for the wrong reason"
        );
        assert_eq!(
            store.len(),
            EXPECTED_TICK_AREA_COLUMNS,
            "the store should hold the whole tick area ({EXPECTED_TICK_AREA_COLUMNS} columns)"
        );

        // The per-chunk figure, not just the total: a total can be right while
        // one chunk is regenerated N times and another never visited.
        let (worst_coord, worst_count) = worst_chunk(&per_chunk);
        assert_eq!(
            worst_count, 1,
            "chunk {worst_coord:?} was generated {worst_count} times over \
             {RANDOM_TICK_PASSES} random-tick passes; every column must be generated \
             exactly once"
        );
        assert_eq!(
            generated, EXPECTED_TICK_AREA_COLUMNS as u64,
            "expected exactly one generation per column of the tick area \
             ({EXPECTED_TICK_AREA_COLUMNS}); got {generated}. \
             {} would mean every chunk is still regenerated every pass.",
            EXPECTED_TICK_AREA_COLUMNS as u64 * u64::from(RANDOM_TICK_PASSES)
        );
        assert_eq!(
            store.evicted(),
            0,
            "the tick area must fit the default capacity without eviction, or the \
             steady state thrashes"
        );
    }

    /// **The negative control, and it must fail the assertion above.**
    ///
    /// `ChunkStore::with_capacity(source, 0)` retains nothing, so every read
    /// falls through to `source.column()` — bit-for-bit the pre-store
    /// behaviour, reproduced as a real *configuration* of the shipped type
    /// rather than as a temporary neuter, so the control is permanent.
    ///
    /// Observed when this landed: **588** generations for 49 columns over 12
    /// random-tick passes, i.e. exactly `49 × 12`, against 49 with the store. At
    /// the measured 909 ms per real column that is 44.5 s of generation per
    /// 50 ms tick budget.
    #[tokio::test(start_paused = true)]
    async fn without_retention_every_chunk_is_regenerated_every_tick() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(counting, 0));

        drive_tick_loop(Arc::clone(&store), shell_tick_area(), TICKS).await;

        let generated = calls.load(Ordering::Relaxed);
        // The per-chunk view of the same failure: without retention *every*
        // column is regenerated on *every* tick, so the worst chunk's count is
        // the tick count itself, not 1.
        let (worst_coord, worst_count) = worst_chunk(&per_chunk);
        assert_eq!(
            worst_count,
            u64::from(RANDOM_TICK_PASSES),
            "control: chunk {worst_coord:?} should have been regenerated once per random-tick \
             pass ({RANDOM_TICK_PASSES}), got {worst_count}"
        );
        assert_eq!(
            generated,
            EXPECTED_TICK_AREA_COLUMNS as u64 * u64::from(RANDOM_TICK_PASSES),
            "the zero-capacity control must reproduce the pre-store behaviour exactly: \
             {EXPECTED_TICK_AREA_COLUMNS} columns × {RANDOM_TICK_PASSES} passes. If this ever reports \
             {EXPECTED_TICK_AREA_COLUMNS} instead, retention has leaked into the control \
             and the positive gate above is no longer measuring anything."
        );
        assert_eq!(store.len(), 0, "a zero-capacity store must retain nothing");
    }

    /// The half of the fix that is not about the tick loop: reading **one
    /// block** must not regenerate a column.
    ///
    /// `crate::server`'s `vitals_tick` does exactly this every 50 ms once the
    /// client has sent a position, on the connection task — the task that
    /// streams chunks. Against the column-regenerating form (once
    /// `ChunkSource::block_state`'s default, now each non-retaining source's
    /// explicit choice — issue #440) each probe is a full column generation,
    /// which is why chunk streaming stops at the first movement packet rather
    /// than at join.
    ///
    /// Negative control in the same body: the unwrapped source, where the same
    /// 40 probes cost 40 generations.
    #[test]
    fn repeated_single_block_probes_generate_one_column_not_forty() {
        const PROBES: u64 = 40;

        // Control: the bare source, whose `block_state` is the explicit
        // column-regenerating form (what used to be the trait default).
        let bare = CountingSource::new();
        for _ in 0..PROBES {
            let _ = bare.block_state(5, 8, 5);
        }
        assert_eq!(
            bare.calls(),
            PROBES,
            "control: `CountingSource::block_state` regenerates a whole column per probe \
             (the column-regenerating form that was `ChunkSource`'s default before issue \
             #440). If this is not {PROBES}, the impl changed and the gate below is \
             measuring the wrong thing."
        );

        // Subject: the same probes through the store.
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let store = ChunkStore::new(counting);
        for _ in 0..PROBES {
            let _ = store.block_state(5, 8, 5);
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "{PROBES} probes of the same block must cost exactly one generation"
        );
        assert_eq!(store.len(), 1, "one column touched, one column resident");
    }

    /// Edits survive the store, in both directions that can lose them.
    ///
    /// 1. A `set_block` is visible to the very next read (the cache was
    ///    updated in place).
    /// 2. A `set_block` is visible **after eviction** (it was written through
    ///    to the inner source first, so the regeneration carries it).
    ///
    /// Property 2 is the one that licenses bounding the store at all. It is
    /// checked against `OverworldChunkSource`, because that is the only source
    /// in this crate with real retention beneath — a source whose `set_block`
    /// discards the edit (no retention) could not possibly pass, and testing
    /// against one would be a world-species vacuity.
    #[test]
    fn edits_survive_both_a_reread_and_an_eviction() {
        // Capacity 1, so touching a second column evicts the first
        // deterministically — no reliance on how many columns a real view
        // would have pushed through.
        let store = ChunkStore::with_capacity(crate::overworld_chunk_source(42), 1);

        let before = store.block_state(0, -50, 0);
        assert_ne!(
            before, "minecraft:diamond_block",
            "precondition: the generator must not already have placed the block this test \
             writes, or neither property below means anything"
        );

        store.set_block(0, -50, 0, "minecraft:diamond_block");

        // Property 1: visible immediately, from the resident column.
        assert_eq!(
            store.block_state(0, -50, 0),
            "minecraft:diamond_block",
            "an edit must be visible to the next read"
        );
        assert_eq!(
            store.column(0, 0).block_state(0, -50, 0),
            "minecraft:diamond_block",
            "an edit must be visible through `column()` too, not only `block_state()`"
        );

        // Force an eviction of (0, 0) by touching a different column.
        let _ = store.column(7, 7);
        assert!(
            store.evicted() >= 1,
            "precondition: the capacity-1 store must actually have evicted something, or \
             property 2 below is not testing eviction at all"
        );
        assert_eq!(store.len(), 1, "capacity 1 must hold exactly one column");

        // Property 2: still visible after the cached copy is gone, because the
        // regeneration goes back through `OverworldChunkSource::edits`.
        assert_eq!(
            store.block_state(0, -50, 0),
            "minecraft:diamond_block",
            "an edit must survive eviction of its cache entry — this is what makes the \
             store's capacity bound lossless"
        );
    }

    /// The bound is real, and it is the property that stops this fix from
    /// trading a starvation bug for an unbounded allocation.
    ///
    /// Also reports the measured per-column clone cost, since that is the one
    /// cost this design deliberately keeps (see the module docs) and a bare
    /// assertion would not record it.
    #[test]
    fn residency_is_bounded_and_the_clone_is_cheap() {
        const CAPACITY: usize = 32;
        const TOUCHED: i32 = 20; // 400 columns, far past the capacity

        let store = ChunkStore::with_capacity(CountingSource::full_height(), CAPACITY);
        for cz in 0..TOUCHED {
            for cx in 0..TOUCHED {
                let _ = store.column(cx, cz);
            }
        }

        assert_eq!(
            store.len(),
            CAPACITY,
            "residency must be pinned at the capacity bound, not grow with what was touched"
        );
        assert_eq!(
            store.evicted(),
            (TOUCHED * TOUCHED) as u64 - CAPACITY as u64,
            "every column past the bound must have been evicted exactly once"
        );
        assert_eq!(store.capacity(), CAPACITY);

        // Not an assertion on wall-clock — a recorded measurement, printed
        // with `--nocapture`. A dense full-height column is ~192 KiB, so the
        // clone `column()` returns is a memcpy of that; the point of recording
        // it is that it is microseconds against the 909 ms it replaces.
        let column = ChunkColumn::new(REAL_MIN_Y, REAL_HEIGHT);
        let started = std::time::Instant::now();
        const CLONES: u32 = 200;
        for _ in 0..CLONES {
            std::hint::black_box(column.clone());
        }
        let per_clone = started.elapsed() / CLONES;
        println!(
            "ChunkColumn clone ({REAL_HEIGHT} rows, ~{} KiB): {per_clone:?} each",
            16 * REAL_HEIGHT * 16 * 2 / 1024
        );
    }

    /// Eviction must be least-recently-used, not arbitrary — otherwise a
    /// capacity that comfortably holds the tick area still thrashes it, because
    /// the streamed view pushes hundreds of one-shot columns through the same
    /// store.
    #[test]
    fn eviction_drops_the_least_recently_used_column() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 2);
        let hot = (0, 0);
        let cold = (1, 0);

        let _ = store.column(hot.0, hot.1);
        let _ = store.column(cold.0, cold.1);
        // Touch `hot` again so `cold` is the least recently used.
        let _ = store.column(hot.0, hot.1);
        // A third column must evict `cold`, not `hot`.
        let _ = store.column(2, 0);

        assert_eq!(store.generated(), 3, "three distinct columns, three generations");
        // Re-reading `hot` must be free; re-reading `cold` must not.
        let before = store.generated();
        let _ = store.column(hot.0, hot.1);
        assert_eq!(
            store.generated(),
            before,
            "the most recently used column was evicted — eviction is not LRU"
        );
        let _ = store.column(cold.0, cold.1);
        assert_eq!(
            store.generated(),
            before + 1,
            "the least recently used column should have been the one evicted"
        );
    }

    /// The store must not change what the world *contains*, only how often it
    /// is computed. Without this, a store that returned blank or stale columns
    /// would pass every count above.
    #[test]
    fn retention_does_not_change_the_blocks() {
        let coords = [(0, 0), (1, -2), (-3, 5)];
        let probes = [(0, -60, 0), (7, 4, 9), (15, 70, 15)];

        // Independently constructed sources per arm, per this crate's own
        // determinism-test reasoning: the generator's memo cache would
        // otherwise make one arm a replay of the other.
        let bare = crate::overworld_chunk_source(7);
        let store = ChunkStore::new(crate::overworld_chunk_source(7));

        for &(cx, cz) in &coords {
            let bare_column = bare.column(cx, cz);
            // Read each column twice through the store: once a miss, once a
            // hit, so a hit that served something different would show up.
            for pass in 0..2 {
                let stored = store.column(cx, cz);
                for &(lx, y, lz) in &probes {
                    assert_eq!(
                        stored.block_state(lx, y, lz),
                        bare_column.block_state(lx, y, lz),
                        "pass {pass}: retained column ({cx}, {cz}) diverged at ({lx}, {y}, {lz})"
                    );
                }
                assert_eq!(
                    store.block_state(cx * 16 + probes[0].0, probes[0].1, cz * 16 + probes[0].2),
                    bare_column.block_state(probes[0].0, probes[0].1, probes[0].2),
                    "pass {pass}: the `block_state` override diverged from `column()`"
                );
            }
        }
    }

    /// A full-height column with every memory page actually **written**.
    ///
    /// `ChunkColumn::new` allocates through `vec![0u16; …]`, i.e. `alloc_zeroed`,
    /// and at 192 KiB that can be served by lazily-zeroed pages the process
    /// never faults in — so a store full of *untouched* columns would understate
    /// resident memory and the RSS measurement below would be a
    /// world-species vacuity (measuring an allocation pattern production never
    /// has, since a generated column is fully written). One write per 8 y-rows
    /// is enough to touch every page at any plausible page size.
    fn touched_column(min_y: i32, height: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(min_y, height);
        let mut y = min_y;
        while y < min_y + height {
            column.set_solid(y.rem_euclid(16), y, 0, true);
            y += 8;
        }
        column
    }

    struct TouchedSource;
    impl ChunkSource for TouchedSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            touched_column(REAL_MIN_Y, REAL_HEIGHT)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // Only `column()` is exercised here (the RSS measurement); this is
            // the plain column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // A memory-measurement fixture; nothing here writes blocks. Explicitly
        // discards rather than inheriting a silent default (issue #440).
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// Fills a store to `capacity` and holds it, so an external
    /// `/usr/bin/time -l` reading attributes the peak RSS to retention.
    fn fill_and_hold(capacity: usize, touch: usize) -> ChunkStore<TouchedSource> {
        let store = ChunkStore::with_capacity(TouchedSource, capacity);
        for i in 0..touch as i32 {
            let _ = store.column(i % 64, i / 64);
        }
        store
    }

    /// **Retained arm** of the RSS measurement. `#[ignore]`d: a measurement
    /// tool, not an assertion, and only meaningful in `--release` under
    /// `/usr/bin/time -l`.
    ///
    /// Run both arms and subtract. Per `docs/plans/chunk-lifecycle.md`'s U2,
    /// **the pair is its own control**: if the delta is ≈0 the measurement is
    /// broken (columns dropped in both arms, or pages not faulted in — see
    /// [`touched_column`]), and the run must be treated as a failure to measure
    /// rather than as "residency is free".
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib -- --ignored --nocapture \
    ///     --exact chunk_store::tests::measure_rss_with_retention
    /// ```
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_with_retention() {
        let store = fill_and_hold(DEFAULT_CAPACITY, DEFAULT_CAPACITY);
        assert_eq!(store.len(), DEFAULT_CAPACITY);
        println!(
            "retained {} columns of {} rows (~{} KiB each); arithmetic ceiling {} MiB",
            store.len(),
            REAL_HEIGHT,
            16 * REAL_HEIGHT * 16 * 2 / 1024,
            (DEFAULT_CAPACITY as i32 * 16 * REAL_HEIGHT * 16 * 2) / (1024 * 1024)
        );
        std::hint::black_box(&store);
    }

    /// **Dropped arm** — identical work, retention disabled. The difference
    /// between this arm's peak RSS and the one above is the store's real cost.
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_without_retention() {
        let store = fill_and_hold(0, DEFAULT_CAPACITY);
        assert_eq!(store.len(), 0);
        println!("retained 0 columns after touching {DEFAULT_CAPACITY}");
        std::hint::black_box(&store);
    }

    /// The premise everything else rests on: what a **real** composed column
    /// costs in release.
    ///
    /// A fresh, independently constructed source per column, because
    /// `OverworldGenerator`'s 512-entry memo cache would otherwise turn every
    /// column after the first into a cache hit and report a per-column cost
    /// near zero — the same trap that made `crate::chunk`'s determinism test
    /// vacuous.
    ///
    /// Reports, never asserts: a duration on a shared box is a sample, not a
    /// measurement (a 2.3× spread was measured on an identical release binary
    /// from load alone), so a threshold here would be a flake generator. The
    /// *count* gates above are what protect the fix.
    #[test]
    #[ignore = "measurement tool; run in --release, and only on a quiet machine"]
    fn measure_real_column_generation_cost() {
        const COLUMNS: usize = 4;
        let mut total = std::time::Duration::ZERO;
        for i in 0..COLUMNS as i32 {
            let source = crate::overworld_chunk_source(42);
            let started = std::time::Instant::now();
            let column = source.column(i * 37, i * 53);
            let elapsed = started.elapsed();
            std::hint::black_box(&column);
            println!("column {i}: {elapsed:?}");
            total += elapsed;
        }
        println!(
            "mean over {COLUMNS} cold columns: {:?} — compare the 50 ms tick budget, and \
             multiply by the 49-column tick area",
            total / COLUMNS as u32
        );
    }

    /// A miss must not hold the cache lock, or `generate_columns_parallel`'s
    /// scoped fan-out is serialised behind it and issue #414 is undone.
    ///
    /// # Predicting the value, not the sign
    ///
    /// Eight columns at 60 ms each through the store: if generation runs with
    /// the lock released the burst takes about `8 / workers × 60 ms` — under
    /// 240 ms at any `available_parallelism ≥ 2`. If the lock is held it takes
    /// **≥ 480 ms** (8 × 60 ms, fully serial). The gate asserts under 400 ms,
    /// which sits between the two hypotheses rather than merely below the
    /// serial one.
    ///
    /// This is the one gate here that reads a duration, because "does a lock
    /// serialise this" has no count. It is bracketed to a single burst and the
    /// two hypotheses are 2× apart, so the load spread that makes durations
    /// untrustworthy would have to exceed 2× to flip it. Skipped rather than
    /// failed on a single-core box, where the question is meaningless.
    #[test]
    fn a_miss_does_not_hold_the_lock_across_generation() {
        struct SleepySource {
            per_column: std::time::Duration,
        }
        impl ChunkSource for SleepySource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                std::thread::sleep(self.per_column);
                ChunkColumn::new(0, 16)
            }

            fn block_state(&self, x: i32, y: i32, z: i32) -> String {
                // Only `column()` is exercised here (the lock-serialisation
                // gate); the plain column-regenerating form, for completeness.
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                let lx = x.rem_euclid(16);
                let lz = z.rem_euclid(16);
                self.column(cx, cz).block_state(lx, y, lz).to_string()
            }

            // A wall-clock-only fixture; nothing here writes blocks. Explicitly
            // discards rather than inheriting a silent default (issue #440).
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
                // No storage; edits are discarded by design for this fixture.
            }
        }

        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        if workers < 2 {
            println!("skipping: single-core box, parallelism is not observable");
            return;
        }

        let per_column = std::time::Duration::from_millis(60);
        let store = ChunkStore::new(SleepySource { per_column });
        let coords: Vec<(i32, i32)> = (0..8).map(|i| (i, 0)).collect();

        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            for chunk in coords.chunks(8 / workers.min(8).max(1)) {
                let store = &store;
                scope.spawn(move || {
                    for &(cx, cz) in chunk {
                        let _ = store.column(cx, cz);
                    }
                });
            }
        });
        let elapsed = started.elapsed();

        assert!(
            elapsed < per_column * 8 * 2 / 3,
            "8 misses at {per_column:?} each took {elapsed:?}; fully serial would be \
             {:?}. The cache lock is being held across `source.column()`, which serialises \
             `generate_columns_parallel`'s fan-out.",
            per_column * 8
        );
        assert_eq!(store.generated(), 8, "each distinct column generated once");
    }

    // ---------------------------------------------------------------------
    // Issue #503's block-entity lead: is the 20 Hz registry scan a *second*,
    // CPU-side, distance-dependent term? See
    // `docs/block-entity-tick-distance.md` for the full write-up. The counter
    // is `CountingSource::per_chunk` for the *remote* column, and the two
    // competing hypotheses are computed from constants below rather than
    // compared as "more" and "less".
    // ---------------------------------------------------------------------

    /// The chunk a "walked away from" hopper sits in — 1,600 blocks out, the
    /// same stroll length `docs/worldgen-store-distance-leak.md` measured its
    /// memory term over, and far outside [`shell_tick_area`] so the random-tick
    /// pass never touches it. Whether this column is resident is therefore
    /// decided by the block-entity scan alone, which is the whole point.
    const REMOTE_CHUNK: (i32, i32) = (100, 100);

    /// A y inside [`CountingSource::full_height`]'s extent, so
    /// `ChunkColumn::block_state` indexes in range.
    const REMOTE_Y: i32 = 64;

    fn remote_pos(chunk: (i32, i32)) -> BlockPos {
        BlockPos::new(chunk.0 * 16 + 8, REMOTE_Y, chunk.1 * 16 + 8)
    }

    /// A registry holding one hopper at `chunk`, and nothing else.
    ///
    /// A hopper specifically, not any of the other four kinds: it is the **only**
    /// variant whose tick reaches the world at all —
    /// `tick_all_with_hopper_lock`'s `enabled` closure is called for
    /// `BlockEntity::Hopper` and for nothing else, and that closure is the
    /// `world.block_state` call the lead is about.
    fn registry_with_one_hopper(chunk: (i32, i32)) -> BlockEntityHandle {
        let handle = BlockEntityHandle::new();
        handle.with(|registry| {
            registry.insert(
                remote_pos(chunk),
                crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
            );
        });
        handle
    }

    fn generations_for(per_chunk: &PerChunk, chunk: (i32, i32)) -> u64 {
        per_chunk
            .lock()
            .expect("per-chunk map poisoned")
            .get(&chunk)
            .copied()
            .unwrap_or(0)
    }

    /// **The subject.** A hopper whose column the player has walked away from
    /// costs **one** column generation in total, not one per tick.
    ///
    /// # The two hypotheses, both computed outside the measurement
    ///
    /// `tick.rs`'s block-entity arm really does call `world.block_state` per
    /// hopper, every tick, and `ChunkStore::block_state` really does regenerate
    /// a whole column on a miss (`ensure` → `source.column`). Issue #503's lead
    /// reads those two facts together and predicts **[`TICKS`]** generations of
    /// the remote column — one per tick, ~50 ms each against a 50 ms budget.
    ///
    /// The competing prediction is **1**, and the reason is the line the lead
    /// does not account for: *the miss is self-healing*. `ensure` inserts with
    /// the newest stamp and `read` refreshes `last_used` on every hit, so a
    /// position polled at 20 Hz is permanently among the most-recently-used
    /// entries. `evict_down_to` takes the **minimum** stamp, so evicting this
    /// column would require [`DEFAULT_CAPACITY`] *other* distinct columns to be
    /// touched inside one 50 ms tick period. The 20 Hz scan does not merely fail
    /// to evict the column — it **pins** it.
    ///
    /// So the two hypotheses are 1 and 52, a factor of [`TICKS`] apart, and this
    /// gate lands on one of them. `without_retention_a_remote_hopper_is_a_cold_
    /// column_every_single_tick` below is the same rig landing on the other, so
    /// the instrument is known to be able to report 52.
    #[tokio::test(start_paused = true)]
    async fn a_walked_away_hopper_costs_one_column_generation_not_one_per_tick() {
        let counting = CountingSource::full_height();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::new(counting));

        let clock = drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        assert!(
            clock.tick_count() >= u64::from(TICKS) - 1,
            "precondition: the loop only advanced {} ticks of {TICKS}, so \"one generation\" \
             would be trivially true",
            clock.tick_count()
        );

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote, 1,
            "the remote hopper's column {REMOTE_CHUNK:?} was generated {remote} times over \
             {TICKS} ticks. 0 would mean the block-entity scan never reached the world at all \
             and this gate measures nothing; {TICKS} is issue #503's lead — one cold column per \
             tick; 1 is the store pinning the column it just polled."
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            EXPECTED_TICK_AREA_COLUMNS as u64 + 1,
            "the whole session should cost the {EXPECTED_TICK_AREA_COLUMNS}-column tick area \
             plus exactly one remote column"
        );
        assert_eq!(
            store.evicted(),
            0,
            "precondition on the mechanism: 49 tick-area columns + 1 remote is far under \
             {DEFAULT_CAPACITY}, so nothing should be evicted. If this is ever non-zero the \
             pinning argument above needs re-deriving, not the assertion relaxing."
        );
    }

    /// **The negative control, and it lands on the lead's own number.**
    ///
    /// `with_capacity(source, 0)` retains nothing, so every poll of the remote
    /// hopper is a cold column — exactly the failure issue #503's lead
    /// describes, reproduced as a real *configuration* of the shipped type
    /// rather than a temporary neuter. Predicted **[`TICKS`]** (the
    /// block-entity scan runs on every tick, unlike the random-tick pass, which
    /// `INITIAL_RANDOM_TICK_DEFERRAL_TICKS` holds off), and that is what makes
    /// the subject's `1` a measurement rather than an absence.
    ///
    /// Without this arm the subject is the *assertion* species of vacuity: a
    /// registry that was never scanned, a hopper that was never ticked and a
    /// closure that was never called would all also report a low number.
    #[tokio::test(start_paused = true)]
    async fn without_retention_a_remote_hopper_is_a_cold_column_every_single_tick() {
        let counting = CountingSource::full_height();
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(counting, 0));

        drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote,
            u64::from(TICKS),
            "control: with retention off the remote hopper's column must be regenerated on \
             every one of {TICKS} ticks, got {remote}. If this ever reports 1, retention has \
             leaked into the control and the subject above is no longer measuring anything."
        );
    }

    /// **The curve, in counter form: flat in distance.**
    ///
    /// The claim under test is that walking away introduces a term that grows
    /// with distance. `world.block_state`'s cost has no coordinate in it — the
    /// store is a `HashMap<(i32, i32), _>` and the LRU stamp is a counter — so
    /// the prediction is the *same* number at every band, including one a
    /// million blocks out, matching `docs/worldgen-store-distance-leak.md`'s
    /// finding that per-column cost is itself flat to 1,048,576 blocks.
    ///
    /// The bands double as the control the walk investigation used (nine runs at
    /// one distance gave a 1.01× spread): here the arms differ *only* in the
    /// hopper's coordinate, so any spread at all is a distance term. A count has
    /// no spread to explain away, which is why this is a counter and not a
    /// timing.
    #[tokio::test(start_paused = true)]
    async fn the_registry_scan_costs_the_same_at_every_distance_from_the_tick_area() {
        // 64 blocks (just outside the 49-column tick area) to 16,000,000.
        const BANDS: [(i32, i32); 4] = [(4, 4), (100, 100), (10_000, 10_000), (1_000_000, 1_000_000)];

        for chunk in BANDS {
            let counting = CountingSource::full_height();
            let calls = Arc::clone(&counting.calls);
            let per_chunk = Arc::clone(&counting.per_chunk);
            let store = Arc::new(ChunkStore::new(counting));

            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                registry_with_one_hopper(chunk),
            )
            .await;

            assert_eq!(
                generations_for(&per_chunk, chunk),
                1,
                "band {chunk:?} ({} blocks out): the remote column must be generated exactly \
                 once, as at every other band. A band-dependent count here is the \
                 distance-dependent CPU term issue #503's lead predicts.",
                chunk.0 * 16
            );
            assert_eq!(
                calls.load(Ordering::Relaxed),
                EXPECTED_TICK_AREA_COLUMNS as u64 + 1,
                "band {chunk:?}: total generations must be the tick area plus one, identically \
                 at every distance"
            );
        }
    }

    /// **The arm that isolates the mechanism, and the one that says the lead is
    /// real.** Once the store is over its ceiling, the 20 Hz-polled remote
    /// column becomes a cold generation **once per random-tick pass** — on the
    /// tick thread, against a 50 ms budget.
    ///
    /// # Why this arm exists
    ///
    /// Without it the subject above has a second, much weaker explanation: at
    /// the default render distance the working set (289-column view + 49-column
    /// tick area = 338) never reaches [`DEFAULT_CAPACITY`] at all, so "no
    /// eviction" could be pure headroom rather than any property of the polling.
    /// `with_capacity(source, EXPECTED_TICK_AREA_COLUMNS)` removes the headroom:
    /// 49 tick-area columns plus one remote is 50 against a ceiling of 49.
    ///
    /// # A wrong prediction, kept
    ///
    /// This arm was written predicting **1**, on the argument that
    /// `ChunkStore::read` refreshes `last_used` on every hit and `ensure`
    /// inserts with the newest stamp, so a position polled at 20 Hz is
    /// permanently among the most-recently-used entries and `evict_down_to`
    /// (which takes the **minimum**) would always prefer a stale tick-area
    /// column. It measured **12** and the argument is wrong, for a reason worth
    /// keeping: the block-entity scan runs *once* per tick and the random-tick
    /// pass then touches 49 columns *after* it, so by the end of a pass the
    /// remote column's stamp is the oldest in the map, not the newest. Being
    /// polled at 20 Hz does not pin a column when something else touches 49
    /// columns in the same 50 ms. The pin only ever came from headroom.
    ///
    /// # The prediction, derived
    ///
    /// One cold generation on the first poll, then one per random-tick pass
    /// thereafter — except the final pass's eviction has no following poll
    /// inside the window, so `1 + (RANDOM_TICK_PASSES - 1)` =
    /// [`RANDOM_TICK_PASSES`]. The three candidate values are therefore 1 (the
    /// headroom regime the subject measures), 12 (this regime) and [`TICKS`]
    /// (no retention at all), and this arm lands on the middle one.
    #[tokio::test(start_paused = true)]
    async fn an_over_capacity_store_makes_the_polled_column_cold_every_pass() {
        let counting = CountingSource::full_height();
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(
            counting,
            EXPECTED_TICK_AREA_COLUMNS,
        ));

        drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        assert!(
            store.evicted() > 0,
            "precondition: a capacity of {EXPECTED_TICK_AREA_COLUMNS} against a \
             {EXPECTED_TICK_AREA_COLUMNS}-column tick area plus one remote column must actually \
             evict, or this arm is the same measurement as the subject and proves nothing"
        );
        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote,
            u64::from(RANDOM_TICK_PASSES),
            "over capacity, the remote hopper's column must be cold once per random-tick pass \
             ({RANDOM_TICK_PASSES}), got {remote}. 1 would mean the polling pins it (it does \
             not — see this test's doc comment); {TICKS} would mean retention had stopped \
             working entirely."
        );
    }

    /// **The curve, and the threshold it turns on.** Cold generations per tick
    /// are ~0 while the accumulated block-entity columns fit under
    /// [`DEFAULT_CAPACITY`] alongside the tick area, and **one per entity per
    /// tick** the moment they do not.
    ///
    /// This is the term issue #503's lead is really about, once the arms above
    /// have located it. `BlockEntityRegistry` has no unload path, so the set of
    /// positions the 20 Hz scan probes only ever grows with exploration. Each
    /// probed position holds one column resident. So exploration does not make
    /// any single call slower — it walks the *working set* across the store's
    /// fixed ceiling, and the miss rate goes from 0 to 1 over a narrow band.
    ///
    /// # Both hypotheses computed from constants, and the boundary is exact
    ///
    /// The rig's resident set is `EXPECTED_TICK_AREA_COLUMNS + entities`. Below
    /// the ceiling every column is generated exactly **once** for the whole
    /// session, so remote generations total `entities`. Above it the access
    /// pattern is a **cyclic** scan of `entities` positions through an LRU of
    /// [`DEFAULT_CAPACITY`], which is LRU's worst case — by the time the scan
    /// returns to a position it has touched every other one, so **every** probe
    /// misses and the total is `entities × TICKS`.
    ///
    /// At `entities = UNDER` those are 400 and 20,800; at `entities = OVER` they
    /// are 600 and 31,200. Four numbers, no fitting, and the two regimes are a
    /// factor of [`TICKS`] apart rather than "more" and "less".
    ///
    /// Production's own threshold is lower than this rig's, because production
    /// also holds a 289-column view: `512 - 289 - 49` = **174** distinct
    /// hopper-bearing *chunks*, since only `BlockEntity::Hopper` probes the world
    /// at all (see `sixteen_hundred_opaque_block_entities_never_reach_the_store`)
    /// and hoppers sharing a chunk share one column.
    ///
    /// # This arm characterises a defect, it does not bless it
    ///
    /// The over-capacity band is **not** desired behaviour — it is issue #503's
    /// block-entity lead, measured. If a fix lands (vanilla ticks block entities
    /// per *loaded chunk*, so the scan should be bounded by the view rather than
    /// by everything the registry ever saw) this band goes red, and the right
    /// response is to rewrite it against the new bound, not to relax it. See
    /// `docs/block-entity-tick-distance.md`.
    #[tokio::test(start_paused = true)]
    async fn the_miss_rate_crosses_from_zero_to_one_when_the_registry_outgrows_the_store() {
        /// Comfortably under the ceiling: `49 + 400 = 449 <= 512`.
        const UNDER: i32 = 400;
        /// Over it: `49 + 600 = 649 > 512`.
        const OVER: i32 = 600;

        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + UNDER as usize <= DEFAULT_CAPACITY);
        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + OVER as usize > DEFAULT_CAPACITY);

        for (entities, over_capacity) in [(UNDER, false), (OVER, true)] {
            let handle = BlockEntityHandle::new();
            handle.with(|registry| {
                for i in 0..entities {
                    registry.insert(
                        remote_pos((2_000 + i, 2_000 + i)),
                        crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
                    );
                }
            });

            // A shorter column than `full_height` for this arm alone: the
            // over-capacity band generates ~31,200 columns, and at 192 KiB each
            // that is allocation churn the measurement does not need. The count
            // is what is under test, not the column's size.
            let counting = CountingSource::sized(0, 128);
            let calls = Arc::clone(&counting.calls);
            let store = Arc::new(ChunkStore::new(counting));

            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                handle,
            )
            .await;

            let total = calls.load(Ordering::Relaxed);
            let remote = total - EXPECTED_TICK_AREA_COLUMNS as u64;
            // Printed as well as asserted: the doc quotes these, and a curve is
            // more use to the next reader than a bare pass.
            eprintln!(
                "entities {entities:>4}  resident-set {:>4}  ceiling {DEFAULT_CAPACITY}  \
                 remote generations {remote:>6}  evictions {:>6}  per tick {:>6.1}",
                EXPECTED_TICK_AREA_COLUMNS + entities as usize,
                store.evicted(),
                remote as f64 / f64::from(TICKS)
            );
            if over_capacity {
                let predicted = u64::from(entities as u32) * u64::from(TICKS);
                assert!(
                    remote >= predicted * 9 / 10,
                    "over capacity ({entities} entities + \
                     {EXPECTED_TICK_AREA_COLUMNS} tick area > {DEFAULT_CAPACITY}): a cyclic scan \
                     through a smaller LRU must miss on essentially every probe, so remote \
                     generations should approach {entities} × {TICKS} = {predicted}; got \
                     {remote}. {entities} would mean the store was still absorbing them."
                );
                assert!(
                    store.evicted() > 0,
                    "precondition: the over-capacity band must actually evict"
                );
            } else {
                assert_eq!(
                    remote,
                    u64::from(entities as u32),
                    "under capacity ({entities} entities + {EXPECTED_TICK_AREA_COLUMNS} tick \
                     area <= {DEFAULT_CAPACITY}): each column must be generated exactly once for \
                     the whole session, so {entities} total. {} would be one per tick.",
                    u64::from(entities as u32) * u64::from(TICKS)
                );
                assert_eq!(
                    store.evicted(),
                    0,
                    "precondition: the under-capacity band must not evict, or it is not the \
                     regime it claims to be"
                );
            }
        }
    }

    /// The registry is scanned **unfiltered** — and the 1,608-of-1,613
    /// unsimulated kinds issue #477 round-trips as
    /// [`BlockEntity::Opaque`](crate::block_entities::BlockEntity::Opaque) reach
    /// the world **zero** times regardless.
    ///
    /// `tick_all_with_hopper_lock` does not filter: it collects *every* key into
    /// a fresh `Vec` and dispatches on the variant, so a vanilla world's whole
    /// block-entity population is walked at 20 Hz. That is a real property and
    /// worth pinning — but the cost of an `Opaque` entry is a `Vec` push, two
    /// hash probes and an empty `tick_non_hopper` arm. **No coordinate of it
    /// reaches the store**, which is what this gate measures: with #477's own
    /// figure of 1,608 opaque entities scattered over distinct far-flung
    /// columns, the store still generates only the 49-column tick area.
    ///
    /// The competing hypothesis is `49 + 1608`: if the scan probed the world per
    /// entry — the shape the lead attributes to it — every one of those columns
    /// would be cold.
    #[tokio::test(start_paused = true)]
    async fn sixteen_hundred_opaque_block_entities_never_reach_the_store() {
        /// Issue #477's measured figure for a real vanilla world: 1,608 of its
        /// 1,613 block entities are kinds this crate does not simulate.
        const OPAQUE_ENTITIES: i32 = 1_608;

        let handle = BlockEntityHandle::new();
        handle.with(|registry| {
            for i in 0..OPAQUE_ENTITIES {
                // One per distinct column, all outside the tick area, so a
                // per-entry world probe would be a distinct cold generation and
                // could not be absorbed by the store.
                registry.insert(
                    remote_pos((1_000 + i, 1_000 + i)),
                    crate::block_entities::BlockEntity::Opaque {
                        id: "minecraft:chest".to_owned(),
                        nbt: lodestone_core::Nbt::End,
                    },
                );
            }
        });
        assert_eq!(
            handle.with(|registry| registry.len()),
            OPAQUE_ENTITIES as usize,
            "precondition: the registry must really hold all {OPAQUE_ENTITIES} entries, or the \
             count below is right for the wrong reason"
        );

        let counting = CountingSource::full_height();
        let calls = Arc::clone(&counting.calls);
        let store = Arc::new(ChunkStore::new(counting));

        drive_tick_loop_with_block_entities(Arc::clone(&store), shell_tick_area(), TICKS, handle)
            .await;

        assert_eq!(
            calls.load(Ordering::Relaxed),
            EXPECTED_TICK_AREA_COLUMNS as u64,
            "the tick area and nothing else. {} would mean the unfiltered scan probes the \
             world per entry.",
            EXPECTED_TICK_AREA_COLUMNS as u64 + OPAQUE_ENTITIES as u64
        );
    }
}
