//! The unified server tick clock (issue #284/#285): a single 20 Hz loop that
//! ticks world state independently of any client connection, plus MSPT/TPS
//! accounting and vanilla-shaped overrun handling.
//!
//! # Before this module
//!
//! Nothing in this crate had one clock. Six independent timers already
//! existed, each a local `tokio::time::interval`/`Duration` literal, each
//! reinventing "one vanilla tick is 50ms" on its own:
//!
//! | timer | file:line (pre-#284) | cadence |
//! |---|---|---|
//! | `MOB_TICK_INTERVAL` | `mobs.rs` (`run_mob_tick_loop`) | 50ms |
//! | `BLOCK_ENTITY_TICK_INTERVAL` | `block_entities.rs` (`run_block_entity_tick_loop`) | 50ms |
//! | `KEEP_ALIVE_INTERVAL` | `server.rs` | 15,000ms |
//! | `TIME_SYNC_INTERVAL` | `server.rs` | 1,000ms |
//! | `VITALS_TICK_INTERVAL` | `server.rs` | 50ms |
//! | `CONTAINER_SYNC_INTERVAL` | `server.rs` | 50ms |
//!
//! Only the first two were world-simulation clocks with no client attached —
//! `run_mob_tick_loop`/`run_block_entity_tick_loop`, spawned once per
//! [`crate::IntegratedServer::open_in_memory_with_mobs`] call and unified
//! behind one shutdown-race helper in `integrated.rs` (`a6cc60a`). The other
//! four are legitimately *per-connection*: keep-alive is a health check for
//! one socket, time-sync/vitals/container-sync all read or write one
//! player's own state. They stay per-connection here — merging a
//! network-facing, per-client cadence into a world clock would not close any
//! island, it would just rename one. What *was* missing is the one thing
//! `server.rs` already had a private `MILLIS_PER_TICK` for and `mobs.rs`/
//! `block_entities.rs` each redefined locally: a single, shared 20 Hz clock
//! for the world itself, instrumented well enough to answer "is the server
//! keeping up" — which is exactly [`TickClock`] plus [`run_tick_loop`] below.
//!
//! # What this module does *not* unify, and why that is not a contradiction
//!
//! [`MILLIS_PER_TICK`] here is `pub(crate)`, not a replacement for
//! `server.rs`'s own private constant of the same value — see that module's
//! own `MILLIS_PER_TICK` doc for why a `serve_play` health check and this
//! module's world clock are allowed to keep independent literals that happen
//! to agree, the same reasoning `mobs.rs`'s `MOB_TICK_INTERVAL` doc already
//! gave for not sharing with `VITALS_TICK_INTERVAL`. This module's contribution
//! is: exactly **one** world-tick loop exists now (not two), and it is
//! instrumented, not that every literal `50` in the crate now points at one
//! constant.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::block_entities::{BlockEntityHandle, BlockEntityRegistry};
use crate::mobs::{LiveMobSource, MobHandle, MobSim};

/// Vanilla's tick period at normal (non-sprinting) speed — 20 TPS, matching
/// `server.rs`'s own private `MILLIS_PER_TICK` and every one of the six
/// timers this module's doc comment tables. `pub(crate)` because
/// [`run_tick_loop`]'s test module needs it; nothing outside this crate has a
/// reason to name it (a caller wants [`TickStats::tps`], not the literal).
pub(crate) const MILLIS_PER_TICK: u64 = 50;
pub(crate) const TICK_PERIOD: Duration = Duration::from_millis(MILLIS_PER_TICK);

/// Rolling-average window for [`TickStats::mspt_avg_ms`] — matches vanilla's
/// own `tickTimesNanos` ring buffer size
/// (`MinecraftServer.java:248`, `private final long[] tickTimesNanos = new long[100];`).
const HISTORY_LEN: usize = 100;

/// How far behind wall-clock schedule the loop must fall before it gives up
/// trying to catch up and forgives the backlog, matching vanilla's
/// `runServer` overload check
/// (`MinecraftServer.java:734-736`):
/// ```text
/// long behindTimeNanos = Util.getNanos() - this.nextTickTimeNanos;
/// if (behindTimeNanos > OVERLOADED_THRESHOLD_NANOS + 20L * thisTickNanos && ...)
/// ```
/// `OVERLOADED_THRESHOLD_NANOS` (`MinecraftServer.java:197`) is
/// `20L * NANOSECONDS_PER_SECOND / 20L`, i.e. exactly one second; the `20L *
/// thisTickNanos` term is 20 ticks' worth of the tick period (1s at 50ms/tick
/// here, since this crate has no `TickRateManager`/sprinting to vary
/// `thisTickNanos` with). Total: **2 seconds** behind before vanilla — and
/// this loop — gives up on the backlog.
fn overload_threshold() -> Duration {
    Duration::from_secs(1) + TICK_PERIOD * 20
}

/// Throttles how often the overload warning re-fires once triggered, matching
/// `MinecraftServer.java:736`'s
/// `this.nextTickTimeNanos - this.lastOverloadWarningNanos >=
/// OVERLOADED_WARNING_INTERVAL_NANOS + 100L * thisTickNanos`.
/// `OVERLOADED_WARNING_INTERVAL_NANOS` (`MinecraftServer.java:199`) is 10
/// seconds; `100L * thisTickNanos` is 100 ticks (5s here). Total: **15
/// seconds** between warnings while the server stays behind.
fn overload_warning_interval() -> Duration {
    Duration::from_secs(10) + TICK_PERIOD * 100
}

/// One forgiven-backlog event, as computed by [`resolve_overload`] — enough
/// for the caller to both log vanilla's own warning text and record it on the
/// [`TickClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OverloadEvent {
    ticks_behind: u64,
    behind_ms: u64,
}

/// The pure overload-detection step behind [`run_tick_loop`]'s "do not try to
/// catch up indefinitely" behavior (issue #285), extracted specifically so it
/// can be tested with hand-built [`tokio::time::Instant`]s rather than through
/// tokio's virtual clock — see this function's own test module for why
/// `tokio::time::advance` cannot exercise this branch **at all**: it fires
/// every timer between the current and target instant *in order*, so it can
/// only ever simulate a healthy schedule (nothing was ever actually behind
/// when the task was polled), never "wall time raced far ahead while the
/// task was blocked on synchronous work," which is the only way this branch
/// is reached in production.
///
/// Takes `now` (the wall-clock instant this check runs at), `next_tick_at`
/// (the previously scheduled next-tick instant), and `last_overload_warning_at`
/// (`None` until the first warning ever fires — see below for why that,
/// not "equal to `next_tick_at`", is the correct initial value).
///
/// Returns the (possibly unchanged) `next_tick_at` / `last_overload_warning_at`
/// pair, and `Some(OverloadEvent)` iff a backlog was just forgiven this call.
///
/// # Why `Option<Instant>`, not `Instant`, for `last_overload_warning_at`
///
/// Vanilla's own field (`lastOverloadWarningNanos`, `MinecraftServer.java:254`)
/// is a bare `long`, Java-default-initialized to `0` — nanoseconds since an
/// arbitrary epoch, not "now." So on a real server's very first overload, the
/// gap `nextTickTimeNanos - 0` is enormous and the warning-interval check at
/// line 736 is trivially satisfied: the first overload always warns. An
/// earlier version of this function instead seeded `last_overload_warning_at`
/// to the loop's own start instant, making that gap exactly zero on the very
/// first check — which would have silently swallowed the first overload
/// event forever (a false negative no test happened to hit, but the boundary
/// tests below now pin the correct behaviour: see
/// `resolve_overload_fires_on_the_very_first_overload_with_no_prior_warning`).
fn resolve_overload(
    now: tokio::time::Instant,
    next_tick_at: tokio::time::Instant,
    last_overload_warning_at: Option<tokio::time::Instant>,
) -> (
    tokio::time::Instant,
    Option<tokio::time::Instant>,
    Option<OverloadEvent>,
) {
    let behind = now.duration_since(next_tick_at);
    let warning_window_elapsed = last_overload_warning_at
        .is_none_or(|last| next_tick_at.duration_since(last) >= overload_warning_interval());
    if behind > overload_threshold() && warning_window_elapsed {
        let ticks_behind = behind.as_millis() as u64 / MILLIS_PER_TICK;
        let adjusted = next_tick_at + TICK_PERIOD * u32::try_from(ticks_behind).unwrap_or(u32::MAX);
        (
            adjusted,
            Some(adjusted),
            Some(OverloadEvent {
                ticks_behind,
                behind_ms: behind.as_millis() as u64,
            }),
        )
    } else {
        (next_tick_at, last_overload_warning_at, None)
    }
}

/// MSPT/TPS/overrun accounting for one [`run_tick_loop`] (issue #285).
///
/// Every field is an [`AtomicU64`] (plus a [`Mutex`]-guarded ring buffer for
/// the rolling average) rather than anything requiring `&mut` — the loop
/// writes every tick, and a caller (a test, or eventually a debug HUD/command)
/// reads concurrently through a shared `Arc<TickClock>` with no `.await` and
/// no lock contention on the hot path beyond the ring buffer push.
#[derive(Debug)]
pub struct TickClock {
    tick_count: AtomicU64,
    last_mspt_micros: AtomicU64,
    overrun_count: AtomicU64,
    history: Mutex<VecDeque<u64>>,
}

impl Default for TickClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TickClock {
    /// A fresh clock: zero ticks, zero overruns, empty history.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tick_count: AtomicU64::new(0),
            last_mspt_micros: AtomicU64::new(0),
            overrun_count: AtomicU64::new(0),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
        }
    }

    /// Records one completed world tick's wall-clock duration. Called exactly
    /// once per iteration of [`run_tick_loop`]'s body — never once per
    /// *skipped* tick, which is the whole point of the overrun handling this
    /// module implements: a tick that never ran (forgiven backlog) is never
    /// recorded here, so `tick_count` after `run_tick_loop` has been driven
    /// for `N` real ticks is exactly `N`, even across an overrun.
    pub(crate) fn record_tick(&self, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.tick_count.fetch_add(1, Ordering::Relaxed);
        self.last_mspt_micros.store(micros, Ordering::Relaxed);
        let mut history = self.history.lock().expect("tick history lock poisoned");
        if history.len() == HISTORY_LEN {
            history.pop_front();
        }
        history.push_back(micros);
    }

    /// Records one overload event — the loop fell more than
    /// [`overload_threshold`] behind schedule and forgave the backlog rather
    /// than bursting through it. Rate-limited by [`overload_warning_interval`]
    /// at the call site, so this increments at most once per warning, not
    /// once per skipped tick.
    pub(crate) fn record_overrun(&self) {
        self.overrun_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Total real (never skipped) ticks this clock has recorded.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count.load(Ordering::Relaxed)
    }

    /// Total overload events recorded — see [`record_overrun`](Self::record_overrun).
    #[must_use]
    pub fn overrun_count(&self) -> u64 {
        self.overrun_count.load(Ordering::Relaxed)
    }

    /// A snapshot of every figure this clock tracks.
    #[must_use]
    pub fn stats(&self) -> TickStats {
        let history = self.history.lock().expect("tick history lock poisoned");
        let sample_count = history.len() as u64;
        let sum_micros: u64 = history.iter().sum();
        let mspt_avg_ms = if sample_count == 0 {
            0.0
        } else {
            (sum_micros as f64 / sample_count as f64) / 1000.0
        };
        let mspt_ms = self.last_mspt_micros.load(Ordering::Relaxed) as f64 / 1000.0;
        // Vanilla never reports faster than 20 TPS even when the average tick
        // is well under 50ms — the tick *period* is the floor a full tick
        // cannot beat, matching the server's own debug-HUD TPS derivation
        // (`1000.0 / max(50.0, averageTickTimeMillis)`).
        let tps = 1000.0 / mspt_avg_ms.max(MILLIS_PER_TICK as f64);
        TickStats {
            tick_count: self.tick_count(),
            mspt_ms,
            mspt_avg_ms,
            tps,
            overrun_count: self.overrun_count(),
        }
    }
}

/// A point-in-time snapshot of [`TickClock`]'s accounting (issue #285).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickStats {
    /// Real world ticks run so far (never counts a skipped/forgiven tick).
    pub tick_count: u64,
    /// The most recently completed tick's own duration, in milliseconds.
    pub mspt_ms: f64,
    /// The rolling average over the last (up to) 100 ticks, in milliseconds.
    pub mspt_avg_ms: f64,
    /// `1000.0 / max(50.0, mspt_avg_ms)` — capped at 20, vanilla-style.
    pub tps: f64,
    /// How many times the loop has fallen more than ~2s behind schedule and
    /// forgiven the backlog. Zero across a healthy run; see
    /// [`run_tick_loop`]'s own doc comment for what a nonzero count means.
    pub overrun_count: u64,
}

/// The unified 20 Hz world-tick loop (issue #284): ticks the live [`MobSim`]
/// and every registered block entity once per [`TICK_PERIOD`], forever,
/// independently of whether any client is connected — replacing the two
/// separate background loops (`mobs::run_mob_tick_loop`,
/// `block_entities::run_block_entity_tick_loop`) that
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] used to spawn
/// side-by-side. Those two functions still exist (their own unit tests still
/// exercise them directly), but this is what production spawns now.
///
/// # Why one loop instead of two
///
/// Two independent `tokio::time::interval`s racing the same 50ms period are
/// two independent opportunities to drift out of phase under load — and, per
/// this crate's own `a6cc60a`, two copies of the same shutdown-race wrapper
/// to keep in sync. Folding both tick bodies into one loop iteration means
/// "the world advanced one tick" is one event with one timestamp, which is
/// what [`TickClock::record_tick`] actually measures: the wall-clock cost of
/// *both* systems together, the true per-tick budget a real MSPT figure
/// needs to reflect.
///
/// # Overrun handling
///
/// Mirrors vanilla's `MinecraftServer::runServer` behind
/// [`overload_threshold`]/[`overload_warning_interval`]: this loop tracks
/// `next_tick_at`, the wall-clock instant the *next* tick should start. Each
/// iteration, if `now` is more than [`overload_threshold`] past
/// `next_tick_at`, the loop **does not** run the missed ticks to catch up —
/// it logs a rate-limited warning and jumps `next_tick_at` forward by however
/// many tick periods it was behind, exactly like vanilla's
/// `this.nextTickTimeNanos += ticks * thisTickNanos;`
/// (`MinecraftServer.java:741`). The world tick body still runs exactly once
/// per loop iteration, both before and after this adjustment — the backlog is
/// forgiven, never replayed. A tick that never ran is never counted by
/// [`TickClock::record_tick`], so `tick_count` reflects real work done, not
/// wall-clock elapsed / 50ms.
///
/// # wasm32
///
/// Native only, like the two loops it replaces: `tokio::time::sleep_until`
/// needs `tokio::time`, unavailable on `wasm32` (see `mobs::run_mob_tick_loop`'s
/// own doc comment for the established precedent this repeats).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_tick_loop(
    mobs: MobHandle,
    mob_out: LiveMobSource,
    block_entities: BlockEntityHandle,
    clock: Arc<TickClock>,
) {
    // Same reasoning as `run_mob_tick_loop`'s own opening publish: a fresh
    // connection's first streaming pass should see the seeded population
    // immediately, not after waiting a full tick period for the loop below to
    // run once.
    mob_out.publish(mobs.with(|sim| sim.snapshots()));

    let mut next_tick_at = tokio::time::Instant::now();
    let mut last_overload_warning_at: Option<tokio::time::Instant> = None;

    loop {
        let now = tokio::time::Instant::now();
        let (adjusted_next, adjusted_warning, overload) =
            resolve_overload(now, next_tick_at, last_overload_warning_at);
        next_tick_at = adjusted_next;
        last_overload_warning_at = adjusted_warning;
        if let Some(event) = overload {
            tracing::warn!(
                ticks_behind = event.ticks_behind,
                behind_ms = event.behind_ms,
                "Can't keep up! Is the server overloaded? Running {}ms or {} ticks behind",
                event.behind_ms,
                event.ticks_behind,
            );
            clock.record_overrun();
        }

        next_tick_at += TICK_PERIOD;
        tokio::time::sleep_until(next_tick_at).await;

        let tick_start = tokio::time::Instant::now();
        mobs.with(MobSim::tick);
        mob_out.publish(mobs.with(|sim| sim.snapshots()));
        block_entities.with(BlockEntityRegistry::tick_all);
        clock.record_tick(tick_start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::ChunkWorld;

    fn handles() -> (MobHandle, LiveMobSource, BlockEntityHandle) {
        (
            MobHandle::new(ChunkWorld::new(-64, 384)),
            LiveMobSource::default(),
            BlockEntityHandle::default(),
        )
    }

    /// Predicted value: at 20 TPS, 10 real tick periods (500ms of virtual
    /// time) advance the loop exactly 10 ticks — not 9 (an off-by-one in the
    /// scheduling), not 11 (a burst), and `overrun_count` stays 0 because
    /// nothing here ever falls behind. Uses `start_paused` virtual time
    /// (never real wall-clock sleep) so this is immune to the box's load —
    /// exactly the "assert tick counts, not wall-clock timing" the task brief
    /// calls for.
    #[tokio::test(start_paused = true)]
    async fn ten_periods_advance_exactly_ten_ticks_with_no_overrun() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(mobs, out, block_entities, Arc::clone(&clock)));
        // Let the freshly spawned task reach its first `Instant::now()` call (its
        // `next_tick_at` baseline) before any `advance()` below — otherwise the
        // task's first poll (and thus its baseline) lands *after* the first
        // `advance`, silently shifting every prediction in this test by one
        // tick period. `tokio::spawn` never polls synchronously, so this is
        // required, not defensive.
        tokio::task::yield_now().await;

        for _ in 0..10 {
            tokio::time::advance(TICK_PERIOD).await;
        }
        // Let the just-woken task actually run its (synchronous) tick body.
        tokio::task::yield_now().await;

        assert_eq!(clock.tick_count(), 10, "expected exactly 10 ticks for 10 periods");
        assert_eq!(clock.overrun_count(), 0, "healthy run must not record an overrun");
    }

    /// Negative control for the assertion above: prove the counter would
    /// actually catch an off-by-one rather than always reading "close enough".
    /// Advancing only 9.9 periods (one period minus one virtual millisecond)
    /// must NOT produce a 10th tick — if it did, the `== 10` assertion above
    /// would be vacuous (it would pass regardless of the loop's real
    /// schedule).
    #[tokio::test(start_paused = true)]
    async fn nine_point_nine_periods_do_not_yet_produce_a_tenth_tick() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(mobs, out, block_entities, Arc::clone(&clock)));
        // Let the freshly spawned task reach its first `Instant::now()` call (its
        // `next_tick_at` baseline) before any `advance()` below — otherwise the
        // task's first poll (and thus its baseline) lands *after* the first
        // `advance`, silently shifting every prediction in this test by one
        // tick period. `tokio::spawn` never polls synchronously, so this is
        // required, not defensive.
        tokio::task::yield_now().await;

        for _ in 0..9 {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::time::advance(TICK_PERIOD - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            clock.tick_count(),
            9,
            "control failed: a 10th tick fired before its scheduled instant"
        );
    }

    // # Why the overrun branch is not tested through `run_tick_loop` at all
    //
    // An earlier version of this test spawned `run_tick_loop` and called
    // `tokio::time::advance(Duration::from_secs(3))`, expecting a single
    // forgiven-backlog tick. Measured result: **60 ticks, zero overruns** —
    // the opposite of both predictions. `tokio::time::advance` does not jump
    // the clock and then re-poll once; it fires every timer that falls
    // between the current and target instant *in the order their deadlines
    // land*, re-polling the task at each one. So a `sleep_until` scheduled
    // for +50ms resolves, the loop runs one (fast, synchronous) tick and
    // reschedules for +100ms, which is still within the swept range and so
    // resolves immediately too — all 60 periods fire in sequence, each seeing
    // itself as perfectly on schedule. This is a healthy run, not a stall: it
    // is indistinguishable, from inside the loop, from 60 real periods
    // elapsing with the task polled promptly each time. There is no
    // `tokio::time` API that jumps the virtual clock without also firing
    // intervening timers, so the overrun branch is untestable through the
    // async loop *by construction*, not by a gap in these tests. Hence
    // `resolve_overload` below: it is exactly the branch `run_tick_loop`
    // cannot exercise, tested directly with hand-built instants instead.

    /// Predicted value: `now` built as exactly [`overload_threshold`] past
    /// `next_tick_at` — the boundary vanilla's own check is strictly `>`,
    /// not `>=`, about (`MinecraftServer.java:735`). Must **not** trigger.
    #[test]
    fn resolve_overload_does_not_trigger_at_exactly_the_threshold() {
        let base = tokio::time::Instant::now();
        let now = base + overload_threshold();
        let (next, warn, overrun) = resolve_overload(now, base, None);
        assert_eq!(overrun, None, "exactly-at-threshold must not count as behind");
        assert_eq!(next, base, "no overrun means no adjustment to next_tick_at");
        assert_eq!(warn, None);
    }

    /// Negative control for the assertion above, proving the detector *can*
    /// fire: one millisecond further behind than the exact-boundary case
    /// must trigger. Predicted `ticks_behind`: `(2000 + 1) / 50 = 40`
    /// (integer division), so `next_tick_at` advances by exactly `40 *
    /// TICK_PERIOD` = 2000ms, landing 1ms *before* `now` — the one
    /// millisecond of true backlog past the last whole tick period is left
    /// for the immediately following real tick to absorb, never replayed as
    /// a separate forgiven tick of its own.
    #[test]
    fn resolve_overload_triggers_one_millisecond_past_the_threshold() {
        let base = tokio::time::Instant::now();
        let now = base + overload_threshold() + Duration::from_millis(1);
        let (next, warn, overrun) = resolve_overload(now, base, None);
        assert_eq!(
            overrun,
            Some(OverloadEvent {
                ticks_behind: 40,
                behind_ms: 2001,
            })
        );
        assert_eq!(next, base + TICK_PERIOD * 40);
        assert_eq!(warn, Some(next));
    }

    /// Pins the fix documented on [`resolve_overload`]'s own doc comment: the
    /// very first overload a fresh loop ever sees (`last_overload_warning_at
    /// == None`) must still fire — mirroring vanilla's `long` field
    /// defaulting to `0`, an enormous gap from any real `nextTickTimeNanos`.
    /// A version that seeded the "last warning" instant to the loop's own
    /// start (gap zero) would silently swallow exactly this case; this test
    /// exists because that was this function's first, wrong implementation.
    #[test]
    fn resolve_overload_fires_on_the_very_first_overload_with_no_prior_warning() {
        let base = tokio::time::Instant::now();
        let now = base + Duration::from_secs(10);
        let (_, _, overrun) = resolve_overload(now, base, None);
        assert!(overrun.is_some(), "the very first overload must not be swallowed");
    }

    /// Rate limiting: a second overload arriving well inside
    /// [`overload_warning_interval`] of the first must **not** re-trigger —
    /// proven against the earlier boundary test's positive case, which used
    /// the identical `behind` distance with `last_overload_warning_at = None`
    /// and *did* trigger, so this is a real control, not a tautology.
    ///
    /// `next_tick_at` at the second check is advanced by a realistic
    /// **healthy** gap first (`elapsed_healthy`, comfortably under the
    /// warning window) — matching how `run_tick_loop` actually gets here: it
    /// keeps incrementing `next_tick_at` by one [`TICK_PERIOD`] per iteration
    /// while caught up, and only re-enters this function's overload branch
    /// once it falls behind again.
    #[test]
    fn resolve_overload_is_rate_limited_inside_the_warning_window() {
        let base = tokio::time::Instant::now();
        let first_now = base + overload_threshold() + Duration::from_millis(1);
        let (next1, warn1, first) = resolve_overload(first_now, base, None);
        assert!(first.is_some(), "setup: the first call must trigger");

        // A second, shorter overload after only 1 real second of otherwise
        // healthy ticking — well inside the 15s warning window.
        let elapsed_healthy = Duration::from_secs(1);
        let next_tick_at_2 = next1 + elapsed_healthy;
        let second_now = next_tick_at_2 + overload_threshold() + Duration::from_millis(1);
        let (next2, warn2, second) = resolve_overload(second_now, next_tick_at_2, warn1);
        assert_eq!(second, None, "rate limit must suppress the second overload");
        assert_eq!(next2, next_tick_at_2, "no adjustment when rate-limited");
        assert_eq!(warn2, warn1, "warning instant must not move when rate-limited");
    }

    /// The other side of the same control: once
    /// [`overload_warning_interval`] has fully elapsed since the last
    /// warning, an ongoing overload **must** fire again — proves the rate
    /// limit is a window, not a permanent one-shot latch. Same
    /// healthy-gap-then-overload construction as the rate-limited test
    /// above, just with `elapsed_healthy` stretched past the window instead
    /// of well inside it.
    #[test]
    fn resolve_overload_fires_again_once_the_warning_window_elapses() {
        let base = tokio::time::Instant::now();
        let first_now = base + overload_threshold() + Duration::from_millis(1);
        let (next1, warn1, first) = resolve_overload(first_now, base, None);
        assert!(first.is_some(), "setup: the first call must trigger");

        let elapsed_healthy = overload_warning_interval();
        let next_tick_at_2 = next1 + elapsed_healthy;
        let second_now = next_tick_at_2 + overload_threshold() + Duration::from_millis(1);
        let (_, _, second) = resolve_overload(second_now, next_tick_at_2, warn1);
        assert!(
            second.is_some(),
            "a fresh overload after the warning window elapsed must fire"
        );
    }

    /// Negative control for the stall test: prove `overrun_count` is not
    /// permanently stuck at a nonzero value (e.g. from a detector that never
    /// resets) — a completely healthy run, with no stall at all, must report
    /// zero. Run alongside the two "no overrun" assertions above, but stated
    /// as its own control because it is the thing that would catch a detector
    /// that fires unconditionally.
    #[tokio::test(start_paused = true)]
    async fn a_healthy_run_never_records_an_overrun() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(mobs, out, block_entities, Arc::clone(&clock)));
        // Let the freshly spawned task reach its first `Instant::now()` call (its
        // `next_tick_at` baseline) before any `advance()` below — otherwise the
        // task's first poll (and thus its baseline) lands *after* the first
        // `advance`, silently shifting every prediction in this test by one
        // tick period. `tokio::spawn` never polls synchronously, so this is
        // required, not defensive.
        tokio::task::yield_now().await;

        for _ in 0..5 {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;

        assert_eq!(clock.overrun_count(), 0);
    }

    /// `TickStats::tps` must read 20.0 for an all-zero (never-ticked) clock —
    /// the `max(50.0, …)` floor in `TickClock::stats` — and must not divide by
    /// zero or panic. This is the magnitude check for the TPS formula itself,
    /// independent of the loop.
    #[test]
    fn tps_of_a_fresh_clock_reads_twenty() {
        let clock = TickClock::new();
        let stats = clock.stats();
        assert_eq!(stats.tick_count, 0);
        assert!((stats.tps - 20.0).abs() < f64::EPSILON);
    }

    /// A tick body that consistently takes 100ms (double budget) must report
    /// `mspt_avg_ms` close to 100.0 and `tps` close to 10.0 — the magnitude
    /// species of check (CLAUDE.md's evidence standards): a detector that only
    /// asserts "tps went down" would pass for any slowdown, large or small.
    #[test]
    fn mspt_average_and_tps_reflect_a_doubled_tick_cost() {
        let clock = TickClock::new();
        for _ in 0..HISTORY_LEN {
            clock.record_tick(Duration::from_millis(100));
        }
        let stats = clock.stats();
        assert!(
            (stats.mspt_avg_ms - 100.0).abs() < 0.5,
            "expected ~100ms average, got {}",
            stats.mspt_avg_ms
        );
        assert!(
            (stats.tps - 10.0).abs() < 0.5,
            "expected ~10 tps at double budget, got {}",
            stats.tps
        );
    }

    /// The rolling history caps at [`HISTORY_LEN`] samples: pushing far more
    /// than that must not let the average drift toward the oldest (discarded)
    /// samples. Feed 100 slow ticks, then 100 fast ones; the average must
    /// land near the fast figure, not halfway between the two.
    #[test]
    fn history_window_evicts_the_oldest_samples() {
        let clock = TickClock::new();
        for _ in 0..HISTORY_LEN {
            clock.record_tick(Duration::from_millis(200));
        }
        for _ in 0..HISTORY_LEN {
            clock.record_tick(Duration::from_millis(50));
        }
        let stats = clock.stats();
        assert!(
            (stats.mspt_avg_ms - 50.0).abs() < 0.5,
            "expected the 200ms samples to have aged out, got avg {}",
            stats.mspt_avg_ms
        );
        assert_eq!(stats.tick_count, (HISTORY_LEN * 2) as u64);
    }
}
