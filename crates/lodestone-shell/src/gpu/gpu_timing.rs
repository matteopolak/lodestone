//! Coarse per-pass GPU timing, via `wgpu`'s `TIMESTAMP_QUERY` feature.
//!
//! # What this measures and how
//!
//! [`GpuQueryTimer`] owns one `wgpu::QuerySet` of `Timestamp` queries, two
//! per named segment (begin, end), and writes them through
//! [`RenderPassDescriptor::timestamp_writes`], **not**
//! `CommandEncoder::write_timestamp`. That distinction is load-bearing: the
//! per-pass descriptor field needs only `Features::TIMESTAMP_QUERY`, which
//! Metal supports, while the encoder-level method needs
//! `Features::TIMESTAMP_QUERY_INSIDE_ENCODERS`, whose own doc excludes Apple
//! GPUs by name ("Metal (AMD & Intel, not Apple GPUs)") — the same is true of
//! `TIMESTAMP_QUERY_INSIDE_PASSES`, which would be needed to time *inside* a
//! single pass rather than around it. So on this machine's adapter, whole-pass
//! timing is the finest grain available, and this module does not pretend
//! otherwise: a pass that fuses several logical stages (see
//! `RenderState`'s "world" segment) is reported as one number because it
//! genuinely is one pass, not because a finer split was skipped.
//!
//! # Feature gating happens twice
//!
//! The **adapter** advertising `TIMESTAMP_QUERY` is not sufficient — the
//! **device** must have requested it via `DeviceDescriptor::required_features`
//! at creation (`lodestone_render::device::GpuContext::from_instance`), or
//! every `timestamp_writes` write here is invalid. [`GpuQueryTimer::new`]
//! checks `device.features()` (the granted set), not the adapter's advertised
//! set, for exactly this reason, and returns `None` when the device does not
//! carry the feature — the caller (`RenderState`, the only owner today; see
//! [`RenderState::gpu_timing_available`](super::RenderState::gpu_timing_available))
//! then reports GPU timing as unavailable rather than silently drawing zeros.
//!
//! # What is (and is not) instrumented
//!
//! `RenderState` owns one `GpuQueryTimer` with two segments, `"world"` (the
//! terrain/entities/block-entities/particles/weather/outline/debug/nametag
//! pass — one real `wgpu` render pass, see `gpu::frame`'s module doc for why
//! it is not several) and `"first_person"` (the hand/held-item pass). The sky
//! pass, the seven individual screen-overlay passes and the HUD's own,
//! separately-submitted encoder are **not** GPU-timed — a deliberate scope
//! cut, not an oversight, because `"world"` is overwhelmingly the dominant
//! cost in this renderer and adding a fourth-plus struct's worth of query
//! plumbing for passes that are typically near-zero cost was not worth the
//! risk. Their absence is documented rather than silent: see
//! `docs/frame-profiling.md`'s "what this cannot see" section before reading
//! a report that sums to less than the CPU-side `world_encode_submit`/
//! `hud_ui_encode_submit` phases in `app::frame_profile`.
//!
//! # Readback is async and lagged by design
//!
//! A GPU query result is not available the instant its pass ends: it is only
//! guaranteed valid after the command buffer containing the resolve has
//! actually executed, which is one or more frames after submission on every
//! backend here. This uses the standard ring-buffered pattern:
//!
//! 1. [`GpuQueryTimer::resolve`] — called once the frame's passes are all
//!    recorded but before `queue.submit` — resolves this frame's queries into
//!    a `QUERY_RESOLVE` buffer, then copies that into a `MAP_READ` buffer
//!    belonging to this frame's ring slot (`frame % FRAMES_IN_FLIGHT`).
//! 2. [`GpuQueryTimer::after_submit`] — called right after `queue.submit` —
//!    starts an async `map_async` on that slot's buffer and harvests any
//!    earlier slot whose mapping has already completed.
//!
//! [`GpuQueryTimer::results_ms`] therefore always reports the **last
//! completed** frame's timings, a few frames behind — exactly the trade this
//! repo's own evidence standard prefers (a counter over a duration, and never
//! a synchronous stall on a debug feature). If a ring slot's previous mapping
//! is still outstanding when it comes back around — real GPU/readback
//! backpressure, not a bug — that is counted in [`GpuQueryTimer::stalled_frames`]
//! rather than silently dropped or overwritten; see that method's doc.
use std::sync::mpsc::Receiver;

/// Frames of readback latency. 3 gives the driver two full frames to finish
/// the resolve-and-copy before this timer would ever need to wait on a slot;
/// see the module doc's ring-buffer description.
const FRAMES_IN_FLIGHT: usize = 3;

#[derive(Debug)]
struct Slot {
    readback: wgpu::Buffer,
    pending: Option<Receiver<Result<(), wgpu::BufferAsyncError>>>,
}

/// Coarse GPU pass timing for a fixed, named set of segments. See the module
/// doc for the mechanism.
#[derive(Debug)]
pub(crate) struct GpuQueryTimer {
    query_set: wgpu::QuerySet,
    segment_names: Vec<&'static str>,
    resolve_buffer: wgpu::Buffer,
    slots: Vec<Slot>,
    /// `wgpu::Queue::get_timestamp_period()`, nanoseconds per tick — a
    /// per-adapter constant, read once at construction.
    period_ns: f64,
    frame: u64,
    /// Milliseconds for the most recently **completed** readback of each
    /// segment. Stays at the last real value between readbacks — never reset
    /// to zero merely because a new frame started, which is what would make a
    /// slow adapter read as "instantly free" between updates.
    results_ms: Vec<f32>,
    /// Whether `results_ms[i]` has ever been written by a real readback.
    /// `false` means "no data yet", which the F3/tracing output must show as
    /// such rather than as `0.0 ms` — a query pool that has produced nothing
    /// yet must not read the same as a pass that costs nothing.
    have_result: Vec<bool>,
    /// Frames where the ring slot due for reuse still had an unread mapping
    /// outstanding — genuine backpressure (see [`Self::after_submit`]), never
    /// silently swallowed. Surfaced in the debug/tracing output so a real
    /// stall is visible rather than read as "GPU timing is just cheap".
    stalled_frames: u64,
}

impl GpuQueryTimer {
    /// Build a timer for `segment_names.len()` named segments, or `None` if
    /// the **device** (not merely the adapter) was not granted
    /// `Features::TIMESTAMP_QUERY` — see the module doc's "gated twice" note.
    /// Every caller must treat `None` as "report GPU timing unavailable",
    /// never as "report zero".
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        segment_names: &[&'static str],
    ) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let segment_count = segment_names.len();
        debug_assert!(segment_count > 0, "a GpuQueryTimer with no segments is pointless");
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some(label),
            ty: wgpu::QueryType::Timestamp,
            count: (segment_count * 2) as u32,
        });
        let buf_size = (segment_count * 2 * 8) as u64;
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu-timer-resolve"),
            size: buf_size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..FRAMES_IN_FLIGHT)
            .map(|_| Slot {
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu-timer-readback"),
                    size: buf_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                pending: None,
            })
            .collect();
        Some(Self {
            query_set,
            segment_names: segment_names.to_vec(),
            resolve_buffer,
            slots,
            period_ns: f64::from(queue.get_timestamp_period()),
            frame: 0,
            results_ms: vec![0.0; segment_count],
            have_result: vec![false; segment_count],
            stalled_frames: 0,
        })
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.segment_names.iter().position(|n| *n == name)
    }

    /// Begin+end timestamp writes for a segment whose begin and end both fall
    /// inside **one** render pass — the common case. Pass the result as that
    /// pass descriptor's `timestamp_writes`.
    pub(crate) fn writes(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let i = self.index_of(name)?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some((i * 2) as u32),
            end_of_pass_write_index: Some((i * 2 + 1) as u32),
        })
    }

    /// Resolve this frame's queries into the ring slot for `self.frame`.
    /// Call once, after every pass that might have written into this timer's
    /// query set has been recorded, and **before** `queue.submit`
    /// (`resolve_query_set`/`copy_buffer_to_buffer` are encoder commands).
    pub(crate) fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let n = (self.segment_names.len() * 2) as u32;
        encoder.resolve_query_set(&self.query_set, 0..n, &self.resolve_buffer, 0);
        let slot = &self.slots[(self.frame as usize) % FRAMES_IN_FLIGHT];
        encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &slot.readback, 0, u64::from(n) * 8);
    }

    /// Start this frame's async readback and harvest any slot whose mapping
    /// already completed. Call once, right after `queue.submit` for the
    /// encoder [`Self::resolve`] was called against.
    ///
    /// **Never silently drops a result.** If the ring slot due for reuse
    /// still has an unread mapping (the GPU/driver did not finish it within
    /// `FRAMES_IN_FLIGHT` frames — real backpressure, not a bug in this
    /// timer), this counts it in [`Self::stalled_frames`] and skips starting
    /// a new map on top of the old one rather than losing either.
    pub(crate) fn after_submit(&mut self, device: &wgpu::Device) {
        // Non-blocking: only makes progress on callbacks already satisfied by
        // work that has completed, never waits on the GPU. A profiling
        // feature must not itself become the frame-time cost it exists to
        // measure.
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest();

        let idx = (self.frame as usize) % FRAMES_IN_FLIGHT;
        if self.slots[idx].pending.is_some() {
            self.stalled_frames += 1;
            self.frame += 1;
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.slots[idx]
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // The channel outliving the callback is the ordinary case; a
                // send failing here just means this timer was dropped
                // mid-flight (world teardown), which is not an error to
                // report.
                let _ = tx.send(result);
            });
        self.slots[idx].pending = Some(rx);
        self.frame += 1;
    }

    fn harvest(&mut self) {
        let segment_count = self.segment_names.len();
        for slot in &mut self.slots {
            let Some(rx) = &slot.pending else { continue };
            let Ok(result) = rx.try_recv() else { continue };
            slot.pending = None;
            if result.is_err() {
                // A mapping failure here (e.g. device lost) is not this
                // timer's to recover from; the values simply stay at their
                // last-known state, which `have_result` already makes an
                // honest "no fresh data" rather than a fabricated zero.
                continue;
            }
            let Ok(data) = slot.readback.slice(..).get_mapped_range() else {
                // The buffer reported a successful map but refused a view —
                // treat exactly like a mapping failure above: stale values,
                // never a fabricated zero.
                continue;
            };
            let raw: &[u64] = bytemuck::cast_slice(&data);
            for seg in 0..segment_count {
                let (begin, end) = (raw[seg * 2], raw[seg * 2 + 1]);
                // `end <= begin` happens for a segment whose pass did not run
                // this frame at all (both indices still hold whatever the
                // previous occupant of this ring slot's frame left, or 0 on
                // the first few frames) — leave the prior value in place
                // rather than reporting a nonsensical or negative duration.
                if end > begin {
                    let ns = (end - begin) as f64 * self.period_ns;
                    self.results_ms[seg] = (ns / 1_000_000.0) as f32;
                    self.have_result[seg] = true;
                }
            }
            drop(data);
            slot.readback.unmap();
        }
    }

    /// `(name, Some(ms))` for a segment with at least one real readback,
    /// `(name, None)` for one that has never resolved yet (e.g. the first
    /// `FRAMES_IN_FLIGHT` frames of a session, or a pass that has simply
    /// never run). Callers must render the `None` case visibly — "—" or
    /// "not yet measured", never a bare `0.0`.
    pub(crate) fn results_ms(&self) -> impl Iterator<Item = (&'static str, Option<f32>)> + '_ {
        self.segment_names
            .iter()
            .copied()
            .zip(self.results_ms.iter().copied())
            .zip(self.have_result.iter().copied())
            .map(|((name, ms), have)| (name, have.then_some(ms)))
    }

    /// Frames where a ring slot's previous readback was still outstanding
    /// when due for reuse — see [`Self::after_submit`]. Non-zero here means
    /// GPU readback is falling behind, not that the timer is broken; report
    /// it next to the timings rather than folding it into them.
    pub(crate) fn stalled_frames(&self) -> u64 {
        self.stalled_frames
    }
}

/// CPU sub-phase timing captured *inside* [`super::RenderState::render`]'s
/// `render_inner` (`gpu/frame.rs`) for the world pass — the fine breakdown
/// the owner's frame-profiler run asked for after finding
/// `world_encode_submit` dominant. See `docs/frame-profiling.md`'s "World
/// sub-phases" section for what each name covers and why the boundaries sit
/// where they do.
///
/// # Why a thread-local bridge, not a new `RenderStats`/`RenderState` field
///
/// `RenderStats` (`gpu/stats.rs`) and `RenderState` (`gpu/state.rs`) were
/// both under concurrent edit by other work at the time this landed, so
/// `render_inner` cannot hand this data back to
/// `app::frame_profile::FrameProfiler` (owned by `WindowApp`, a different
/// struct with no reference into `RenderState`) through either of those
/// files. This module owns a thread-local instead: `gpu/frame.rs`'s only
/// obligation is a handful of one-line [`record_world_subphase`] calls at
/// checkpoints it already has natural seams for (see that file's own
/// comments at each call site). The shell is single-threaded for rendering —
/// `WindowApp::redraw` and `render_inner` always run on the same thread — so
/// a thread-local needs no synchronisation and cannot race with itself.
///
/// [`take_world_subphases`] is called from exactly one place,
/// `app::frame_profile::FrameProfiler::mark`, at the existing
/// `FramePhase::WorldEncodeSubmit` checkpoint `app/redraw.rs` already marks
/// every frame — nothing about that call site needed to change for this to
/// exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldSubphase {
    /// Every `prepare_*` call, the sky pass, and every camera/outline/
    /// debug-line/plugin-billboard/crack uniform write — everything that
    /// runs before `begin_render_pass`, because a render pass cannot itself
    /// create or write a buffer (see `gpu/frame.rs`'s own module doc,
    /// "Submission order is load-bearing").
    PrepareBuffers,
    /// Opaque terrain only: the packed-table loop's per-section frustum-only
    /// `visible()` check plus its draw, and the model-arena loop's
    /// `TerrainCull::classify` plus its draw — separated from the rest of
    /// the pass because this is the loop the strategy.rs/draw-call-count
    /// question is actually about.
    TerrainCullAndDraw,
    /// Everything else this pass records: entities, block entities,
    /// particles, weather, water, translucent geometry, the outline, debug
    /// lines, nametags, the first-person hand's own pass, and the seven
    /// screen overlays — all still before `queue.submit`. Not split further
    /// for the same reason `app::frame_profile` gives for folding HUD,
    /// effects and the container/menu into one bucket: none individually
    /// costs enough on its own to be worth a separate checkpoint, and each
    /// is a different subsystem's file this instrument does not own.
    OtherDraws,
    /// `CommandEncoder::finish` + `Queue::submit`, plus this module's own GPU
    /// query resolve/`after_submit` bookkeeping immediately around them.
    /// `queue.submit` only *enqueues* work — see `app::frame_profile`'s
    /// module doc — so this is deliberately expected to be the smallest of
    /// the four on a healthy frame; a large reading here points at
    /// driver/queue contention, not at command-recording cost.
    Submit,
}

/// [`WorldSubphase`] variant count, kept in one place for the same reason
/// `app::frame_profile::PHASE_COUNT` is.
pub(crate) const WORLD_SUBPHASE_COUNT: usize = 4;

impl WorldSubphase {
    pub(crate) const ALL: [WorldSubphase; WORLD_SUBPHASE_COUNT] = [
        WorldSubphase::PrepareBuffers,
        WorldSubphase::TerrainCullAndDraw,
        WorldSubphase::OtherDraws,
        WorldSubphase::Submit,
    ];

    /// Short, stable name for the F3/tracing detail line and the CSV dump's
    /// header — kept separate from `Debug` for the same reason
    /// `FramePhase::name` is.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            WorldSubphase::PrepareBuffers => "world.prepare_buffers",
            WorldSubphase::TerrainCullAndDraw => "world.terrain_cull_draw",
            WorldSubphase::OtherDraws => "world.other_draws",
            WorldSubphase::Submit => "world.submit",
        }
    }
}

/// Counts alongside [`WorldSubphase::TerrainCullAndDraw`]'s timing — this
/// repo's own evidence standard: "3 ms across 60 draws is a different
/// problem from 3 ms across 6000," so the timing above means nothing without
/// these next to it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WorldSubphaseCounts {
    /// Packed-table sections iterated (`self.sections.len()`) — every one of
    /// them is visited; that loop has no cull counters of its own, only an
    /// inline `TerrainCull::visible` check, so this is its entire "visited"
    /// figure. Compare against `RenderStats::sections_drawn` for that loop's
    /// visited-vs-drawn (the packed table is the demo world only, never a
    /// live session — see `gpu.rs`'s own module doc).
    pub packed_sections_visited: usize,
    /// Model-arena (live-vanilla) sections iterated (`model.sections.len()`).
    /// Compare against `RenderStats::sections_drawn` plus its three
    /// `sections_culled_*` fields (already on the F3 overlay) for
    /// visited-vs-drawn on the loop that actually matters live.
    pub model_sections_visited: usize,
}

thread_local! {
    static WORLD_SUBPHASES: std::cell::RefCell<[Option<f32>; WORLD_SUBPHASE_COUNT]> =
        const { std::cell::RefCell::new([None; WORLD_SUBPHASE_COUNT]) };
    static WORLD_SUBPHASE_COUNTS_CELL: std::cell::RefCell<Option<WorldSubphaseCounts>> =
        const { std::cell::RefCell::new(None) };
}

/// Record one sub-phase's elapsed milliseconds for the frame currently being
/// recorded. Called from `gpu/frame.rs`'s `render_inner` — see that file's
/// checkpoint comments. Overwrites any previous value for `phase` this frame
/// rather than accumulating: every sub-phase runs at most once per call to
/// `render_inner`.
pub(crate) fn record_world_subphase(phase: WorldSubphase, ms: f32) {
    let i = WorldSubphase::ALL.iter().position(|p| *p == phase).expect("phase is in ALL");
    WORLD_SUBPHASES.with(|cell| cell.borrow_mut()[i] = Some(ms));
}

/// Record the per-frame counts alongside the sub-phase timings above.
pub(crate) fn record_world_subphase_counts(counts: WorldSubphaseCounts) {
    WORLD_SUBPHASE_COUNTS_CELL.with(|cell| *cell.borrow_mut() = Some(counts));
}

/// Drain this frame's sub-phase timings and counts, resetting both to
/// "not recorded" for the next frame. `None` for a timing slot means
/// genuinely never recorded this frame — the caller
/// (`app::frame_profile::FrameProfiler::mark`) must show that as a skip,
/// never a fabricated `0.0`, exactly like every other phase this instrument
/// reports.
pub(crate) fn take_world_subphases()
-> ([Option<f32>; WORLD_SUBPHASE_COUNT], Option<WorldSubphaseCounts>) {
    let timings = WORLD_SUBPHASES.with(|cell| cell.replace([None; WORLD_SUBPHASE_COUNT]));
    let counts = WORLD_SUBPHASE_COUNTS_CELL.with(|cell| cell.borrow_mut().take());
    (timings, counts)
}

#[cfg(test)]
mod world_subphase_tests {
    use super::*;

    /// The magnitude species this repo's evidence standard asks for: sleep a
    /// *non-round* interval, record it through the exact same
    /// `record_world_subphase`/`take_world_subphases` pair `gpu/frame.rs` and
    /// `FrameProfiler::mark` use, and require the drained figure to land on
    /// it — not merely be positive. `23` ms rather than a round `20`/`50`,
    /// for the same reason `frame_profile`'s own control avoids one.
    ///
    /// This is the control the task asked to be watched failing: temporarily
    /// changing `record_world_subphase` to a no-op (commenting out the body)
    /// makes this assert `Some(_)` against `None` and fail immediately —
    /// verified by hand before this landed, then restored; a version of this
    /// test that only checked `> 0.0` would have passed against a stray
    /// leftover value from a previous test in the same process just as
    /// easily as against a real measurement, which is exactly the "merely
    /// positive" failure mode this repo has paid for.
    #[test]
    fn record_and_take_round_trip_the_real_elapsed_time_not_a_placeholder() {
        // Drain first: `take_world_subphases` is process-global state (a
        // `thread_local`, but this test binary runs its tests on one thread
        // by default), so a prior test in this module could have left a
        // stale value behind.
        let _ = take_world_subphases();

        let t0 = crate::platform::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(23));
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        record_world_subphase(WorldSubphase::TerrainCullAndDraw, ms);
        record_world_subphase_counts(WorldSubphaseCounts {
            packed_sections_visited: 11,
            model_sections_visited: 4,
        });

        let (timings, counts) = take_world_subphases();
        let recorded = timings[WorldSubphase::ALL
            .iter()
            .position(|p| *p == WorldSubphase::TerrainCullAndDraw)
            .unwrap()];
        assert!(
            matches!(recorded, Some(ms) if (18.0..=80.0).contains(&ms)),
            "expected ~23ms (wide tolerance for scheduler jitter), got {recorded:?}"
        );
        // Pairwise-distinct fixture values (11 != 4), so a transposition of
        // the two count fields cannot survive this assertion.
        let counts = counts.expect("counts recorded above must round-trip");
        assert_eq!(counts.packed_sections_visited, 11);
        assert_eq!(counts.model_sections_visited, 4);

        // Draining must reset state for the next frame — a phase not
        // recorded again must read back as `None`, never a stale `Some` from
        // this test.
        let (timings2, counts2) = take_world_subphases();
        assert!(timings2.iter().all(Option::is_none));
        assert!(counts2.is_none());
    }

    /// Every [`WorldSubphase`] variant round-trips through `name()` to a
    /// distinct, non-empty string, and `ALL`'s order matches each variant's
    /// own declared position — the same "compiler will not catch a missed
    /// arm" trap `docs/frame-profiling.md` already calls out for
    /// `FramePhase`.
    #[test]
    fn every_subphase_has_a_distinct_name() {
        let names: Vec<&str> = WorldSubphase::ALL.iter().map(|p| p.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "every WorldSubphase name must be distinct: {names:?}");
    }
}
