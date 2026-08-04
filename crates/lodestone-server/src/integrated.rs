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
use crate::mobs::{LiveMobSource, MobHandle};
use crate::protocol::ServerProtocol;
use crate::server::{EntitySource, NoEntities, serve_connection};
use crate::spawn::{Task, spawn};
use crate::tick::{TickClock, TickStats};
// `run_tick_loop` (like `open_in_memory_with_mobs`, its one caller) is
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
/// [`crate::tick::run_tick_loop`], so there is now only one call site left —
/// this helper still earns its keep as the one place the shutdown-race
/// wrapper is written, rather than because it is shared by several callers
/// today. Native only, like the tick loop itself and every caller of this
/// function.
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
                _ = serve_connection(&mut conn, &protocol, &source, &entities, view_radius, &block_entities, &mobs) => {}
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
        // Issue #12: built *synchronously*, here, before the tick task spawns —
        // not inside `run_mob_tick_loop`'s own future the way the pre-handle
        // version built its `ChunkWorld`/`MobSim` — so the exact same handle
        // (cloned below) can be shared by the connection task (which mutates
        // it on an `Attack` packet, through `crate::server::apply_attack`)
        // and the tick-loop task (which ticks and republishes it). See
        // `MobHandle`'s own doc comment for why this is `'static`-safe.
        let (cx_range, cz_range) = mob_area;
        let (center_x, center_z) = mob_center;
        let mob_handle = MobHandle::seeded(
            &world_source,
            cx_range,
            cz_range,
            center_x,
            center_z,
            mob_count,
        );

        let conn_signal = shutdown.clone();
        let conn_entities = live_mobs.clone();
        let conn_block_entities = block_entities.clone();
        let conn_mobs = mob_handle.clone();
        let task = spawn(async move {
            let mut conn = Connection::new(server_end);
            tokio::select! {
                _ = conn_signal.notified() => {}
                _ = serve_connection(&mut conn, &protocol, &source, &conn_entities, view_radius, &conn_block_entities, &conn_mobs) => {}
            }
        });

        let clock = Arc::new(TickClock::new());
        let tick_task = spawn_tick_task(
            &shutdown,
            run_tick_loop(mob_handle, live_mobs, block_entities, Arc::clone(&clock)),
        );

        (
            Self {
                #[cfg(not(target_arch = "wasm32"))]
                local_addr: None,
                shutdown,
                task,
                tick_task: Some(tick_task),
                clock: Some(clock),
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
        let source = Arc::new(source);
        let shutdown = Arc::new(Notify::new());
        let signal = shutdown.clone();
        // Shared across every accepted connection (like `protocol`/`source`
        // above) rather than one per connection, so two LAN players placing
        // and interacting with the same furnace see the same state — no
        // tick loop is spawned for it here, though (see the struct's
        // `tick_task` doc comment for why only the mobs constructor
        // does that); a block entity placed over LAN exists and holds state
        // but does not advance on its own, the same real-but-static gap
        // `open_in_memory_with_entities` has. Same reasoning for `mobs`: no
        // live population over LAN via this constructor, but an `Attack`
        // packet against it is still safe (see `MobHandle::default`'s own
        // doc comment) rather than a special-cased no-op path.
        let block_entities = BlockEntityHandle::default();
        let mobs = MobHandle::default();

        let task = spawn(async move {
            loop {
                tokio::select! {
                    _ = signal.notified() => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _peer)) = accepted else { break };
                        let protocol = protocol.clone();
                        let source = source.clone();
                        let block_entities = block_entities.clone();
                        let mobs = mobs.clone();
                        // Fire-and-forget: route through the same `spawn` seam so
                        // all task spawning stays confined to `crate::spawn`, and
                        // detach by dropping the returned handle (a tokio
                        // `JoinHandle` detaches, it does not abort, on drop).
                        drop(spawn(async move {
                            let mut conn = Connection::new(socket);
                            let _ = serve_connection(
                                &mut conn, &*protocol, &*source, &NoEntities, view_radius,
                                &block_entities, &mobs,
                            )
                            .await;
                        }));
                    }
                }
            }
        });

        Ok(Self {
            local_addr,
            shutdown,
            task,
            tick_task: None,
            clock: None,
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
    /// or `None` for a handle with no unified tick loop — every constructor
    /// except [`open_in_memory_with_mobs`](Self::open_in_memory_with_mobs),
    /// the only one that starts [`crate::tick::run_tick_loop`] today (the same
    /// scope the struct's own `tick_task` field already had before this
    /// accessor existed).
    #[must_use]
    pub fn tick_stats(&self) -> Option<TickStats> {
        self.clock.as_deref().map(TickClock::stats)
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
