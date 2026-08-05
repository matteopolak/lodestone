//! Frame pacing: the tick-catch-up clamp and the unfocused/occluded schedule.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

// ---------------------------------------------------------------------------
// Frame pacing
// ---------------------------------------------------------------------------

/// Vanilla's cap on how many 20 Hz client ticks a single update may run.
///
/// Read from the decompiled 26.2 client, not guessed:
/// `.cache/mc/26.2/client-src/net/minecraft/client/Minecraft.java:262` declares
/// `private static final int MAX_TICKS_PER_UPDATE = 10;` and `:1176` applies it
/// as `for (int i = 0; i < Math.min(10, ticksToDo); i++)`. Note *where* the cap
/// lives: `DeltaTracker.Timer::advanceGameTime` returns the full uncapped tick
/// count and keeps the sub-tick residual, and `runTick` then simply **runs at
/// most ten of them and drops the rest**. Missed real time is discarded, never
/// replayed — which is the whole point.
///
/// **Aliased, not re-derived**, since §4.1(c): the number the simulation actually
/// clamps to lives beside the one accumulator, and this file's copy of it was how
/// the shell came to run five catch-up ticks while claiming ten.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MAX_TICKS_PER_UPDATE: u32 = lodestone_ecs::MAX_CATCH_UP_TICKS;

/// Length of one client tick in seconds (20 Hz).
///
/// An alias, like [`MAX_TICKS_PER_UPDATE`]: the accumulator that counts in this
/// period lives in `lodestone-ecs`, and a local copy is how the two clocks §4.1(c)
/// unified came to disagree in the first place. Only this file's tests and doc
/// links read it, hence the `dead_code` allowance in non-test builds.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const TICK_SECS: f64 = lodestone_ecs::TICK_PERIOD;

/// The most real time one update may hand the simulation, in seconds.
///
/// `10 × 0.05 = 0.5`. Anything beyond this is dropped rather than replayed, so
/// alt-tabbing away for a minute costs ten ticks of catch-up, not 1200. The pacer
/// clamps here and `FrameClock::begin_frame` clamps to the same constant, so the
/// two agree by construction rather than by coincidence.
pub(crate) const MAX_CATCHUP_SECS: f64 = lodestone_ecs::MAX_CATCH_UP_SECS;

/// Presentation rate while the window is visible but **unfocused**. The
/// simulation keeps running at the full 20 Hz either way; only presentation is
/// throttled.
pub(crate) const UNFOCUSED_FPS: u32 = 30;

/// [`UNFOCUSED_FPS`] as the interval between presented frames.
pub(crate) const UNFOCUSED_FRAME_INTERVAL: Duration =
    Duration::from_nanos(1_000_000_000 / UNFOCUSED_FPS as u64);

/// How long the event loop sleeps between iterations while unfocused. Kept
/// comfortably shorter than [`TICK_SECS`] so the tick loop is never the thing
/// being paced — if this ever exceeded 50 ms the sim would fall behind the
/// server even though we are "still ticking".
pub(crate) const BACKGROUND_POLL: Duration = Duration::from_millis(8);

/// What one iteration of the event loop should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameStep {
    /// Real seconds to advance the simulation by, already clamped to
    /// [`MAX_CATCHUP_SECS`].
    pub dt: f64,
    /// Whether to acquire a swapchain image and draw. When `false` the sim still
    /// steps — we skip *presenting*, never ticking.
    pub render: bool,
}

/// Owns the frame clock and decides, per iteration, how far to advance the sim
/// and whether to draw.
///
/// ## Why this lives here and not in `sim`
///
/// `Sim::step` already clamps its own accumulator, but the *policy* — how much
/// catch-up is acceptable, and whether an unfocused window should present —
/// belongs to the driver, alongside the winit focus/occlusion events that inform
/// it. Keeping the clock here also means the sim is advanced by an explicit,
/// injectable `dt`, so this is testable against a real `Sim` with a synthetic
/// clock and no window.
///
/// ## The bug this exists to fix
///
/// Presentation used to gate simulation: `redraw` stepped the sim and then
/// acquired a swapchain image in the same call, with the GPU-readiness guard
/// *before* the step. A backgrounded or occluded window makes `acquire()` slow
/// (macOS stops vending drawables to an occluded `CAMetalLayer` and the call
/// stalls until it times out), so the loop's iteration rate collapsed — and with
/// it the tick rate, since ticks only advanced when a frame did. Skipping
/// presentation instead of skipping the tick is what keeps keep-alives and
/// movement packets flowing while tabbed out; a client the server considers
/// stalled stops receiving chunks entirely.
///
/// ## Why the unfocused frame schedule is absolute, not "elapsed since the last
/// frame"
///
/// This was measured, not reasoned about. The obvious gate —
/// `now - last_render >= interval`, then `last_render = now` — **loses frames**,
/// because it can only fire on a loop iteration and each iteration pushes the
/// next deadline out by however far it overshot. At a 120 Hz loop with a 30 fps
/// target there are only four chances per interval, and the accumulated
/// overshoot cost 4 of every 30 frames: a one-second unfocused run presented
/// **26** frames, not 30. A 30 fps limiter that silently delivers 26 is the
/// quiet kind of wrong.
///
/// So the deadline is absolute: `next_render` advances by exactly one interval
/// from *itself*, never from `now`, and phase error cannot accumulate. The one
/// exception is a stall longer than an interval, where the schedule is re-based
/// onto `now` — otherwise coming back from a two-minute alt-tab would present a
/// burst of catch-up frames, which is the same mistake as replaying catch-up
/// ticks.
#[derive(Debug)]
pub(crate) struct FramePacer {
    last_step: Instant,
    /// The absolute time the next unfocused frame is due. Advanced by whole
    /// intervals so the presented rate does not drift below the target.
    next_render: Instant,
    focused: bool,
    occluded: bool,
}

impl FramePacer {
    /// A pacer whose clock starts at `now`, focused and visible.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_step: now,
            next_render: now + UNFOCUSED_FRAME_INTERVAL,
            focused: true,
            occluded: false,
        }
    }

    /// Record a focus change. Does **not** touch the step clock: the elapsed
    /// time since the last step is real time the sim still owes, and it is
    /// clamped on the next `begin_frame` like any other stall.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Record an occlusion change (window fully covered / minimised).
    pub(crate) fn set_occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
    }

    /// Whether the window currently has focus. Test-only: the app never asks,
    /// because focus must not gate anything except presentation — which
    /// [`Self::begin_frame`] already decides.
    #[cfg(test)]
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }

    /// Advance the frame clock to `now` and decide what this iteration does.
    ///
    /// The returned `dt` is the real elapsed time **clamped** to
    /// [`MAX_CATCHUP_SECS`]; the excess is dropped, exactly as vanilla drops
    /// ticks past `MAX_TICKS_PER_UPDATE`.
    pub(crate) fn begin_frame(&mut self, now: Instant) -> FrameStep {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64();
        self.last_step = now;

        let render = if self.occluded {
            // Nothing is on screen to update, and acquiring a drawable is what
            // stalls. Drop presentation entirely and keep ticking.
            false
        } else if self.focused {
            // Vsync (or the compositor) paces us; do not second-guess it.
            self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            true
        } else {
            now >= self.next_render
        };
        if render && !self.focused {
            // Advance the deadline from itself, not from `now`, so overshoot
            // does not accumulate into a lower delivered frame rate. Re-base
            // only when we are more than a whole interval late, which means a
            // real stall rather than ordinary jitter — replaying the backlog as
            // a burst of frames would be the presentation-side version of the
            // catch-up-tick bug.
            self.next_render += UNFOCUSED_FRAME_INTERVAL;
            if self.next_render <= now {
                self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            }
        }

        FrameStep {
            dt: dt.min(MAX_CATCHUP_SECS),
            render,
        }
    }

    /// How the event loop should wait after this iteration: spin while focused
    /// (vsync paces us), otherwise sleep briefly so a backgrounded window stops
    /// burning a core while still ticking well above 20 Hz.
    pub(crate) fn control_flow(&self, now: Instant) -> ControlFlow {
        if self.focused && !self.occluded {
            ControlFlow::Poll
        } else {
            ControlFlow::WaitUntil(now + BACKGROUND_POLL)
        }
    }
}
