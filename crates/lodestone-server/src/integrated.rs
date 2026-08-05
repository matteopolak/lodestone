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
use crate::chunk_store::ChunkStore;
use crate::mobs::{LiveMobSource, MobHandle};
use crate::protocol::ServerProtocol;
use crate::server::{
    EntitySource, NoEntities, serve_connection_shared, serve_connection_with_mob_events_shared,
};
use crate::spawn::{Task, spawn};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock, TickStats};
// `run_tick_loop` (like `open_in_memory_with_mobs` and, since issue #439,
// `bind` — its two callers) is
// `#[cfg(not(target_arch = "wasm32"))]`-gated in `tick.rs` — this import must
// carry the identical `cfg`, or it is an unresolved-import hard error on
// wasm32 regardless of whether the name is ever reached at that target.
// **This was already broken on `main` before this change**: the two
// functions this loop replaces (`mobs::run_mob_tick_loop`,
// `block_entities::run_block_entity_tick_loop`) were imported by this same
// file with no such gate, so `cargo build -p lodestone-server --target
// wasm32-unknown-unknown` (the check `scripts/wasm-check.sh` runs) was
// already red — re-verified directly in a throwaway worktree at this
// crate's own pre-#284 `HEAD`, not assumed. Fixed here rather than left,
// since this refactor already touches every one of these imports.
#[cfg(not(target_arch = "wasm32"))]
use crate::tick::run_tick_loop;

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
fn spawn_tick_task<F>(shutdown: &Arc<Notify>, fut: F) -> Task
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

/// A running integrated server that owns its serving task(s).
///
/// Dropping the handle signals shutdown and aborts the task, so a server can
/// never outlive the value that started it — the "can't leak a thread"
/// guarantee a shell consuming this needs.
#[derive(Debug)]
pub struct IntegratedServer {
    #[cfg(not(target_arch = "wasm32"))]
    local_addr: Option<std::net::SocketAddr>,
    shutdown: Arc<Notify>,
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
        let shutdown = Arc::new(Notify::new());
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
        let source = Arc::new(ChunkStore::new(source));
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
                _ = serve_connection_shared(&mut conn, &protocol, &source, &entities, view_radius, &block_entities, &mobs) => {}
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
    /// `world_source` is a **second, independent instance** of whatever
    /// [`ChunkSource`] `source` also is — not the same value, and not shared.
    /// See `mobs::run_mob_tick_loop`'s own doc comment for why two instances
    /// rather than one shared one: every `ChunkSource` this crate ships is a
    /// pure function of its construction parameters/seed, so two instances
    /// built the same way (same seed) produce identical terrain.
    ///
    /// `mob_area` is the `(cx_range, cz_range)` of chunk columns loaded once
    /// into the sim's `ChunkWorld` snapshot — pick a range that covers
    /// `mob_center` with room to path around in; it does not grow later (see
    /// the scope note on `mobs::run_mob_tick_loop`). `mob_center` is the block
    /// `(x, z)` mobs are seeded around; `mob_count` is how many.
    ///
    /// Native only, like [`bind`](Self::bind) — the tick loop's timer needs
    /// `tokio::time`, unavailable on `wasm32` (see `mobs::run_mob_tick_loop`'s
    /// doc comment).
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn open_in_memory_with_mobs<P, S, M>(
        protocol: P,
        source: S,
        world_source: M,
        mob_area: (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>),
        mob_center: (i32, i32),
        mob_count: usize,
        view_radius: i32,
    ) -> (Self, DuplexStream)
    where
        P: ServerProtocol + 'static,
        S: ChunkSource + 'static,
        M: ChunkSource + 'static,
    {
        let (client_end, server_end) = memory_pair();
        let shutdown = Arc::new(Notify::new());
        let live_mobs = LiveMobSource::default();
        // Shared with the tick task spawned below, the same way `live_mobs`
        // is — this is the constructor `docs/block-entities.md` named as the
        // one with somewhere to hang the unified tick loop's block-entity work
        // off of (issue #284; before that, a separate
        // `run_block_entity_tick_loop`).
        let block_entities = BlockEntityHandle::default();
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
        // Issue #12: built *synchronously*, here, before the tick task spawns —
        // not inside `run_mob_tick_loop`'s own future the way the pre-handle
        // version built its `ChunkWorld`/`MobSim` — so the exact same handle
        // (cloned below) can be shared by the connection task (which mutates
        // it on an `Attack` packet, through `crate::server::apply_attack`)
        // and the tick-loop task (which ticks and republishes it). See
        // `MobHandle`'s own doc comment for why this is `'static`-safe.
        let (cx_range, cz_range) = mob_area;
        // Issues #307/#308: the same small fixed region `mob_area` already
        // names, reused rather than adding a second range parameter — see
        // `tick::run_tick_loop`'s own doc comment for why this crate has no
        // general "loaded chunks" registry to draw a wider one from yet.
        let tick_area = (cx_range.clone(), cz_range.clone());
        let (center_x, center_z) = mob_center;
        let mob_handle = MobHandle::seeded(
            &world_source,
            cx_range,
            cz_range,
            center_x,
            center_z,
            mob_count,
        );

        // Issues #307/#308: `source` is now shared between the connection
        // task (which serves it over the wire — chunk generation, and every
        // player-driven `set_block`) and the tick task (which random-ticks
        // it) — the same object, not two independent instances, which is
        // exactly what makes a random tick's mutation visible to the client
        // this server actually serves rather than to an unwatched second
        // copy. Contrast `world_source: M` right above, which stays
        // deliberately unshared (see this function's own doc comment on
        // that parameter) — that one backs *mob pathing*, which never needs
        // to agree with what the client was sent, only with itself.
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
        // Note `world_source: M` above is deliberately **not** wrapped: it is
        // read exactly once per column by `MobHandle::seeded`, so retention
        // would buy it nothing.
        let source = Arc::new(ChunkStore::new(source));
        let conn_signal = shutdown.clone();
        let conn_entities = live_mobs.clone();
        let conn_block_entities = block_entities.clone();
        let conn_mobs = mob_handle.clone();
        let conn_source = Arc::clone(&source);
        let conn_block_ticks = block_tick_feed.clone();
        let conn_explosions = explosion_feed.clone();
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
                    &conn_block_entities,
                    &conn_mobs,
                    &conn_block_ticks,
                    &conn_explosions,
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
        let tick_task = spawn_tick_task(&shutdown, async move {
            // Owned by the tick task, with no lock, per `docs/server-ecs.md`.
            // Phase 1 replaces this binding with a `&mut` argument to
            // `run_tick_loop` and runs `GameTick` once per iteration.
            let _server_world = server_world;
            run_tick_loop(
                mob_handle,
                live_mobs,
                block_entities,
                tick_clock,
                tick_source,
                block_tick_feed,
                tick_area,
                explosion_feed,
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
            },
            client_end,
        )
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
        let source = Arc::new(ChunkStore::new(source));
        let shutdown = Arc::new(Notify::new());
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
        let block_entities = BlockEntityHandle::default();
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
        let tick_task = spawn_tick_task(&shutdown, async move {
            // Owned by the tick task, with no lock, per `docs/server-ecs.md`.
            let _server_world = server_world;
            run_tick_loop(
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
            )
            .await;
        });

        let relay_block_ticks = hub_block_ticks.clone();
        let relay_explosions = hub_explosions.clone();
        let relay_mobs = live_mobs.clone();
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
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((socket, _peer)) = accepted else { break };
                        let protocol = protocol.clone();
                        let source = source.clone();
                        let block_entities = block_entities.clone();
                        let mobs = mobs.clone();
                        let entities = relay_mobs.clone();
                        let subscriber = LanSubscriber::default();
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
                            let _ = serve_connection_with_mob_events_shared(
                                &mut conn, &*protocol, &source, &entities, view_radius,
                                &block_entities, &mobs,
                                &conn_block_ticks, &conn_explosions,
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

        Ok(Self {
            local_addr,
            shutdown,
            task,
            tick_task: Some(tick_task),
            clock: Some(clock),
            server_tick: Some(server_tick),
        })
    }

    /// Returns the bound socket address, if this server was started with
    /// [`bind`](IntegratedServer::bind). In-memory servers have no address.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.local_addr
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
        self.shutdown.notify_waiters();
    }

    /// Signals shutdown and awaits the serving task to completion.
    ///
    /// Prefer this over dropping when you want to be sure the task has wound
    /// down (e.g. before rebinding the same port).
    pub async fn shutdown(mut self) {
        self.shutdown.notify_waiters();
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
    }
}

impl Drop for IntegratedServer {
    fn drop(&mut self) {
        // Never leak a serving task past the handle: signal, then abort in
        // case a task is parked somewhere the signal cannot reach.
        self.shutdown.notify_waiters();
        self.task.abort();
        if let Some(tick_task) = &self.tick_task {
            tick_task.abort();
        }
    }
}
