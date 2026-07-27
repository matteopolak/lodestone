//! Live-server networking, kept strictly behind `lodestone-client`'s public API.
//!
//! The shell selects a version by **protocol number** through
//! [`lodestone_registry::adapter_for_protocol`] and otherwise deals only in the
//! version-free `lodestone-model` types re-exported by the client. It never
//! names a packet, a version, or a version crate. TCP is reached only via
//! [`ClientBuilder::connect`], preserving the [`Transport`] seam a future wasm
//! build needs.
//!
//! The client is async (tokio); the shell's render loop is not. So a background
//! thread owns a current-thread runtime and the [`ClientHandle`]/`EventStream`,
//! and forwards decoded events as [`NetUpdate`]s down a synchronous channel the
//! app drains once per frame.
//!
//! ## What actually arrives (measured, see the report)
//!
//! With the `v770` family compiled in, this connects and logs in against the
//! live 26.2 server, and `Login`/`KeepAlive`/`Chat`/`Disconnect` surface as
//! events. **`ChunkLoaded` carries only a [`ChunkPos`] — no block data — and
//! per the §12.24 ruling it stays that way**: it is a "region dirty at `pos`,
//! re-mesh it" signal, not a payload. World state must never be reconstructible
//! only from a bounded/lossy event stream, so the decoded blocks live in a
//! `World` owned by the client; consumers *query* that world rather than
//! accumulate it from events.
//!
//! ## Outbound (S6)
//!
//! Movement flows the other way through the same thread: the sim queues a
//! [`ClientAction::Move`] every 20 Hz tick onto an `mpsc` sender; the net loop
//! drains it each iteration and hands it to [`ClientHandle::send_action`], which
//! the version adapter lowers into the concrete movement packet. The shell never
//! names that packet.
//!
//! **Seam status (verified 2026-07-27):** the v770 adapter now has a `Move`
//! encode arm (→ `move_player_pos_rot`) and a `SwingArm` arm (→ `swing`), so the
//! `Move`s the controller queues each tick now reach the wire instead of being
//! dropped as `Ok(None)`. As a side-effect the client's read-model records our
//! own outgoing position (a local echo), so `ClientHandle::position()` returns
//! `Some` once we start moving. Two things remain out of *this* crate's hands:
//! whether the server accepts our physics without a corrective teleport is
//! `impl-physics`'s live gate (its negative control), and server-authoritative
//! reconciliation arrives as [`ClientEvent::TeleportPlayer`], which the shell
//! will consume alongside the live-world swap.
//!
//! ## Reading the client-owned world (the section-source seam)
//!
//! The read path now exists: [`ClientHandle::sections_at`] hands back owned
//! `Arc<ChunkSection>` snapshots for a batch of `(ChunkPos, section_index)`
//! requests under a single lock acquisition, and [`ClientHandle::loaded_chunks`]
//! enumerates which columns are resident. The net thread owns the `ClientHandle`
//! but publishes an `Arc` clone of it into a shared [`OnceLock`] the moment the
//! session is up, so the render/mesh thread can pull a live 27-section
//! neighbourhood out of the client's world without touching tokio and without
//! blocking the net loop. [`NetClient::sections_at`] / [`NetClient::loaded_chunks`]
//! / [`NetClient::server_position`] are that surface; before login they return
//! empty, never panic.
//!
//! **What this does *not* yet reach, verified 2026-07-28 and reported upstream:**
//! a `ChunkSection` carries block-states + biomes only — *no light*, and the
//! handle exposes *no column geometry* (`min_y` / `section_count`). `block_at`
//! returns the column's `air_id` (not `None`) for out-of-range Y, so `min_y` is
//! not even derivable by probing. So the shell can read section *contents* but
//! cannot yet (a) place them at their true world-Y or (b) light them — both are
//! pending `lodestone-world`/client seams (`ClientHandle::column_geometry` or a
//! restored `chunk(pos) -> Arc<LoadedChunk>`, plus a bulk light read). Rendered
//! live terrain waits on those; the *data* path is proven end-to-end here.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::JoinHandle;
use std::time::Duration;

use lodestone_client::{
    ChunkPos, ChunkSection, ClientAction, ClientBuilder, ClientEvent, ClientHandle, LoginProfile,
    PlayerListEntry, ServerAddress, Vec3,
};

pub use lodestone_testsupport::unique_username;

use crate::overlay::{BossBarView, Sidebar, boss_bars_from, sidebar_from};

/// A handle to the live client, published by the net thread once the session is
/// up and read by the render/mesh thread. `None` until login completes.
type SharedHandle = Arc<OnceLock<Arc<ClientHandle>>>;

/// A decoded, version-free update the app can act on without touching tokio.
#[derive(Debug, Clone)]
pub enum NetUpdate {
    /// The background task is attempting to connect.
    Connecting,
    /// Login completed; the local player entity id.
    LoggedIn {
        /// Server-assigned entity id for the local player.
        entity_id: i32,
    },
    /// A chat/system message (plain text).
    Chat(String),
    /// A chunk became dirty at this position: the server sent (and the client
    /// applied to its world) chunk data here, so any mesh covering this column
    /// should be rebuilt. Block data is *not* carried — it is queried from the
    /// client-owned world per the §12.24 ruling (see the module docs).
    Chunk {
        /// Chunk X.
        x: i32,
        /// Chunk Z.
        z: i32,
    },
    /// Player health/food changed. `health == 0.0` means dead — the
    /// chunk-blackout trap the shell surfaces in its status line.
    Health {
        /// Current health in `0..=20`.
        health: f32,
        /// Current food level in `0..=20`.
        food: i32,
    },
    /// The player died.
    Death,
    /// The session ended (clean or with a reason).
    Disconnected(String),
    /// A transport or setup error.
    Error(String),
}

/// A live client running on a background thread. Drop to request shutdown.
#[derive(Debug)]
pub struct NetClient {
    rx: Receiver<NetUpdate>,
    /// Outbound actions (movement, swings, chat) queued for the net thread to
    /// hand to the client. Kept off the render thread; the net loop drains it.
    action_tx: Sender<ClientAction>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Published by the net thread once login completes; lets the render/mesh
    /// thread read the client-owned world lock-free of tokio.
    handle: SharedHandle,
}

impl NetClient {
    /// Spawn a background thread that connects to `host:port` speaking the given
    /// protocol number and forwards events. Returns immediately.
    #[must_use]
    pub fn connect(host: String, port: u16, protocol: i32) -> Self {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle: SharedHandle = Arc::new(OnceLock::new());
        let handle_thread = Arc::clone(&handle);

        let thread = std::thread::Builder::new()
            .name("lodestone-net".into())
            .spawn(move || {
                run(
                    host,
                    port,
                    protocol,
                    tx,
                    action_rx,
                    stop_thread,
                    handle_thread,
                )
            })
            .expect("spawn net thread");

        Self {
            rx,
            action_tx,
            stop,
            thread: Some(thread),
            handle,
        }
    }

    /// Drain all updates received since the last poll (non-blocking).
    #[must_use]
    pub fn poll(&self) -> Vec<NetUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = self.rx.try_recv() {
            out.push(u);
        }
        out
    }

    /// Queue an outbound action for the net thread to submit through the client
    /// handle. Best-effort: if the session has ended the send is silently
    /// dropped (the shell keeps rendering regardless).
    pub fn send_action(&self, action: ClientAction) {
        let _ = self.action_tx.send(action);
    }

    /// Batch-read owned section snapshots from the client-owned world, one lock
    /// acquisition for the whole request. Empty (all `None`) before login or for
    /// columns/sections the client doesn't hold — never blocks, never panics.
    ///
    /// This is the section-source seam the render/mesh layer consumes: it hands
    /// out block-state sections only. Placing them at their true world-Y and
    /// lighting them needs column geometry and a light read the handle does not
    /// yet expose (see the module docs); this surface is ready for both the
    /// moment those land.
    #[must_use]
    pub fn sections_at(&self, requests: &[(ChunkPos, usize)]) -> Vec<Option<Arc<ChunkSection>>> {
        match self.handle.get() {
            Some(h) => h.sections_at(requests),
            None => vec![None; requests.len()],
        }
    }

    /// The columns the client currently holds. Empty before login.
    #[must_use]
    pub fn loaded_chunks(&self) -> Vec<ChunkPos> {
        self.handle
            .get()
            .map_or_else(Vec::new, |h| h.loaded_chunks())
    }

    /// The current tab-list entries (version-free `PlayerListEntry`), read from
    /// the client-owned state through the shared handle. Empty before login and
    /// never blocks — same lock-free read path as [`sections_at`](Self::sections_at).
    #[must_use]
    pub fn players(&self) -> Vec<PlayerListEntry> {
        self.handle.get().map_or_else(Vec::new, |h| h.players())
    }

    /// The scoreboard sidebar to draw, folded from the live snapshot, or `None`
    /// when no objective occupies the sidebar slot (or before login).
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        self.handle
            .get()
            .and_then(|h| sidebar_from(&h.scoreboard()))
    }

    /// The active boss bars to draw, folded from the live snapshot, in server
    /// render order. Empty when none are shown (or before login).
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.handle
            .get()
            .map_or_else(Vec::new, |h| boss_bars_from(&h.boss_bars()))
    }

    /// The server-known position of the local player, once movement or a
    /// teleport has established it. `None` before then.
    #[must_use]
    pub fn server_position(&self) -> Option<Vec3> {
        self.handle.get().and_then(|h| h.position())
    }

    /// A server-less client used only in tests: no thread, no connection. It
    /// captures every [`send_action`](Self::send_action) on the returned
    /// receiver so the outbound path can be asserted without a live server.
    #[cfg(test)]
    pub(crate) fn loopback() -> (Self, Receiver<ClientAction>) {
        // `rx`'s sender is dropped immediately, so `poll` just yields nothing.
        let (_tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let client = Self {
            rx,
            action_tx,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
        };
        (client, action_rx)
    }

    /// Like [`loopback`](Self::loopback) but also hands back the inbound
    /// [`NetUpdate`] sender, so tests can drive the phase/status mapping in
    /// [`crate::sim`] without a live server. Also returns the captured-action
    /// receiver so a test can both push the session to `Connected` and assert
    /// the outbound movement it then produces.
    #[cfg(test)]
    pub(crate) fn loopback_with_feed() -> (Self, Receiver<ClientAction>, Sender<NetUpdate>) {
        let (tx, rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let client = Self {
            rx,
            action_tx,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            handle: Arc::new(OnceLock::new()),
        };
        (client, action_rx, tx)
    }
}

impl Drop for NetClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    host: String,
    port: u16,
    protocol: i32,
    tx: Sender<NetUpdate>,
    action_rx: Receiver<ClientAction>,
    stop: Arc<AtomicBool>,
    shared_handle: SharedHandle,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(NetUpdate::Error(format!("runtime: {e}")));
            return;
        }
    };

    runtime.block_on(async move {
        let Some(adapter) = lodestone_registry::adapter_for_protocol(protocol) else {
            let _ = tx.send(NetUpdate::Error(format!(
                "no version family compiled in for protocol {protocol}; build with the `live` feature"
            )));
            return;
        };

        let _ = tx.send(NetUpdate::Connecting);
        let profile = LoginProfile {
            username: unique_username(),
            uuid: uuid::Uuid::new_v4(),
        };
        let server = ServerAddress { host, port };

        let (handle, mut events) =
            match ClientBuilder::new(server, profile, adapter)
                .connect_timeout(Some(Duration::from_secs(10)))
                .connect()
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(NetUpdate::Error(format!("connect: {e}")));
                    return;
                }
            };

        // Publish the handle so the render/mesh thread can read the client-owned
        // world (sections_at / loaded_chunks / position). `send_action` is `&self`
        // so the net loop keeps driving outbound movement through the same `Arc`.
        // `shutdown` is `&mut self` and unreachable through a shared `Arc`; the
        // stop path below instead breaks the loop, dropping the runtime and
        // tearing the driver (and its connection) down — a TCP close rather than
        // a protocol disconnect, which the shell treats as equivalent.
        let handle = Arc::new(handle);
        let _ = shared_handle.set(Arc::clone(&handle));

        let mut handed_actions: u64 = 0;
        loop {
            // Flush queued outbound actions first so player movement (queued at
            // 20 Hz) reaches the client promptly rather than waiting on the next
            // inbound event. `send_action` is sync and cheap.
            //
            // NB: this counts actions *handed to the client handle*, not bytes on
            // the wire. Whether an action produces a packet is the version
            // adapter's `encode_action`; on v770 a `Move`/`SwingArm` in the Play
            // state now encodes (→ `move_player_pos_rot` / `swing`), while actions
            // the adapter can't represent in the current state are dropped quietly
            // by the driver. This counter is a queue-liveness signal, never proof
            // of wire delivery — that lives in `impl-physics`'s live gate.
            while let Ok(action) = action_rx.try_recv() {
                let _ = handle.send_action(action);
                handed_actions += 1;
                if handed_actions == 1 || handed_actions.is_multiple_of(20) {
                    tracing::debug!(target: "net", "handed {handed_actions} action(s) to client handle (encode is the adapter's job)");
                }
            }
            // A short timeout keeps the outbound drain responsive even when the
            // server is quiet (no inbound events to wake us).
            match tokio::time::timeout(Duration::from_millis(15), events.recv()).await {
                Ok(Some(event)) => {
                    if forward(&tx, event).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(NetUpdate::Disconnected("stream closed".into()));
                    break;
                }
                Err(_timeout) => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    });
}

/// Forward one event; `Err` signals the loop to stop.
fn forward(tx: &Sender<NetUpdate>, event: ClientEvent) -> Result<(), ()> {
    let update = match event {
        ClientEvent::Login { entity_id, .. } => NetUpdate::LoggedIn { entity_id },
        ClientEvent::Chat { text, .. } => NetUpdate::Chat(text.to_plain_string()),
        ClientEvent::Disconnect { reason } => {
            let _ = tx.send(NetUpdate::Disconnected(reason.to_plain_string()));
            return Err(());
        }
        ClientEvent::HealthChanged { health, food, .. } => NetUpdate::Health { health, food },
        ClientEvent::Death { .. } => NetUpdate::Death,
        // §12.24: the shell treats `ChunkLoaded` as a *dirty-region signal* and
        // ignores any payload — the ruling is that decoded chunks live in a
        // client-owned `World` that consumers query, not in the (bounded,
        // backpressuring) event stream. `impl-world` has since widened this
        // event to also carry `column`; we deliberately do not consume it, both
        // to honour the ruling and to stay robust if that field is reverted.
        ClientEvent::ChunkLoaded { pos, .. } => NetUpdate::Chunk { x: pos.x, z: pos.z },
        // Everything else (keep-alive, entities, time, teleport, player list,
        // chunk unloads) isn't needed by the shell yet.
        _ => return Ok(()),
    };
    tx.send(update).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_unique_per_call() {
        // The shell relies on a fresh name per run: offline UUIDs derive from the
        // *name*, so reusing a dead shared player blacks out every later join.
        // Uniqueness is the load-bearing property, not any particular prefix.
        let a = unique_username();
        let b = unique_username();
        assert_ne!(a, b, "two runs must not collide on a username");
        assert!(
            !a.is_empty() && a.len() <= 16,
            "not a valid Minecraft username length: {a:?}"
        );
    }

    #[test]
    fn poll_is_empty_before_any_events() {
        // Connecting to a dead port yields an error update eventually, but poll
        // right away should simply be empty (non-blocking).
        let client = NetClient::connect("127.0.0.1".into(), 1, 776);
        let _ = client.poll();
    }

    #[test]
    fn loopback_captures_sent_actions_in_order() {
        use lodestone_client::{ClientAction, Rotation, Vec3};
        let (client, actions) = NetClient::loopback();
        let a = ClientAction::Move {
            pos: Vec3::new(1.0, 2.0, 3.0),
            rotation: Rotation::new(45.0, -10.0),
            on_ground: true,
        };
        let b = ClientAction::SwingArm {
            hand: lodestone_client::Hand::Main,
        };
        client.send_action(a.clone());
        client.send_action(b.clone());
        assert_eq!(actions.try_recv().unwrap(), a);
        assert_eq!(actions.try_recv().unwrap(), b);
        assert!(actions.try_recv().is_err());
    }
}
