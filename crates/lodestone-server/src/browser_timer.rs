//! A browser-compatible periodic driver for `server::serve_play`'s wasm32 arm
//! (issue #636).
//!
//! # What it is
//!
//! Native `serve_play` races its socket read against four `tokio::time::
//! interval_at` timers inside one `tokio::select!` (`keep_alive_tick`,
//! `time_sync_tick`, `vitals_tick`, `container_sync_tick`) — see that
//! function's own doc comment. The wasm32 arm has never had an equivalent:
//! `tokio::time` is compiled in for this crate's wasm32 target (`Cargo.toml`
//! lists the `time` feature), but calling it does not merely fail to build —
//! it **hangs on its first poll**, silently, with no panic and no log line
//! (measured; see this repo's `CLAUDE.md`). So every timed system that used to
//! ride one of those four timers has simply never run in the browser, and
//! `IntegratedServer::open_in_memory`'s wasm32 loop degraded to a bare
//! `while let Some(packet) = conn.read_packet().await? { .. }` with no timer
//! arm at all.
//!
//! [`BrowserInterval`] is the replacement primitive: a `tokio::time::
//! interval_at`-alike built entirely on a real browser **macrotask**
//! (`window.setTimeout`), the same mechanism `crate::chunk::yield_to_browser`
//! already uses for the join/streaming yield points (`35f4800b`), generalised
//! from a fixed one-macrotask yield to a caller-supplied period. It is
//! deliberately *not* a microtask (`Promise::resolve().then(..)`) — a
//! microtask drains within the same JS task and never lets the browser paint
//! or service input, which would satisfy "there is an `.await` point" while
//! doing nothing to stop the tab from hanging, exactly the failure `35f4800b`
//! fixed for chunk generation.
//!
//! # How it works
//!
//! [`BrowserInterval::tick`] sleeps until its next deadline (via
//! [`browser_sleep`]) and then reschedules using [`next_deadline`] — **`Delay`
//! semantics, never `Burst`**. `tokio::time`'s default `MissedTickBehavior::
//! Burst` fires every missed tick back to back with zero delay between them,
//! which this repo has already paid for once: a native `keep_alive_tick`
//! stall spanning two intervals wrote a challenge and found it unanswered in
//! the same instant, because `Burst` gave the client no time to reply (see
//! `server::serve_play`'s `keep_alive_tick` doc comment). `next_deadline`
//! collapses any backlog into a single tick and restarts the period from
//! *now* instead.
//!
//! # How to change it
//!
//! [`next_deadline`] is deliberately a free function taking and returning
//! [`BrowserInstant`] rather than a private method on [`BrowserInterval`], so
//! its rebasing arithmetic can be unit-tested on every target — this file's
//! `js_sys`/`web_sys` calls cannot run under a native `cargo test` at all, so
//! without this split the one thing worth predicting a value for (does a
//! stall collapse instead of bursting) would be untestable outside a browser.
//! If a second wasm32-only caller needs a portable sleep, reuse
//! [`browser_sleep`] rather than a second `set_timeout` call site —
//! `wasm_bindgen::closure::Closure` construction is easy to get subtly wrong
//! (`crates/lodestone-shell/src/platform.rs`'s `relay::sleep` doc makes the
//! same point for its own, separate copy of this primitive; that one cannot
//! be reused from here because `lodestone-server` does not and must not
//! depend on `lodestone-shell`).
//!
//! # Configuration
//!
//! None — every call site supplies its own `Duration` (`server::serve_play`'s
//! wasm32 arm uses the same `VANILLA_TICK_MILLIS`-derived cadences the native
//! arm's constants already document).
//!
//! # Dependencies
//!
//! `web_time::Instant` (this crate's existing portable clock, already a
//! dependency — see `Cargo.toml`'s comment on why `web-time` and not
//! `std::time::Instant`, which traps on wasm32) for elapsed-time bookkeeping,
//! and `js-sys`/`web-sys`/`wasm-bindgen-futures` (wasm32-only dependencies,
//! same versions `crate::chunk::yield_to_browser` already pins) for the real
//! macrotask.

use std::time::Duration;

/// This module's portable "now". `web_time::Instant`'s non-wasm arm is
/// literally `pub use std::time::*` (see `Cargo.toml`), so on every other
/// target this is `std::time::Instant` unchanged — the alias exists so
/// [`next_deadline`] reads the same on both targets and so this file never
/// spells out `std::time::Instant` or `tokio::time::Instant` itself: the
/// former traps on wasm32 at runtime, and the latter is `tick.rs`'s one
/// documented allowance under `scripts/wasm-check.sh`'s `tokio-instant-ban`
/// rule, which this file is deliberately not a second exception to.
pub(crate) type BrowserInstant = web_time::Instant;

/// Resolves after `duration` via a real browser macrotask, never
/// `tokio::time::sleep` — see this module's doc for why that hangs rather
/// than merely failing to compile. Identical in shape to
/// `crate::chunk::yield_to_browser` and `lodestone_shell::platform::relay::
/// sleep` (neither of which this crate can reuse: the former is a fixed
/// zero-length yield with no `Duration` parameter, the latter lives in a
/// crate this one must not depend on), generalised to a caller-supplied
/// period.
#[cfg(target_arch = "wasm32")]
async fn browser_sleep(duration: Duration) {
    let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect(
            "no global `window`: this crate's wasm32 build only runs inside a browser tab",
        );
        // A missed `set_timeout` call (the only failure mode here — an
        // exhausted timer-id space or similar) leaves `resolve` uncalled,
        // so this future simply never completes rather than completing
        // early or panicking. Matches `lodestone_shell::platform::relay::
        // sleep`'s own reasoning for the identical shape.
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The next deadline for a periodic driver, using **`Delay`** missed-tick
/// semantics — see this module's doc comment for why `Burst` is actively
/// dangerous here. Pulled out of [`BrowserInterval::tick`] as a pure,
/// target-independent function purely so it has a native-testable surface at
/// all (see the module doc's "How to change it").
///
/// `previous_deadline + period` is used unless it has already passed by more
/// than one period, in which case the schedule rebases from `now` — so a
/// caller that fails to poll this for an arbitrarily long stretch gets
/// exactly one tick when it returns, never a burst of catch-up ticks.
pub(crate) fn next_deadline(
    previous_deadline: BrowserInstant,
    period: Duration,
    now: BrowserInstant,
) -> BrowserInstant {
    let candidate = previous_deadline + period;
    if candidate > now { candidate } else { now + period }
}

/// A `tokio::time::interval_at`-alike for wasm32, built entirely on
/// [`browser_sleep`]/[`next_deadline`]. `wasm32`-only: native `serve_play`
/// already has the real thing (`tokio::time::interval_at`) and has no reason
/// to route through a slower, `set_timeout`-backed stand-in.
#[cfg(target_arch = "wasm32")]
pub(crate) struct BrowserInterval {
    period: Duration,
    next: BrowserInstant,
}

#[cfg(target_arch = "wasm32")]
impl BrowserInterval {
    /// Anchored one full `period` out from construction, matching every
    /// native `interval_at` call in `server::serve_play` — the first tick
    /// must not fire in the same instant as whatever join-time work the
    /// caller already did.
    pub(crate) fn new(period: Duration) -> Self {
        Self {
            period,
            next: BrowserInstant::now() + period,
        }
    }

    /// Waits until this interval's next deadline, then reschedules per
    /// [`next_deadline`]. Cancel-safe: dropping this future (e.g. because a
    /// `tokio::select!` branch alongside it won the race) leaves `self.next`
    /// untouched, so a cancelled wait simply retries the same deadline next
    /// time this is polled — the same property `tokio::time::Interval::tick`
    /// itself has.
    pub(crate) async fn tick(&mut self) {
        let now = BrowserInstant::now();
        if self.next > now {
            browser_sleep(self.next - now).await;
        }
        self.next = next_deadline(self.next, self.period, BrowserInstant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::{BrowserInstant, next_deadline};
    use std::time::Duration;

    // `next_deadline` is pure and target-independent (`BrowserInstant` is
    // `std::time::Instant` on every target this test actually runs on — see
    // the type's own doc), so these run under a plain native `cargo test`
    // with no browser and no wasm target involved. That is deliberate: it is
    // the one piece of this module a native test can observe at all.

    #[test]
    fn on_schedule_advances_by_exactly_one_period() {
        let period = Duration::from_millis(50);
        let previous = BrowserInstant::now();
        // "Now" is still inside the current window — nothing was missed.
        let now = previous + Duration::from_millis(10);
        let next = next_deadline(previous, period, now);
        assert_eq!(next, previous + period);
    }

    #[test]
    fn a_stall_spanning_two_periods_collapses_to_one_tick_from_now() {
        // The scenario the module doc names: a caller that could not poll
        // this for 2.5 periods. `Burst` semantics would leave `next` at
        // `previous + period` (already in the past by 1.5 periods), so the
        // very next `tick()` call would resolve immediately a second time —
        // the "found it unanswered in the same instant" bug. `Delay`
        // semantics must instead rebase a full period out from `now`.
        let period = Duration::from_millis(50);
        let previous = BrowserInstant::now();
        let now = previous + Duration::from_millis(125);
        let next = next_deadline(previous, period, now);
        assert_eq!(next, now + period);
        // The discriminating assertion: the wrong (`Burst`-shaped) hypothesis
        // computes `previous + period`, which is `now - 75ms` here — already
        // elapsed. Assert the two hypotheses disagree at this input, per this
        // repo's own evidence standard ("do not predict the plausible round
        // number" / "an input where both hypotheses coincide is not a test").
        let burst_hypothesis = previous + period;
        assert!(
            burst_hypothesis < now,
            "burst_hypothesis ({burst_hypothesis:?}) must already be in the \
             past at `now` ({now:?}) for this input to discriminate at all"
        );
        assert_ne!(next, burst_hypothesis);
    }

    #[test]
    fn exactly_on_the_deadline_still_advances_one_full_period() {
        // The boundary case: `candidate == now` must take the `now + period`
        // branch (a `>` comparison, not `>=`), otherwise a caller that polls
        // at the exact instant a deadline elapses reschedules zero
        // milliseconds out and spins.
        let period = Duration::from_millis(50);
        let previous = BrowserInstant::now();
        let now = previous + period;
        let next = next_deadline(previous, period, now);
        assert_eq!(next, now + period);
    }
}
