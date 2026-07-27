//! The [`ClientBuilder`] entry point.

use std::time::Duration;

use lodestone_model::{LoginProfile, ServerAddress, VersionAdapter};
use lodestone_net::{Connection, Transport};
use tokio::sync::{mpsc, oneshot};

use crate::config::{KeepAlivePolicy, RespawnPolicy};
use crate::driver::Driver;
#[cfg(not(target_arch = "wasm32"))]
use crate::error::ClientError;
use crate::handle::{ClientHandle, EventStream};

/// Default capacity of the event channel.
const DEFAULT_EVENT_BUFFER: usize = 256;

/// Builds and starts a client session.
///
/// A session is fully described by a [`ServerAddress`], a [`LoginProfile`], and
/// a boxed [`VersionAdapter`] that owns all protocol choreography. The builder
/// adds cross-cutting options (keep-alive policy, timeouts, buffering) that are
/// version-free.
#[derive(Debug)]
pub struct ClientBuilder {
    server: ServerAddress,
    profile: LoginProfile,
    adapter: Box<dyn VersionAdapter>,
    keep_alive: KeepAlivePolicy,
    respawn: RespawnPolicy,
    read_timeout: Option<Duration>,
    // Only read by the native-only `connect()` (TCP). On wasm the transport is
    // always supplied via `connect_with`, so this is intentionally unused there.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    connect_timeout: Option<Duration>,
    event_buffer: usize,
}

impl ClientBuilder {
    /// Creates a builder for the given server, identity, and protocol adapter.
    #[must_use]
    pub fn new(
        server: ServerAddress,
        profile: LoginProfile,
        adapter: Box<dyn VersionAdapter>,
    ) -> Self {
        Self {
            server,
            profile,
            adapter,
            keep_alive: KeepAlivePolicy::default(),
            respawn: RespawnPolicy::default(),
            read_timeout: None,
            connect_timeout: None,
            event_buffer: DEFAULT_EVENT_BUFFER,
        }
    }

    /// Sets the keep-alive policy. Defaults to [`KeepAlivePolicy::Automatic`].
    #[must_use]
    pub fn keep_alive_policy(mut self, policy: KeepAlivePolicy) -> Self {
        self.keep_alive = policy;
        self
    }

    /// Sets the respawn policy. Defaults to [`RespawnPolicy::Automatic`], which
    /// auto-respawns the player on death so chunk streaming resumes.
    #[must_use]
    pub fn respawn_policy(mut self, policy: RespawnPolicy) -> Self {
        self.respawn = policy;
        self
    }

    /// Sets a maximum idle time between inbound packets.
    ///
    /// When the server sends nothing for this long, the session ends with
    /// [`ClientError::Timeout`]. Defaults to no timeout.
    #[must_use]
    pub fn read_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Sets a maximum time to establish the TCP connection in [`ClientBuilder::connect`].
    ///
    /// Ignored by [`ClientBuilder::connect_with`], which is handed an already
    /// established transport. Defaults to no timeout.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the event channel capacity. Must be non-zero.
    #[must_use]
    pub fn event_buffer(mut self, capacity: usize) -> Self {
        self.event_buffer = capacity.max(1);
        self
    }

    /// Connects over TCP and starts the driver.
    ///
    /// Native-only: `wasm32` targets have no TCP stack, so the browser must
    /// supply a `ws-web` transport through [`ClientBuilder::connect_with`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] if the TCP connection cannot be
    /// established, or [`ClientError::Timeout`] if it exceeds `connect_timeout`.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect(self) -> Result<(ClientHandle, EventStream), ClientError> {
        let address = (self.server.host.clone(), self.server.port);
        let connection = match self.connect_timeout {
            Some(duration) => crate::native_time::timeout(duration, Connection::connect(address))
                .await
                .map_err(|_| ClientError::Timeout)??,
            None => Connection::connect(address).await?,
        };
        Ok(self.start(connection))
    }

    /// Starts the driver over an already established transport.
    ///
    /// This is the hermetic entry point used by tests (paired with
    /// [`lodestone_net::memory_pair`]) and by an in-process server, and it is
    /// the entry point browsers use with a `ws-web` transport.
    ///
    /// Natively this must be called from within a Tokio runtime; on `wasm32` it
    /// must be called on the browser event loop (both are already the case for
    /// the intended callers).
    #[must_use]
    pub fn connect_with<T>(self, transport: T) -> (ClientHandle, EventStream)
    where
        T: Transport + 'static,
    {
        self.start(Connection::new(transport))
    }

    /// Spawns the driver task and wires up the handle and event stream.
    fn start<T>(self, connection: Connection<T>) -> (ClientHandle, EventStream)
    where
        T: Transport + 'static,
    {
        let (events_tx, events_rx) = mpsc::channel(self.event_buffer);
        let (actions_tx, actions_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // The maintained read-model. The driver holds the sole writing clone;
        // the handle holds a reading clone for cheap queries and waits.
        let read_model = crate::state::SharedState::default();

        let driver = Driver::new(
            connection,
            self.adapter,
            read_model.clone(),
            events_tx,
            self.keep_alive,
            self.respawn,
            self.read_timeout,
            self.profile,
            self.server,
        );

        let task = crate::spawn::spawn_driver(driver.run(actions_rx, shutdown_rx));
        let handle = ClientHandle::new(actions_tx, shutdown_tx, task, read_model);
        let stream = EventStream::new(events_rx);
        (handle, stream)
    }
}
