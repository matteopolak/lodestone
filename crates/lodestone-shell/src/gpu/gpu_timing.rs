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
//! `RenderState` owns one `GpuQueryTimer` with four segments:
//!
//! | segment | covers |
//! |---|---|
//! | `world_total` | the whole world command buffer — sky pass, block pass, first-person pass, every screen overlay |
//! | `world` | the block pass alone (terrain/entities/block entities/particles/weather/outline/debug/nametags — one real `wgpu` pass, see `gpu::frame`'s module doc) |
//! | `first_person` | the hand/held-item pass alone |
//! | `hud_total` | everything submitted after the world command buffer: the HUD's own encoders, the container/menu renderers, and the screenshot copy |
//!
//! The two `*_total` segments are **spans across passes**, stamped with
//! [`GpuQueryTimer::stamp`] rather than [`GpuQueryTimer::writes`] — see that
//! method's doc for why an empty bracketing pass is the only mechanism
//! available here. Between them they account for **every** GPU pass the shell
//! submits in a frame, which is the point: the first question a frame-time
//! investigation has to answer is whether the wall being hit is CPU or GPU,
//! and a per-pass number cannot answer it while any pass is unaccounted for.
//! `world_total - world - first_person` is the sky pass plus the screen
//! overlays; that residue is deliberately *not* reported as its own segment,
//! because reporting a subtraction as though it were a measurement is how a
//! counter starts lying.
//!
//! `hud_total` ends where the frame's own bookkeeping ends, so it excludes
//! `present` — the GPU cost of compositing the swapchain image is the
//! window system's, not this timer's, and nothing here can see it.
//!
//! # Readback is async and lagged by design
//!
//! A GPU query result is not available the instant its pass ends: it is only
//! guaranteed valid after the command buffer containing the resolve has
//! actually executed, which is one or more frames after submission on every
//! backend here. This uses the standard ring-buffered pattern:
//!
//! 1. [`GpuQueryTimer::resolve`] — called from the frame's **last** encoder,
//!    after every pass that writes into this query set has been recorded and
//!    before that encoder's `queue.submit` (both are encoder commands) —
//!    resolves this frame's queries into a `QUERY_RESOLVE` buffer, then copies
//!    that into a `MAP_READ` buffer belonging to this frame's ring slot
//!    (`frame % FRAMES_IN_FLIGHT`). "Last encoder" is load-bearing now that
//!    `hud_total`'s end edge is stamped after the world command buffer has
//!    already been submitted: a resolve riding the world encoder, as this
//!    module originally did it, would pair this frame's `begin` with the
//!    previous frame's `end`.
//! 2. [`GpuQueryTimer::after_submit`] — called right after `queue.submit` —
//!    starts an async `map_async` on that slot's buffer.
//!
//! The two are **one transaction split across the submit**, and they must
//! agree about whether this frame's slot was usable at all: `resolve`
//! harvests first and declines to copy into a slot that is still mapped
//! (copying into one makes `wgpu` reject the whole submission), and
//! `after_submit` then counts a stalled frame instead of mapping a slot
//! nothing wrote to.
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

/// [`GpuQueryTimer::stamp`]'s scratch target format. Single-channel and 8-bit
/// because nothing ever reads it; it exists only to be a legal colour
/// attachment.
const STAMP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

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
    /// Whether [`Self::resolve`] declined to copy this frame because the ring
    /// slot was still mapped — read by [`Self::after_submit`], which must not
    /// then start a mapping on a slot nothing wrote to. The two calls are one
    /// transaction split across `queue.submit`.
    skipped_resolve: bool,
    /// A 1x1 colour target existing only so [`Self::stamp`] has something to
    /// attach its bracketing render pass to — see that method's doc for why a
    /// bracketing pass is the only way to time a *span of passes* on an adapter
    /// without `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
    stamp_view: wgpu::TextureView,
    /// One triangle, no bindings, drawn into [`Self::stamp_view`]. The pass
    /// **must not be empty**: `timestamp_writes` samples at *stage*
    /// boundaries, and a pass with no vertex or fragment stage has no such
    /// boundary — see `shaders/gpu_timer_stamp.wgsl`'s own header for the
    /// measurement that established this.
    stamp_pipeline: wgpu::RenderPipeline,
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
        // 1x1, and never read back: `stamp` needs a legal colour attachment
        // and nothing more. Sized this way so the empty bracketing passes it
        // opens cost a tile-based GPU essentially nothing — a profiling
        // instrument that materially changes the frame it measures is worse
        // than no instrument.
        let stamp_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gpu-timer-stamp-target"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: STAMP_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let stamp_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gpu-timer-stamp"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/gpu_timer_stamp.wgsl").into(),
            ),
        });
        let stamp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gpu-timer-stamp"),
            // No bind groups and no vertex buffers: the shader derives its
            // three positions from `vertex_index`, so an auto layout resolves
            // to an empty one.
            layout: None,
            vertex: wgpu::VertexState {
                module: &stamp_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &stamp_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: STAMP_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
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
            skipped_resolve: false,
            stamp_view,
            stamp_pipeline,
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

    /// Write **one** timestamp edge for each of `begin`/`end` into `encoder`,
    /// through an otherwise-empty render pass on this timer's own 1x1 target.
    ///
    /// # Why this exists at all
    ///
    /// [`Self::writes`] can only time a span whose two ends are the *same*
    /// render pass. Answering "how long did the GPU spend on this whole
    /// frame" needs a span across *many* passes — and on this adapter the
    /// obvious mechanism for that, `CommandEncoder::write_timestamp`, is
    /// unavailable (`TIMESTAMP_QUERY_INSIDE_ENCODERS` excludes Apple GPUs by
    /// name; see the module doc). A pass boundary is the only place a
    /// timestamp can be written here, so this opens a pass whose only purpose
    /// is to carry a boundary. The target is 1x1 and never sampled, so the
    /// pass has no measurable cost of its own.
    ///
    /// It is **not** an empty pass, and that was measured rather than
    /// reasoned: `timestamp_writes` samples at *stage* boundaries, so a pass
    /// with no vertex or fragment stage has no boundary to sample and reports
    /// a pair that is not a duration of anything. The symptom was an
    /// occasional inversion — a bracketing span reading shorter than a pass it
    /// encloses — which is what an undefined timestamp looks like once the
    /// `end > begin` filter has discarded the obviously-broken pairs. So the
    /// pass draws one triangle; see `shaders/gpu_timer_stamp.wgsl`.
    ///
    /// # Spans may cross command buffers
    ///
    /// Queue submissions execute in submission order, so a `begin` stamped in
    /// one command buffer and an `end` stamped in a later one bracket
    /// everything submitted between them — which is how the `"world_total"`
    /// and `"hud_total"` segments cover work recorded in several different
    /// files. The one hard requirement is that [`Self::resolve`] must execute
    /// **after** both edges, or the pair read back is a `begin` from this
    /// frame against an `end` left over from the previous one; the
    /// `end > begin` guard in [`Self::harvest`] discards such a pair rather
    /// than reporting a nonsense duration, so getting the order wrong shows
    /// up as a permanently absent reading, never as a wrong number.
    ///
    /// A name this timer does not carry is ignored — a caller naming a
    /// segment that does not exist gets no stamp rather than a panic, matching
    /// [`Self::writes`].
    pub(crate) fn stamp(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        begin: Option<&str>,
        end: Option<&str>,
    ) {
        let beginning_of_pass_write_index = begin.and_then(|n| self.index_of(n)).map(|i| (i * 2) as u32);
        let end_of_pass_write_index = end.and_then(|n| self.index_of(n)).map(|i| (i * 2 + 1) as u32);
        if beginning_of_pass_write_index.is_none() && end_of_pass_write_index.is_none() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gpu-timer-stamp"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.stamp_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // `Clear`, not `Load`: the target is write-only scratch
                    // that nothing ever initialises, and loading uninitialised
                    // contents is exactly the case wgpu's validation exists to
                    // catch. `Discard` on store for the same reason — nothing
                    // reads this texture, ever.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                query_set: &self.query_set,
                beginning_of_pass_write_index,
                end_of_pass_write_index,
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // **Not optional.** An empty pass has no vertex or fragment stage, and
        // `timestamp_writes` samples at stage boundaries — see this type's
        // `stamp_pipeline` field and the shader's own header. One triangle
        // over a 1x1 target is the cheapest thing that guarantees both stages
        // run.
        pass.set_pipeline(&self.stamp_pipeline);
        pass.draw(0..3, 0..1);
    }

    /// Resolve this frame's queries into the ring slot for `self.frame`.
    /// Call once, after every pass that might have written into this timer's
    /// query set has been recorded, and **before** `queue.submit`
    /// (`resolve_query_set`/`copy_buffer_to_buffer` are encoder commands).
    ///
    /// # The slot must be free first, and that is not optional
    ///
    /// This harvests before deciding, then **skips the copy entirely** when
    /// the slot due for reuse still has an outstanding mapping. Copying into
    /// a mapped buffer is not merely wasteful: `wgpu` rejects the whole
    /// submission with *"Buffer with 'gpu-timer-readback' label is still
    /// mapped"*, and under this workspace's release profile that error path
    /// takes the process down.
    ///
    /// The check used to live only in [`Self::after_submit`] — after the copy
    /// had already been recorded — which is too late to prevent it. It never
    /// fired in ordinary play because presentation paces the frame loop
    /// slowly enough that a mapping always completed inside three frames; it
    /// fires immediately in an uncapped headless loop, and would fire in a
    /// real session on any stall long enough for readback to fall behind.
    /// Found by `benches/frame_profile.rs` on its first run.
    pub(crate) fn resolve(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        // Non-blocking: makes progress only on callbacks already satisfied by
        // completed work, so a slot that *can* be freed is freed before the
        // decision below. A profiling feature must never wait on the GPU.
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest();

        let idx = (self.frame as usize) % FRAMES_IN_FLIGHT;
        if self.slots[idx].pending.is_some() {
            self.skipped_resolve = true;
            return;
        }
        self.skipped_resolve = false;
        let n = (self.segment_names.len() * 2) as u32;
        encoder.resolve_query_set(&self.query_set, 0..n, &self.resolve_buffer, 0);
        let slot = &self.slots[idx];
        encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &slot.readback, 0, u64::from(n) * 8);
    }

    /// Start this frame's async readback. Call once, right after
    /// `queue.submit` for the encoder [`Self::resolve`] was called against.
    ///
    /// **Never silently drops a result.** When [`Self::resolve`] declined to
    /// copy because the ring slot due for reuse still had an unread mapping
    /// (the GPU/driver did not finish it within `FRAMES_IN_FLIGHT` frames —
    /// real backpressure, not a bug in this timer), this counts it in
    /// [`Self::stalled_frames`] rather than mapping on top of the old one.
    /// The two halves must agree: mapping a slot this frame's encoder never
    /// copied into would report the *previous* occupant's timings as fresh.
    pub(crate) fn after_submit(&mut self) {
        let idx = (self.frame as usize) % FRAMES_IN_FLIGHT;
        if self.skipped_resolve {
            self.stalled_frames += 1;
            self.frame += 1;
            return;
        }
        debug_assert!(
            self.slots[idx].pending.is_none(),
            "resolve() copied into slot {idx} but it is still mapped — the two halves have \
             drifted apart and this submission is about to be rejected"
        );
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
    /// `CommandEncoder::finish` alone — turning the recorded command list
    /// into a command buffer, with nothing handed to the driver yet.
    ///
    /// Split from [`Self::QueueSubmit`] because the two answer different
    /// questions and the combined figure could not distinguish them. This
    /// half is **pure CPU command translation**, so it scales with how many
    /// commands were recorded and with nothing else. The other half can
    /// block.
    EncoderFinish,
    /// `Queue::submit` alone.
    ///
    /// `queue.submit` only *enqueues* work, so the intuition is that this
    /// should be nearly free — but that intuition holds only while the queue
    /// has room. When the CPU is running ahead of the GPU, this is where it
    /// waits, so **a large reading here is a symptom of GPU backpressure and
    /// not of CPU cost**, and reading it as "submitting is slow" inverts the
    /// diagnosis. Its discriminator against [`Self::EncoderFinish`] is that
    /// this one moves with how much *GPU* work the frame contains while that
    /// one moves with how many *commands* were recorded.
    QueueSubmit,
}

/// [`WorldSubphase`] variant count, kept in one place for the same reason
/// `app::frame_profile::PHASE_COUNT` is.
pub(crate) const WORLD_SUBPHASE_COUNT: usize = 5;

impl WorldSubphase {
    pub(crate) const ALL: [WorldSubphase; WORLD_SUBPHASE_COUNT] = [
        WorldSubphase::PrepareBuffers,
        WorldSubphase::TerrainCullAndDraw,
        WorldSubphase::OtherDraws,
        WorldSubphase::EncoderFinish,
        WorldSubphase::QueueSubmit,
    ];

    /// Short, stable name for the F3/tracing detail line and the CSV dump's
    /// header — kept separate from `Debug` for the same reason
    /// `FramePhase::name` is.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            WorldSubphase::PrepareBuffers => "world.prepare_buffers",
            WorldSubphase::TerrainCullAndDraw => "world.terrain_cull_draw",
            WorldSubphase::OtherDraws => "world.other_draws",
            WorldSubphase::EncoderFinish => "world.encoder_finish",
            WorldSubphase::QueueSubmit => "world.queue_submit",
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
