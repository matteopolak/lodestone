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
