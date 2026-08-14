//! How the four schedules get driven — the seam named in `docs/bevy-migration.md`
//! Stage 0: "winit-driven in the shell, timer-driven headless".

use bevy_app::App;

use crate::schedules::{Extract, GameTick};

/// One thread, fixed order, per §4.1(b) — `lodestone-physics` is bit-exact
/// against a JVM oracle with golden traces, so which thread an input lands on
/// must never be a scheduling artefact. Both arms therefore run `GameTick`,
/// `Update` and `Extract` on the caller's own thread; only *when* that
/// happens differs.
#[derive(Debug)]
pub enum Runner {
    /// The host event loop (winit) drives ticks itself, once per
    /// `RedrawRequested`, by calling `App::update()` directly — see
    /// `lodestone-shell`'s `WindowApp::redraw`. No internal timer: §2.5 is
    /// explicit that packet ingest must never be gated on frame rate, so the
    /// winit arm intentionally has no loop of its own for this variant to
    /// own. It exists as a value so callers can name "which arm are we" (for
    /// logging, tests, `cfg` decisions) without a `bool`.
    Winit,
    /// A hand-rolled fixed-tick loop for embeddings with no window: bots,
    /// tests, the eventual headless integrated-server arm. Modeled on
    /// azalea's `run_schedule_loop` (`azalea-client/src/client.rs:163-223`):
    /// `GameTick` at `tick_hz`, catch-up capped at `max_catch_up_ticks`
    /// (matching `docs/frame-pacing.md`'s ten-tick rule), `Update`/`Extract`
    /// once per iteration via [`Runner::run_headless`].
    ///
    /// Native-only: the timer it needs (`std::time::Instant`) panics on
    /// wasm32 with no runtime — the same hazard
    /// `lodestone-client::native_time` exists to confine — so this variant
    /// does not exist on that target rather than existing and being
    /// impossible to construct correctly.
    #[cfg(not(target_arch = "wasm32"))]
    Headless {
        /// `GameTick` rate, e.g. `20.0` for vanilla's 20 Hz.
        tick_hz: f64,
        /// Vanilla's `MAX_TICKS_PER_UPDATE` (10): how many catch-up ticks a
        /// single iteration may run before the remaining backlog is dropped
        /// rather than replayed in a burst.
        max_catch_up_ticks: u32,
    },
}

#[cfg(not(target_arch = "wasm32"))]
impl Runner {
    /// Runs `app` until `should_stop` returns `true`. Only meaningful for
    /// [`Runner::Headless`]; the winit arm has no loop of its own for this
    /// method to run (see its doc comment), so calling this on `Runner::Winit`
    /// is a programmer error and panics rather than silently doing nothing.
    ///
    /// Each iteration: run `GameTick` zero or more times (fixed-step
    /// accumulator, capped at `max_catch_up_ticks`), then `app.update()`
    /// (bevy's own `First`/`PreUpdate`/`Update`/`PostUpdate`/`Last` chain,
    /// which is where `FrameSet` lives), then `Extract`. A stalled loop
    /// sleeps the remainder of a tick period rather than busy-polling.
    pub fn run_headless(&self, app: &mut App, mut should_stop: impl FnMut() -> bool) {
        let Runner::Headless {
            tick_hz,
            max_catch_up_ticks,
        } = self
        else {
            panic!(
                "Runner::run_headless called on Runner::Winit, which has no loop of its own \
                 — the host event loop drives it by calling App::update() per RedrawRequested"
            );
        };
        let tick_duration = std::time::Duration::from_secs_f64(1.0 / tick_hz.max(1.0));
        let mut last = lodestone_time::Instant::now();
        let mut accumulator = std::time::Duration::ZERO;

        while !should_stop() {
            let now = lodestone_time::Instant::now();
            accumulator += now.duration_since(last);
            last = now;

            let mut ticks_run = 0u32;
            while accumulator >= tick_duration && ticks_run < *max_catch_up_ticks {
                app.world_mut().run_schedule(GameTick);
                accumulator -= tick_duration;
                ticks_run += 1;
            }
            if ticks_run == *max_catch_up_ticks {
                // A stall longer than the whole catch-up budget: drop the
                // rest rather than replay it in a burst (vanilla's own rule,
                // docs/frame-pacing.md).
                accumulator = std::time::Duration::ZERO;
            }

            app.update();
            app.world_mut().run_schedule(Extract);

            if accumulator < tick_duration {
                std::thread::sleep(tick_duration - accumulator);
            }
        }
    }
}
