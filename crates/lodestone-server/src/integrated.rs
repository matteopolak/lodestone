//! [`IntegratedServer`] — the reachable entry point for singleplayer.
//!
//! Everything else in this crate is a *primitive*: [`serve_connection`] is an
//! `async fn` that serves exactly one connection, delivers the initial view,
//! and then keeps serving (keep-alives, movement, further acknowledgements)
//! until the client disconnects. That is the right shape for a test, but a
//! menu / shell that wants to *start singleplayer* needs a handle it can hold,
//! query for its address, and shut down cleanly without leaking a task.
//! `IntegratedServer` is that handle.
//!
//! It deliberately offers the two transports the plan (§8) calls for behind the
//! **same** [`serve_connection`] loop, which is the whole point — singleplayer
//! and open-to-LAN are the identical server, differing only in transport:
//!
//! * [`IntegratedServer::open_in_memory`] — a [`memory_pair`] duplex. The server
//!   half is served in-process; the client half is handed back for
//!   `lodestone-client`'s `connect_with`. No socket, no port, works on wasm
//!   targets that have a task spawner. This is singleplayer.
//! * [`IntegratedServer::bind`] — a real `TcpListener` (native only). Every
//!   accepted socket is served by the same loop. This is open-to-LAN, and it
//!   exists to prove the claim that LAN falls out of singleplayer for free.
//!
//! Transport choice, justified: the in-memory duplex is the default for
//! singleplayer because it needs no port, races nothing, and is the path that
//! eventually lets a browser build run singleplayer with no server at all. TCP
//! is offered alongside — not instead — because binding a loopback socket is the
//! honest way to exercise real framing end-to-end and is what "open to LAN"
//! actually is. Both run byte-for-byte the same [`serve_connection`].
//!
//! Spawning requires a task-spawning context: an entered Tokio runtime natively
//! (a shell that runs under `#[tokio::main]` already satisfies this), or the
//! browser event loop on wasm (`wasm-bindgen-futures`' `spawn_local`, selected
//! behind the [`spawn`](crate::spawn) seam). The in-memory path is thus the one
//! that lets a browser build run singleplayer with no server at all.
//!
//! [`serve_connection`]: crate::serve_connection
//! [`memory_pair`]: lodestone_net::memory_pair

use std::sync::Arc;

use lodestone_net::{Connection, memory_pair};
use tokio::io::DuplexStream;
use tokio::sync::Notify;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
#[cfg(not(target_arch = "wasm32"))]
use crate::chunk::generate_columns_offloaded;
use crate::chunk_store::ChunkStore;
use crate::dimension::{Dimension, DimensionalSource};
#[cfg(not(target_arch = "wasm32"))]
use crate::mobs::ChunkWorld;
use crate::mobs::{LiveMobSource, MobHandle};
use crate::players::{PlayerAwareSource, PlayerRegistry};
use crate::protocol::ServerProtocol;
use crate::server::{
    EntitySource, NoEntities, serve_connection_shared,
    serve_connection_with_mob_events_and_commands_shared, serve_connection_with_mob_events_shared,
};
use crate::spawn::{Task, spawn};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock, TickStats};
// `run_tick_loop`/`run_tick_loop_with_weather` (like `open_in_memory_with_mobs`
// and, since issue #439, `bind` — their callers) are
// `#[cfg(not(target_arch = "wasm32"))]`-gated in `tick.rs` — these imports must
// carry the identical `cfg`, or they are unresolved-import hard errors on
// wasm32 regardless of whether the names are ever reached at that target.
// **This was already broken on `main` before this change**: the two
// functions this loop replaces (`mobs::run_mob_tick_loop`,
// `block_entities::run_block_entity_tick_loop`) were imported by this same
// file with no such gate, so `cargo build -p lodestone-server --target
// wasm32-unknown-unknown` (the check `scripts/wasm-check.sh` runs) was
// already red — re-verified directly in a throwaway worktree at this
// crate's own pre-#284 `HEAD`, not assumed. Fixed here rather than left,
// since this refactor already touches every one of these imports.
#[cfg(not(target_arch = "wasm32"))]
use crate::tick::run_tick_loop_with_weather;
// Issue #325: the night-skip vote and its feed, wired into
// `open_in_memory_with_mobs_using` (singleplayer) — see that constructor and
// `crate::sleep`'s module doc. Native-only for the same reason the tick-loop
// import above is: `run_tick_loop_with_weather` is `cfg`-gated, and the
// sleep-feed `container_sync_tick` arm in `serve_play` is native-only too.
#[cfg(not(target_arch = "wasm32"))]
use crate::sleep::{SleepFeed, SleepVote};
// Issue #325 calls `run_tick_loop_with_weather` directly (to carry the real
// sleep vote), and that function needs the weather pair even though this crate
// does not wire weather yet — see the call in
// `open_in_memory_with_mobs_using`.
#[cfg(not(target_arch = "wasm32"))]
use crate::weather::{WeatherFeed, WeatherState};

/// Chebyshev radius, in chunks, of the region [`IntegratedServer::bind`]'s
/// world tick loop random-ticks around the origin (issue #439).
///
/// A fixed constant rather than a `bind` parameter, and rather than something
/// derived from where players actually are, because this crate has no
/// loaded-chunk registry to derive it from — the same acknowledged limitation
/// `tick::run_tick_loop`'s own doc comment records for
/// `open_in_memory_with_mobs`'s `mob_area`. `docs/plans/chunk-lifecycle.md`
/// (#289) is what replaces it with a ticket-driven set; until then, widening
/// it costs a full generator run per chunk per tick, which is why it is small.
#[cfg(not(target_arch = "wasm32"))]
const LAN_TICK_RADIUS: i32 = 2;

/// One LAN connection's private view of the world tick loop's output, plus a
/// liveness flag the connection task clears on its way out (issue #439).
///
/// See the relay arm in [`IntegratedServer::bind`] for why each connection
/// needs its own pair rather than sharing the tick loop's: both feeds are
/// drain-all, so sharing one would let whichever connection drained first
/// consume every other player's updates.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct LanSubscriber {
    block_ticks: BlockTickFeed,
    explosions: ExplosionFeed,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

/// Hand-written rather than derived: `AtomicBool::default()` is **`false`**, so
/// a derived `Default` would mark every brand-new subscriber dead and the relay
/// would prune it before publishing a single update — a fan-out that silently
/// delivers nothing, which no compile error would have caught.
#[cfg(not(target_arch = "wasm32"))]
impl Default for LanSubscriber {
    fn default() -> Self {
        Self {
            block_ticks: BlockTickFeed::default(),
            explosions: ExplosionFeed::default(),
            alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl LanSubscriber {
    /// `false` once this connection's task has returned, so the relay can drop
    /// the subscriber instead of publishing into a feed nobody will ever read.
    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Environment variable that re-enables [`crate::mobs::seed_demo_mobs`]'s
/// population for a debug session. Any value, including empty, enables it.
pub const DEMO_MOBS_ENV: &str = "LODESTONE_DEMO_MOBS";

/// How many demo mobs a world actually gets, given what its caller asked for.
///
/// **Zero, unless [`DEMO_MOBS_ENV`] is set.**
///
/// # Why the constructor's own argument is not simply honoured
///
/// `seed_demo_mobs` is not spawning; it is a fixed ring of six mobs — zombie, cow,
/// wolf, blaze, **guardian**, creeper — placed around the world spawn once, at
/// world open, to give issue #217's computed AI motion something to move (see
/// `crate::mobs::DEMO_SPECIES`, which says so). It was always a development
/// fixture, and it shipped: a new singleplayer world greeted the player with a
/// guardian flopping about on dry land next to a blaze.
///
/// The honest end state is that the two production constructors take no such
/// argument at all, and their `mob_count` parameter is removed along with
/// `lodestone-shell`'s literal `6` at the call site in `net.rs`. That is a
/// cross-crate change; this function is the server-side half, and it is complete
/// on its own — a caller passing `6` now gets zero mobs, so the shell's value has
/// no effect either way and the two halves can land independently.
///
/// Real mob **spawning** (issues #222/#221) is a different feature entirely and is
/// what should eventually populate a world. `MobSim` and every roster table stay
/// exactly as they are: this removes a hardcoded fixture, not the simulation.
/// `MobSim::run_spawn_cycle` is still the seam a real
/// `SpawnCandidateSource` plugs into.
#[must_use]
pub fn demo_mob_count(requested: usize) -> usize {
    if std::env::var_os(DEMO_MOBS_ENV).is_some() {
        requested
    } else {
        0
    }
}

/// Labels `overworld` as the overworld and makes the Nether reachable from it, so a
/// portal in this world leads somewhere.
///
/// # What this does and does not cost
///
/// Wrapping is free: [`DimensionalSource`] forwards every [`ChunkSource`] method, so
/// the world behaves identically until something travels. The Nether's generator and
/// `ChunkStore` are built by the closure below **on the first portal trip**, never at
/// world open — see [`DimensionalSource::with_siblings`] for why that matters to a
/// test suite where every test opens a world.
///
/// # The seed
///
/// Taken from [`crate::worldgen_data::active_world_seed`], the same static
/// `crate::natural_spawn` reads for slime chunks. That is the right answer for every
/// world whose overworld came from `overworld_chunk_source` (which sets it), and it
/// is the only answer available here: `overworld` is caller-supplied and generic, so
/// this function cannot ask it. A world built on a hand-rolled test source therefore
/// gets a Nether on whatever seed was last used — harmless, because such a world has
/// no obsidian to light either, and it is why the Nether is lazy rather than eager.
///
/// # `retention` is the same policy the overworld got
///
/// Not a smaller one. The player streams the same square in either dimension, and a
/// capacity that does not cover the streamed view puts the columns under their feet
/// permanently in eviction range at ~909 ms a column to regenerate — see
/// `crate::chunk_store`'s module docs.
fn with_nether<S>(overworld: S, view_radius: i32, uncapped: bool) -> DimensionalSource<S>
where
    S: ChunkSource + 'static,
{
    let portals = crate::portal::PortalIndex::new();
    let shared = portals.clone();
    let factory: crate::dimension::SiblingFactory = Arc::new(move |dimension| match dimension {
        Dimension::Nether => {
            let seed = crate::worldgen_data::active_world_seed();
            let terrain = crate::worldgen_data::nether_chunk_source(seed);
            let store = if uncapped {
                ChunkStore::for_integrated_view_radius(terrain, view_radius)
            } else {
                ChunkStore::for_view_radius(terrain, view_radius)
            };
            // `alone`, not `with_siblings`: the way *home* is the source the
            // connection joined with, which `crate::server` still holds. See
            // `DimensionalSource`'s "the links are one-directional" note.
            Some(Arc::new(DimensionalSource::alone(
                store,
                Dimension::Nether,
                shared.clone(),
            )) as Arc<dyn ChunkSource>)
        }
        Dimension::Overworld => None,
    });
    DimensionalSource::with_siblings(overworld, Dimension::Overworld, factory, portals)
}

/// Spawns `fut` racing against `shutdown`'s notification — whichever finishes
/// first ends the task. The unified background tick loop
/// [`open_in_memory_with_mobs`](IntegratedServer::open_in_memory_with_mobs)
/// starts (`tick::run_tick_loop`, issue #284) needs exactly this shape, so it
/// exists once here rather than once per call site.
///
/// # History: this used to be shared by *two* tick tasks, not one
///
/// Before #284, this helper backed two separate spawn sites
/// (`mobs::run_mob_tick_loop` and `block_entities::run_block_entity_tick_loop`,
/// unified behind this one function in `a6cc60a`). #284 went one step
/// further and merged the two *loops themselves* into
/// [`crate::tick::run_tick_loop`], leaving a single call site. Issue #439 added
/// the second: [`bind`](IntegratedServer::bind) spawns the same loop for LAN, so
/// this helper is once again genuinely shared rather than merely being the one
/// place the shutdown-race wrapper is written. Native only, like the tick loop
/// itself and every caller of this function.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_tick_task<F>(shutdown: &Arc<ShutdownSignal>, fut: F) -> Task
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let signal = shutdown.clone();
    spawn(async move {
        tokio::select! {
            _ = signal.notified() => {}
            _ = fut => {}
        }
    })
}

/// The shutdown signal, **sticky** — a bare [`Notify`] is not, and that cost a
/// 25-minute hang.
///
/// # The lost wakeup
///
/// Every background task here is `select!`ed against this signal, and
/// [`IntegratedServer::shutdown`] *joins* several of them: the connection task,
/// the tick task, the query listener, LAN discovery. Joining is only safe if the
/// signal is guaranteed to arrive.
///
/// `Notify::notify_waiters` **stores no permit**. It wakes the tasks registered as
/// waiters at that instant and nothing else, and a `notified()` future does not
/// register until it is first polled. So a `shutdown()` that runs before a
/// just-spawned task has been polled once loses the notification outright: the
/// `select!`'s signal arm never completes, the other arm is a serve loop that
/// never returns on its own, and `join().await` waits forever.
///
/// That is a race on task scheduling, so it is invisible on an idle machine and
/// reproducible on a loaded one — which is exactly the reported behaviour:
/// `tests/level_dat_round_trip.rs` passes in 0.8 s alone (measured, twice) and hung
/// for ~25 minutes in a contended workspace run, taking the shared cargo lock with
/// it. Its `_client` end stays alive for the whole test, so the connection task
/// has no other way to finish.
///
/// # Why this is not fixable on the notifying side
///
/// There is nothing `shutdown()` can do about it: the defect is that the waiter
/// was not yet listening. Re-notifying in a loop would be a race against a race,
/// and a timeout on the join would convert a hang into a silent data-loss window —
/// the final flush is ordered *after* those joins precisely so nothing can mark a
/// chunk dirty afterwards.
///
/// So the state has to be sticky, and the two orderings below are what make it
/// impossible to lose: the waiter **registers before it checks the flag**, and the
/// trigger **sets the flag before it notifies**. Whichever order the two tasks
/// interleave in, at least one of the two observations fires.
///
/// # Not gated to native, deliberately
///
/// This first landed behind `#[cfg(not(target_arch = "wasm32"))]` — copied from
/// [`spawn_tick_task`] above, which really is native-only — while its field and
/// three constructor calls stayed unconditional, so the crate compiled natively and
/// not for `wasm32`. Spreading the gate to match would have been the wrong repair:
/// nothing in this type is native-only (`Notify` comes from tokio's `sync` feature,
/// which the wasm target's own dependency entry enables, and `AtomicBool` is core),
/// and the browser build genuinely runs [`IntegratedServer`] — in-process
/// singleplayer over a `DuplexStream` is the whole point of that entry. A shutdown
/// signal that did not exist there would leave the browser no way to stop its own
/// server. Only [`Self::notify_handle`] is gated, because its one consumer is.
#[derive(Debug, Default)]
struct ShutdownSignal {
    notify: Arc<Notify>,
    fired: std::sync::atomic::AtomicBool,
}

impl ShutdownSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Fire the signal. Idempotent, and safe to call from `Drop`.
    fn trigger(&self) {
        // Flag first, notify second — see the type's doc comment. Reversing these
        // two lines restores the lost wakeup.
        self.fired
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Resolves once [`Self::trigger`] has been called, **including when it was
    /// called before this future existed**.
    async fn notified(&self) {
        let fut = self.notify.notified();
        let mut fut = std::pin::pin!(fut);
        // `enable()` registers this waiter *now*, without awaiting. Doing it
        // before the load below is the half of the fix that lives on this side:
        // a `trigger` that runs after this line cannot miss us, and one that ran
        // before it is caught by the load.
        fut.as_mut().enable();
        if self.fired.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        fut.await;
    }

    /// The raw [`Notify`], for `crate::rcon`'s listener — which is *aborted* rather
    /// than joined on shutdown, so a lost wakeup there costs nothing and does not
    /// justify widening this type across another module.
    ///
    /// The one member of this type that is genuinely native-only, because rcon is:
    /// a browser has no TCP listener to bind. This is where the gate belongs, and
    /// the whole of it.
    #[cfg(not(target_arch = "wasm32"))]
    fn notify_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }
}

/// A running integrated server that owns its serving task(s).
///
/// Dropping the handle signals shutdown and aborts the task, so a server can
/// never outlive the value that started it — the "can't leak a thread"
/// guarantee a shell consuming this needs.
#[derive(Debug)]
pub struct IntegratedServer {
    #[cfg(not(target_arch = "wasm32"))]
    local_addr: Option<std::net::SocketAddr>,
    shutdown: Arc<ShutdownSignal>,
    task: Task,
    /// The unified world-tick task (issue #284: mob sim + block entities, one
    /// loop), present only when this handle was built by
    /// [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs). Kept
    /// separate from `task` (rather than folded into the same future) because
    /// the world is meant to keep ticking independently of any one
    /// connection — see that constructor's own doc comment. Before #284 this
    /// was two separate fields (`mob_task`, `block_entity_task`) for two
    /// separate loops; merging the loops made the second field redundant.
    tick_task: Option<Task>,
    /// MSPT/TPS/overrun accounting for `tick_task` (issue #285) — `Some` iff
    /// `tick_task` is, and read through [`tick_stats`](Self::tick_stats).
    clock: Option<Arc<TickClock>>,
    /// The read-only witness for this server's own `bevy_ecs::World` (issue
    /// #433 Phase 0), `Some` iff `tick_task` is — the `World` itself is owned
    /// outright by that task and has no lock, so this handle is the *only*
    /// thing about it observable from here. Read through
    /// [`server_tick_count`](Self::server_tick_count); see
    /// `crate::ecs::ServerTickWitness` for why it is a one-way valve rather
    /// than an accessor.
    server_tick: Option<crate::ecs::ServerTickWitness>,
    /// The one-shot mob-seeding task (issue #454), `Some` only for
    /// [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs).
    ///
    /// It exists as a *third* task rather than as a prologue to `tick_task`
    /// because `tick_task`'s clock must start immediately: putting an `.await`
    /// in front of `run_tick_loop` delays its first `Instant::now()` by however
    /// long terrain generation takes, which is both the stall this issue removes
    /// and a silent break of `integrated_memory.rs`'s paused-clock gate ("5 tick
    /// periods must produce exactly 5 ticks" cannot hold if the loop has not
    /// started yet). Seeding races `shutdown` like the tick task does, so it
    /// still cannot outlive this handle.
    seed_task: Option<Task>,
    /// The world-save handle (issue #437), `Some` only for
    /// [`open_persistent_with_mobs`](Self::open_persistent_with_mobs).
    ///
    /// Held here so [`shutdown`](Self::shutdown) can flush the world before the
    /// handle goes away — a singleplayer world that only saved on an autosave
    /// timer would lose everything since the last tick on a clean quit, which
    /// is the common case rather than the rare one.
    #[cfg(not(target_arch = "wasm32"))]
    save: Option<crate::region_source::WorldSaveHandle>,
    /// The autosave timer task (issue #437), `Some` alongside `save`.
    ///
    /// A fourth task rather than a step inside `run_tick_loop`, for the same
    /// reason `seed_task` is a third: the tick loop's budget is 50 ms and a
    /// region write is unbounded. It races `shutdown` like the others, so it
    /// cannot outlive this handle.
    #[cfg(not(target_arch = "wasm32"))]
    autosave_task: Option<Task>,
    /// The world's `level.dat` (issue #468's gap list), `Some` alongside
    /// `save`.
    ///
    /// Stamped with `Time` and `LastPlayed` on every save and at shutdown, so
    /// a world's age accumulates across sessions instead of restarting. See
    /// [`crate::region_source::LevelDatHandle`] for why the base tick count
    /// lives there rather than in [`TickClock`].
    #[cfg(not(target_arch = "wasm32"))]
    level_dat: Option<std::sync::Arc<crate::region_source::LevelDatHandle>>,
    /// The `entities/` region store (issue #303), `Some` alongside `save`.
    ///
    /// Paired with `mobs` below, and both are needed rather than one: the store
    /// is the disk, the handle is the population, and an entity save is a read of
    /// the second written through the first. Held here for the same reason
    /// `level_dat` is — [`shutdown`](Self::shutdown) must flush the mobs before
    /// the handle goes away, or a clean quit loses every mob spawned since the
    /// last autosave.
    #[cfg(not(target_arch = "wasm32"))]
    entity_storage: Option<crate::entity_storage::EntityStorage>,
    /// The live mob simulation, `Some` for every constructor that starts a tick
    /// loop.
    ///
    /// **Not new shared state**: [`MobHandle`] already exists and is already
    /// cloned into the tick task and every connection. This field is a third
    /// clone of the same handle so the save path can read the population without
    /// a channel, exactly as `save`/`level_dat` above reach persistence.
    #[cfg(not(target_arch = "wasm32"))]
    mobs: Option<MobHandle>,
    /// Issues #327/#328/#323: the world's shared game rules, difficulty and clock.
    /// The **same** handle the tick loop advances and every connection reads; kept
    /// here so the persistence path can load it at open and stamp it on save.
    world_state: crate::world_state::WorldStateHandle,
    /// The RCON listener task (issue #331), `Some` once
    /// [`start_rcon`](Self::start_rcon) has been called.
    ///
    /// Races the same `shutdown` notify every other background task races, so
    /// it cannot outlive this handle — `shutdown()` and `Drop` both abort it as
    /// a belt-and-suspenders, exactly like `autosave_task`, because a task
    /// parked in `accept()` cannot see the notify until a new connection
    /// arrives.
    #[cfg(not(target_arch = "wasm32"))]
    rcon_task: Option<Task>,
    /// The GameSpy4/UT3 query listener task (issue #332), `Some` only for
    /// [`bind`](Self::bind), which starts it automatically on the same address
    /// as the game TCP socket (UDP and TCP port spaces are independent).
    ///
    /// Unlike the RCON listener it is **joined** on shutdown rather than
    /// aborted: the run loop races the `shutdown` notify directly (through
    /// [`spawn_tick_task`]), so once the notify fires the task returns promptly
    /// and the UDP port is released before `shutdown()` returns.
    #[cfg(not(target_arch = "wasm32"))]
    query_task: Option<Task>,
    /// The LAN-discovery multicast broadcaster (issue #535), `Some` only when
    /// [`LanConfig::discovery`] asked for one and the UDP bind succeeded.
    /// Joined on shutdown for the same reason `query_task` is.
    #[cfg(not(target_arch = "wasm32"))]
    discovery_task: Option<Task>,
}

/// Everything an open-to-LAN host can configure (issue #535).
///
/// Four subsystems here were implemented, gated and then unreachable, because
/// [`IntegratedServer::bind`] took no way to say anything about them and every
/// other constructor passed `::default()`/`::none()`. This is the "config
/// surface" half of that issue: RCON (#331), the query listener (#332),
/// resource-pack pushes (#334), plugin channels (#335) and commands (#48).
///
/// `Default` reproduces `bind`'s pre-#535 behaviour exactly — the query
/// listener on, everything else off — so `bind` is now a thin wrapper over
/// [`IntegratedServer::open_to_lan`] and no existing caller changes behaviour.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct LanConfig {
    /// The server's own view-distance cap. Every connection's requested
    /// distance is clamped to it (#545).
    pub view_radius: i32,
    /// Start an RCON listener (#331). `None` — the default — leaves the port
    /// closed. The password is in the config; a `port` of `0` lets the OS
    /// choose, and the chosen address comes back from `local_rcon_addr`.
    pub rcon: Option<crate::rcon::RconConfig>,
    /// Serve the GameSpy4/UT3 query protocol on the same port's UDP space
    /// (#332). On by default, matching what `bind` has always done.
    pub query: bool,
    /// Announce this world on the LAN discovery multicast group so it appears
    /// in a vanilla client's multiplayer list without being typed in (#535
    /// scope 3). Off by default — it is a broadcast, and a caller should opt in.
    pub discovery: Option<LanDiscovery>,
    /// The command dispatcher every accepted connection's `/`-commands reach
    /// (#48). `CommandDispatch::none()` by default, which **refuses** rather
    /// than permits.
    pub commands: crate::command::CommandDispatch,
    /// Server-initiated resource-pack pushes (#334).
    pub resource_packs: crate::server::ResourcePackPushFeed,
    /// The wire-level plugin-channel registry (#335).
    pub plugin_channels: crate::plugin_channels::PluginChannelRegistry,
    /// Ops, whitelist and the two ban lists this host enforces at join (#336).
    ///
    /// The `Default` is empty: nobody is banned, nobody is an operator and the
    /// whitelist is off — which is what `bind` has always done, so no existing
    /// caller changes behaviour. A host that wants real access control loads the
    /// four JSON files with `AccessHandle::load(world_dir)` and passes the result;
    /// the same handle is shared by every accepted connection, so an op granted on
    /// one is an op on the next.
    pub access: crate::access::AccessHandle,
}

/// How to announce a LAN world on vanilla's discovery multicast group.
///
/// Vanilla's `ServerStatusPinger`/`LanServerDetection` listens on UDP
/// `224.0.2.60:4445` for a `[MOTD]<name>[/MOTD][AD]<port>[/AD]` string and
/// re-broadcasts every 1.5 s (`LanServerPinger.PING_INTERVAL`). That literal
/// format is the whole protocol — there is no handshake and no reply.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct LanDiscovery {
    /// The world name shown in the multiplayer list's LAN section.
    pub motd: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl LanDiscovery {
    /// Vanilla's `LanServerPinger` group and port.
    pub const GROUP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(224, 0, 2, 60);
    /// See [`GROUP`](Self::GROUP).
    pub const PORT: u16 = 4445;
    /// `LanServerPinger.PING_INTERVAL`.
    pub const INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);

    /// The exact datagram body vanilla parses.
    #[must_use]
    pub fn payload(&self, port: u16) -> String {
        format!("[MOTD]{}[/MOTD][AD]{port}[/AD]", self.motd)
    }
}

impl IntegratedServer {
    /// Starts a single-client, in-memory integrated server (singleplayer) and
    /// returns the handle plus the **client** transport endpoint.
    ///
    /// Hand the returned [`DuplexStream`] to `lodestone-client`'s
    /// `ClientBuilder::connect_with`; the server half is served in a spawned
    /// task by the shared [`serve_connection`](crate::serve_connection) loop.
    ///
    /// Must be called from within a task-spawning context: an entered Tokio
    /// runtime natively (a shell under `#[tokio::main]` satisfies it), or the
    /// browser event loop on wasm (`spawn_local`).
    #[must_use]
    pub fn open_in_memory<P, S>(protocol: P, source: S, view_radius: i32) -> (Self, DuplexStream)
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        Self::open_in_memory_with_entities(protocol, source, NoEntities, view_radius)
    }

    /// Like [`open_in_memory`](Self::open_in_memory) but also streams entities:
    /// once the client reaches Play, each of its inbound packets drives a diff of
    /// `entities.snapshots()` against what this connection was last sent, emitting
    /// spawn / update / remove packets (see [`serve_connection`]). The `entities`
    /// source is a read-only view; the caller still owns the simulation and its
    /// tick, so a shared world can back both this and the sim loop.
    #[must_use]
    pub fn open_in_memory_with_entities<P, S, E>(
        protocol: P,
        source: S,
        entities: E,
        view_radius: i32,
    ) -> (Self, DuplexStream)
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
        E: EntitySource + 'static,
    {
        let (client_end, server_end) = memory_pair();
        let shutdown = ShutdownSignal::new();
        let signal = shutdown.clone();
        // Issue #293: shared rather than moved in by value, so chunk
        // generation can be handed to `spawn_blocking` instead of blocking
        // this runtime's core thread — see `crate::chunk::generate_columns_offloaded`
        // and `crate::server::SourceRef`. There is exactly one connection
        // here, so the `Arc` is not about sharing between tasks; it is
        // purely what makes the closure `'static`.
        //
        // Issue #289 / `docs/plans/chunk-lifecycle.md` U3: wrapped in a
        // [`ChunkStore`] so a column is generated **once** and thereafter read.
        // This constructor spawns no tick loop, so it does not suffer the
        // per-tick regeneration `open_in_memory_with_mobs` did — but it does
        // serve a connection, and `serve_connection`'s `vitals_tick` probes a
        // single block every 50 ms through `ChunkSource::block_state`, whose
        // *default* implementation regenerates a whole column to read one cell.
        // See `crate::chunk_store`'s module docs.
        //
        // Issue #505: sized from `view_radius`, not from a literal. This
        // constructor serves the whole `[-view_radius, view_radius]²` square at
        // join, and a capacity that does not cover it puts the columns the player
        // is looking at permanently in eviction range.
        //
        // `for_integrated_view_radius`, i.e. **uncapped**: this is singleplayer,
        // where the render distance is the player's own choice about their own
        // memory. `IntegratedServer::bind` below keeps the hosted ceiling. See
        // `chunk_store::integrated_capacity_for_view_radius` for the numbers and
        // for why a short capacity drops the *innermost* rings.
        let source = Arc::new(with_nether(
            ChunkStore::for_integrated_view_radius(source, view_radius),
            view_radius,
            true,
        ));
        // A fresh, empty registry for this one connection's lifetime. Nothing
        // ticks it here — only `open_in_memory_with_mobs` spawns the tick
        // loop (see that constructor's doc comment) — so a block entity
        // placed through this constructor exists and holds state, but never
        // advances on its own. Still real: `apply_use_item_on` can insert
        // into it and a later `CONTAINER_CLICK`/read could observe it.
        let block_entities = BlockEntityHandle::default();
        // A fresh, mobless handle for the same reason `block_entities` above
        // is fresh-and-empty: nothing ticks it here, only
        // `open_in_memory_with_mobs` seeds and ticks a real population. An
        // `Attack` packet against any id through this constructor simply
        // finds no mob (see `MobHandle::default`'s own doc comment).
        let mobs = MobHandle::default();

        let task = spawn(async move {
            let mut conn = Connection::new(server_end);
            // Serve the one connection, but never outlive an explicit shutdown:
            // whichever finishes first ends the task, and the connection (and
            // thus the client's read side) is dropped on the way out.
            tokio::select! {
                _ = signal.notified() => {}
                // Issue #545: `MAX_CLIENT_VIEW_RADIUS` as the live-change ceiling
                // — see the `open_in_memory_with_mobs_using` call site below for
                // the policy, and `crate::server::ViewTracker::max_radius` for
                // why the join radius could not serve as both.
                _ = serve_connection_shared(&mut conn, &protocol, &source, &entities, view_radius, crate::server::MAX_CLIENT_VIEW_RADIUS, &block_entities, &mobs) => {}
            }
        });

        (
            Self {
                #[cfg(not(target_arch = "wasm32"))]
                local_addr: None,
                shutdown,
                task,
                tick_task: None,
                clock: None,
                // No tick task, so nobody owns a server `World` (issue #433).
                server_tick: None,
                // Nothing seeds a mob population through this constructor (see
                // the `mobs` binding above), so there is nothing to seed.
                seed_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                save: None,
                #[cfg(not(target_arch = "wasm32"))]
                autosave_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                level_dat: None,
                #[cfg(not(target_arch = "wasm32"))]
                entity_storage: None,
                // Nothing persists here, so the save path has no population to read.
                #[cfg(not(target_arch = "wasm32"))]
                mobs: None,
                // No tick loop here, so there is nothing to share a store *with*.
                world_state: crate::world_state::WorldStateHandle::default(),
                // No RCON listener (issue #331) unless the caller starts one
                // explicitly with `start_rcon` — a listener needs a password
                // and a command dispatch, which these constructors do not take.
                #[cfg(not(target_arch = "wasm32"))]
                rcon_task: None,
                // No query listener (issue #332): it starts only on the TCP
                // `bind` path, which is the host-facing entry point.
                #[cfg(not(target_arch = "wasm32"))]
                query_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                discovery_task: None,
            },
            client_end,
        )
    }

    /// Like [`open_in_memory_with_entities`](Self::open_in_memory_with_entities),
    /// but the entity source is a real, live-ticked [`crate::MobSim`] (issue
    /// #217) rather than a caller-supplied [`EntitySource`]: this constructor
    /// also spawns the unified tick-loop task that owns the sim *and* every
    /// block entity (`tick::run_tick_loop`, issue #284 — see that module's own
    /// doc comment for why one loop now covers both), so dropping the
    /// returned handle stops *both* the connection task and the world-tick
    /// task, and shutdown waits on both. Also builds this server's
    /// [`TickClock`] (issue #285), readable through
    /// [`tick_stats`](Self::tick_stats).
    ///
    /// Mob pathing reads the same [`ChunkStore`] this constructor wraps `source`
    /// in, so a singleplayer world has exactly **one** terrain source.
    ///
    /// This used to take a second, independent `ChunkSource` for the mob world,
    /// on the argument that a deterministic generator produces identical terrain
    /// from two instances — true, and it cost a full second generation of the
    /// whole `mob_area` at world open, serially, before any task spawned. Issue
    /// #454 pointed seeding at the shared store; issue #436 removed the
    /// now-unread parameter.
    ///
    /// `mob_area` is the `(cx_range, cz_range)` of chunk columns loaded once
    /// into the sim's `ChunkWorld` snapshot — pick a range that covers
    /// `mob_center` with room to path around in; it does not grow later (see
    /// the scope note on `mobs::run_mob_tick_loop`). `mob_center` is the block
    /// `(x, z)` demo mobs are seeded around.
    ///
    /// **`mob_count` is a debug request, not an instruction.** It is routed through
    /// [`demo_mob_count`], which answers `0` unless [`DEMO_MOBS_ENV`] is set, so a
    /// world opened by a player has no demo population however large a number is
    /// passed here. Read that function before changing this.
    ///
    /// Native only, like [`bind`](Self::bind) — the tick loop's timer needs
    /// `tokio::time`, unavailable on `wasm32` (see `mobs::run_mob_tick_loop`'s
    /// doc comment).
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn open_in_memory_with_mobs<P, S>(
        protocol: P,
        source: S,
        mob_area: (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>),
        mob_center: (i32, i32),
        mob_count: usize,
        view_radius: i32,
    ) -> (Self, DuplexStream)
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        // A private registry, because an in-memory world has nothing to
        // persist into. A *persistent* world must not take this path — see
        // [`Self::open_in_memory_with_mobs_using`].
        Self::open_in_memory_with_mobs_using(
            protocol,
            source,
            mob_area,
            mob_center,
            mob_count,
            view_radius,
            BlockEntityHandle::default(),
            crate::region_source::ScheduledTickHandle::default(),
            // No world directory, so no `entities/` set to restore from.
            None,
        )
    }

    /// [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs) with the
    /// block-entity registry supplied by the caller.
    ///
    /// # Why this exists at all
    ///
    /// Because a registry the server creates privately is a registry the save
    /// path can never read, and that is the exact shape of the island issue
    /// #468 was: `chunk_nbt` wrote an empty `block_entities` list for every
    /// chunk and a saved container came back empty. The world's containers
    /// have to live in **one** registry that both the tick loop and
    /// [`crate::region_source::WorldSaveHandle`] can see, so
    /// [`Self::open_persistent_with_mobs`] takes it from the
    /// `RegionChunkSource` and hands it in here.
    ///
    /// Private on purpose: the choice is between "in-memory, private registry"
    /// and "persistent, the world's registry", and both public constructors
    /// already make it correctly. A third caller passing an unrelated handle
    /// would recreate the island.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_arguments)]
    fn open_in_memory_with_mobs_using<P, S>(
        protocol: P,
        source: S,
        mob_area: (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>),
        mob_center: (i32, i32),
        mob_count: usize,
        view_radius: i32,
        block_entities: BlockEntityHandle,
        // Issue #468. Threaded exactly as `block_entities` above is, and for the
        // same reason: the tick loop owns the queues at runtime, the persistence
        // path needs the same instance to save them, and only the caller knows
        // whether there is a world on disk to save to. In-memory passes a fresh
        // default; `open_persistent_with_mobs` passes the region source's own.
        scheduled: crate::region_source::ScheduledTickHandle,
        // Issue #303. `Some` only for `open_persistent_with_mobs`: the store the
        // seeding task restores this world's saved mobs and dropped items from,
        // once it has replaced the `Default` sim. Threaded here rather than
        // applied by the caller for the reason the restore site documents —
        // `MobHandle::reseed` discards the whole sim, so a restore that ran
        // before it would be silently undone.
        entities_on_disk: Option<crate::entity_storage::EntityStorage>,
    ) -> (Self, DuplexStream)
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        let (client_end, server_end) = memory_pair();
        let shutdown = ShutdownSignal::new();
        let live_mobs = LiveMobSource::default();
        // The local player's own roster, so the tab list is not empty: without
        // this, `entities.players()` (the bare `LiveMobSource` below has no
        // override) answers `None`, `stream_pass`'s tab-list branch never
        // runs, and no `player_info_update` ever reaches this connection —
        // not even one naming itself. Vanilla always lists you in your own
        // tab list; a real player registry, wrapped around `live_mobs` below
        // via `PlayerAwareSource`, is what makes `PlayerRegistry::join` (in
        // `crate::server`'s join sequence) actually register this connection
        // instead of being skipped. Same shape as the LAN-relay path's
        // `relay_players` further down this file.
        let player_registry = PlayerRegistry::new();
        // Issues #307/#308: shared with the tick task the same way
        // `block_entities` is, above — see [`BlockTickFeed`]'s own doc
        // comment for why this is safe with exactly one connection (this
        // constructor's own shape) and would need a per-connection cursor
        // for a multi-connection server.
        let block_tick_feed = BlockTickFeed::default();
        // Issue #425: shared with the tick task the same way `block_tick_feed`
        // is, above, and for the same reason — see [`ExplosionFeed`]'s own
        // doc comment for why this is safe with exactly one connection (this
        // constructor's own shape).
        let explosion_feed = ExplosionFeed::default();
        // Issue #325 / `docs/plans/world-state.md` S1: the night-skip vote and
        // its feed, shared between the connection task and the tick task the
        // same way the two feeds above are. The connection records `lay_down`/
        // `get_up` (bed click / wake-up) and feeds the voter count on its
        // `container_sync_tick`; the tick task's loop computes the vote and
        // publishes any `SkippedNight` back through the feed the connection
        // drains. One inner handle each, cloned twice — see [`SleepVote`]'s
        // own doc comment. A fresh vote and feed are the singleplayer shape;
        // with `player_registry` above carrying exactly the one local player
        // once they join, the voter count reaches 1 and
        // `SleepState::sleepers_needed`'s `max(1, …)` floor still demands
        // exactly one sleeper — the same outcome the pre-registry singleplayer
        // shape got from the floor alone, now reached by the real count too.
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        // Issue #12: the *handle* is still built synchronously here, before any
        // task spawns, so the exact same `MobSim` can be shared by the
        // connection task (which mutates it on an `Attack` packet, through
        // `crate::server::apply_attack`) and the tick-loop task (which ticks and
        // republishes it). See `MobHandle`'s own doc comment for why this is
        // `'static`-safe.
        let (cx_range, cz_range) = mob_area;
        // Issues #307/#308: the same small fixed region `mob_area` already
        // names, reused rather than adding a second range parameter — see
        // `tick::run_tick_loop`'s own doc comment for why this crate has no
        // general "loaded chunks" registry to draw a wider one from yet.
        let tick_area = (cx_range.clone(), cz_range.clone());
        let (center_x, center_z) = mob_center;

        // Issues #307/#308: `source` is now shared between the connection
        // task (which serves it over the wire — chunk generation, and every
        // player-driven `set_block`) and the tick task (which random-ticks
        // it) — the same object, not two independent instances, which is
        // exactly what makes a random tick's mutation visible to the client
        // this server actually serves rather than to an unwatched second
        // copy. **Since issue #454 mob pathing shares it too**, so this is now
        // the one and only terrain source a singleplayer world has; see the
        // seeding task below and this function's own doc comment.
        //
        // Issue #289 / `docs/plans/chunk-lifecycle.md` U3 — **this is the
        // singleplayer starvation fix.** [`ChunkStore`] makes a column
        // generated once and thereafter read, which matters here more than
        // anywhere because both tasks sharing this source were regenerating on
        // a 50 ms timer: `run_tick_loop` re-fetched every column of
        // `tick_area` every tick (49 columns at the shell's
        // `view_radius.clamp(1, 3)`), and the connection's `vitals_tick`
        // regenerated one column per 50 ms to read a single block. At the
        // 909 ms per composed column measured in release (see `chunk_store`), either one alone
        // exceeds the 50 ms tick budget by more than an order of magnitude.
        // See `crate::chunk_store`'s module docs for the full accounting.
        //
        // Note this is now built *before* anything mob-related, which is
        // load-bearing rather than cosmetic: the seeding task below reads its
        // terrain through this same store, so the 49 columns of `mob_area` are
        // generated **once** for the whole world instead of once here and once
        // more from a second, independent generator (issue #454).
        //
        // Issue #505: the capacity is derived from `view_radius`, not a literal, and
        // the derivation adds `CONCURRENT_SCAN_COLUMNS` on top of the view rather
        // than assuming the view covers it.
        //
        // That headroom used to be justified by the tick area being a *disjoint*
        // square (it was centred on world spawn and never moved). It follows the
        // players now — see `crate::tick_area` — so in the steady state it is a
        // subset of the view and the union has collapsed. The reserve stays because
        // the collapse is not instantaneous: the area moves the tick a movement
        // packet lands, before the new strip has finished streaming, and a teleport
        // or the playerless fallback puts it transiently outside the view again.
        //
        // `for_integrated_view_radius`, i.e. **uncapped**: this is the real
        // singleplayer world, the one whose render-distance slider the player owns.
        // See `chunk_store::integrated_capacity_for_view_radius`.
        let source = Arc::new(with_nether(
            ChunkStore::for_integrated_view_radius(source, view_radius),
            view_radius,
            true,
        ));

        // Issue #454: **mob seeding is off the critical path.**
        //
        // `MobHandle::seeded` used to run right here, synchronously, before any
        // task spawned — a serial `ChunkWorld::from_source` over the whole
        // `mob_area`. At the shell's `view_radius.clamp(1, 3)` that is 49
        // columns, and **measured in release at 10.86 s** inside the
        // `runtime.block_on` that opens a world, before the client could even
        // connect. Vanilla does not block world-open on mob population.
        //
        // Do **not** re-derive that figure from `chunk_store`'s 909 ms per
        // column: `49 × 909 ms ≈ 45 s` is what issue #454 predicted and it is
        // 4× too high. The 909 ms was measured across four *independently
        // constructed* sources precisely so the generator's 512-entry memo would
        // absorb nothing; seeding is the opposite case — one source, 49
        // *contiguous* columns — so the memo absorbs a great deal and the real
        // per-column cost here is about 222 ms. See
        // `docs/world-open-latency.md`; the post-fix constructor measures
        // 75.6 ms, and both figures are provisional (durations here spread 2.3×
        // on machine load alone).
        //
        // So the constructor hands back a `Default` handle — empty, mobless, and
        // already documented as safe to `Attack` against (see that impl) — and
        // this task fills it in. Two things make that cheap rather than merely
        // moved:
        //
        // * `generate_columns_offloaded` (issue #293/#414) fans the batch out
        //   over scoped threads **and** runs it on the blocking pool, so it
        //   neither serialises nor blocks the core thread the connection task
        //   and `run_tick_loop` share.
        // * it reads through the shared `source` store above, so every one of
        //   these columns is either already resident from the connection's
        //   initial view (`mob_area` is a subset of it in production) or becomes
        //   resident *for* that view. Either way each column is generated once.
        let seed_coords: Vec<(i32, i32)> = cz_range
            .clone()
            .flat_map(|cz| cx_range.clone().map(move |cx| (cx, cz)))
            .collect();
        let seed_source = Arc::clone(&source);
        let mob_handle = MobHandle::default();
        let seed_mobs = mob_handle.clone();
        // Issue #303. A third clone, for the handle this constructor returns, so
        // `open_persistent_with_mobs`'s autosave and `shutdown`'s flush can read
        // the population. `mob_handle` itself is moved into the tick task below.
        let handle_mobs = mob_handle.clone();
        // Issue #303: the entity area to restore, and where from. Cloned here
        // because the ranges are consumed by `seed_coords` above.
        let restore_area = (cx_range.clone(), cz_range.clone());
        let seed_task = spawn_tick_task(&shutdown, async move {
            let t_seed = web_time::Instant::now();
            tracing::info!(
                "mob seed task: generating {} columns for mob_area",
                seed_coords.len(),
            );
            let columns = generate_columns_offloaded(seed_source, seed_coords.clone()).await;
            let gen_ms = t_seed.elapsed().as_millis();
            // `generate_columns_offloaded` guarantees the result is aligned
            // index-for-index with the coordinates it was given, which is what
            // makes this zip correct rather than merely plausible — see its own
            // doc comment on why it returns a `Vec` and not a map.
            // `demo_mob_count(mob_count)`, not `mob_count`: singleplayer is a game,
            // not a demo harness. See that function.
            seed_mobs.reseed(
                ChunkWorld::from_columns(seed_coords.into_iter().zip(columns)),
                center_x,
                center_z,
                demo_mob_count(mob_count),
            );
            // Issue #303: **after** the reseed, never before. `MobHandle::reseed`
            // replaces the whole `MobSim` (see its own doc comment — "everything
            // is thrown away"), so restoring first would delete every saved mob
            // and leave a green tree with an empty world. This is also why the
            // restore lives in the seed task rather than in
            // `open_persistent_with_mobs`: that function returns before this task
            // has run.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(storage) = &entities_on_disk {
                let (cx_range, cz_range) = restore_area;
                match storage.load_area(cx_range, cz_range) {
                    Ok(saved) if !saved.is_empty() => {
                        let restored = seed_mobs.with(|sim| sim.restore_saved(&saved));
                        tracing::info!(
                            "entity load: restored {restored} of {} saved entities",
                            saved.len(),
                        );
                    }
                    Ok(_) => {}
                    // Logged rather than propagated: this task has no error
                    // channel, and a world whose mobs cannot be read is still a
                    // world worth playing. The load is *not* silent, which is the
                    // property that matters — a blank `entities/` read as "no
                    // mobs here" is exactly the failure #303 exists to stop.
                    Err(err) => tracing::error!("entity load failed, mobs not restored: {err}"),
                }
            }
            // Read the clock **once**: the previous form called `elapsed()` twice, so
            // the logged parts did not sum to the logged total. `saturating_sub` for
            // the same reason as `server.rs`'s welcome timing — `as_millis()` is
            // `u128`, and a sub-millisecond phase makes a plain subtraction underflow
            // and panic in debug while wrapping silently in release.
            let seed_ms = t_seed.elapsed().as_millis();
            tracing::info!(
                "mob seed task done: {}ms (gen={}ms, reseed={}ms)",
                seed_ms,
                gen_ms,
                seed_ms.saturating_sub(gen_ms),
            );
        });

        let conn_signal = shutdown.clone();
        // `PlayerAwareSource` rather than a bare `live_mobs.clone()`: see
        // `player_registry`'s own doc comment above for why the tab list
        // needs this. `snapshots()` still comes from `live_mobs` alone —
        // `PlayerAwareSource` never folds the registry into it, so the
        // connection's own player entity remains excluded from its own
        // entity diff, exactly as `crate::players::PlayerRegistry::view`'s
        // `viewer` parameter already ensures for the roster/tab-list side.
        let conn_entities = PlayerAwareSource::new(live_mobs.clone(), player_registry.clone());
        let conn_block_entities = block_entities.clone();
        let conn_mobs = mob_handle.clone();
        let conn_source = Arc::clone(&source);
        let conn_block_ticks = block_tick_feed.clone();
        let conn_explosions = explosion_feed.clone();
        // Issue #325: cloned out here rather than inside the `async move`
        // below, for the same reason `clock` is — an `Arc::clone` *inside* the
        // block would move the original out of reach of the tick task, which
        // passes the same inner handle to `run_tick_loop_with_weather`.
        let conn_sleep_vote = sleep_vote.clone();
        let conn_sleep_feed = sleep_feed.clone();
        // Issues #327/#328/#323. **One** world state, cloned out here for the same
        // reason the sleep vote is: a clone made inside the `async move` below would
        // move the original out of reach of the tick task, and two stores is the bug
        // — a rule set on the connection has to be the rule the loop reads, and the
        // clock the loop advances has to be the clock the connection broadcasts.
        let world_state = crate::world_state::WorldStateHandle::new();
        let conn_world_state = world_state.clone();
        // A third clone for the returned handle, so a caller (the persistence path,
        // a gate) reads and stamps the *same* store the loop advances.
        let world_state_for_handle = world_state.clone();
        let task = spawn(async move {
            let mut conn = Connection::new(server_end);
            tokio::select! {
                _ = conn_signal.notified() => {}
                // Issue #293: the `_shared` variant, so this task's chunk
                // generation runs on the blocking pool rather than on the
                // one core thread it shares with `run_tick_loop` below.
                // `&conn_source` rather than `&*conn_source` is the entire
                // call-site change — see `crate::server::SourceRef`.
                _ = serve_connection_with_mob_events_shared(
                    &mut conn,
                    &protocol,
                    &conn_source,
                    &conn_entities,
                    view_radius,
                    // Issue #545: singleplayer's live-change ceiling is the
                    // slider's own maximum, not the radius this connection
                    // joined with — raising render distance mid-session used to
                    // be silently clamped back. Uncapped for the same reason
                    // `for_integrated_view_radius` above is: it is the memory of
                    // the person who moved the slider. See
                    // `crate::server::MAX_CLIENT_VIEW_RADIUS`.
                    crate::server::MAX_CLIENT_VIEW_RADIUS,
                    &conn_block_entities,
                    &conn_mobs,
                    &conn_block_ticks,
                    &conn_explosions,
                    &conn_sleep_vote,
                    &conn_sleep_feed,
                    &conn_world_state,
                ) => {}
            }
        });

        let clock = Arc::new(TickClock::new());
        // Issue #433 Phase 0: build this server's own `bevy_ecs::World` here,
        // synchronously, before the tick task spawns — the same reason
        // `mob_handle` above is built here rather than inside the future, and
        // it is also what makes the Phase 0 gate deterministic (no polling: by
        // the time this constructor returns, `ServerBoot` has already run).
        //
        // `into_world()` rather than keeping the `App`: `bevy_app::App` is
        // **not** `Send` (its `runner` field is a `Box<dyn FnOnce(App) ->
        // AppExit>` with no `Send` bound), so it cannot cross `spawn`. `World`
        // is, and it carries the `Schedules` resource with it. See
        // `crate::ecs`'s module doc — Phase 1 threads `&mut World` into
        // `run_tick_loop`, not `&mut App`.
        let server_app = crate::ecs::ServerApp::bootstrap();
        let server_tick = server_app.witness();
        let server_world = server_app.into_world();
        // Cloned out here rather than inside the `async move` below: an
        // `Arc::clone(&x)` *inside* the block moves `x` into the coroutine, so
        // `clock` would no longer be available for the `Self` literal further
        // down. Before this change the calls were argument expressions,
        // evaluated eagerly, and the distinction did not arise.
        let tick_clock = Arc::clone(&clock);
        let tick_source = Arc::clone(&source);
        // **The world tick follows the player from here on.** `tick_area` above is
        // now only the fallback the loop uses while no player has reported a
        // position; the anchor set rides `world_state`, which the connection task
        // already holds, so the two ends share one store without a new parameter on
        // the `serve_connection*` chain. See `crate::tick_area`.
        //
        // The dimension is this source's own: `IntegratedServer` opens one tick loop
        // over one `ChunkSource`, so an anchor published while the player is in the
        // Nether names a dimension this loop does not serve and is correctly
        // ignored — the overworld simply stops ticking, which is vanilla's "no
        // player tickets, no ticking".
        let follow = crate::tick_area::TickFollow {
            // `DimensionalSource::dimension` — its own inherent accessor, which
            // returns the dimension it serves rather than the trait's `Option`.
            dimension: source.dimension(),
            radius: crate::chunk_store::CONCURRENT_TICK_RADIUS,
            anchors: world_state.tick_anchors().clone(),
        };
        let tick_task = spawn_tick_task(&shutdown, async move {
            // Owned by the tick task, with no lock, per `docs/server-ecs.md`.
            // Phase 1 replaces this binding with a `&mut` argument to
            // `run_tick_loop` and runs `GameTick` once per iteration.
            let _server_world = server_world;
            // Issue #325: the `_with_weather` variant so the real sleep vote
            // and feed reach the loop (the plain `run_tick_loop` wrapper only
            // forwards a fresh, disconnected vote — that is the loop `bind`'s
            // LAN worlds run on, which is why they do not skip the night yet).
            // Weather itself is not wired here (issue #324's own change), so a
            // default feed and state are passed — exactly what the wrapper
            // would have passed, which is why switching variants is
            // observably a no-op for the sky.
            run_tick_loop_with_weather(
                mob_handle,
                live_mobs,
                block_entities,
                tick_clock,
                tick_source,
                block_tick_feed,
                tick_area,
                explosion_feed,
                WeatherFeed::default(),
                WeatherState::default(),
                &sleep_vote,
                &sleep_feed,
                scheduled,
                world_state,
                follow,
            )
            .await;
        });

        (
            Self {
                #[cfg(not(target_arch = "wasm32"))]
                local_addr: None,
                shutdown,
                task,
                tick_task: Some(tick_task),
                clock: Some(clock),
                server_tick: Some(server_tick),
                seed_task: Some(seed_task),
                #[cfg(not(target_arch = "wasm32"))]
                save: None,
                #[cfg(not(target_arch = "wasm32"))]
                autosave_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                level_dat: None,
                // Set by `open_persistent_with_mobs` after this returns; an
                // in-memory world has no `entities/` directory to write into.
                #[cfg(not(target_arch = "wasm32"))]
                entity_storage: None,
                #[cfg(not(target_arch = "wasm32"))]
                mobs: Some(handle_mobs),
                world_state: world_state_for_handle,
                #[cfg(not(target_arch = "wasm32"))]
                rcon_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                query_task: None,
                #[cfg(not(target_arch = "wasm32"))]
                discovery_task: None,
            },
            client_end,
        )
    }

    /// The same singleplayer world as
    /// [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs), but
    /// **persistent**: columns are loaded from `world_dir`'s Anvil region files
    /// when they exist, every mutation is retained, and the world is written
    /// back on [`shutdown`](Self::shutdown) and on an autosave timer (issue
    /// #437).
    ///
    /// # How it composes
    ///
    /// This wraps `source` in a [`crate::region_source::RegionChunkSource`] and
    /// hands *that* to the ordinary constructor, which wraps it in
    /// [`ChunkStore`] as usual. The resulting stack is
    /// `ChunkStore → RegionChunkSource → source`, which is the only ordering
    /// that works — see `region_source`'s module doc for why persistence has to
    /// sit below the cache and above the generator, and in particular why it
    /// must not forward `set_block` down.
    ///
    /// Because the wrap happens here, **every** existing mutation path is
    /// carried without touching it: player edits, random ticks, and the mob
    /// sim's grazing all reach the world through `ChunkSource::set_block`, and
    /// that is the choke point being hooked. Nothing in `tick.rs`, `mobs.rs` or
    /// `server.rs` changes, which also means `MobSim`'s immutable
    /// `world: &'w ChunkWorld` borrow is untouched.
    ///
    /// # The autosave does not run on the tick thread
    ///
    /// The spawned autosave task does its filesystem work inside
    /// `spawn_blocking`, so a full-region write never lands on the core thread
    /// the connection task and `run_tick_loop` share. That is deliberate: the
    /// world-open stall (10.86 s → 75.6 ms, `docs/world-open-latency.md`) was
    /// the last large performance defect in this crate and had exactly this
    /// shape. The only work on the mutation path itself is a `HashSet` insert.
    ///
    /// # Errors
    ///
    /// Returns [`crate::region_source::Error`] if `world_dir`'s region
    /// directory cannot be created. Reading is deliberately *not* fallible —
    /// a missing region file is a world that has never been saved, which is
    /// every world's first open.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_arguments)]
    pub fn open_persistent_with_mobs<P, S>(
        protocol: P,
        world_dir: &std::path::Path,
        source: S,
        min_y: i32,
        height: i32,
        mob_area: (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>),
        mob_center: (i32, i32),
        mob_count: usize,
        view_radius: i32,
        autosave: std::time::Duration,
    ) -> Result<
        (
            Self,
            DuplexStream,
            crate::region_source::RegionChunkSource<S>,
        ),
        crate::region_source::Error,
    >
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        let persistent =
            crate::region_source::RegionChunkSource::new(source, world_dir, min_y, height)?;
        let save = persistent.save_handle();
        // Read out before `persistent` is moved into the constructor below.
        let persistent_scheduled = persistent.scheduled_ticks();
        // Before any task spawns, and before the first chunk is written: a
        // world directory that has region files but no `level.dat` is not a
        // world any other tool — vanilla included — will open. Creating it
        // here also means a world that is opened and immediately closed still
        // leaves something loadable behind.
        //
        // Spawn defaults to the mob centre at y=64 rather than taking another
        // parameter: that coordinate is already this constructor's notion of
        // where the world is centred, and a caller that wants a different one
        // can rewrite the field through `lodestone_anvil::level_dat`.
        let spawn = lodestone_anvil::level_dat::Spawn {
            pos: [mob_center.0, 64, mob_center.1],
            ..lodestone_anvil::level_dat::Spawn::default()
        };
        let level_dat = std::sync::Arc::new(crate::region_source::LevelDatHandle::open_or_create(
            world_dir, &spawn, 0,
        )?);
        // A second handle to the *same* world, returned to the caller. This is
        // what anything outside the connection loop (a `/setblock`, a gate)
        // mutates through, and it is the identical object the `ChunkStore`
        // below wraps — not a second copy, which is the mistake issue #454
        // caught in the mob-pathing source.
        let world = persistent.clone();
        // **The world's own registry, not a fresh one.** This is the join that
        // makes block entities persist at all: the tick loop advances the
        // containers in this registry and `WorldSaveHandle::save` reads the
        // same one. Passing `BlockEntityHandle::default()` here compiles, ticks
        // correctly, and writes an empty `block_entities` list forever — the
        // island #468 names.
        let block_entities = persistent.block_entities();
        // Issue #303: the `entities/` region set, created eagerly next to
        // `region/` so a later entity save cannot fail for a reason the caller
        // could have been told about here.
        let entity_storage = crate::entity_storage::EntityStorage::new(world_dir)?;
        let (mut server, client_end) = Self::open_in_memory_with_mobs_using(
            protocol,
            persistent,
            mob_area,
            mob_center,
            mob_count,
            view_radius,
            block_entities,
            // Issue #468's last wire: the same handle the save path reads, so a
            // pending repeater tick survives a quit.
            persistent_scheduled,
            // Issue #303: the same store the autosave below writes through, so a
            // restored cow is one the next save recognises as its own (see
            // `EntityStorage::save`'s uuid-identity clearing).
            Some(entity_storage.clone()),
        );

        let autosave_handle = save.clone();
        let autosave_level_dat = std::sync::Arc::clone(&level_dat);
        // Issues #327/#328/#323: the world's scalars, loaded off disk **before**
        // anything can change them and stamped on every autosave.
        //
        // Load races the connection's own join by construction (the connection task
        // is spawned inside the constructor above), and that is tolerable rather than
        // ignored: the join's `encode_set_time` may carry a zero clock for one
        // second, and the periodic broadcast corrects it on its next tick. Moving the
        // load before the constructor needs the store built outside it, which is the
        // follow-up #300 wants anyway.
        let autosave_world_state = server.world_state.clone();
        if let Some(data) = level_dat.data() {
            autosave_world_state.load_level_data(&data);
        }
        // The one thing that must *not* survive the load on a brand-new world:
        // `LevelDat::for_new_world` had to write *some* `spawn` compound and had no
        // terrain to consult, so it wrote a placeholder at the mob centre. Loading
        // that back would look like a resolved world spawn and suppress the spiral
        // search forever — the player would spawn at the placeholder even if it is
        // ocean. Clearing it makes the first join do the search, and its answer is
        // what the next autosave persists.
        if level_dat.created() {
            autosave_world_state.clear_world_spawn();
        }
        // Cloned before the `Self` literal for the same reason `tick_clock` is
        // in the constructor above: an `Arc::clone` inside the `async move`
        // would move the binding into the coroutine.
        let autosave_clock = server.clock.clone();
        // Issue #303: the two halves of an entity save — where to write, and what
        // population to read. Cloned out here for the same reason `autosave_clock`
        // is: a clone made inside the `async move` would move the binding.
        let autosave_entities = entity_storage.clone();
        let autosave_mobs = server.mobs.clone();
        let autosave_task = spawn_tick_task(&server.shutdown, async move {
            let mut ticker = tokio::time::interval(autosave);
            // The first tick of a tokio interval completes immediately; a save
            // at t=0 has nothing to write and would only burn a blocking-pool
            // slot during world open, the exact window issue #454 cleared.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let handle = autosave_handle.clone();
                // The whole point: the write happens on the blocking pool.
                let result = tokio::task::spawn_blocking(move || handle.save()).await;
                if let Ok(Err(err)) = result {
                    tracing::warn!("autosave failed, chunks stay dirty for the next attempt: {err}");
                }
                // `level.dat` rides the same blocking pool and the same
                // interval. It is a few hundred bytes, so unlike a region
                // write it is not the reason this is off-thread — it is here
                // because a `Time` that only advanced at shutdown would be
                // lost by a crash, which is precisely when a world's age
                // matters.
                let ticks = autosave_clock
                    .as_ref()
                    .map_or(0, |clock| clock.tick_count());
                let level = std::sync::Arc::clone(&autosave_level_dat);
                // Issues #327/#328/#323: the rules, difficulty and day clock ride the
                // same write. Snapshotted here rather than inside the closure because
                // the closure crosses `spawn_blocking`.
                let scalars = autosave_world_state.level_data_fields();
                let result = tokio::task::spawn_blocking(move || level.write(ticks, &scalars)).await;
                if let Ok(Err(err)) = result {
                    tracing::warn!("autosave could not stamp level.dat: {err}");
                }
                // Issue #303: the mobs and dropped items, on the same interval and
                // the same blocking pool as the terrain.
                //
                // **Snapshotted on this thread, written on the pool.** The sim
                // lives behind `MobHandle`'s mutex, which the tick loop takes
                // every tick; holding it across a region write would stall the
                // world for the length of a filesystem operation. `saved_entities`
                // is a `Vec` build under the lock and nothing else — no I/O, no
                // compression.
                if let Some(mobs) = &autosave_mobs {
                    let saved = mobs.with(|sim| sim.saved_entities());
                    let storage = autosave_entities.clone();
                    let result = tokio::task::spawn_blocking(move || storage.save(&saved)).await;
                    if let Ok(Err(err)) = result {
                        tracing::warn!("autosave could not write entities: {err}");
                    }
                }
            }
        });
        server.save = Some(save);
        server.level_dat = Some(level_dat);
        server.entity_storage = Some(entity_storage);
        // Replaces the mob-seeding task slot only if it is free; seeding owns
        // it for `open_in_memory_with_mobs`, so the autosave task is kept
        // alive by racing the same `shutdown` notify and is dropped with the
        // handle.
        server.autosave_task = Some(autosave_task);
        Ok((server, client_end, world))
    }

    /// The world's shared game rules, difficulty and clock (issues #327/#328/#323).
    ///
    /// The **same** store the tick loop advances and every connection reads, so a
    /// host can set a rule or read the day time without a packet round trip. A
    /// constructor with no tick loop returns a private default — there is nothing
    /// to share one with.
    #[must_use]
    pub fn world_state(&self) -> &crate::world_state::WorldStateHandle {
        &self.world_state
    }

    /// The live mob simulation, for a host that needs to read or seed the
    /// population from outside the tick loop (issue #303).
    ///
    /// The **same** handle the tick loop advances, every connection attacks
    /// against, and the entity save reads — not a copy, on the same argument
    /// [`world_state`](Self::world_state) makes. `None` for a constructor that
    /// starts no tick loop, where there is nothing to share.
    ///
    /// # Racing world open
    ///
    /// The mob-seeding task ([`crate::MobHandle::reseed`]) **replaces** the whole
    /// sim once the terrain it needs has been generated off-thread, so anything
    /// inserted through this handle before that point is discarded. Poll
    /// [`crate::MobSim::next_id`] — `>= 1000` once the reseed has run — before
    /// seeding through it.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn mobs(&self) -> Option<&MobHandle> {
        self.mobs.as_ref()
    }

    /// Writes every dirty chunk now, on the calling thread, returning how many
    /// columns were written.
    ///
    /// `Ok(0)` when nothing has changed since the last save — and `Ok(0)` is
    /// also what a non-persistent server returns, so a caller cannot tell those
    /// apart; use [`dirty_chunk_count`](Self::dirty_chunk_count) if the
    /// distinction matters.
    ///
    /// # Errors
    ///
    /// Returns [`crate::region_source::Error`] if a region file could not be
    /// written. The affected chunks stay dirty, so the next save retries them.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_now(&self) -> Result<usize, crate::region_source::Error> {
        // `level.dat` first, and its failure is *not* allowed to swallow the
        // chunk save: metadata is worth less than blocks, so a metadata error
        // is logged and the region write still happens. The reverse ordering
        // would let a failed chunk save skip the stamp for no benefit.
        if let Some(level) = &self.level_dat {
            let ticks = self.clock.as_ref().map_or(0, |clock| clock.tick_count());
            if let Err(err) = level.write(ticks, &self.world_state.level_data_fields()) {
                tracing::warn!("could not stamp level.dat: {err}");
            }
        }
        match &self.save {
            Some(handle) => handle.save(),
            None => Ok(0),
        }
    }

    /// The world's `level.dat` handle, or `None` for a non-persistent server.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn level_dat(&self) -> Option<&crate::region_source::LevelDatHandle> {
        self.level_dat.as_deref()
    }

    /// How many chunk columns are waiting to be written, or `None` for a
    /// non-persistent server.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn dirty_chunk_count(&self) -> Option<usize> {
        self.save.as_ref().map(super::region_source::WorldSaveHandle::dirty_count)
    }

    /// This world's persistence counters, or `None` for a non-persistent
    /// server. Counts, not timings — see
    /// [`crate::region_source::PersistenceStats`].
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn persistence_stats(&self) -> Option<&crate::region_source::PersistenceStats> {
        self.save.as_ref().map(super::region_source::WorldSaveHandle::stats)
    }

    /// Binds a TCP listener and serves every accepted connection with the same
    /// loop (open-to-LAN). Native targets only.
    ///
    /// The listener is bound before returning, so [`local_addr`] is immediately
    /// available (bind to port `0` to get an OS-assigned port for tests).
    ///
    /// # Errors
    ///
    /// Returns the [`std::io::Error`] from binding the listener.
    ///
    /// [`local_addr`]: IntegratedServer::local_addr
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn bind<A, P, S>(
        addr: A,
        protocol: P,
        source: S,
        view_radius: i32,
    ) -> std::io::Result<Self>
    where
        A: tokio::net::ToSocketAddrs,
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        Self::open_to_lan(
            addr,
            protocol,
            source,
            LanConfig {
                view_radius,
                // `bind`'s pre-#535 behaviour, verbatim: query on, nothing else.
                query: true,
                ..LanConfig::default()
            },
        )
        .await
    }

    /// [`bind`](Self::bind) with everything an open-to-LAN host can configure
    /// (issue #535) — RCON, the query listener, LAN discovery, commands,
    /// resource-pack pushes and plugin channels. See [`LanConfig`].
    ///
    /// This is the entry point a "Open to LAN" menu item calls. It is one call:
    /// build a [`LanConfig`], hand it the same protocol and source singleplayer
    /// already uses, and hold the returned handle for the session.
    ///
    /// # Errors
    ///
    /// The [`std::io::Error`] from binding the TCP listener, or from binding the
    /// RCON listener when [`LanConfig::rcon`] is set. A failed **query** or
    /// **discovery** bind is deliberately non-fatal and logged instead: neither
    /// is needed to play, and taking the whole world down for a busy UDP port
    /// would be the wrong trade.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn open_to_lan<A, P, S>(
        addr: A,
        protocol: P,
        source: S,
        config: LanConfig,
    ) -> std::io::Result<Self>
    where
        A: tokio::net::ToSocketAddrs,
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
    {
        let LanConfig {
            view_radius,
            rcon,
            query,
            discovery,
            commands,
            resource_packs,
            plugin_channels,
            access,
        } = config;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr().ok();

        let protocol = Arc::new(protocol);
        // Issue #289 / `docs/plans/chunk-lifecycle.md` U3, for the same two
        // reasons as `open_in_memory_with_mobs` above: one `run_tick_loop`
        // re-fetching every column of its tick area every tick, plus one
        // `vitals_tick` per connection regenerating a column per 50 ms to read
        // a single block. LAN's tick area is smaller (`LAN_TICK_RADIUS`, 25
        // columns) but the per-column cost is the same, and the store is
        // shared across every accepted connection exactly as `source` already
        // was. See `crate::chunk_store`'s module docs.
        //
        // Issue #505: sized from `view_radius` like the two in-memory constructors
        // above. The store is shared across every accepted connection, and
        // `view_radius` is this server's configured cap — `dispatch_play_packet`
        // clamps each client's requested distance to it — so one derivation from
        // the cap covers every connection's worst case rather than the first
        // one's.
        //
        // `for_view_radius`, i.e. **capped at `MAX_CAPACITY`** — the one
        // constructor that keeps the ceiling, and deliberately not the same policy
        // as the two in-memory ones. This is a *host*: the memory is spent on
        // behalf of every accepted connection, none of whom chose the setting.
        // `chunk_store::integrated_capacity_for_view_radius` carries the argument
        // and the price list for the other side of the fork.
        let source = Arc::new(with_nether(
            ChunkStore::for_view_radius(source, view_radius),
            view_radius,
            false,
        ));
        let shutdown = ShutdownSignal::new();
        let signal = shutdown.clone();
        // Shared across every accepted connection (like `protocol`/`source`
        // above) rather than one per connection, so two LAN players placing
        // and interacting with the same furnace see the same state — and,
        // since issue #439, shared with the **one** world tick loop spawned
        // below, so that furnace actually advances. Same reasoning for
        // `mobs`: no live population over LAN via this constructor (nothing
        // seeds one), but an `Attack` packet against it is still safe (see
        // `MobHandle::default`'s own doc comment) rather than a
        // special-cased no-op path, and the tick loop ticks whatever is
        // there.
        //
        // **Taken from the source when the source has a world on disk.** A
        // `default()` here compiles, ticks correctly and loses every chest a LAN
        // guest fills, because the save path reads the *source's* registry — the
        // same island #468 closed for singleplayer, which survived here only
        // because this constructor is generic over `S` and could not name
        // `RegionChunkSource::block_entities`. `ChunkSource::world_registries`
        // is that name; `ChunkStore` forwards it, so the wrap above is
        // transparent.
        let registries = source.world_registries();
        let block_entities = registries
            .as_ref()
            .map_or_else(BlockEntityHandle::default, |r| r.block_entities.clone());
        let mobs = MobHandle::default();

        // Issue #439: LAN worlds had **no world tick at all**. `run_tick_loop`
        // had exactly one caller (`open_in_memory_with_mobs`), so over LAN
        // block entities held state but never advanced, scheduled and fluid
        // ticks never drained, random ticks never fired, mobs never ticked
        // and `game_tick` never incremented.
        //
        // # Exactly one loop per world, and why it is spawned *here*
        //
        // The correctness question is not "does a tick loop exist" but "how
        // many". A world advancing at N× speed with N players reads as a
        // physics bug for a long time before anyone suspects the loop count,
        // and `docs/server-ecs.md`'s never-straddle rule is precisely about
        // this: a *world* concern must not live on a *connection*.
        //
        // So the loop is spawned once, **outside** the accept loop, over the
        // same `Arc<S>` source and the same `block_entities`/`mobs` handles
        // every connection shares. Putting it inside the accept arm would
        // give one loop per player; that is structurally impossible here
        // because the accept arm has no access to `clock`, and the single
        // `tick_task: Option<Task>` field below can hold exactly one.
        //
        // Note the loop is per `IntegratedServer`, **not** global: `bind` and
        // `open_in_memory_with_mobs` construct different worlds over
        // different sources, so "both entry points share one loop" would be
        // wrong. One world, one loop.
        let live_mobs = LiveMobSource::default();
        // The tick loop publishes random-tick block changes and detonations
        // into these. See the relay arm in the accept loop below for why LAN
        // needs a fan-out where singleplayer does not.
        let hub_block_ticks = BlockTickFeed::default();
        let hub_explosions = ExplosionFeed::default();
        let clock = Arc::new(TickClock::new());
        // Issue #433 Phase 0, same as `open_in_memory_with_mobs` above — and for
        // the reason that constructor's comment gives: one world, one loop, one
        // `World`. #439 gave LAN its own tick loop, so LAN gets its own server
        // `World` too rather than sharing singleplayer's, which would be exactly
        // the "both entry points share one loop" mistake the comment above this
        // block already rules out.
        let server_app = crate::ecs::ServerApp::bootstrap();
        let server_tick = server_app.witness();
        let server_world = server_app.into_world();
        // Every one of these is cloned out *here* rather than inside the
        // `async move` below: a `.clone()` inside the block moves the original
        // into the coroutine, and all six are still needed afterwards (`clock`
        // for the `Self` literal, the other five for the relay arm). Before this
        // change they were argument expressions, evaluated eagerly, and the
        // distinction did not arise.
        let tick_clock = Arc::clone(&clock);
        let tick_source = Arc::clone(&source);
        let tick_mobs = mobs.clone();
        let tick_live_mobs = live_mobs.clone();
        let tick_block_entities = block_entities.clone();
        let tick_block_ticks = hub_block_ticks.clone();
        let tick_explosions = hub_explosions.clone();
        let tick_scheduled = registries
            .as_ref()
            .map_or_else(Default::default, |r| r.scheduled.clone());
        // Issues #327/#328/#323: one store for the LAN world, so a rule a LAN
        // player sets is the rule the tick loop reads and the clock the loop
        // advances is the clock every connection broadcasts. `bind` used to give
        // each accepted socket its own `WorldAdminState` local — two LAN players
        // each had a private, divergent view, which is what #327 reported.
        let lan_world_state = crate::world_state::WorldStateHandle::new();
        let tick_world_state = lan_world_state.clone();
        let handle_world_state = lan_world_state.clone();
        // Built out here rather than inline in the call below for the reason every
        // other `*_clone` in this function is: a `.clone()` written inside the
        // `async move` would move `lan_world_state` into the coroutine.
        let lan_follow = crate::tick_area::TickFollow {
            dimension: crate::dimension::Dimension::Overworld,
            radius: LAN_TICK_RADIUS,
            anchors: lan_world_state.tick_anchors().clone(),
        };
        let tick_task = spawn_tick_task(&shutdown, async move {
            // Owned by the tick task, with no lock, per `docs/server-ecs.md`.
            let _server_world = server_world;
            run_tick_loop_with_weather(
                tick_mobs,
                tick_live_mobs,
                tick_block_entities,
                tick_clock,
                tick_source,
                tick_block_ticks,
                // The same small fixed region `open_in_memory_with_mobs`
                // threads through as `mob_area`, centred on (0, 0), because
                // this crate still has no "loaded chunks" registry to derive
                // a real one from — see `tick::run_tick_loop`'s own doc
                // comment and `docs/plans/chunk-lifecycle.md` (#289), which
                // is what replaces this constant with a ticket-driven set.
                // `bind`'s public signature deliberately does not grow a
                // parameter for it.
                (
                    -LAN_TICK_RADIUS..=LAN_TICK_RADIUS,
                    -LAN_TICK_RADIUS..=LAN_TICK_RADIUS,
                ),
                tick_explosions,
                // Weather is not wired on the LAN path either — a default feed and
                // state, exactly what the `run_tick_loop` wrapper passed before this
                // switched variants, so the sky is observably unchanged. The variant
                // switch buys the world-state parameter and nothing else.
                WeatherFeed::default(),
                WeatherState::default(),
                // Issue #325: LAN stays sleep-free, matching the wrapper.
                &crate::sleep::SleepVote::new(),
                &crate::sleep::SleepFeed::default(),
                // Issue #468: the source's own queues when it has a world on
                // disk, so a repeater delay or a fluid tick set while hosting
                // survives a restart; a fresh handle only for a truly in-memory
                // source, where there is nothing to persist into.
                tick_scheduled,
                tick_world_state,
                // The LAN path follows its players too, through the same shared
                // `WorldStateHandle` above — every accepted socket's packet dispatch
                // publishes into it. The single-anchor caveat bites here rather than
                // in singleplayer: `TickAnchors::publish` replaces the whole set, so
                // with two LAN players the tick area follows whichever moved most
                // recently instead of the union of both. That is strictly better than
                // the fixed origin box it replaces, and the fix is per-connection
                // anchor bookkeeping, not more geometry — `FollowArea` already unions.
                lan_follow,
            )
            .await;
        });

        let relay_block_ticks = hub_block_ticks.clone();
        let relay_explosions = hub_explosions.clone();
        let relay_mobs = live_mobs.clone();
        // Issue #535's config surface, cloned out here for the same reason the
        // six above are: the accept arm lives inside an `async move`, so a
        // `.clone()` written there would move the original in.
        let conn_commands = commands;
        let conn_resource_packs = resource_packs;
        let conn_plugin_channels = plugin_channels;
        // Issue #336: moved into the accept loop like the three above it, and
        // cloned per socket below.
        let conn_access = access;
        // Issue #438: **one** registry for every connection this listener
        // accepts, created out here for the same reason the tick loop above is
        // spawned out here. A registry per connection would make each player
        // the sole inhabitant of their own world — the bug this fixes, wearing
        // a different hat.
        let relay_players = PlayerRegistry::new();
        // Issue #332: the GameSpy4/UT3 query listener, on the same address as
        // the game TCP socket — vanilla's own default (query port = server
        // port), and free because UDP and TCP port spaces are independent. It
        // reads the *same* shared `relay_players` every connection uses, so its
        // online count and player list are real, unlike a status reply, which
        // must report `0` (see `serve_connection`'s comment on why). The run
        // loop races the `shutdown` notify through `spawn_tick_task`, so it
        // ends (and releases the UDP port) when the server shuts down.
        //
        // Deliberately non-fatal: if the UDP bind fails (some other process
        // holds the port's UDP space) the game still serves; the query side
        // just does not come up, and the failure is logged once rather than
        // taking the whole `bind` down with it.
        let query_task = match local_addr.filter(|_| query) {
            Some(query_addr) => {
                let query_config = crate::query::QueryConfig {
                    host_ip: query_addr.ip().to_string(),
                    host_port: query_addr.port(),
                    ..crate::query::QueryConfig::default()
                };
                match crate::query::QueryServer::bind(
                    query_addr,
                    query_config,
                    relay_players.clone(),
                )
                .await
                {
                    Ok(query) => Some(spawn_tick_task(&shutdown, query.run())),
                    Err(err) => {
                        tracing::warn!("query listener disabled (UDP {query_addr}): {err}");
                        None
                    }
                }
            }
            None => None,
        };
        let task = spawn(async move {
            // Issue #439's fan-out. `BlockTickFeed`/`ExplosionFeed` are
            // append-and-**drain-all**: the first consumer takes everything
            // and a second sees nothing (their own doc comments say so, and
            // say the fix is a per-connection cursor). Handing the same feed
            // to every LAN connection would therefore desync every player but
            // one; handing the tick loop a feed nobody drains would grow
            // without bound while the server idles with no clients.
            //
            // So each connection gets its **own** feed pair, and this arm
            // drains the hub and re-publishes to all of them. It lives as a
            // third `select!` arm rather than a fourth task on purpose: this
            // task is already owned and aborted by `shutdown`, so the relay
            // needs no extra `Task` field and cannot outlive the server. The
            // subscriber list is only ever touched by this one task, so it
            // needs no lock either.
            //
            // `LiveMobSource` needs none of this — it is a
            // replace-latest-snapshot cache every connection diffs
            // independently, so it is already multi-consumer safe and is
            // shared directly.
            let mut subscribers: Vec<LanSubscriber> = Vec::new();
            let mut relay = tokio::time::interval(crate::tick::TICK_PERIOD);
            loop {
                tokio::select! {
                    _ = signal.notified() => break,
                    _ = relay.tick() => {
                        let changes = relay_block_ticks.drain_all();
                        let detonations = relay_explosions.drain_all();
                        // Issue #530's effect lane, relayed with its `except`
                        // tag intact: the hub cannot decide the exclusion,
                        // because "which player is excluded" is only meaningful
                        // against the connection about to drain it.
                        let effects = relay_block_ticks.drain_effects_tagged();
                        // Prune first, so a departed player's feed stops
                        // accumulating rather than growing forever.
                        subscribers.retain(LanSubscriber::is_alive);
                        for subscriber in &subscribers {
                            for (x, y, z, state) in &changes {
                                subscriber.block_ticks.publish(*x, *y, *z, state.clone());
                            }
                            for detonation in &detonations {
                                subscriber.explosions.publish(*detonation);
                            }
                            for (except, effect) in &effects {
                                match except {
                                    Some(player) => subscriber
                                        .block_ticks
                                        .publish_effect_except(*player, effect.clone()),
                                    None => subscriber.block_ticks.publish_effect(effect.clone()),
                                }
                            }
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((socket, peer)) = accepted else { break };
                        // Issue #336: the address the IP ban list is matched on.
                        let peer_ip = Some(peer.ip());
                        let protocol = protocol.clone();
                        let source = source.clone();
                        let block_entities = block_entities.clone();
                        let mobs = mobs.clone();
                        let commands = conn_commands.clone();
                        let resource_packs = conn_resource_packs.clone();
                        let plugin_channels = conn_plugin_channels.clone();
                        // One clone per accepted socket, all naming the same store.
                        let world_state = lan_world_state.clone();
                        // Issue #336: one clone per accepted socket, all naming
                        // the same lists — an op granted by one connection is an
                        // op for the next.
                        let access = conn_access.clone();
                        // Issue #438: the mob source and the shared player
                        // registry, composed. `PlayerAwareSource::snapshots`
                        // still returns only the mobs — the players travel
                        // through `EntitySource::players()`, which is what
                        // hands `serve_connection` a *viewer* id to exclude.
                        // See `crate::players`' own module docs.
                        let entities =
                            PlayerAwareSource::new(relay_mobs.clone(), relay_players.clone());
                        // Issue #465's LAN half, the one line `BlockTickFeed`'s
                        // own doc comment names: `subscriber()` keeps the
                        // outbound queue per-connection (the relay's drain-all
                        // depends on it) while **sharing** the inbound one, so a
                        // LAN player placing a repeater or a redstone torch
                        // actually reaches the tick loop that hosts its
                        // scheduled recheck. `default()` shared neither, and
                        // dropped every such placement silently.
                        let subscriber = LanSubscriber {
                            block_ticks: hub_block_ticks.subscriber(),
                            ..LanSubscriber::default()
                        };
                        let conn_block_ticks = subscriber.block_ticks.clone();
                        let conn_explosions = subscriber.explosions.clone();
                        let alive = Arc::clone(&subscriber.alive);
                        subscribers.push(subscriber);
                        // Fire-and-forget: route through the same `spawn` seam so
                        // all task spawning stays confined to `crate::spawn`, and
                        // detach by dropping the returned handle (a tokio
                        // `JoinHandle` detaches, it does not abort, on drop).
                        drop(spawn(async move {
                            let mut conn = Connection::new(socket);
                            // `_shared` + `&source` (issue #293): chunk
                            // generation for this connection runs on the
                            // blocking pool, so a LAN player crossing a chunk
                            // boundary no longer stalls the tick loop spawned
                            // above — which on a current-thread runtime would
                            // otherwise be the very same thread.
                            // Issue #325: LAN stays sleep-free — a fresh vote
                            // no connection calls, matching the fresh
                            // disconnected vote `run_tick_loop` (the loop this
                            // world's tick task runs) forwards. See
                            // `crate::sleep`'s module doc.
                            // Issue #545: open-to-LAN keeps the configured
                            // `view_radius` as its live-change ceiling, which is
                            // vanilla's `serverViewDistance`
                            // (`ChunkMap.java:826`) and the same policy that
                            // keeps `MAX_CAPACITY` on this path — a host spends
                            // memory and bandwidth on behalf of players who did
                            // not choose the setting.
                            // Issue #535: the *commands* variant, so a LAN
                            // host's `CommandDispatch` (and its resource-pack
                            // and plugin-channel surfaces) reach the
                            // connection. `bind` used the plain
                            // `..._mob_events_shared` wrapper, which hardcodes
                            // all three to `::default()` — which is exactly
                            // what left #48/#334/#335 unreachable.
                            let _ = serve_connection_with_mob_events_and_commands_shared(
                                &mut conn, &*protocol, &source, &entities, view_radius,
                                &block_entities, &mobs,
                                &conn_block_ticks, &conn_explosions,
                                &commands, &resource_packs, &plugin_channels, &world_state,
                                &access, peer_ip,
                            )
                            .await;
                            // Lets the relay arm above drop this connection's
                            // feeds on its next pass.
                            alive.store(false, std::sync::atomic::Ordering::Relaxed);
                        }));
                    }
                }
            }
        });

        // Issue #535 scope 3. Non-fatal on failure for the same reason the query
        // bind is: a world nobody can *discover* is still a world you can join
        // by typing the address.
        let discovery_task = match (discovery, local_addr) {
            (Some(discovery), Some(bound)) => {
                match spawn_lan_discovery(&shutdown, &discovery, bound.port()) {
                    Some(task) => Some(task),
                    None => None,
                }
            }
            _ => None,
        };

        let mut server = Self {
            local_addr,
            shutdown,
            task,
            tick_task: Some(tick_task),
            clock: Some(clock),
            server_tick: Some(server_tick),
            // LAN seeds no mob population (nothing calls `MobHandle::reseed`
            // here — see the `mobs` binding above), so there is nothing to seed.
            seed_task: None,
            #[cfg(not(target_arch = "wasm32"))]
            save: None,
            #[cfg(not(target_arch = "wasm32"))]
            autosave_task: None,
            #[cfg(not(target_arch = "wasm32"))]
            level_dat: None,
            #[cfg(not(target_arch = "wasm32"))]
            entity_storage: None,
            // LAN worlds are not persistent yet (`save` is `None` above), so
            // there is nothing for an entity save to write to.
            #[cfg(not(target_arch = "wasm32"))]
            mobs: None,
            world_state: handle_world_state,
            // Set by the `start_rcon` call just below when the caller asked for
            // one (issue #331). It needs a password, so it stays opt-in.
            #[cfg(not(target_arch = "wasm32"))]
            rcon_task: None,
            // `LanConfig::query` (issue #332), on by default; `None` also when
            // the UDP bind failed and the warning above was logged.
            #[cfg(not(target_arch = "wasm32"))]
            query_task,
            #[cfg(not(target_arch = "wasm32"))]
            discovery_task,
        };
        // After the `Self` literal, because `start_rcon` needs the handle's
        // shutdown signal — and propagating with `?` here is deliberate: a
        // caller that asked for RCON and did not get it has a security-relevant
        // surprise, unlike the two UDP listeners above.
        if let Some(rcon) = rcon {
            server.start_rcon(rcon)?;
        }
        Ok(server)
    }

    /// Returns the bound socket address, if this server was started with
    /// [`bind`](IntegratedServer::bind). In-memory servers have no address.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.local_addr
    }

    /// Starts an RCON listener on this server (issue #331), racing the same
    /// shutdown signal every other background task races.
    ///
    /// The listener is bound synchronously before this returns, so a port
    /// conflict is reported here rather than later, and the returned address
    /// is immediately connectable — give the config a port of `0` to let the
    /// OS assign one. The listener task is stored on the handle, so
    /// [`shutdown`](Self::shutdown) (and `Drop`) stop it with the server.
    ///
    /// # Errors
    ///
    /// Returns the [`std::io::Error`] from binding the listener.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_rcon(
        &mut self,
        config: crate::rcon::RconConfig,
    ) -> std::io::Result<std::net::SocketAddr> {
        // The listener gets **this** server's shared world state, whatever the
        // caller put in the config. A private `WorldStateHandle` here is the bug
        // issues #327 and #328 were both reported for, and over RCON it is
        // invisible: `/gamerule keep_inventory true` would report success and
        // change nothing anyone reads. Substituting rather than asserting means a
        // host cannot get it wrong.
        let config = crate::rcon::RconConfig { world: self.world_state.clone(), ..config };
        let (task, addr) = crate::rcon::spawn_listener(self.shutdown.notify_handle(), config)?;
        self.rcon_task = Some(task);
        Ok(addr)
    }

    /// A snapshot of this server's MSPT/TPS/overrun accounting (issue #285),
    /// or `None` for a handle with no unified tick loop.
    ///
    /// Two constructors start [`crate::tick::run_tick_loop`] and so return
    /// `Some`: [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs)
    /// (singleplayer) and [`bind`](Self::bind) (LAN, since issue #439). The
    /// remaining in-memory constructors return `None`, which is also what
    /// `bind` used to return — `tests/lan_world_tick.rs` leans on exactly that
    /// as its control.
    ///
    /// **One clock per handle, so this is one *world's* accounting.** It
    /// therefore cannot detect a duplicated tick loop: a per-connection loop
    /// would carry its own [`TickClock`] and this would still read a healthy
    /// 20 TPS while the world advanced at N×. See `tests/lan_world_tick.rs`
    /// for the counting-`ChunkSource` instrument that can.
    #[must_use]
    pub fn tick_stats(&self) -> Option<TickStats> {
        self.clock.as_deref().map(TickClock::stats)
    }

    /// How many times a system registered on this server's own
    /// `bevy_ecs::World` has run (issue #433 Phase 0), or `None` for a handle
    /// with no world-tick task — the same `Some` iff `tick_task` rule
    /// [`tick_stats`](Self::tick_stats) follows.
    ///
    /// # What this is for, and what it deliberately is not
    ///
    /// It is the evidence that the server `World` is *live* rather than an inert
    /// scaffold — the client's `WindowApp.ecs` (issue #37) is an `App` nothing
    /// ever runs a schedule against, and this accessor exists so the same thing
    /// cannot happen here unnoticed. It is **not** a way to read the `World`:
    /// the count is mirrored out through `crate::ecs::ServerTickWitness`,
    /// carries no simulation state, and hands out no reference. Per
    /// `docs/server-ecs.md` the `World` has no lock precisely because nothing
    /// outside the tick task reaches into it, and this must not become the
    /// exception.
    ///
    /// After Phase 0 this reads `Some(1)` for the life of the handle (one
    /// `ServerBoot` run). Once Phase 1 lands it advances once per world tick and
    /// should track [`TickStats::tick_count`] — a divergence between the two is
    /// the island detector.
    #[must_use]
    pub fn server_tick_count(&self) -> Option<u64> {
        self.server_tick
            .as_ref()
            .map(crate::ecs::ServerTickWitness::count)
    }

    /// Signals the serving task to stop without awaiting it. Idempotent.
    pub fn trigger_shutdown(&self) {
        self.shutdown.trigger();
    }

    /// Signals shutdown and awaits the serving task to completion.
    ///
    /// Prefer this over dropping when you want to be sure the task has wound
    /// down (e.g. before rebinding the same port).
    pub async fn shutdown(mut self) {
        self.shutdown.trigger();
        // Await the task(s) without moving the fields (the handle also has a
        // `Drop` impl). Natively this joins the tokio task; on wasm the task
        // is not joinable, so this returns once the `Notify` has been fired
        // above. `tick_task` is only `Some` for a handle built by
        // `open_in_memory_with_mobs`; the same one `notify_waiters()` call
        // above already signalled it (both tasks `select!` on clones of the
        // same `Arc<Notify>`).
        self.task.join().await;
        if let Some(mut tick_task) = self.tick_task.take() {
            tick_task.join().await;
        }
        // Aborted rather than joined, unlike the two above. Seeding's whole
        // point (issue #454) is that it holds a multi-second generation batch;
        // joining it would make `shutdown()` wait out the very stall this
        // removed. It races `shutdown` too, so the notify above has already
        // asked it to stop — this only covers a task parked on the blocking
        // pool where the signal cannot reach.
        if let Some(seed_task) = self.seed_task.take() {
            seed_task.abort();
        }
        // Aborted, not joined: it is an infinite timer loop, so joining it
        // would hang forever. The flush below is what actually persists.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(autosave_task) = self.autosave_task.take() {
            autosave_task.abort();
        }
        // Aborted, not joined: the accept loop parks in `accept()`, where the
        // notify above cannot reach it until a connection arrives. It also
        // cannot outlive the handle, which is all this abort guarantees.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(rcon_task) = self.rcon_task.take() {
            rcon_task.abort();
        }
        // Joined, not aborted, unlike the two above: the query listener races
        // the `shutdown` notify directly (through `spawn_tick_task`), so the
        // notify above has already ended it — joining just makes sure the UDP
        // port is actually released before this returns, so a caller that
        // immediately rebinds the same address does not race the old listener.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(mut query_task) = self.query_task.take() {
            query_task.join().await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(mut discovery_task) = self.discovery_task.take() {
            discovery_task.join().await;
        }
        // The final flush, **after** the tick and connection tasks have
        // stopped. Ordering is load-bearing: saving first would race a tick
        // that mutates a block between the write and the shutdown, and that
        // block would be lost with no error anywhere. Nothing can mark a chunk
        // dirty once both tasks are joined.
        // The world's age, stamped **before** the region flush and after both
        // tasks have stopped, so the number written is the tick count this
        // session actually reached rather than one sampled mid-tick.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(level) = self.level_dat.take() {
            let ticks = self.clock.as_ref().map_or(0, |clock| clock.tick_count());
            let scalars = self.world_state.level_data_fields();
            match tokio::task::spawn_blocking(move || level.write(ticks, &scalars)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => tracing::warn!("level.dat stamp on shutdown failed: {err}"),
                Err(err) => tracing::warn!("level.dat stamp on shutdown panicked: {err}"),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(handle) = self.save.take() {
            // `spawn_blocking` rather than a direct call: `shutdown` is an
            // `async fn` and may well be running on the runtime's core thread.
            match tokio::task::spawn_blocking(move || handle.save()).await {
                Ok(Ok(written)) => {
                    tracing::debug!("world saved on shutdown: {written} chunk columns");
                }
                Ok(Err(err)) => tracing::warn!("world save on shutdown failed: {err}"),
                Err(err) => tracing::warn!("world save on shutdown panicked: {err}"),
            }
        }
        // Issue #303: the mobs and dropped items, last, and for the same
        // ordering reason as the terrain above — the tick task has stopped, so
        // `saved_entities` cannot observe a half-advanced sim, and nothing can
        // spawn a mob after this point that we would then lose.
        //
        // A world with an autosave timer alone loses every mob spawned since the
        // last tick of it on a clean quit, which is the common case rather than
        // the rare one.
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(storage), Some(mobs)) = (self.entity_storage.take(), self.mobs.take()) {
            let saved = mobs.with(|sim| sim.saved_entities());
            let count = saved.len();
            match tokio::task::spawn_blocking(move || storage.save(&saved)).await {
                Ok(Ok(written)) => {
                    tracing::debug!("entities saved on shutdown: {written} of {count}");
                }
                Ok(Err(err)) => tracing::warn!("entity save on shutdown failed: {err}"),
                Err(err) => tracing::warn!("entity save on shutdown panicked: {err}"),
            }
        }
    }
}

impl Drop for IntegratedServer {
    fn drop(&mut self) {
        // Never leak a serving task past the handle: signal, then abort in
        // case a task is parked somewhere the signal cannot reach.
        self.shutdown.trigger();
        self.task.abort();
        if let Some(tick_task) = &self.tick_task {
            tick_task.abort();
        }
        if let Some(seed_task) = &self.seed_task {
            seed_task.abort();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(rcon_task) = &self.rcon_task {
            rcon_task.abort();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(query_task) = &self.query_task {
            query_task.abort();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(discovery_task) = &self.discovery_task {
            discovery_task.abort();
        }
    }
}

/// Spawns vanilla's `LanServerPinger`: one `[MOTD]…[/MOTD][AD]port[/AD]`
/// datagram to the discovery multicast group every 1.5 s, until shutdown.
///
/// `None` (with a warning) if the UDP socket cannot be created or the group
/// cannot be reached — a world nobody can discover is still a world you can join
/// by typing the address, so this is not worth failing `open_to_lan` for.
///
/// The send is `try_send_to`-shaped rather than awaited-with-backpressure: a
/// datagram nobody is listening for must never hold up the loop, and a dropped
/// ping is re-sent 1.5 s later by construction.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_lan_discovery(shutdown: &Arc<ShutdownSignal>, discovery: &LanDiscovery, port: u16) -> Option<Task> {
    let socket = match std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => socket,
        Err(err) => {
            tracing::warn!("LAN discovery disabled (UDP bind failed): {err}");
            return None;
        }
    };
    if let Err(err) = socket.set_nonblocking(true) {
        tracing::warn!("LAN discovery disabled (non-blocking mode failed): {err}");
        return None;
    }
    let socket = match tokio::net::UdpSocket::from_std(socket) {
        Ok(socket) => socket,
        Err(err) => {
            tracing::warn!("LAN discovery disabled (socket registration failed): {err}");
            return None;
        }
    };
    let target = std::net::SocketAddrV4::new(LanDiscovery::GROUP, LanDiscovery::PORT);
    let payload = discovery.payload(port);
    Some(spawn_tick_task(shutdown, async move {
        let mut ticker = tokio::time::interval(LanDiscovery::INTERVAL);
        loop {
            ticker.tick().await;
            if let Err(err) = socket.send_to(payload.as_bytes(), target).await {
                // Logged at debug: a laptop with no route to the multicast group
                // would otherwise warn every 1.5 s forever.
                tracing::debug!("LAN discovery ping failed: {err}");
            }
        }
    }))
}

/// Issue #454's gate: **world open must generate nothing at all.**
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use lodestone_core::State;
    use uuid::Uuid;

    use super::*;
    use crate::chunk::ChunkColumn;
    use crate::protocol::{ServerBound, ServerDirective};

    /// The seven required [`ServerProtocol`] methods, each answering with
    /// something inert. Nothing here drives a client, so none of them is ever
    /// actually called — mirrors `crate::ecs::gate`'s `Silent` rather than
    /// sharing it, because that one is private to that module.
    #[derive(Debug)]
    struct Silent;

    impl ServerProtocol for Silent {
        fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
            ServerBound::Ignored
        }
        fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            Vec::new()
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            Vec::new()
        }
        fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
            Vec::new()
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            ServerDirective::None
        }
        fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
            ServerDirective::None
        }
        fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
            ServerDirective::None
        }
    }

    /// A [`ChunkSource`] that hands out an all-air column instantly and records
    /// **per-coordinate** how many times it was asked for one.
    ///
    /// Per-coordinate rather than a bare total because the two defects this
    /// counts are different shapes: "49 columns generated at all" and "these 49
    /// generated *twice*". A single total cannot tell them apart.
    ///
    /// The counter lives behind an [`Arc`] because
    /// [`IntegratedServer::open_in_memory_with_mobs`] takes both of its sources
    /// **by value**, so the test cannot keep a borrow of either.
    #[derive(Debug, Clone)]
    struct CountingSource {
        calls: Arc<Mutex<HashMap<(i32, i32), usize>>>,
    }

    impl CountingSource {
        fn new(calls: &Arc<Mutex<HashMap<(i32, i32), usize>>>) -> Self {
            Self {
                calls: Arc::clone(calls),
            }
        }
    }

    impl ChunkSource for CountingSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            *self
                .calls
                .lock()
                .expect("counting source lock poisoned")
                .entry((cx, cz))
                .or_insert(0) += 1;
            ChunkColumn::new(0, 16)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; this gate only counts
            // generations, never reads terrain back for content.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // Built into `IntegratedServer` (which wraps sources in a
        // `ChunkStore`), so a player action could reach this through the
        // store's write-through. The source has no storage, so the edit is
        // deliberately discarded. Explicit rather than inherited (issue #440).
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this counting stub.
        }
    }

    fn total(calls: &Arc<Mutex<HashMap<(i32, i32), usize>>>) -> usize {
        calls
            .lock()
            .expect("counting source lock poisoned")
            .values()
            .sum()
    }

    /// The shell's own singleplayer parameters (`lodestone-shell/src/net.rs`),
    /// so this gate measures the configuration a player actually opens a world
    /// with rather than a convenient small one: `view_radius = 9`,
    /// `mob_radius = view_radius.clamp(1, 3) = 3` (a 7×7 = **49**-column tick
    /// area), mob centre block `(8, 8)`, six demo mobs.
    const VIEW_RADIUS: i32 = 9;
    const MOB_RADIUS: i32 = 3;

    /// One `CountingSource`, because since issue #436 there is only one source to
    /// pass. That is not a loss of coverage: the *two*-source arrangement is
    /// still measured, by
    /// [`control_two_independent_sources_generate_the_tick_area_twice`], which
    /// builds its own pair deliberately rather than going through this helper —
    /// so that control still reads 98 with every coordinate at 2, and this helper
    /// still reads 0.
    fn open_like_the_shell_does(
        calls: &Arc<Mutex<HashMap<(i32, i32), usize>>>,
    ) -> (IntegratedServer, DuplexStream) {
        IntegratedServer::open_in_memory_with_mobs(
            Silent,
            CountingSource::new(calls),
            (-MOB_RADIUS..=MOB_RADIUS, -MOB_RADIUS..=MOB_RADIUS),
            (8, 8),
            6,
            VIEW_RADIUS,
        )
    }

    /// **Issue #454's gate.** Opening a world must generate **zero** chunk
    /// columns before returning.
    ///
    /// The number is exact and predicted from the code path, not observed and
    /// written down: the constructor's job is to build handles and spawn tasks,
    /// so the only column generation it can legitimately do is none. The
    /// pre-fix figure is **49** — `MobHandle::seeded` ran a serial
    /// `ChunkWorld::from_source` over the whole `mob_area` inside the
    /// constructor, before any task spawned, which at the 909 ms per composed
    /// column measured in `chunk_store` is the ~45 s stall issue #454 is about.
    /// Observed pre-fix at 49 and post-fix at 0.
    ///
    /// # Why this is deterministic, with no polling
    ///
    /// `open_in_memory_with_mobs` is a plain `fn`. A spawned task is never
    /// polled synchronously, and this test reads the counter with **no
    /// intervening `.await`**, so nothing the constructor spawned can have run
    /// yet. Do not add an `.await` between the call and the read — the seeding
    /// task deliberately generates these columns *later*, so a yield would make
    /// this gate race its own subject.
    #[tokio::test]
    async fn world_open_generates_no_columns_at_all() {
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let (server, _client) = open_like_the_shell_does(&calls);
        let generated = total(&calls);
        assert_eq!(
            generated,
            0,
            "opening a world must generate no columns on the calling thread; got {generated}. \
             {} would mean mob seeding is back inside the constructor (issue #454).",
            (2 * MOB_RADIUS + 1) * (2 * MOB_RADIUS + 1)
        );
        drop(server);
    }

    /// The discovery datagram body is the *whole* LAN-discovery protocol —
    /// vanilla's `LanServerDetection` parses this literal string and nothing
    /// else, so an off-by-one in the markers is a world that never appears in
    /// the multiplayer list with no error anywhere.
    #[test]
    fn lan_discovery_payload_is_vanillas_literal_format() {
        let discovery = LanDiscovery {
            motd: "Matthew's World".to_string(),
        };
        assert_eq!(
            discovery.payload(25565),
            "[MOTD]Matthew's World[/MOTD][AD]25565[/AD]"
        );
        assert_eq!(LanDiscovery::PORT, 4445);
        assert_eq!(LanDiscovery::GROUP.octets(), [224, 0, 2, 60]);
    }

    /// Issue #535's config surface: RCON came up because `LanConfig` asked for
    /// it, not because a test called `start_rcon` by hand — which is the whole
    /// distinction the issue is about, since `start_rcon`'s only caller was its
    /// own test.
    #[tokio::test]
    async fn open_to_lan_starts_rcon_from_its_config() {
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let source = CountingSource::new(&calls);
        let server = IntegratedServer::open_to_lan(
            "127.0.0.1:0",
            Silent,
            source,
            LanConfig {
                view_radius: 0,
                rcon: Some(crate::rcon::RconConfig::new(
                    (std::net::Ipv4Addr::LOCALHOST, 0).into(),
                    "hunter2",
                    crate::command::CommandDispatch::none(),
                )),
                // Off, so this gate measures the RCON wiring alone and binds no
                // UDP port a parallel test could contend for.
                query: false,
                ..LanConfig::default()
            },
        )
        .await
        .expect("open_to_lan must bind");
        assert!(server.local_addr().is_some());
        assert!(
            server.rcon_task.is_some(),
            "`LanConfig::rcon` must start the listener that `bind` never could"
        );
        // Dropped rather than `shutdown().await`ed: `Drop` aborts every task,
        // where `shutdown` *joins* the accept loop and the tick loop, and this
        // gate has no reason to wait out a tick.
        drop(server);
    }

    /// **An open-to-LAN host must tick the world's *own* registries, not private
    /// ones.** Silent data loss: `open_to_lan` built a
    /// `BlockEntityHandle::default()`, so every chest filled while hosting was
    /// ticked correctly, sent correctly, and never written — the save path reads
    /// the source's registry, not the server's.
    ///
    /// The join is `ChunkSource::world_registries` surviving the `ChunkStore`
    /// wrap the constructor puts in front of the source, which is the part that
    /// cannot be seen by playing.
    #[test]
    fn a_persistent_source_hands_its_registries_through_the_chunk_store_wrap() {
        let dir = std::env::temp_dir().join("lodestone-lan-registries-k4m9");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch world dir");

        let calls = Arc::new(Mutex::new(HashMap::new()));
        let region = crate::region_source::RegionChunkSource::new(
            CountingSource::new(&calls),
            &dir,
            -64,
            384,
        )
        .expect("open world");
        let world_side = region.block_entities();

        // Exactly what `open_to_lan` does to the source before asking.
        let wrapped = ChunkStore::for_view_radius(region, 2);
        let registries = wrapped
            .world_registries()
            .expect("a world on disk must report its registries through the cache wrap");

        let pos = lodestone_model::BlockPos::new(3, 70, 5);
        registries
            .block_entities
            .with(|reg| reg.insert(pos, crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new())));
        assert!(
            world_side.with(|reg| reg.get(pos).is_some()),
            "the registry the LAN tick loop mutates must be the one the save path reads"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tick area, in the order the seeding task asks for it.
    fn mob_area_coords() -> Vec<(i32, i32)> {
        (-MOB_RADIUS..=MOB_RADIUS)
            .flat_map(|cz| (-MOB_RADIUS..=MOB_RADIUS).map(move |cx| (cx, cz)))
            .collect()
    }

    /// **Issue #454's second gate: once the seeding task has run, every column
    /// of the tick area has been generated exactly once — not twice.**
    ///
    /// The duplication was the actual defect (the ~11 s stall was its symptom):
    /// mob seeding built its own `ChunkWorld` from a **second, independent**
    /// generator that shared nothing with the `ChunkStore` the connection serves
    /// from, so opening a world generated the same 49 columns twice. Both sources
    /// here report into one counter, exactly as two instances of the same seeded
    /// generator would in production, so a second generation shows up as a count
    /// of 2 for some coordinate.
    ///
    /// Asserted per coordinate, not as a total: "98 generations" and "49
    /// generations of which one column was fetched 50 times" are different bugs,
    /// and a bare total cannot tell them apart.
    #[tokio::test]
    async fn seeding_generates_each_tick_area_column_exactly_once() {
        let expected = mob_area_coords();
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let (server, _client) = open_like_the_shell_does(&calls);

        // Bounded, and it waits on a *count* rather than a duration: the seeding
        // task hands its batch to the blocking pool, so there is no synchronous
        // point to observe instead. 400 × 5 ms is four orders of magnitude more
        // than 49 all-air columns need and still terminates rather than hanging
        // if the task never runs at all — which is the failure this would
        // otherwise mask.
        let mut waited = 0;
        while total(&calls) < expected.len() && waited < 400 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            waited += 1;
        }
        assert!(
            waited < 400,
            "the seeding task never generated the tick area; it is not running at all"
        );

        let counts = calls.lock().expect("counting source lock poisoned").clone();
        let worst = counts.iter().max_by_key(|entry| *entry.1);
        assert_eq!(
            worst.map(|(_, n)| *n),
            Some(1),
            "every column must be generated exactly once; worst offender {worst:?}. \
             A count of 2 means mob seeding is reading a second generator again \
             instead of the shared ChunkStore (issue #454)."
        );
        let mut generated: Vec<(i32, i32)> = counts.keys().copied().collect();
        generated.sort_unstable();
        let mut wanted = expected.clone();
        wanted.sort_unstable();
        assert_eq!(
            generated, wanted,
            "seeding must fetch exactly the tick area, no more and no less"
        );

        drop(server);
    }

    /// **The control for the gate above.** The pre-fix arrangement — two
    /// independent sources, one for the connection's store and one for mob
    /// pathing — must generate the tick area **twice**.
    ///
    /// Reproduced rather than described: `ChunkStore::for_view_radius(source,
    /// VIEW_RADIUS)` is what the connection path serves from — the same
    /// constructor and the same radius `open_in_memory_with_mobs` uses, so issue
    /// #505's capacity derivation is in the picture here too rather than a
    /// literal — `MobHandle::seeded(&world_source, …)` is what the constructor
    /// used to call, and both report into a single counter the way two instances
    /// of one seeded generator do in production. Predicted exactly: 49 columns ×
    /// 2 paths = **98**, with every coordinate at 2.
    ///
    /// If this ever reads 49, the two paths have stopped being independent and
    /// the gate above is passing for a reason that has nothing to do with the
    /// fix.
    #[test]
    fn control_two_independent_sources_generate_the_tick_area_twice() {
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let store = ChunkStore::for_view_radius(CountingSource::new(&calls), VIEW_RADIUS);
        let world_source = CountingSource::new(&calls);

        // The connection path: the initial view, of which the tick area is a
        // subset. Only the tick area is fetched here — that is the overlap, and
        // the overlap is the whole point.
        for &(cx, cz) in &mob_area_coords() {
            let _ = store.column(cx, cz);
        }
        // The mob path, exactly as the pre-#454 constructor called it.
        let handle = MobHandle::seeded(
            &world_source,
            -MOB_RADIUS..=MOB_RADIUS,
            -MOB_RADIUS..=MOB_RADIUS,
            8,
            8,
            6,
        );

        let counts = calls.lock().expect("counting source lock poisoned").clone();
        let area = mob_area_coords().len();
        assert_eq!(
            total(&calls),
            area * 2,
            "two independent sources must generate the tick area twice ({area} × 2)"
        );
        assert!(
            counts.values().all(|&n| n == 2),
            "and every single column must be generated twice, not just the total \
             happening to double: {counts:?}"
        );
        drop(handle);
    }

    /// **The control for the gate above, and it must fail the same assertion.**
    ///
    /// `MobHandle::seeded` is *exactly* the call
    /// `open_in_memory_with_mobs` used to make, inline, before it spawned
    /// anything — it survives unchanged (see its own doc comment), so the work
    /// issue #454 moved can still be measured directly rather than described.
    /// Driving it over the same [`CountingSource`] and the same `mob_area` must
    /// generate **49** columns, on the calling thread, with nothing spawned.
    ///
    /// Two things this proves that the gate alone cannot:
    ///
    /// * the detector fires — a `CountingSource` that silently counted nothing
    ///   would pass `world_open_generates_no_columns_at_all` vacuously, and this
    ///   is the reading that rules that out;
    /// * the pre-fix figure was 49 and not some smaller number, so the ~45 s
    ///   arithmetic in issue #454 is multiplying the right count.
    ///
    /// It also pins the area arithmetic: `(2 * 3 + 1)²`, i.e. the `-3..=3`
    /// square the shell's `view_radius.clamp(1, 3)` produces — **not** the 3×3
    /// a casual reading of "mob radius 3" suggests.
    #[test]
    fn control_the_old_synchronous_seeding_generates_the_whole_mob_area() {
        const EXPECTED: usize = ((2 * MOB_RADIUS + 1) * (2 * MOB_RADIUS + 1)) as usize;
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let source = CountingSource::new(&calls);
        let handle = MobHandle::seeded(
            &source,
            -MOB_RADIUS..=MOB_RADIUS,
            -MOB_RADIUS..=MOB_RADIUS,
            8,
            8,
            6,
        );
        let generated = total(&calls);
        assert_eq!(
            generated, EXPECTED,
            "the pre-#454 constructor generated the whole mob area synchronously; \
             this control must reproduce that exactly, and 0 would mean the counter \
             is not wired and the gate beside it is vacuous"
        );
        assert!(
            generated > 0,
            "a control that counts nothing cannot detect the defect"
        );
        drop(handle);
    }

    /// Wall-clock world-open cost with the **real** composed overworld
    /// generator, at the shell's own parameters — a *recording*, not a gate.
    ///
    /// `#[ignore]`d and duration-shaped on purpose: durations in this repo
    /// showed a 2.3× spread from machine load alone on an identical release
    /// binary, so this figure is provisional unless the box is quiet. The
    /// assertion that actually protects the fix is
    /// [`world_open_generates_no_columns_at_all`] above, which counts.
    ///
    /// Run with:
    /// `cargo test --release -p lodestone-server --lib -- --ignored --nocapture world_open_wall_clock`
    #[tokio::test]
    #[ignore = "wall-clock recording with the real overworld generator; run explicitly \
                with --release -- --ignored --nocapture"]
    async fn world_open_wall_clock_with_the_real_generator() {
        let seed = 42;

        // The pre-fix cost, measured rather than deduced: this is the exact call
        // the constructor used to make inline. Run first, on the same box in the
        // same second as the post-fix reading below, so the pair is a comparison
        // and not two independent samples of a 2.3×-noisy quantity.
        let started = web_time::Instant::now();
        let seeded = MobHandle::seeded(
            &crate::overworld_chunk_source(seed),
            -MOB_RADIUS..=MOB_RADIUS,
            -MOB_RADIUS..=MOB_RADIUS,
            8,
            8,
            6,
        );
        let before = started.elapsed();
        drop(seeded);

        let started = web_time::Instant::now();
        let (server, _client) = IntegratedServer::open_in_memory_with_mobs(
            Silent,
            crate::overworld_chunk_source(seed),
            (-MOB_RADIUS..=MOB_RADIUS, -MOB_RADIUS..=MOB_RADIUS),
            (8, 8),
            6,
            VIEW_RADIUS,
        );
        let after = started.elapsed();

        println!("pre-#454 synchronous mob seeding (49 columns): {before:?}");
        println!("post-#454 world open (constructor only):       {after:?}");
        drop(server);
    }
}
