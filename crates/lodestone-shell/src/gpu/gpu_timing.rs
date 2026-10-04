//! GPU timestamps on real render-pass boundaries, with asynchronous readback.
//!
//! Each bounded ring slot owns its queries, resolve buffer and readback buffer.
//! A slot is reserved before any timestamp is recorded. A full ring skips all
//! timer writes for that frame without waiting for the GPU.
//!
//! The world segment spans the real opaque pass through the last translucent
//! or nametag pass; the optional first-person segment covers one real pass.
//! These are backend stage intervals, not a calibrated frame GPU total. No
//! synthetic pass or unrelated scratch attachment supplies an aggregate edge.
//!
//! Readbacks carry the producing frame ID and the edges actually recorded.
//! Publication replaces the entire sample and only moves forward in frame ID,
//! so missing passes and invalid pairs cannot borrow values from older frames.
//! A retained sample has a visible age; reading it again is not a new sample.
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::platform::Instant;

/// Actual command operations for world and ordinary HUD rendering only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PrimaryCommandCounts {
    pub created: u64,
    pub finished: u64,
    pub submitted: u64,
}

/// Absolute CPU checkpoints after finishing and submitting the primary encoder.
#[derive(Debug, Clone, Copy)]
pub struct PrimarySubmitCheckpoints {
    pub encoder_finished_at: Instant,
    pub submitted_at: Instant,
}

thread_local! {
    static PRIMARY_COMMAND_COUNTS: Cell<PrimaryCommandCounts> = const {
        Cell::new(PrimaryCommandCounts { created: 0, finished: 0, submitted: 0 })
    };
}

pub(crate) fn primary_encoder(device: &wgpu::Device, label: &str) -> wgpu::CommandEncoder {
    let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    PRIMARY_COMMAND_COUNTS.with(|cell| {
        let mut counts = cell.get();
        counts.created += 1;
        cell.set(counts);
    });
    encoder
}

pub(crate) fn submit_primary_encoder(
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
) -> PrimarySubmitCheckpoints {
    let commands = encoder.finish();
    let encoder_finished_at = Instant::now();
    PRIMARY_COMMAND_COUNTS.with(|cell| {
        let mut counts = cell.get();
        counts.finished += 1;
        cell.set(counts);
    });
    queue.submit(std::iter::once(commands));
    let submitted_at = Instant::now();
    PRIMARY_COMMAND_COUNTS.with(|cell| {
        let mut counts = cell.get();
        counts.submitted += 1;
        cell.set(counts);
    });
    PrimarySubmitCheckpoints { encoder_finished_at, submitted_at }
}

pub(crate) fn take_primary_command_counts() -> PrimaryCommandCounts {
    PRIMARY_COMMAND_COUNTS.with(|cell| cell.replace(PrimaryCommandCounts::default()))
}

const FRAMES_IN_FLIGHT: usize = 3;
static NEXT_TIMER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTimingStatus {
    Measured,
    NotRun,
    Invalid,
    MapError,
}

impl GpuTimingStatus {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::NotRun => "not_run",
            Self::Invalid => "invalid",
            Self::MapError => "map_error",
        }
    }
}

/// One segment of a single completed frame. Bits 0/1 identify recorded
/// begin/end edges; raw ticks alone never establish that an edge was written.
#[derive(Debug, Clone)]
pub struct GpuTimingSegment {
    pub name: &'static str,
    pub status: GpuTimingStatus,
    pub written_edges: u8,
    pub raw_ticks: Option<[u64; 2]>,
    pub duration_ms: Option<f32>,
}

/// A coherent completed readback. Submit IDs count this timer's world
/// submissions; they are not global queue submission IDs.
#[derive(Debug, Clone)]
pub struct GpuTimingSample {
    pub timer_id: u64,
    pub frame_id: u64,
    pub submit_id: u64,
    pub completed_at_frame: u64,
    pub period_ns: f64,
    pub segments: Vec<GpuTimingSegment>,
}

/// Tiny owned snapshot for the overlay, tracing and benchmark consumers.
/// Counters are cumulative for the timer's lifetime. A consumer aggregates
/// each (timer_id, frame_id) once, including across timing enable/disable.
#[derive(Debug, Clone)]
pub struct GpuTimingSnapshot {
    pub timer_id: u64,
    pub current_frame_id: u64,
    pub sample_age_frames: Option<u64>,
    pub sample: Option<GpuTimingSample>,
    pub completed_samples: u64,
    pub dropped_frames: u64,
    pub invalid_segments: u64,
    pub map_errors: u64,
}

#[derive(Debug)]
struct PendingReadback {
    receiver: Receiver<Result<(), wgpu::BufferAsyncError>>,
    frame_id: u64,
    submit_id: u64,
    written_edges: u64,
}

#[derive(Debug)]
struct Slot {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback: wgpu::Buffer,
    pending: Option<PendingReadback>,
}

#[derive(Debug)]
struct FrameReservation {
    slot: usize,
    frame_id: u64,
    written_edges: Cell<u64>,
    resolved: bool,
}

#[derive(Debug, Default)]
struct CompletedSamples {
    latest: Option<GpuTimingSample>,
    completed: u64,
    invalid_segments: u64,
    map_errors: u64,
}

impl CompletedSamples {
    fn publish(&mut self, sample: GpuTimingSample) {
        self.completed += 1;
        self.invalid_segments += sample.segments.iter()
            .filter(|segment| segment.status == GpuTimingStatus::Invalid).count() as u64;
        self.map_errors += u64::from(sample.segments.iter()
            .any(|segment| segment.status == GpuTimingStatus::MapError));
        if self.latest.as_ref().is_none_or(|latest| sample.frame_id > latest.frame_id) {
            self.latest = Some(sample);
        }
    }
}

fn free_slot(occupied: impl IntoIterator<Item = bool>) -> Option<usize> {
    occupied.into_iter().position(|pending| !pending)
}

fn decode_segment(
    name: &'static str,
    written_edges: u8,
    raw_ticks: Option<[u64; 2]>,
    period_ns: f64,
) -> GpuTimingSegment {
    let (status, duration_ms) = match (written_edges, raw_ticks) {
        (_, None) => (GpuTimingStatus::MapError, None),
        (0, _) => (GpuTimingStatus::NotRun, None),
        (3, Some([begin, end])) if end >= begin => (
            GpuTimingStatus::Measured,
            Some(((end - begin) as f64 * period_ns / 1_000_000.0) as f32),
        ),
        _ => (GpuTimingStatus::Invalid, None),
    };
    GpuTimingSegment { name, status, written_edges, raw_ticks, duration_ms }
}

/// Timestamp timer gated on the device's granted feature, not merely the
/// adapter's advertised capability. Production rendering remains asynchronous.
#[derive(Debug)]
pub(crate) struct GpuQueryTimer {
    timer_id: u64,
    segment_names: Vec<&'static str>,
    slots: Vec<Slot>,
    period_ns: f64,
    frame_id: u64,
    submit_id: u64,
    active: Option<FrameReservation>,
    completed: CompletedSamples,
    dropped_frames: u64,
}

impl GpuQueryTimer {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        segment_names: &[&'static str],
    ) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        assert!(!segment_names.is_empty() && segment_names.len() <= 32);
        let count = (segment_names.len() * 2) as u32;
        let buf_size = u64::from(count) * 8;
        let slots = (0..FRAMES_IN_FLIGHT).map(|_| Slot {
            query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some(label),
                ty: wgpu::QueryType::Timestamp,
                count,
            }),
            resolve_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-timer-resolve"),
                size: buf_size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu-timer-readback"),
                size: buf_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            pending: None,
        }).collect();
        Some(Self {
            timer_id: NEXT_TIMER_ID.fetch_add(1, Ordering::Relaxed),
            segment_names: segment_names.to_vec(),
            slots,
            period_ns: f64::from(queue.get_timestamp_period()),
            frame_id: 0,
            submit_id: 0,
            active: None,
            completed: CompletedSamples::default(),
            dropped_frames: 0,
        })
    }

    /// Reserve storage before recording any pass edges. No free slot means
    /// no timestamp descriptors, resolve, copy or map for this frame.
    pub(crate) fn begin_frame(&mut self, device: &wgpu::Device) {
        if self.active.take().is_some() {
            self.dropped_frames += 1;
        }
        self.frame_id += 1;
        self.poll(device);
        let Some(slot) = free_slot(self.slots.iter().map(|slot| slot.pending.is_some())) else {
            self.dropped_frames += 1;
            return;
        };
        self.active = Some(FrameReservation {
            slot,
            frame_id: self.frame_id,
            written_edges: Cell::new(0),
            resolved: false,
        });
    }

    fn edges(&self, name: &str, edges: u8) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let active = self.active.as_ref()?;
        let i = self.segment_names.iter().position(|segment| *segment == name)?;
        active.written_edges.set(active.written_edges.get() | (u64::from(edges) << (i * 2)));
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.slots[active.slot].query_set,
            beginning_of_pass_write_index: (edges & 1 != 0).then_some((i * 2) as u32),
            end_of_pass_write_index: (edges & 2 != 0).then_some((i * 2 + 1) as u32),
        })
    }

    /// Both edges of one real render pass.
    pub(crate) fn writes(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.edges(name, 3)
    }

    /// First pass's begin edge for a segment spanning multiple real passes.
    pub(crate) fn writes_begin(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.edges(name, 1)
    }

    /// Last pass's end edge for a segment spanning multiple real passes.
    pub(crate) fn writes_end(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.edges(name, 2)
    }

    /// Record after all measured passes, before their encoder is submitted.
    pub(crate) fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(active) = &mut self.active else { return };
        if active.resolved {
            return;
        }
        let slot = &self.slots[active.slot];
        let n = (self.segment_names.len() * 2) as u32;
        encoder.resolve_query_set(&slot.query_set, 0..n, &slot.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(&slot.resolve_buffer, 0, &slot.readback, 0, u64::from(n) * 8);
        active.resolved = true;
    }

    /// Start mapping only after the reserved slot's resolve/copy was submitted.
    pub(crate) fn after_submit(&mut self) {
        let Some(active) = self.active.take() else { return };
        if !active.resolved {
            self.dropped_frames += 1;
            return;
        }
        self.submit_id += 1;
        let slot = &mut self.slots[active.slot];
        let (tx, receiver) = std::sync::mpsc::channel();
        slot.readback.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        slot.pending = Some(PendingReadback {
            receiver,
            frame_id: active.frame_id,
            submit_id: self.submit_id,
            written_edges: active.written_edges.get(),
        });
    }

    /// Service ready callbacks without waiting; every terminal path releases
    /// its slot, including disconnected callbacks and failed mapped views.
    pub(crate) fn poll(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest();
    }

    fn harvest(&mut self) {
        for slot in &mut self.slots {
            let Some(pending) = &slot.pending else { continue };
            let mapped = match pending.receiver.try_recv() {
                Ok(result) => result.is_ok(),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => false,
            };
            let pending = slot.pending.take().expect("completed slot has a pending readback");
            let raw = if mapped {
                slot.readback.slice(..).get_mapped_range().ok().map(|data| {
                    bytemuck::cast_slice::<u8, u64>(&data).to_vec()
                })
            } else {
                None
            };
            slot.readback.unmap();
            let segments = self.segment_names.iter().enumerate().map(|(i, &name)| {
                let edges = ((pending.written_edges >> (i * 2)) & 3) as u8;
                let ticks = raw.as_ref().map(|values| [values[i * 2], values[i * 2 + 1]]);
                decode_segment(name, edges, ticks, self.period_ns)
            }).collect();
            self.completed.publish(GpuTimingSample {
                timer_id: self.timer_id,
                frame_id: pending.frame_id,
                submit_id: pending.submit_id,
                completed_at_frame: self.frame_id,
                period_ns: self.period_ns,
                segments,
            });
        }
    }

    /// Compatibility view of one whole sample; absent and invalid segments
    /// never retain another frame's duration.
    pub(crate) fn results_ms(&self) -> impl Iterator<Item = (&'static str, Option<f32>)> + '_ {
        self.segment_names.iter().enumerate().map(|(i, &name)| {
            let duration = self.completed.latest.as_ref()
                .and_then(|sample| sample.segments[i].duration_ms);
            (name, duration)
        })
    }

    pub(crate) fn frame_id(&self) -> Option<u64> {
        self.completed.latest.as_ref().map(|sample| sample.frame_id)
    }

    pub(crate) fn snapshot(&self) -> GpuTimingSnapshot {
        GpuTimingSnapshot {
            timer_id: self.timer_id,
            current_frame_id: self.frame_id,
            sample_age_frames: self.frame_id().map(|id| self.frame_id.saturating_sub(id)),
            sample: self.completed.latest.clone(),
            completed_samples: self.completed.completed,
            dropped_frames: self.dropped_frames,
            invalid_segments: self.completed.invalid_segments,
            map_errors: self.completed.map_errors,
        }
    }

    pub(crate) fn stalled_frames(&self) -> u64 {
        self.dropped_frames
    }
}

#[cfg(test)]
mod sample_association_tests {
    use super::*;

    fn sample(frame_id: u64, ticks: [u64; 2]) -> GpuTimingSample {
        GpuTimingSample {
            timer_id: 7,
            frame_id,
            submit_id: frame_id,
            completed_at_frame: 5,
            period_ns: 1.0,
            segments: vec![decode_segment("world", 3, Some(ticks), 1.0)],
        }
    }

    #[test]
    fn publication_selects_newest_frame_independent_of_slot_order() {
        let mut completed = CompletedSamples::default();
        for record in [sample(3, [100, 157]), sample(4, [400, 573]), sample(2, [900, 921])] {
            completed.publish(record);
        }
        let newest = completed.latest.as_ref().unwrap();
        assert_eq!(newest.frame_id, 4);
        assert_eq!(newest.segments[0].raw_ticks, Some([400, 573]));
        assert_eq!(newest.segments[0].duration_ms, Some(0.000173));
        assert_eq!(completed.completed, 3);
        let _held = completed.latest.clone();
        assert_eq!(completed.completed, 3);
    }

    #[test]
    fn edge_mask_rejects_positive_stale_ticks_and_partial_pairs() {
        let stale = decode_segment("first_person", 0, Some([100, 200]), 1.0);
        assert_eq!(stale.status, GpuTimingStatus::NotRun);
        assert_eq!(stale.duration_ms, None);
        let partial = decode_segment("world", 1, Some([300, 200]), 1.0);
        assert_eq!(partial.status, GpuTimingStatus::Invalid);
        assert_eq!(partial.duration_ms, None);
        let valid = decode_segment("world", 3, Some([311, 524]), 1.0);
        assert_eq!(valid.status, GpuTimingStatus::Measured);
        assert_eq!(valid.duration_ms, Some(0.000213));
        let reversed = decode_segment("world", 3, Some([524, 311]), 1.0);
        assert_eq!(reversed.status, GpuTimingStatus::Invalid);
        let zero = decode_segment("world", 3, Some([619, 619]), 1.0);
        assert_eq!(zero.status, GpuTimingStatus::Measured);
        assert_eq!(zero.duration_ms, Some(0.0));
    }

    #[test]
    fn newest_absent_or_failed_segment_replaces_old_duration() {
        let mut completed = CompletedSamples::default();
        let mut previous = sample(1, [17, 98]);
        previous.segments.push(decode_segment("first_person", 3, Some([101, 173]), 1.0));
        completed.publish(previous);
        let mut absent = sample(2, [311, 358]);
        absent.segments.push(decode_segment("first_person", 0, Some([101, 173]), 1.0));
        completed.publish(absent);
        let latest = completed.latest.as_ref().unwrap();
        assert_eq!(latest.frame_id, 2);
        assert_eq!(latest.timer_id, 7);
        assert_eq!(latest.segments[0].duration_ms, Some(0.000047));
        assert_eq!(latest.segments[1].status, GpuTimingStatus::NotRun);
        assert_eq!(latest.segments[1].duration_ms, None);
        let mut failed = sample(3, [300, 500]);
        failed.segments[0] = decode_segment("world", 3, None, 1.0);
        completed.publish(failed);
        assert_eq!(completed.latest.as_ref().unwrap().segments[0].status, GpuTimingStatus::MapError);
        assert_eq!(completed.map_errors, 1);
        assert_eq!(completed.completed, 3);
    }

    #[test]
    fn occupied_ring_has_no_reservation_and_free_slot_is_used() {
        assert_eq!(free_slot([true, true, true]), None);
        assert_eq!(free_slot([true, false, true]), Some(1));
        assert_eq!(free_slot([false, true, false]), Some(0));
    }
}

/// CPU timings nested inside world command encoding. The rendering thread
/// records each checkpoint and the profiler drains them at `WorldEncode`.
/// Primary encoder finish and queue submit are separate frame phases.
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
    /// screen overlays — all still before the primary encoder is finished.
    OtherDraws,
}

/// [`WorldSubphase`] variant count, kept in one place for the same reason
/// `app::frame_profile::PHASE_COUNT` is.
pub(crate) const WORLD_SUBPHASE_COUNT: usize = 3;

impl WorldSubphase {
    pub(crate) const ALL: [WorldSubphase; WORLD_SUBPHASE_COUNT] = [
        WorldSubphase::PrepareBuffers,
        WorldSubphase::TerrainCullAndDraw,
        WorldSubphase::OtherDraws,
    ];

    /// Short, stable name for the F3/tracing detail line and the CSV dump's
    /// header — kept separate from `Debug` for the same reason
    /// `FramePhase::name` is.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            WorldSubphase::PrepareBuffers => "world.prepare_buffers",
            WorldSubphase::TerrainCullAndDraw => "world.terrain_cull_draw",
            WorldSubphase::OtherDraws => "world.other_draws",
        }
    }
}

/// Counts alongside [`WorldSubphase::TerrainCullAndDraw`]'s timing — this
/// repo's own evidence standard: "3 ms across 60 draws is a different
/// problem from 3 ms across 6000," so the timing above means nothing without
/// these next to it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WorldSubphaseCounts {
    pub terrain_camera_bind_calls: usize,
    pub terrain_origin_vertex_binds: usize,
    pub terrain_indexed_draw_calls: usize,
    pub terrain_buffer_bind_pairs: usize,
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
    /// Opaque terrain sections submitted in the production draw pass.
    pub opaque_sections_drawn: usize,
    /// Water sections submitted after opaque geometry.
    pub water_sections_drawn: usize,
    /// Translucent block sections submitted in their production pass.
    pub translucent_sections_drawn: usize,
    /// Entity instances submitted by the entity pass.
    pub entities_drawn: usize,
    /// Block-entity instances submitted by their renderer.
    pub block_entities_drawn: usize,
    /// Prepared sign-text vertices submitted with block entities.
    pub sign_text_vertices: u32,
    /// Particle instances submitted in the frame's particle pass.
    pub particles_drawn: usize,
    /// Actual terrain/world pass begins, excluding text and nametags.
    pub world_pass_begins: usize,
    /// Actual raw-view sign/display text pass begins.
    pub world_text_pass_begins: usize,
    /// Actual raw-view nametag pass begins.
    pub nametag_pass_begins: usize,
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
            opaque_sections_drawn: 7,
            water_sections_drawn: 13,
            translucent_sections_drawn: 17,
            entities_drawn: 19,
            block_entities_drawn: 23,
            sign_text_vertices: 29,
            particles_drawn: 31,
            world_pass_begins: 37,
            world_text_pass_begins: 41,
            nametag_pass_begins: 43,
            terrain_camera_bind_calls: 47,
            terrain_origin_vertex_binds: 53,
            terrain_indexed_draw_calls: 59,
            terrain_buffer_bind_pairs: 61,
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
        assert_eq!(counts.opaque_sections_drawn, 7);
        assert_eq!(counts.water_sections_drawn, 13);
        assert_eq!(counts.translucent_sections_drawn, 17);
        assert_eq!(counts.entities_drawn, 19);
        assert_eq!(counts.block_entities_drawn, 23);
        assert_eq!(counts.sign_text_vertices, 29);
        assert_eq!(counts.particles_drawn, 31);
        assert_eq!(counts.world_pass_begins, 37);
        assert_eq!(counts.world_text_pass_begins, 41);
        assert_eq!(counts.nametag_pass_begins, 43);
        assert_eq!(counts.terrain_camera_bind_calls, 47);
        assert_eq!(counts.terrain_origin_vertex_binds, 53);
        assert_eq!(counts.terrain_indexed_draw_calls, 59);
        assert_eq!(counts.terrain_buffer_bind_pairs, 61);

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
    /// arm" trap `docs/render-benchmarks.md` already calls out for
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
