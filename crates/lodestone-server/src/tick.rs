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
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::mobs::{Detonation, LiveMobSource, MobHandle, MobSim};
use lodestone_entity::ai::mob::EatenBlock;
use crate::random_tick::{DEFAULT_RANDOM_TICK_SPEED, RandomTickScheduler};
use crate::scheduled_tick::{ScheduledTick, TickPriority};
use lodestone_model::BlockPos;

/// Vanilla's `mobGriefing` game rule, which gates whether a mob may change the
/// world — here, whether a grazing sheep's eat actually removes the block
/// (`ai/goal/EatBlockGoal.java:63,71`; `GameRules.RULE_MOBGRIEFING`, default
/// **true**).
///
/// A function returning vanilla's default rather than a real rule lookup, and
/// that is a disclosed gap, not an oversight: this crate has **no `GameRules`
/// registry**. The nearest thing is `crate::server::WorldAdminState`'s
/// `game_rules: HashMap<String, String>`, which is per-*connection* state owned
/// by `serve_play` — the wrong side of the world for a tick loop that runs with
/// no connection at all, and which that type's own doc comment already
/// describes as a confirmation echo rather than a rule store. Wiring the real
/// rule means a world-level `GameRules` the tick loop and every connection
/// share; when that exists, this function is the only call site to change.
///
/// Returning the default is the conservative choice for the *observable*
/// behaviour: vanilla ships `mobGriefing` on, so a sheep eating grass is what a
/// player expects to see, and modelling it as off would hide the feature behind
/// a rule nobody can currently turn back on.
fn mob_griefing() -> bool {
    true
}

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

/// Vanilla's own per-queue drain cap, `ServerLevel.MAX_SCHEDULED_TICKS_PER_TICK`
/// (`ServerLevel.java:194`) — see `crate::scheduled_tick`'s module doc for the
/// full citation of `blockTicks.tick(tick, 65536, ...)`/`fluidTicks.tick(tick,
/// 65536, ...)` (`ServerLevel.java:389,391`).
const MAX_SCHEDULED_TICKS_PER_TICK: usize = 65536;

/// How many ticks after world open to defer the first random-tick pass
/// (issue #481). The seed task in
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] runs
/// `generate_columns_offloaded` on the blocking pool — that call fans the
/// `mob_area` columns (49 at the shell's `view_radius.clamp(1, 3)`) out over
/// scoped OS threads and inserts them through the shared [`crate::ChunkStore`].
/// At the measured ~222 ms per warm contiguous column and `available_parallelism`
/// workers (≥ 2 in production), the batch completes in under 1.5 real seconds.
///
/// Deferring random ticks for 40 ticks (2.0 s) gives the seeding task time to
/// finish before the first `world.column()` call lands — so by the time random
/// ticks start, every column of `tick_area` is a cheap ~3.1 µs clone rather
/// than a cold ~909 ms generator run on the core thread.
///
/// Two seconds of deferred random ticks is imperceptible: grass spreading
/// takes minutes, and nothing else in the random-tick pass produces a visible
/// result on a sub-second timescale.
///
/// This is not a correctness gate — if the seed task has not finished after
/// 40 ticks, the random-tick pass pays the remaining cold generations on the
/// core thread, exactly as it did before this deferral existed. The gate only
/// removes the common case where the tick loop starts before seeding does.
const INITIAL_RANDOM_TICK_DEFERRAL_TICKS: u64 = 40;

/// Seeds for [`RandomTickScheduler`]'s two independent generators (issue
/// #307). Vanilla seeds its position LCG (`Level.randValue`) from an
/// arbitrary thread-local draw at level creation — this crate has no
/// per-world seed store to draw a "real" one from yet, so these are fixed
/// literals rather than derived from the world seed. Picking a different
/// (still fixed) literal changes nothing about which blocks are *eligible*
/// for a random tick, only the pseudo-random order/positions they are
/// visited in — see `random_tick.rs`'s own doc comment for why the draw
/// *pattern*, not the literal values, is what this crate's tests assert on.
const RANDOM_TICK_POSITION_SEED: i32 = 0x5EED_1234u32 as i32;
const RANDOM_TICK_BEHAVIOR_SEED: u64 = 0x5EED_5678;

/// A shared feed of block changes the world tick loop wants every connection
/// to learn about (issue #307/#308's one real producer reaching a client
/// today: grass ↔ dirt, via `crate::random_tick`). Mirrors [`LiveMobSource`]'s
/// publish shape — "world state that changes independently of any one
/// connection, and every connection must notice" is the exact problem
/// `LiveMobSource` already solves for mob positions; this is the same idiom
/// for block state.
///
/// # Single-consumer only
///
/// Unlike [`LiveMobSource`] (a replace-latest-snapshot design every connection
/// diffs independently), this is an **append-and-drain-all** queue: whichever
/// connection calls [`drain_all`](BlockTickFeed::drain_all) first consumes
/// every pending change, and a second concurrent consumer would see nothing.
/// This is correct for [`crate::IntegratedServer::open_in_memory_with_mobs`]
/// because it spawns **exactly one** connection task per feed instance (the
/// in-memory singleplayer duplex).
///
/// **[`IntegratedServer::bind`] (LAN) now does spawn a tick loop** — issue
/// #439; this doc comment previously said it did not, and that it therefore
/// never constructed one of these. It does. It does *not*, however, hand the
/// same instance to several connections, which is what would actually break:
/// each LAN connection gets its **own** feed pair and a relay arm in `bind`'s
/// accept loop drains the tick loop's hub feed and re-publishes into all of
/// them. That is a fan-out in front of this type rather than the
/// per-connection cursor over a shared append-only log this comment used to
/// recommend — the cursor is still the better shape (it needs no copy per
/// subscriber), and it is what to build if the subscriber count ever grows
/// past a handful.
/// # The inbound half (issue #465)
///
/// Field `.1` runs the *other* way: a connection publishes the block ticks its
/// own mutation scheduled, and [`run_tick_loop`] rebases them onto its own
/// counter and drains them like any other. It rides on this type rather than on
/// a new one because `BlockTickFeed` is already threaded through every
/// `serve_connection*` wrapper **and** into `run_tick_loop`, so this needs no
/// signature change anywhere.
///
/// It exists because `server::propagate_placement`'s `ScheduledTickQueue` was
/// local and discarded. Dust is synchronous (0 ticks, measured live) and so
/// completes there; a torch, repeater, comparator or observer instead reacts by
/// *scheduling* a recheck 1, 2 or `2d` ticks out, and only `run_tick_loop` owns
/// a queue those can land in. Without this channel, placing one of the delayed
/// families beside a live circuit does nothing at all, forever.
///
/// ## Why schedules and not positions
///
/// The originally brokered shape had the connection publish a *position* and
/// the loop re-run the fan-out there. **That does not work, and it was
/// measured** — see `server::propagate_placement`'s own doc comment for the
/// numbers. The inline fan-out consumes the change it propagates, so the loop's
/// second run finds a settled circuit and never reaches the component. Carrying
/// the schedule instead means the fan-out happens exactly once, at packet time,
/// like vanilla.
///
/// `trigger_tick` on an entry in this queue is a **relative delay**, not an
/// absolute tick: the publisher has no `game_tick` to be absolute against.
///
/// ## LAN is still not wired (carried forward, deliberately)
///
/// [`IntegratedServer::bind`] gives each connection its **own** feed pair and
/// relays the hub's *outbound* changes into them. Nothing relays the inbound
/// direction, so a request published on a per-connection feed is dropped and
/// LAN placement of a delayed component still does nothing. The fix is one
/// line in `integrated.rs`, whose owner is not this change's:
/// build each `LanSubscriber`'s feed with
/// [`subscriber`](BlockTickFeed::subscriber) on the hub instead of
/// `BlockTickFeed::default()`, which shares the inbound queue while keeping
/// the outbound one per-connection (the property the relay's drain-all
/// depends on). That constructor is provided here so the change really is one
/// line. Singleplayer — `open_in_memory_with_mobs`, and every gate below — is
/// unaffected: it hands the *same* instance to the loop and to its one
/// connection, so `Clone` already shares both halves.
#[derive(Debug, Clone, Default)]
pub struct BlockTickFeed(
    Arc<Mutex<Vec<(i32, i32, i32, String)>>>,
    /// Issue #465: block ticks a connection's own mutation scheduled, waiting
    /// to be rebased onto the tick loop's counter and hosted in its
    /// `block_ticks` queue. `trigger_tick` is a relative delay.
    Arc<Mutex<Vec<ScheduledTick<String>>>>,
);

impl BlockTickFeed {
    /// Records one block change for every consumer to learn about on their
    /// next [`drain_all`](Self::drain_all).
    pub(crate) fn publish(&self, x: i32, y: i32, z: i32, state: String) {
        self.0
            .lock()
            .expect("block tick feed lock poisoned")
            .push((x, y, z, state));
    }

    /// Drains and returns every change published since the last call —
    /// see the struct doc comment for why this is safe only for exactly one
    /// consumer.
    pub fn drain_all(&self) -> Vec<(i32, i32, i32, String)> {
        std::mem::take(&mut *self.0.lock().expect("block tick feed lock poisoned"))
    }

    /// A feed with its **own** outbound queue and this one's **shared**
    /// inbound queue — the shape a LAN per-connection subscriber needs (issue
    /// #465). See the struct doc comment: outbound must be per-connection
    /// because it is drain-all, inbound must be shared because the tick loop
    /// is the only drainer.
    // Deliberately unused in production today: its one intended caller is
    // `IntegratedServer::bind`'s `LanSubscriber`, in `integrated.rs`, which is
    // not this change's to edit. Kept (rather than deferred until that landing)
    // so the LAN fix is genuinely one line, and exercised by
    // `a_subscriber_shares_the_inbound_queue_and_splits_the_outbound_one`.
    #[allow(dead_code)]
    pub(crate) fn subscriber(&self) -> Self {
        Self(Arc::default(), Arc::clone(&self.1))
    }

    /// Hands the tick loop block ticks that a connection's own mutation
    /// scheduled, for it to rebase and host (issue #465).
    ///
    /// Each entry's `trigger_tick` is a **delay in ticks**, not an absolute
    /// tick — the publisher runs outside the tick loop and has no counter to be
    /// absolute against.
    pub(crate) fn request_scheduled_ticks(&self, ticks: Vec<ScheduledTick<String>>) {
        if ticks.is_empty() {
            return;
        }
        self.1
            .lock()
            .expect("block tick feed lock poisoned")
            .extend(ticks);
    }

    /// Drains every block tick requested since the last call.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn drain_scheduled_ticks(&self) -> Vec<ScheduledTick<String>> {
        std::mem::take(&mut *self.1.lock().expect("block tick feed lock poisoned"))
    }
}

/// A shared feed of detonations the world tick loop wants every connection
/// to learn about (issue #425) — the exact same idiom [`BlockTickFeed`]
/// already establishes just above, applied to
/// [`MobSim::take_detonations`]'s own drain instead of a random-ticked block
/// change. `MobSim::tick` already discards its `explode` return entirely
/// before this (issue #213's exposure/damage maths had two production
/// callers, both direct-explosion tests calling `MobSim::explode` by hand,
/// and zero path from "a creeper's fuse completed" to anything a client
/// could see) — this is what [`run_tick_loop`] publishes into so
/// `crate::server::serve_play`'s own `container_sync_tick` arm can forward a
/// real `EXPLODE` packet, the same way it already forwards
/// [`BlockTickFeed`]'s random-tick changes.
///
/// Same single-consumer caveat as [`BlockTickFeed`], and the same resolution
/// for LAN (issue #439): singleplayer has exactly one connection task per feed
/// instance, and `IntegratedServer::bind` gives each connection its own
/// instance behind a relay. See [`BlockTickFeed`]'s doc comment.
#[derive(Debug, Clone, Default)]
pub struct ExplosionFeed(Arc<Mutex<Vec<Detonation>>>);

impl ExplosionFeed {
    /// Records one detonation for every consumer to learn about on their
    /// next [`drain_all`](Self::drain_all).
    pub(crate) fn publish(&self, detonation: Detonation) {
        self.0
            .lock()
            .expect("explosion feed lock poisoned")
            .push(detonation);
    }

    /// Drains and returns every detonation published since the last call —
    /// see the struct doc comment for why this is safe only for exactly one
    /// consumer.
    pub fn drain_all(&self) -> Vec<Detonation> {
        std::mem::take(&mut *self.0.lock().expect("explosion feed lock poisoned"))
    }
}

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
/// Mirrors vanilla's `MinecraftServer::runServer` exactly, including the part
/// an earlier read of it got only half right: **vanilla does catch up, just
/// not indefinitely.** `next_tick_at` tracks the wall-clock instant the next
/// tick should start; `next_tick_at += TICK_PERIOD` happens unconditionally
/// every iteration (vanilla: `this.nextTickTimeNanos += thisTickNanos;`,
/// `MinecraftServer.java:752`), and `tokio::time::sleep_until` for an
/// already-past instant resolves immediately with no artificial delay — so
/// while only mildly behind, consecutive iterations run back-to-back at full
/// speed with no sleep between them, exactly matching vanilla's own
/// `waitUntilNextTick`/`haveTime` (`:846-863`), which does not park at all
/// once `Util.getNanos() >= nextTickTimeNanos`. *That* is vanilla's catch-up
/// mechanism, and this loop already has it for free from `sleep_until`'s own
/// semantics — no separate code path needed.
///
/// The forgiveness branch below is not the catch-up mechanism; it is what
/// happens once catching up the normal way would take too long. Each
/// iteration, if `now` is more than [`overload_threshold`] past
/// `next_tick_at` (2 real seconds — see that function's own doc comment), the
/// loop gives up: it logs a rate-limited warning and jumps `next_tick_at`
/// forward by however many tick periods it was behind, exactly like
/// vanilla's `this.nextTickTimeNanos += ticks * thisTickNanos;`
/// (`MinecraftServer.java:741`). The world tick body still runs exactly once
/// per loop iteration, both before and after this adjustment — that backlog,
/// specifically, is forgiven rather than replayed; smaller backlogs are
/// still replayed by the back-to-back iterations described above. A tick
/// that never ran is never counted by [`TickClock::record_tick`], so
/// `tick_count` reflects real work done, not wall-clock elapsed / 50ms.
///
/// # Scheduled ticks and random ticks (issues #307/#308)
///
/// Each iteration additionally drains the block-tick queue, then the
/// fluid-tick queue, then runs random ticks over `tick_area` — in exactly
/// that order, mirroring `ServerLevel.tick`'s own sequence
/// (`ServerLevel.java:388-391,400-401`: `blockTicks.tick(...)` before
/// `fluidTicks.tick(...)` before `this.getChunkSource().tick(...)`, which is
/// what eventually calls `tickChunk`'s random ticks — see
/// `ServerChunkCache.java:403`). See [`crate::scheduled_tick`] for the queues'
/// own ordering contract and [`crate::random_tick`] for the random-tick
/// selection and the one block (grass ↔ dirt) modeled end to end.
///
/// **Nothing schedules a block or fluid tick yet** — `block_ticks`/
/// `fluid_ticks` below are drained every iteration (proving the *order* is
/// wired: block before fluid before random, every tick), but no producer in
/// this crate calls [`ScheduledTickQueue::schedule`] on them today. Stated
/// plainly, per this issue's own brief: the scheduled-tick *queue* (#308) is
/// real and tested in isolation (`crate::scheduled_tick`'s own test module),
/// but is an acknowledged island here until a block behaviour (fluid flow
/// #309, gravity blocks #311, redstone #314-322) schedules into it. Random
/// ticks (#307) are **not** an island: [`RandomTickScheduler::tick_chunk`]
/// runs against `world` (the same [`ChunkSource`] the connection this loop
/// shares a server with actually serves), and every resulting change is
/// both persisted (`ChunkSource::set_block`) and published through
/// `block_tick_out` for `serve_play`'s `container_sync_tick` arm to forward
/// to the connected client — see that arm's own doc comment for the wire
/// half.
///
/// `tick_area` is deliberately the same `(cx_range, cz_range)` shape
/// `open_in_memory_with_mobs` already threads through for `mob_area` — not a
/// generic "loaded chunks" registry (this crate has none — see
/// `crate::chunk`'s module doc), but a small fixed region, matching the
/// scope mob pathing already accepted. Every chunk in it is re-fetched via
/// `world.column(cx, cz)` **every tick**; for an unedited column this
/// re-runs the generator (no per-tick cache beyond `OverworldChunkSource`'s
/// own edit cache — see that type's doc comment), which is a real,
/// documented performance gap for anything wider than a handful of chunks,
/// not a correctness one.
///
/// # wasm32
///
/// Native only, like the two loops it replaces: `tokio::time::sleep_until`
/// needs `tokio::time`, unavailable on `wasm32` (see `mobs::run_mob_tick_loop`'s
/// own doc comment for the established precedent this repeats).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_tick_loop<W>(
    mobs: MobHandle,
    mob_out: LiveMobSource,
    block_entities: BlockEntityHandle,
    clock: Arc<TickClock>,
    world: Arc<W>,
    block_tick_out: BlockTickFeed,
    tick_area: (RangeInclusive<i32>, RangeInclusive<i32>),
    explosion_out: ExplosionFeed,
    // Issue #468's last wire. The two scheduled-tick queues used to be locals
    // here, so the queues the persistence path reads were always empty in
    // production and a pending repeater tick was lost on quit — the schema
    // (`chunk_nbt::SavedTick`) and the save/load halves were both built and both
    // gated against real vanilla bytes while nothing filled them.
    //
    // The game tick they are measured against travels with them, because it must
    // come from *this* counter and not be re-derived. A second clock here is
    // issue #323's bug in a new place: `SET_TIME` decoded, really did darken the
    // sky, every link in the wire green, while the value was wall-clock
    // elapsed-since-join rather than the tick counter.
    scheduled: crate::region_source::ScheduledTickHandle,
) where
    W: ChunkSource,
{
    // Same reasoning as `run_mob_tick_loop`'s own opening publish: a fresh
    // connection's first streaming pass should see the seeded population
    // immediately, not after waiting a full tick period for the loop below to
    // run once.
    mob_out.publish(mobs.with(|sim| sim.snapshots()));

    let mut next_tick_at = tokio::time::Instant::now();
    let mut last_overload_warning_at: Option<tokio::time::Instant> = None;
    let mut game_tick: u64 = 0;
    // #308: one queue per vanilla queue (`ServerLevel.blockTicks`/
    // `fluidTicks`, `ServerLevel.java:209-210`). Owned by `scheduled` rather
    // than by this function since #468, which is what lets them be saved; they
    // are borrowed out of it once per tick below.
    // #307.
    let mut random_ticks = RandomTickScheduler::new(RANDOM_TICK_POSITION_SEED, RANDOM_TICK_BEHAVIOR_SEED);
    let (tick_cx_range, tick_cz_range) = tick_area;

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
        // Issue #425: `MobSim::tick` already calls `MobSim::explode` the
        // tick a creeper's own fuse completes (`1feed17`/`614acb8`), but
        // until now nothing read the detonation back out of the sim — see
        // `ExplosionFeed`'s own doc comment just above for why this is the
        // one production path that turns "a creeper detonated" into an
        // `EXPLODE` packet reaching a connection at all.
        for detonation in mobs.with(MobSim::take_detonations) {
            explosion_out.publish(detonation);
        }
        // Issue #456: the world half of a grazing sheep, and the same shape as
        // the detonation drain above for the same structural reason —
        // `MobSim::tick` holds `world: &'w ChunkWorld` **immutably**, so it can
        // only record the eat as an intent; this loop is the one place that owns
        // a mutable `ChunkSource` and can apply it. `EatBlockGoal` reaching this
        // drain is what makes the grass actually disappear rather than the goal
        // counting down against a world that never changes.
        //
        // Vanilla `ai/goal/EatBlockGoal.java:59-80`, and the two variants really
        // are different operations rather than one with a different block id:
        //
        // * `AtFeet` is `destroyBlock(pos, false)` — the block the sheep stands
        //   *in* becomes air, and the `false` is "no drops", so a grazed
        //   short-grass yields nothing.
        // * `Below` is `setBlock(below, DIRT, 2)` — the `grass_block` underfoot
        //   is *replaced*, not destroyed, so nothing drops there either but the
        //   cell stays solid and the sheep does not fall.
        //
        // Published on `block_tick_out` because that is already the wire path
        // `serve_play` drains for random-ticked block changes, so the client
        // sees this exactly as it sees grass spreading — no second feed needed.
        for (pos, eaten) in mobs.with(MobSim::take_grazes) {
            // Drained unconditionally, gated afterwards: with `mobGriefing` off
            // the eat still *happened* (vanilla calls `mob.ate()` either way —
            // see `MobSim::take_grazes`'s own doc comment), so swallowing the
            // queue entry is correct and leaving it to accumulate would not be.
            if !mob_griefing() {
                continue;
            }
            let (target, state) = match eaten {
                EatenBlock::AtFeet => (pos, "minecraft:air"),
                EatenBlock::Below => (BlockPos::new(pos.x, pos.y - 1, pos.z), "minecraft:dirt"),
            };
            world.set_block(target.x, target.y, target.z, state);
            block_tick_out.publish(target.x, target.y, target.z, state.to_owned());
        }
        // Issue #321: the hopper redstone lock. `tick_all`'s unlocked shorthand
        // would tick every hopper as `enabled: true` forever, which is what this
        // line used to do — see `BlockEntityRegistry::tick_all_with_hopper_lock`,
        // and note this is the **only** production caller holding both a
        // `ChunkSource` and the registry, so it is the only place the lock can be
        // read at all.
        //
        // Read off the block state rather than recomputed from neighbours here,
        // because the block state is vanilla's own source of truth:
        // `HopperBlock.checkPoweredState` maintains `ENABLED` on every neighbour
        // change and on placement (`crate::random_tick`'s hopper arms), and
        // `HopperBlockEntity` then simply obeys it. Recomputing would duplicate
        // the signal walk and could disagree with what the client was told.
        block_entities.with(|registry| {
            registry.tick_all_with_hopper_lock(&|pos| {
                crate::redstone::hopper_enabled(&world.block_state(pos.x, pos.y, pos.z))
            });
        });

        game_tick += 1;
        // Issue #468: the tick every pending `trigger_tick` is relative to, so a
        // saved queue can be rebased on load. One relaxed atomic store — the
        // tick-thread cost is a count of one, no I/O and no encoding.
        scheduled.set_game_tick(game_tick);

        // Issue #468: both queues are borrowed out of `scheduled` for the whole
        // scheduled-tick and random-tick section, and every use site inside is
        // textually unchanged — deliberately a wrapper rather than a rewrite,
        // because this is the function the redstone work lives in.
        //
        // `with` is **synchronous**, and that is the safety property rather than
        // a limitation: a closure cannot contain an `.await`, so the compiler —
        // not a reviewer — guarantees the `MutexGuard` never crosses a suspension
        // point, which would make this task non-`Send`. Verified: there is no
        // `.await` anywhere in the wrapped region.
        //
        // The region extends past the last use of `fluid_ticks` to the end of the
        // random-tick pass, which `docs/tick-scheduling.md`'s step 3 does not
        // mention: `random_ticks.tick_chunk` also takes `&mut block_ticks`, so
        // closing the closure at the fluid loop would put that call out of scope.
        // The body below is left at its original indentation for the same reason
        // the wrapper shape was chosen — re-indenting it would touch every line
        // of the section and bury the real change.
        scheduled.with(|queues| {
        let mut block_ticks = &mut queues.block;
        let fluid_ticks = &mut queues.fluid;
        // Issue #465: adopt the block ticks a player's own mutation scheduled.
        // `server::propagate_placement` runs the fan-out inline at packet time
        // (like vanilla) and cannot host what it schedules, because the queue
        // those land in lives here. So it hands them over with relative delays
        // and this rebases them onto `game_tick`.
        //
        // Placed after `game_tick += 1` and before the `block_ticks` drain on
        // purpose, and the ordering is the whole fidelity argument. Vanilla
        // handles queued packets at the top of a tick
        // (`MinecraftServer.tickServer` -> `tickConnections`) and drains
        // `ServerLevel.blockTicks` later in that *same* tick, so a placement
        // arriving between tick N-1 and tick N schedules against N and fires at
        // `N + delay`. That is exactly what this does, which is why the residual
        // deviation is **not** in the fired tick number — see
        // `redstone_placement_gate`'s
        // `the_delay_is_measured_from_the_tick_that_drained_the_request_not_from_tick_zero`
        // for the measurement, and note that `has_scheduled` is consulted here
        // for the same reason `propagate_and_react` consults it: two placements
        // in one inter-tick window must not double-schedule one position.
        for pending in block_tick_out.drain_scheduled_ticks() {
            if block_ticks.has_scheduled(pending.pos, &pending.kind) {
                continue;
            }
            block_ticks.schedule(
                pending.pos,
                pending.kind,
                game_tick + pending.trigger_tick,
                pending.priority,
            );
        }

        // #308, block before fluid (`ServerLevel.java:388-391`). Draining
        // (rather than iterating a live queue) is what keeps a tick
        // scheduled *by* one of these callbacks out of this same pass — see
        // `ScheduledTickQueue::drain_due`'s own doc comment.
        //
        // Issue #314/#315/#317: `block_ticks` now has real producers —
        // redstone torches/repeaters/comparators/observers schedule into it
        // from `crate::random_tick::propagate_and_react` whenever a
        // neighbour notification finds one of them out of steady state (see
        // that function's own doc comment). This drain is where the delayed
        // flip actually happens: `redstone::TICK_TORCH`/`TICK_REPEATER`/
        // `TICK_COMPARATOR`/`TICK_OBSERVER` each dispatch to their own
        // family's `run_scheduled_tick`, and any resulting mutation is
        // re-propagated through the same `propagate_and_react` call site a
        // random tick would use, so a chain reaction (a repeater flipping
        // and feeding a further torch) resolves depth-first within this one
        // drain, exactly like vanilla's `LevelTicks::runCollectedTicks`
        // invoking its callback once per due entry, in `DRAIN_ORDER`.
        for due in block_ticks.drain_due(game_tick, MAX_SCHEDULED_TICKS_PER_TICK) {
            let (x, y, z) = due.pos;
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let min_x = cx * 16;
            let min_z = cz * 16;
            let lx = x - min_x;
            let lz = z - min_z;
            let mut column = world.column(cx, cz);
            if y < column.min_y || y >= column.min_y + column.height {
                continue;
            }
            let state = column.block_state(lx, y, lz).to_string();

            let new_state = if due.kind == crate::redstone::TICK_TORCH {
                let has_signal = crate::redstone_torch::has_neighbor_signal(&crate::redstone::make_lookup(&column, min_x, min_z), BlockPos::new(x, y, z), &state);
                crate::redstone_torch::run_scheduled_tick(&state, has_signal)
            } else if due.kind == crate::redstone::TICK_REPEATER {
                let facing = crate::redstone::diode_facing(&state);
                let should_on =
                    crate::redstone_diode::repeater_should_turn_on(&crate::redstone::make_lookup(&column, min_x, min_z), BlockPos::new(x, y, z), facing);
                match crate::redstone_diode::run_scheduled_tick(&state, should_on) {
                    crate::redstone_diode::RepeaterTickOutcome::TurnedOff(s) => Some(s),
                    crate::redstone_diode::RepeaterTickOutcome::TurnedOn { new_state, reschedule } => {
                        if reschedule {
                            let delay = crate::redstone_diode::repeater_delay(&new_state);
                            block_ticks.schedule((x, y, z), crate::redstone::TICK_REPEATER.to_string(), game_tick + u64::from(delay), TickPriority::VeryHigh);
                        }
                        Some(new_state)
                    }
                    crate::redstone_diode::RepeaterTickOutcome::Locked | crate::redstone_diode::RepeaterTickOutcome::NoChange => None,
                }
            } else if due.kind == crate::redstone::TICK_COMPARATOR {
                let facing = crate::redstone::diode_facing(&state);
                let input = crate::redstone::input_signal(&crate::redstone::make_lookup(&column, min_x, min_z), BlockPos::new(x, y, z), facing);
                let side = crate::redstone::alternate_signal(&crate::redstone::make_lookup(&column, min_x, min_z), BlockPos::new(x, y, z), facing, false);
                crate::redstone_diode::run_scheduled_comparator_tick(&state, input, side)
            } else if due.kind == crate::redstone::TICK_OBSERVER {
                let (new_state, reschedule) = crate::redstone_observer::run_scheduled_tick(&state);
                if reschedule {
                    block_ticks.schedule((x, y, z), crate::redstone::TICK_OBSERVER.to_string(), game_tick + 2, TickPriority::Normal);
                }
                Some(new_state)
            } else {
                // No other block-tick behaviour is modeled — see this
                // function's own doc comment.
                None
            };

            if let Some(new_state) = new_state {
                if new_state != state {
                    column.set_block(lx, y, lz, &new_state);
                    world.set_block(x, y, z, &new_state);
                    block_tick_out.publish(x, y, z, new_state);
                }
                for event in crate::random_tick::propagate_and_react(&mut column, min_x, min_z, x, y, z, &mut block_ticks, game_tick) {
                    let (ex, ey, ez) = event.pos;
                    world.set_block(ex, ey, ez, &event.to);
                    block_tick_out.publish(ex, ey, ez, event.to);
                }
            }
        }
        for _due in fluid_ticks.drain_due(game_tick, MAX_SCHEDULED_TICKS_PER_TICK) {
            // Same acknowledgement as above, for fluid ticks.
        }

        // #307, after both scheduled-tick queues (`ServerChunkCache.java:403`
        // runs after `ServerLevel`'s own `blockTicks`/`fluidTicks.tick`,
        // `:388-391`).
        //
        // #481: the random-tick pass is deferred for the first few ticks
        // after world open, so the background column-seeding task has time to
        // populate the shared [`ChunkStore`] before any `world.column()` call
        // pays the full per-column generation cost on the core thread. See
        // [`INITIAL_RANDOM_TICK_DEFERRAL_TICKS`] for the arithmetic.
        if game_tick > INITIAL_RANDOM_TICK_DEFERRAL_TICKS {
            for cz in tick_cz_range.clone() {
                for cx in tick_cx_range.clone() {
                    let mut column = world.column(cx, cz);
                    let events = random_ticks.tick_chunk(&mut column, cx, cz, DEFAULT_RANDOM_TICK_SPEED, &mut block_ticks, game_tick);
                    for event in events {
                        let (x, y, z) = event.pos;
                        world.set_block(x, y, z, &event.to);
                        block_tick_out.publish(x, y, z, event.to);
                    }
                }
            }
        }
        });

        clock.record_tick(tick_start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::ChunkWorld;
    // No longer imported at module scope: `run_tick_loop` borrows its queues out
    // of `ScheduledTickHandle` since #468 and names the type nowhere.
    use crate::scheduled_tick::ScheduledTickQueue;
    // For `ResourceKey::from_str` in the issue-#456 graze gates below.
    use std::str::FromStr;

    fn handles() -> (MobHandle, LiveMobSource, BlockEntityHandle) {
        (
            MobHandle::new(ChunkWorld::new(-64, 384)),
            LiveMobSource::default(),
            BlockEntityHandle::default(),
        )
    }

    /// A minimal [`ChunkSource`] for tests that only need `run_tick_loop` to
    /// have *something* to random-tick against — every column is bare air,
    /// so #307's random ticks run (proving the loop's own ordering/timing)
    /// but never produce an event (nothing eligible), which is exactly what
    /// the MSPT/overrun tests in this module want: zero interference from
    /// #307/#308 with the clock behaviour they actually assert on.
    struct EmptyWorld;
    impl ChunkSource for EmptyWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 16)
        }
    }

    /// `(world, block_tick_out, tick_area)` — the three new `run_tick_loop`
    /// arguments (issues #307/#308), factored out because every existing
    /// clock/overrun test in this module needs them but does not care what
    /// they are.
    fn world_tick_args() -> (Arc<EmptyWorld>, BlockTickFeed, (RangeInclusive<i32>, RangeInclusive<i32>)) {
        (Arc::new(EmptyWorld), BlockTickFeed::default(), (0..=0, 0..=0))
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
        let (world, block_tick_out, tick_area) = world_tick_args();
        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::clone(&clock),
            world,
            block_tick_out,
            tick_area,
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
        ));
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
        let (world, block_tick_out, tick_area) = world_tick_args();
        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::clone(&clock),
            world,
            block_tick_out,
            tick_area,
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
        ));
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

    /// One pending block tick, built through a real `ScheduledTickQueue`
    /// because `ScheduledTick` carries a private `sub_tick_order` and cannot be
    /// constructed with a struct literal from here.
    fn one_pending(pos: (i32, i32, i32)) -> Vec<ScheduledTick<String>> {
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        queue.schedule(pos, crate::redstone::TICK_REPEATER.to_owned(), 2, TickPriority::Normal);
        queue.drain_due(u64::MAX, usize::MAX)
    }

    /// Issue #465's LAN shape, asserted at the type level so the remaining gap
    /// is provably nothing more than one call site in `integrated.rs`.
    ///
    /// A `LanSubscriber`'s feed must split the two directions: the **outbound**
    /// queue per-connection (it is drain-all, so a shared one would let the
    /// first consumer starve every other player), the **inbound** queue shared
    /// (only the tick loop drains it, and a per-connection one would silently
    /// swallow every placement). Both halves are asserted, including the
    /// negative direction — `Default` deliberately shares *neither*, which is
    /// why `bind` cannot keep using it.
    #[test]
    fn a_subscriber_shares_the_inbound_queue_and_splits_the_outbound_one() {
        let hub = BlockTickFeed::default();
        let conn = hub.subscriber();

        conn.request_scheduled_ticks(one_pending((7, 1, 9)));
        assert_eq!(
            hub.drain_scheduled_ticks().iter().map(|t| t.pos).collect::<Vec<_>>(),
            vec![(7, 1, 9)],
            "a subscriber's scheduled block tick must reach the hub the tick loop drains"
        );

        conn.publish(1, 2, 3, "minecraft:stone".to_owned());
        assert!(
            hub.drain_all().is_empty(),
            "the outbound queues must stay separate, or one LAN player's drain-all starves the rest"
        );
        assert_eq!(conn.drain_all().len(), 1, "the subscriber keeps its own outbound change");

        // The control: this is what `bind` does today, and why LAN placement of
        // a delayed component is still dropped.
        let orphan = BlockTickFeed::default();
        orphan.request_scheduled_ticks(one_pending((7, 1, 9)));
        assert!(
            hub.drain_scheduled_ticks().is_empty(),
            "CONTROL FAILED: a `default()` feed already reaches the hub, so `subscriber()` would \
             not be needed and the LAN gap this documents would not exist"
        );
    }

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
        let (world, block_tick_out, tick_area) = world_tick_args();
        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::clone(&clock),
            world,
            block_tick_out,
            tick_area,
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
        ));
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

    // ---------------------------------------------------------------------
    // Issue #456: the graze drain. Before this, `MobSim::take_grazes` had no
    // consumer anywhere — a sheep ran a real `EatBlockGoal`, recorded a real
    // eat, and the world never changed. These gates drive the **production
    // loop**, so they fail if the drain is removed from `run_tick_loop` rather
    // than merely if `take_grazes` regresses.
    // ---------------------------------------------------------------------

    /// A [`ChunkSource`] that records every [`ChunkSource::set_block`] the tick
    /// loop applies, and serves bare-air columns.
    ///
    /// Air, deliberately: the loop random-ticks `tick_area` against this same
    /// source every tick, and an eligible block there would write its own
    /// `set_block` calls into the very list these gates read — a grass block
    /// dying to `minecraft:dirt` is *byte-identical* to the `Below` graze this
    /// asserts. Air makes the recording unambiguous by making the graze the only
    /// possible writer.
    ///
    /// The graze decision does not come from here in any case: `EatBlockGoal`
    /// reads the *sim's* `ChunkWorld` (see `grass_world` below). Production
    /// shares one object between the two roles; separating them here is what
    /// lets the gate watch the mutation arrive.
    #[derive(Default)]
    struct RecordingWorld(Arc<Mutex<Vec<(i32, i32, i32, String)>>>);

    impl ChunkSource for RecordingWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 16)
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.0
                .lock()
                .expect("recording world lock poisoned")
                .push((x, y, z, name.to_owned()));
        }
    }

    /// Grass over a wide area, with `short_grass` on top only when `at_feet`.
    ///
    /// **49×49, not a few cells, and that width is load-bearing.** Grazing
    /// destroys its own supply, and the sheep also strolls; over a small patch a
    /// gate stops measuring "does the eat reach the world" and starts measuring
    /// how quickly the sheep runs out of grass — one earlier draft of the
    /// interval gate read 125 eats against a predicted 444 for exactly that
    /// reason.
    fn grass_world(at_feet: bool) -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -24..=24 {
            for z in -24..=24 {
                world.set_block(x, -1, z, "minecraft:grass_block");
                if at_feet {
                    world.set_block(x, 0, z, "minecraft:short_grass");
                }
            }
        }
        world
    }

    /// Ticks the real [`run_tick_loop`] over a sheep on grass until the loop
    /// applies a block change, and returns `(world edits, feed publications)`.
    ///
    /// The sheep comes from [`MobSim::spawn_species`], so its `EatBlockGoal`
    /// comes from the per-species roster — **not** hand-installed. Installing it
    /// by hand *as well* is a known trap: two goals at the same priority each
    /// draw their own `next_i32(interval)`, and an interval gate built that way
    /// measured 627 eats against a predicted 444.
    ///
    /// `tick_area` is a chunk the sheep is nowhere near, so the random-tick pass
    /// cannot contribute an edit even if `RecordingWorld` were ever given
    /// eligible terrain.
    async fn graze_until_edit(
        at_feet: bool,
        baby: bool,
        max_ticks: usize,
    ) -> (Vec<(i32, i32, i32, String)>, Vec<(i32, i32, i32, String)>) {
        let world = grass_world(at_feet);
        let mobs = MobHandle::new(world);
        mobs.with(|sim| {
            let sheep =
                lodestone_model::ResourceKey::from_str("minecraft:sheep").expect("static key");
            let m = sim.spawn_species(sheep, lodestone_model::Vec3::new(0.5, 0.0, 0.5));
            if baby {
                m.set_age(-24_000);
            }
        });

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(RecordingWorld(Arc::clone(&recorded)));
        let feed = BlockTickFeed::default();
        tokio::spawn(run_tick_loop(
            mobs,
            LiveMobSource::default(),
            BlockEntityHandle::default(),
            Arc::new(TickClock::new()),
            source,
            feed.clone(),
            (64..=64, 64..=64),
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
        ));
        // See `ten_periods_advance_exactly_ten_ticks_with_no_overrun`: the
        // spawned task must reach its first `Instant::now()` before any
        // `advance`, or every tick prediction shifts by one period.
        tokio::task::yield_now().await;

        let mut published = Vec::new();
        for _ in 0..max_ticks {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            published.extend(feed.drain_all());
            if !recorded.lock().expect("poisoned").is_empty() {
                break;
            }
        }
        let edits = recorded.lock().expect("poisoned").clone();
        (edits, published)
    }

    /// The half-width of [`grass_world`]'s patch. An edit inside it is under
    /// terrain the sheep could actually have reached and grazed.
    const GRASS_HALF_WIDTH: i32 = 24;

    /// Whether `(x, z)` is inside the grass patch.
    ///
    /// This replaced an `assert_eq!((x, z), (0, 0))` that asserted the sheep
    /// grazes where it spawned — which is **false, and for a good reason**: a
    /// sheep is persistent, so vanilla clears its idle throttle every tick and
    /// its `WaterAvoidingRandomStrollGoal` is live, so it wanders before it eats.
    /// Both arms observed the graze at `(-5, 4)`, not `(0, 0)`. Pinning the spawn
    /// column would have been the same mistake as an earlier gate in this repo
    /// that pinned a mob to `(0,0,0)` while `RandomStrollGoal` legitimately
    /// walked it to `(-2,0,-2)`.
    ///
    /// `y` remains asserted exactly, because `y` is the only coordinate that
    /// distinguishes the two `EatenBlock` branches — `x`/`z` are the mob's own
    /// column either way and carry no information about which one ran.
    fn in_grass_patch(x: i32, z: i32) -> bool {
        x.abs() <= GRASS_HALF_WIDTH && z.abs() <= GRASS_HALF_WIDTH
    }

    /// How long to let the loop run before giving up. An adult's mean grazing
    /// interval is `adjustedTickDelay(1000)` = 500 ticks
    /// (`EatBlockGoal::ADULT_INTERVAL`), and the eat lands
    /// `EAT_ANIMATION_TICKS - CONSUME_AT` ticks into the animation after that,
    /// so a few thousand ticks is generous without being unbounded.
    const GRAZE_TICK_BUDGET: usize = 4_000;

    /// The `Below` case: a sheep standing on a `grass_block` with nothing edible
    /// at its feet turns that block to **dirt**, one cell down.
    ///
    /// The assertion is on **`y` alone** on purpose. `x`/`z` are identical for
    /// both `EatenBlock` variants — the mob's own column — so they carry no
    /// information about which branch ran; `y` is the only coordinate that does.
    #[tokio::test(start_paused = true)]
    async fn a_grazing_sheep_turns_the_grass_block_below_it_to_dirt() {
        let (edits, published) = graze_until_edit(false, true, GRAZE_TICK_BUDGET).await;

        assert!(
            !edits.is_empty(),
            "no block change reached the world in {GRAZE_TICK_BUDGET} ticks — this is \
             the pre-fix state exactly: the goal records the eat and nothing drains it"
        );
        let (x, y, z, ref state) = edits[0];
        assert_eq!(y, -1, "the Below branch must edit one cell *down* from the sheep, not its own cell");
        assert_eq!(state, "minecraft:dirt", "setBlock(below, DIRT, 2) — replaced, not destroyed");
        assert!(in_grass_patch(x, z), "edit landed outside the grass at ({x}, {z})");
        assert!(
            published.contains(&(x, y, z, state.clone())),
            "the change must also reach the wire feed, or the client never sees it: \
             published = {published:?}"
        );
    }

    /// The `AtFeet` case, and the control that proves the assertion above is
    /// about the branch rather than about "some block changed": same sheep, same
    /// loop, same world **plus** `short_grass` in the sheep's own cell, and now
    /// the edit must land at `y = 0` as **air**. If the drain ignored
    /// `EatenBlock` and always did one of the two, exactly one of this pair
    /// would fail.
    #[tokio::test(start_paused = true)]
    async fn a_sheep_standing_in_short_grass_destroys_that_block_instead() {
        let (edits, published) = graze_until_edit(true, true, GRAZE_TICK_BUDGET).await;

        assert!(!edits.is_empty(), "no block change reached the world");
        let (x, y, z, ref state) = edits[0];
        assert_eq!(y, 0, "the AtFeet branch must edit the sheep's own cell");
        assert_eq!(state, "minecraft:air", "destroyBlock(pos, false) — gone, and no drops");
        assert!(in_grass_patch(x, z), "edit landed outside the grass at ({x}, {z})");
        assert!(
            published.contains(&(x, y, z, state.clone())),
            "published = {published:?}"
        );
    }
}
