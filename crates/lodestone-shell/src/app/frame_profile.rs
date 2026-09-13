//! Per-phase CPU frame timing: where `redraw`'s wall-clock time actually goes.
//!
//! Split out of `app.rs`; see that module's own header for the layout. See
//! `docs/frame-profiling.md` for the operator-facing "how do I read this"
//! doc — this file's docs are about the mechanism.
//!
//! # The phase boundaries, and why they sit where they do
//!
//! [`FramePhase`] names seven checkpoints inside [`super::WindowApp::redraw`].
//! They are **not** a clean "input / tick / mesh / prepare / record / submit
//! / present" split, because `redraw` itself does not have clean seams there
//! — and reporting seams that do not exist would be worse than reporting the
//! real ones:
//!
//! * **Input** is genuinely absent. Raw key/mouse events are handled by
//!   winit callbacks (`app::lifecycle`/`app::input`) outside `redraw`
//!   entirely, so there is nothing to time here. [`FramePhase::Setup`]
//!   starts *after* that — the pacer/option-sync work at the top of
//!   `redraw` — and this module says so rather than mislabelling it "input".
//! * **Record and submit are fused.** `RenderState::render_with_crack_and_effects`
//!   builds its command encoder *and* calls `queue.submit` internally
//!   (`gpu::frame::render_inner`), so `redraw` has no seam between them to
//!   time separately. [`FramePhase::WorldEncodeSubmit`] is that whole call.
//!   Splitting it would need a callback boundary threaded into `gpu/frame.rs`
//!   — itself a file under concurrent edit — for a distinction the CPU side
//!   cannot observe anyway (`queue.submit` only *enqueues* work; the actual
//!   GPU cost is what `gpu::gpu_timing` measures separately).
//! * **HUD, effects and container/menu rendering share one bucket**
//!   ([`FramePhase::HudUiEncodeSubmit`]) because each of those issues its own
//!   `device.create_command_encoder`/`queue.submit` pair in sequence
//!   (`HudRenderer::render_with_item_models`,
//!   the container/menu draws), and none of them individually costs enough
//!   to be worth a separate checkpoint per call — the CPU cost here is
//!   dominated by state gather (colour-stream building), not by the
//!   `queue.submit` calls themselves.
//!
//! # Early returns do not corrupt the ring buffers
//!
//! `redraw` has several early `return`s (no GPU state yet, a failed
//! `acquire()`, window unfocused/occluded) between [`FrameProfiler::begin_frame`]
//! and the phases that follow. Rather than requiring every one of those
//! returns to know about this profiler, [`FrameProfiler::begin_frame`]
//! finalises whatever was pending from the **previous** call before resetting
//! for the new frame: every phase that was marked gets a sample pushed into
//! its ring buffer, and every phase that was *not* reached this frame — an
//! entirely ordinary outcome, e.g. every phase from `Acquire` onward on an
//! unfocused frame — increments that phase's [`PhaseWindow::skipped`] counter
//! rather than being silently absent. So a frame that returns after
//! [`FramePhase::SimTick`] contributes a real `Setup`/`SimTick` sample and a
//! visible skip to every later phase, never a fabricated zero and never a
//! silently missing frame.
use std::time::Duration;

use crate::platform::Instant;

/// Samples kept per phase — about 4 s of frames at 60 fps, fewer at a lower
/// rate (this is a **count**, not a duration, so a slow session simply covers
/// a longer wall-clock span; see the module's evidence-standard note in
/// `docs/frame-profiling.md` for why a count is preferred here).
const WINDOW: usize = 240;

/// One checkpoint in `redraw`'s per-frame timeline. See the module doc for
/// why these are the seven that exist and not the owner's original
/// "input/tick/mesh/prepare/record/submit/present" list verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FramePhase {
    /// Pacing decision, live-option pushes (vsync, view bobbing, damage tilt,
    /// cutout leaves) and resource-pack live-reload — everything before the
    /// simulation steps.
    Setup,
    /// `Sim::step`: the 20 Hz ECS tick(s) this iteration owed, including any
    /// vanilla catch-up.
    SimTick,
    /// Draining completed chunk meshes and uploading them to GPU buffers
    /// (`Sim::drain_removals`/`drain_meshes` + `RenderState::upload_section`).
    /// The mesh **computation** itself happens off this thread earlier; this
    /// phase is the upload, which is the part that actually costs wall time
    /// on the frame thread.
    MeshUpload,
    /// `SurfaceTarget::acquire` — can stall for real on a backgrounded/occluded
    /// window (macOS stops vending `CAMetalLayer` drawables); see
    /// `app::pacing::FramePacer`'s module doc for why presentation is skipped
    /// rather than ticking, and why this phase existing separately from
    /// `Setup` is what lets a stall here be told apart from pacing cost.
    Acquire,
    /// Per-frame render-state gather between a successful `acquire()` and the
    /// world-render call: camera/entity resolution, crack targets, screen
    /// effects, hotbar snapshots, remote-skin polling — everything
    /// `render_with_crack_and_effects` needs that is not itself GPU work.
    Prepare,
    /// `RenderState::render_with_crack_and_effects` — world geometry command
    /// recording **and** `queue.submit`, fused; see the module doc.
    WorldEncodeSubmit,
    /// HUD, status-effect overlay and container/menu rendering — each its own
    /// encoder/submit pair; see the module doc for why they share one bucket.
    HudUiEncodeSubmit,
    /// `pre_present_notify` + `SurfaceFrame::present`.
    Present,
}

/// [`FramePhase`] variant count — kept in one place so [`FrameProfiler`]'s
/// arrays, and [`super::frame_profile_dump::DumpWriter`]'s row shape, cannot
/// drift out of sync with the enum by one missed match arm.
pub(super) const PHASE_COUNT: usize = 8;

impl FramePhase {
    pub(super) const ALL: [FramePhase; PHASE_COUNT] = [
        FramePhase::Setup,
        FramePhase::SimTick,
        FramePhase::MeshUpload,
        FramePhase::Acquire,
        FramePhase::Prepare,
        FramePhase::WorldEncodeSubmit,
        FramePhase::HudUiEncodeSubmit,
        FramePhase::Present,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Short, stable name for the F3 overlay and the tracing line — kept
    /// separate from `Debug` so a rename of the enum variant (a refactor) does
    /// not silently reformat every historical `LODESTONE_FRAME_PROFILE_DUMP`
    /// CSV's header.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            FramePhase::Setup => "setup",
            FramePhase::SimTick => "sim_tick",
            FramePhase::MeshUpload => "mesh_upload",
            FramePhase::Acquire => "acquire",
            FramePhase::Prepare => "prepare",
            FramePhase::WorldEncodeSubmit => "world_encode_submit",
            FramePhase::HudUiEncodeSubmit => "hud_ui_encode_submit",
            FramePhase::Present => "present",
        }
    }
}

/// One checkpoint inside [`FramePhase::HudUiEncodeSubmit`], the counterpart to
/// `gpu::gpu_timing::WorldSubphase` for the other half of the frame.
///
/// # Why these six, and why they are not a `FramePhase`
///
/// These are **nested inside** one phase's span rather than sequential
/// siblings of it, so they cannot share [`FrameProfiler`]'s single
/// "elapsed since the previous mark" cursor — [`FrameProfiler`] keeps a second
/// cursor for them, reset automatically whenever
/// [`FramePhase::WorldEncodeSubmit`] is marked (which is precisely where this
/// phase begins).
///
/// Unlike the world's sub-phases, every boundary here is inside
/// `app::redraw`, so no thread-local bridge is needed: `redraw` calls
/// [`FrameProfiler::mark_hud`] directly at seams it already has.
///
/// The split is by **what the work is**, not by encoder, because the CPU cost
/// here turned out not to sit where the phase's name suggests: `redraw` spends
/// most of this phase *gathering* the state a `HudFrame` needs — chat spans,
/// the tab list, boss bars, locator dots, effect icons — before any encoder
/// exists at all. Folding that into a bucket called "hud ui encode submit"
/// invited exactly the wrong conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HudSubphase {
    /// The `if self.show_debug` block: building this profiler's own F3 lines
    /// and the pie-chart snapshot.
    ///
    /// **This is the observer-effect number.** It is zero whenever F3 is
    /// closed, and F3 open is the only state in which anyone reads the frame
    /// rate off the overlay — so any conclusion drawn from an on-screen fps
    /// figure has to be read against this line. `summary()` sorts a ring
    /// buffer per phase, which is why the block is gated at all.
    DebugGather,
    /// Everything between that block and the HUD draw: chat span/wrap
    /// building, tab-list and boss-bar snapshots, locator dots, effect icons,
    /// hotbar records, and the `HudFrame` field assignment itself. No GPU work
    /// whatsoever — this is pure state gather.
    FrameGather,
    /// `HudRenderer::render_with_item_models` — its own encoder and submit.
    HudDraw,
    /// The creative/container/recipe-book renderers, when one is open. Zero
    /// on an ordinary playing frame, which is what makes it worth separating
    /// from the HUD draw rather than fusing the two.
    ContainerDraw,
    /// Every `menu::render` overlay draw — pause, death, advancements,
    /// settings, statistics, links, social, command block, sign edit, book
    /// edit, spectator menu — plus the screenshot copy. Counted as well as
    /// timed (`HudSubphaseCounts::menu_overlays_drawn`): several of these can
    /// stack in one frame.
    MenuOverlays,
    /// [`RenderState::gpu_timing_end_frame`](crate::gpu::RenderState::gpu_timing_end_frame)
    /// — the stamp/resolve/submit that closes GPU timing for the frame.
    ///
    /// This is the profiler paying for itself, reported rather than hidden:
    /// it is one extra command-buffer submission per frame that exists only
    /// because this instrument does. If it ever grows past the phases it
    /// exists to measure, the instrument is the bug.
    GpuTimingEnd,
}

/// [`HudSubphase`] variant count — same one-place rule as [`PHASE_COUNT`].
pub(crate) const HUD_SUBPHASE_COUNT: usize = 6;

impl HudSubphase {
    pub(crate) const ALL: [HudSubphase; HUD_SUBPHASE_COUNT] = [
        HudSubphase::DebugGather,
        HudSubphase::FrameGather,
        HudSubphase::HudDraw,
        HudSubphase::ContainerDraw,
        HudSubphase::MenuOverlays,
        HudSubphase::GpuTimingEnd,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Short, stable name for the F3 detail line and the CSV header — kept
    /// separate from `Debug` for the same reason [`FramePhase::name`] is.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            HudSubphase::DebugGather => "hud.debug_gather",
            HudSubphase::FrameGather => "hud.frame_gather",
            HudSubphase::HudDraw => "hud.hud_draw",
            HudSubphase::ContainerDraw => "hud.container_draw",
            HudSubphase::MenuOverlays => "hud.menu_overlays",
            HudSubphase::GpuTimingEnd => "hud.gpu_timing_end",
        }
    }
}

/// Counts alongside [`HudSubphase`]'s timings, for the same reason
/// `gpu::gpu_timing::WorldSubphaseCounts` exists: "2 ms of chat gather" is a
/// different problem at 10 lines than at 100, and a duration alone cannot tell
/// the two apart.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HudSubphaseCounts {
    /// Chat lines gathered into the `HudFrame` this frame. Jumps roughly
    /// tenfold when the chat box is open (10 recent lines vs 100), which is
    /// the single largest swing in `HudSubphase::FrameGather`'s input.
    pub chat_lines: usize,
    /// Lines this profiler formatted for the F3 overlay this frame — `0`
    /// whenever the overlay is closed, so `hud.debug_gather` and this move
    /// together or one of them is lying.
    pub debug_lines: usize,
    /// `menu::render` overlays drawn this frame. Several can stack (a
    /// settings page over a paused world over a loading screen), so this is a
    /// count and not a flag.
    pub menu_overlays_drawn: usize,
}

/// A fixed-size ring buffer of one phase's per-frame durations, in
/// milliseconds.
#[derive(Debug)]
struct PhaseWindow {
    samples: [f32; WINDOW],
    /// Next write position; wraps modulo `WINDOW`.
    next: usize,
    /// Samples written so far, capped at `WINDOW`. `len < WINDOW` is "this
    /// window has not settled yet" — see [`FrameProfiler::summary`]'s doc for
    /// where that is surfaced to a caller.
    len: usize,
    /// Frames where this phase was never reached (see the module doc's
    /// "early returns" section). Cumulative for the process lifetime, not
    /// windowed — a rate over an arbitrary window would itself need a second
    /// window to trust.
    skipped: u64,
}

impl PhaseWindow {
    const fn new() -> Self {
        Self {
            samples: [0.0; WINDOW],
            next: 0,
            len: 0,
            skipped: 0,
        }
    }

    fn push(&mut self, ms: f32) {
        self.samples[self.next] = ms;
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    fn mean(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        self.samples[..self.len].iter().sum::<f32>() / self.len as f32
    }

    /// `p` in `[0.0, 1.0]`. Sorts a stack copy of the live samples — `WINDOW`
    /// is small enough (240) that this costs microseconds, and it is only
    /// ever called from the F3 overlay / the once-a-second tracing line, never
    /// once per frame from the hot path.
    fn percentile(&self, p: f32) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let mut sorted = self.samples;
        let sorted = &mut sorted[..self.len];
        sorted.sort_unstable_by(|a, b| a.total_cmp(b));
        let idx = ((p * (self.len - 1) as f32).round() as usize).min(self.len - 1);
        sorted[idx]
    }
}

/// Owns the per-phase ring buffers and the current frame's in-progress marks.
/// See the module doc for the finalise-on-next-`begin_frame` design.
#[derive(Debug)]
pub(crate) struct FrameProfiler {
    windows: [PhaseWindow; PHASE_COUNT],
    /// This frame's recorded duration per phase, `None` until [`Self::mark`]
    /// closes it out. Cleared (after finalising into `windows`) at the top of
    /// every [`Self::begin_frame`].
    pending: [Option<f32>; PHASE_COUNT],
    /// Start-to-start interval for the pending frame. It becomes known at
    /// the following [`Self::begin_frame`], immediately before that frame is
    /// finalised into the CSV.
    pending_interval_ms: Option<f32>,
    /// Segment captured when the pending frame began. Kept separate from
    /// `segment` so a transition at the next frame boundary cannot relabel
    /// the frame being finalised.
    pending_segment: Option<&'static str>,
    /// Segment selected by the benchmark driver for the next frame.
    segment: Option<&'static str>,
    /// Start of the pending frame, used to derive its start-to-start
    /// interval when the following frame begins. `None` before the first
    /// call to [`Self::begin_frame`].
    last_frame_start: Option<Instant>,
    /// Wall-clock instant the *previous* mark (or `begin_frame`) was taken —
    /// what the next `mark` measures elapsed time against.
    cursor: Instant,
    /// Total frames [`Self::begin_frame`] has been called for — the CSV
    /// dump's row index.
    frame_count: u64,
    dump: Option<super::frame_profile_dump::DumpWriter>,
    /// Last time [`Self::report_due`] fired. The tracing report owns its own
    /// cadence so it remains independent of the explicit headless summary.
    last_report: Instant,
    /// Rolling windows for `world_encode_submit`'s own internal breakdown —
    /// see `gpu::gpu_timing::WorldSubphase`'s doc for what each covers and
    /// why this data arrives through a thread-local rather than as a new
    /// `FramePhase` variant (these are *nested inside* one phase's span, not
    /// additional sequential siblings of it, so they cannot share
    /// `FrameProfiler`'s single "elapsed since previous mark" cursor).
    world_subphase_windows: [PhaseWindow; crate::gpu::gpu_timing::WORLD_SUBPHASE_COUNT],
    /// The last frame's `WorldSubphaseCounts`, alongside the timings above —
    /// not windowed, mirroring how `RenderStats` itself is a last-frame-only
    /// snapshot everywhere else in this shell.
    world_subphase_counts: Option<crate::gpu::gpu_timing::WorldSubphaseCounts>,
    /// Frames where `FramePhase::WorldEncodeSubmit` itself got a real sample
    /// but `gpu::gpu_timing::take_world_subphases` returned `None` for one of
    /// the four slots. This is a **health check on the bridge**, not an
    /// ordinary "early return" skip: every real call to `render_inner`
    /// records all four sub-phases unconditionally, so a nonzero count here
    /// means this struct's draining logic and `gpu/frame.rs`'s checkpoints
    /// have drifted apart (a renamed/added `WorldSubphase` variant on one
    /// side only, for instance) — never expected to move on a healthy build.
    world_subphase_bridge_misses: u64,
    /// Rolling windows for `hud_ui_encode_submit`'s own internal breakdown —
    /// see [`HudSubphase`]'s doc.
    hud_subphase_windows: [PhaseWindow; HUD_SUBPHASE_COUNT],
    /// This frame's recorded HUD sub-phase durations, `None` until
    /// [`Self::mark_hud`] closes each one out. Finalised into
    /// `hud_subphase_windows` alongside the phases proper, so a frame that
    /// never opened a container contributes a **skip** to
    /// `hud.container_draw` rather than a `0.0` sample — a phase that did not
    /// run and a phase that ran for free must not read the same, exactly as
    /// for [`FramePhase`].
    hud_pending: [Option<f32>; HUD_SUBPHASE_COUNT],
    /// Second cursor, for the sub-phases nested inside
    /// [`FramePhase::HudUiEncodeSubmit`] — see [`HudSubphase`]'s doc for why
    /// `cursor` cannot serve. Re-based automatically when
    /// [`FramePhase::WorldEncodeSubmit`] is marked.
    hud_cursor: Instant,
    /// The last frame's [`HudSubphaseCounts`], alongside the timings above —
    /// not windowed, mirroring `world_subphase_counts`.
    hud_subphase_counts: Option<HudSubphaseCounts>,
    /// Relighting work accumulated while simulating this frame. Unlike GPU
    /// workload counts, zero is a real observation: no block update ran.
    relight_workload: crate::mesher::RelightWorkload,
}

impl FrameProfiler {
    /// A profiler whose clock starts at `now`. `dump_path` is
    /// `LODESTONE_FRAME_PROFILE_DUMP`'s value, if set and openable — see
    /// `frame_profile_dump`'s module doc for what happens when it is set but
    /// not openable (never silent).
    pub(crate) fn new(now: Instant, dump_path: Option<&std::path::Path>) -> Self {
        Self {
            windows: [const { PhaseWindow::new() }; PHASE_COUNT],
            pending: [None; PHASE_COUNT],
            pending_interval_ms: None,
            pending_segment: None,
            segment: None,
            last_frame_start: None,
            cursor: now,
            frame_count: 0,
            dump: dump_path.map(super::frame_profile_dump::DumpWriter::open),
            last_report: now,
            world_subphase_windows: [const { PhaseWindow::new() };
                crate::gpu::gpu_timing::WORLD_SUBPHASE_COUNT],
            world_subphase_counts: None,
            world_subphase_bridge_misses: 0,
            hud_subphase_windows: [const { PhaseWindow::new() }; HUD_SUBPHASE_COUNT],
            hud_pending: [None; HUD_SUBPHASE_COUNT],
            hud_cursor: now,
            hud_subphase_counts: None,
            relight_workload: crate::mesher::RelightWorkload::default(),
        }
    }

    /// Whether `interval` has elapsed since the last time this returned
    /// `true` (or since construction). Advances by whole `interval`s from
    /// itself rather than from `now`, matching `app::pacing::FramePacer`'s
    /// own reasoning for why an absolute schedule beats "elapsed since last
    /// fire": otherwise a caller polling faster than `interval` can drift the
    /// effective cadence slower than requested.
    pub(crate) fn report_due(&mut self, now: Instant, interval: Duration) -> bool {
        if now.saturating_duration_since(self.last_report) < interval {
            return false;
        }
        self.last_report += interval;
        if self.last_report + interval <= now {
            // A real stall (window minimised, breakpoint, …), not ordinary
            // jitter: re-base rather than firing a burst of catch-up reports.
            self.last_report = now;
        }
        true
    }

    /// Start timing a new frame at `now`. Finalises the *previous* frame
    /// first — see the module doc for why finalisation is deferred to here
    /// rather than requiring every `redraw` early-return to call something.
    pub(crate) fn begin_frame(&mut self, now: Instant) {
        if let Some(previous_start) = self.last_frame_start {
            self.pending_interval_ms = Some(
                now.saturating_duration_since(previous_start).as_secs_f32() * 1000.0,
            );
            self.finalise();
        }
        self.pending_segment = self.segment;
        self.last_frame_start = Some(now);
        self.cursor = now;
    }

    /// Label the next frame begun with [`Self::begin_frame`]. Ordinary play
    /// leaves this as `None`, which writes an empty CSV field.
    pub(crate) fn set_segment(&mut self, segment: Option<&'static str>) {
        self.segment = segment;
    }

    /// Close out the phase that ran between the last mark (or `begin_frame`)
    /// and `now`, and start timing the next one from `now`. Call once per
    /// phase, in the phase's own natural position in `redraw` — a phase not
    /// reached this frame (an early return before it) simply never gets a
    /// `mark` call, which [`Self::finalise`] turns into a skip rather than a
    /// zero.
    pub(crate) fn mark(&mut self, phase: FramePhase, now: Instant) {
        let ms = now.saturating_duration_since(self.cursor).as_secs_f32() * 1000.0;
        self.pending[phase.index()] = Some(ms);
        self.cursor = now;
        // Drain the world-encode sub-phase bridge (`gpu::gpu_timing`) right
        // here, at the exact point the data is freshest — `render_inner`
        // (`gpu/frame.rs`) has just returned, so whatever it recorded this
        // call is still sitting in the thread-local untouched. See that
        // module's doc for why this cannot instead ride in through
        // `RenderStats`/a `RenderState` field.
        if phase == FramePhase::WorldEncodeSubmit {
            self.drain_world_subphases();
            // `hud_ui_encode_submit` starts exactly where `world_encode_submit`
            // ends, so its own cursor re-bases here rather than needing a
            // `begin_hud` call site in `redraw` that could be forgotten (and
            // whose absence would silently attribute the whole previous frame
            // to the first HUD sub-phase).
            self.hud_cursor = now;
        }
    }

    /// Close out one [`HudSubphase`] and start the next, exactly as
    /// [`Self::mark`] does for a [`FramePhase`] — against this profiler's
    /// separate HUD cursor. A sub-phase never marked this frame becomes a
    /// skip, never a `0.0` sample.
    pub(crate) fn mark_hud(&mut self, sub: HudSubphase, now: Instant) {
        let ms = now.saturating_duration_since(self.hud_cursor).as_secs_f32() * 1000.0;
        self.hud_pending[sub.index()] = Some(ms);
        self.hud_cursor = now;
    }

    /// Record this frame's [`HudSubphaseCounts`]. Overwrites rather than
    /// accumulating: `redraw` calls it once per frame with the whole set.
    pub(crate) fn record_hud_counts(&mut self, counts: HudSubphaseCounts) {
        self.hud_subphase_counts = Some(counts);
    }

    /// Record relighting work consumed from `Sim` for this frame.
    pub(crate) fn record_relight_workload(&mut self, workload: crate::mesher::RelightWorkload) {
        self.relight_workload = workload;
    }

    fn drain_world_subphases(&mut self) {
        let (timings, counts) = crate::gpu::gpu_timing::take_world_subphases();
        for (i, sample) in timings.into_iter().enumerate() {
            match sample {
                Some(ms) => self.world_subphase_windows[i].push(ms),
                None => self.world_subphase_bridge_misses += 1,
            }
        }
        // Only overwrite the last-known counts when this call actually had
        // fresh ones — a frame where `gpu/frame.rs`'s counts checkpoint was
        // somehow skipped should keep showing the last real reading, not
        // fall back to a fabricated default (all-zero would read as "zero
        // sections visited", which is never true once a world exists).
        if counts.is_some() {
            self.world_subphase_counts = counts;
        }
    }

    fn finalise(&mut self) {
        // `begin_frame` calls this only after observing `last_frame_start`,
        // so even a frame that returned before its first phase mark is real
        // and must contribute skips. The old pending-array guard discarded
        // exactly that all-skipped first frame.
        self.frame_count += 1;
        let interval_ms = self.pending_interval_ms.take();
        let segment = self.pending_segment.take();
        let mut world_encode_ran_this_frame = false;
        // Captured before `pending` is cleared, because the CSV row below has
        // to be able to tell a phase that was skipped from one that was
        // measured — see the `row` binding's own comment for the bug this
        // replaced.
        let row: [Option<f32>; PHASE_COUNT] = self.pending;
        for phase in FramePhase::ALL {
            let i = phase.index();
            match self.pending[i].take() {
                Some(ms) => {
                    self.windows[i].push(ms);
                    if phase == FramePhase::WorldEncodeSubmit {
                        world_encode_ran_this_frame = true;
                    }
                }
                None => self.windows[i].skipped += 1,
            }
        }
        let hud_row: [Option<f32>; HUD_SUBPHASE_COUNT] = self.hud_pending;
        for sub in HudSubphase::ALL {
            let i = sub.index();
            match self.hud_pending[i].take() {
                Some(ms) => self.hud_subphase_windows[i].push(ms),
                None => self.hud_subphase_windows[i].skipped += 1,
            }
        }
        if let Some(dump) = &mut self.dump {
            // `row` is this frame's `pending` verbatim, so a skipped phase
            // writes an empty CSV field. It used to be read back out of the
            // ring buffer instead — `self.windows[i].samples[last].into()`,
            // which is `Some(_)` unconditionally, so **every skipped phase
            // silently inherited the last frame that did run it** and the
            // "never a fabricated value" contract this module's own doc
            // states was broken for the dump alone. The tell was that no
            // dump row ever had an empty field, on any session.
            // Only the frame that actually ran `render_inner` gets a real
            // world-subphase row — `world_subphase_windows` otherwise still
            // holds the *previous* real frame's values (see
            // `drain_world_subphases`'s doc), and stamping those onto a
            // skipped frame's row would misattribute them.
            let world_row: [Option<f32>; crate::gpu::gpu_timing::WORLD_SUBPHASE_COUNT] =
                if world_encode_ran_this_frame {
                    std::array::from_fn(|i| {
                        let w = &self.world_subphase_windows[i];
                        (w.len > 0).then(|| w.samples[(w.next + WINDOW - 1) % WINDOW])
                    })
                } else {
                    [None; crate::gpu::gpu_timing::WORLD_SUBPHASE_COUNT]
                };
            let world_counts = world_encode_ran_this_frame
                .then_some(self.world_subphase_counts)
                .flatten();
            let hud_counts = row[FramePhase::HudUiEncodeSubmit.index()]
                .is_some()
                .then_some(self.hud_subphase_counts)
                .flatten();
            dump.write_row(
                self.frame_count,
                interval_ms,
                segment,
                row,
                world_row,
                hud_row,
                world_counts,
                hud_counts,
                self.relight_workload,
            );
        }
    }

    /// `(name, mean_ms, p95_ms, p99_ms, samples, skipped)` for every phase, in
    /// [`FramePhase::ALL`] order. `samples < window` (both carried on
    /// [`PhaseSummary`]) means this window has not settled yet — a caller
    /// must show that ratio rather than presenting an early percentile with
    /// the same confidence as one over a full window.
    pub(crate) fn summary(&self) -> impl Iterator<Item = PhaseSummary> + '_ {
        FramePhase::ALL.into_iter().map(move |phase| {
            let w = &self.windows[phase.index()];
            PhaseSummary {
                phase,
                mean_ms: w.mean(),
                p95_ms: w.percentile(0.95),
                p99_ms: w.percentile(0.99),
                samples: w.len,
                window: WINDOW,
                skipped: w.skipped,
                // `world_encode_submit`'s own internal breakdown, appended
                // to its line by `PhaseSummary::line`. `None` for every
                // other phase, and `None` here too until the bridge has a
                // real reading (never a fabricated empty bracket) — see
                // `world_subphase_detail`'s doc.
                detail: match phase {
                    FramePhase::WorldEncodeSubmit => self.world_subphase_detail(),
                    FramePhase::HudUiEncodeSubmit => self.hud_subphase_detail(),
                    _ => None,
                },
            }
        })
    }

    /// `"world.prepare_buffers: mean/p95/p99 ms, ... | sections visited: N
    /// packed + M model"` — the sub-phase breakdown for
    /// [`FramePhase::WorldEncodeSubmit`]'s own F3/tracing line. `None` until
    /// at least one sub-phase window has a real sample (the first
    /// `WorldEncodeSubmit` mark of a session, or a build where
    /// `gpu/frame.rs`'s checkpoints were never reached), matching every
    /// other "no reading yet" case this instrument reports — never a
    /// fabricated `0.00`.
    fn world_subphase_detail(&self) -> Option<String> {
        if self.world_subphase_windows.iter().all(|w| w.len == 0) {
            return None;
        }
        let mut parts: Vec<String> = crate::gpu::gpu_timing::WorldSubphase::ALL
            .into_iter()
            .zip(&self.world_subphase_windows)
            .map(|(sp, w)| {
                // A sub-phase with zero samples so far (the other three, on
                // the very first frame that ever recorded any of them) must
                // read as "no reading yet", never a fabricated `0.00/0.00/0.00`
                // sitting next to a sub-phase with a real one — the same
                // "zero looks free" trap `gpu::gpu_timing::GpuQueryTimer`'s
                // own `have_result` flag exists to avoid.
                if w.len == 0 {
                    format!("{}: <no reading yet>", sp.name())
                } else {
                    format!(
                        "{}: {:.2}/{:.2}/{:.2} ms",
                        sp.name(),
                        w.mean(),
                        w.percentile(0.95),
                        w.percentile(0.99)
                    )
                }
            })
            .collect();
        if let Some(counts) = &self.world_subphase_counts {
            parts.push(format!(
                "sections visited: {} packed + {} model",
                counts.packed_sections_visited, counts.model_sections_visited
            ));
        }
        if self.world_subphase_bridge_misses > 0 {
            parts.push(format!("bridge_misses: {}", self.world_subphase_bridge_misses));
        }
        Some(parts.join(", "))
    }

    /// `"hud.debug_gather: mean/p95/p99 ms, ... | chat lines: N, ..."` — the
    /// sub-phase breakdown appended to [`FramePhase::HudUiEncodeSubmit`]'s own
    /// F3/tracing line, the counterpart to [`Self::world_subphase_detail`].
    /// `None` until at least one sub-phase window has a real sample.
    fn hud_subphase_detail(&self) -> Option<String> {
        if self.hud_subphase_windows.iter().all(|w| w.len == 0) {
            return None;
        }
        let mut parts: Vec<String> = HudSubphase::ALL
            .into_iter()
            .zip(&self.hud_subphase_windows)
            .map(|(sub, w)| {
                // A sub-phase that has never run — `hud.container_draw` on a
                // session where no container was ever opened — reports its
                // skip count rather than a `0.00` mean. Those two are the same
                // number and mean opposite things, which is exactly the trap
                // `GpuQueryTimer::have_result` exists for one layer down.
                if w.len == 0 {
                    format!("{}: <never ran, {} skip>", sub.name(), w.skipped)
                } else {
                    format!(
                        "{}: {:.2}/{:.2}/{:.2} ms ({} skip)",
                        sub.name(),
                        w.mean(),
                        w.percentile(0.95),
                        w.percentile(0.99),
                        w.skipped,
                    )
                }
            })
            .collect();
        if let Some(counts) = &self.hud_subphase_counts {
            parts.push(format!(
                "chat lines: {}, debug lines: {}, menu overlays: {}",
                counts.chat_lines, counts.debug_lines, counts.menu_overlays_drawn
            ));
        }
        Some(parts.join(", "))
    }
}

/// One phase's summary statistics, as returned by [`FrameProfiler::summary`].
#[derive(Debug, Clone)]
pub(crate) struct PhaseSummary {
    pub phase: FramePhase,
    pub mean_ms: f32,
    pub p95_ms: f32,
    pub p99_ms: f32,
    /// Samples currently in the ring buffer (`<= window`).
    pub samples: usize,
    /// The ring buffer's capacity — [`WINDOW`], threaded through so a display
    /// site never has to import the constant to say "12/240".
    pub window: usize,
    pub skipped: u64,
    /// A sub-phase breakdown for this phase, if one exists — today only
    /// [`FramePhase::WorldEncodeSubmit`] carries one (`world_subphase_detail`),
    /// sourced from `gpu::gpu_timing`'s thread-local bridge. `None` for
    /// every other phase, including `HudUiEncodeSubmit`: that bucket's own
    /// internal calls (`HudRenderer::render_with_item_models`,
    /// the container/menu draws) live in files
    /// outside this instrument's edit scope today, so it is not broken down
    /// further — see `docs/frame-profiling.md`'s "How to change it" section
    /// for where the next checkpoint would go.
    pub detail: Option<String>,
}

impl PhaseSummary {
    /// One F3-line-shaped string: `"setup: 0.12/0.30/0.41 ms (240/240, 0 skip)"`,
    /// with `" [detail]"` appended when [`Self::detail`] is `Some`.
    #[must_use]
    pub(crate) fn line(&self) -> String {
        let base = format!(
            "{}: {:.2}/{:.2}/{:.2} ms ({}/{}, {} skip)",
            self.phase.name(),
            self.mean_ms,
            self.p95_ms,
            self.p99_ms,
            self.samples,
            self.window,
            self.skipped
        );
        match &self.detail {
            Some(detail) => format!("{base} [{detail}]"),
            None => base,
        }
    }
}

/// Env var naming the raw per-frame CSV dump path. Named in
/// `docs/frame-profiling.md`.
pub(crate) const DUMP_ENV_VAR: &str = "LODESTONE_FRAME_PROFILE_DUMP";

#[cfg(test)]
mod tests {
    use super::*;

    /// The magnitude species this repo's evidence standard asks for: sleep a
    /// known duration inside a marked phase and require the reported figure
    /// to land on it, not merely be positive. `18` ms rather than a round
    /// `20`/`50`/`100` — the round-number trap this repo has paid for is an
    /// input chosen to make arms coincide, and a sleep duration is exactly
    /// the kind of "plausible round number" that trap warns about.
    #[test]
    fn mark_reports_the_real_elapsed_time_not_a_placeholder() {
        let t0 = Instant::now();
        let mut p = FrameProfiler::new(t0, None);
        p.begin_frame(t0);
        std::thread::sleep(Duration::from_millis(18));
        p.mark(FramePhase::Setup, Instant::now());
        p.begin_frame(Instant::now());
        let setup = p.summary().find(|s| s.phase == FramePhase::Setup).unwrap();
        assert_eq!(setup.samples, 1);
        assert!(
            (15.0..=60.0).contains(&setup.mean_ms),
            "expected ~18ms (wide tolerance for scheduler jitter), got {}",
            setup.mean_ms
        );
    }

    /// A phase never marked this frame must show as a skip, never as a
    /// fabricated `0.0` sample — the control: assert the sample count stays
    /// at zero and the skip counter moves, not merely that nothing panicked.
    #[test]
    fn an_unreached_phase_is_a_skip_not_a_zero_sample() {
        let t0 = Instant::now();
        let mut p = FrameProfiler::new(t0, None);
        p.begin_frame(t0);
        p.mark(FramePhase::Setup, t0 + Duration::from_millis(1));
        // Early return: SimTick onward never marked this frame.
        p.begin_frame(t0 + Duration::from_millis(2));

        let setup = p.summary().find(|s| s.phase == FramePhase::Setup).unwrap();
        assert_eq!(setup.samples, 1, "the phase that WAS marked must get a sample");
        assert_eq!(setup.skipped, 0);

        let tick = p.summary().find(|s| s.phase == FramePhase::SimTick).unwrap();
        assert_eq!(
            tick.samples, 0,
            "an unreached phase must contribute zero samples, not a 0.0 ms one"
        );
        assert_eq!(tick.skipped, 1);
    }

    /// A percentile over a window that has not filled yet must still be
    /// computed (never a placeholder), but the caller-visible `samples`/
    /// `window` pair must say it has not settled — this is the assertable
    /// half of that contract.
    #[test]
    fn a_partially_filled_window_reports_its_own_fill_state() {
        let t0 = Instant::now();
        let mut p = FrameProfiler::new(t0, None);
        for i in 0..5u64 {
            p.begin_frame(t0 + Duration::from_millis(i * 10));
            p.mark(FramePhase::Present, t0 + Duration::from_millis(i * 10 + 1));
        }
        p.begin_frame(t0 + Duration::from_millis(60));
        let present = p.summary().find(|s| s.phase == FramePhase::Present).unwrap();
        assert_eq!(present.samples, 5);
        assert_eq!(present.window, WINDOW);
        assert!(present.samples < present.window);
    }

    /// Percentile ordering sanity, against hand-picked non-round values so a
    /// mean/percentile mixup (the two are computed by different code paths
    /// here) cannot hide behind coincidentally equal numbers.
    #[test]
    fn percentile_is_monotonic_and_not_the_mean() {
        let mut w = PhaseWindow::new();
        for ms in [1.1, 2.3, 3.7, 40.9, 5.2, 6.6, 7.4, 8.8, 9.1, 55.5] {
            w.push(ms);
        }
        let p50 = w.percentile(0.50);
        let p95 = w.percentile(0.95);
        let p99 = w.percentile(0.99);
        assert!(p50 <= p95, "p50 {p50} should be <= p95 {p95}");
        assert!(p95 <= p99, "p95 {p95} should be <= p99 {p99}");
        assert_ne!(p95, w.mean(), "chosen fixture must discriminate percentile from mean");
    }

    /// The magnitude control for the HUD sub-phase breakdown: sleep a
    /// non-round interval inside one marked sub-phase and require the
    /// `hud_ui_encode_submit` detail line to land on it — not merely to exist.
    ///
    /// **Watched failing**: changing `mark_hud` to record `0.0` instead of the
    /// real elapsed time makes the parsed mean land at `0.00` and this test's
    /// range assertion fail immediately; deleting the `hud_cursor` re-base in
    /// `mark` makes the first sub-phase absorb the whole previous frame and it
    /// fails the same way from the other side. A version asserting only
    /// `detail.is_some()` would pass against both.
    #[test]
    fn hud_subphase_detail_reports_the_real_sub_phase_time_and_counts() {
        let now = Instant::now();
        let mut p = FrameProfiler::new(now, None);
        p.begin_frame(now);
        // `mark` on WorldEncodeSubmit is what re-bases the HUD cursor — the
        // production sequence, not a private setter, so this exercises the
        // real seam.
        p.mark(FramePhase::WorldEncodeSubmit, Instant::now());
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(21));
        p.mark_hud(HudSubphase::FrameGather, Instant::now());
        let slept_ms = t0.elapsed().as_secs_f32() * 1000.0;
        // Pairwise-distinct so a transposition of the three count fields
        // cannot survive the assertion below.
        p.record_hud_counts(HudSubphaseCounts {
            chat_lines: 37,
            debug_lines: 12,
            menu_overlays_drawn: 3,
        });
        p.begin_frame(Instant::now());

        let hud = p
            .summary()
            .find(|s| s.phase == FramePhase::HudUiEncodeSubmit)
            .unwrap();
        let detail = hud
            .detail
            .expect("HudUiEncodeSubmit must carry a sub-phase detail once one has a real sample");
        assert!(
            detail.contains("chat lines: 37, debug lines: 12, menu overlays: 3"),
            "detail must carry the exact counts recorded: {detail}"
        );
        let mean: f32 = detail
            .split("hud.frame_gather: ")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .expect("detail must contain a mean for the sub-phase that was marked")
            .parse()
            .expect("mean figure must be a valid float");
        assert!(
            (15.0..=80.0).contains(&mean),
            "expected ~{slept_ms:.1}ms (wide tolerance for scheduler jitter), got {mean} from \
             {detail}"
        );
    }

    /// A HUD sub-phase that never ran — `hud.container_draw` on a session
    /// where no container was ever opened — must report as a skip, never as a
    /// `0.00` mean sitting beside sub-phases that really did cost nothing.
    /// Those two are the same number and mean opposite things.
    #[test]
    fn a_hud_subphase_that_never_ran_reads_as_a_skip_not_as_free() {
        let now = Instant::now();
        let mut p = FrameProfiler::new(now, None);
        for _ in 0..3 {
            p.begin_frame(Instant::now());
            p.mark(FramePhase::WorldEncodeSubmit, Instant::now());
            p.mark_hud(HudSubphase::FrameGather, Instant::now());
        }
        p.begin_frame(Instant::now());

        let detail = p
            .summary()
            .find(|s| s.phase == FramePhase::HudUiEncodeSubmit)
            .unwrap()
            .detail
            .expect("one sub-phase had real samples, so a detail must exist");
        assert!(
            detail.contains("hud.container_draw: <never ran, 3 skip>"),
            "a sub-phase with no samples must name its skip count rather than showing a mean: \
             {detail}"
        );
        assert!(
            detail.contains("hud.frame_gather: "),
            "the sub-phase that did run must still report a mean: {detail}"
        );
    }

    /// The control for the CSV dump's skip handling. A phase not reached this
    /// frame must write an **empty** field, so a spreadsheet cannot average a
    /// skip into a real cost.
    ///
    /// **This test was written against a real defect and observed failing.**
    /// `finalise` built its row by reading the ring buffer back with
    /// `self.windows[i].samples[last].into()` — an `f32 -> Option<f32>`
    /// conversion that is `Some(_)` unconditionally — so every skipped phase
    /// inherited whatever the last frame that *did* run it recorded, and on
    /// the very first frames a `0.0000` that had never been measured. No dump
    /// row on any session had ever had an empty field. Restoring that line
    /// turns the `sim_tick` assertion below red while the `setup` one stays
    /// green, which is what makes this a test of the skip and not of the CSV
    /// writer.
    #[test]
    fn a_skipped_phase_writes_an_empty_csv_field_not_a_stale_value() {
        let path = std::env::temp_dir().join("lodestone-frame-profile-skip-control.csv");
        let _ = std::fs::remove_file(&path);
        {
            let now = Instant::now();
            let mut p = FrameProfiler::new(now, Some(path.as_path()));
            // Frame 1: everything marked, so `sim_tick` has a real value the
            // buggy version could inherit on frame 2. Non-round and distinct
            // per phase, so a row that transposed two columns could not pass.
            p.begin_frame(now);
            p.mark(FramePhase::Setup, now + Duration::from_micros(1_300));
            p.mark(FramePhase::SimTick, now + Duration::from_micros(9_700));
            // Frame 2: `Setup` only — every later phase is an early return.
            let f2 = now + Duration::from_millis(20);
            p.begin_frame(f2);
            p.mark(FramePhase::Setup, f2 + Duration::from_micros(2_100));
            p.begin_frame(f2 + Duration::from_millis(20));
        }
        let text = std::fs::read_to_string(&path).expect("dump file must exist");
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().expect("header row").split(',').collect();
        let setup_col = header.iter().position(|c| *c == "setup").expect("setup column");
        let tick_col = header.iter().position(|c| *c == "sim_tick").expect("sim_tick column");
        let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').collect()).collect();
        assert_eq!(rows.len(), 2, "two frames were finalised, so two rows: {text}");

        // Frame 1 measured both. `sim_tick` is 8.4 and not 9.7: `mark`
        // measures elapsed since the *previous* mark, so the second phase's
        // span starts where `Setup`'s ended — 9.7 - 1.3. Predicting 9.7 here
        // (the number the fixture literally names) is exactly the
        // reach-for-the-plausible-figure mistake, and it failed on first run.
        assert_eq!(rows[0][setup_col], "1.3000");
        assert_eq!(rows[0][tick_col], "8.4000");
        // Frame 2 measured only `setup`. The bug reported frame 1's 8.4 here.
        assert_eq!(rows[1][setup_col], "2.1000");
        assert_eq!(
            rows[1][tick_col], "",
            "a phase not reached this frame must write an empty field, never the last frame that \
             did reach it: {text}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Raw benchmark analysis needs the real presentation interval and the
    /// workload segment on the same row as the phase timings. The interval
    /// for a frame becomes known only when the next frame begins, while the
    /// label must remain the one captured when that frame started.
    #[test]
    fn dump_carries_frame_interval_and_benchmark_segment() {
        let path = std::env::temp_dir().join("lodestone-frame-profile-segment-control.csv");
        let _ = std::fs::remove_file(&path);
        {
            let t0 = Instant::now();
            let mut profiler = FrameProfiler::new(t0, Some(&path));
            profiler.set_segment(Some("terrain.stationary"));
            profiler.begin_frame(t0);
            profiler.mark(FramePhase::Setup, t0 + Duration::from_millis(2));

            profiler.set_segment(Some("terrain.moving"));
            profiler.begin_frame(t0 + Duration::from_millis(17));
            profiler.mark(FramePhase::Setup, t0 + Duration::from_millis(19));
            profiler.begin_frame(t0 + Duration::from_millis(34));
        }

        let csv = std::fs::read_to_string(&path).expect("dump file must exist");
        let mut lines = csv.lines();
        assert!(
            lines
                .next()
                .expect("header row")
                .starts_with("frame,frame_interval_ms,segment,")
        );
        assert!(
            lines
                .next()
                .expect("stationary row")
                .starts_with("1,17.0000,terrain.stationary,")
        );
        assert!(
            lines
                .next()
                .expect("moving row")
                .starts_with("2,17.0000,terrain.moving,")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dump_pairs_world_and_hud_timings_with_per_frame_workload_counts() {
        let path = std::env::temp_dir().join("lodestone-frame-profile-count-control.csv");
        let _ = std::fs::remove_file(&path);
        let _ = crate::gpu::gpu_timing::take_world_subphases();
        {
            let t0 = Instant::now();
            let mut profiler = FrameProfiler::new(t0, Some(&path));
            profiler.set_segment(Some("megaworld.stationary"));
            profiler.begin_frame(t0);
            crate::gpu::gpu_timing::record_world_subphase(
                crate::gpu::gpu_timing::WorldSubphase::TerrainCullAndDraw,
                1.25,
            );
            crate::gpu::gpu_timing::record_world_subphase_counts(
                crate::gpu::gpu_timing::WorldSubphaseCounts {
                    packed_sections_visited: 137,
                    model_sections_visited: 911,
                    opaque_sections_drawn: 23,
                    water_sections_drawn: 29,
                    translucent_sections_drawn: 31,
                    entities_drawn: 37,
                    block_entities_drawn: 41,
                    sign_text_vertices: 43,
                    particles_drawn: 47,
                },
            );
            profiler.mark(FramePhase::WorldEncodeSubmit, t0 + Duration::from_millis(2));
            profiler.mark_hud(HudSubphase::DebugGather, t0 + Duration::from_millis(3));
            profiler.record_hud_counts(HudSubphaseCounts {
                chat_lines: 17,
                debug_lines: 29,
                menu_overlays_drawn: 3,
            });
            profiler.mark(FramePhase::HudUiEncodeSubmit, t0 + Duration::from_millis(4));
            profiler.begin_frame(t0 + Duration::from_millis(10));
        }

        let csv = std::fs::read_to_string(&path).expect("dump file must exist");
        let mut lines = csv.lines();
        let header: Vec<&str> = lines.next().expect("header").split(',').collect();
        let row: Vec<&str> = lines.next().expect("row").split(',').collect();
        for (name, expected) in [
            ("world.packed_sections_visited", "137"),
            ("world.model_sections_visited", "911"),
            ("world.opaque_sections_drawn", "23"),
            ("world.water_sections_drawn", "29"),
            ("world.translucent_sections_drawn", "31"),
            ("world.entities_drawn", "37"),
            ("world.block_entities_drawn", "41"),
            ("world.sign_text_vertices", "43"),
            ("world.particles_drawn", "47"),
            ("hud.chat_lines", "17"),
            ("hud.debug_lines", "29"),
            ("hud.menu_overlays_drawn", "3"),
        ] {
            let column = header.iter().position(|column| *column == name).unwrap();
            assert_eq!(row[column], expected, "wrong {name} in {csv}");
        }
        let _ = std::fs::remove_file(path);
    }

    /// The end-to-end magnitude control for the `world_encode_submit`
    /// sub-phase bridge: record a *real*, non-round sleep through
    /// `gpu::gpu_timing::record_world_subphase` (exactly as `gpu/frame.rs`
    /// does), mark `WorldEncodeSubmit` (exactly as `app/redraw.rs` does, with
    /// no changes needed there), and require the resulting F3/tracing detail
    /// string to name the right sub-phase and land on the slept duration —
    /// not merely be present.
    ///
    /// **Watched failing**: commenting out `drain_world_subphases`'s call
    /// site inside `mark` (so the bridge is never drained) makes
    /// `world_subphase_detail` return `None` and this test's
    /// `.expect("...")` panic immediately — confirmed by hand before this
    /// landed, then restored. A version of this test that only asserted
    /// `detail.is_some()` would have passed against a bridge that drained
    /// garbage just as easily as against a real measurement, which is the
    /// "merely positive" failure mode this repo's evidence standard warns
    /// against.
    #[test]
    fn world_encode_submit_detail_reports_the_real_sub_phase_time() {
        // Defensive drain: this thread may have run an earlier test that left
        // the (thread-local) bridge non-empty.
        let _ = crate::gpu::gpu_timing::take_world_subphases();

        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(27));
        let elapsed_ms = t0.elapsed().as_secs_f32() * 1000.0;
        crate::gpu::gpu_timing::record_world_subphase(
            crate::gpu::gpu_timing::WorldSubphase::TerrainCullAndDraw,
            elapsed_ms,
        );
        crate::gpu::gpu_timing::record_world_subphase_counts(
            crate::gpu::gpu_timing::WorldSubphaseCounts {
                packed_sections_visited: 613,
                model_sections_visited: 208,
                opaque_sections_drawn: 3,
                water_sections_drawn: 5,
                translucent_sections_drawn: 7,
                entities_drawn: 11,
                block_entities_drawn: 13,
                sign_text_vertices: 17,
                particles_drawn: 19,
            },
        );

        let now = Instant::now();
        let mut p = FrameProfiler::new(now, None);
        p.begin_frame(now);
        p.mark(FramePhase::WorldEncodeSubmit, Instant::now());

        let world = p
            .summary()
            .find(|s| s.phase == FramePhase::WorldEncodeSubmit)
            .unwrap();
        let detail = world.detail.expect(
            "WorldEncodeSubmit must carry a sub-phase detail once the bridge has a real reading",
        );
        assert!(
            detail.contains("world.terrain_cull_draw"),
            "detail must name the sub-phase that was actually recorded: {detail}"
        );
        assert!(
            detail.contains("613 packed + 208 model"),
            "detail must carry the exact counts recorded, pairwise-distinct so a \
             transposition cannot survive: {detail}"
        );
        // Pull `world.terrain_cull_draw`'s mean back out of the formatted
        // string and check it against the real slept duration — the
        // magnitude check itself, not just "some text appeared".
        let mean_str = detail
            .split("world.terrain_cull_draw: ")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .expect("detail must contain a mean figure for the recorded sub-phase");
        let mean: f32 = mean_str.parse().expect("mean figure must be a valid float");
        assert!(
            (18.0..=80.0).contains(&mean),
            "expected ~27ms (wide tolerance for scheduler jitter), got {mean} from {detail}"
        );
    }
}
