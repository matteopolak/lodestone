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
use crate::border::WorldBorder;
use crate::chunk::ChunkSource;
use crate::mobs::{Detonation, LiveMobSource, MobHandle, MobSim};
use lodestone_entity::ai::mob::EatenBlock;
use crate::random_tick::RandomTickScheduler;
use crate::scheduled_tick::{ScheduledTick, TickPriority};
use crate::sleep::{SleepEvent, SleepFeed, SleepState, SleepVote};
use crate::weather::{WeatherFeed, WeatherState};
use lodestone_model::BlockPos;

/// The natural-spawn driver's RNG seed. A fixed literal, like every other seed
/// in this module (`RANDOM_TICK_POSITION_SEED` and friends): the world seed is
/// not threaded into this loop, and the spawn stream only has to be *reproducible*,
/// not world-derived.
const NATURAL_SPAWN_SEED: u64 = 0x5350_4157_4E45_5221;

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

/// Vanilla's `advanceWeather` game rule, which gates whether the weather
/// cycle's timers advance at all — issue #324 / `docs/plans/world-state.md`
/// W1, read by [`crate::weather::WeatherState::tick`]. Vanilla's
/// `GameRules.ADVANCE_WEATHER` defaults to **true**
/// (`ServerLevel.java:713` gates `advanceWeatherCycle`'s timer block on it).
///
/// A function returning vanilla's default rather than a real rule lookup, and
/// that is the same disclosed gap as [`mob_griefing`] just above: this crate
/// has **no world-level `GameRules` registry** (R1 of the world-state plan).
/// The nearest thing, `crate::server::WorldAdminState`'s `game_rules`, is
/// per-*connection* state owned by `serve_play` — the wrong side of the
/// world for a tick loop that runs with no connection at all. When R1 lands
/// a world-level `GameRules` every connection shares, this function is the
/// only call site to change.
///
/// Returning the default is the conservative choice for the *observable*
/// behaviour: vanilla ships `advanceWeather` on, so rain and thunder actually
/// cycle, and modelling it as off would freeze the sky behind a rule nobody
/// can currently turn back on.
fn advance_weather() -> bool {
    true
}

/// Issue #325 / `docs/plans/world-state.md` S1: whether a passed night-skip
/// vote may actually jump the clock. Vanilla's `ServerLevel.advanceTime`
/// (`ServerLevel.java:367-379`) — the rule checked inside `tickSleepingPlayers`
/// after a vote passes — returns true only when the weather has not ended a
/// thunder storm this tick (the world is not allowed to "skip" while a
/// thunderstorm is being resolved).
///
/// Same shape as [`advance_weather`] just above: a function returning a
/// constant is the disclosed gap this crate has **no world-level `GameRules`
/// registry** (R1 of the world-state plan), and when R1 lands a world-level
/// `GameRules`, this function is the only call site to change.
///
/// Returning the default is the conservative choice for the *observable*
/// behaviour: vanilla ships `doDaylightCycle` (and weather resolution) on, so
/// night skips past, and modelling the rule as off would freeze the day
/// forever behind a toggle nobody can currently turn back on.
fn advance_time() -> bool {
    true
}

/// Issue #325 / `docs/plans/world-state.md` S1: the fraction of players whose
/// vote is required to skip the night — the `playersSleepingPercentage` of
/// `ServerLevel`'s `sleepStatus` (`SleepStatus.java`, defaulting to
/// `sleepingPercentage=100` in `ServerLevel.java`'s constructor).
///
/// Another R1-shaped constant: vanilla's `GameRules.PLAYERS_SLEEPING_PERCENTAGE`
/// ships at 100 (every player must be in bed) and is command-tunable. This
/// crate has no world-level game-rules registry (see the R1 gaps above), so
/// 100 is hard-coded and [`SleepState::sleepers_needed`]'s `max(1, …)` floor
/// still makes singleplayer require exactly one sleeper.
fn players_sleeping_percentage() -> u32 {
    100
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
///
/// `pub(crate)` because it is *observable*: a gate counting column generations
/// over N ticks sees `N - INITIAL_RANDOM_TICK_DEFERRAL_TICKS` random-tick
/// passes, not N. Three gates hardcoded the pre-deferral assumption that every
/// tick is a pass and reported **zero** columns when this landed
/// (`chunk_store`'s pair and `tests/lan_world_tick.rs`). A gate must derive its
/// tick count from this constant rather than restate `40`, so raising the
/// deferral moves the expectations with it instead of silently voiding them.
///
/// # This defers *one* of three `world.column()` callers, not all of them
///
/// This comment used to claim the random-tick pass was "the only thing in this
/// loop that touches `world.column()`". It is not, in two ways, and **the
/// deferral covers neither** — so it is a startup-smoothing measure, not a bound
/// on tick-thread generation:
///
/// * `block_ticks.drain_due` calls `world.column()` directly, from tick 1,
///   *above* the deferral gate.
/// * the block-entity scan calls `world.block_state()` per hopper, also from
///   tick 1 and also above the gate, and `ChunkStore::block_state` regenerates a
///   whole column on an LRU miss. Measured: with retention off, a single remote
///   hopper is a cold column on **every one of 52 ticks, including the 40 this
///   constant covers** — `chunk_store::tests`'
///   `without_retention_a_remote_hopper_is_a_cold_column_every_single_tick`.
///   Past `DEFAULT_CAPACITY` that reaches **610 cold columns per tick**; see
///   `docs/block-entity-tick-distance.md` and issue #503.
///
/// A gate that counts `world.column()` calls over this loop must therefore say
/// which caller it is attributing them to. `chunk_store`'s pair is only clean
/// because it passes an **empty** `BlockEntityHandle`.
pub(crate) const INITIAL_RANDOM_TICK_DEFERRAL_TICKS: u64 = 40;

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
    /// Issue #530: sounds, particles and level events the world tick produced —
    /// see [`crate::effects`].
    ///
    /// **A third lane here rather than a feed of its own**, because a feed is
    /// nine `serve_connection*` signatures wide and an effect is the same kind
    /// of thing as the block update in lane 0: something the world tick did that
    /// this connection has no other way to learn about. Outbound, so it splits
    /// per-connection under [`subscriber`](Self::subscriber) exactly as lane 0
    /// does.
    ///
    /// The `Option<Uuid>` is vanilla's `except` player — the first argument of
    /// `Level.playSound(@Nullable Entity except, …)` (`Level.java:436`) and of
    /// `Level.levelEvent(@Nullable Entity except, …)`. `None` reaches everyone.
    /// Without it the acting player's own break and place sounds could not be
    /// published at all, because the shell predicts them locally and would play
    /// each one twice; see [`drain_effects_for`](Self::drain_effects_for).
    Arc<Mutex<Vec<(Option<uuid::Uuid>, crate::effects::WorldEffect)>>>,
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

    /// Records one world effect (issue #530) for every player to learn about on
    /// their next [`drain_effects_for`](Self::drain_effects_for) — vanilla's
    /// `except == null`.
    pub(crate) fn publish_effect(&self, effect: crate::effects::WorldEffect) {
        self.push_effect(None, effect);
    }

    /// Records one world effect that `except` must **not** receive — vanilla's
    /// `Level.playSound(player, …)`, whose first argument is the player to skip.
    ///
    /// This is what lets a player's own block break and place sounds be
    /// published: the acting client predicts them locally
    /// (`docs/block-sound-types.md`), so it must be excluded while every other
    /// player still hears them.
    pub(crate) fn publish_effect_except(
        &self,
        except: uuid::Uuid,
        effect: crate::effects::WorldEffect,
    ) {
        self.push_effect(Some(except), effect);
    }

    fn push_effect(&self, except: Option<uuid::Uuid>, effect: crate::effects::WorldEffect) {
        self.2
            .lock()
            .expect("block tick feed lock poisoned")
            .push((except, effect));
    }

    /// Drains every world effect published since the last call and returns the
    /// ones `viewer` should hear — single-consumer for the same reason
    /// [`drain_all`](Self::drain_all) is.
    ///
    /// Draining is unconditional; only the *return* is filtered. An effect this
    /// viewer is excluded from is dropped rather than left to accumulate, which
    /// is correct for both shapes this type serves: singleplayer has one
    /// consumer, and each LAN connection owns its own outbound queue behind
    /// `IntegratedServer::bind`'s relay.
    pub fn drain_effects_for(&self, viewer: uuid::Uuid) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut *self.2.lock().expect("block tick feed lock poisoned"))
            .into_iter()
            .filter_map(|(except, effect)| (except != Some(viewer)).then_some(effect))
            .collect()
    }

    /// Drains every world effect with its `except` tag intact, for a fan-out
    /// that must re-publish rather than send — `IntegratedServer::bind`'s relay,
    /// which copies the hub's effects into each connection's own queue and
    /// cannot decide the exclusion on the hub's behalf.
    pub(crate) fn drain_effects_tagged(
        &self,
    ) -> Vec<(Option<uuid::Uuid>, crate::effects::WorldEffect)> {
        std::mem::take(&mut *self.2.lock().expect("block tick feed lock poisoned"))
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
        Self(Arc::default(), Arc::clone(&self.1), Arc::default())
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

/// Applies one [`FallingBlockEffect`](crate::gravity_tick::FallingBlockEffect) to
/// the world and the outbound feed.
///
/// One function for all four variants, and both callers walk their effect list in
/// order — that is the point. The two orderings a player can actually see (clear
/// before spawn, place before discard) are properties of the sequence
/// `crate::mobs::MobSim` returns, and routing every variant through one applier is
/// what stops a caller from reordering them by accident. See
/// `crate::gravity_tick`'s module doc for what each reversal looks like on screen.
///
/// `column` is `Some` only where the caller already holds one (the scheduled-tick
/// drain); passing it keeps the drain's own subsequent reads consistent with the
/// write. The `ChunkSource` write and the client publish happen either way, so a
/// `None` caller loses nothing but that consistency.
///
/// [`Spawned`](crate::gravity_tick::FallingBlockEffect::Spawned) and
/// [`Discarded`](crate::gravity_tick::FallingBlockEffect::Discarded) are
/// deliberately no-ops here: `MobSim` has already inserted or removed the entity,
/// and the wire half is the entity streamer's diff of `snapshots()`. They are
/// still *in* the sequence because their position relative to the block writes is
/// the fact being asserted.
fn apply_falling_block_effect<W: ChunkSource>(
    world: &W,
    out: &BlockTickFeed,
    column: Option<(&mut crate::chunk::ChunkColumn, i32, i32)>,
    effect: &crate::gravity_tick::FallingBlockEffect,
) {
    use crate::gravity_tick::FallingBlockEffect;
    let (pos, state) = match effect {
        FallingBlockEffect::ClearedOrigin { pos, .. } => (*pos, crate::chunk::AIR.to_string()),
        FallingBlockEffect::Placed { pos, state, .. } => (*pos, state.clone()),
        FallingBlockEffect::Spawned { .. } | FallingBlockEffect::Discarded { .. } => return,
    };
    if let Some((column, min_x, min_z)) = column {
        column.set_block(pos.x - min_x, pos.y, pos.z - min_z, &state);
    }
    world.set_block(pos.x, pos.y, pos.z, &state);
    out.publish(pos.x, pos.y, pos.z, state);
}

/// Publishes the open/close sound for a state transition, if it was one (issue
/// #530). A no-op for every other block, so call sites need no guard of their own.
///
/// `game_tick` stands in for vanilla's `random.nextFloat() * 0.1F + 0.9F` pitch
/// draw: this loop's per-tick RNG is owned by the random-tick scheduler and a
/// door sound must not consume from it (the draw *sequence* is what
/// `crate::random_tick`'s parity gates pin). Cycling the pitch over the tick
/// counter keeps the audible variation without touching that sequence.
fn publish_openable_sound(out: &BlockTickFeed, pos: BlockPos, from: &str, to: &str, game_tick: u64) {
    let pitch = 0.9 + (game_tick % 11) as f32 * 0.01;
    if let Some(effect) = crate::effects::openable_toggled(pos, from, to, pitch) {
        out.publish_effect(effect);
    }
}

/// Publishes the moving block entity for a cell a piston move just filled.
///
/// A `moving_piston` block state is `INVISIBLE` and says nothing about *which*
/// block is travelling through it, so a client that receives only the block update
/// has a cell it knows is animating and no geometry to animate. The record it needs
/// is read back out of the pending commit tick at that same cell — the pending tick
/// *is* this crate's `PistonMovingBlockEntity`; see `crate::piston::finish_kind` for
/// why it lives there rather than in a block-entity map.
///
/// **The wire ordering this depends on is the drain order in `crate::server`**, not
/// the call order here: block changes are drained before the effect lane, so the
/// `block_update` that establishes the `moving_piston` state always precedes this
/// record even though a caller publishes them the other way round. That matters
/// because the client's own `sync_block_entity` creates the record the state write
/// implies, and this then fills it in.
///
/// A no-op for any other state, so a caller can hand it every block change it
/// publishes without testing first.
fn publish_moving_piston(
    out: &BlockTickFeed,
    block_ticks: &crate::scheduled_tick::ScheduledTickQueue<String>,
    x: i32,
    y: i32,
    z: i32,
    state: &str,
) {
    if !crate::piston::is_moving_piston(state) {
        return;
    }
    let Some(entity) = block_ticks
        .iter()
        .find(|pending| pending.pos == (x, y, z) && crate::piston::is_finish_kind(&pending.kind))
        .and_then(|pending| crate::piston::parse_finish_kind(&pending.kind))
    else {
        return;
    };
    out.publish_effect(crate::effects::WorldEffect::BlockEntityData {
        pos: BlockPos::new(x, y, z),
        block_entity_type: crate::piston::PISTON_BLOCK_ENTITY.to_string(),
        nbt: crate::block_entities::moving_piston_nbt(&entity),
    });
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
    scheduled: crate::region_source::ScheduledTickHandle,
    // Which dimension this loop serves and where its players are, so `tick_area`
    // above becomes a *fallback* rather than the whole story. `TickFollow::default()`
    // carries an empty anchor set and therefore reproduces the fixed-area behaviour
    // exactly — see `crate::tick_area`.
    follow: crate::tick_area::TickFollow,
) where
    W: ChunkSource,
{
    // Issue #324 / `docs/plans/world-state.md` W1. Forwards to the real body
    // with a fresh, permanently-drained-by-nobody [`WeatherFeed`] — the same
    // compatibility shape every `serve_connection*` wrapper uses for a
    // feed it does not carry, and for the same reason: the world-loop's
    // non-weather callers (`crate::chunk_store`'s gate,
    // `crate::redstone_placement_gate`, and this module's own tests) are not
    // this issue's to edit, so the weather feed is additive rather than a
    // tenth parameter. The weather *still advances* here — `WeatherState` is
    // ticked either way — it just publishes into a feed no connection reads.
    // Production (`crate::IntegratedServer::open_in_memory_with_mobs`, and
    // `bind` since #439) calls the `_with_weather` variant with a real feed.
    //
    // The same applies to the night-skip vote (issue #325): a fresh
    // [`SleepVote`] and [`SleepFeed`] no connection reads. The vote's
    // arithmetic still runs here — a `SleepState` is ticked either way, so
    // the loop shape is identical — it just never passes. This is the loop
    // `bind`'s LAN worlds run on, which is why LAN does not yet skip the
    // night: the LAN connection relays world-global feeds, but a vote whose
    // `lay_down`/`get_up` arms no connection calls is structurally
    // disconnected, and wiring it is a separate LAN pass (see
    // `crate::sleep`'s module doc).
    run_tick_loop_with_weather(
        mobs,
        mob_out,
        block_entities,
        clock,
        world,
        block_tick_out,
        tick_area,
        explosion_out,
        WeatherFeed::default(),
        WeatherState::default(),
        // Issue #325: see the wrapper doc above.
        &SleepVote::new(),
        &SleepFeed::default(),
        scheduled,
        // A fresh, unshared world state — the same compatibility shape as the
        // weather feed above. Rules still *apply* here (this loop reads them
        // every tick), they are just at their defaults with nothing able to
        // change them, which is exactly the behaviour before #327.
        crate::world_state::WorldStateHandle::default(),
        follow,
    )
    .await
}

/// The real body shared by [`run_tick_loop`] and
/// [`run_tick_loop_with_weather`] — the latter only differs in that it
/// carries a real [`WeatherFeed`] the connection drains instead of the
/// wrapper's discarded default. See the wrapper's own doc comment for why a
/// second, differently-named function exists instead of adding `weather_out`
/// to [`run_tick_loop`]'s own signature (it would break the world-loop's
/// non-weather call sites in `crate::chunk_store`/`crate::redstone_placement_gate`,
/// which are not this issue's to edit). Issue #325's [`SleepVote`]/[`SleepFeed`]
/// follow the same `_with_weather` shape for the same reason, so the sleep
/// wiring lives here too.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_tick_loop_with_weather<W>(
    mobs: MobHandle,
    mob_out: LiveMobSource,
    block_entities: BlockEntityHandle,
    clock: Arc<TickClock>,
    world: Arc<W>,
    block_tick_out: BlockTickFeed,
    tick_area: (RangeInclusive<i32>, RangeInclusive<i32>),
    explosion_out: ExplosionFeed,
    // Issue #324 / `docs/plans/world-state.md` W1: the weather transitions
    // this loop publishes into (`WeatherState::tick`'s return), drained by
    // the connection — `serve_play`'s `container_sync_tick` arm — into real
    // `GAME_EVENT` packets. Same single-consumer snapshot-feed shape as
    // `block_tick_out`/`explosion_out`, so it is safe for the one connection
    // `open_in_memory_with_mobs` spawns and is fanned out per-connection by
    // `bind`'s LAN relay.
    weather_out: WeatherFeed,
    // The weather machine, seeded by the caller and owned by this loop from
    // here on (a plain struct, with no lock — see `crate::weather`'s module
    // doc). Production passes `WeatherState::default()`; passing it in rather
    // than constructing it here is what lets a world seed drive the cycle
    // when #437 lands a per-world seed store, and what lets this module's own
    // test start a world already mid-cycle instead of waiting out a
    // 12k-180k-tick rain delay.
    weather: WeatherState,
    // Issue #325 / `docs/plans/world-state.md` S1: the night-skip vote.
    // `sleep_vote` is the shared roster and voter count — connections call
    // `lay_down`/`get_up` on it (the `UseItemOn` bed arm and the
    // `PlayerCommand` arm in `server.rs`) — and this loop reads it via
    // `snapshot()` once per tick, folding it into a loop-owned
    // [`SleepState`] (see below). The same single-consumer snapshot shape as
    // `weather_out` makes it safe for the one connection
    // `open_in_memory_with_mobs` spawns; a vote the wrapper's discarded
    // default (no connection calls) can never pass.
    sleep_vote: &SleepVote,
    // Issue #325: where a passed vote publishes its [`SleepEvent::SkippedNight`]
    // broadcast, drained by the connection — `serve_play`'s
    // `container_sync_tick` arm — into a real `encode_set_time` so the
    // client's day clock jumps to the morning. Snapshot-feed, like
    // `weather_out`, with the same single-consumer caveat.
    sleep_feed: &SleepFeed,
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
    // Issues #327/#328/#323. The world's shared game rules, difficulty and clock
    // — the same handle every connection reads (see `crate::world_state`). Four
    // of this module's own constant-returning stubs (`mob_griefing`,
    // `advance_weather`, `advance_time`, `players_sleeping_percentage`) existed
    // *because* there was no registry here; three of them now read this instead,
    // and the clock is no longer two locals.
    world_state: crate::world_state::WorldStateHandle,
    // See [`run_tick_loop`]'s own parameter comment: the dimension this loop serves
    // plus the shared player-anchor set, which together turn `tick_area` from the
    // whole simulated world into a fallback for when no player is in it.
    follow: crate::tick_area::TickFollow,
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
    // Issue #325 / `docs/plans/world-state.md` S1: the day clock, advanced one
    // per tick in lockstep with `game_tick` until a night skip jumps it — the
    // `dayTime` counter of vanilla's `ServerLevel.tickTime` (which increments
    // both `gameTime` and `dayTime` as two counters). Owned by this thread with
    // no lock, exactly like `game_tick`. `i64` because the night skip lands on
    // `SleepState::morning_after`'s multiples of `DAY_LENGTH_TICKS`.
    let mut day_time: i64 = 0;
    // Issue #324 / `docs/plans/world-state.md` W1: the weather cycle, owned
    // by the tick thread with no lock, exactly like `game_tick`/`block_ticks`
    // — the plain-struct shape the ECS migration (shape A) turns into a
    // `Resource` mechanically later. Seeded by the caller (see the parameter
    // comment); this binding is what makes it mutable for the loop.
    let mut weather = weather;
    // Issue #325 / `docs/plans/world-state.md` S1: the night-skip vote's
    // state, owned by this loop with no lock, exactly like `weather` — the
    // shared [`SleepVote`] holds the roster, but who has been *deep* asleep is
    // measured here against this thread's own `game_tick`, and the loop is
    // what decides a pass (see `crate::sleep`'s module doc).
    let mut sleep_state = SleepState::default();
    // #308: one queue per vanilla queue (`ServerLevel.blockTicks`/
    // `fluidTicks`, `ServerLevel.java:209-210`). Owned by `scheduled` rather
    // than by this function since #468, which is what lets them be saved; they
    // are borrowed out of it once per tick below.
    // #307.
    let mut random_ticks = RandomTickScheduler::new(RANDOM_TICK_POSITION_SEED, RANDOM_TICK_BEHAVIOR_SEED);
    // `crate::fluid`'s per-dimension constants, resolved **lazily** on the first
    // fluid tick rather than here.
    //
    // The vertical extent is the load-bearing field and it has to come from a
    // real column, not from `FluidEnv::OVERWORLD`'s 26.2 literals: fluid spread
    // reads the cell below whatever it looks at, so a fluid on the floor of the
    // world asks for `min_y - 1`, and `ChunkColumn::block_state` panics there.
    // Every test double in this crate is shorter than 384 rows.
    //
    // Lazy because #481's whole point is that a `world.column()` call before the
    // background seeding task has run costs a full generation on this thread. A
    // world with no fluid ticks never pays for this at all, and one that has them
    // pays a single column clone — the same cost the block drain above already
    // pays *per due tick*.
    let mut fluid_env: Option<crate::fluid::FluidEnv> = None;
    // `crate::fire`'s behaviour stream, and its lazily-resolved vertical extent.
    //
    // One stream for the whole world, not one per fire block: `FireBlock::tick`
    // draws from `level.random`, which is shared across every block tick in
    // vanilla too, so a per-cell RNG would produce a *different* world rather
    // than a more deterministic one. The draw count per tick is the
    // specification (`crate::fire`'s own tests gate it against a reference
    // generator), and that only holds if the arm below is the single consumer.
    //
    // The extent is lazy for exactly `fluid_env`'s reason above, and it matters
    // more here: `fire::block_at` answers air outside build height, so a fire on
    // the world floor reading `min_y - 1` needs the real bounds rather than
    // `FireEnv::OVERWORLD`'s 26.2 literals, which no test double matches.
    let mut fire_rng = crate::mob_spawn::SpawnRng::new(crate::fire::FIRE_BEHAVIOR_SEED);
    let mut fire_env: Option<(i32, i32)> = None;
    let mut fire_changes: Vec<(BlockPos, String)> = Vec::new();
    let mut fire_primed_tnt: Vec<BlockPos> = Vec::new();
    // `crate::explosion_blocks`' behaviour stream, drawn once per ray of a blast
    // (1,352 rays) plus one per `createFire` candidate. Separate from `fire_rng`
    // so a blast cannot shift the fire stream, and from `random_ticks`' own
    // behaviour seed for the same reason.
    let mut blast_rng =
        crate::mob_spawn::SpawnRng::new(crate::explosion_blocks::EXPLOSION_BEHAVIOR_SEED);
    // The blast's *loot* stream, deliberately separate from `blast_rng`: that one
    // feeds the ray march, whose per-ray draw count `explosion_blocks` gates
    // exactly. Sharing one stream would make the crater shape depend on how many
    // items happened to drop.
    let mut blast_drops_rng = crate::mob_spawn::SpawnRng::new(
        crate::explosion_blocks::EXPLOSION_BEHAVIOR_SEED ^ 0x100D_5EED,
    );
    // `crate::redstone_dispenser`'s slot pick (`random_slot`) and toss math
    // (`plain_toss`) — one stream for both, matching vanilla's single
    // `RandomSource` per level: `DispenserBlockEntity.getRandomSlot` and
    // `DefaultDispenseItemBehavior.execute` draw from the same generator.
    let mut dispenser_rng =
        crate::mob_spawn::SpawnRng::new(crate::redstone_dispenser::DISPENSER_BEHAVIOR_SEED);
    let (tick_cx_range, tick_cz_range) = tick_area;
    // **The columns this loop simulates, and they now follow the players.**
    //
    // This used to be two `RangeInclusive`s destructured above and iterated
    // directly, which is what made the whole world tick a 49-column square nailed
    // to chunk (0, 0) — `crate::chunk_store`'s module doc recorded the symptom
    // ("`mob_area` is centred on world spawn and never moves") and natural
    // spawning, random ticks and the fluid queue all inherited it. See
    // `crate::tick_area` for the design, the per-dimension filter and why an empty
    // anchor set deliberately falls back to exactly the square the caller passed.
    //
    // The two ranges above are still read: they *are* that fallback, and every
    // playerless caller of this loop (`crate::chunk_store`'s memory gates,
    // `crate::redstone_placement_gate`, this module's own tests) depends on it.
    let mut area = crate::tick_area::FollowArea::new(
        follow,
        tick_cx_range.clone(),
        tick_cz_range.clone(),
    );
    // The terrain view the natural spawner reads, rebuilt only when `area` moves
    // (or on the staleness cadence below) rather than per tick — see
    // `FollowArea::snapshot_terrain` for why that gate is the whole cost story.
    // `None` until the first cycle needs one, so a loop with `spawn_mobs` off
    // never pays for a snapshot at all.
    let mut spawn_terrain: Option<std::sync::Arc<crate::mobs::ChunkWorld>> = None;
    let mut spawn_terrain_built_at: u64 = 0;
    // Issues #221/#222: the natural-spawn driver. Long-lived rather than built
    // per tick because it owns the per-column light cache — see
    // `crate::natural_spawn`'s module doc for the per-cycle budget and the TTL
    // that keep a 49-column area inside the 50 ms tick budget.
    //
    // `with_world_seed` is a *different* seed from `NATURAL_SPAWN_SEED` and both
    // are needed: the literal seeds the spawn stream (reproducible is all that is
    // asked of it), while the world seed decides which chunks are **slime
    // chunks**, which is not free to choose. See
    // `crate::worldgen_data::active_world_seed` for why the world seed arrives
    // through a global rather than as a parameter here.
    let mut natural_spawner = crate::natural_spawn::NaturalSpawner::new(
        crate::worldgen_data::bundled_biome_spawners().clone(),
        NATURAL_SPAWN_SEED,
    )
    .with_world_seed(crate::worldgen_data::active_world_seed());
    let mut despawn_rng = crate::mob_spawn::SpawnRng::new(NATURAL_SPAWN_SEED ^ 0x5DEE_C0DE);
    // Issue #326 / `docs/plans/world-state.md` B1: the world border, ticked
    // first each loop (per `ServerLevel.tick`'s order) and owned by this
    // thread with no lock, exactly like `weather`/`game_tick`. A **static
    // default today** — nothing calls `lerp_size_between`/`set_size` yet, so
    // the loop ticks an inert border and every connection joins against the
    // same full-size default (see `crate::border`'s module doc for why both
    // halves agree and what shape B deletes). The resize entry point is
    // `BorderFeed::with`; wiring it to the world loop is the follow-up this
    // landing deliberately does not claim.
    let mut border = WorldBorder::default();

    loop {
        let now = tokio::time::Instant::now();
        let (adjusted_next, adjusted_warning, overload) =
            resolve_overload(now, next_tick_at, last_overload_warning_at);
        next_tick_at = adjusted_next;
        last_overload_warning_at = adjusted_warning;
        if let Some(event) = overload {
            // The item-settling pass's own cost rides along, because it is the one
            // thing in this loop whose work scales with something a *player* controls
            // and it is otherwise invisible here: `MobSim` settles every dropped item
            // through swept collision against real per-block-state shapes, so a floor
            // covered in drops does strictly more work per tick than the one boolean
            // lookup this used to be. Measured at 36 cell probes per item per tick, so
            // a four-figure number here means items are a real share of the overrun
            // and a five-figure one means they are most of it.
            //
            // Read from the *previous* tick (this runs before the body), which is
            // exactly the tick that ran long. `serve_play`'s own `LoopStallWatch` does
            // not cover this — that watches the connection task's `select!` arms, and
            // this is a different task entirely.
            let item_probes = mobs.with(|sim| sim.items_settled_probe_count());
            tracing::warn!(
                ticks_behind = event.ticks_behind,
                behind_ms = event.behind_ms,
                item_settle_probes = item_probes,
                "Can't keep up! Is the server overloaded? Running {}ms or {} ticks behind",
                event.behind_ms,
                event.ticks_behind,
            );
            clock.record_overrun();
        }

        next_tick_at += TICK_PERIOD;
        tokio::time::sleep_until(next_tick_at).await;

        let tick_start = tokio::time::Instant::now();
        // **Where the world is ticking this tick.** Integer arithmetic over at most
        // a few dozen coordinate pairs, so it is affordable every tick; the
        // expensive half (the terrain snapshot below) is gated on the `true` this
        // returns, which is what keeps a chunk-boundary crossing from putting a
        // whole area's worth of column fetches inside one unserviced window.
        let area_moved = area.recompute();
        // Issue #326 B1: border ticks first, per `ServerLevel.tick`'s order
        // (`WorldBorder.tick` then the rest of the level's tick). Inert today —
        // a static full-size default — but this is where a resize's lerp
        // advances once a caller exists to start one.
        border.tick();
        // **Dropped items settle against the live world, not the sim's snapshot.**
        //
        // `MobSim::tick` would use `MobSim`'s own `ChunkWorld`, which is a static
        // 7×7-column snapshot of `mob_area` taken once when the world opened
        // (`MobHandle::reseed`). Everywhere outside those columns its `is_solid`
        // answers `false` for every cell — the column is absent, not empty — so a
        // dropped item accelerated downward forever, phased through the terrain,
        // and was discarded at `min_y - 64`. Inside them it answered from
        // *unedited worldgen*, so a block the player had placed did not stop an
        // item and one they had mined still did.
        //
        // This loop is the only place holding the live `ChunkSource`, which is why
        // the oracle is supplied from here rather than fixed inside the sim. Same
        // structural reason the graze and detonation drains below live here:
        // `MobSim` holds its world immutably and cannot see an edit.
        //
        // `is_solid` is `ChunkColumn::is_solid`'s predicate — neither air nor a
        // fluid — re-derived from the source's block state so an item lands on a
        // slab or a chest and falls through water, exactly as the snapshot arm
        // did. `block_state` reads through the resident `ChunkStore` column, so
        // the cells an item is falling through are the ones already streamed to
        // the player standing there.
        {
            let world = Arc::clone(&world);
            mobs.with(|sim| {
                // The **block-state name**, not a solid/air bit. One bit per cell
                // cannot express the shape an item comes to rest on: a bottom slab,
                // soul sand and a patch of short grass all answered "solid" and all
                // settled the item at the top of the cell, so an item on any grassy
                // surface floated a full block above the ground. `MobSim` resolves
                // the name against the real 26.2 shape census — see
                // `mobs::ItemCollision`.
                sim.tick_with_terrain(&|x, y, z| world.block_state(x, y, z));
            });
        }
        // Issue #328's first real enforcement: **Peaceful removes monsters.**
        // Vanilla does it in `Mob.checkDespawn`, which discards any
        // `MobCategory.MONSTER` entity when
        // `level.getDifficulty() == Difficulty.PEACEFUL`; a difficulty that is
        // stored, broadcast and read by nothing is what #328 reported.
        //
        // Also the `mob_drops` rule's carrier: `MobSim` has no handle on the world
        // store (it is version-free and holds only a `ChunkWorld`), so the loop
        // hands it the flag each tick rather than the store. One bool copy.
        let peaceful = !world_state.monsters_may_spawn();
        let mob_drops = world_state.mob_drops();
        mobs.with(|sim| {
            sim.set_mob_drops(mob_drops);
            if peaceful {
                sim.remove_monsters();
            }
        });
        // Issues #221/#222: **the natural spawn cycle, and the despawn pass.**
        // Both engines were complete and driverless — `MobSim::run_spawn_cycle`
        // and `MobSim::despawn_pass` had no production caller at all, so a world
        // held exactly the mobs `seed_demo_mobs` put in it, forever.
        //
        // Gated on the `spawn_mobs` game rule, which is what that rule's
        // accessor was waiting for (`docs/world-state.md` said so explicitly).
        // Peaceful is handled a few lines up by `remove_monsters`, so a monster
        // spawned here is evicted next tick rather than never proposed —
        // vanilla's own order, and it keeps the RNG stream independent of
        // difficulty.
        if world_state.spawn_mobs() {
            let players: Vec<lodestone_model::Vec3> =
                mobs.with(|sim| sim.players().iter().map(|p| p.perception.position).collect());
            if !players.is_empty() {
                // **The terrain the spawn cycle runs against, and it now follows the
                // player.** This used to be `mobs.with(|sim| sim.world())` — the
                // sim's own leaked `ChunkWorld`, a 49-column snapshot of `mob_area`
                // taken once at world open and never moved. Outside those columns
                // `column()` returns `None`, so `random_pos_within` found no surface
                // and `cluster` returned an empty vec: **natural spawning stopped
                // working entirely once the player walked out of the origin box**,
                // which is what an earlier agent measured reaching the wire and
                // could not fix from where it was.
                //
                // Rebuilt only when the area moved, or once per `LIGHT_TTL_TICKS` so
                // player edits eventually appear — the same cadence the spawner
                // already drops its light cache on, which is deliberate: a fresh
                // snapshot with a stale light cache would light cells from terrain
                // that is no longer there. Everything else in this loop reads the
                // live `ChunkSource` per column already; only this one wants a view
                // that is stable for the duration of a cycle.
                let stale = game_tick.saturating_sub(spawn_terrain_built_at)
                    >= crate::natural_spawn::LIGHT_TTL_TICKS;
                if area_moved || stale || spawn_terrain.is_none() {
                    spawn_terrain = Some(area.snapshot_terrain(&*world));
                    spawn_terrain_built_at = game_tick;
                }
                // `expect` over a second `if let`: the branch above assigns `Some`
                // whenever it is `None`, so this cannot fail, and unwrapping here
                // keeps the failure loud rather than silently skipping a cycle.
                let spawn_world = std::sync::Arc::clone(
                    spawn_terrain
                        .as_ref()
                        .expect("the branch above assigns a terrain view when none exists"),
                );
                // The moon phase, which in 26.2 is the whole of
                // `SURFACE_SLIME_SPAWN_CHANCE` (0.0 at new moon, 0.5 at full) —
                // see `NaturalSpawner::surface_slime_spawn_chance`. This loop's own
                // lock-free `day_time` mirror rather than `world_state.time()`, so
                // it costs nothing per tick; it is one tick stale here because
                // `tick_time()` runs further down, and one tick cannot move a
                // 24 000-tick phase boundary. This is also the mirror's first
                // *reader* — until now it was written and never read.
                natural_spawner.set_day_time(day_time);
                // `SpawnPlacements.checkSpawnRules`' peaceful guard. The
                // `remove_monsters` sweep a few lines up is the *other* half and
                // is not a substitute: without this, a monster proposed on
                // Peaceful is published in this tick's snapshot set and evicted in
                // the next, so the client receives an `ADD_ENTITY` followed by a
                // `REMOVE_ENTITIES` and the player sees it blink. Read from the
                // store rather than reusing `peaceful` above so the two cannot
                // drift; both are one bool copy per tick.
                natural_spawner.set_difficulty(world_state.difficulty().0);
                natural_spawner.begin_cycle(spawn_world, game_tick, players.clone());
                mobs.with(|sim| {
                    // Vanilla's `spawnableChunkCount` for the cap formula, read off
                    // the area actually simulated rather than a constant: `MAGIC_NUMBER`
                    // (289) worth of chunks yields caps equal to the per-chunk maxima,
                    // so a smaller follow area scales every category cap down with it.
                    let mut state = sim.census(area.spawnable_chunks());
                    sim.run_spawn_cycle(&mut state, &mut natural_spawner, area.chunks());
                });
            }
            // Nearest-player despawn runs whether or not anything spawned: it is
            // the other half of the same accounting, and vanilla runs it every
            // tick from `Mob.checkDespawn`.
            let nearest = mobs.with(|sim| sim.players().first().map(|p| p.perception.position));
            mobs.with(|sim| sim.despawn_pass(nearest, &mut despawn_rng));
        }
        // Issue #241a: pillager patrols. Same live, player-following terrain
        // snapshot the natural spawner above used — see
        // `MobSim::run_patrol_spawn_cycle`'s own doc comment for why it must
        // not be `MobSim`'s own static `self.world`. Called every tick
        // regardless of `players.is_empty()` above (unlike the natural-spawn
        // block, which is gated on a nonempty player list) because vanilla's
        // own `PatrolSpawner.tick` decrements its countdown every world tick
        // with no such gate, and skipping calls here would make patrols
        // rarer than vanilla rather than merely checked less often.
        //
        // `is_bright_outside` is vanilla's `ServerLevel.isBrightOutside()`
        // simplified to "daytime", ignoring thunder — no weather state
        // crosses this seam. `day_time` is this loop's own tracked mirror,
        // already read above for `natural_spawner.set_day_time`.
        //
        // `spawn_terrain` is only ever `Some` once the natural-spawn block
        // above has run at least once (built lazily on first need). If it is
        // still `None` — `spawn_mobs` is off, or no players are connected
        // yet — patrols simply do not attempt that tick: a real, disclosed
        // divergence from vanilla (which shares no such cache), and a
        // narrower gate than `spawn_patrols` alone.
        let is_bright_outside = day_time.rem_euclid(24000) < 12000;
        if let Some(terrain) = spawn_terrain.as_ref() {
            let spawn_world = std::sync::Arc::clone(terrain);
            mobs.with(|sim| {
                sim.run_patrol_spawn_cycle(
                    &spawn_world,
                    world_state.spawn_patrols(),
                    is_bright_outside,
                    world_state.difficulty().0,
                );
            });
        }
        // Issue #240: the wandering trader spawn cycle. Same live,
        // player-following terrain snapshot and the same "only once
        // `spawn_terrain` exists" gate as the patrol block just above, for
        // the same reason — see `MobSim::run_wandering_trader_spawn_cycle`'s
        // own doc comment. Called every tick `spawn_terrain` is available,
        // matching vanilla's own `CustomSpawner.tick`, which decrements its
        // countdown unconditionally.
        if let Some(terrain) = spawn_terrain.as_ref() {
            let spawn_world = std::sync::Arc::clone(terrain);
            mobs.with(|sim| {
                sim.run_wandering_trader_spawn_cycle(
                    &spawn_world,
                    world_state.spawn_wandering_traders(),
                );
            });
        }
        mob_out.publish(mobs.with(|sim| sim.snapshots()));
        // Issue #425: `MobSim::tick` already calls `MobSim::explode` the
        // tick a creeper's own fuse completes (`1feed17`/`614acb8`), but
        // until now nothing read the detonation back out of the sim — see
        // `ExplosionFeed`'s own doc comment just above for why this is the
        // one production path that turns "a creeper detonated" into an
        // `EXPLODE` packet reaching a connection at all.
        for detonation in mobs.with(MobSim::take_detonations) {
            // The block half of the blast (`crate::explosion_blocks`), run before
            // the `EXPLODE` packet so the crater and the packet land in the same
            // tick. `destroy_blocks` writes air through `world` itself and hands
            // back only the cells that actually changed.
            //
            // Vanilla runs `calculateExplodedPositions` *before* `hurtEntities`,
            // and `MobSim::explode` has already done the entity half by the time
            // a detonation reaches this drain — the swap is unobservable, because
            // entity exposure ray-casts against the collision world for line of
            // sight rather than against the destroyed set.
            //
            // Drops go through `block_drops`, which rolls each destroyed block's
            // table with the radius in the loot context. That parameter is what
            // makes `survives_explosion` keep `1/radius` of the crater instead of
            // passing unconditionally — without it a creeper would drop *every*
            // block it destroyed, which is why nothing dropped at all until the
            // parameter existed.
            //
            // `mob_drops` is the wrong rule to gate this on (a blast is not a mob
            // death); `block_drops` is the one vanilla's `destroyBlock` path
            // consults, so it gates here too.
            let probe = world.column(
                (detonation.centre.x.floor() as i32).div_euclid(16),
                (detonation.centre.z.floor() as i32).div_euclid(16),
            );
            let env = crate::explosion_blocks::BlastEnv::in_column(probe.min_y, probe.height);
            let (changes, popped, primed_tnt) = crate::block_drops::drop_explosion_loot_in_blast(
                &*world,
                env,
                detonation.centre,
                detonation.radius,
                crate::block_drops::bundled_tables(),
                &mut blast_rng,
                &mut blast_drops_rng,
            );
            for (at, new_state) in changes {
                block_tick_out.publish(at.x, at.y, at.z, new_state);
            }
            if world_state.block_drops() {
                mobs.with(|sim| {
                    for drop in popped {
                        let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
                        sim.spawn_item(
                            drop.stack.item.clone(),
                            drop.position,
                            drop.velocity,
                            lodestone_entity::ItemLifecycle::newly_dropped(
                                count,
                                lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                            ),
                        );
                    }
                });
            }
            // `TntBlock::wasExploded` — chain reaction. Gated on `TNT_EXPLODES`
            // the same way `crate::fire`'s TNT arm below is, matching vanilla's
            // own `if (level.getGameRules().get(GameRules.TNT_EXPLODES))` guard
            // in `TntBlock::prime`.
            if world_state.tnt_explodes() {
                mobs.with(|sim| {
                    for pos in primed_tnt {
                        sim.spawn_tnt_short_fuse(lodestone_model::Vec3::new(
                            f64::from(pos.x) + 0.5,
                            f64::from(pos.y),
                            f64::from(pos.z) + 0.5,
                        ));
                    }
                });
            }
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
            if !world_state.mob_griefing() {
                continue;
            }
            let (target, state) = match eaten {
                EatenBlock::AtFeet => (pos, "minecraft:air"),
                EatenBlock::Below => (BlockPos::new(pos.x, pos.y - 1, pos.z), "minecraft:dirt"),
            };
            // Issue #530: `EatBlockGoal` sends level event 2001 for the break
            // particles (`crate::mobs::MobSim::take_grazes`'s own doc says so),
            // which is a server-caused effect no client predicts. The *old* state
            // is what the particles are made of, so it is read before the write.
            let broken = world.block_state(target.x, target.y, target.z);
            if let Some(effect) = crate::effects::block_destroyed(target, &broken) {
                block_tick_out.publish_effect(effect);
            }
            world.set_block(target.x, target.y, target.z, state);
            block_tick_out.publish(target.x, target.y, target.z, state.to_owned());
        }
        // Issue #530: mob hurt and death sounds. `MobSim::apply_damage` already
        // damaged and killed mobs with no audible result at all — the sim records
        // the vocalisation for the same reason it records a detonation (it holds
        // the world immutably and owns no connection).
        for effect in mobs.with(MobSim::take_vocalisations) {
            block_tick_out.publish_effect(effect);
        }
        // Issue #322: target-block projectile impacts. Drained here (outside
        // the `scheduled.with` region below) because `MobSim` is the only
        // thing that saw the hit; resolved *inside* it further down because a
        // target's power write needs `block_ticks` (for
        // `redstone_target::apply_hit`'s `has_pending_decay` guard and to
        // schedule the decay) and the live `world`, neither of which `MobSim`
        // holds — see `crate::mobs::ProjectileBlockHit`'s own doc comment.
        let projectile_block_hits = mobs.with(MobSim::take_projectile_block_hits);
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
        //
        // Issue #504: `is_loaded` gates the scan by chunk residency *before*
        // `enabled` ever reaches `world.block_state` — `ChunkStore::block_state`
        // regenerates a whole column on a miss, and this closure used to run
        // that for every registered hopper, every tick, forever (the registry
        // has no eviction). `is_column_resident` answers with no generation at
        // all, so a hopper outside every loaded chunk now costs a `HashMap`
        // lookup instead of a worldgen call.
        block_entities.with(|registry| {
            registry.tick_all_with_hopper_lock(
                &|pos| world.is_column_resident(pos.x.div_euclid(16), pos.z.div_euclid(16)),
                &|pos| crate::redstone::hopper_enabled(&world.block_state(pos.x, pos.y, pos.z)),
            );
        });

        // Issue #323. The clock is the **world's**, not this loop's: one
        // `tick_time` advances `game_time` unconditionally and `day_time` only
        // under the `advance_time` rule (`ServerLevel.tickTime`, where `setDayTime`
        // is gated and `gameTime` is not). The locals below are still the loop's
        // arithmetic, but they are *sourced* here rather than incremented — which
        // is the whole of #323's fix, because the connection's periodic
        // `encode_set_time` now reads the same store instead of wall-clock
        // elapsed-since-join.
        let world_time = world_state.tick_time();
        game_tick = world_time.game_time.max(0) as u64;
        day_time = world_time.day_time;
        // `/weather`'s consumer half: a caller-side request queued on
        // `WorldStateHandle` (the same handle this loop already reads for
        // `tick_time` above — see `crate::world_state::WeatherRequest`'s own
        // doc for why a request queue rather than a direct write). Applied
        // **before** this tick's own `weather.tick` call, and directly to the
        // `pub(crate)` fields rather than through a method, so the flip is
        // immediate — `weather.tick`'s own transition detection compares
        // against whatever `raining`/`thundering` already are when it starts,
        // so a request applied first is what makes the boolean flip (and its
        // `StartRaining`/`StopRaining` event) land on *this* tick instead of
        // never, since nothing would otherwise notice a value that was
        // already set going in.
        if let Some(request) = world_state.take_weather_request() {
            let was_raining = weather.raining;
            match request {
                crate::world_state::WeatherRequest::Clear { duration } => {
                    weather.clear_weather_time = duration.max(1);
                    weather.raining = false;
                    weather.thundering = false;
                }
                crate::world_state::WeatherRequest::Rain { duration } => {
                    weather.clear_weather_time = 0;
                    weather.rain_time = duration.max(1);
                    weather.thunder_time = duration.max(1);
                    weather.raining = true;
                    weather.thundering = false;
                }
                crate::world_state::WeatherRequest::Thunder { duration } => {
                    weather.clear_weather_time = 0;
                    weather.rain_time = duration.max(1);
                    weather.thunder_time = duration.max(1);
                    weather.raining = true;
                    weather.thundering = true;
                }
            }
            if was_raining != weather.raining {
                weather_out.publish(if weather.raining {
                    crate::weather::WeatherEvent::StartRaining
                } else {
                    crate::weather::WeatherEvent::StopRaining
                });
            }
        }
        // Issue #324 / `docs/plans/world-state.md` W1: the weather cycle is
        // world-global state, so it belongs to the world tick (not to any
        // connection — the straddle the world-state plan's migration exists
        // to delete). `advance_weather()` stands in for the R1 game rule.
        for event in weather.tick(advance_weather()) {
            weather_out.publish(event);
        }
        // Issue #325 / `docs/plans/world-state.md` S1: the night-skip vote, in
        // vanilla's own position — `ServerLevel.tick` runs
        // `tickSleepingPlayers` right after the weather-cycle timers
        // (`ServerLevel.java:367-379`). Snapshot the shared roster, fold it
        // into the loop-owned [`SleepState`] (recording each sleeper's
        // lay-down tick and dropping anyone who woke), then test the vote: at
        // least `sleepers_needed` players asleep, and at least that many deep
        // (`DEEP_SLEEP_TICKS`).
        //
        // On a pass, vanilla's three steps run in order
        // (`ServerLevel.java:367-379`): the clock jumps to the next morning
        // (`moveToTimeMarker`, gated on the `advanceTime` rule standing in for
        // R1); the skip is broadcast so each connection can re-anchor its day
        // clock (`encode_set_time` on the connection side); and every sleeper
        // wakes (`wakeUpAllPlayers` → `SleepStatus.removeAllSleepers`) and the
        // roster is cleared so a day-sleeping click cannot vote again tonight.
        let (active, sleeper_ids) = sleep_vote.snapshot();
        sleep_state.reconcile(&sleeper_ids, game_tick);
        if sleep_state.vote_passes(active, players_sleeping_percentage(), game_tick) {
            if world_state.advance_time() {
                let morning = SleepState::morning_after(day_time);
                day_time = morning;
                // The jump belongs to the world, not to this loop: the next
                // `tick_time` must continue from the morning, and every connection's
                // own broadcast reads the same store.
                world_state.set_day_time(morning);
                sleep_feed.publish(SleepEvent::SkippedNight {
                    game_time: game_tick as i64,
                    morning,
                });
            }
            sleep_state.wake_all();
            sleep_vote.clear();
        }
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
        //
        // Issue: fluid spread rides the **fluid** queue, not the block one, so
        // this loop routes on `kind`. `BlockTickFeed` carries one relative-delay
        // stream because it is one channel from the connection tasks; the split
        // has to happen somewhere, and here is where both queues are in scope.
        // `crate::fluid::TICK_FLUID` is the only kind that goes left.
        for pending in block_tick_out.drain_scheduled_ticks() {
            let queue = if pending.kind == crate::fluid::TICK_FLUID {
                &mut *fluid_ticks
            } else {
                &mut *block_ticks
            };
            if queue.has_scheduled(pending.pos, &pending.kind) {
                continue;
            }
            queue.schedule(
                pending.pos,
                pending.kind,
                game_tick + pending.trigger_tick,
                pending.priority,
            );
        }

        // Issue #322: resolve each target-block hit `MobSim::resolve_projectile_impacts`
        // found this tick — see the drain above for why this has to happen
        // inside this closure rather than there. A hit at a position that is
        // no longer (or never was) a `minecraft:target` by the time this runs
        // is silently dropped, matching every other drain here that re-checks
        // live state rather than trusting a snapshot taken a moment earlier.
        let target_decay_kind = crate::redstone_target::TICK_TARGET_DECAY.to_owned();
        for hit in &projectile_block_hits {
            let state = world.block_state(hit.pos.x, hit.pos.y, hit.pos.z);
            if !crate::redstone::is_target(&state) {
                continue;
            }
            let strength = crate::redstone_target::redstone_strength(
                hit.axis, hit.frac.x, hit.frac.y, hit.frac.z,
            );
            let has_pending_decay =
                block_ticks.has_scheduled((hit.pos.x, hit.pos.y, hit.pos.z), &target_decay_kind);
            let Some(outcome) =
                crate::redstone_target::apply_hit(&state, strength, hit.is_arrow, has_pending_decay)
            else {
                continue;
            };
            world.set_block(hit.pos.x, hit.pos.y, hit.pos.z, &outcome.new_state);
            block_tick_out.publish(hit.pos.x, hit.pos.y, hit.pos.z, outcome.new_state.clone());
            block_ticks.schedule(
                (hit.pos.x, hit.pos.y, hit.pos.z),
                target_decay_kind.clone(),
                game_tick + u64::from(outcome.delay),
                TickPriority::Normal,
            );
            // Neighbour fan-out, matching every other analog-power write in
            // this family (`crate::random_tick::propagate_and_react`) — a
            // target's own doc names this as "none beyond the ordinary
            // fan-out", so this is that ordinary fan-out, not a special case.
            let cx = hit.pos.x.div_euclid(16);
            let cz = hit.pos.z.div_euclid(16);
            let mut column = world.column(cx, cz);
            for event in crate::random_tick::propagate_and_react(
                &mut column, cx * 16, cz * 16, hit.pos.x, hit.pos.y, hit.pos.z, &mut block_ticks, game_tick,
            ) {
                let (ex, ey, ez) = event.pos;
                world.set_block(ex, ey, ez, &event.to);
                block_tick_out.publish(ex, ey, ez, event.to);
            }
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
            // Fire, **before** the column work below and not through it. A fire
            // tick spreads two cells horizontally and four up, so it crosses
            // chunk borders exactly as fluid spread does, and a
            // `ChunkColumn`-bounded reaction would stop dead at the seam. So this
            // arm runs against `world` in world coordinates and returns early;
            // `fire::run_scheduled_tick` writes each change through `world`
            // itself (vanilla's immediate `setBlock`, which the spread loop then
            // reads back), so the loop only forwards what changed.
            //
            // Fire is not random-ticked — `FireBlock::tick`'s first statement is
            // its own reschedule — so this arm is the only thing keeping a fire
            // alive at all. `random_tick::react_at_placement` seeds the first
            // pending tick for any fire a world edit writes; a fire that loses
            // its queue entry is inert forever.
            if due.kind == crate::fire::TICK_FIRE {
                let (min_y, height) = *fire_env.get_or_insert_with(|| {
                    let probe = world.column(x.div_euclid(16), z.div_euclid(16));
                    (probe.min_y, probe.height)
                });
                let env = crate::fire::FireEnv::overworld_in(
                    min_y,
                    height,
                    world_state.difficulty().0,
                    weather.raining,
                );
                fire_changes.clear();
                fire_primed_tnt.clear();
                crate::fire::run_scheduled_tick(
                    &*world,
                    env,
                    BlockPos::new(x, y, z),
                    block_ticks,
                    game_tick,
                    &mut fire_rng,
                    &mut fire_changes,
                    &mut fire_primed_tnt,
                );
                for (at, new_state) in fire_changes.drain(..) {
                    block_tick_out.publish(at.x, at.y, at.z, new_state);
                }
                // `TntBlock::prime`, the fire-consumption arm — see
                // `crate::fire::check_burn_out`'s own doc for why this is
                // reported rather than spawned inline.
                if world_state.tnt_explodes() {
                    mobs.with(|sim| {
                        for pos in fire_primed_tnt.drain(..) {
                            sim.spawn_tnt(
                                lodestone_model::Vec3::new(
                                    f64::from(pos.x) + 0.5,
                                    f64::from(pos.y),
                                    f64::from(pos.z) + 0.5,
                                ),
                                crate::mobs::tnt::DEFAULT_FUSE_TIME,
                            );
                        }
                    });
                } else {
                    fire_primed_tnt.clear();
                }
                continue;
            }
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

            // `FallingBlock.tick`, reached from `FallingBlock.onPlace`'s scheduled
            // tick (`crate::gravity_tick::ticks_after_place`). Handled here with a
            // `continue` rather than through the `new_state` chain below, because
            // it is the one arm whose reaction is **at the tick's own position**:
            // `settle_gravity_at` is exactly `isFree(below)` plus the drop, while
            // the chain below ends in `propagate_and_react`, which notifies the
            // origin's six neighbours and *not* the origin. Routing gravity through
            // that would settle the sand's neighbours and leave the sand hanging —
            // the reported symptom, with a scheduled tick that looked correct.
            //
            // Before this arm existed, only a neighbour update could reach the
            // gravity check at all, which is precisely the owner's report: sand
            // placed in mid-air did not fall until another block was placed beside
            // it.
            //
            // **This is now the only place in the tree a block leaves the world to
            // fall**, and it creates a real `FallingBlockEntity` rather than
            // teleporting: `settle_gravity_at` answers "unsupported, landing at
            // `y`" and `MobSim::spawn_falling_block` returns the two effects in
            // `FallingBlockEntity.fall`'s own order (clear the cell, *then*
            // broadcast the entity). The effects are applied in the order given —
            // see `gravity_tick::FallingBlockEffect` for why that order is a
            // returned value rather than two statements here.
            if due.kind == crate::gravity_tick::TICK_GRAVITY {
                if let Some(settle) =
                    crate::random_tick::settle_gravity_at(&column, min_x, min_z, x, y, z)
                {
                    let origin = BlockPos::new(x, y, z);
                    let (_id, effects) = mobs.with(|sim| {
                        sim.spawn_falling_block(settle.state.clone(), origin, settle.landing_y)
                    });
                    for effect in effects {
                        apply_falling_block_effect(
                            &*world,
                            &block_tick_out,
                            Some((&mut column, min_x, min_z)),
                            &effect,
                        );
                    }
                    // `setBlock(pos, air, 3)`'s flag-1 half: the cell the block
                    // left has to notify its neighbours, or a *stack* of sand
                    // collapses exactly one block deep — the block above the one
                    // that just left never learns its support is gone. The gravity
                    // arm in `react_to_notification` schedules rather than settles,
                    // so this cascades with vanilla's delay per layer instead of
                    // resolving the whole column in one tick.
                    for event in crate::random_tick::propagate_and_react(
                        &mut column,
                        min_x,
                        min_z,
                        x,
                        y,
                        z,
                        &mut block_ticks,
                        game_tick,
                    ) {
                        let (ex, ey, ez) = event.pos;
                        world.set_block(ex, ey, ez, &event.to);
                        publish_moving_piston(&block_tick_out, &block_ticks, ex, ey, ez, &event.to);
                        block_tick_out.publish(ex, ey, ez, event.to);
                    }
                }
                continue;
            }

            // `redstone_tripwire::TICK_TRIPWIRE_RECHECK` — `TripWireHookBlock
            // .tick`'s periodic recheck. Handled here rather than through the
            // `Option<String>` chain below for the same reason gravity is: one
            // scan can rewrite the hook's own cell, a receiving hook, and every
            // wire segment between them, not a single position — see
            // `crate::random_tick::run_tripwire_recheck`'s own doc comment.
            if due.kind == crate::redstone_tripwire::TICK_TRIPWIRE_RECHECK {
                for event in crate::random_tick::run_tripwire_recheck(&mut column, min_x, min_z, BlockPos::new(x, y, z)) {
                    let (ex, ey, ez) = event.pos;
                    world.set_block(ex, ey, ez, &event.to);
                    block_tick_out.publish(ex, ey, ez, event.to);
                }
                continue;
            }

            // `crate::redstone_dispenser::TICK_DISPENSER_FIRE` — the one-shot
            // fire `on_neighbor_changed` schedules on the rising edge. Handled
            // here, with its own `continue`, because it needs the live
            // container (`block_entities`) and the mob simulation (`mobs`),
            // neither of which the `Option<String>` chain below has in scope.
            //
            // Issue #320's remainder: a dropper always either pushes into a
            // container ahead or plain-tosses, never consulting the item
            // table below (`DropperBlock.getDispenseMethod` hardcodes
            // `DefaultDispenseItemBehavior` regardless of item); a dispenser
            // instead matches the item against spawn egg, boat, bone meal and
            // flint-and-steel in turn, falling to a plain toss when none
            // match or a matched behaviour reports no effect. See
            // `crate::redstone_dispenser`'s own module doc for the full
            // behaviour table, including everything still deliberately
            // unmodelled and why.
            if due.kind == crate::redstone_dispenser::TICK_DISPENSER_FIRE {
                if crate::redstone_dispenser::is_dispenser_family(&state) {
                    let origin = BlockPos::new(x, y, z);
                    let slots = block_entities.with(|reg| {
                        reg.get(origin)
                            .map(crate::block_entities::BlockEntity::container_slots)
                    });
                    // `random_slot`'s own `None` is vanilla's `-1`: an empty
                    // container plays a click sound instead of dispensing —
                    // sound effects are out of this crate's scope, so this is
                    // a silent no-op rather than a missing feature.
                    let picked = slots.as_ref().and_then(|slots| {
                        let occupied: Vec<bool> = slots.iter().map(Option::is_some).collect();
                        crate::redstone_dispenser::random_slot(&occupied, |bound| {
                            dispenser_rng.next_int(i32::try_from(bound).unwrap_or(i32::MAX)).max(0) as u32
                        })
                    });
                    if let (Some(slots), Some(slot)) = (slots, picked) {
                        let stack = slots[slot]
                            .clone()
                            .expect("random_slot only ever picks an occupied slot");
                        let item = stack.item.clone();
                        let item_str = item.to_string();
                        let face = crate::redstone_dispenser::facing(&state);
                        let center = (f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5);
                        // Bounded to this dispenser's own 16x16 column, the
                        // same approximation `crate::redstone::make_lookup`'s
                        // every other caller in this file already accepts —
                        // a target one cell past a chunk edge reads as air.
                        let lookup = crate::redstone::make_lookup(&column, min_x, min_z);

                        // `consumed`: one item leaves the picked slot.
                        // `toss`: additionally becomes a tossed item entity.
                        // Two independent flags because a dropper's container
                        // push, a spawned mob/vehicle and a bone-meal/ignite
                        // world edit are all "consumed, not tossed" — only
                        // the absence of any matching behaviour, or a matched
                        // behaviour's own explicit fallback (a boat with
                        // nothing to land on, a dropper with no container
                        // ahead), is a toss.
                        let mut consumed = true;
                        let mut toss = false;

                        if crate::redstone_dispenser::is_dropper(&state) {
                            // `DropperBlock.dispenseFrom`'s container branch.
                            let front = face.relative(origin);
                            let menu = block_entities.with(|reg| reg.get(front).and_then(crate::block_entities::BlockEntity::menu_name));
                            if menu.is_some_and(crate::redstone_dispenser::is_pushable_container) {
                                let mut dest = block_entities
                                    .with(|reg| reg.get(front).map(crate::block_entities::BlockEntity::container_slots))
                                    .unwrap_or_default();
                                let mut single = stack.clone();
                                single.count = 1;
                                if crate::hopper::try_move_item_into(single, &mut dest).is_none() {
                                    block_entities.with(|reg| {
                                        if let Some(entity) = reg.get_mut(front) {
                                            for (i, s) in dest.into_iter().enumerate() {
                                                entity.set_container_slot(i, s);
                                            }
                                        }
                                    });
                                } else {
                                    // Full: neither a push nor a toss — the
                                    // item stays exactly where it was.
                                    consumed = false;
                                }
                            } else {
                                // No container ahead — the same plain toss a
                                // dispenser's own unmatched-item fallback uses.
                                toss = true;
                            }
                        } else if let Some(entity_type) = crate::spawn_egg::entity_type_for_egg(&item_str) {
                            let position = crate::redstone_dispenser::spawn_egg_position(origin, face, &lookup);
                            mobs.with(|sim| {
                                sim.spawn_species(entity_type, position);
                            });
                        } else if let Some(entity_type) = crate::boat::entity_type_for_boat_item(&item_str) {
                            match crate::redstone_dispenser::boat_dispense(origin, face, crate::boat::BOAT_WIDTH, &lookup) {
                                crate::redstone_dispenser::BoatDispense::Place { position, yaw } => {
                                    mobs.with(|sim| {
                                        sim.spawn_vehicle(entity_type, position, yaw);
                                    });
                                }
                                crate::redstone_dispenser::BoatDispense::Fallback => toss = true,
                            }
                        } else if item_str == "minecraft:tnt" {
                            // `TntDispenseItemBehavior` — spawns a primed TNT
                            // entity just outside the dispenser's own face,
                            // reusing the exact position formula the spawn-egg
                            // and boat arms above already use for the same
                            // "outside this face" placement.
                            let position = crate::redstone_dispenser::spawn_egg_position(origin, face, &lookup);
                            mobs.with(|sim| {
                                sim.spawn_tnt(position, crate::mobs::tnt::DEFAULT_FUSE_TIME);
                            });
                        } else if item_str == crate::bone_meal::BONE_MEAL {
                            let target = face.relative(origin);
                            let target_state = lookup(target);
                            let above_state = lookup(BlockPos::new(target.x, target.y + 1, target.z));
                            match crate::bone_meal::apply_bone_meal(&target_state, &above_state, &mut dispenser_rng) {
                                crate::bone_meal::BoneMealOutcome::Grew { state: new_state } => {
                                    world.set_block(target.x, target.y, target.z, &new_state);
                                    block_tick_out.publish(target.x, target.y, target.z, new_state);
                                }
                                crate::bone_meal::BoneMealOutcome::ConsumedNoChange => {}
                                // Not a target, or a family this crate cannot
                                // grow: vanilla's own `OptionalDispenseItemBehavior`
                                // never falls back to a toss here either — the
                                // item just stays put, unconsumed.
                                crate::bone_meal::BoneMealOutcome::NotBonemealable
                                | crate::bone_meal::BoneMealOutcome::NotModelled { .. } => {
                                    consumed = false;
                                }
                            }
                        } else if item_str == "minecraft:flint_and_steel" {
                            let (min_y, height) = *fire_env.get_or_insert_with(|| {
                                let probe = world.column(x.div_euclid(16), z.div_euclid(16));
                                (probe.min_y, probe.height)
                            });
                            let env = crate::fire::FireEnv::overworld_in(min_y, height, world_state.difficulty().0, weather.raining);
                            // Cross-chunk-correct, unlike `lookup` above:
                            // `crate::fire`'s functions take a `ChunkSource`
                            // directly rather than a closure, so this reads
                            // through `world` itself rather than the bounded
                            // column — matching the `TICK_FIRE` arm's own
                            // precedent just above.
                            match crate::redstone_dispenser::flint_and_steel_ignite(&*world, env, origin, face) {
                                Some((target, new_state)) => {
                                    world.set_block(target.x, target.y, target.z, &new_state);
                                    block_tick_out.publish(target.x, target.y, target.z, new_state);
                                }
                                // Same shape as bone meal: no toss fallback in
                                // vanilla's own `FlintAndSteelDispenseItemBehavior`.
                                None => consumed = false,
                            }
                        } else {
                            toss = true;
                        }

                        if consumed {
                            let mut remaining = stack.clone();
                            remaining.count = remaining.count.saturating_sub(1);
                            let remainder = if remaining.count == 0 { None } else { Some(remaining) };
                            block_entities.with(|reg| {
                                if let Some(entity) = reg.get_mut(origin) {
                                    entity.set_container_slot(slot, remainder);
                                }
                            });
                        }
                        if toss {
                            let (position, velocity) =
                                crate::redstone_dispenser::plain_toss(center, face, &mut || {
                                    dispenser_rng.next_f64()
                                });
                            mobs.with(|sim| {
                                sim.spawn_item(
                                    item,
                                    lodestone_model::Vec3::new(position.0, position.1, position.2),
                                    lodestone_model::Vec3::new(velocity.0, velocity.1, velocity.2),
                                    // Vanilla's dispensed `ItemEntity` never calls
                                    // `setDefaultPickUpDelay()`, so it keeps the
                                    // constructor's `pickupDelay = 0` — unlike a
                                    // broken block's drop, it is pickable the
                                    // instant it appears.
                                    lodestone_entity::ItemLifecycle {
                                        age: 0,
                                        pickup_delay: 0,
                                        count: 1,
                                        max_stack_size: lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                                    },
                                );
                            });
                        }
                    }
                }
                continue;
            }

            // `crate::mobs::tnt::TICK_TNT_PRIME` — the redstone-signal ignition
            // arm (`TntBlock::onPlace`/`neighborChanged`) schedules this at
            // `current_tick` itself; see `crate::random_tick`'s TNT arm for why
            // it cannot spawn the entity directly. Handled here, with its own
            // `continue`, for the reason every other entity-spawning arm in
            // this drain is: it needs `mobs`, which the `Option<String>` chain
            // below has no access to.
            //
            // Re-checked against the *live* block (`crate::mobs::tnt::is_tnt_block`)
            // rather than trusting the scheduling arm's premise: a block that
            // changed again before this tick fires (mined, replaced) must not
            // spawn a phantom TNT entity where nothing is left.
            if due.kind == crate::mobs::tnt::TICK_TNT_PRIME {
                if crate::mobs::tnt::is_tnt_block(&state) && world_state.tnt_explodes() {
                    let origin = BlockPos::new(x, y, z);
                    world.set_block(x, y, z, crate::chunk::AIR);
                    block_tick_out.publish(x, y, z, crate::chunk::AIR.to_owned());
                    mobs.with(|sim| {
                        sim.spawn_tnt(
                            lodestone_model::Vec3::new(f64::from(origin.x) + 0.5, f64::from(origin.y), f64::from(origin.z) + 0.5),
                            crate::mobs::tnt::DEFAULT_FUSE_TIME,
                        );
                    });
                }
                continue;
            }

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
            } else if due.kind == crate::redstone_target::TICK_TARGET_DECAY {
                // `TargetBlock.tick` (`:85-89`) — decay the analog `power`
                // back to 0. Issue #322: scheduled for real now, by the
                // projectile-block-hit resolution earlier in this same
                // `scheduled.with` region (search this file for
                // `crate::mobs::ProjectileBlockHit`) — a target's `power`
                // set by a projectile hit no longer decays from nothing.
                crate::redstone_target::run_scheduled_tick(&state)
            } else if due.kind == crate::hand_use::TICK_BUTTON {
                // Issue #532: a pressed button releasing itself after its
                // `ticksToStayPressed`. The same shape as the redstone families
                // above — a pure decision on the state, re-propagated below — so a
                // button feeding a door closes it again when the button pops up.
                crate::hand_use::release_button(&state)
            } else if crate::piston::is_finish_kind(&due.kind) {
                // The second phase of a piston move —
                // `PistonMovingBlockEntity.tick`'s commit branch. The state to write
                // travels in the tick's own kind, because the pending tick *is* this
                // crate's moving block entity (see `piston::finish_kind`).
                //
                // Vanilla's `if (level.getBlockState(pos).is(Blocks.MOVING_PISTON))`
                // guard is reproduced: anything else already rewrote this cell (a
                // player broke it, a second move claimed it), and committing over
                // that would resurrect a block from a move that no longer exists.
                if crate::piston::is_moving_piston(&state) {
                    crate::piston::parse_finish_kind(&due.kind).map(|entity| entity.moved_state)
                } else {
                    None
                }
            } else {
                // No other block-tick behaviour is modeled — see this
                // function's own doc comment.
                None
            };

            if let Some(new_state) = new_state {
                if new_state != state {
                    // Issue #530: a door, trapdoor or fence gate a scheduled tick
                    // just toggled — vanilla's `DoorBlock.playSound`. The one
                    // openable path that is genuinely server-driven, so nothing
                    // predicts it and it was silent.
                    publish_openable_sound(&block_tick_out, BlockPos::new(x, y, z), &state, &new_state, game_tick);
                    column.set_block(lx, y, lz, &new_state);
                    world.set_block(x, y, z, &new_state);
                    block_tick_out.publish(x, y, z, new_state);
                }
                for event in crate::random_tick::propagate_and_react(&mut column, min_x, min_z, x, y, z, &mut block_ticks, game_tick) {
                    let (ex, ey, ez) = event.pos;
                    publish_openable_sound(&block_tick_out, BlockPos::new(ex, ey, ez), &event.from, &event.to, game_tick);
                    world.set_block(ex, ey, ez, &event.to);
                    publish_moving_piston(&block_tick_out, &block_ticks, ex, ey, ez, &event.to);
                    block_tick_out.publish(ex, ey, ez, event.to);
                }
            }
        }
        // Fluid spread — `crate::fluid`, the port of `FlowingFluid.tick`. This
        // loop was an empty acknowledgement until that module landed; it is now
        // the only thing that makes a placed or exposed liquid actually move.
        //
        // Unlike the block drain above, this runs against `world` in **world
        // coordinates** rather than against one `ChunkColumn`: fluid spread
        // crosses chunk borders (a source three cells from a border reaches four
        // cells past it), and a column-bounded reaction would stop dead at the
        // seam. `ChunkSource::block_state`/`set_block` already take world
        // coordinates and already reflect prior edits, so no neighbourhood
        // assembly is needed here.
        //
        // The reschedules `run_scheduled_tick` makes land in `fluid_ticks` while
        // it is being drained, which `drain_due`'s collect-then-run split keeps
        // out of this same pass — so a flow advances one cell per delay period
        // rather than resolving the whole pool inside one tick.
        let mut fluid_changes: Vec<(BlockPos, String)> = Vec::new();
        for due in fluid_ticks.drain_due(game_tick, MAX_SCHEDULED_TICKS_PER_TICK) {
            let (x, y, z) = due.pos;
            let env = *fluid_env.get_or_insert_with(|| {
                let probe = world.column(x.div_euclid(16), z.div_euclid(16));
                crate::fluid::FluidEnv::overworld_in(probe.min_y, probe.height)
            });
            fluid_changes.clear();
            crate::fluid::run_scheduled_tick(
                &*world,
                env,
                BlockPos::new(x, y, z),
                fluid_ticks,
                game_tick,
                &mut fluid_changes,
            );
            // `run_scheduled_tick` has already written every one of these through
            // `world` (it reads the world back as it spreads, exactly as
            // vanilla's immediate `setBlock` does), so this loop only forwards
            // them to connected clients.
            for (pos, state) in fluid_changes.drain(..) {
                block_tick_out.publish(pos.x, pos.y, pos.z, state);
            }
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
        let tick_speed = world_state.random_tick_speed();
        if game_tick > INITIAL_RANDOM_TICK_DEFERRAL_TICKS && tick_speed > 0 {
            // The follow area rather than the two fixed ranges: crops, grass, fire,
            // leaf decay and every other randomly-ticking block now grow where the
            // player is standing instead of only around chunk (0, 0).
            for &(cx, cz) in area.chunks() {
                {
                    let mut column = world.column(cx, cz);
                    // Issue #508: the *rule*, not `DEFAULT_RANDOM_TICK_SPEED`.
                    // The getter has existed and been tested since #327; this line
                    // is the reader it was missing, and `/gamerule
                    // random_tick_speed 0` now really does stop crop growth.
                    let events = random_ticks.tick_chunk(&mut column, cx, cz, tick_speed, &mut block_ticks, game_tick);
                    for event in events {
                        let (x, y, z) = event.pos;
                        world.set_block(x, y, z, &event.to);
                        publish_moving_piston(&block_tick_out, &block_ticks, x, y, z, &event.to);
                        block_tick_out.publish(x, y, z, event.to);
                    }
                }
            }
        }

        // Every live `FallingBlockEntity`, one tick — the driver half of the
        // falling-block animation. Without it a spawned entity would sit still at
        // its spawn cell forever, which is a *worse* symptom than the teleport it
        // replaced: the block would appear to simply vanish.
        //
        // **Position is vanilla's**: `ServerLevel.tick` runs `tickChunks` — which
        // is the scheduled-block, scheduled-fluid and random-tick passes above —
        // and only then walks `entityTickList`. So an entity created by a block
        // tick this tick does take its first `0.04` step in the same tick, exactly
        // as it does in the jar, and the `ADD_ENTITY` a connection's next streaming
        // pass sends already carries the post-step position (vanilla's tracker also
        // reads it at end of tick).
        //
        // Inside the queue scope rather than in the mob block above because a
        // landing has to `propagate_and_react`, which needs `block_ticks`.
        // `MobSim::tick_falling_blocks` returns only the ticks that *finished*; an
        // airborne entity's new position rides the ordinary `snapshots()` diff, so
        // there is no per-tick position event to forward.
        for effect in mobs.with(MobSim::tick_falling_blocks) {
            // No column: `Placed` carries world coordinates and `ChunkSource`
            // already takes them. The propagation below needs one, so it is built
            // on demand — a landing is rare, unlike the per-tick step.
            apply_falling_block_effect(&*world, &block_tick_out, None, &effect);
            if let crate::gravity_tick::FallingBlockEffect::Placed { pos, .. } = &effect {
                // `setBlock(pos, blockState, 3)`'s flag-1 half, at the landing
                // cell: the placed block notifies its neighbours, which is what
                // lets a pile settle rather than one block land on top of something
                // that should also have fallen. Column fetched *after* the
                // placement so the propagation sees it.
                let cx = pos.x.div_euclid(16);
                let cz = pos.z.div_euclid(16);
                let mut column = world.column(cx, cz);
                for event in crate::random_tick::propagate_and_react(
                    &mut column,
                    cx * 16,
                    cz * 16,
                    pos.x,
                    pos.y,
                    pos.z,
                    &mut block_ticks,
                    game_tick,
                ) {
                    let (ex, ey, ez) = event.pos;
                    world.set_block(ex, ey, ez, &event.to);
                    publish_moving_piston(&block_tick_out, &block_ticks, ex, ey, ez, &event.to);
                    block_tick_out.publish(ex, ey, ez, event.to);
                }
            }
        }

        // Every **unridden** boat, one tick — `AbstractBoat.tick`'s buoyancy and
        // drag. A ridden one is skipped inside `tick_vehicles`, which is the
        // client-authority handover: while somebody is aboard, their
        // `MoveVehicle` is the only writer, and ticking the hull here as well is
        // what would make a boat fight its rider.
        //
        // Beside `tick_falling_blocks` because it is the same kind of thing —
        // vanilla's `entityTickList` walk, after `tickChunks` — and because this
        // scope already holds the world the collision shapes come from. The
        // closure is needed rather than `ChunkWorld`'s coarse solidity: a boat's
        // `waterLevel` is computed from each cell's fluid **amount**, so a
        // boolean would put every surface `1/9` of a block off.
        //
        // No effects to forward: a boat's new position rides the ordinary
        // `snapshots()` diff exactly as an airborne falling block's does.
        mobs.with(|sim| sim.tick_vehicles(&|x, y, z| world.block_state(x, y, z)));

        // Every live primed TNT, one tick — gravity, collision/bounce and the
        // fuse countdown (`crate::mobs::tnt`'s own module doc). Beside
        // `tick_vehicles` for the same reason that call is beside
        // `tick_falling_blocks`: this scope already holds the live world the
        // collision shapes come from. A detonation this tick queues into
        // `MobSim::pending_detonations` exactly as a creeper's does, so it
        // reaches the `take_detonations` drain above on the tick after this
        // one — the same one-tick latency `tick_vehicles`/`tick_falling_blocks`
        // already accept for their own effects.
        mobs.with(|sim| sim.tick_tnt(&|x, y, z| world.block_state(x, y, z)));
        });

        clock.record_tick(tick_start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `LEVEL_STEP`/`WeatherEvent` are referenced only by the weather loop gates
    // below, so they live here rather than at module scope (the lib build
    // warns on imports that only tests touch).
    use crate::weather::{LEVEL_STEP, WeatherEvent};
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

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; the clock/overrun gates only
            // care that the loop runs, not what this reads.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // `run_tick_loop` can forward grazing/random-tick mutations to this
        // (tick.rs's own `world.set_block`), so it must not panic; but the
        // source has no storage, so the edit is deliberately discarded.
        // Explicit rather than inherited (issue #440).
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
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
            crate::tick_area::TickFollow::default(),
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
            crate::tick_area::TickFollow::default(),
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

    /// The effect lane's `except` player: an effect published against one player
    /// reaches every other one and never that one, which is what lets a block
    /// break and place sound be published at all (the acting client predicts
    /// both locally).
    #[test]
    fn an_excluded_player_does_not_receive_their_own_effect() {
        let actor = uuid::Uuid::from_u128(1);
        let bystander = uuid::Uuid::from_u128(2);
        let effect = crate::effects::WorldEffect::LevelEvent {
            event: crate::effects::PARTICLES_DESTROY_BLOCK,
            pos: lodestone_model::BlockPos::new(4, 5, 6),
            data: 1,
            global: false,
        };

        let feed = BlockTickFeed::default();
        feed.publish_effect_except(actor, effect.clone());
        assert_eq!(feed.drain_effects_for(bystander), vec![effect.clone()]);

        let feed = BlockTickFeed::default();
        feed.publish_effect_except(actor, effect.clone());
        assert!(feed.drain_effects_for(actor).is_empty());

        // An untagged effect reaches everyone, including whoever caused the tick.
        let feed = BlockTickFeed::default();
        feed.publish_effect(effect.clone());
        assert_eq!(feed.drain_effects_for(actor), vec![effect]);
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
            crate::tick_area::TickFollow::default(),
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

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; this fixture only records
            // `set_block` calls, nothing reads terrain back.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
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
            crate::tick_area::TickFollow::default(),
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

    /// A source that records which chunk columns the loop asked for, so a gate can
    /// see **where** the world tick is actually running.
    ///
    /// Terrain is an empty column, deliberately: nothing here reads a block back,
    /// and an empty column makes the random-tick pass produce no events, so the
    /// recorded set is exactly "which columns the loop visited" with no other
    /// writer confusing it.
    #[derive(Default)]
    struct ColumnProbe(Arc<Mutex<Vec<(i32, i32)>>>);

    impl ChunkSource for ColumnProbe {
        fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
            self.0.lock().expect("probe lock poisoned").push((cx, cz));
            crate::chunk::ChunkColumn::new(0, 16)
        }

        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            // Deliberately *not* `self.column(..)`: a block-state probe must not
            // land in the recorded set, or the block-entity scan and the fluid pass
            // would be indistinguishable from the random-tick area this measures.
            "minecraft:air".to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    /// **The world tick follows the player, and it stops ticking the origin.**
    ///
    /// This is the production-level gate for `crate::tick_area`: it drives the real
    /// [`run_tick_loop`] and observes which columns the random-tick pass fetches.
    ///
    /// # Why the player is at chunk (100, -37)
    ///
    /// Every other gate in this area puts the player at chunk `(0, 0)`, which is the
    /// single position where "a box fixed at the origin" and "a box centred on the
    /// player" are the **same set** — so none of them can fail under the old
    /// behaviour. `(100, -37)` is far outside the fallback square, uses a negative
    /// axis, and has `100 != -37` so the axes cannot be transposed unnoticed.
    ///
    /// # The assertion, stated as the old behaviour's failure
    ///
    /// The fallback square (`(64..=64, 64..=64)` here) must be visited **zero**
    /// times and the player's own column must be visited, which is exactly inverted
    /// from what the fixed area did. Asserting only "the player's column is visited"
    /// would pass for an area that ticks *both*, i.e. for a widened constant rather
    /// than a moved one.
    #[tokio::test(start_paused = true)]
    async fn the_random_tick_pass_follows_the_player_and_abandons_the_fixed_area() {
        let (mobs, out, block_entities) = handles();
        let visited = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(ColumnProbe(Arc::clone(&visited)));

        // The anchor set on its own rather than through a `WorldStateHandle`: this
        // gate is about the geometry, and `run_tick_loop` (the wrapper) supplies its
        // own default world state, whose `random_tick_speed` is already vanilla's
        // non-zero default.
        let anchors = crate::tick_area::TickAnchors::default();
        anchors.publish(vec![crate::tick_area::TickAnchor {
            dimension: crate::dimension::Dimension::Overworld,
            cx: 100,
            cz: -37,
        }]);
        let follow = crate::tick_area::TickFollow {
            dimension: crate::dimension::Dimension::Overworld,
            radius: 1,
            anchors,
        };

        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::new(TickClock::new()),
            source,
            BlockTickFeed::default(),
            // The fallback: a single column at (64, 64), nowhere near the player.
            // Under the old fixed-area behaviour this is the *only* column that
            // would ever be visited.
            (64..=64, 64..=64),
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
            follow,
        ));
        // See `a_healthy_run_never_records_an_overrun`: the spawned task must reach
        // its first `Instant::now()` before any `advance`.
        tokio::task::yield_now().await;

        // Past `INITIAL_RANDOM_TICK_DEFERRAL_TICKS`, derived rather than restated —
        // raising the deferral must move this expectation with it rather than
        // silently voiding the gate.
        for _ in 0..(INITIAL_RANDOM_TICK_DEFERRAL_TICKS + 5) {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }

        let seen = visited.lock().expect("poisoned").clone();
        assert!(
            !seen.is_empty(),
            "the random-tick pass must have run at all — if this is empty the \
             deferral or the tick-speed rule kept it from ever firing, and every \
             assertion below would be vacuous"
        );
        assert!(
            seen.contains(&(100, -37)),
            "the player's own column must be ticked; visited {seen:?}"
        );
        // At radius 1 the area is the 3x3 around the player, so its corners are
        // predicted exactly rather than bounded.
        for corner in [(99, -38), (101, -36), (99, -36), (101, -38)] {
            assert!(
                seen.contains(&corner),
                "the 3x3 around the player must be ticked, missing {corner:?}"
            );
        }
        // The claim the old behaviour cannot pass.
        assert!(
            !seen.contains(&(64, 64)),
            "the fallback column was still ticked, so the area did not move — this \
             is the assertion a player at chunk (0, 0) could never make"
        );
        assert!(
            !seen.contains(&(-37, 100)),
            "the axes must not be transposed"
        );
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

    // ---------------------------------------------------------------------
    // Issue #324 / `docs/plans/world-state.md` W1: weather reaches the feed.
    // These gate the **production loop**, so they fail if the weather drain
    // is removed from `run_tick_loop` rather than merely if `WeatherState`
    // regresses — the `crate::weather` unit gates and this one would be
    // exactly the "hermetic green, island red" pair CLAUDE.md's rule 1 names.
    // ---------------------------------------------------------------------

    /// The loop ticks [`WeatherState`] once per iteration and publishes its
    /// transitions into [`WeatherFeed`].
    ///
    /// The state is seeded **mid-cycle** (rain already on, level mid-ramp)
    /// rather than left at `WeatherState::default()`: a fresh world's first
    /// rain is `RAIN_DELAY`'s 12k-180k ticks away, and waiting that out would
    /// make this gate measure nothing but tokio's clock. With rain on and a
    /// long `rain_time`, the very first loop tick must move the level by
    /// exactly `LEVEL_STEP` and publish it — a one-tick, exact-value gate.
    #[tokio::test(start_paused = true)]
    async fn the_loop_ticks_weather_and_publishes_its_transitions() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        let (world, block_tick_out, tick_area) = world_tick_args();
        let weather_out = WeatherFeed::default();
        // `/weather rain` mid-spell: raining, a long way from its next flip,
        // level ramping up from 0.5.
        let mut weather = WeatherState::default();
        weather.raining = true;
        weather.thundering = false;
        weather.rain_time = i32::MAX;
        weather.thunder_time = i32::MAX;
        weather.rain_level = 0.5;

        // Issue #325: a fresh night-skip vote no connection calls — the
        // `_with_weather` body ticks a `SleepState` either way, so the loop
        // shape is identical, but an empty roster can never pass. Wrapped in
        // `async move` because the loop borrows the vote/feed, and
        // `tokio::spawn` demands `'static`. The feed the loop writes is
        // cloned out first — the test drains the original `weather_out` after
        // the spawn, so the block must not capture it.
        let vote = SleepVote::new();
        let feed = SleepFeed::default();
        let weather_for_loop = weather_out.clone();
        tokio::spawn(async move {
            run_tick_loop_with_weather(
                mobs,
                out,
                block_entities,
                Arc::clone(&clock),
                world,
                block_tick_out,
                tick_area,
                ExplosionFeed::default(),
                weather_for_loop,
                weather,
                &vote,
                &feed,
                crate::region_source::ScheduledTickHandle::default(),
                crate::world_state::WorldStateHandle::default(),
                crate::tick_area::TickFollow::default(),
            )
            .await;
        });
        // See `ten_periods_advance_exactly_ten_ticks_with_no_overrun`: the
        // spawned task must reach its first `Instant::now()` before the first
        // `advance`, or every tick prediction shifts by one period.
        tokio::task::yield_now().await;

        tokio::time::advance(TICK_PERIOD).await;
        tokio::task::yield_now().await;

        let events = weather_out.drain_all();
        assert_eq!(
            events.len(),
            1,
            "one tick of rain on must broadcast exactly one transition: {events:?}"
        );
        match events[0] {
            WeatherEvent::RainLevelChanged(level) => {
                assert!(
                    (level - 0.51).abs() < 1e-5,
                    "level must ramp 0.01 from 0.5 to 0.51, got {level}"
                );
            }
            other => panic!("expected a rain ramp, got {other:?}"),
        }
    }

    /// `/weather`'s consumer half, driven through the **production** loop:
    /// a `WeatherRequest` queued on `WorldStateHandle` *before* the loop ever
    /// ticks must be applied on the very first pass and broadcast — this is
    /// the gate `crate::world_state`'s own `a_weather_request_is_queued_then_taken_exactly_once`
    /// cannot be, since that one only proves the queue itself works and never
    /// drives `run_tick_loop_with_weather` at all. Without the hunk this
    /// gates, a queued request would sit forever: `take_weather_request` is
    /// never called by anything else.
    ///
    /// Starts clear (not raining) so the flip is unambiguous, and asserts the
    /// exact two-event order this crate's own application logic produces:
    /// the immediate `StartRaining` from the request being applied *before*
    /// this tick's own `weather.tick()` call (see that hunk's own comment for
    /// why applying first is what makes the flip land on tick one rather than
    /// never), followed by that same tick's ordinary `RainLevelChanged` ramp.
    #[tokio::test(start_paused = true)]
    async fn a_queued_weather_request_is_applied_on_the_loops_first_pass() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        let (world, block_tick_out, tick_area) = world_tick_args();
        let weather_out = WeatherFeed::default();
        // Clear, not raining — so a flip to raining is unambiguous.
        let weather = WeatherState::default();

        let world_state = crate::world_state::WorldStateHandle::new();
        world_state.request_weather(crate::world_state::WeatherRequest::Rain { duration: i32::MAX });

        let vote = SleepVote::new();
        let feed = SleepFeed::default();
        let weather_for_loop = weather_out.clone();
        tokio::spawn(async move {
            run_tick_loop_with_weather(
                mobs,
                out,
                block_entities,
                Arc::clone(&clock),
                world,
                block_tick_out,
                tick_area,
                ExplosionFeed::default(),
                weather_for_loop,
                weather,
                &vote,
                &feed,
                crate::region_source::ScheduledTickHandle::default(),
                world_state,
                crate::tick_area::TickFollow::default(),
            )
            .await;
        });
        tokio::task::yield_now().await;

        tokio::time::advance(TICK_PERIOD).await;
        tokio::task::yield_now().await;

        let events = weather_out.drain_all();
        assert_eq!(
            events,
            vec![WeatherEvent::StartRaining, WeatherEvent::RainLevelChanged(0.01)],
            "a queued Rain request must flip the boolean immediately (before this tick's \
             own weather.tick() runs) and that same tick must still ramp the level: {events:?}"
        );
    }

    /// Gate (c)'s magnitude half, driven through the **production** loop
    /// rather than at `WeatherState::tick` directly: rain forced on from a
    /// dry start, the level must ramp exactly `LEVEL_STEP` per tick and
    /// reach exactly `1.0` (clamped), each ramp broadcast as a `(7, level)`
    /// wire pair — the same 100-tick 0→1.0 ramp `docs/plans/world-state.md`
    /// W1's gate (c) names, against real `GAME_EVENT` ids rather than the
    /// event enum. (The exact v770 *bytes* for those pairs are pinned by
    /// `encode_game_event_wire_layout`; the serve_play drain that turns the
    /// drained pairs into packets is pinned by `tests/serve_play.rs`.)
    #[tokio::test(start_paused = true)]
    async fn rain_forced_on_ramps_exactly_level_step_per_tick_through_the_loop() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        let (world, block_tick_out, tick_area) = world_tick_args();
        let weather_out = WeatherFeed::default();
        // `/weather rain`: raining, never flipping, level starting dry.
        let mut weather = WeatherState::default();
        weather.raining = true;
        weather.thundering = false;
        weather.rain_time = i32::MAX;
        weather.thunder_time = i32::MAX;
        weather.rain_level = 0.0;

        // Issue #325: a fresh night-skip vote no connection calls (see the
        // other weather gate's comment — the loop shape is identical, the
        // vote just cannot pass). `async move` so the borrow survives
        // `tokio::spawn`'s `'static` bound; the feed the loop writes is
        // cloned out first for the same reason as the other gate.
        let vote = SleepVote::new();
        let feed = SleepFeed::default();
        let weather_for_loop = weather_out.clone();
        tokio::spawn(async move {
            run_tick_loop_with_weather(
                mobs,
                out,
                block_entities,
                Arc::clone(&clock),
                world,
                block_tick_out,
                tick_area,
                ExplosionFeed::default(),
                weather_for_loop,
                weather,
                &vote,
                &feed,
                crate::region_source::ScheduledTickHandle::default(),
                crate::world_state::WorldStateHandle::default(),
                crate::tick_area::TickFollow::default(),
            )
            .await;
        });
        tokio::task::yield_now().await;

        for _ in 0..110 {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;

        let events = weather_out.drain_all();
        // Ticks 1..=100 ramp toward 1.0 (the 100th reaches 0.99999934 — 100
        // × 0.01 accumulated in f32 — and the 101st clamps to exactly 1.0);
        // after that old == new, so no further ramps broadcast. That is the
        // count and the clamp; both are load-bearing.
        assert_eq!(
            events.len(),
            101,
            "expected the 100-tick ramp plus the clamp tick, got {events:?}"
        );
        let mut previous = -1.0f32;
        for (i, event) in events.iter().enumerate() {
            let WeatherEvent::RainLevelChanged(level) = event else {
                panic!("tick {i} must be a rain ramp, got {event:?}");
            };
            let expected = ((i + 1) as f32 * LEVEL_STEP).clamp(0.0, 1.0);
            assert!(
                (level - expected).abs() < 1e-4,
                "tick {i}: level must be ≈{expected}, got {level}"
            );
            assert!(
                *level > previous,
                "tick {i}: the ramp must be monotonic: {previous} -> {level}"
            );
            previous = *level;
            assert_eq!(event.wire(), (7, *level), "rain ramp must be GAME_EVENT id 7");
        }
        assert_eq!(previous, 1.0, "the ramp must land exactly on 1.0");
    }

    /// Gate for issue #325's wiring through the **production** loop, not at
    /// `SleepState` directly (its arithmetic is already pinned by
    /// `crate::sleep`'s own tests): a singleplayer-shaped vote — nobody calls
    /// `set_active`, so `active` stays `0` and [`SleepState::sleepers_needed`]'s
    /// `max(1, …)` floor demands exactly one sleeper — with one sleeper already
    /// in bed when the loop starts. The loop must tick `day_time` in lockstep
    /// with `game_tick`, record the lay-down, and on the
    /// `DEEP_SLEEP_TICKS`-th (100th) tick publish **exactly one**
    /// `SkippedNight` — `game_time == 101`, `morning == 24_000` — then wake the
    /// sleeper and clear the roster so no later tick re-fires the skip.
    ///
    /// Every value is computed here from vanilla's arithmetic, never echoed
    /// from the loop: the lay-down at `game_tick == 1` makes the deep-sleep
    /// threshold pass at `game_tick == 101` (`101 - 1 == DEEP_SLEEP_TICKS`),
    /// `morning_after(101)` is the next multiple of `DAY_LENGTH_TICKS`, i.e.
    /// `24_000`, and the cleared roster makes ticks 102..=110 silent — a
    /// second-skip event at any of them would fail the exact `len() == 1`.
    #[tokio::test(start_paused = true)]
    async fn one_deep_sleeper_skips_the_night_exactly_once_through_the_loop() {
        let (mobs, out, block_entities) = handles();
        let clock = Arc::new(TickClock::new());
        let (world, block_tick_out, tick_area) = world_tick_args();
        let weather_out = WeatherFeed::default();
        // The roster is shared with the loop; nothing here calls `set_active`,
        // which is precisely the singleplayer shape (`active == 0`).
        let vote = SleepVote::new();
        let feed = SleepFeed::default();
        // `LOCAL_PLAYER_ENTITY_ID` (server.rs).
        vote.lay_down(1);

        // Wrapped in `async move`: the loop borrows the vote/feed, and
        // `tokio::spawn` demands `'static`, so the future owns them and this
        // test keeps its own clones for the drain/roster assertions.
        let loop_vote = vote.clone();
        let loop_feed = feed.clone();
        tokio::spawn(async move {
            run_tick_loop_with_weather(
                mobs,
                out,
                block_entities,
                Arc::clone(&clock),
                world,
                block_tick_out,
                tick_area,
                ExplosionFeed::default(),
                weather_out,
                WeatherState::default(),
                &loop_vote,
                &loop_feed,
                crate::region_source::ScheduledTickHandle::default(),
                crate::world_state::WorldStateHandle::default(),
                crate::tick_area::TickFollow::default(),
            )
            .await;
        });
        // See `ten_periods_advance_exactly_ten_ticks_with_no_overrun`: the
        // spawned task must reach its first `Instant::now()` before the first
        // `advance`, or every tick prediction shifts by one period.
        tokio::task::yield_now().await;

        // 110 ticks: the 101st fires the skip (deep-sleep threshold), the rest
        // prove it does not re-fire.
        for _ in 0..110 {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;

        let events = feed.drain_all();
        assert_eq!(
            events.len(),
            1,
            "a single deep sleeper must skip the night exactly once in 110 ticks: {events:?}"
        );
        assert_eq!(
            events[0],
            SleepEvent::SkippedNight {
                game_time: 101,
                morning: 24_000,
            }
        );
        // The skip cleared the roster, so a day-sleeping click cannot vote
        // again tonight.
        assert!(
            vote.snapshot().1.is_empty(),
            "the passed vote's roster must be cleared by the skip"
        );
    }
    // ---------------------------------------------------------------------
    // The fire arm of the block-tick drain. `crate::fire`'s own tests gate
    // `run_scheduled_tick`'s behaviour against a reference generator; these two
    // gate the *wiring* — that the loop dispatches `TICK_FIRE` at all, and that
    // what it writes reaches the wire feed. Without them the arm is the island
    // shape CLAUDE.md's rule 1 names: `fire.rs` entirely green, zero blocks
    // changed in a running world.
    // ---------------------------------------------------------------------

    /// A `ChunkSource` with real storage, because a fire tick reads back the
    /// cells it writes and reads the cell *below* itself. `RecordingWorld`
    /// answers air for every read no matter what was set, which cannot express
    /// "fire over netherrack".
    struct OverlayWorld(Arc<Mutex<std::collections::HashMap<(i32, i32, i32), String>>>);

    impl OverlayWorld {
        fn with(cells: &[((i32, i32, i32), &str)]) -> Arc<Self> {
            let map = cells
                .iter()
                .map(|&(pos, state)| (pos, state.to_owned()))
                .collect();
            Arc::new(Self(Arc::new(Mutex::new(map))))
        }

        fn get(&self, pos: (i32, i32, i32)) -> String {
            self.0
                .lock()
                .expect("overlay world lock poisoned")
                .get(&pos)
                .cloned()
                .unwrap_or_else(|| crate::chunk::AIR.to_owned())
        }
    }

    impl ChunkSource for OverlayWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 16)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.get((x, y, z))
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.0
                .lock()
                .expect("overlay world lock poisoned")
                .insert((x, y, z), name.to_owned());
        }
    }

    /// Runs the loop for a handful of ticks with one `TICK_FIRE` already due at
    /// `pos`, and reports `(the cell afterwards, everything published)`.
    async fn fire_tick_once(
        below: &str,
        pos: (i32, i32, i32),
    ) -> (String, Vec<(i32, i32, i32, String)>) {
        let (x, y, z) = pos;
        let world = OverlayWorld::with(&[
            ((x, y, z), "minecraft:fire[age=0]"),
            ((x, y - 1, z), below),
        ]);
        let scheduled = crate::region_source::ScheduledTickHandle::default();
        // Due at tick 1, which is the first tick the loop drains.
        scheduled.with(|queues| {
            queues.block.schedule(
                pos,
                crate::fire::TICK_FIRE.to_owned(),
                1,
                TickPriority::Normal,
            );
        });
        let feed = BlockTickFeed::default();
        let (mobs, out, block_entities) = handles();
        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::new(TickClock::new()),
            Arc::clone(&world),
            feed.clone(),
            (0..=0, 0..=0),
            ExplosionFeed::default(),
            scheduled,
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;

        let mut published = Vec::new();
        for _ in 0..4 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            published.extend(feed.drain_all());
        }
        (world.get(pos), published)
    }

    /// A fire with nothing under it and nothing flammable beside it fails
    /// `FireBlock`'s `canSurvive` and is removed — and that removal happens
    /// **before any RNG draw**, so the predicted cell state is exactly
    /// `minecraft:air` with no distribution involved. It must also reach
    /// `BlockTickFeed`, or a connected client keeps rendering a fire the server
    /// has already deleted.
    #[tokio::test(start_paused = true)]
    async fn the_loop_runs_a_due_fire_tick_and_publishes_the_change() {
        let pos = (3, 5, 3);
        let (cell, published) = fire_tick_once(crate::chunk::AIR, pos).await;
        assert_eq!(
            cell, "minecraft:air",
            "an unsupported fire fails canSurvive and must be removed; if the cell still \
             holds fire, the drain never dispatched TICK_FIRE at all"
        );
        assert!(
            published.contains(&(pos.0, pos.1, pos.2, "minecraft:air".to_owned())),
            "the removal must reach the wire feed: published = {published:?}"
        );
    }

    /// **The control**, and the reason the assertion above is about fire rather
    /// than about "the loop clears cells": identical loop, identical schedule,
    /// identical position — netherrack underneath. `face_sturdy_up` is then true,
    /// so `canSurvive` passes and the cell must still hold fire. Netherrack is
    /// also `#infiniburn_overworld`, which suppresses the rain-out draw, so this
    /// arm has no way to reach air at all.
    ///
    /// Only the age may move (`min(15, age + nextInt(3) / 2)`), so the assertion
    /// is on the block rather than the whole state string.
    #[tokio::test(start_paused = true)]
    async fn a_supported_fire_survives_its_tick() {
        let pos = (3, 5, 3);
        let (cell, _) = fire_tick_once("minecraft:netherrack", pos).await;
        assert!(
            cell.starts_with("minecraft:fire"),
            "fire over netherrack passes canSurvive and must persist, got {cell:?} — if this \
             is air, the arm is removing fire unconditionally rather than running FireBlock::tick"
        );
    }

    // ---------------------------------------------------------------------
    // Issue #320: the dispenser fire arm. `crate::redstone_dispenser`'s own
    // tests gate `random_slot`/`plain_toss` in isolation; this one gates the
    // *wiring* — that the drain actually reaches a live container and mob
    // simulation through the production `run_tick_loop`, the island shape
    // CLAUDE.md's rule 1 names (a correct module with zero production
    // callers). Before this arm existed, `TICK_DISPENSER_FIRE` was scheduled
    // and never drained, so a dispenser filled with arrows sat there forever.
    // ---------------------------------------------------------------------

    /// A `ChunkSource` whose `column()` **reflects its own edits** — unlike
    /// [`OverlayWorld`] above, whose `column()` is hardcoded blank air
    /// because fire's own arm reads through `world.block_state(...)`
    /// directly and never through the returned column at all. The dispenser
    /// arm reads its block state via `column.block_state(...)`, exactly
    /// like every other arm in the big `for due in
    /// block_ticks.drain_due(...)` loop (torch/repeater/comparator/
    /// tripwire/gravity), so a double for it has to answer through that
    /// path too, or `is_dispenser_family` sees blank air and the arm is a
    /// silent no-op — which is exactly the failure mode this struct exists
    /// to avoid reproducing inside the test itself.
    struct ColumnBackedWorld(Arc<Mutex<std::collections::HashMap<(i32, i32, i32), String>>>);

    impl ColumnBackedWorld {
        fn with(cells: &[((i32, i32, i32), &str)]) -> Arc<Self> {
            let map = cells
                .iter()
                .map(|&(pos, state)| (pos, state.to_owned()))
                .collect();
            Arc::new(Self(Arc::new(Mutex::new(map))))
        }
    }

    impl ChunkSource for ColumnBackedWorld {
        fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
            let mut column = crate::chunk::ChunkColumn::new(0, 16);
            for (&(x, y, z), state) in self.0.lock().expect("column-backed world lock poisoned").iter() {
                if x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                    column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
                }
            }
            column
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.0
                .lock()
                .expect("column-backed world lock poisoned")
                .get(&(x, y, z))
                .cloned()
                .unwrap_or_else(|| crate::chunk::AIR.to_owned())
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.0
                .lock()
                .expect("column-backed world lock poisoned")
                .insert((x, y, z), name.to_owned());
        }
    }

    /// East is the discriminating facing: the dispenser's own cell
    /// (`center.x`), an opposite-face (`west`) toss (`center.x - 0.7`) and
    /// the real dispense point (`center.x + 0.7`) are three different `x`
    /// values, so a wrong hypothesis for *where* the item lands is
    /// falsifiable rather than merely "an item appeared somewhere".
    #[tokio::test(start_paused = true)]
    async fn the_loop_drains_a_dispenser_fire_and_the_item_lands_off_the_east_face() {
        let pos = (11, 6, 9);
        let (px, py, pz) = pos;
        let world = ColumnBackedWorld::with(&[(pos, "minecraft:dispenser[facing=east,triggered=true]")]);
        let scheduled = crate::region_source::ScheduledTickHandle::default();
        scheduled.with(|queues| {
            queues.block.schedule(
                pos,
                crate::redstone_dispenser::TICK_DISPENSER_FIRE.to_owned(),
                1,
                TickPriority::Normal,
            );
        });
        let feed = BlockTickFeed::default();
        let (mobs, out, block_entities) = handles();
        block_entities.with(|reg| {
            let mut container =
                crate::block_entities::BlockEntity::container_of_size("minecraft:dispenser", crate::block_entities::CONTAINER_3X3_SIZE);
            container.set_container_slot(0, Some(lodestone_model::ItemStack::new(
                "minecraft:arrow".parse().expect("valid item key"),
                3,
            )));
            reg.insert(BlockPos::new(px, py, pz), container);
        });
        tokio::spawn(run_tick_loop(
            mobs.clone(),
            out,
            block_entities.clone(),
            Arc::new(TickClock::new()),
            Arc::clone(&world),
            feed.clone(),
            (0..=0, 0..=0),
            ExplosionFeed::default(),
            scheduled,
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;

        // Poll one tick at a time and stop the instant the item exists, so the
        // captured position is `plain_toss`'s own output with **zero** falling
        // ticks applied yet — `MobSim::tick` (which steps gravity) runs earlier
        // in the loop body than the scheduled-tick drain that spawns this item,
        // so a freshly spawned entity gets its first physics step only on the
        // *next* iteration.
        let mut spawned_at_tick = None;
        for tick in 1..=8 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            if mobs.with(|sim| sim.item_count()) >= 1 {
                spawned_at_tick = Some(tick);
                break;
            }
        }
        assert!(
            spawned_at_tick.is_some(),
            "no item ever appeared — the drain never dispatched TICK_DISPENSER_FIRE at all"
        );

        let snapshots = mobs.with(|sim| sim.snapshots());
        assert_eq!(
            snapshots.len(),
            1,
            "exactly one item must exist: {snapshots:?}"
        );
        assert_eq!(
            snapshots[0].entity_type.to_string(),
            "minecraft:item",
            "a dispensed stack is an item entity, not something drawing as the block itself"
        );
        assert_eq!(
            snapshots[0].metadata,
            vec![crate::protocol::MetadataField::Item {
                item: "minecraft:arrow".parse().expect("valid key"),
                count: 1,
            }],
            "exactly one arrow leaves the stack of three — `ItemStack.split(1)`, not the whole stack"
        );

        let center_x = f64::from(px) + 0.5;
        let expected_x = center_x + 0.7; // east: `DISPENSE_SCALE` out from centre.
        let own_cell_x = center_x; // wrong hypothesis: tossed at the dispenser's own cell.
        let opposite_x = center_x - 0.7; // wrong hypothesis: facing read backwards (west).
        let position = snapshots[0].position;
        assert!(
            (position.x - expected_x).abs() < 1e-6,
            "position.x = {} does not match the predicted east dispense point {expected_x}",
            position.x
        );
        assert_eq!(
            position.z,
            f64::from(pz) + 0.5,
            "east does not move z at all — a nonzero drift here means the facing math crossed axes"
        );
        // The pair that makes this discriminating: a value strictly between
        // `own_cell_x` and `expected_x` rules out both wrong hypotheses at
        // once, since `own_cell_x` sits exactly halfway between `expected_x`
        // and `opposite_x` by construction (`centre ± DISPENSE_SCALE`).
        assert!(
            position.x > own_cell_x + 0.2,
            "position.x = {} sits at or behind the dispenser's own cell ({own_cell_x}) — the arm \
             tossed the item at its own position rather than off the facing side",
            position.x
        );
        assert!(
            position.x > opposite_x + 0.2,
            "position.x = {} sits toward the opposite (west) face's {opposite_x} rather than \
             east's {expected_x} — the arm read the facing backwards",
            position.x
        );
        assert!(
            (position.y - (f64::from(py) + 0.5 - 0.15625)).abs() < 1e-6,
            "position.y = {} does not match the sideways-toss y-shift (0.15625 below centre)",
            position.y
        );

        // And the container itself lost exactly the one item — the other half
        // of "dispensing", not merely "an item entity exists somewhere".
        let remaining = block_entities.with(|reg| {
            reg.get(BlockPos::new(px, py, pz))
                .map(crate::block_entities::BlockEntity::container_slots)
        });
        assert_eq!(
            remaining.as_ref().and_then(|slots| slots[0].as_ref().map(|s| s.count)),
            Some(2),
            "the container's slot 0 must go from 3 to 2, not stay at 3 or empty out entirely: {remaining:?}"
        );
    }
}
