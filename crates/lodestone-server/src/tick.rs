//! The unified server tick clock: a single 20 Hz loop that
//! ticks world state independently of any client connection, plus MSPT/TPS
//! accounting and vanilla-shaped overrun handling.
//!
//! # Before this module
//!
//! Nothing in this crate had one clock. Six independent timers already
//! existed, each a local `tokio::time::interval`/`Duration` literal, each
//! reinventing "one vanilla tick is 50ms" on its own:
//!
//! | timer | former location | cadence |
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

use std::collections::{HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy_ecs::world::World;
use crate::block_entities::BlockEntityHandle;
use crate::border::{BorderFeed, WorldBorder};
use crate::chunk::ChunkSource;
use crate::mobs::{
    Detonation, EntityTickEffectBatch, EntityTickOwner, LiveMobSource, MobHandle, MobSim,
};
use lodestone_entity::ai::mob::EatenBlock;
use crate::random_tick::RandomTickScheduler;
use crate::scheduled_tick::{
    merge_due_owner_batches, ScheduledTick, ScheduledTickOwnerBatch, ScheduledTickQueueAccess,
    TickPriority,
};
use crate::sleep::{SleepEvent, SleepFeed, SleepState, SleepVote};
use crate::weather::{WeatherFeed, WeatherState};
use lodestone_model::BlockPos;

/// The natural-spawn driver's RNG seed. A fixed literal, like every other seed
/// in this module (`RANDOM_TICK_POSITION_SEED` and friends): the world seed is
/// not threaded into this loop, and the spawn stream only has to be *reproducible*,
/// not world-derived.
const NATURAL_SPAWN_SEED: u64 = 0x5350_4157_4E45_5221;

/// Whether the weather cycle's timers advance.
///
/// The tick loop does not yet read this rule from the shared
/// [`crate::world_state::WorldStateHandle`], so it uses the default value of
/// `true`. Keeping the fallback behind this function gives the stored rule one
/// call site to replace.
fn advance_weather() -> bool {
    true
}

/// The fraction of players whose vote is required to skip the night.
///
/// The tick loop does not yet read this setting from the shared world-rule
/// store, so `100` is used until it does. [`SleepState::sleepers_needed`]'s
/// `max(1, …)` floor still makes singleplayer require exactly one sleeper.
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

/// This module's own re-export of `tokio::time::Instant`, under a name that is
/// not the confinement rule's banned literal.
///
/// `scripts/wasm-check.sh`'s `lodestone-server tokio-instant-ban` rule greps
/// this crate's `src/` for the literal text `tokio::time::Instant` and allows
/// exactly one file to say it: this one, because [`run_tick_loop`] below has a
/// real, load-bearing use — `next_tick_at`/`last_overload_warning_at` — that
/// `wasm32` never reaches (`open_in_memory`'s own doc names this module as the
/// one it deliberately never spawns). `server.rs`'s native-only `serve_play`
/// needed the identical type for its own keep-alive/time-sync/vitals/
/// container-sync timers and, before this alias existed, spelled the literal
/// out five ways (a bare parameter type, a struct field, and three separate
/// `::now()` call shapes) — eleven sites the grep could see and the compiler
/// could not, since the whole function is `#[cfg(not(target_arch = "wasm32"))]`
/// and therefore already absent from a `wasm32` build regardless. The rule does
/// not know that; it is a textual guard, not a reachability analysis, and this
/// repo's own doc says why that is deliberate ("make it CHECKABLE"). Naming the
/// type through here rather than repeating the literal is the same "one
/// confined home" shape [`crate::server::JoinStopwatch`] already uses for
/// `web_time::Instant` — the difference is only that `serve_play`'s timers need
/// a *tokio* clock (for `tokio::time::interval_at`/`Interval::tick`), which
/// `web_time::Instant` cannot stand in for.
///
/// Do not add a second file to the rule's allowlist to route around this —
/// that is the "decorative guard" failure mode this repo has hit before. Reuse
/// this alias, or if a third file genuinely needs the real clock, give it a
/// documented reason exactly like this one's.
pub(crate) type PlayTimerInstant = tokio::time::Instant;

/// Rolling-average window for [`TickStats::mspt_avg_ms`] — matches vanilla's
/// own `tickTimesNanos` ring buffer size
/// (`private final long[] tickTimesNanos = new long[100];`).
pub const TICK_HISTORY_LEN: usize = 100;

/// One coarse phase of [`run_tick_loop`]'s body, for per-phase timing.
///
/// Deliberately three, not finer. The back two thirds of the tick body run
/// inside `scheduled.with`'s closure — block-tick drain, fire/redstone/fluid
/// propagation, random ticks, falling blocks, vehicles, TNT, minecarts,
/// dragons — which holds the scheduled-tick queue mutex across its whole
/// extent (see that closure's own doc comment, and this module's own record
/// of the self-deadlock a re-entrant call into that same mutex caused). A
/// phase boundary here is a bare timestamp with no lock taken and nothing
/// called back into `scheduled`, so it cannot deadlock; splitting the third
/// phase further would mean scattering timestamps through ~1,000 lines of
/// mob/redstone logic another agent may be editing concurrently, which is a
/// collision risk this instrument does not need to take just to answer
/// "which third of the tick dominates". If a future pass wants a finer split
/// of that phase, do it from *inside* `scheduled.with` once no other agent
/// holds this file, not by widening the lock-safety argument above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TickPhase {
    /// World border tick, dropped-item settling against live terrain, mob
    /// removal (peaceful), the natural spawn cycle, the despawn pass,
    /// patrols, wandering traders, detonations, block drops, mob grazing,
    /// vocalisations, projectile block hits, spawner blocks.
    MobsAndItems = 0,
    /// The weather cycle (`WeatherState::tick`) and the night-skip vote.
    WeatherAndSleep = 1,
    /// Everything inside `scheduled.with`: the scheduled block-tick drain,
    /// fire/redstone/fluid propagation, random ticks, falling blocks,
    /// vehicles, TNT, minecarts, dragons. The only phase that calls
    /// `world.column()` — a chunk-boundary block tick can trigger real
    /// worldgen — so it is the phase a keep-alive-timeout-shaped stall (see
    /// this module's own doc for the incident this instrument exists to
    /// catch a repeat of) would show up in.
    ScheduledAndPhysics = 2,
}

/// [`TickPhase`] variant count — keep in sync with the enum by construction
/// (every array below is sized off this, not off a second literal).
const TICK_PHASE_COUNT: usize = 3;

/// [`TickPhase`] names in discriminant order, for a report that wants to
/// join a phase index back to a label.
pub(crate) const TICK_PHASE_NAMES: [&str; TICK_PHASE_COUNT] =
    ["mobs_and_items", "weather_and_sleep", "scheduled_and_physics"];

/// Above this, one phase in one tick counts as "over budget" rather than
/// only contributing a sample to that phase's percentile record — 20% of
/// the 50ms tick period. One threshold shared by all three phases rather
/// than three tuned constants: nothing has established a real per-phase
/// budget yet, and an unjustified separate number per phase would be
/// exactly the "predict the plausible round number" failure this crate's
/// own evidence-standard rules warn about. Revisit once real per-phase
/// percentiles from a loaded server exist to derive one from.
const PHASE_SOFT_BUDGET: Duration = Duration::from_millis(MILLIS_PER_TICK / 5);

/// The single largest [`TickPhase`] duration a [`TickClock`] has ever
/// recorded, and which phase and (approximately) which tick it was — "the
/// worst unserviced window, named", as opposed to a rolling percentile that
/// forgets anything older than [`TICK_HISTORY_LEN`] samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorstPhaseWindow {
    pub phase: TickPhase,
    pub micros: u64,
    /// [`TickClock::tick_count`] read at the moment this was recorded. Every
    /// phase for a given tick is recorded before that tick's own
    /// `record_tick` call increments the counter, so this is "the tick
    /// index this phase belonged to", not off by a whole tick — but it is a
    /// diagnostic label, not a value anything asserts equality on.
    pub tick_count: u64,
}

/// A percentile summary of one [`TickPhase`]'s recorded durations — the
/// tail, not the mean, per this crate's own "measure the tail" rule: a
/// keep-alive timeout here was once diagnosed from an average that hid the
/// one window that actually mattered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseStats {
    pub phase: TickPhase,
    /// How many samples this is derived from — at most [`TICK_HISTORY_LEN`],
    /// because the ring buffer drops the oldest sample past that.
    pub sample_count: u64,
    /// Total samples recorded for this phase since the clock was created.
    /// Unlike [`Self::sample_count`], this counter is never limited by the
    /// percentile record window.
    pub total_sample_count: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Total ticks (not bounded by the ring buffer — a plain running
    /// counter) where this phase exceeded [`PHASE_SOFT_BUDGET`].
    pub over_budget_count: u64,
}

/// Cumulative work observed at the chunk-owner boundaries of the live tick
/// loop. These are counts, not timings: a profile can join them to a phase
/// sample without treating a machine-dependent duration as an invariant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnerTickStats {
    /// Selected chunk owners that completed the random-tick pass. This counts
    /// ownership visits, not block changes: an empty column still consumed the
    /// owner's deterministic random-number draws.
    pub random_tick_owned_chunks: u64,
    /// Selected chunk owners that completed the thunder-decision pass. This
    /// counts ownership visits, not lightning bolts: most visits make no
    /// strike decision visible to a client.
    pub thunder_owned_chunks: u64,
    /// Due scheduled block ticks handed to the central drain.
    pub scheduled_block_ticks: u64,
    /// Due scheduled fluid ticks handed to the central drain.
    pub scheduled_fluid_ticks: u64,
    /// Block-entity owner batches returned to the central world writer.
    pub block_entity_batches: u64,
    /// Visible block-entity effects contained in those batches.
    pub block_entity_effects: u64,
    /// Ambient entity-effect owner batches returned to the central publisher.
    pub entity_effect_batches: u64,
    /// Ambient entity effects contained in those batches.
    pub entity_effects: u64,
}

/// Vanilla's own per-queue drain cap, `ServerLevel.MAX_SCHEDULED_TICKS_PER_TICK`
/// — see `crate::scheduled_tick`'s module doc for the
/// full citation of `blockTicks.tick(tick, 65536, ...)`/`fluidTicks.tick(tick,
/// 65536, ...)`.
const MAX_SCHEDULED_TICKS_PER_TICK: usize = 65536;

/// How many ticks after world open to defer the first random-tick pass
/// The seed task in
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
/// tick is a pass and reported **zero** columns when the deferral was introduced
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
/// [`docs/block-entity-tick-distance.md`](../../../docs/block-entity-tick-distance.md).
///
/// A gate that counts `world.column()` calls over this loop must therefore say
/// which caller it is attributing them to. `chunk_store`'s pair is only clean
/// because it passes an **empty** `BlockEntityHandle`.
pub(crate) const INITIAL_RANDOM_TICK_DEFERRAL_TICKS: u64 = 40;

/// Seeds for [`RandomTickScheduler`]'s two independent generators (issue
/// The position LCG is seeded from a level-local state value,
/// arbitrary thread-local draw at level creation — this crate has no
/// per-world seed store to draw a "real" one from yet, so these are fixed
/// literals rather than derived from the world seed. Picking a different
/// (still fixed) literal changes nothing about which blocks are *eligible*
/// for a random tick, only the pseudo-random order/positions they are
/// visited in — see `random_tick.rs`'s own doc comment for why the draw
/// *pattern*, not the literal values, is what this crate's tests assert on.
const RANDOM_TICK_POSITION_SEED: i32 = 0x5EED_1234u32 as i32;
const RANDOM_TICK_BEHAVIOR_SEED: u64 = 0x5EED_5678;

/// Seed for the driver's own [`crate::lightning::block_random_pos`] LCG state
/// (`Level.randValue`'s counterpart, for lightning target selection rather
/// than random ticks) — [`RANDOM_TICK_POSITION_SEED`]'s own reasoning
/// restated for a second, independent LCG: any fixed literal is fine, since
/// nothing here asserts on the exact sequence, only that strikes land inside
/// their per-chunk column.
const LIGHTNING_RAND_VALUE_SEED: i32 = 0x5EED_4C49u32 as i32;

/// A shared feed of block changes the world tick loop wants every connection
/// to learn about. The current producer is grass ↔ dirt via
/// `crate::random_tick`. Mirrors [`LiveMobSource`]'s
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
/// **[`IntegratedServer::bind`] (LAN) spawns a tick loop** and relays its hub
/// feed to every active connection. It does *not*, however, hand the
/// same instance to several connections, which is what would actually break:
/// each LAN connection gets its **own** feed pair and a relay arm in `bind`'s
/// accept loop drains the tick loop's hub feed and re-publishes into all of
/// them. That is a fan-out in front of this type rather than the
/// per-connection cursor over a shared append-only log this comment used to
/// recommend — the cursor is still the better shape (it needs no copy per
/// subscriber), and it is what to build if the subscriber count ever grows
/// past a handful.
/// # The inbound half
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
/// line in `integrated.rs`, which is maintained independently:
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
    /// block ticks a connection's own mutation scheduled, waiting
    /// to be rebased onto the tick loop's counter and hosted in its
    /// `block_ticks` queue. `trigger_tick` is a relative delay.
    Arc<Mutex<Vec<ScheduledTick<String>>>>,
    /// sounds, particles and level events the world tick produced —
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
    /// `Level.playSound(@Nullable Entity except, …)` (vanilla's own `Level`) and of
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

    /// Records one world effect for every player to learn about on
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
    /// See the struct doc comment: outbound must be per-connection
    /// because it is drain-all, inbound must be shared because the tick loop
    /// is the only drainer.
    ///
    /// Called from `IntegratedServer`'s `LanSubscriber` construction (both the
    /// single-connection `publish`/`publish_with_config` path and the
    /// multi-connection `open_to_lan` relay), and exercised directly by
    /// `a_subscriber_shares_the_inbound_queue_and_splits_the_outbound_one`.
    pub(crate) fn subscriber(&self) -> Self {
        Self(Arc::default(), Arc::clone(&self.1), Arc::default())
    }

    /// Hands the tick loop block ticks a caller wants resumed against a live
    /// world — production-internal use is connection-triggered
    /// placement path (`crate::server`), and this is also the hook a
    /// captured-contraption benchmark re-injects a schematic's own
    /// `PendingBlockTicks` through (`crates/lodestone-anvil/tests/redstone_benchmark.rs`,
    /// `docs/oracles-and-benchmarks.md`) — a schematic stamped with a raw
    /// [`crate::ChunkSource::set_block`] carries no scheduled tick of its
    /// own, so this is the only way to resume a captured circuit mid-cycle
    /// instead of only measuring an inert, perfectly-settled one.
    ///
    /// Each entry's `trigger_tick` is a **delay in ticks**, not an absolute
    /// tick — the publisher (a connection task, or a caller with no tick loop
    /// of its own to be absolute against) has no live `game_tick` counter to
    /// measure from; [`crate::tick::run_tick_loop`] rebases each one onto its
    /// own counter on drain.
    pub fn request_scheduled_ticks(&self, ticks: Vec<ScheduledTick<String>>) {
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
/// to learn about — the exact same idiom [`BlockTickFeed`]
/// already establishes just above, applied to
/// [`MobSim::take_detonations`]'s own drain instead of a random-ticked block
/// change. `MobSim::tick` already discards its `explode` return entirely
/// before this. Exposure and damage calculations had two production
/// callers, both direct-explosion tests calling `MobSim::explode` by hand,
/// and zero path from "a creeper's fuse completed" to anything a client
/// could see) — this is what [`run_tick_loop`] publishes into so
/// `crate::server::serve_play`'s own `container_sync_tick` arm can forward a
/// real `EXPLODE` packet, the same way it already forwards
/// [`BlockTickFeed`]'s random-tick changes.
///
/// Same single-consumer caveat as [`BlockTickFeed`], and the same resolution
/// for LAN: singleplayer has exactly one connection task per feed
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
/// A no-op for every other block, so call sites need no guard of their own.
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

/// Posts `VibrationEvent::NoteBlockPlay` for a real note-block pulse — the
/// closing half of `crate::redstone_note_block`'s own previously-disclosed
/// (now re-verified false) "nowhere for the pulse to land" gap. Called at
/// every one of this file's four `propagate_and_react_with_entities`
/// consumers, so every code path that can trigger a note block (a rising
/// redstone edge, wherever it originates) reaches an allay's ear the same
/// way. `world.block_state` is read **after** this event's write has completed,
/// matching every other post-write consumer in this file
/// (`publish_moving_piston` reads the committed state the same way) — the
/// block above the note block is unaffected by that write either way.
fn post_note_block_vibration<W: crate::chunk::ChunkSource>(
    world: &std::sync::Arc<W>,
    mobs: &MobHandle,
    pos: (i32, i32, i32),
    from: &str,
    to: &str,
) {
    let (x, y, z) = pos;
    let above_is_air = crate::random_tick::is_air_variant(&world.block_state(x, y + 1, z));
    if crate::redstone_note_block::played_pulse_on_transition(from, to, above_is_air) {
        mobs.with(|sim| {
            sim.post_vibration(
                lodestone_model::Vec3::new(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5),
                lodestone_entity::vibration::VibrationEvent::NoteBlockPlay,
                None,
            );
        });
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
fn publish_moving_piston<Q: ScheduledTickQueueAccess<String> + ?Sized>(
    out: &BlockTickFeed,
    block_ticks: &Q,
    x: i32,
    y: i32,
    z: i32,
    state: &str,
) {
    if !crate::piston::is_moving_piston(state) {
        return;
    }
    let Some(entity) = block_ticks
        .matching_at((x, y, z), |kind| crate::piston::is_finish_kind(kind))
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

/// entity-aware reaction surface: shoves every mob standing in
/// a moving piston cell's own swept path, the instant that cell first
/// appears. Called at the same four `propagate_and_react_with_entities`
/// consumers [`publish_moving_piston`] already is, and reads the identical
/// pending-commit record that function does — see
/// `crate::mobs::piston_shove`'s own module doc for why the wiring lives
/// entirely in this file's consumers rather than in `crate::piston`/
/// `crate::random_tick`, and for why `source = dest - push_direction` is
/// the whole of the query region regardless of which cell (a pushed block,
/// an extending head, a retracting base) this is.
///
/// **Item 4 of the same issue rides along here too.** `MobSim` never holds a
/// connected player (position is client-reported — see
/// `crate::mobs::piston_shove`'s own module doc), so a player cannot be
/// shoved through the same call `sim.shove_from_piston` makes below. This
/// publishes a [`crate::effects::WorldEffect::PistonPlayerPush`] alongside it
/// — a server-side-only signal (see that variant's own doc) — carrying
/// exactly the two cells and the push direction a connection needs to
/// correct its own last-known position, without adding a `PlayerRegistry`
/// parameter to `run_tick_loop`'s already-long signature and its two dozen
/// callers.
///
/// A no-op for any other state, so a caller can hand it every block change
/// it publishes without testing first — the same convention
/// [`publish_moving_piston`] already establishes.
fn shove_entities_from_piston<Q: ScheduledTickQueueAccess<String> + ?Sized>(
    mobs: &MobHandle,
    block_tick_out: &BlockTickFeed,
    block_ticks: &Q,
    x: i32,
    y: i32,
    z: i32,
    state: &str,
) {
    if !crate::piston::is_moving_piston(state) {
        return;
    }
    let Some(entity) = block_ticks
        .matching_at((x, y, z), |kind| crate::piston::is_finish_kind(kind))
        .and_then(|pending| crate::piston::parse_finish_kind(&pending.kind))
    else {
        return;
    };
    let push_direction = entity.push_direction();
    let dest = BlockPos::new(x, y, z);
    let source = push_direction.opposite().relative(dest);
    mobs.with(|sim| {
        sim.shove_from_piston(source, dest, push_direction);
    });
    block_tick_out.publish_effect(crate::effects::WorldEffect::PistonPlayerPush {
        source,
        dest,
        push_delta: crate::piston::push_delta(push_direction),
    });
}

/// How far behind wall-clock schedule the loop must fall before it gives up
/// trying to catch up and forgives the backlog, matching the real per-world
/// run-server overload check, transcribed as the rule it implements: the
/// server is overloaded once the elapsed time since the next scheduled tick
/// exceeds a fixed threshold plus twenty ticks' worth of this tick's own
/// duration.
///
/// The real overload threshold is
/// exactly one second; the "twenty ticks' worth of this tick's own duration"
/// term is 20 ticks' worth of the tick period (1s at 50ms/tick
/// here, since this crate has no tick-rate manager/sprinting to vary
/// that duration with). Total: **2 seconds** behind before the real engine —
/// and
/// this loop — gives up on the backlog.
fn overload_threshold() -> Duration {
    Duration::from_secs(1) + TICK_PERIOD * 20
}

/// Throttles how often the overload warning re-fires once triggered, matching
/// vanilla's own main server loop's
/// `this.nextTickTimeNanos - this.lastOverloadWarningNanos >=
/// OVERLOADED_WARNING_INTERVAL_NANOS + 100L * thisTickNanos`.
/// `OVERLOADED_WARNING_INTERVAL_NANOS` (vanilla's own constant) is 10
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
/// catch up indefinitely" behavior, extracted specifically so it
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
/// Vanilla's own field (`lastOverloadWarningNanos`)
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

/// MSPT/TPS/overrun accounting for one [`run_tick_loop`].
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
    /// Per-[`TickPhase`] rolling duration record, same shape and cap as
    /// `record` above, indexed by the phase's discriminant.
    phase_history: [Mutex<VecDeque<u64>>; TICK_PHASE_COUNT],
    /// Per-phase cumulative sample counts. This remains separate from the
    /// bounded histories so callers can prove a long-running clock reached
    /// each recorder even after its percentile window fills.
    phase_sample_count: [AtomicU64; TICK_PHASE_COUNT],
    /// Per-phase "exceeded [`PHASE_SOFT_BUDGET`]" counts — a counter, not a
    /// duration, so it stays cheap and load-invariant to read even after
    /// millions of ticks, unlike re-deriving it from the (bounded) record.
    phase_over_budget: [AtomicU64; TICK_PHASE_COUNT],
    owner_stats: [AtomicU64; 8],
    /// The largest single phase duration ever recorded, and which phase.
    worst_phase: Mutex<Option<WorstPhaseWindow>>,
}

impl Default for TickClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TickClock {
    /// A fresh clock: zero ticks, zero overruns, empty record.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tick_count: AtomicU64::new(0),
            last_mspt_micros: AtomicU64::new(0),
            overrun_count: AtomicU64::new(0),
            history: Mutex::new(VecDeque::with_capacity(TICK_HISTORY_LEN)),
            phase_history: std::array::from_fn(|_| Mutex::new(VecDeque::with_capacity(TICK_HISTORY_LEN))),
            phase_sample_count: [const { AtomicU64::new(0) }; TICK_PHASE_COUNT],
            phase_over_budget: [const { AtomicU64::new(0) }; TICK_PHASE_COUNT],
            owner_stats: [const { AtomicU64::new(0) }; 8],
            worst_phase: Mutex::new(None),
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
        if history.len() == TICK_HISTORY_LEN {
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

    /// Records one [`TickPhase`]'s wall-clock duration for the tick
    /// currently in progress.
    ///
    /// Every production call site is a bare timestamp taken outside any
    /// lock this loop holds and outside `scheduled.with`'s closure — see
    /// [`TickPhase`]'s own doc for why that boundary is load-bearing rather
    /// than incidental. This function itself only ever locks its own
    /// `phase_history`/`worst_phase` mutexes, neither of which any other
    /// code in this module locks transitively, so calling it cannot
    /// self-deadlock the way a call into `scheduled` from inside its own
    /// closure would.
    pub(crate) fn record_phase(&self, phase: TickPhase, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let idx = phase as usize;
        self.phase_sample_count[idx].fetch_add(1, Ordering::Relaxed);
        {
            let mut history = self.phase_history[idx]
                .lock()
                .expect("tick phase history lock poisoned");
            if history.len() == TICK_HISTORY_LEN {
                history.pop_front();
            }
            history.push_back(micros);
        }
        if elapsed > PHASE_SOFT_BUDGET {
            self.phase_over_budget[idx].fetch_add(1, Ordering::Relaxed);
        }
        let mut worst = self.worst_phase.lock().expect("worst tick phase lock poisoned");
        if worst.is_none_or(|w| micros > w.micros) {
            *worst = Some(WorstPhaseWindow { phase, micros, tick_count: self.tick_count() });
        }
    }

    /// Adds work performed at the tick loop's owner hand-off boundaries.
    pub(crate) fn record_owner_work(&self, stats: OwnerTickStats) {
        for (counter, value) in self.owner_stats.iter().zip([
            stats.random_tick_owned_chunks,
            stats.thunder_owned_chunks,
            stats.scheduled_block_ticks,
            stats.scheduled_fluid_ticks,
            stats.block_entity_batches,
            stats.block_entity_effects,
            stats.entity_effect_batches,
            stats.entity_effects,
        ]) {
            counter.fetch_add(value, Ordering::Relaxed);
        }
    }

    /// Snapshot the owner-boundary work accumulated since this clock started.
    #[must_use]
    pub fn owner_stats(&self) -> OwnerTickStats {
        let read = |index: usize| self.owner_stats[index].load(Ordering::Relaxed);
        OwnerTickStats {
            random_tick_owned_chunks: read(0),
            thunder_owned_chunks: read(1),
            scheduled_block_ticks: read(2),
            scheduled_fluid_ticks: read(3),
            block_entity_batches: read(4),
            block_entity_effects: read(5),
            entity_effect_batches: read(6),
            entity_effects: read(7),
        }
    }

    /// A percentile summary of `phase`'s recorded durations — see
    /// [`PhaseStats`]. Sorts a clone of the ring buffer (bounded at
    /// [`TICK_HISTORY_LEN`] samples), so this is cheap enough for a debug command
    /// or a test to call, but it is not itself called from the tick loop.
    #[must_use]
    pub fn phase_stats(&self, phase: TickPhase) -> PhaseStats {
        let idx = phase as usize;
        let mut samples: Vec<u64> = {
            let history = self.phase_history[idx]
                .lock()
                .expect("tick phase history lock poisoned");
            history.iter().copied().collect()
        };
        samples.sort_unstable();
        let sample_count = samples.len();
        let percentile = |p: f64| -> f64 {
            if sample_count == 0 {
                return 0.0;
            }
            let rank = ((p * sample_count as f64).ceil() as usize).clamp(1, sample_count) - 1;
            samples[rank] as f64 / 1000.0
        };
        PhaseStats {
            phase,
            sample_count: sample_count as u64,
            total_sample_count: self.phase_sample_count[idx].load(Ordering::Relaxed),
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms: samples.last().copied().unwrap_or(0) as f64 / 1000.0,
            over_budget_count: self.phase_over_budget[idx].load(Ordering::Relaxed),
        }
    }

    /// The largest single [`TickPhase`] duration this clock has ever
    /// recorded, and which phase — `None` before the first phase is
    /// recorded.
    #[must_use]
    pub fn worst_phase_window(&self) -> Option<WorstPhaseWindow> {
        *self.worst_phase.lock().expect("worst tick phase lock poisoned")
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
            mobs_and_items: self.phase_stats(TickPhase::MobsAndItems),
            weather_and_sleep: self.phase_stats(TickPhase::WeatherAndSleep),
            scheduled_and_physics: self.phase_stats(TickPhase::ScheduledAndPhysics),
            owner_work: self.owner_stats(),
            worst_phase_window: self.worst_phase_window(),
        }
    }
}

/// A point-in-time snapshot of [`TickClock`]'s accounting.
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
    /// Snapshot of the first tick phase's percentile summary.
    pub mobs_and_items: PhaseStats,
    /// Snapshot of the second tick phase's percentile summary.
    pub weather_and_sleep: PhaseStats,
    /// Snapshot of the scheduled and physics phase's percentile summary.
    pub scheduled_and_physics: PhaseStats,
    /// Cumulative scheduled and chunk-owner work from this live tick loop.
    pub owner_work: OwnerTickStats,
    /// Largest phase duration seen since this clock was created.
    pub worst_phase_window: Option<WorstPhaseWindow>,
}

/// `GameRules.MAX_COMMAND_SEQUENCE_LENGTH`'s default (`65536`) — this crate
/// has no gamerule store for it yet, so the `TICK_COMMAND_BLOCK` arm below
/// uses vanilla's own default as a plain bound rather than reading a rule
/// that does not exist, matching `CommandBlock.executeChain`'s own overflow
/// guard in spirit if not in configurability.
const MAX_COMMAND_CHAIN_LENGTH: u32 = 65_536;

/// Runs one command block's `command` as its own synthetic
/// [`crate::commands::CommandSource`] (`crate::command_block
/// ::COMMAND_BLOCK_SOURCE_UUID`, permission level 2 — `Commands
/// .LEVEL_GAMEMASTERS`, matching `LevelBasedPermissionSet.GAMEMASTER` in
/// `CommandBlockEntity`'s own `createCommandSourceStack`) and returns whether
/// it ran (`CommandResponse::is_ran`) — the `TICK_COMMAND_BLOCK` arm below
/// folds that into [`crate::command_block::CommandBlockData::success_count`].
///
/// [`crate::commands::Effect::SetBlock`]/`Fill` are applied inline against
/// `world`, exactly like `crate::server`'s `ChatCommand` arm applies its own
/// caller's self-targeted effects — a command block has the same shape of
/// "self" as that arm's player does, just with no connection to send a
/// directive down. Every other effect kind (aimed at a *different* uuid, via
/// a selector) is dropped rather than queued: this loop has no
/// `PlayerRegistry` in scope to queue it on, the same honest gap
/// `crate::rcon`'s console source already accepts for the same reason.
fn run_command_block_command(
    commands: &crate::commands::ServerCommands,
    world_state: &crate::world_state::WorldStateHandle,
    mobs: &MobHandle,
    world: &dyn ChunkSource,
    block_tick_out: &BlockTickFeed,
    pos: BlockPos,
    facing: crate::neighbor_update::Direction,
    command: &str,
) -> bool {
    let source_uuid = crate::command_block::COMMAND_BLOCK_SOURCE_UUID;
    let source = crate::commands::CommandSource::player(
        source_uuid,
        -1,
        "@",
        lodestone_model::Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.5, f64::from(pos.z) + 0.5),
        lodestone_model::Rotation { yaw: crate::command_block::yaw_for_facing(facing), pitch: 0.0 },
        crate::commands::overworld_dimension(),
        2,
    );
    let command_world = crate::commands::CommandWorld {
        rules: world_state,
        players: &[],
        state: world_state,
        mobs: Some(mobs),
        // not threaded to this helper — a command block running
        // `/worldborder` is a real but niche case, and this function already
        // accepts a comparable gap for `PlayerRegistry` (see its own doc
        // comment) rather than widening its signature for it. `None` is the
        // honest answer, not a silent drop: the command still runs and every
        // other built-in still works, `/worldborder` just refuses here.
        border: None,
        // Same reasoning as `border` above: a command block running
        // `/op`/`/deop`/`/whitelist` is a niche case not worth threading a
        // fifth handle through this helper for.
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        // `/execute if`/`unless block` — this helper already has a chunk
        // source in scope (`world`), unlike `border`/`access` above, so
        // there is nothing to disclose as missing here: a conditional
        // command block gating on the block in front of it is a real,
        // common vanilla pattern.
        blocks: Some(world),
    };
    let Some(outcome) = commands.run(&command_world, &source, command) else {
        return false;
    };
    for directed in outcome.effects {
        if directed.target != source_uuid {
            continue;
        }
        match directed.effect {
            crate::commands::Effect::SetBlock { pos: (x, y, z), block } => {
                world.set_block(x, y, z, &block);
                block_tick_out.publish(x, y, z, block);
            }
            crate::commands::Effect::Fill { positions, block } => {
                for (x, y, z) in positions {
                    world.set_block(x, y, z, &block);
                    block_tick_out.publish(x, y, z, block.clone());
                }
            }
            _ => {}
        }
    }
    outcome.response.is_ran()
}

/// The unified 20 Hz world-tick loop: ticks the live [`MobSim`]
/// and every registered block entity once per [`TICK_PERIOD`], forever,
/// independently of whether any client is connected — replacing the two
/// separate background loops (`mobs::run_mob_tick_loop`,
/// `block_entities::run_block_entity_tick_loop`) that
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] used to spawn
/// side-by-side. Both of those functions have since been deleted as dead
/// code — this is the only loop production spawns now, and the only one
/// left in the tree.
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
/// every iteration (vanilla's own main server loop: `this.nextTickTimeNanos += thisTickNanos;`),
/// and `tokio::time::sleep_until` for an
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
/// vanilla's own main server loop's `this.nextTickTimeNanos += ticks * thisTickNanos;`.
/// The world tick body still runs exactly once
/// per loop iteration, both before and after this adjustment — that backlog,
/// specifically, is forgiven rather than replayed; smaller backlogs are
/// still replayed by the back-to-back iterations described above. A tick
/// that never ran is never counted by [`TickClock::record_tick`], so
/// `tick_count` reflects real work done, not wall-clock elapsed / 50ms.
///
/// # Scheduled ticks and random ticks
///
/// Each iteration additionally drains the block-tick queue, then the
/// fluid-tick queue, then runs random ticks over `tick_area` — in exactly
/// that order, mirroring `ServerLevel.tick`'s own sequence
/// (`blockTicks.tick(...)` before
/// `fluidTicks.tick(...)` before `this.getChunkSource().tick(...)`, which is
/// what eventually calls `tickChunk`'s random ticks — see
/// vanilla's own chunk-cache tick). See [`crate::scheduled_tick`] for the queues'
/// own ordering contract and [`crate::random_tick`] for the random-tick
/// selection and the one block (grass ↔ dirt) modeled end to end.
///
/// **Nothing schedules a block or fluid tick yet** — `block_ticks`/
/// `fluid_ticks` below are drained every iteration (proving the *order* is
/// wired: block before fluid before random, every tick), but no producer in
/// this crate calls [`ScheduledTickQueue::schedule`] on them today. Stated
/// plainly: the scheduled-tick *queue* is
/// real and tested in isolation (`crate::scheduled_tick`'s own test module),
/// but is an acknowledged island here until a block behaviour (fluid flow
/// gravity blocks, redstone) schedules into it. Random
/// ticks are **not** an island: [`RandomTickScheduler::tick_chunk`]
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
    // Compatibility wrapper for world-state behavior. Forwards to the real body
    // with a fresh, permanently-drained-by-nobody [`WeatherFeed`] — the same
    // compatibility shape every `serve_connection*` wrapper uses for a
    // feed it does not carry, and for the same reason: the world-loop's
    // non-weather callers (`crate::chunk_store`'s gate,
    // `crate::redstone_placement_gate`, and this module's own tests) are not
    // maintained outside this module, so the weather feed is additive rather than a
    // tenth parameter. The weather *still advances* here — `WeatherState` is
    // ticked either way — it just publishes into a feed no connection reads.
    // Production (`crate::IntegratedServer::open_in_memory_with_mobs`, and
    // `bind` calls the `_with_weather` variant with a real feed.
    //
    // The same applies to the night-skip vote: fresh [`SleepVote`] and
    // [`SleepFeed`] values that no connection reads. The vote's
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
        // see the wrapper doc above.
        &SleepVote::new(),
        &SleepFeed::default(),
        scheduled,
        // A fresh, unshared world state — the same compatibility shape as the
        // weather feed above. Rules still *apply* here (this loop reads them
        // every tick), they are just at their defaults with nothing able to
        // change them, preserving the wrapper's default behavior.
        crate::world_state::WorldStateHandle::default(),
        follow,
        // Same compatibility shape as every feed above: a fresh, unshared
        // border. It still ticks (a resize's lerp would still advance), it
        // is just never resized by anything and never read by a connection.
        BorderFeed::default(),
    )
    .await
}

/// Runs the weather-aware loop without a server ECS `World`.
///
/// This preserves the generic loop used by independently managed dimensions
/// and test fixtures. [`run_primary_tick_loop_with_weather`] adds the one
/// primary-world behavior without changing those callers.
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
    weather_out: WeatherFeed,
    weather: WeatherState,
    sleep_vote: &SleepVote,
    sleep_feed: &SleepFeed,
    scheduled: crate::region_source::ScheduledTickHandle,
    world_state: crate::world_state::WorldStateHandle,
    follow: crate::tick_area::TickFollow,
    border: BorderFeed,
) where
    W: ChunkSource,
{
    run_tick_loop_with_weather_impl(
        mobs,
        mob_out,
        block_entities,
        clock,
        world,
        block_tick_out,
        tick_area,
        explosion_out,
        weather_out,
        weather,
        sleep_vote,
        sleep_feed,
        scheduled,
        world_state,
        follow,
        border,
        None,
    )
    .await;
}

/// Runs a primary integrated world's tick loop with its server ECS `World`.
///
/// The `World` stays owned by this task for its full lifetime. Its `GameTick`
/// schedule runs after the existing scheduled-and-physics timing sample and
/// immediately before the completed-tick accounting. Loops for independently
/// managed dimensions continue to use [`run_tick_loop_with_weather`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_primary_tick_loop_with_weather<W>(
    server_world: World,
    mobs: MobHandle,
    mob_out: LiveMobSource,
    block_entities: BlockEntityHandle,
    clock: Arc<TickClock>,
    world: Arc<W>,
    block_tick_out: BlockTickFeed,
    tick_area: (RangeInclusive<i32>, RangeInclusive<i32>),
    explosion_out: ExplosionFeed,
    weather_out: WeatherFeed,
    weather: WeatherState,
    sleep_vote: &SleepVote,
    sleep_feed: &SleepFeed,
    scheduled: crate::region_source::ScheduledTickHandle,
    world_state: crate::world_state::WorldStateHandle,
    follow: crate::tick_area::TickFollow,
    border: BorderFeed,
) where
    W: ChunkSource,
{
    run_tick_loop_with_weather_impl(
        mobs,
        mob_out,
        block_entities,
        clock,
        world,
        block_tick_out,
        tick_area,
        explosion_out,
        weather_out,
        weather,
        sleep_vote,
        sleep_feed,
        scheduled,
        world_state,
        follow,
        border,
        Some(server_world),
    )
    .await;
}

/// Accepts every scheduled-tick owner completion before the live callback
/// drain. Queue owners keep due records local until this boundary; this
/// central consumer validates the complete tick-start batch set and restores
/// the existing global drain order before any block or fluid callback runs.
#[cfg(not(target_arch = "wasm32"))]
fn apply_scheduled_tick_owner_batches<T>(
    batches: Vec<ScheduledTickOwnerBatch<T>>,
) -> Vec<ScheduledTick<T>> {
    merge_due_owner_batches(batches)
}

/// Applies block-entity owner messages after their serial execution phase.
///
/// Block-entity owners may mutate their own simulation state, but they do not
/// receive a [`ChunkSource`] and therefore cannot make visible world writes.
/// This is the single central writer. It consumes each owner batch in the
/// plan's established order, preserving today's deterministic behavior while
/// making a later cross-owner executor return messages instead of borrowing a
/// second region's world state.
#[cfg(not(target_arch = "wasm32"))]
fn apply_block_entity_effect_batches<W: ChunkSource>(
    world: &W,
    block_tick_out: &BlockTickFeed,
    batches: Vec<crate::block_entities::BlockEntityTickEffectBatch>,
) {
    for effect in crate::block_entities::merge_tick_effect_batches(batches) {
        let pos = effect.pos;
        let lit = effect.lit;
        debug_assert_eq!(
            effect.owner,
            crate::block_entities::BlockEntityTickOwner::Chunk {
                cx: pos.x.div_euclid(16),
                cz: pos.z.div_euclid(16),
            },
            "a block-entity effect must be handed to the writer by its position's owner"
        );
        let state = world.block_state(pos.x, pos.y, pos.z);
        let new_state =
            crate::redstone::with_property(&state, "lit", if lit { "true" } else { "false" });
        if new_state != state {
            world.set_block(pos.x, pos.y, pos.z, &new_state);
            block_tick_out.publish(pos.x, pos.y, pos.z, new_state);
        }
    }
}

/// Publishes entity-owner messages only after their serial simulation phase.
///
/// Entity owners produce no world writes here: `BlockTickFeed` remains the
/// single publisher shared with the connection layer. Batches deliberately do
/// not imply that their owners ran in parallel. Their effects are restored to
/// their former serial visit sequence before publication, preserving current
/// cross-chunk behavior while making a later worker hand-off explicit.
#[cfg(not(target_arch = "wasm32"))]
fn apply_entity_effect_batches(
    block_tick_out: &BlockTickFeed,
    batches: Vec<EntityTickEffectBatch>,
) {
    let mut effects = Vec::new();
    for batch in batches {
        for effect in batch.effects() {
            debug_assert_eq!(
                effect.owner,
                batch.owner,
                "an entity owner batch may contain only its own effects"
            );
            debug_assert_eq!(
                effect.owner,
                EntityTickOwner::Chunk {
                    cx: (effect.source.x.floor() as i32).div_euclid(16),
                    cz: (effect.source.z.floor() as i32).div_euclid(16),
                },
                "an entity effect must be handed to the publisher by its source owner's chunk"
            );
            effects.push(effect.clone());
        }
    }
    effects.sort_unstable_by_key(|effect| effect.sequence);
    for effect in effects {
        block_tick_out.publish_effect(effect.effect().clone());
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn run_tick_loop_with_weather_impl<W>(
    mobs: MobHandle,
    mob_out: LiveMobSource,
    block_entities: BlockEntityHandle,
    clock: Arc<TickClock>,
    world: Arc<W>,
    block_tick_out: BlockTickFeed,
    tick_area: (RangeInclusive<i32>, RangeInclusive<i32>),
    explosion_out: ExplosionFeed,
    // Weather transitions are world-state behavior. This loop
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
    // when a per-world seed store exists, and what lets this module's
    // test start a world already mid-cycle instead of waiting out a
    // 12k-180k-tick rain delay.
    weather: WeatherState,
    // The night-skip vote is world-state behavior.
    // `sleep_vote` is the shared roster and voter count — connections call
    // `lay_down`/`get_up` on it (the `UseItemOn` bed arm and the
    // `PlayerCommand` arm in `server.rs`) — and this loop reads it via
    // `snapshot()` once per tick, folding it into a loop-owned
    // [`SleepState`] (see below). The same single-consumer snapshot shape as
    // `weather_out` makes it safe for the one connection
    // `open_in_memory_with_mobs` spawns; a vote the wrapper's discarded
    // default (no connection calls) can never pass.
    sleep_vote: &SleepVote,
    // This feed publishes a passed vote's [`SleepEvent::SkippedNight`]
    // broadcast, drained by the connection — `serve_play`'s
    // `container_sync_tick` arm — into a real `encode_set_time` so the
    // client's day clock jumps to the morning. Snapshot-feed, like
    // `weather_out`, with the same single-consumer caveat.
    sleep_feed: &SleepFeed,
    // Persisted scheduled-tick queues. Keeping the two queues out of local state
    // here, so the queues the persistence path reads were always empty in
    // production and a pending repeater tick was lost on quit — the schema
    // (`chunk_nbt::SavedTick`) and the save/load halves were both built and both
    // gated against real vanilla bytes while nothing filled them.
    //
    // The game tick they are measured against travels with them, because it must
    // come from *this* counter and not be re-derived. A second clock here is
    // prevents a clock-domain mismatch: `SET_TIME` decoded and really darkened the
    // sky, every link in the wire green, while the value was wall-clock
    // elapsed-since-join rather than the tick counter.
    scheduled: crate::region_source::ScheduledTickHandle,
    // The world's shared game rules, difficulty and clock use the same handle
    // every connection reads (see `crate::world_state`). Mob griefing and time
    // advancement read it directly; weather advancement and the required
    // sleeper percentage remain local defaults until this loop reads their
    // stored values.
    world_state: crate::world_state::WorldStateHandle,
    // See [`run_tick_loop`]'s own parameter comment: the dimension this loop serves
    // plus the shared player-anchor set, which together turn `tick_area` from the
    // whole simulated world into a fallback for when no player is in it.
    follow: crate::tick_area::TickFollow,
    // The world border is a shared handle:
    // a `/worldborder` command mutates through `BorderFeed::with` and every
    // connection reads through `BorderFeed::get` — see `crate::border`'s
    // module doc for the "interim shape" this replaces. Before this
    // parameter existed, this loop ticked a private `WorldBorder::default()`
    // no caller could ever reach, and every `serve_connection*` entry point
    // built its own throwaway `BorderFeed::default()`, so a resize command
    // would have mutated a border nothing read and nothing ticked.
    border: BorderFeed,
    // Only the two primary `IntegratedServer` loops provide this `World`.
    // Every other caller keeps its existing behavior without manufacturing a
    // separate scheduler for a dimension or test fixture.
    mut server_world: Option<World>,
) where
    W: ChunkSource,
{
    // Same reasoning as `run_mob_tick_loop`'s own opening publish: a fresh
    // connection's first streaming pass should see the seeded population
    // immediately, not after waiting a full tick period for the loop below to
    // run once.
    mob_out.publish(mobs.with(|sim| sim.snapshots()));
    // The `BOSS_EVENT` twin of the snapshot publish immediately above —
    // see `LiveMobSource::publish_boss_bars`'s own doc for why this is a
    // second call rather than folded into `publish` itself.
    mob_out.publish_boss_bars(mobs.with(|sim| sim.boss_bars()));

    let mut next_tick_at = tokio::time::Instant::now();
    let mut last_overload_warning_at: Option<tokio::time::Instant> = None;
    let mut game_tick: u64 = 0;
    // The day clock is world-state behavior, advanced one
    // per tick in lockstep with `game_tick` until a night skip jumps it — the
    // `dayTime` counter of vanilla's `ServerLevel.tickTime` (which increments
    // both `gameTime` and `dayTime` as two counters). Owned by this thread with
    // no lock, exactly like `game_tick`. `i64` because the night skip lands on
    // `SleepState::morning_after`'s multiples of `DAY_LENGTH_TICKS`.
    let mut day_time: i64 = 0;
    // The weather cycle is world-state behavior, owned
    // by the tick thread with no lock, exactly like `game_tick`/`block_ticks`
    // — the plain-struct shape the ECS migration (shape A) turns into a
    // `Resource` mechanically later. Seeded by the caller (see the parameter
    // comment); this binding is what makes it mutable for the loop.
    let mut weather = weather;
    // The night-skip vote's state is world-state behavior,
    // state, owned by this loop with no lock, exactly like `weather` — the
    // shared [`SleepVote`] holds the roster, but who has been *deep* asleep is
    // measured here against this thread's own `game_tick`, and the loop is
    // what decides a pass (see `crate::sleep`'s module doc).
    let mut sleep_state = SleepState::default();
    // One queue per persisted scheduled-tick class: block and fluid. Owned by
    // `scheduled` rather than this function, which lets them be saved; they
    // are borrowed out of it once per tick below.
    //
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
    // Resolve lazily because a `world.column()` call before the
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
    // `crate::lightning`'s two independent streams: strike target selection and a
    // bolt's own life/flashes/ignition state machine, kept on separate
    // streams for `LIGHTNING_BOLT_SEED`'s documented reason — a strike
    // decision must never shift which roll an already-live bolt's state
    // machine sees. `lightning_rand_value` is the third, non-`SpawnRng`
    // stream `block_random_pos` needs (`Level.randValue`'s own tiny LCG).
    let mut lightning_strike_rng = crate::mob_spawn::SpawnRng::new(crate::lightning::LIGHTNING_STRIKE_SEED);
    let mut lightning_bolt_rng = crate::mob_spawn::SpawnRng::new(crate::lightning::LIGHTNING_BOLT_SEED);
    let mut lightning_rand_value: i32 = LIGHTNING_RAND_VALUE_SEED;
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
    // `minecraft:spawner` block entities' delay reroll and per-attempt cell
    // pick (`crate::mob_spawner::SpawnerState::tick`) — one stream shared by
    // every spawner in the world, matching vanilla's single per-level
    // `RandomSource` the same way `dispenser_rng` above does for dispensers.
    let mut spawner_rng = crate::mob_spawn::SpawnRng::new(crate::mob_spawner::SPAWNER_BEHAVIOR_SEED);
    // The built-in tree a `TICK_COMMAND_BLOCK` drain runs a command block's
    // own command through — see that arm's own comment below. Built once per
    // loop, the same convention `crate::server`'s per-connection `CommandSession`
    // already uses for its own `ServerCommands::new()` rather than sharing one
    // built-in tree across every caller.
    let command_tree = crate::commands::ServerCommands::new();
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
    // The natural-spawn driver is long-lived rather than built
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
    // `Zombie.hurtServer`'s reinforcement placement search — the 50-candidate
    // `Mth.nextInt(random, 7, 40) * Mth.nextInt(random, -1, 1)` offset draws.
    // On its own stream, the same reason every other spawn-time RNG here is.
    let mut reinforcement_rng = crate::mob_spawn::SpawnRng::new(NATURAL_SPAWN_SEED ^ 0x5245_494E);
    // The world border ticks first each loop. `border` is the shared handle passed
    // in — see this function's own parameter comment.

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
        // Tick the border before the rest of the world tick, matching the
        // required order. The shared feed means
        // against the shared feed, so a `/worldborder` resize's lerp actually
        // advances tick over tick.
        border.with(WorldBorder::tick);
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
            // Supply the villager schedule's world clock.
            // `world_state.time()` (read-only — `tick_time()` is what
            // actually advances the clock, called later this same loop
            // iteration) rather than a second `tick_time()` call, which
            // would double-advance `day_time` once per tick. Reduced mod
            // 24000 here rather than inside `MobSim`, matching this crate's
            // own convention (`docs/`'s own note that "nothing here needs"
            // the reduction except a consumer that actually places the sun —
            // a villager schedule is exactly that consumer).
            let day_time = (world_state.time().day_time.rem_euclid(24_000)) as i32;
            mobs.with(|sim| {
                sim.set_day_time(day_time);
                // Use the previous tick's sleep roster; this tick's
                // `sleep_state.reconcile` runs later in this loop, in
                // vanilla's own `tickSleepingPlayers` position — see that
                // call's own comment) — a one-tick lag, the same shape
                // every other perception feed in this loop already carries.
                sim.set_sleeping_players(sleep_state.sleepers_snapshot());
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
        // **Peaceful removes monsters.**
        // Vanilla does it in `Mob.checkDespawn`, which discards any
        // `MobCategory.MONSTER` entity when
        // `level.getDifficulty() == Difficulty.PEACEFUL`; a difficulty that is
        // is stored and broadcast but not read by the simulation, monsters remain.
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
        // Set the regional-difficulty inputs needed by every spawn this tick.
        // `MobSim::set_spawn_difficulty` and its two readers — the
        // zombie/husk/zombie_villager door-breaking coin flip
        // (`species_shape`'s caller in `spawn_species`) and
        // `lodestone_entity::spawn_equipment`'s armour/weapon roll — have
        // existed since `615256a1`/`7357c652`, but nothing called the setter
        // outside a test: every real spawn saw the `0.0`/`false` defaults, so
        // no mob has ever broken down a door or spawned wearing gear on a
        // real world. `local_game_time` stays `0` (no per-chunk
        // inhabited-time tracking exists yet — see
        // `crate::regional_difficulty`'s own module doc), which only ever
        // *understates* the scalar, never flips a Peaceful/non-Peaceful or
        // threshold verdict; world age and moon phase alone already lift
        // `special_multiplier` off zero on a world old enough to matter.
        // Read fresh each tick (not the loop's one-tick-stale `day_time`
        // mirror `crate::lightning` uses further down) since this runs
        // before this tick's own `tick_time()` call.
        let spawn_difficulty_time = world_state.time();
        let spawn_difficulty_instance = crate::regional_difficulty::DifficultyInstance::new(
            world_state.difficulty().0,
            spawn_difficulty_time.game_time,
            0,
            crate::regional_difficulty::moon_brightness_for_day_time(spawn_difficulty_time.day_time),
        );
        mobs.with(|sim| {
            sim.set_spawn_difficulty(
                spawn_difficulty_instance.special_multiplier(),
                spawn_difficulty_instance.is_hard(),
            );
            // `level.isSpawningMonsters()` — `Zombie.hurtServer`'s other
            // reinforcement-roll gate alongside the `hard` flag just above.
            sim.set_spawn_monsters_enabled(world_state.spawn_mobs());
        });
        // **the natural spawn cycle, and the despawn pass.**
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
        let mut natural_tickets = Vec::new();
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
                // Vanilla's `spawnableChunkCount` for the cap formula, read off
                // the area actually simulated rather than a constant: `MAGIC_NUMBER`
                // (289) worth of chunks yields caps equal to the per-chunk maxima,
                // so a smaller follow area scales every category cap down with it.
                //
                // Planning holds the mob lock only long enough to take its census.
                // Candidate selection, plugin adjudication, and the later materialize
                // pass are separate phases. In particular, an `Adjudicate` system can
                // neither observe nor nest the `MobHandle` lock.
                if let Some(server_world) = server_world.as_mut() {
                    let mut state = mobs.with(|sim| sim.census(area.spawnable_chunks()));
                    let planned = MobSim::plan_spawn_cycle(
                        &mut state,
                        &mut natural_spawner,
                        area.chunks(),
                    );
                    let mut proposals = server_world.resource_mut::<crate::ecs::ServerProposalQueue>();
                    natural_tickets.extend(planned.into_iter().map(|(category, candidate)| {
                        proposals.stage(crate::ecs::ServerProposalAction::NaturalSpawnMob {
                            entity_type: candidate.entity_type,
                            pos: candidate.pos,
                            category,
                        })
                    }));
                } else {
                    mobs.with(|sim| {
                        let mut state = sim.census(area.spawnable_chunks());
                        sim.run_spawn_cycle(&mut state, &mut natural_spawner, area.chunks());
                    });
                }
            }
        }
        // The primary world's shared proposal pass runs after every source has
        // staged its actions and before any natural candidate materializes.
        // External requests use the same pass; their oneshot replies are sent
        // without a mob lock, and their callers preserve the legacy direct
        // apply ownership after awaiting it. It still runs while `spawn_mobs`
        // is off so plugin messages and scheduler callbacks retain their tick
        // contract.
        if let Some(server_world) = server_world.as_mut() {
            server_world.run_schedule(crate::ecs::GameTick);
            let tickets: HashSet<_> = natural_tickets.into_iter().collect();
            let resolutions = server_world
                .resource_mut::<crate::ecs::ServerProposalQueue>()
                .take_resolutions();
            if !tickets.is_empty() {
                mobs.with(|sim| {
                    for resolution in resolutions {
                        if !tickets.contains(&resolution.ticket()) {
                            continue;
                        }
                        match resolution.outcome {
                            Ok(crate::ecs::ServerProposalAction::NaturalSpawnMob {
                                entity_type,
                                pos,
                                category,
                            }) => {
                                sim.spawn_species(entity_type, pos)
                                    .set_category(category)
                                    .set_persistent(category.is_persistent());
                            }
                            Ok(crate::ecs::ServerProposalAction::SpawnMob { entity_type, pos }) => {
                                sim.spawn_species(entity_type, pos);
                            }
                            Ok(crate::ecs::ServerProposalAction::DespawnMob { .. }) | Err(_) => {}
                        }
                    }
                });
            }
        }
        if world_state.spawn_mobs() {
            // Nearest-player despawn runs after accepted natural candidates have
            // materialized, so the cap census and despawn state keep the same
            // tick ordering as the direct path.
            let nearest = mobs.with(|sim| sim.players().first().map(|p| p.perception.position));
            mobs.with(|sim| sim.despawn_pass(nearest, &mut despawn_rng));
        }
        // Pillager patrols use the same live, player-following terrain
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
        // The wandering-trader spawn cycle uses the same live,
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
        // The `BOSS_EVENT` twin of the snapshot publish immediately above —
        // see `LiveMobSource::publish_boss_bars`'s own doc for why this is a
        // second call rather than folded into `publish` itself.
        mob_out.publish_boss_bars(mobs.with(|sim| sim.boss_bars()));
        // `MobSim::tick` already calls `MobSim::explode` for the
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
        // Apply the world half of a grazing sheep, using the same shape as
        // the detonation drain above for the same structural reason —
        // `MobSim::tick` holds `world: &'w ChunkWorld` **immutably**, so it can
        // only record the eat as an intent; this loop is the one place that owns
        // a mutable `ChunkSource` and can apply it. `EatBlockGoal` reaching this
        // drain is what makes the grass actually disappear rather than the goal
        // counting down against a world that never changes.
        //
        // Vanilla's own eat-block goal, and the two variants really
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
            // The grazing action sends level event 2001 for the break
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
        // Mob hurt and death sounds. `MobSim::apply_damage` already
        // damaged and killed mobs with no audible result at all — the sim records
        // the vocalisation for the same reason it records a detonation (it holds
        // the world immutably and owns no connection).
        for effect in mobs.with(MobSim::take_vocalisations) {
            block_tick_out.publish_effect(effect);
        }
        // Periodic ambient effects are a real entity simulation phase. Its
        // chunk owners return messages, and only this central publisher drains
        // them onto the connection feed; see `apply_entity_effect_batches`.
        let entity_effect_batches = mobs.with(MobSim::take_ambient_sound_effect_batches);
        clock.record_owner_work(OwnerTickStats {
            entity_effect_batches: entity_effect_batches.len() as u64,
            entity_effects: entity_effect_batches
                .iter()
                .map(|batch| batch.effects().len() as u64)
                .sum(),
            ..OwnerTickStats::default()
        });
        apply_entity_effect_batches(&block_tick_out, entity_effect_batches);
        // Drain target-block projectile impacts here, outside
        // the `scheduled.with` region below) because `MobSim` is the only
        // thing that saw the hit; resolved *inside* it further down because a
        // target's power write needs `block_ticks` (for
        // `redstone_target::apply_hit`'s `has_pending_decay` guard and to
        // schedule the decay) and the live `world`, neither of which `MobSim`
        // holds — see `crate::mobs::ProjectileBlockHit`'s own doc comment.
        let projectile_block_hits = mobs.with(MobSim::take_projectile_block_hits);
        // Apply the hopper redstone lock. `tick_all`'s unlocked shorthand
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
        // `is_loaded` gates the scan by chunk residency *before*
        // `enabled` ever reaches `world.block_state` — `ChunkStore::block_state`
        // regenerates a whole column on a miss, and this closure used to run
        // that for every registered hopper, every tick, forever (the registry
        // has no eviction). `is_column_resident` answers with no generation at
        // all, so a hopper outside every loaded chunk now costs a `HashMap`
        // lookup instead of a worldgen call.
        let furnace_effect_batches = block_entities.with(|registry| {
            registry.tick_all_by_owner_with_hopper_lock(
                &|pos| world.is_column_resident(pos.x.div_euclid(16), pos.z.div_euclid(16)),
                &|pos| crate::redstone::hopper_enabled(&world.block_state(pos.x, pos.y, pos.z)),
            )
        });
        clock.record_owner_work(OwnerTickStats {
            block_entity_batches: furnace_effect_batches.len() as u64,
            block_entity_effects: furnace_effect_batches
                .iter()
                .map(|batch| batch.effects().len() as u64)
                .sum(),
            ..OwnerTickStats::default()
        });
        // `BlockEntityRegistry` has no `ChunkSource` handle of its own (see its
        // module doc's "No visual sync" note), so this is where
        // `FurnaceTick::lit_changed` finally reaches the block a client is
        // streamed — the same shape as the target-block write just above, and
        // the one production caller that module's own doc names as holding
        // both a `ChunkSource` and the registry.
        apply_block_entity_effect_batches(&*world, &block_tick_out, furnace_effect_batches);

        // Spawner block entities.
        // `tick_all_with_hopper_lock` above deliberately does not advance one —
        // `crate::mob_spawner::SpawnerState::tick` needs the player list and the
        // live entity set to decide anything, and `BlockEntityRegistry` has a
        // handle to neither (the same reason the natural-spawn cycle above does
        // not live in that registry either). Snapshotted once per tick rather
        // than re-read per spawner, same reasoning as `spawner_players` below.
        let spawner_snapshots = mobs.with(|sim| sim.snapshots());
        let spawner_players: Vec<lodestone_model::Vec3> =
            mobs.with(|sim| sim.players().iter().map(|p| p.perception.position).collect());
        let spawner_blocks_work = world_state.spawner_blocks_work();
        let spawner_difficulty = world_state.difficulty().0;
        let mut spawner_attempts: Vec<crate::mob_spawner::SpawnAttempt> = Vec::new();
        block_entities.with(|registry| {
            // Positions snapshotted up front — `tick_all_with_hopper_lock`'s own
            // doc explains why a plain `HashMap` iterator cannot be walked while
            // `get_mut` also wants to mutate the map.
            let positions: Vec<BlockPos> = registry
                .iter()
                .filter_map(|(pos, entity)| {
                    matches!(entity, crate::block_entities::BlockEntity::Spawner(_))
                        .then_some(*pos)
                })
                .collect();
            for pos in positions {
                // residency gate, reused: a spawner outside every
                // loaded chunk must not cost a worldgen call just to be told no.
                if !world.is_column_resident(pos.x.div_euclid(16), pos.z.div_euclid(16)) {
                    continue;
                }
                let Some(crate::block_entities::BlockEntity::Spawner(state)) =
                    registry.get_mut(pos)
                else {
                    continue;
                };
                // `BaseSpawner.isNearPlayer`: an alive player within
                // `required_player_range` blocks of the spawner's centre.
                let required_range = f64::from(state.required_player_range());
                let near_player = spawner_players.iter().any(|p| {
                    let dx = p.x - (f64::from(pos.x) + 0.5);
                    let dy = p.y - (f64::from(pos.y) + 0.5);
                    let dz = p.z - (f64::from(pos.z) + 0.5);
                    dx * dx + dy * dy + dz * dz <= required_range * required_range
                });
                // `level.noCollision`, approximated as "the candidate's floor
                // cell has an empty collision shape" — see
                // `crate::mob_spawner`'s module doc for the scope note.
                let is_valid_position = |v: lodestone_model::Vec3| {
                    let block = world.block_state(
                        v.x.floor() as i32,
                        v.y.floor() as i32,
                        v.z.floor() as i32,
                    );
                    crate::spawn_egg::collision_boxes_for(&block).is_empty()
                };
                // `level.getEntities(EntityTypeTest.forExactClass(...), aabb,
                // NO_SPECTATORS).size()` over the already-taken snapshot set —
                // an exact-type count, not a category count.
                let nearby_count =
                    |entity_type: &lodestone_model::ResourceKey, spawn_range: i32| {
                        let range = f64::from(spawn_range);
                        let min_x = f64::from(pos.x) - range;
                        let min_y = f64::from(pos.y) - range;
                        let min_z = f64::from(pos.z) - range;
                        let max_x = f64::from(pos.x + 1) + range;
                        let max_y = f64::from(pos.y + 1) + range;
                        let max_z = f64::from(pos.z + 1) + range;
                        spawner_snapshots
                            .iter()
                            .filter(|s| {
                                &s.entity_type == entity_type
                                    && (min_x..=max_x).contains(&s.position.x)
                                    && (min_y..=max_y).contains(&s.position.y)
                                    && (min_z..=max_z).contains(&s.position.z)
                            })
                            .count() as i32
                    };
                let ctx = crate::mob_spawner::SpawnCtx {
                    near_player,
                    spawner_blocks_work,
                    difficulty: spawner_difficulty,
                    pos,
                    is_valid_position: &is_valid_position,
                    nearby_count: &nearby_count,
                };
                spawner_attempts.extend(state.tick(&ctx, &mut spawner_rng));
            }
        });
        for attempt in spawner_attempts {
            mobs.with(|sim| {
                sim.spawn_species(attempt.entity_type, attempt.position);
            });
        }

        // The zombie reinforcement check uses the
        // placement half — `MobSim::attack_from_player` already decided
        // *whether* to call one in (`ReinforcementCall`'s own doc explains
        // the split); this is `Zombie.hurtServer`'s own 50-candidate search
        // against the live world, simplified the same way
        // `mob_spawner`'s own `is_valid_position` closure above is: "solid
        // ground below, open air at foot and head height" stands in for
        // `SpawnPlacements.isSpawnPositionOk`/`noCollision`, and there is no
        // liquid check (a disclosed reduction, matching this crate's other
        // simplified spawn-placement passes). Stops at the first candidate
        // that passes, exactly as vanilla's own loop `break`s on the first
        // hit.
        for call in mobs.with(MobSim::take_reinforcement_calls) {
            let origin_x = call.position.x.floor() as i32;
            let origin_y = call.position.y.floor() as i32;
            let origin_z = call.position.z.floor() as i32;
            let mut placed = None;
            for _ in 0..50 {
                let dx = (7 + reinforcement_rng.next_int(34)) * (reinforcement_rng.next_int(3) - 1);
                let dy = (7 + reinforcement_rng.next_int(34)) * (reinforcement_rng.next_int(3) - 1);
                let dz = (7 + reinforcement_rng.next_int(34)) * (reinforcement_rng.next_int(3) - 1);
                let x = origin_x + dx;
                let y = origin_y + dy;
                let z = origin_z + dz;
                let candidate = lodestone_model::Vec3::new(
                    f64::from(x) + 0.5,
                    f64::from(y),
                    f64::from(z) + 0.5,
                );
                // `!level.hasNearbyAlivePlayer(xt, yt, zt, 7.0)`.
                if spawner_players.iter().any(|p| {
                    let ddx = p.x - candidate.x;
                    let ddy = p.y - candidate.y;
                    let ddz = p.z - candidate.z;
                    ddx * ddx + ddy * ddy + ddz * ddz <= 49.0
                }) {
                    continue;
                }
                let below = world.block_state(x, y - 1, z);
                let feet = world.block_state(x, y, z);
                let head = world.block_state(x, y + 1, z);
                if !crate::spawn_egg::collision_boxes_for(&below).is_empty()
                    && crate::spawn_egg::collision_boxes_for(&feet).is_empty()
                    && crate::spawn_egg::collision_boxes_for(&head).is_empty()
                {
                    placed = Some(candidate);
                    break;
                }
            }
            if let Some(pos) = placed {
                mobs.with(|sim| {
                    sim.spawn_species(call.entity_type.clone(), pos)
                        .set_attack_target_id(Some(call.target_id))
                        .apply_reinforcement_callee_charge();
                });
            }
        }

        // Per-phase timing (see `TickPhase::MobsAndItems`'s own doc): closes
        // out everything from `tick_start` through the spawner-block pass
        // just above. A bare timestamp, no lock held, so it cannot
        // deadlock and cannot fold in the top-of-loop `sleep_until` wait.
        let t_mobs_end = tokio::time::Instant::now();
        clock.record_phase(TickPhase::MobsAndItems, t_mobs_end.duration_since(tick_start));

        // The clock is the **world's**, not this loop's: one
        // `tick_time` advances `game_time` unconditionally and `day_time` only
        // under the `advance_time` rule (`ServerLevel.tickTime`, where `setDayTime`
        // is gated and `gameTime` is not). The locals below are still the loop's
        // arithmetic, but they are *sourced* here rather than incremented — which
        // is the complete explanation, because the connection's periodic
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
        // The weather cycle is world-state behavior and is
        // world-global state, so it belongs to the world tick (not to any
        // connection — the straddle the world-state plan's migration exists
        // to delete). `advance_weather()` stands in for the R1 game rule.
        for event in weather.tick(advance_weather()) {
            weather_out.publish(event);
        }
        // The night-skip vote is world-state behavior and runs in
        // vanilla's own position — `ServerLevel.tick` runs
        // `tickSleepingPlayers` right after the weather-cycle timers.
        // Snapshot the shared roster, fold it
        // into the loop-owned [`SleepState`] (recording each sleeper's
        // lay-down tick and dropping anyone who woke), then test the vote: at
        // least `sleepers_needed` players asleep, and at least that many deep
        // (`DEEP_SLEEP_TICKS`).
        //
        // On a pass, vanilla's three steps run in order
        // (its own night-skip broadcast routine): the clock jumps to the next morning
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
        // Store the tick every pending `trigger_tick` is relative to, so a
        // saved queue can be rebased on load. One relaxed atomic store — the
        // tick-thread cost is a count of one, no I/O and no encoding.
        scheduled.set_game_tick(game_tick);

        // Per-phase timing (see `TickPhase::WeatherAndSleep`'s own doc):
        // closes out the weather cycle and the night-skip vote above. Taken
        // *before* `scheduled.with` opens below, never from inside it — see
        // `TickPhase::ScheduledAndPhysics`'s doc for why.
        let t_weather_end = tokio::time::Instant::now();
        clock.record_phase(TickPhase::WeatherAndSleep, t_weather_end.duration_since(t_mobs_end));

        // both queues are borrowed out of `scheduled` for the whole
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
        // mention: `random_ticks.tick_chunk` also takes the block-tick owner, so
        // closing the closure at the fluid loop would put that call out of scope.
        // The body below is left at its original indentation for the same reason
        // the wrapper shape was chosen — re-indenting it would touch every line
        // of the section and bury the real change.
        scheduled.with(|queues| {
        let block_ticks = &mut queues.block;
        let fluid_ticks = &mut queues.fluid;
        // Adopt the block ticks scheduled by a player's mutation.
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
            if pending.kind == crate::fluid::TICK_FLUID {
                if fluid_ticks.has_scheduled(pending.pos, &pending.kind) {
                    continue;
                }
                fluid_ticks.schedule(
                    pending.pos,
                    pending.kind,
                    game_tick + pending.trigger_tick,
                    pending.priority,
                );
            } else {
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
        }

        // Resolve each target-block hit from `MobSim::resolve_projectile_impacts`.
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
            for event in crate::random_tick::propagate_and_react_with_entities_across_chunks(
                &mut column, cx * 16, cz * 16, &*world, hit.pos.x, hit.pos.y, hit.pos.z, block_ticks, game_tick,
                Some(&block_entities),
            ) {
                let (ex, ey, ez) = event.pos;
                world.set_block(ex, ey, ez, &event.to);
                shove_entities_from_piston(&mobs, &block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                post_note_block_vibration(&world, &mobs, (ex, ey, ez), &event.from, &event.to);
                block_tick_out.publish(ex, ey, ez, event.to);
            }
        }

        // Block before fluid. Draining
        // (rather than iterating a live queue) is what keeps a tick
        // scheduled *by* one of these callbacks out of this same pass — see
        // `ScheduledTickQueue::drain_due`'s own doc comment.
        //
        // `block_ticks` has producers —
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
        let due_block_ticks = apply_scheduled_tick_owner_batches(
            block_ticks.drain_due_owner_batches(game_tick, MAX_SCHEDULED_TICKS_PER_TICK),
        );
        clock.record_owner_work(OwnerTickStats {
            scheduled_block_ticks: due_block_ticks.len() as u64,
            ..OwnerTickStats::default()
        });
        for due in due_block_ticks {
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
                    for event in crate::random_tick::propagate_and_react_with_entities_across_chunks(
                        &mut column,
                        min_x,
                        min_z,
                        &*world,
                        x,
                        y,
                        z,
                        block_ticks,
                        game_tick,
                        Some(&block_entities),
                    ) {
                        let (ex, ey, ez) = event.pos;
                        world.set_block(ex, ey, ez, &event.to);
                        publish_moving_piston(&block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                        shove_entities_from_piston(&mobs, &block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                        post_note_block_vibration(&world, &mobs, (ex, ey, ez), &event.from, &event.to);
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
                for event in crate::random_tick::run_tripwire_recheck(&mut column, min_x, min_z, &*world, BlockPos::new(x, y, z)) {
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
            // A dropper always either pushes into a
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
                        // A dispenser sitting on the last cell of its chunk
                        // fires *into* the next one, so every read below —
                        // where a spawn egg's mob or a dispensed boat or
                        // minecart lands, and what block is in front — goes
                        // through the same multi-column view the reaction
                        // dispatch uses rather than a bounded column that
                        // answers air one cell past the seam. Read-only for
                        // the whole arm: the world edits it makes go through
                        // `world`, so nothing here needs a write path.
                        let columns =
                            crate::random_tick::RedstoneColumns::new(&mut column, min_x, min_z, &*world);
                        let lookup = crate::redstone::make_columns_lookup(&columns);

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
                        // Set by the bucket arms below: `consumeWithRemainder`
                        // swaps the picked slot's item for a different one
                        // (empty↔filled bucket) rather than a plain shrink.
                        let mut swap_remainder: Option<lodestone_model::ResourceKey> = None;

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
                        } else if let Some(kind) = crate::mobs::minecart::MinecartKind::from_item(&item_str) {
                            // `MinecartDispenseItemBehavior` — a rail directly
                            // ahead of the dispenser (or one under an
                            // air-filled cell ahead) required, else the same
                            // plain-toss fallback every other unmatched item
                            // gets.
                            match crate::redstone_dispenser::minecart_dispense(origin, face, &lookup) {
                                crate::redstone_dispenser::MinecartDispense::Place { position } => {
                                    mobs.with(|sim| {
                                        sim.spawn_minecart(kind, position);
                                    });
                                }
                                crate::redstone_dispenser::MinecartDispense::Fallback => toss = true,
                            }
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
                        } else if let Some(entity_type) = crate::redstone_dispenser::arrow_entity_type(&item_str) {
                            // Projectile dispensing: arrow item variants all use the same default
                            // power/uncertainty; see `arrow_entity_type`'s own
                            // doc for why a tipped arrow's potion is not
                            // modelled here.
                            let position = crate::redstone_dispenser::projectile_dispense_position(center, face);
                            let velocity = crate::redstone_dispenser::projectile_velocity(
                                face,
                                crate::redstone_dispenser::ARROW_DISPENSE_POWER,
                                crate::redstone_dispenser::ARROW_DISPENSE_UNCERTAINTY,
                                &mut || dispenser_rng.next_f64(),
                            );
                            mobs.with(|sim| {
                                sim.spawn_projectile(
                                    entity_type.parse().expect("arrow_entity_type names a real key"),
                                    lodestone_entity::projectile::Projectile::arrow(
                                        lodestone_model::Vec3::new(position.0, position.1, position.2),
                                        lodestone_model::Vec3::new(velocity.0, velocity.1, velocity.2),
                                    ),
                                );
                            });
                        } else if let Some(kind) = crate::fluid::bucket_empty_item_kind(&item_str) {
                            // The "filled bucket" shared behaviour
                            // (`filledBucketBehavior` in the jar) — water and
                            // lava only, see `crate::fluid`'s own doc section
                            // for why the other four registrants are not here.
                            let target = face.relative(origin);
                            let target_state = lookup(target);
                            if crate::fluid::is_bucket_emptiable_target(&target_state) {
                                let new_state = crate::fluid::bucket_empty_state(kind);
                                world.set_block(target.x, target.y, target.z, new_state);
                                block_tick_out.publish(target.x, target.y, target.z, new_state.to_owned());
                                swap_remainder =
                                    Some("minecraft:bucket".parse().expect("valid key"));
                            } else {
                                // `DefaultDispenseItemBehavior` fallback: a
                                // filled bucket with nowhere to empty just
                                // tosses like an ordinary item.
                                toss = true;
                            }
                        } else if item_str == "minecraft:bucket" {
                            let target = face.relative(origin);
                            let target_state = lookup(target);
                            if let Some(kind) = crate::fluid::bucket_pickup_kind(&target_state) {
                                world.set_block(target.x, target.y, target.z, crate::chunk::AIR);
                                block_tick_out.publish(target.x, target.y, target.z, crate::chunk::AIR.to_owned());
                                swap_remainder = Some(
                                    crate::fluid::filled_bucket_item(kind)
                                        .parse()
                                        .expect("valid key"),
                                );
                            } else {
                                // Nothing to pick up: vanilla's own
                                // `super.execute` fallback for an empty bucket
                                // is a plain toss.
                                toss = true;
                            }
                        } else {
                            toss = true;
                        }

                        if consumed {
                            let mut remaining = stack.clone();
                            remaining.count = remaining.count.saturating_sub(1);
                            let remainder = if remaining.count == 0 {
                                // `consumeWithRemainder`'s common case: the
                                // picked slot held exactly one bucket, so the
                                // swap lands directly in the now-empty slot.
                                swap_remainder.map(|item| lodestone_model::ItemStack::new(item, 1))
                            } else {
                                // A rarer case `consumeWithRemainder` handles by
                                // searching the dispenser's own inventory for a
                                // free slot (falling further back to dropping it
                                // in the world if none exists). Searching this
                                // container's other eight slots is not modelled
                                // here; the swap item is tossed outside instead
                                // — still delivered, never silently discarded.
                                if let Some(item) = swap_remainder {
                                    let (position, velocity) = crate::redstone_dispenser::plain_toss(
                                        center,
                                        face,
                                        &mut || dispenser_rng.next_f64(),
                                    );
                                    mobs.with(|sim| {
                                        sim.spawn_item(
                                            item,
                                            lodestone_model::Vec3::new(position.0, position.1, position.2),
                                            lodestone_model::Vec3::new(velocity.0, velocity.1, velocity.2),
                                            lodestone_entity::ItemLifecycle {
                                                age: 0,
                                                pickup_delay: 0,
                                                count: 1,
                                                max_stack_size: lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                                            },
                                        );
                                    });
                                }
                                Some(remaining)
                            };
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

            // `crate::command_block::TICK_COMMAND_BLOCK` is handled here.
            // It has the same live-state requirements as the dispenser-fire arm
            // just above and for the same reason: this needs the live
            // `block_entities` container, plus (new here) a real command
            // dispatcher, neither of which the `Option<String>` chain below
            // has in scope. See `crate::command_block`'s own module doc for
            // exactly how a command block gets scheduled here in the first
            // place (today: "Always Active", not yet a live redstone pulse).
            if due.kind == crate::command_block::TICK_COMMAND_BLOCK {
                if crate::command_block::is_command_block_family(&state) {
                    let origin = BlockPos::new(x, y, z);
                    let mode = crate::command_block::mode_for_block(&state);
                    let conditional = crate::command_block::is_conditional(&state);
                    let facing = crate::command_block::facing(&state);
                    let snapshot = block_entities.with(|reg| match reg.get(origin) {
                        Some(crate::block_entities::BlockEntity::CommandBlock(d)) => Some(d.clone()),
                        _ => None,
                    });
                    if let Some(mut data) = snapshot {
                        // `markConditionMet`'s predecessor read — see the
                        // `SetCommandBlock` handler in `crate::server` for
                        // the identical read, done there for the same reason
                        // (never nest a second `.with` inside a first).
                        let predecessor_succeeded = conditional.then(|| {
                            let behind = facing.opposite().relative(origin);
                            let behind_state = world.block_state(behind.x, behind.y, behind.z);
                            crate::command_block::is_command_block_family(&behind_state)
                                && block_entities.with(|reg| {
                                    matches!(
                                        reg.get(behind),
                                        Some(crate::block_entities::BlockEntity::CommandBlock(d))
                                            if d.success_count > 0
                                    )
                                })
                        });
                        let decision = crate::command_block::tick(
                            mode, data.condition_met, conditional, predecessor_succeeded, data.powered, data.auto,
                        );
                        data.condition_met = decision.condition_met;
                        if decision.zero_success_if_conditional {
                            data.success_count = 0;
                        }
                        if decision.run && !data.already_ran_this_tick(game_tick as i64) {
                            // `CommandBlock.execute`'s own `commandSet` gate —
                            // an empty command still zeroes the success count
                            // rather than attempting to run anything.
                            if data.command.is_empty() {
                                data.success_count = 0;
                            } else {
                                let ran = run_command_block_command(
                                    &command_tree, &world_state, &mobs, &*world, &block_tick_out, origin, facing,
                                    &data.command,
                                );
                                data.success_count = i32::from(ran);
                            }
                            data.record_executed(game_tick as i64);
                        }
                        if decision.reschedule {
                            block_ticks.schedule(
                                (x, y, z),
                                crate::command_block::TICK_COMMAND_BLOCK.to_owned(),
                                game_tick + 1,
                                TickPriority::Normal,
                            );
                        }
                        let mut final_success = data.success_count;
                        block_entities.with(|reg| {
                            if let Some(crate::block_entities::BlockEntity::CommandBlock(d)) = reg.get_mut(origin) {
                                *d = data.clone();
                            }
                        });
                        // `CommandBlock.executeChain`, walked only on the tick
                        // this origin actually ran something — `Sequence`
                        // mode never reaches here at all, since `tick`'s own
                        // `Sequence` branch always answers `run: false`.
                        if decision.run {
                            let mut prev_pos = origin;
                            let mut walk_facing = facing;
                            for _ in 0..MAX_COMMAND_CHAIN_LENGTH {
                                let next_pos = crate::command_block::next_chain_position(prev_pos, walk_facing);
                                let next_state = world.block_state(next_pos.x, next_pos.y, next_pos.z);
                                if !crate::command_block::chain_link_present(&next_state) {
                                    break;
                                }
                                let link_snapshot = block_entities.with(|reg| match reg.get(next_pos) {
                                    Some(crate::block_entities::BlockEntity::CommandBlock(d)) => Some(d.clone()),
                                    _ => None,
                                });
                                let Some(mut link) = link_snapshot else { break };
                                walk_facing = crate::command_block::facing(&next_state);
                                let mut should_break = false;
                                if crate::command_block::chain_link_should_run(link.powered, link.auto) {
                                    let link_conditional = crate::command_block::is_conditional(&next_state);
                                    let met = crate::command_block::mark_condition_met(
                                        link_conditional,
                                        Some(final_success > 0),
                                    );
                                    link.condition_met = met;
                                    if met {
                                        if link.already_ran_this_tick(game_tick as i64) {
                                            // `performCommand`'s same-tick dedup
                                            // returning `false` — `executeChain`
                                            // stops the walk right here.
                                            should_break = true;
                                        } else {
                                            if link.command.is_empty() {
                                                link.success_count = 0;
                                            } else {
                                                let ran = run_command_block_command(
                                                    &command_tree, &world_state, &mobs, &*world, &block_tick_out,
                                                    next_pos, walk_facing, &link.command,
                                                );
                                                link.success_count = i32::from(ran);
                                            }
                                            link.record_executed(game_tick as i64);
                                        }
                                    } else if link_conditional {
                                        link.success_count = 0;
                                    }
                                }
                                final_success = link.success_count;
                                block_entities.with(|reg| {
                                    if let Some(crate::block_entities::BlockEntity::CommandBlock(d)) =
                                        reg.get_mut(next_pos)
                                    {
                                        *d = link.clone();
                                    }
                                });
                                if should_break {
                                    break;
                                }
                                prev_pos = next_pos;
                            }
                        }
                    }
                }
                continue;
            }

            // The torch/repeater/comparator/observer decision, plus the
            // cascade it drives, both live in `block_tick_reaction` rather
            // than inline here: standing this loop up needs a mob sim, a
            // block-entity registry, a weather handle and a command tree,
            // none of which a delayed-redstone chain depends on, and that
            // dependency was the whole reason no test ran such a chain across
            // several ticks. Reads inside go through a multi-column view, so
            // a repeater whose input or side is across a chunk seam sees what
            // the resident neighbour actually holds instead of air.
            //
            // The wire half stays here, because it needs the feeds this
            // module has and the reaction does not.
            let reaction = crate::block_tick_reaction::run_due_block_tick(
                &mut column,
                min_x,
                min_z,
                &*world,
                &due.kind,
                BlockPos::new(x, y, z),
                &state,
                block_ticks,
                game_tick,
                Some(&block_entities),
            );
            if let Some(new_state) = reaction.new_state {
                if new_state != state {
                    // A door, trapdoor or fence gate a scheduled tick just
                    // toggled. The one openable path that is genuinely
                    // server-driven, so nothing predicts it and it was silent.
                    publish_openable_sound(&block_tick_out, BlockPos::new(x, y, z), &state, &new_state, game_tick);
                    block_tick_out.publish(x, y, z, new_state);
                }
                for event in reaction.events {
                    let (ex, ey, ez) = event.pos;
                    publish_openable_sound(&block_tick_out, BlockPos::new(ex, ey, ez), &event.from, &event.to, game_tick);
                    world.set_block(ex, ey, ez, &event.to);
                    publish_moving_piston(&block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                    shove_entities_from_piston(&mobs, &block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                    post_note_block_vibration(&world, &mobs, (ex, ey, ez), &event.from, &event.to);
                    block_tick_out.publish(ex, ey, ez, event.to);
                }
            }
        }
        // Fluid spread — `crate::fluid`. This loop is the production path that
        // makes a placed or exposed liquid actually move.
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
        let due_fluid_ticks = apply_scheduled_tick_owner_batches(
            fluid_ticks.drain_due_owner_batches(game_tick, MAX_SCHEDULED_TICKS_PER_TICK),
        );
        clock.record_owner_work(OwnerTickStats {
            scheduled_fluid_ticks: due_fluid_ticks.len() as u64,
            ..OwnerTickStats::default()
        });
        for due in due_fluid_ticks {
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

        // Run random ticks after both scheduled-tick queues, preserving the
        // block, fluid, then random ordering.
        //
        // The random-tick pass is deferred for the first few ticks
        // after world open, so the background column-seeding task has time to
        // populate the shared [`ChunkStore`] before any `world.column()` call
        // pays the full per-column generation cost on the core thread. See
        // [`INITIAL_RANDOM_TICK_DEFERRAL_TICKS`] for the arithmetic.
        let tick_speed = world_state.random_tick_speed();
        if game_tick > INITIAL_RANDOM_TICK_DEFERRAL_TICKS && tick_speed > 0 {
            clock.record_owner_work(OwnerTickStats {
                random_tick_owned_chunks: area.owned_chunks().len() as u64,
                ..OwnerTickStats::default()
            });
            // The follow area rather than the two fixed ranges: crops, grass, fire,
            // leaf decay and every other randomly-ticking block now grow where the
            // player is standing instead of only around chunk (0, 0).
            for owned in area.owned_chunks() {
                let (cx, cz) = owned.chunk;
                {
                    let mut column = world.column(cx, cz);
                    // Read the current game rule, not `DEFAULT_RANDOM_TICK_SPEED`.
                    // The getter is already covered by tests; this line
                    // is the reader it was missing, and `/gamerule
                    // random_tick_speed 0` now really does stop crop growth.
                    let events =
                        random_ticks.tick_chunk(&mut column, cx, cz, tick_speed, block_ticks, game_tick, &*world);
                    for event in events {
                        let (x, y, z) = event.pos;
                        world.set_block(x, y, z, &event.to);
                        publish_moving_piston(&block_tick_out, &*block_ticks, x, y, z, &event.to);
                        shove_entities_from_piston(&mobs, &block_tick_out, &*block_ticks, x, y, z, &event.to);
                        block_tick_out.publish(x, y, z, event.to);
                    }
                }
            }
        }

        // Make one per-chunk thunder decision per tick over the same follow area and
        // startup-deferral window as the random-tick pass just above. Gated
        // on `thundering` alone — vanilla's own gate is `raining &&
        // isThundering()`, independent of `randomTickSpeed`, which only
        // throttles the *random*-tick pass above — so a storm keeps striking
        // even with `/gamerule random_tick_speed 0`.
        //
        // `crate::lightning::tick_thunder_for_chunk` only decides *whether*
        // and *where* a strike happens (and, via `should_be_skeleton_trap`,
        // is `crate::regional_difficulty::DifficultyInstance`'s one real
        // consumer — see that module's own doc for why its other named
        // consumers have nothing here to attach to yet); turning a decision
        // into a live, ticking, network-visible entity and its `thunderHit`
        // effects is `crate::mobs::lightning`'s job. Before this, neither
        // `tick_thunder_for_chunk` nor `MobSim::tick_lightning` had a single
        // production caller, so a thunderstorm never struck anything.
        if game_tick > INITIAL_RANDOM_TICK_DEFERRAL_TICKS && weather.thundering {
            clock.record_owner_work(OwnerTickStats {
                thunder_owned_chunks: area.owned_chunks().len() as u64,
                ..OwnerTickStats::default()
            });
            // `min_y`/`height` are uniform for the whole dimension, so the
            // fire-tick arm's own cache is reused rather than paying a second
            // `world.column` fetch for the same two integers.
            let (lightning_min_y, lightning_height) = *fire_env.get_or_insert_with(|| {
                let probe = world.column(0, 0);
                (probe.min_y, probe.height)
            });
            let env = crate::lightning::LightningEnv { min_y: lightning_min_y, height: lightning_height };
            let spawn_mobs_rule = world_state.spawn_mobs();
            let lightning_difficulty = world_state.difficulty().0;
            let total_game_time = world_state.time().game_time;
            let living_entities = mobs.with(|sim| sim.living_entity_positions());
            let mut strikes = Vec::new();
            for owned in area.owned_chunks() {
                let (cx, cz) = owned.chunk;
                if let Some(strike) = crate::lightning::tick_thunder_for_chunk(
                    &*world,
                    env,
                    cx * 16,
                    cz * 16,
                    weather.raining,
                    weather.thundering,
                    lightning_difficulty,
                    total_game_time,
                    day_time,
                    spawn_mobs_rule,
                    // No POI/lightning-rod search here — `crate::lightning`'s
                    // own module doc names this as a documented reduction,
                    // `None` until this crate has a POI manager.
                    None,
                    &living_entities,
                    &mut lightning_rand_value,
                    &mut lightning_strike_rng,
                ) {
                    strikes.push(strike);
                }
            }
            if !strikes.is_empty() {
                mobs.with(|sim| sim.spawn_lightning_bolts(strikes, &mut lightning_bolt_rng));
            }
        }
        // Every live bolt's state machine and `thunderHit` effects, one tick
        // — run unconditionally (not gated on `thundering`) so a bolt struck
        // moments before the storm ends still finishes its life/flashes
        // countdown, matching `LightningBolt.tick` running from vanilla's own
        // `entityTickList` independently of `tickThunder`'s gate.
        mobs.with(|sim| sim.tick_lightning(world_state.difficulty().0, &mut lightning_bolt_rng));
        // `MobSim::tick_lightning`'s fire-ignition candidates: `MobSim` holds
        // only a frozen pathfinding snapshot (`take_lightning_fires`'s own
        // doc), so the live write happens here, gated exactly like
        // `LightningBolt.spawnFire` — air, and `FireBlock::canSurvive`.
        for pos in mobs.with(MobSim::take_lightning_fires) {
            let (min_y, height) = *fire_env.get_or_insert_with(|| {
                let probe = world.column(pos.x.div_euclid(16), pos.z.div_euclid(16));
                (probe.min_y, probe.height)
            });
            let fire_placement_env =
                crate::fire::FireEnv::overworld_in(min_y, height, world_state.difficulty().0, weather.raining);
            if crate::random_tick::is_air_variant(&world.block_state(pos.x, pos.y, pos.z))
                && crate::fire::can_survive(&*world, fire_placement_env, pos)
            {
                let new_state = crate::fire::state_for_placement(&*world, fire_placement_env, pos);
                world.set_block(pos.x, pos.y, pos.z, &new_state);
                block_tick_out.publish(pos.x, pos.y, pos.z, new_state);
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
                // The neighbour-notification half of a write, at the landing
                // cell: the placed block notifies its neighbours, which is what
                // lets a pile settle rather than one block land on top of something
                // that should also have fallen. Column fetched *after* the
                // placement so the propagation sees it.
                let cx = pos.x.div_euclid(16);
                let cz = pos.z.div_euclid(16);
                let mut column = world.column(cx, cz);
                for event in crate::random_tick::propagate_and_react_with_entities_across_chunks(
                    &mut column,
                    cx * 16,
                    cz * 16,
                    &*world,
                    pos.x,
                    pos.y,
                    pos.z,
                    block_ticks,
                    game_tick,
                    Some(&block_entities),
                ) {
                    let (ex, ey, ez) = event.pos;
                    world.set_block(ex, ey, ez, &event.to);
                    publish_moving_piston(&block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                    shove_entities_from_piston(&mobs, &block_tick_out, &*block_ticks, ex, ey, ez, &event.to);
                    post_note_block_vibration(&world, &mobs, (ex, ey, ez), &event.from, &event.to);
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

        // Every live minecart, one tick — rail-following (or off-rail)
        // physics, riding, and the furnace/TNT specials
        // (`crate::mobs::minecart`'s own module doc). Beside `tick_tnt` for
        // the same reason that call is beside `tick_vehicles`: this scope
        // already holds the live world the collision shapes and rail states
        // come from. A TNT minecart's detonation queues into
        // `MobSim::pending_detonations` exactly as primed TNT's does, so it
        // reaches the `take_detonations` drain above on the tick after this
        // one, the same accepted one-tick latency.
        mobs.with(|sim| sim.tick_minecarts(&|x, y, z| world.block_state(x, y, z)));

        // The ender dragon's phase machine and its crystals' healing proc.
        // Unlike its neighbours above this needs no block reads: every input
        // the phase machine consumes — the live crystal count and the nearest
        // player's distance — comes from `MobSim` itself, so it takes no world
        // closure. Without this line the whole fight is inert: `spawn_dragon`
        // and the phase transitions are reachable only from tests, which is
        // exactly the island shape `CLAUDE.md` opens with.
        mobs.with(super::mobs::MobSim::tick_dragons);

        // The wither's own emergence countdown, heal ticks and skull-fire —
        // same shape and same reason as `tick_dragons` immediately above: no
        // block reads needed, and without this line a summoned wither (once
        // something spawns one) is inert the same way an un-ticked dragon
        // would otherwise remain inert.
        mobs.with(super::mobs::MobSim::tick_withers);
        });

        // Per-phase timing (see `TickPhase::ScheduledAndPhysics`'s own doc):
        // closes out everything `scheduled.with`'s closure just ran. Taken
        // immediately after the closure returns (the mutex is already
        // released by here), so this timestamp is outside the lock too.
        let t_scheduled_end = tokio::time::Instant::now();
        clock.record_phase(TickPhase::ScheduledAndPhysics, t_scheduled_end.duration_since(t_weather_end));

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
    // `run_tick_loop` borrows its queues from `ScheduledTickHandle` internally,
    // so tests import only the queue value types here.
    use crate::scheduled_tick::{ChunkScheduledTickQueue, ScheduledTickQueue};
    // For `ResourceKey::from_str` in the grazing gates below.
    use std::str::FromStr;

    fn handles() -> (MobHandle, LiveMobSource, BlockEntityHandle) {
        (
            MobHandle::new(ChunkWorld::new(-64, 384)),
            LiveMobSource::default(),
            BlockEntityHandle::default(),
        )
    }

    /// The live tick loop reaches this central consumer for both block and
    /// fluid drains. Reversed owner completion must not move a callback ahead
    /// of an earlier global `(trigger, priority, insertion)` slot.
    #[test]
    fn scheduled_tick_central_consumer_restores_reversed_owner_completion() {
        fn build_queue() -> ChunkScheduledTickQueue<&'static str> {
            let mut queue = ChunkScheduledTickQueue::new();
            assert!(queue.schedule((-1, 0, 0), "west-first", 4, TickPriority::Normal));
            assert!(queue.schedule((16, 0, 0), "east-second", 4, TickPriority::Normal));
            assert!(queue.schedule((-2, 0, 0), "west-third", 4, TickPriority::Normal));
            queue
        }

        let mut queue = build_queue();
        let batches = queue.drain_due_owner_batches(4, usize::MAX);
        let serial: Vec<_> = build_queue()
            .drain_due(4, usize::MAX)
            .iter()
            .map(|tick| tick.kind)
            .collect();
        let mut completed = batches;
        completed.reverse();
        let completion_order: Vec<_> = completed
            .iter()
            .flat_map(|batch| batch.assignments())
            .map(|assignment| assignment.tick().kind)
            .collect();
        assert_ne!(
            completion_order, serial,
            "control requires reversed owner completion to change raw callback order"
        );
        assert_eq!(
            apply_scheduled_tick_owner_batches(completed)
                .iter()
                .map(|tick| tick.kind)
                .collect::<Vec<_>>(),
            serial,
            "the production central consumer must restore the original callback order"
        );
    }

    /// A minimal [`ChunkSource`] for tests that only need `run_tick_loop` to
    /// have *something* to random-tick against — every column is bare air,
    /// so random ticks run (proving the loop's ordering and timing)
    /// but never produce an event (nothing eligible), which is exactly what
    /// the MSPT/overrun tests in this module want: zero interference from
    /// with the clock behaviour they actually assert on.
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

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; the clock/overrun gates only
            // care that the loop runs, not what this reads.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
        }

        // `run_tick_loop` can forward grazing/random-tick mutations to this
        // (tick.rs's own `world.set_block`), so it must not panic; but the
        // source has no storage, so the edit is deliberately discarded.
        // Explicit rather than inherited.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// `(world, block_tick_out, tick_area)` — the three new `run_tick_loop`
    /// arguments, factored out because every existing
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

    /// LAN shape, asserted at the type level so the remaining gap
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
    /// not `>=`, about (its own main server loop). Must **not** trigger.
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
        for _ in 0..TICK_HISTORY_LEN {
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

    /// The rolling record caps at [`TICK_HISTORY_LEN`] samples: pushing far more
    /// than that must not let the average drift toward the oldest (discarded)
    /// samples. Feed 100 slow ticks, then 100 fast ones; the average must
    /// land near the fast figure, not halfway between the two.
    #[test]
    fn history_window_evicts_the_oldest_samples() {
        let clock = TickClock::new();
        for _ in 0..TICK_HISTORY_LEN {
            clock.record_tick(Duration::from_millis(200));
        }
        for _ in 0..TICK_HISTORY_LEN {
            clock.record_tick(Duration::from_millis(50));
        }
        let stats = clock.stats();
        assert!(
            (stats.mspt_avg_ms - 50.0).abs() < 0.5,
            "expected the 200ms samples to have aged out, got avg {}",
            stats.mspt_avg_ms
        );
        assert_eq!(stats.tick_count, (TICK_HISTORY_LEN * 2) as u64);
    }

    // ---------------------------------------------------------------------
    // Per-phase timing: `TickPhase`/`PhaseStats`/`WorstPhaseWindow` mirror
    // the shape of the MSPT/overrun accounting above, one layer finer.
    // ---------------------------------------------------------------------

    /// [`TICK_PHASE_NAMES`] must stay the same length as [`TickPhase`] has
    /// variants and in discriminant order — the same guard
    /// `lodestone-worldgen-core`'s `STAGE_NAMES` carries for the same
    /// reason: a report joining an index back to a label silently mislabels
    /// every row past the first drift.
    #[test]
    fn tick_phase_names_cover_every_phase_in_discriminant_order() {
        assert_eq!(TICK_PHASE_NAMES.len(), TICK_PHASE_COUNT);
        assert_eq!(TICK_PHASE_NAMES[TickPhase::MobsAndItems as usize], "mobs_and_items");
        assert_eq!(TICK_PHASE_NAMES[TickPhase::WeatherAndSleep as usize], "weather_and_sleep");
        assert_eq!(TICK_PHASE_NAMES[TickPhase::ScheduledAndPhysics as usize], "scheduled_and_physics");
    }

    /// Predicted value, not a magnitude-species guess: ten known samples
    /// (1ms..=10ms) fed through `record_phase`, checked against the
    /// nearest-rank percentile formula worked out by hand. `p50` of ten
    /// ascending samples is the 5th smallest (`ceil(0.50*10)=5`); `p95` and
    /// `p99` both land on the 10th (`ceil(9.5)=10`, `ceil(9.9)=10`) because
    /// ten samples cannot resolve either percentile finer than the max —
    /// that is arithmetic, not a bug in the test.
    #[test]
    fn phase_stats_reports_the_hand_derived_percentiles_for_known_samples() {
        let clock = TickClock::new();
        for ms in 1..=10u64 {
            clock.record_phase(TickPhase::MobsAndItems, Duration::from_millis(ms));
        }
        let stats = clock.phase_stats(TickPhase::MobsAndItems);
        assert_eq!(stats.sample_count, 10);
        assert!((stats.p50_ms - 5.0).abs() < f64::EPSILON, "p50 {}", stats.p50_ms);
        assert!((stats.p95_ms - 10.0).abs() < f64::EPSILON, "p95 {}", stats.p95_ms);
        assert!((stats.p99_ms - 10.0).abs() < f64::EPSILON, "p99 {}", stats.p99_ms);
        assert!((stats.max_ms - 10.0).abs() < f64::EPSILON, "max {}", stats.max_ms);
        // A phase that was never recorded must read as empty, not as a stale
        // read of another phase's buffer — the control that would catch two
        // phases sharing one array index by mistake.
        let untouched = clock.phase_stats(TickPhase::WeatherAndSleep);
        assert_eq!(untouched.sample_count, 0);
        assert_eq!(untouched.max_ms, 0.0);
    }

    /// The percentile ring retains only [`TICK_HISTORY_LEN`] samples, but callers
    /// that drive a clock need an independent cumulative count to prove that
    /// every tick reached a phase recorder after the rolling window fills.
    #[test]
    fn phase_stats_keeps_a_cumulative_count_after_rolling_history_eviction() {
        let clock = TickClock::new();
        let total = TICK_HISTORY_LEN + 7;
        for ms in 1..=total as u64 {
            clock.record_phase(TickPhase::MobsAndItems, Duration::from_millis(ms));
        }

        let stats = clock.phase_stats(TickPhase::MobsAndItems);
        assert_eq!(stats.sample_count, TICK_HISTORY_LEN as u64);
        assert_eq!(stats.total_sample_count, total as u64);
        assert_eq!(stats.max_ms, total as f64);
        assert_eq!(clock.phase_stats(TickPhase::WeatherAndSleep).total_sample_count, 0);
    }

    /// [`PHASE_SOFT_BUDGET`] is 10ms (20% of the 50ms tick period). Feed
    /// exactly three samples over it and two under, interleaved, and require
    /// the counter to land on exactly 3 — a magnitude check, not a "the
    /// counter moved" one — and prove it is per-phase, not global, by
    /// checking a second, untouched phase stayed at zero.
    #[test]
    fn phase_over_budget_counts_exactly_the_samples_that_exceed_the_threshold() {
        let clock = TickClock::new();
        for ms in [1, 12, 9, 15, 50] {
            clock.record_phase(TickPhase::ScheduledAndPhysics, Duration::from_millis(ms));
        }
        assert_eq!(clock.phase_stats(TickPhase::ScheduledAndPhysics).over_budget_count, 3);
        assert_eq!(clock.phase_stats(TickPhase::MobsAndItems).over_budget_count, 0);
    }

    /// The worst-window tracker must report the single largest duration
    /// across *all* phases, name which phase it was, and must not be
    /// overwritten by a later, smaller sample — only a new maximum moves it.
    #[test]
    fn worst_phase_window_tracks_the_global_maximum_and_ignores_smaller_later_samples() {
        let clock = TickClock::new();
        assert!(clock.worst_phase_window().is_none(), "must be empty before any phase is recorded");

        clock.record_phase(TickPhase::MobsAndItems, Duration::from_millis(5));
        clock.record_phase(TickPhase::WeatherAndSleep, Duration::from_millis(20));
        clock.record_phase(TickPhase::ScheduledAndPhysics, Duration::from_millis(3));
        let worst = clock.worst_phase_window().expect("a sample was recorded");
        assert_eq!(worst.phase, TickPhase::WeatherAndSleep);
        assert_eq!(worst.micros, 20_000);

        // A new, larger sample on a *different* phase must replace it.
        clock.record_phase(TickPhase::MobsAndItems, Duration::from_millis(25));
        let worst = clock.worst_phase_window().expect("a sample was recorded");
        assert_eq!(worst.phase, TickPhase::MobsAndItems);
        assert_eq!(worst.micros, 25_000);

        // A smaller sample afterwards must not un-set the recorded worst.
        clock.record_phase(TickPhase::ScheduledAndPhysics, Duration::from_millis(1));
        let worst = clock.worst_phase_window().expect("still recorded");
        assert_eq!(worst.phase, TickPhase::MobsAndItems);
        assert_eq!(worst.micros, 25_000);
    }

    /// `TickClock::stats` is the public snapshot a caller can read without
    /// retaining the clock. Its phase values must be the same values as the
    /// direct phase queries, rather than empty placeholders that make a
    /// healthy-looking snapshot unable to identify the costly phase.
    #[test]
    fn stats_snapshots_every_phase_and_the_recorded_worst_window() {
        let clock = TickClock::new();
        clock.record_phase(TickPhase::MobsAndItems, Duration::from_millis(7));
        clock.record_phase(TickPhase::WeatherAndSleep, Duration::from_millis(12));
        clock.record_phase(TickPhase::ScheduledAndPhysics, Duration::from_millis(25));

        let stats = clock.stats();
        assert_eq!(stats.mobs_and_items, clock.phase_stats(TickPhase::MobsAndItems));
        assert_eq!(stats.weather_and_sleep, clock.phase_stats(TickPhase::WeatherAndSleep));
        assert_eq!(
            stats.scheduled_and_physics,
            clock.phase_stats(TickPhase::ScheduledAndPhysics)
        );
        assert_eq!(stats.worst_phase_window, clock.worst_phase_window());

        // These controls make a zero/default snapshot observably wrong even
        // if the direct-query comparisons above were accidentally weakened.
        assert_eq!(stats.mobs_and_items.sample_count, 1);
        assert_eq!(stats.weather_and_sleep.over_budget_count, 1);
        assert_eq!(stats.scheduled_and_physics.max_ms, 25.0);
        assert_eq!(
            stats.worst_phase_window,
            Some(WorstPhaseWindow {
                phase: TickPhase::ScheduledAndPhysics,
                micros: 25_000,
                tick_count: 0,
            })
        );
    }

    /// **Validation control for the instrument itself.** An idle world with
    /// no players, no mobs and no scheduled ticks (`run_tick_loop`'s own
    /// default wrapper, `EmptyWorld`, `world_tick_args`'s empty tick area)
    /// cannot physically do any of the work a phase boundary measures — and
    /// because this test runs under `start_paused` virtual time with no
    /// `.await` anywhere inside one tick's body (`scheduled.with`'s own doc
    /// comment above states this and it is enforced by the closure's type,
    /// not just asserted), `Instant::now()` cannot advance between any two
    /// of one tick's boundaries either. So every phase, every tick, must
    /// read *exactly* zero — not "small". An instrument whose boundary
    /// accidentally spanned the top-of-loop `sleep_until` wait, or that
    /// leaked a previous tick's timestamp into this one, would show up here
    /// as a large, nonzero reading — the same shape of control as a pure
    /// camera rotation revealing the `vram_bytes` mis-attribution this
    /// crate's evidence standards cite: an input that cannot physically
    /// move the quantity must not move it. Deterministic rather than a
    /// wall-clock timing, so it is immune to this machine's load, unlike
    /// the "idle world" control this task's brief describes in the general
    /// case.
    #[tokio::test(start_paused = true)]
    async fn phase_durations_on_an_idle_world_are_exactly_zero_under_paused_time() {
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
        tokio::task::yield_now().await;

        const TICKS: usize = 20;
        for _ in 0..TICKS {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;

        assert_eq!(clock.tick_count(), TICKS as u64);
        for &phase in &[TickPhase::MobsAndItems, TickPhase::WeatherAndSleep, TickPhase::ScheduledAndPhysics] {
            let stats = clock.phase_stats(phase);
            assert_eq!(stats.sample_count, TICKS as u64, "phase {phase:?} missing samples");
            assert_eq!(stats.max_ms, 0.0, "phase {phase:?} moved on an idle world: {stats:?}");
            assert_eq!(stats.over_budget_count, 0, "phase {phase:?} over budget on an idle world");
        }
        let worst = clock.worst_phase_window().expect("20 ticks recorded samples");
        assert_eq!(worst.micros, 0, "worst window must also read zero on an idle world");
    }

    /// **Real numbers, not a control.** The test above proves the phase
    /// boundaries are placed correctly using a deterministic paused clock,
    /// which by construction can only ever read zero. This one runs the
    /// same idle scenario under *real* wall-clock time, so it reports the
    /// unavoidable per-phase floor cost (scheduling, `tokio::spawn`/`.await`
    /// overhead, the mutex takes) on real hardware — not a loaded server's
    /// actual cost, just the cost of the loop existing at all. Printed with
    /// `--nocapture`. Interpret the output using the measurement guidance in
    /// `docs/tick-scheduling.md#profiling-the-tick-loop-and-world-generation`;
    /// it is not asserted on for a
    /// specific magnitude, only for internal consistency (samples recorded,
    /// percentile ordering holds), because a specific millisecond figure
    /// here would be exactly the "duration gathered while other agents
    /// build gets attributed to the wrong cause" hazard this repo's own
    /// rules warn about.
    #[tokio::test]
    async fn phase_durations_floor_cost_on_an_idle_world_under_real_time() {
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

        const TICKS: u64 = 15;
        while clock.tick_count() < TICKS {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        println!("PHASE_REPORT ticks={}", clock.tick_count());
        for &phase in &[TickPhase::MobsAndItems, TickPhase::WeatherAndSleep, TickPhase::ScheduledAndPhysics] {
            let stats = clock.phase_stats(phase);
            assert!(stats.p50_ms <= stats.p95_ms && stats.p95_ms <= stats.p99_ms && stats.p99_ms <= stats.max_ms);
            println!(
                "PHASE_REPORT phase={:<21} samples={:>3} p50_ms={:>8.3} p95_ms={:>8.3} p99_ms={:>8.3} max_ms={:>8.3} over_budget={}",
                TICK_PHASE_NAMES[phase as usize],
                stats.sample_count,
                stats.p50_ms,
                stats.p95_ms,
                stats.p99_ms,
                stats.max_ms,
                stats.over_budget_count
            );
        }
        if let Some(worst) = clock.worst_phase_window() {
            println!(
                "PHASE_REPORT worst_phase={} worst_us={} at_tick={}",
                TICK_PHASE_NAMES[worst.phase as usize], worst.micros, worst.tick_count
            );
        }
    }

    // ---------------------------------------------------------------------
    // The graze drain connects `MobSim::take_grazes` to the live world.
    // Without it, the simulation has no
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

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; this fixture only records
            // `set_block` calls, nothing reads terrain back.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
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

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
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

        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(
            mobs,
            out,
            block_entities,
            Arc::clone(&clock),
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
        assert_eq!(
            clock.owner_stats().random_tick_owned_chunks,
            45,
            "five eligible ticks must visit the moved 3x3 area exactly once per owned chunk"
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
    // Weather state reaches the feed through the world tick.
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

        // Use a fresh night-skip vote that no connection calls. The
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
                BorderFeed::default(),
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
                BorderFeed::default(),
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

        // a fresh night-skip vote no connection calls (see the
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
                BorderFeed::default(),
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

    // ---------------------------------------------------------------------
    // Lightning reaches the production loop through both
    // `crate::lightning::tick_thunder_for_chunk` and `MobSim::tick_lightning`.
    // Test-only callers would otherwise leave every
    // gate for the mechanism (`crate::lightning`'s own module, and
    // `crate::mobs::lightning`'s sidecar) drove the pieces directly, which
    // is exactly the "hermetic green, island red" pair CLAUDE.md's rule 1
    // warns about for the sibling weather gates just above.
    // ---------------------------------------------------------------------

    /// A thundering, single-chunk world (`world_tick_args`'s
    /// `(0..=0, 0..=0)`) must eventually strike, driven entirely through
    /// [`run_tick_loop_with_weather`] rather than by calling
    /// `spawn_lightning_bolts`/`tick_lightning` directly.
    ///
    /// The production stream is a **fixed** seed
    /// (`crate::lightning::LIGHTNING_STRIKE_SEED`), so the test predicts
    /// exactly which tick strikes: with one chunk
    /// ticked per eligible tick, `should_attempt_strike` draws exactly once
    /// per tick from that same fixed stream, so replaying it here (the
    /// identical draw the production loop will make) finds the exact draw
    /// count to the first zero, and the loop is advanced exactly that many
    /// ticks past the startup deferral — no more, no fewer.
    #[tokio::test(start_paused = true)]
    async fn a_thundering_world_produces_a_real_bolt_through_the_loop() {
        // Predict the exact tick, from the same fixed stream the loop itself
        // draws from — see this test's own doc comment.
        let mut probe = crate::mob_spawn::SpawnRng::new(crate::lightning::LIGHTNING_STRIKE_SEED);
        let mut draws_to_first_strike: u64 = 0;
        loop {
            draws_to_first_strike += 1;
            if probe.next_int(crate::lightning::STRIKE_ROLL_BOUND) == 0 {
                break;
            }
            assert!(
                draws_to_first_strike < 2_000_000,
                "no zero roll found in a generous range; the fixed seed may have changed"
            );
        }
        let total_ticks = INITIAL_RANDOM_TICK_DEFERRAL_TICKS + draws_to_first_strike;

        let (mobs, out, block_entities) = handles();
        let mobs_for_assert = mobs.clone();
        let clock = Arc::new(TickClock::new());
        let (world, block_tick_out, tick_area) = world_tick_args();
        let weather_out = WeatherFeed::default();
        // Raining and thundering for the whole test, never flipping.
        let mut weather = WeatherState::default();
        weather.raining = true;
        weather.thundering = true;
        weather.rain_time = i32::MAX;
        weather.thunder_time = i32::MAX;
        weather.rain_level = 1.0;
        weather.thunder_level = 1.0;

        let vote = SleepVote::new();
        let feed = SleepFeed::default();
        let loop_clock = Arc::clone(&clock);
        tokio::spawn(async move {
            run_tick_loop_with_weather(
                mobs,
                out,
                block_entities,
                loop_clock,
                world,
                block_tick_out,
                tick_area,
                ExplosionFeed::default(),
                weather_out,
                weather,
                &vote,
                &feed,
                crate::region_source::ScheduledTickHandle::default(),
                crate::world_state::WorldStateHandle::default(),
                crate::tick_area::TickFollow::default(),
                BorderFeed::default(),
            )
            .await;
        });
        tokio::task::yield_now().await;

        assert_eq!(
            mobs_for_assert.with(|sim| sim.lightning_bolt_count()),
            0,
            "no strike must have happened before the predicted tick"
        );

        for _ in 0..total_ticks {
            tokio::time::advance(TICK_PERIOD).await;
        }
        tokio::task::yield_now().await;

        assert_eq!(
            mobs_for_assert.with(|sim| sim.lightning_bolt_count()),
            1,
            "the predicted tick must have produced exactly one live bolt, through the real loop"
        );
        let bolts: Vec<_> = mobs_for_assert
            .with(|sim| sim.snapshots())
            .into_iter()
            .filter(|s| s.entity_type.to_string() == crate::lightning::LIGHTNING_BOLT)
            .collect();
        assert_eq!(bolts.len(), 1, "the bolt must reach the same snapshot stream every other entity does");
        assert_eq!(
            clock.owner_stats().thunder_owned_chunks,
            draws_to_first_strike,
            "the one-owner fixture must make one thunder visit for every eligible deterministic draw"
        );
    }

    /// Gate for wiring through the **production** loop, not at
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
                BorderFeed::default(),
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

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
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
    // The dispenser fire arm. `crate::redstone_dispenser`'s
    // tests gate `random_slot`/`plain_toss` in isolation; this one gates the
    // *wiring* — that the drain actually reaches a live container and mob
    // simulation through the production `run_tick_loop`, the island shape
    // CLAUDE.md's rule 1 names (a correct module with zero production
    // callers). Before this arm existed, `TICK_DISPENSER_FIRE` was scheduled
    // and never drained, so a dispenser filled with cobblestone sat there
    // forever. Cobblestone rather than an arrow: arrows now dispense as a
    // projectile, which this plain-toss gate is not
    // testing — see `the_loop_dispenses_an_arrow_as_a_real_projectile` below
    // for that arm.
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

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
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
                "minecraft:cobblestone".parse().expect("valid item key"),
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
                item: "minecraft:cobblestone".parse().expect("valid key"),
                count: 1,
            }],
            "exactly one cobblestone leaves the stack of three — `ItemStack.split(1)`, not the whole stack"
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

    /// The island shape rule 1 names, for `FurnaceTick::lit_changed`: a
    /// registered furnace with fuel and an ingredient really does flip
    /// `Furnace::is_lit` (`tick_all_advances_a_registered_furnace` above
    /// proves that in isolation), but until this arm existed nothing carried
    /// that flip out through `BlockEntityRegistry::tick_all_with_hopper_lock`
    /// into a `ChunkSource::set_block` — so the block a client is streamed
    /// stayed `lit=false` forever. This drives the real production loop, not
    /// the registry directly, so it proves the whole chain.
    #[tokio::test(start_paused = true)]
    async fn the_loop_syncs_a_lit_furnace_to_its_own_block_state() {
        let pos = (11, 6, 9);
        let (px, py, pz) = pos;
        let world = ColumnBackedWorld::with(&[(pos, "minecraft:furnace[facing=north,lit=false]")]);
        let feed = BlockTickFeed::default();
        let (mobs, out, block_entities) = handles();
        block_entities.with(|reg| {
            let mut furnace = crate::furnace::Furnace::new(crate::furnace::FurnaceKind::Furnace);
            furnace.set_fuel(Some(lodestone_model::ItemStack::new(
                "minecraft:coal".parse().expect("valid item key"),
                1,
            )));
            furnace.set_input(Some(lodestone_model::ItemStack::new(
                "minecraft:iron_ore".parse().expect("valid item key"),
                1,
            )));
            reg.insert(
                BlockPos::new(px, py, pz),
                crate::block_entities::BlockEntity::Furnace(furnace),
            );
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
            crate::region_source::ScheduledTickHandle::default(),
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;

        let mut lit_at_tick = None;
        for tick in 1..=8 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            if crate::redstone::get_bool_property(&world.block_state(px, py, pz), "lit") == Some(true) {
                lit_at_tick = Some(tick);
                break;
            }
        }
        assert!(
            lit_at_tick.is_some(),
            "furnace block state never reached lit=true — the tick loop never wrote the flip through"
        );
        assert!(
            world.block_state(px, py, pz).starts_with("minecraft:furnace["),
            "the write must preserve the block's own identity and other properties, not just append lit"
        );
    }

    /// Projectile dispensing: a dispenser loaded with arrows must put a
    /// real `minecraft:arrow` **projectile** on the wire, not a plain-tossed
    /// item entity. The negative control (`item_count` staying `0`) proves the
    /// stack takes the projectile path rather than falling through to `plain_toss`.
    #[tokio::test(start_paused = true)]
    async fn the_loop_dispenses_an_arrow_as_a_real_projectile() {
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
            let mut container = crate::block_entities::BlockEntity::container_of_size(
                "minecraft:dispenser",
                crate::block_entities::CONTAINER_3X3_SIZE,
            );
            container.set_container_slot(
                0,
                Some(lodestone_model::ItemStack::new(
                    "minecraft:arrow".parse().expect("valid item key"),
                    3,
                )),
            );
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

        let mut spawned_at_tick = None;
        for tick in 1..=8 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            if mobs.with(|sim| sim.projectile_count()) >= 1 {
                spawned_at_tick = Some(tick);
                break;
            }
        }
        assert!(
            spawned_at_tick.is_some(),
            "no projectile ever appeared — the arrow arm never fired"
        );
        assert_eq!(
            mobs.with(|sim| sim.item_count()),
            0,
            "an arrow must never fall through to the plain-toss item path"
        );

        let snapshots = mobs.with(|sim| sim.snapshots());
        assert_eq!(snapshots.len(), 1, "exactly one entity must exist: {snapshots:?}");
        assert_eq!(
            snapshots[0].entity_type.to_string(),
            "minecraft:arrow",
            "the dispensed entity must be the arrow itself, not an item entity carrying one"
        );

        // The projectile's spawn point is `dispense_position` shifted `0.1`
        // *higher* than a plain toss's own point — the one arithmetic
        // difference `projectile_dispense_position` exists to apply. East
        // moves only x, so y and z pin the position precisely enough to
        // catch a missing offset.
        let expected_x = f64::from(px) + 0.5 + 0.7;
        let position = snapshots[0].position;
        assert!(
            (position.x - expected_x).abs() < 0.5,
            "position.x = {} is not near the east dispense point {expected_x} (a wide tolerance \
             because the shoot velocity's own randomness is not being pinned here)",
            position.x
        );
        assert!(
            (position.y - (f64::from(py) + 0.5 + 0.1)).abs() < 1e-6,
            "position.y = {} does not match the projectile's own +0.1 offset over a plain toss's point",
            position.y
        );
        assert_eq!(
            position.z,
            f64::from(pz) + 0.5,
            "east does not move z at all"
        );

        // It flies east: a stationary or backward-moving arrow means the
        // facing-derived direction never reached `projectile_velocity`.
        assert!(
            snapshots[0].velocity.x > 0.0,
            "an east-facing dispenser must launch the arrow toward +x: {:?}",
            snapshots[0].velocity
        );
    }

    /// Fluid-bucket dispensing. Two arms in one test share a
    /// dispenser position but run as two independent scenarios (each its own
    /// fresh world/handles), matching this crate's own habit of keeping
    /// ordering and `amount`-shaped discriminators in separate cases: filling
    /// needs an *empty* target, and picking up needs a *source fluid* target
    /// — the two cannot share one fixture.
    #[tokio::test(start_paused = true)]
    async fn the_loop_empties_a_water_bucket_into_the_world_ahead() {
        let pos = (11, 6, 9);
        let (px, py, pz) = pos;
        let target = (px + 1, py, pz); // east, one cell ahead.
        let world = ColumnBackedWorld::with(&[
            (pos, "minecraft:dispenser[facing=east,triggered=true]"),
            (target, "minecraft:air"),
        ]);
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
            let mut container = crate::block_entities::BlockEntity::container_of_size(
                "minecraft:dispenser",
                crate::block_entities::CONTAINER_3X3_SIZE,
            );
            container.set_container_slot(
                0,
                Some(lodestone_model::ItemStack::new(
                    "minecraft:water_bucket".parse().expect("valid item key"),
                    1,
                )),
            );
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

        let mut fired = false;
        for _ in 1..=8 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            if world.block_state(target.0, target.1, target.2) != "minecraft:air" {
                fired = true;
                break;
            }
        }
        assert!(fired, "the target cell never changed — the bucket arm never fired");
        assert_eq!(
            world.block_state(target.0, target.1, target.2),
            "minecraft:water[level=0]",
            "a water bucket must place a water *source*, not a flowing state"
        );

        // The slot swaps to an empty bucket in place — `consumeWithRemainder`,
        // not a plain shrink. A gate that only checked "the stack count
        // dropped" would pass an implementation that merely deleted the
        // filled bucket instead of returning the empty one.
        let remaining = block_entities.with(|reg| {
            reg.get(BlockPos::new(px, py, pz))
                .map(crate::block_entities::BlockEntity::container_slots)
        });
        let slot0 = remaining.as_ref().and_then(|slots| slots[0].clone());
        assert_eq!(
            slot0.as_ref().map(|s| s.item.to_string()),
            Some("minecraft:bucket".to_owned()),
            "slot 0 must hold an empty bucket after emptying, not stay a water bucket or go empty: {slot0:?}"
        );
        assert_eq!(slot0.map(|s| s.count), Some(1));
    }

    /// The pickup half, same shape: a plain bucket dispensed at a water
    /// source ahead must remove the source and swap the slot to a filled
    /// water bucket.
    #[tokio::test(start_paused = true)]
    async fn the_loop_picks_up_a_water_source_with_a_plain_bucket() {
        let pos = (11, 6, 9);
        let (px, py, pz) = pos;
        let target = (px + 1, py, pz);
        let world = ColumnBackedWorld::with(&[
            (pos, "minecraft:dispenser[facing=east,triggered=true]"),
            (target, "minecraft:water[level=0]"),
        ]);
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
            let mut container = crate::block_entities::BlockEntity::container_of_size(
                "minecraft:dispenser",
                crate::block_entities::CONTAINER_3X3_SIZE,
            );
            container.set_container_slot(
                0,
                Some(lodestone_model::ItemStack::new(
                    "minecraft:bucket".parse().expect("valid item key"),
                    1,
                )),
            );
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

        let mut fired = false;
        for _ in 1..=8 {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            if world.block_state(target.0, target.1, target.2) == "minecraft:air" {
                fired = true;
                break;
            }
        }
        assert!(fired, "the target's water source never disappeared — the pickup arm never fired");

        let remaining = block_entities.with(|reg| {
            reg.get(BlockPos::new(px, py, pz))
                .map(crate::block_entities::BlockEntity::container_slots)
        });
        let slot0 = remaining.as_ref().and_then(|slots| slots[0].clone());
        assert_eq!(
            slot0.as_ref().map(|s| s.item.to_string()),
            Some("minecraft:water_bucket".to_owned()),
            "slot 0 must hold a filled water bucket after pickup: {slot0:?}"
        );
        assert_eq!(slot0.map(|s| s.count), Some(1));
    }

    /// `post_note_block_vibration` is what makes every one of
    /// this file's four `propagate_and_react_with_entities` consumers reach
    /// an allay's ear — see this function's own doc for the previously-
    /// disclosed (now re-verified false) blocker it closes. A rising-edge
    /// transition on an unburied harp note block, one block from a spawned
    /// allay, must post a vibration `MobSim::tick`'s own `resolve_vibrations`
    /// then resolves into `SimMob::allay_liked_noteblock`.
    #[test]
    fn a_note_block_pulse_reaches_an_allays_ear() {
        let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
        let allay_id = mobs.with(|sim| {
            sim.spawn_species(
                lodestone_model::ResourceKey::from_str("minecraft:allay").expect("valid key"),
                lodestone_model::Vec3::new(1.0, 0.0, 0.0),
            )
            .id()
        });

        let world: Arc<EmptyWorld> = Arc::new(EmptyWorld);
        let from = "minecraft:note_block[instrument=harp,note=0,powered=false]";
        let to = "minecraft:note_block[instrument=harp,note=0,powered=true]";
        post_note_block_vibration(&world, &mobs, (0, 0, 0), from, to);

        mobs.with(MobSim::tick);

        assert_eq!(
            mobs.with(|sim| sim.get(allay_id).expect("alive").allay_liked_noteblock()),
            Some(lodestone_model::Vec3::new(0.5, 0.5, 0.5)),
            "a rising-edge pulse one block away must reach the allay's LIKED_NOTEBLOCK_POSITION, \
             at the block-centre position post_note_block_vibration posts"
        );
    }

    /// **Control**: the falling edge never plays a pulse
    /// (`redstone_note_block::on_neighbor_changed`'s own gate), so an
    /// identical fixture with `from`/`to` swapped must post nothing and the
    /// allay must never hear anything — proving the positive test above
    /// exercises a real gate, not one that fires unconditionally.
    #[test]
    fn a_note_block_falling_edge_posts_nothing() {
        let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
        let allay_id = mobs.with(|sim| {
            sim.spawn_species(
                lodestone_model::ResourceKey::from_str("minecraft:allay").expect("valid key"),
                lodestone_model::Vec3::new(1.0, 0.0, 0.0),
            )
            .id()
        });

        let world: Arc<EmptyWorld> = Arc::new(EmptyWorld);
        let from = "minecraft:note_block[instrument=harp,note=0,powered=true]";
        let to = "minecraft:note_block[instrument=harp,note=0,powered=false]";
        post_note_block_vibration(&world, &mobs, (0, 0, 0), from, to);

        mobs.with(MobSim::tick);

        assert_eq!(
            mobs.with(|sim| sim.get(allay_id).expect("alive").allay_liked_noteblock()),
            None,
            "a falling edge must never be heard as a note-block play"
        );
    }

    /// Piston entity shoving is driven through the production shape: a lit torch
    /// triggers a real `crate::random_tick::propagate_and_react` piston
    /// extension (the same call `crate::piston`'s own oracle gate uses to
    /// build a real rig, not a hand-fabricated one), and this file's own
    /// [`shove_entities_from_piston`] — called exactly as it is at every one
    /// of this file's real `propagate_and_react_with_entities` consumers —
    /// must shove a mob standing in the pushed block's destination cell.
    #[test]
    fn a_real_piston_extension_shoves_a_mob_standing_in_its_path() {
        let mut column = crate::chunk::ChunkColumn::new(0, 16);
        // `piston[facing=south,extended=false]` at (4, 1, 8), a pushable
        // dirt block one cell south (the push direction), and an unlit
        // torch two cells west of the piston — the same rig shape
        // `redstone_piston_order_oracle_gate.rs`'s own `piston_rig` uses.
        column.set_block(4, 1, 8, "minecraft:piston[facing=south,extended=false]");
        column.set_block(4, 1, 9, "minecraft:dirt");
        column.set_block(3, 1, 8, &crate::redstone_torch::set_standing_lit(false));

        let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
        // Standing exactly where the pushed dirt is about to land — the
        // direct "a block is about to occupy my cell" case.
        let pig_id = mobs.with(|sim| {
            sim.spawn_species(
                lodestone_model::ResourceKey::from_str("minecraft:pig").expect("valid key"),
                lodestone_model::Vec3::new(4.5, 1.0, 10.5),
            )
            .id()
        });
        let before = mobs.with(|sim| sim.get(pig_id).expect("alive").position());

        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        column.set_block(3, 1, 8, &crate::redstone_torch::set_standing_lit(true));
        let events = crate::random_tick::propagate_and_react(
            &mut column,
            0,
            0,
            3,
            1,
            8,
            &mut block_ticks,
            40,
        );

        // Premise check: the extension actually produced a `moving_piston`
        // write at the dirt's own destination — otherwise the shove below
        // would pass vacuously (nothing to shove from).
        assert!(
            events.iter().any(|e| {
                e.pos == (4, 1, 10) && crate::piston::is_moving_piston(&e.to)
            }),
            "PREMISE FAILED: extending must write a moving_piston at the pushed dirt's own \
             destination (4, 1, 10) -- events: {events:?}"
        );

        for event in &events {
            let (ex, ey, ez) = event.pos;
            shove_entities_from_piston(&mobs, &BlockTickFeed::default(), &block_ticks, ex, ey, ez, &event.to);
        }

        let after = mobs.with(|sim| sim.get(pig_id).expect("alive").position());
        assert_ne!(before, after, "a mob standing where the pushed block lands must be shoved");
        assert!(
            (after.z - before.z - 1.0).abs() < 1e-9 && after.x == before.x && after.y == before.y,
            "must move exactly one block further south (the push direction), no other axis: \
             before={before:?} after={after:?}"
        );
    }

    /// The same real rig as
    /// [`a_real_piston_extension_shoves_a_mob_standing_in_its_path`], but
    /// checking [`shove_entities_from_piston`]'s other production effect —
    /// the [`crate::effects::WorldEffect::PistonPlayerPush`] a connection
    /// reads to correct a player standing in the same swept region, since
    /// `MobSim` (asserted above) has no reach to a connected player at all.
    #[test]
    fn a_real_piston_extension_publishes_a_player_push_effect_for_its_own_swept_region() {
        let mut column = crate::chunk::ChunkColumn::new(0, 16);
        column.set_block(4, 1, 8, "minecraft:piston[facing=south,extended=false]");
        column.set_block(4, 1, 9, "minecraft:dirt");
        column.set_block(3, 1, 8, &crate::redstone_torch::set_standing_lit(false));

        let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        column.set_block(3, 1, 8, &crate::redstone_torch::set_standing_lit(true));
        let events = crate::random_tick::propagate_and_react(
            &mut column,
            0,
            0,
            3,
            1,
            8,
            &mut block_ticks,
            40,
        );
        assert!(
            events.iter().any(|e| {
                e.pos == (4, 1, 10) && crate::piston::is_moving_piston(&e.to)
            }),
            "PREMISE FAILED: extending must write a moving_piston at the pushed dirt's own \
             destination (4, 1, 10) -- events: {events:?}"
        );

        let block_tick_out = BlockTickFeed::default();
        for event in &events {
            let (ex, ey, ez) = event.pos;
            shove_entities_from_piston(&mobs, &block_tick_out, &block_ticks, ex, ey, ez, &event.to);
        }

        // Single-consumer drain (see `BlockTickFeed::drain_effects_for`'s own
        // doc) — any uuid works, since this effect is `publish_effect`'d with
        // no `except`.
        let published = block_tick_out.drain_effects_for(uuid::Uuid::nil());
        let push = published.iter().find_map(|effect| match effect {
            crate::effects::WorldEffect::PistonPlayerPush { source, dest, push_delta } => {
                Some((*source, *dest, *push_delta))
            }
            _ => None,
        });
        let (source, dest, push_delta) =
            push.unwrap_or_else(|| panic!("no PistonPlayerPush effect was published, got {published:?}"));

        assert_eq!(dest, BlockPos::new(4, 1, 10), "must name the pushed dirt's own destination cell");
        assert_eq!(source, BlockPos::new(4, 1, 9), "must name the cell the dirt vacated");
        assert!(
            (push_delta.z - 1.0).abs() < 1e-9 && push_delta.x == 0.0 && push_delta.y == 0.0,
            "the push direction must be one block south, matching the mob shove above: {push_delta:?}"
        );
    }

    /// **Control**: the identical rig, but the piston never fires (the
    /// torch stays unlit) — no `moving_piston` write exists anywhere, and
    /// the pig must not move at all. Without this, the positive test above
    /// could pass merely because `shove_entities_from_piston` moves every
    /// mob regardless of whether a real piston event occurred.
    #[test]
    fn an_unlit_torch_never_extends_the_piston_and_never_shoves_anyone() {
        let mut column = crate::chunk::ChunkColumn::new(0, 16);
        column.set_block(4, 1, 8, "minecraft:piston[facing=south,extended=false]");
        column.set_block(4, 1, 9, "minecraft:dirt");
        column.set_block(3, 1, 8, &crate::redstone_torch::set_standing_lit(false));

        let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
        let pig_id = mobs.with(|sim| {
            sim.spawn_species(
                lodestone_model::ResourceKey::from_str("minecraft:pig").expect("valid key"),
                lodestone_model::Vec3::new(4.5, 1.0, 10.5),
            )
            .id()
        });
        let before = mobs.with(|sim| sim.get(pig_id).expect("alive").position());

        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Torch left unlit -- no re-light this time.
        let events = crate::random_tick::propagate_and_react(
            &mut column,
            0,
            0,
            3,
            1,
            8,
            &mut block_ticks,
            40,
        );
        assert!(
            !events.iter().any(|e| crate::piston::is_moving_piston(&e.to)),
            "PREMISE: an already-unlit torch must produce no piston write at all -- events: {events:?}"
        );

        for event in &events {
            let (ex, ey, ez) = event.pos;
            shove_entities_from_piston(&mobs, &BlockTickFeed::default(), &block_ticks, ex, ey, ez, &event.to);
        }

        let after = mobs.with(|sim| sim.get(pig_id).expect("alive").position());
        assert_eq!(before, after, "with no piston event at all, nothing must move");
    }
}
