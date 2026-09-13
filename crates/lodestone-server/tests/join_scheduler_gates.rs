//! Unit 10's structural gates: the per-ring **barrier** is gone, the in-flight
//! count is **bounded**, and the one-column time-to-first-chunk property
//! **survives** — each as a counter, each with a control that must fail it.
//!
//! # Why counters and not wall time
//!
//! `4307b59` is the scar this unit exists to remove, and the reasoning that
//! produced it was a measurement problem: a serial sweep cannot distinguish the
//! old FIFO memo cache from Unit 6's staged store (`docs/worldgen-staged-store.md`
//! — both read identical stage counts, because serially a cache never has a racing
//! miss), and a wall-clock comparison of two scheduler shapes on this machine has a
//! range that overlaps almost completely (Unit 6 measured 40.2–106.1 s against
//! 41.6–103.1 s over six alternated samples and declined the speedup claim).
//!
//! So nothing here is timed. The three properties are counted:
//!
//! | property | counter | control that must fail it |
//! |---|---|---|
//! | no barrier | max **distinct rings** in flight at once | the per-ring shape, which reports exactly 1 |
//! | bounded fan-out | max **columns** in flight at once | the per-ring shape, whose maximum is the largest ring (64) |
//! | latency | columns **completed before the first emit** | the previous flat shape, which reports 289 |
//!
//! The first two controls are the *same* arm, which is the point: `4307b59`
//! conflated two defects, and the per-ring barrier only ever addressed one of them
//! while creating the serialisation this unit deletes.
//!
//! The arms are **not** interleaved, deliberately: interleaving exists to stop a
//! machine-load drift being attributed to an arm, and every assertion here is
//! structural rather than temporal. The `hold` profile below is a stagger, not a
//! cost model — a `sleep` does not compete for a core, so **wall time in this
//! binary means nothing** and is not reported.
//!
//! The real-generator, real-stage-counter half of Unit 10's evidence is
//! `join_scheduler_counters.rs`, which is `#[ignore]`d and release-only.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_server::join_scheduler::{ColumnPipeline, generation_window_for};
use lodestone_server::{ChunkColumn, ChunkSource};

/// The burst named in `4307b59` — *"cache contention with 289 concurrent generator
/// calls"*. 17×17 at `view_radius = 8`.
const VIEW_RADIUS: i32 = 8;
const COLUMNS: usize = 289;

/// The window every arm here is driven with, fixed rather than
/// [`generation_window_for`]'s host-derived value so the assertions below do not
/// vary with the machine's core count. Its *derivation* is gated separately, in
/// `join_scheduler`'s own unit tests.
const GATE_WINDOW: usize = 8;

/// Chebyshev ring of a view-relative coordinate. The view is centred on `(0, 0)`,
/// so this is `join_view_rings`' own predicate read backwards — derived from the
/// same expression the enumeration uses rather than restated as a table.
fn ring_of((cx, cz): (i32, i32)) -> usize {
    cx.abs().max(cz.abs()) as usize
}

/// The wire order: rings outward from the centre, `dz`-outer/`dx`-inner within a
/// ring. Mirrors `crate::server::join_view_rings`, which is private.
fn wire_order(view_radius: i32) -> Vec<(i32, i32)> {
    (0..=view_radius)
        .flat_map(|r| {
            let mut ring = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        ring.push((dx, dz));
                    }
                }
            }
            ring
        })
        .collect()
}

#[derive(Default)]
struct ProbeState {
    /// Coordinate → its ring, for every column currently inside `column()`.
    inflight: HashMap<(i32, i32), usize>,
    max_inflight: usize,
    max_distinct_rings: usize,
}

/// A [`ChunkSource`] that reports how much of the view was in flight at once, and
/// from how many different rings.
///
/// The maxima are sampled on **entry** to `column()`, which is sufficient: any two
/// columns that overlap are both resident when the later one enters, so the later
/// entry observes the pair. Sampling on exit as well would only add noise.
struct RingProbe {
    /// Position in the wire order, per coordinate — the input to `hold`.
    index_of: HashMap<(i32, i32), usize>,
    state: Mutex<ProbeState>,
    completed: AtomicUsize,
    /// Stagger period. See [`hold`](Self::hold).
    stagger: Duration,
}

impl RingProbe {
    fn new(coords: &[(i32, i32)], stagger: Duration) -> Arc<Self> {
        Arc::new(Self {
            index_of: coords.iter().copied().zip(0..).collect(),
            state: Mutex::new(ProbeState::default()),
            completed: AtomicUsize::new(0),
            stagger,
        })
    }

    /// `2 ms + (index mod GATE_WINDOW) × stagger`.
    ///
    /// **A stagger, not a cost model.** Its only job is to make the columns inside
    /// one window finish at *different* times: with a uniform hold every member of
    /// a freshly-filled window completes at the same instant, so whether the
    /// window's oldest member is still resident when the next one is spawned
    /// becomes a race against thread scheduling. Making the hold a function of
    /// `index mod window` guarantees that when the window's head retires, six of
    /// its seven siblings are still inside `column()` — so the cross-ring overlap
    /// the gate asserts is deterministic rather than likely.
    fn hold(&self, index: usize) -> Duration {
        Duration::from_millis(2) + self.stagger * ((index % GATE_WINDOW) as u32)
    }

    fn snapshot(&self) -> (usize, usize, usize) {
        let state = self.state.lock().expect("probe state poisoned");
        (
            state.max_inflight,
            state.max_distinct_rings,
            self.completed.load(Ordering::SeqCst),
        )
    }
}

impl ChunkSource for RingProbe {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let pos = (cx, cz);
        let index = *self
            .index_of
            .get(&pos)
            .expect("the gate only asks for coordinates in the view it declared");
        {
            let mut state = self.state.lock().expect("probe state poisoned");
            state.inflight.insert(pos, ring_of(pos));
            state.max_inflight = state.max_inflight.max(state.inflight.len());
            let rings: HashSet<usize> = state.inflight.values().copied().collect();
            state.max_distinct_rings = state.max_distinct_rings.max(rings.len());
        }
        std::thread::sleep(self.hold(index));
        {
            let mut state = self.state.lock().expect("probe state poisoned");
            state.inflight.remove(&pos);
        }
        // 16 blocks tall: this binary asserts scheduling, and a full -64..320
        // column would allocate 196 KiB per call for no assertion.
        self.completed.fetch_add(1, Ordering::SeqCst);
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// What one arm reports.
#[derive(Debug)]
struct ArmResult {
    emitted: Vec<(i32, i32)>,
    max_inflight: usize,
    max_distinct_rings: usize,
    /// The `completed` counter read at the instant the first column was emitted —
    /// the time-to-first-chunk property, and the only one of the three that is
    /// about latency.
    completed_before_first_emit: usize,
}

/// **The unit under test**: `crate::join_scheduler`'s primed sliding window, driven
/// exactly as `serve_connection`'s `SourceRef::Shared` arm drives it.
async fn window_arm(coords: &[(i32, i32)], stagger: Duration) -> ArmResult {
    let probe = RingProbe::new(coords, stagger);
    let mut pipeline = ColumnPipeline::with_window(Arc::clone(&probe), coords.to_vec(), GATE_WINDOW);
    let mut emitted = Vec::with_capacity(coords.len());
    let mut completed_before_first_emit = usize::MAX;
    while let Some((pos, _column)) = pipeline
        .next()
        .await
        .expect("a source without an encoder cannot fail")
    {
        if emitted.is_empty() {
            completed_before_first_emit = probe.completed.load(Ordering::SeqCst);
        }
        emitted.push(pos);
    }
    let (max_inflight, max_distinct_rings, _) = probe.snapshot();
    ArmResult {
        emitted,
        max_inflight,
        max_distinct_rings,
        completed_before_first_emit,
    }
}

/// **The control for the first two counters**: the per-ring barrier, exactly as
/// `server.rs` spelled it before this unit — spawn every column of ring `r`, await
/// all of them, only then look at ring `r + 1`.
///
/// It is also, structurally, what the `SourceRef::Borrowed` arm still does (that
/// arm blocks per ring rather than spawning per column, but the barrier is in the
/// same place), so its `completed_before_first_emit == 1` assertion below is the
/// time-to-first-chunk evidence for both arms.
async fn barrier_arm(coords: &[(i32, i32)], stagger: Duration, view_radius: i32) -> ArmResult {
    let probe = RingProbe::new(coords, stagger);
    let mut emitted = Vec::with_capacity(coords.len());
    let mut completed_before_first_emit = usize::MAX;
    for r in 0..=view_radius {
        let ring: Vec<(i32, i32)> = coords
            .iter()
            .copied()
            .filter(|&c| ring_of(c) == r as usize)
            .collect();
        let handles: Vec<_> = ring
            .iter()
            .map(|&(cx, cz)| {
                let probe = Arc::clone(&probe);
                tokio::task::spawn_blocking(move || probe.column(cx, cz))
            })
            .collect();
        for (pos, handle) in ring.into_iter().zip(handles) {
            handle.await.expect("no ring worker may panic");
            if emitted.is_empty() {
                completed_before_first_emit = probe.completed.load(Ordering::SeqCst);
            }
            emitted.push(pos);
        }
    }
    let (max_inflight, max_distinct_rings, _) = probe.snapshot();
    ArmResult {
        emitted,
        max_inflight,
        max_distinct_rings,
        completed_before_first_emit,
    }
}

/// **The control for the third counter**: the previous shape — generate the
/// whole view, *then* encode any of it. It is what `join_view_rings` was
/// introduced to replace, and the only arm here whose
/// `completed_before_first_emit` is not 1.
async fn flat_arm(coords: &[(i32, i32)], stagger: Duration) -> ArmResult {
    let probe = RingProbe::new(coords, stagger);
    let handles: Vec<_> = coords
        .iter()
        .map(|&(cx, cz)| {
            let probe = Arc::clone(&probe);
            tokio::task::spawn_blocking(move || probe.column(cx, cz))
        })
        .collect();
    for handle in handles {
        handle.await.expect("no worker may panic");
    }
    let completed_before_first_emit = probe.completed.load(Ordering::SeqCst);
    let (max_inflight, max_distinct_rings, _) = probe.snapshot();
    ArmResult {
        emitted: coords.to_vec(),
        max_inflight,
        max_distinct_rings,
        completed_before_first_emit,
    }
}

/// **Unit 10's acceptance criterion, and both of its controls.**
///
/// One test rather than three because the controls and the subject must be read
/// against *each other*: "max distinct rings in flight ≥ 2" is only evidence of
/// anything once the same detector has been shown to report exactly 1 on the shape
/// that had the barrier. Splitting them would let a detector that always reports
/// its input's ring count pass the subject half.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_per_ring_barrier_is_gone_the_fan_out_is_bounded_and_453_survives() {
    let coords = wire_order(VIEW_RADIUS);
    assert_eq!(coords.len(), COLUMNS, "the view named in 4307b59 is 17x17");
    let stagger = Duration::from_millis(8);

    let window = window_arm(&coords, stagger).await;
    let barrier = barrier_arm(&coords, stagger, VIEW_RADIUS).await;
    let flat = flat_arm(&coords, stagger).await;

    eprintln!(
        "[U10] window: rings_in_flight={} max_inflight={} before_first_emit={}\n\
         [U10] barrier(control): rings_in_flight={} max_inflight={} before_first_emit={}\n\
         [U10] flat(control): rings_in_flight={} max_inflight={} before_first_emit={}",
        window.max_distinct_rings,
        window.max_inflight,
        window.completed_before_first_emit,
        barrier.max_distinct_rings,
        barrier.max_inflight,
        barrier.completed_before_first_emit,
        flat.max_distinct_rings,
        flat.max_inflight,
        flat.completed_before_first_emit,
    );

    // --- 1. The barrier is gone. ---
    assert_eq!(
        barrier.max_distinct_rings, 1,
        "the control reported {} rings in flight at once; a per-ring barrier can only ever \
         hold one ring's columns, so anything but 1 means this detector is measuring \
         something other than the barrier and the subject assertion below proves nothing",
        barrier.max_distinct_rings
    );
    assert!(
        window.max_distinct_rings >= 2,
        "only {} ring in flight at a time under the window scheduler — the barrier is still \
         there, or the window collapsed to 1",
        window.max_distinct_rings
    );

    // --- 2. The fan-out is bounded, and bounded by the *window* rather than by
    // the view. This is the half of `5104adf` that `4307b59` was right about.
    let largest_ring = 8 * VIEW_RADIUS as usize;
    assert_eq!(
        barrier.max_inflight, largest_ring,
        "the per-ring shape's in-flight count is the largest ring ({largest_ring} columns at \
         view_radius {VIEW_RADIUS}) — it scaled with the view. Reading anything else here \
         means the control did not reproduce the old shape"
    );
    assert!(
        window.max_inflight <= GATE_WINDOW,
        "{} columns in flight against a window of {GATE_WINDOW}; the window is not bounding \
         the fan-out, which is how 5104adf reached 289 concurrent generator calls",
        window.max_inflight
    );
    assert!(
        window.max_inflight >= 2,
        "only {} column in flight ever — the window never opened, so assertion 1 passed \
         for the wrong reason",
        window.max_inflight
    );

    // --- 3. Time-to-first-chunk survives: one column of generation before the
    // first chunk is encoded, unchanged from the barrier shape.
    assert_eq!(
        window.completed_before_first_emit, 1,
        "{} columns had been generated when the first chunk was emitted; #453 requires the \
         player's own column to reach the wire after exactly one",
        window.completed_before_first_emit
    );
    assert_eq!(
        barrier.completed_before_first_emit, 1,
        "the barrier shape also emitted after one column, so this is a preserved property \
         and not an improvement claim"
    );
    assert_eq!(
        flat.completed_before_first_emit, COLUMNS,
        "the pre-#453 control must report the whole view generated before the first encode; \
         if it reports 1 as well, the counter cannot see the regression it exists to catch"
    );

    // --- The wire order is untouched on every arm. ---
    assert_eq!(
        window.emitted, coords,
        "the window scheduler must emit in wire order, not completion order"
    );
    assert_eq!(barrier.emitted, coords);
    assert_eq!(flat.emitted, coords);
}

/// Why a window-sized batch could not have helped the `SourceRef::Borrowed` arm,
/// recorded as arithmetic rather than as a claim.
///
/// The first version of this landing gave that arm `[1, window, window, …]`
/// batches so both arms would share one scheduling story. It cannot work, for a
/// reason that is a property of the ring sizes: the cumulative size through ring
/// `r` is `1 + 8(1 + 2 + … + r)` = `1 + 4r(r + 1)`, and `r(r + 1)` is always even,
/// so **every ring boundary sits at an offset ≡ 1 (mod 8)** — exactly where a
/// window-8 batch boundary sits. No batch straddles a ring, ring 8's single
/// 64-column blocking batch becomes eight serial ones, and the arm gets strictly
/// more barriers than the rings gave it. Since a blocking source has no encode to
/// overlap with anyway, that arm keeps the rings; see `server.rs`'s comment.
///
/// This is here so the next person to notice the two arms differ finds the
/// measurement instead of re-deriving it.
#[test]
fn ring_boundaries_are_congruent_to_one_mod_eight_so_a_window_batch_never_straddles_one() {
    let coords = wire_order(VIEW_RADIUS);
    let mut cumulative = 0usize;
    for r in 0..=VIEW_RADIUS {
        let ring = coords.iter().filter(|&&c| ring_of(c) == r as usize).count();
        assert_eq!(
            ring,
            if r == 0 { 1 } else { 8 * r as usize },
            "ring {r} is not the size join_view_rings' predicate implies"
        );
        assert_eq!(
            cumulative % GATE_WINDOW,
            if r == 0 { 0 } else { 1 },
            "ring {r} starts at offset {cumulative}, which is not ≡ 1 (mod {GATE_WINDOW}) — \
             the congruence this test records has broken and a window batch could now \
             straddle a ring boundary"
        );
        cumulative += ring;
    }
    assert_eq!(cumulative, COLUMNS);
}

/// The window is derived from cores, never from the view — the property that makes
/// `5104adf`'s 289 concurrent generator calls unreachable at any view radius.
#[test]
fn the_window_never_scales_with_the_view() {
    for parallelism in [1usize, 2, 4, 8, 10, 16, 64] {
        let window = generation_window_for(parallelism);
        assert!(window >= 2, "a window of {window} is not a window");
        // One in-flight column per hardware thread. It was `2 ×` this until
        // §12.132, where a sweep over the real 289-column burst measured the
        // doubled value at 1.49× against the floor's 2.60× — instructions retired
        // flat to 1.4% across every arm, so the loss was scheduling and the cause
        // was cache capacity rather than any lock.
        assert_eq!(window, parallelism.max(2));
    }
    // At every plausible core count this machine or CI could report, the window
    // stays well under the view — which is the whole difference from the reverted
    // commit. 289 cores would be needed before a 289-column join burst put 289
    // columns in flight, and at 289 cores that is not the same defect.
    for parallelism in [1usize, 2, 4, 8, 16, 32] {
        assert!(
            generation_window_for(parallelism) < COLUMNS,
            "at {parallelism} cores the window reaches the whole view"
        );
    }
}
