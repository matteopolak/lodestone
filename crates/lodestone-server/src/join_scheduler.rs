//! The join burst's generation scheduler: a **primed sliding window** over the
//! wire order, replacing the per-ring barrier (`docs/plans/worldgen-rewrite.md`
//! Unit 10).
//!
//! # What the barrier was, and why it can go
//!
//! `crate::server`'s join loop used to walk `crate::server`'s `join_view_rings`
//! and, for each ring, spawn every one of its columns into the blocking pool and
//! **wait for all of them** before asking for the next ring. That is a barrier:
//! ring `r + 1` could not start until ring `r`'s *slowest* column finished, so the
//! per-ring tails stacked. `5104adf` removed it by spawning all 289–361 columns at
//! once and `4307b59` reverted that — *"cache contention with 289 concurrent
//! generator calls"*.
//!
//! Two separate defects were live in that revert, and only one of them was the
//! cache:
//!
//! | defect | fixed by |
//! |---|---|
//! | two `Mutex`-guarded FIFO memo caches recomputing on a racing miss, ~5,000 lock attempts on one `Arc<Mutex>` | Unit 6's staged sharded store (`docs/worldgen-staged-store.md`) |
//! | **in-flight column count scaling with *view radius*** — 289 concurrent blocking-pool threads on an 8-core machine | this module |
//!
//! Unit 6 removed the first: a stage now computes exactly once regardless of
//! arrival order (measured 441/361 exactly, 3 of 3 concurrent 289-column bursts,
//! against the old cache's varying 452/452/448 and 380/383/372). So the barrier's
//! stated rationale — "ring 0 seeds the cache" — describes nothing.
//!
//! The second defect was never the cache's fault and removing the barrier alone
//! would reintroduce it. So this module does not go back to `5104adf`'s flat
//! fan-out: **the in-flight count is derived from `available_parallelism`, not
//! from the view radius** ([`generation_window`]). That is the whole difference,
//! and it is why this is a *scheduler* rather than a deletion.
//!
//! # The dependency edges it schedules on
//!
//! From the plan's parallel model. For a column `C`:
//!
//! * fill / surface / carve depend on the seed alone — embarrassingly parallel;
//! * `ore(C)` reads `pre_ore(3×3(C))`;
//! * `veg(C)` reads `post_ore(3×3(C))`, which closes over `pre_ore(5×5(C))`;
//! * `top_layer(C)` depends on `veg(C)` alone.
//!
//! So two columns at Chebyshev distance ≥ 5 share **no** store entry and are
//! wholly independent; adjacent columns share 20 of their 25 pre-ore entries.
//! Those shared entries are the real dependency edges, and Unit 6's per-entry
//! `OnceLock` is what honours them: a second worker arriving mid-computation
//! *waits for the value* instead of computing its own copy. **That is the
//! synchronisation the barrier was standing in for**, and it is per-edge rather
//! than per-ring — two workers block only when they need the same chunk's same
//! stage at the same instant.
//!
//! Because the window is a **contiguous** window over the outward ring order, the
//! in-flight set is always spatially local, so those shared edges are hits rather
//! than independent cold computations. Nothing here needs to know that; it falls
//! out of scheduling in wire order.
//!
//! # Why "primed"
//!
//! Issue #453 is the property that the player's own column reaches the client
//! after **one** column of generation, not after the whole view. A plain sliding
//! window would break it: the window is filled *before* the head is awaited, so on
//! a fast source the entire window completes before the first emit and
//! "columns generated before the first chunk was encoded" jumps from 1 to `window`.
//!
//! So the window is **1 for the first column and `window` thereafter**
//! ([`ColumnPipeline::next`]). The head column is generated alone, which is
//! exactly the one-column serialisation #453 already bought and documented as a
//! deliberate trade; every column after it runs with the window fully open. The
//! barrier is deleted for rings 1..=r and kept, deliberately, for the single
//! column of ring 0.
//!
//! This is a **counter**, not a timing: `join_scheduler_gates.rs` asserts
//! `columns_completed_before_first_emit == 1` on both arms and shows the pre-#453
//! flat shape reporting 289.
//!
//! # Only the `SourceRef::Shared` arm is scheduled, and that is not an oversight
//!
//! `SourceRef::Borrowed` (the transport tests) holds a source that is not
//! `'static`, so it cannot be spawned at all: every batch on that arm is a
//! `generate_columns_parallel` call that blocks until the whole batch finishes.
//! A window's entire payoff is overlapping generation with the *encode* of an
//! already-finished column, and a blocking source has nothing to overlap. Worse,
//! measured while building `join_scheduler_gates.rs`: the rings' cumulative sizes
//! are `1 + 4r(r + 1)`, which is always ≡ 1 (mod 8), so at a window of 8 no
//! window-sized batch even *straddles* a ring boundary — the split would replace
//! ring 8's single 64-column batch with eight serial ones and add barriers rather
//! than remove them. So that arm keeps the rings, and what is held identical
//! across the two arms is the **wire order**, which is what the client sees and
//! what both `805a1fb` gates assert.
//!
//! # What this module does *not* change
//!
//! The wire order. `crate::server`'s `join_view_rings` still decides it, this
//! pipeline emits strictly in the order it was handed, and
//! `the_shared_arm_streams_the_view_outward_too` /
//! `join_streams_the_view_outward_from_the_players_own_column` still gate it on
//! both `SourceRef` arms. Emitting in *completion* order would be the natural
//! mistake and it is what
//! `tests::control_completion_order_is_not_input_order` exists to reject.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::chunk::{ChunkColumn, ChunkSource};

/// How many columns the join burst keeps in flight once primed, derived from the
/// machine rather than from the view.
///
/// `2 × available_parallelism`, floored at 2:
///
/// * **the factor of 2** is the encode overlap. The caller awaits one column,
///   then writes it to the socket; with a window of exactly `parallelism` the
///   pool would be idle for the whole of that write. Doubling keeps a full set of
///   workers resident while the emitted column is being encoded, which is the
///   entire reason a window beats a barrier.
/// * **the floor of 2** is what makes a window a window. At 1 this degenerates to
///   the fully serial shape, and the ring-overlap detector in
///   `join_scheduler_gates.rs` would be vacuous on a machine reporting
///   `available_parallelism() == 1`.
/// * **there is no ceiling, and that is the point.** `5104adf`'s in-flight count
///   was `(2r + 1)²` — it grew with the *view radius*, which is why an 8-core
///   machine ran 289 concurrent generator calls. This one grows with cores, so
///   the pathological case is unreachable by construction at any view radius.
///
/// # The store interaction, stated rather than left to be discovered
///
/// Each in-flight `column()` call pins its own 5×5 pre-ore neighbourhood (25
/// entries) in the staged store, and `STORE_RETENTION` is 512. At `2 × P` in
/// flight the pinned set therefore passes 512 somewhere above 10 cores, and
/// reclamation cannot fire for the duration of the burst. That is **safe** —
/// nothing evicted means nothing recomputed, which is what licenses reading the
/// stage counters as one-per-chunk — and it is not a change: the per-ring loop
/// held a whole ring in flight, up to 72 columns at `view_radius = 9`, so it
/// exceeded the same bound by more.
#[must_use]
pub fn generation_window() -> usize {
    generation_window_for(
        std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4),
    )
}

/// [`generation_window`]'s arithmetic, split out so it is testable without
/// depending on the host's core count.
#[must_use]
pub fn generation_window_for(parallelism: usize) -> usize {
    (2 * parallelism.max(1)).max(2)
}

/// A primed sliding window over a fixed coordinate order.
///
/// [`next`](Self::next) tops the in-flight set up to the window, then awaits and
/// emits the **oldest** one — so emission order is exactly the order the
/// coordinates were handed in, independent of which column finished first. See the
/// module doc for why the first top-up is to 1 rather than to `window`.
///
/// # wasm32
///
/// There is no blocking pool, so the window is forced to 1 and columns are
/// generated inline — the unchanged behaviour of a target that never had a second
/// thread. Same as `crate::chunk::generate_columns_offloaded`'s `cfg`.
pub struct ColumnPipeline<S> {
    source: Arc<S>,
    coords: Vec<(i32, i32)>,
    /// Index of the next coordinate to hand to the pool.
    next_spawn: usize,
    /// Index of the next coordinate to emit. `coords[next_emit]` is the position
    /// paired with the front of `inflight`, which is what makes emission order a
    /// pure function of `coords`.
    next_emit: usize,
    window: usize,
    /// Set once the head column has been emitted. Until then the window is 1.
    primed: bool,
    #[cfg(not(target_arch = "wasm32"))]
    inflight: VecDeque<tokio::task::JoinHandle<ChunkColumn>>,
    #[cfg(target_arch = "wasm32")]
    inflight: VecDeque<ChunkColumn>,
}

impl<S> std::fmt::Debug for ColumnPipeline<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnPipeline")
            .field("columns", &self.coords.len())
            .field("window", &self.window)
            .field("spawned", &self.next_spawn)
            .field("emitted", &self.next_emit)
            .field("inflight", &self.inflight.len())
            .finish_non_exhaustive()
    }
}

impl<S: ChunkSource + 'static> ColumnPipeline<S> {
    /// A pipeline over `coords` with the machine-derived [`generation_window`].
    #[must_use]
    pub fn new(source: Arc<S>, coords: Vec<(i32, i32)>) -> Self {
        Self::with_window(source, coords, generation_window())
    }

    /// A pipeline with an explicit window, for gates that must not vary with the
    /// host's core count.
    #[must_use]
    pub fn with_window(source: Arc<S>, coords: Vec<(i32, i32)>, window: usize) -> Self {
        Self {
            source,
            coords,
            next_spawn: 0,
            next_emit: 0,
            window: window.max(1),
            primed: false,
            inflight: VecDeque::new(),
        }
    }

    /// The window this pipeline was built with.
    #[must_use]
    pub fn window(&self) -> usize {
        self.window
    }

    /// How many columns have yet to be emitted.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.coords.len() - self.next_emit
    }

    /// The next column in coordinate order, or `None` once the view is drained.
    ///
    /// Ordering is load-bearing for the wire and is *not* a property of the pool:
    /// the front of `inflight` always corresponds to `coords[next_emit]`, so a
    /// column that finishes early simply sits in the queue.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn next(&mut self) -> Option<((i32, i32), ChunkColumn)> {
        if self.next_emit >= self.coords.len() {
            return None;
        }
        // The first top-up is to 1, not to `window`: issue #453's
        // time-to-first-chunk. See the module doc.
        let target = if self.primed { self.window } else { 1 };
        while self.inflight.len() < target && self.next_spawn < self.coords.len() {
            let (cx, cz) = self.coords[self.next_spawn];
            let source = Arc::clone(&self.source);
            self.inflight
                .push_back(tokio::task::spawn_blocking(move || source.column(cx, cz)));
            self.next_spawn += 1;
        }
        let handle = self
            .inflight
            .pop_front()
            .expect("the top-up above spawns at least one column while any remain");
        let column = handle.await.expect("worldgen join burst panicked");
        let pos = self.coords[self.next_emit];
        self.next_emit += 1;
        self.primed = true;
        Some((pos, column))
    }

    /// wasm32: no blocking pool, so this is the serial path. See the struct doc.
    #[cfg(target_arch = "wasm32")]
    pub async fn next(&mut self) -> Option<((i32, i32), ChunkColumn)> {
        if self.next_emit >= self.coords.len() {
            return None;
        }
        let (cx, cz) = self.coords[self.next_spawn];
        self.next_spawn += 1;
        let column = self.source.column(cx, cz);
        let pos = self.coords[self.next_emit];
        self.next_emit += 1;
        self.primed = true;
        let _ = &self.inflight;
        Some((pos, column))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A source whose column cost is a function of its position in a known list,
    /// so completion order is a *chosen* permutation rather than whatever the pool
    /// happened to do. `delays[i]` is applied to `coords[i]`.
    struct SkewedSource {
        coords: Vec<(i32, i32)>,
        delays: Vec<Duration>,
        completed: Arc<AtomicUsize>,
    }

    impl ChunkSource for SkewedSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let idx = self
                .coords
                .iter()
                .position(|&c| c == (cx, cz))
                .expect("the gate only asks for coordinates it declared");
            std::thread::sleep(self.delays[idx]);
            self.completed.fetch_add(1, Ordering::SeqCst);
            // A 16-block-tall column: this gate measures ordering and in-flight
            // counts, and a full -64..320 column would allocate 196 KiB per call
            // for no assertion.
            ChunkColumn::new(0, 16)
        }

        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:air".to_string()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    /// Twelve columns whose costs *decrease* with index, so the pool finishes them
    /// in exactly reverse order. That makes "completion order" a concrete,
    /// deterministic sequence the control below can produce.
    fn inverted_cost_view(n: usize) -> (Vec<(i32, i32)>, Vec<Duration>) {
        let coords: Vec<(i32, i32)> = (0..n as i32).map(|i| (i, 0)).collect();
        let delays = (0..n)
            .map(|i| Duration::from_millis(((n - i) * 4) as u64))
            .collect();
        (coords, delays)
    }

    #[test]
    fn the_window_is_derived_from_cores_and_never_below_two() {
        assert_eq!(generation_window_for(0), 2, "a bogus 0 must still window");
        assert_eq!(generation_window_for(1), 2);
        assert_eq!(generation_window_for(8), 16);
        assert_eq!(generation_window_for(64), 128);
        assert!(
            generation_window() >= 2,
            "the host-derived window must window on any machine"
        );
    }

    /// The window must not scale with the view, which is the whole defect
    /// `4307b59` reverted. 289 columns must not mean 289 in flight.
    #[test]
    fn the_window_does_not_scale_with_the_view() {
        let window = generation_window();
        assert!(
            window < 289,
            "a window of {window} would reproduce 5104adf's 289 concurrent generator calls \
             on this machine — the in-flight count must derive from cores, not the view"
        );
    }

    /// Emission order is the input order even when every column finishes in
    /// exactly the opposite order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_pipeline_emits_input_order_under_inverted_costs() {
        let (coords, delays) = inverted_cost_view(12);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        let mut pipeline = ColumnPipeline::with_window(source, coords.clone(), 8);
        let mut emitted = Vec::new();
        while let Some((pos, _column)) = pipeline.next().await {
            emitted.push(pos);
        }
        assert_eq!(
            emitted, coords,
            "the pipeline must emit in coordinate order regardless of completion order"
        );
        assert_eq!(completed.load(Ordering::SeqCst), coords.len());
    }

    /// **The control for the assertion above, and it must fail it.**
    ///
    /// A scheduler that emitted whichever column finished first would, on this
    /// cost profile, emit exactly the reverse of the input — the delays are
    /// monotonically decreasing, so completion order *is* reverse index order.
    /// Producing that sequence and requiring the equality above to reject it is
    /// what stops `the_pipeline_emits_input_order_under_inverted_costs` from being
    /// satisfied by a source that happens to finish in order anyway.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn control_completion_order_is_not_input_order() {
        let (coords, delays) = inverted_cost_view(12);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        // Spawn the whole view, then drain in completion order. For this cost
        // profile that is reverse index order, so it can be produced without a
        // `select!` over 12 futures.
        let handles: Vec<_> = coords
            .iter()
            .map(|&(cx, cz)| {
                let source = Arc::clone(&source);
                tokio::task::spawn_blocking(move || source.column(cx, cz))
            })
            .collect();
        let mut emitted = Vec::new();
        for (idx, handle) in handles.into_iter().enumerate().rev() {
            handle.await.expect("no worker may panic");
            emitted.push(coords[idx]);
        }

        assert_ne!(
            emitted, coords,
            "if completion order equals input order on this cost profile, the ordering \
             assertion beside this control is vacuous — the source is not actually skewed"
        );
        assert_eq!(
            emitted.first().copied(),
            coords.last().copied(),
            "the cheapest column is last in the view, so completion order starts there"
        );
    }

    /// Issue #453, as a counter: exactly **one** column has been generated at the
    /// moment the first one is emitted. This is what "primed" buys, and it is the
    /// property a plain sliding window would lose.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn exactly_one_column_is_generated_before_the_first_emit() {
        let (coords, delays) = inverted_cost_view(16);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        let mut pipeline = ColumnPipeline::with_window(source, coords.clone(), 8);
        let (first, _column) = pipeline.next().await.expect("a non-empty view emits");
        let at_first = completed.load(Ordering::SeqCst);
        assert_eq!(first, coords[0]);
        assert_eq!(
            at_first, 1,
            "{at_first} columns had been generated when the first was emitted; #453 requires \
             the player's own column to reach the wire after one column of generation"
        );

        // …and the window really does open afterwards, or the line above would be
        // satisfied by a fully serial pipeline.
        let _ = pipeline.next().await;
        let _ = pipeline.next().await;
        assert!(
            completed.load(Ordering::SeqCst) > 3,
            "after priming, more columns must be in flight than have been emitted — \
             otherwise this is the serial shape and the barrier was not removed"
        );
    }

    /// A one-column view still works, and a zero-column view emits nothing rather
    /// than panicking on the `pop_front` above.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn degenerate_views_are_handled() {
        let completed = Arc::new(AtomicUsize::new(0));
        let empty: Vec<(i32, i32)> = Vec::new();
        let source = Arc::new(SkewedSource {
            coords: empty.clone(),
            delays: Vec::new(),
            completed: Arc::clone(&completed),
        });
        let mut pipeline = ColumnPipeline::with_window(source, empty, 8);
        assert!(pipeline.next().await.is_none());
        assert_eq!(pipeline.remaining(), 0);

        let one = vec![(3, 4)];
        let source = Arc::new(SkewedSource {
            coords: one.clone(),
            delays: vec![Duration::from_millis(0)],
            completed: Arc::clone(&completed),
        });
        let mut pipeline = ColumnPipeline::with_window(source, one, 8);
        assert_eq!(
            pipeline.next().await.map(|(pos, _)| pos),
            Some((3, 4)),
            "a single-column view emits it"
        );
        assert!(pipeline.next().await.is_none());
    }
}
