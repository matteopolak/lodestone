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
/// Vanilla's `FramerateLimitTracker` AFK thresholds
/// (`FramerateLimitTracker.java`'s `AFK_THRESHOLD_MS`/`LONG_AFK_THRESHOLD_MS`),
/// in seconds — how long since the last real key/mouse input before the
/// framerate cap tightens. Only consulted when `inactivityFpsLimit == Afk`;
/// see [`effective_target_fps`].
pub(crate) const AFK_THRESHOLD_SECS: f64 = 60.0;
/// See [`AFK_THRESHOLD_SECS`].
pub(crate) const LONG_AFK_THRESHOLD_SECS: f64 = 600.0;
/// Vanilla's `FramerateLimitTracker.SHORT_AFK_LIMIT`/... — actually named
/// `AFK_LIMIT`/`LONG_AFK_LIMIT` in `FramerateLimitTracker.java:11-12`.
pub(crate) const AFK_FPS: u32 = 30;
/// See [`AFK_FPS`].
pub(crate) const LONG_AFK_FPS: u32 = 10;

/// Vanilla's `FramerateLimitTracker::getFramerateLimit`'s AFK half
/// (`FramerateLimitTracker.java:26-34`), minus the `WINDOW_ICONIFIED`/
/// `OUT_OF_LEVEL_MENU` branches: this pacer already throttles an
/// occluded/unfocused window unconditionally (the module doc's table
/// predates `framerateLimit`), so those two vanilla branches are subsumed by
/// a mechanism this client had before the option existed. What is new here
/// is the AFK clock, gated on `inactivityFpsLimit == Afk` exactly as vanilla
/// gates its own (`Minimized` never reduces for idle input — only for window
/// state, which the pacer already covers).
///
/// `raw_limit` is the persisted `framerate_limit` (`10..=260`, `260` = the
/// `UNLIMITED_FRAMERATE_CUTOFF` sentinel `Options.java` never applies the
/// limiter past — `Minecraft.java:1331`'s `if (framerateLimit < 260)`).
/// Returns `None` for "no cap at all", so a focused window with the row left
/// at Unlimited and not AFK still lets vsync/the compositor pace it exactly
/// as before this option existed.
///
/// **Composes with vsync rather than being gated by it** — see
/// [`crate::config::Options::enable_vsync`]'s doc for the citation that this
/// is vanilla's own behaviour, not a choice made here.
#[must_use]
pub(crate) fn effective_target_fps(
    raw_limit: u32,
    inactivity: crate::config::InactivityFpsLimit,
    idle_secs: f64,
) -> Option<u32> {
    let capped = (raw_limit < crate::config::UNLIMITED_FRAMERATE_CUTOFF).then_some(raw_limit);
    if inactivity != crate::config::InactivityFpsLimit::Afk {
        return capped;
    }
    let afk = if idle_secs > LONG_AFK_THRESHOLD_SECS {
        Some(LONG_AFK_FPS)
    } else if idle_secs > AFK_THRESHOLD_SECS {
        Some(capped.unwrap_or(u32::MAX).min(AFK_FPS))
    } else {
        None
    };
    match (capped, afk) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The presented-frame interval for a given target rate. `UNFOCUSED_FRAME_INTERVAL`
/// is exactly `frame_interval(UNFOCUSED_FPS)`; kept as a named function rather
/// than only a constant because [`FramePacer::begin_frame`] now schedules
/// against a **live, option-derived** rate too (`framerateLimit`/
/// `inactivityFpsLimit`), not only the fixed unfocused one.
fn frame_interval(fps: u32) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)))
}

#[derive(Debug)]
pub(crate) struct FramePacer {
    last_step: Instant,
    /// The absolute time the next scheduled frame is due — "scheduled"
    /// meaning either the fixed unfocused rate or a live `target_fps` cap
    /// passed into [`Self::begin_frame`]. Advanced by whole intervals so the
    /// presented rate does not drift below the target.
    next_render: Instant,
    /// The last time [`Self::record_input`] saw a real key/mouse event —
    /// vanilla's `FramerateLimitTracker::onInputReceived`'s clock. Feeds
    /// [`effective_target_fps`]'s AFK half through [`Self::idle_secs`].
    last_input: Instant,
    focused: bool,
    occluded: bool,
}

impl FramePacer {
    /// A pacer whose clock starts at `now`, focused, visible and not idle.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_step: now,
            next_render: now + UNFOCUSED_FRAME_INTERVAL,
            last_input: now,
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

    /// Record real key/mouse input — vanilla's `onInputReceived`, called from
    /// `KeyboardHandler`/`MouseHandler`, never from raw pointer motion. Resets
    /// the AFK clock [`effective_target_fps`] reads through [`Self::idle_secs`].
    pub(crate) fn record_input(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// Real seconds since the last recorded input — [`effective_target_fps`]'s
    /// `idle_secs` argument.
    pub(crate) fn idle_secs(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.last_input).as_secs_f64()
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
    ///
    /// `target_fps` is the caller's [`effective_target_fps`] result for this
    /// instant — `None` for "no cap, let vsync/the compositor pace a focused
    /// window". A focused-and-uncapped window still renders every iteration,
    /// exactly as before this parameter existed; every other combination
    /// (unfocused, occluded, or a real cap while focused) is paced against an
    /// **absolute** schedule, never a busy-wait — see the module doc for why
    /// the schedule must be absolute rather than "elapsed since the last
    /// frame". [`Self::control_flow`] is what turns the capped-focused case
    /// into an actual sleep instead of a spin; this method only decides
    /// *whether* to render this iteration.
    pub(crate) fn begin_frame(&mut self, now: Instant, target_fps: Option<u32>) -> FrameStep {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64();
        self.last_step = now;

        // `None` here means "no schedule to keep" — only the focused,
        // uncapped case. Unfocused always has at least `UNFOCUSED_FPS`,
        // tightened further by an explicit lower cap; a focused, capped
        // window is paced at exactly the cap.
        let scheduled_fps = match (self.focused, target_fps) {
            (true, None) => None,
            (true, Some(fps)) => Some(fps),
            (false, None) => Some(UNFOCUSED_FPS),
            (false, Some(fps)) => Some(fps.min(UNFOCUSED_FPS)),
        };

        let render = if self.occluded {
            // Nothing is on screen to update, and acquiring a drawable is what
            // stalls. Drop presentation entirely and keep ticking.
            false
        } else if scheduled_fps.is_some() {
            now >= self.next_render
        } else {
            // Vsync (or the compositor) paces us; do not second-guess it. Keep
            // the schedule reset to "now" so a later transition into a
            // scheduled state (losing focus, or dialling in a cap) starts from
            // *now* rather than from a deadline set focused-uncapped frames
            // ago.
            self.next_render = now + UNFOCUSED_FRAME_INTERVAL;
            true
        };
        if let (true, Some(fps)) = (render, scheduled_fps) {
            // Advance the deadline from itself, not from `now`, so overshoot
            // does not accumulate into a lower delivered frame rate. Re-base
            // only when we are more than a whole interval late, which means a
            // real stall rather than ordinary jitter — replaying the backlog
            // as a burst of frames would be the presentation-side version of
            // the catch-up-tick bug.
            let interval = frame_interval(fps);
            self.next_render += interval;
            if self.next_render <= now {
                self.next_render = now + interval;
            }
        }

        FrameStep {
            dt: dt.min(MAX_CATCHUP_SECS),
            render,
        }
    }

    /// How the event loop should wait after this iteration: spin while
    /// focused and uncapped (vsync paces us), sleep until the next scheduled
    /// deadline while capped, otherwise sleep briefly so a backgrounded
    /// window stops burning a core while still ticking well above 20 Hz.
    ///
    /// **This is the fix for "not a busy-wait".** Without it, a focused
    /// window with `framerateLimit` set below the display's refresh rate
    /// would still report `ControlFlow::Poll`, so the event loop would spin
    /// at 100% of a core calling `begin_frame` every iteration only to find
    /// `render == false` most of the time — a cap implemented as a spin loop
    /// checking a clock, which is the busy-wait this method exists to avoid.
    pub(crate) fn control_flow(&self, now: Instant, target_fps: Option<u32>) -> ControlFlow {
        if self.occluded {
            ControlFlow::WaitUntil(now + BACKGROUND_POLL)
        } else if self.focused {
            match target_fps {
                None => ControlFlow::Poll,
                Some(_) => ControlFlow::WaitUntil(self.next_render),
            }
        } else {
            ControlFlow::WaitUntil(now + BACKGROUND_POLL)
        }
    }
}
