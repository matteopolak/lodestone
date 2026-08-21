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
//!   (`HudRenderer::render_with_item_models`, `EffectsRenderer::render`,
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
    /// Wall-clock instant the *previous* mark (or `begin_frame`) was taken —
    /// what the next `mark` measures elapsed time against.
    cursor: Instant,
    /// Total frames [`Self::begin_frame`] has been called for — the CSV
    /// dump's row index.
    frame_count: u64,
    dump: Option<super::frame_profile_dump::DumpWriter>,
    /// Last time [`Self::report_due`] fired — a clock this struct owns
    /// rather than reusing `WindowApp::last_log`'s, so the periodic
    /// `tracing` line has its own cadence instead of silently inheriting
    /// whatever `last_log`'s stdout print happens to use; see that field's
    /// doc in `app.rs`.
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
            cursor: now,
            frame_count: 0,
            dump: dump_path.map(super::frame_profile_dump::DumpWriter::open),
            last_report: now,
            world_subphase_windows: [const { PhaseWindow::new() };
                crate::gpu::gpu_timing::WORLD_SUBPHASE_COUNT],
            world_subphase_counts: None,
            world_subphase_bridge_misses: 0,
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
        self.finalise();
        self.cursor = now;
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
        }
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
        // The very first `begin_frame` of a session has no previous frame to
        // finalise — `frame_count == 0` and nothing has ever been marked.
        // Without this guard every phase's `skipped` counter would take a
        // phantom +1 before the first real frame ever ran, which is exactly
        // the "fabricated" reading this module's own doc promises not to
        // produce.
        if self.frame_count == 0 && self.pending.iter().all(Option::is_none) {
            return;
        }
        self.frame_count += 1;
        let mut world_encode_ran_this_frame = false;
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
        if let Some(dump) = &mut self.dump {
            let row: [Option<f32>; PHASE_COUNT] = std::array::from_fn(|i| {
                // Read back what was just pushed/skipped rather than the
                // (already-cleared) `pending`, so the dumped row and the ring
                // buffers can never disagree about what happened this frame.
                self.windows[i].samples[(self.windows[i].next + WINDOW - 1) % WINDOW]
                    .into()
            });
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
            dump.write_row(self.frame_count, row, world_row);
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
                detail: (phase == FramePhase::WorldEncodeSubmit)
                    .then(|| self.world_subphase_detail())
                    .flatten(),
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
    /// `EffectsRenderer::render`, the container/menu draws) live in files
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
