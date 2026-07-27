//! The connection driver: executes adapter directives against a [`Connection`].

use std::time::Duration;

use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::{Connection, NetError, Transport};
use tokio::sync::{mpsc, oneshot};

use crate::config::{KeepAlivePolicy, RespawnPolicy};
use crate::error::{ClientError, SessionOutcome};
use crate::state::SharedState;

/// Outcome of a bounded packet read.
enum ReadError {
    /// The transport failed (mid-frame close, I/O error, …).
    Transport(NetError),
    /// No packet arrived within the configured read timeout (native only; the
    /// wasm reader has no timer and never produces this).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    TimedOut,
}

/// Result of executing a directive batch: keep going, or stop with an outcome.
///
/// The stop payload is boxed because [`SessionOutcome`] carries a full
/// [`lodestone_model::Text`] disconnect reason, which is a large tree; boxing
/// keeps this hot control-flow enum small.
enum Step {
    Continue,
    Stop(Box<SessionOutcome>),
}

/// Owns the connection and turns adapter directives into I/O.
///
/// The driver is the single owner of the live [`ConnectionState`]. Because both
/// inbound packet handling and outbound action encoding happen inside this one
/// task, `encode_action` is always evaluated against the current state, never a
/// stale copy — even when actions are submitted from another task.
#[derive(Debug)]
pub(crate) struct Driver<T: Transport> {
    conn: Connection<T>,
    adapter: Box<dyn VersionAdapter>,
    state: ConnectionState,
    read_model: SharedState,
    events: mpsc::Sender<ClientEvent>,
    keep_alive: KeepAlivePolicy,
    respawn: RespawnPolicy,
    read_timeout: Option<Duration>,
    profile: LoginProfile,
    server: ServerAddress,
}

impl<T: Transport> Driver<T> {
    // The driver constructor genuinely needs every collaborator it is handed
    // (connection, adapter, read-model, event sink, policies, and the login
    // profile/server the adapter's begin_login requires); grouping them into a
    // parameter struct would only move the argument list elsewhere without
    // improving clarity.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conn: Connection<T>,
        adapter: Box<dyn VersionAdapter>,
        read_model: SharedState,
        events: mpsc::Sender<ClientEvent>,
        keep_alive: KeepAlivePolicy,
        respawn: RespawnPolicy,
        read_timeout: Option<Duration>,
        profile: LoginProfile,
        server: ServerAddress,
    ) -> Self {
        Self {
            conn,
            adapter,
            state: ConnectionState::Handshaking,
            read_model,
            events,
            keep_alive,
            respawn,
            read_timeout,
            profile,
            server,
        }
    }

    /// Runs the session to completion, returning why it ended.
    pub(crate) async fn run(
        mut self,
        mut actions: mpsc::UnboundedReceiver<ClientAction>,
        mut shutdown: oneshot::Receiver<()>,
    ) -> SessionOutcome {
        // wasm32 has no tokio runtime timer, so a read timeout cannot be
        // enforced there without panicking (like a wall-clock read); it is
        // ignored, and we say so once rather than silently.
        #[cfg(target_arch = "wasm32")]
        if self.read_timeout.is_some() {
            tracing::warn!("read_timeout is unsupported on wasm32 (no runtime timer); ignoring");
        }

        // Kick off the protocol-owned login sequence.
        match self.adapter.begin_login(&self.profile, &self.server) {
            Ok(directives) => {
                tracing::debug!(count = directives.len(), "executing begin_login directives");
                if let Step::Stop(outcome) = self.execute(directives).await {
                    return *outcome;
                }
            }
            Err(error) => {
                tracing::error!(%error, "begin_login failed");
                return SessionOutcome::Failed(ClientError::Adapter(error));
            }
        }

        let read_timeout = self.read_timeout;
        let mut actions_open = true;

        loop {
            tokio::select! {
                biased;

                // Local shutdown request wins over other work.
                _ = &mut shutdown => {
                    tracing::debug!("local shutdown requested");
                    self.graceful_local_close().await;
                    return SessionOutcome::LocalClose;
                }

                // User-submitted actions. Encode-and-write failures are logged
                // but do not tear down the session; a genuinely dead transport
                // is detected by the read branch.
                maybe_action = actions.recv(), if actions_open => {
                    match maybe_action {
                        Some(action) => self.handle_action(action).await,
                        None => {
                            // All handles dropped; keep serving events until the
                            // server closes.
                            actions_open = false;
                        }
                    }
                }

                // Inbound packets drive the state machine.
                read = read_packet_timed(&mut self.conn, read_timeout) => {
                    match read {
                        Err(ReadError::TimedOut) => {
                            tracing::warn!("read timed out");
                            return SessionOutcome::Failed(ClientError::Timeout);
                        }
                        Err(ReadError::Transport(error)) => {
                            tracing::error!(%error, "transport error");
                            return SessionOutcome::Failed(ClientError::Transport(error));
                        }
                        Ok(Some((packet_id, payload))) => {
                            // Hand the adapter the client-owned world as a
                            // `WorldSink` so decoded chunks are applied in place
                            // and never travel the event channel. The write
                            // guard is dropped before directives are executed.
                            let result = {
                                let mut world = self.read_model.world_write();
                                self.adapter.handle_packet(
                                    &mut *world,
                                    self.state,
                                    packet_id,
                                    &payload,
                                )
                            };
                            match result {
                                Ok(directives) => {
                                    // The world may have gained or lost chunks;
                                    // wake world-query waiters even if the
                                    // adapter emits no notification directive.
                                    self.read_model.wake();
                                    if let Step::Stop(outcome) = self.execute(directives).await {
                                        return *outcome;
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%error, packet_id, "adapter rejected packet");
                                    return SessionOutcome::Failed(ClientError::Adapter(error));
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::debug!("server closed connection cleanly");
                            return SessionOutcome::ServerClosed;
                        }
                    }
                }
            }
        }
    }

    /// Executes a directive batch in order. Ordering is significant: a
    /// [`Directive::SetState`] only affects directives that follow it, and a
    /// [`Directive::SetCompression`] only affects packets written after it.
    async fn execute(&mut self, directives: Vec<Directive>) -> Step {
        for directive in directives {
            match directive {
                Directive::Send { packet_id, payload } => {
                    if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
                        return Step::Stop(Box::new(SessionOutcome::Failed(
                            ClientError::Transport(error),
                        )));
                    }
                }
                Directive::SetState(next) => {
                    tracing::debug!(?next, "state transition");
                    self.state = next;
                }
                Directive::SetCompression(threshold) => {
                    tracing::debug!(threshold, "set compression");
                    self.conn.set_compression(threshold);
                }
                Directive::Emit(event) => {
                    if let Step::Stop(outcome) = self.emit(event).await {
                        return Step::Stop(outcome);
                    }
                }
                Directive::Disconnect(reason) => {
                    tracing::debug!(reason = %reason.to_plain_string(), "server disconnect");
                    let _ = self
                        .events
                        .send(ClientEvent::Disconnect {
                            reason: reason.clone(),
                        })
                        .await;
                    return Step::Stop(Box::new(SessionOutcome::ServerDisconnected { reason }));
                }
                other => {
                    tracing::warn!(?other, "ignoring unknown directive variant");
                }
            }
        }
        Step::Continue
    }

    /// Surfaces an event, first auto-responding when policy requires it, and
    /// folding it into the maintained read-model.
    ///
    /// The auto-response is sent *before* the event is surfaced so a slow event
    /// consumer cannot delay (and thus risk timing out) a keep-alive. The
    /// read-model is updated synchronously — the driver never awaits while it
    /// holds the state lock — so waiters observe the new state the moment the
    /// event is processed, without stalling packet handling.
    ///
    /// [`ClientEvent::ChunkLoaded`] and [`ClientEvent::ChunkUnloaded`] are
    /// lightweight position-only notifications: the decoded chunk data has
    /// already been applied to the client-owned world by the adapter through the
    /// [`lodestone_world::WorldSink`], so the heavy payload never travels the
    /// bounded event channel. This keeps the per-event cost of the channel
    /// scalar and prevents a stalled consumer from buffering whole columns;
    /// world consumers read chunks via the handle's
    /// query and `wait_for_chunk` methods instead.
    ///
    /// [`ChunkColumn`]: lodestone_world::ChunkColumn
    async fn emit(&mut self, event: ClientEvent) -> Step {
        let auto_action = match &event {
            ClientEvent::KeepAlive { id } if self.keep_alive.is_automatic() => {
                Some(ClientAction::KeepAliveResponse { id: *id })
            }
            ClientEvent::Death { .. } if self.respawn.is_automatic() => Some(ClientAction::Respawn),
            _ => None,
        };

        if let Some(action) = auto_action {
            match self.adapter.encode_action(self.state, &action) {
                Ok(Some((packet_id, payload))) => {
                    if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
                        return Step::Stop(Box::new(SessionOutcome::Failed(
                            ClientError::Transport(error),
                        )));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(%error, ?action, "failed to encode automatic response");
                    return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Adapter(
                        error,
                    ))));
                }
            }
        }

        // Fold the (now uniformly lightweight) event into the read-model by
        // reference, then forward ownership to the event stream. Chunk data has
        // already been applied to the world through the `WorldSink`, so
        // `ChunkLoaded` / `ChunkUnloaded` are just position-only notifications
        // here.
        self.read_model.apply(&event);
        let _ = self.events.send(event).await;
        Step::Continue
    }

    /// Encodes and writes a user action. Non-fatal: unrepresentable actions are
    /// dropped and encode/write failures are logged, leaving the session alive.
    ///
    /// A [`ClientAction::Move`] is also folded into the read-model as an
    /// optimistic local prediction, so movement helpers make progress without
    /// waiting for the server to echo the position back; an authoritative
    /// [`ClientEvent::TeleportPlayer`] later overrides it if the server
    /// disagrees. This mirrors how a real client predicts its own motion.
    async fn handle_action(&mut self, action: ClientAction) {
        if let ClientAction::Move {
            pos,
            rotation,
            on_ground,
            // The read-model's local prediction tracks pose only; it has no
            // use for horizontal-collision (never rendered or queried today).
            horizontal_collision: _,
        } = &action
        {
            self.read_model
                .set_local_movement(*pos, *rotation, *on_ground);
        }

        match self.adapter.encode_action(self.state, &action) {
            Ok(Some((packet_id, payload))) => {
                if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
                    tracing::warn!(%error, "failed to write client action");
                }
            }
            Ok(None) => {
                tracing::debug!(?action, "action has no packet in current state; dropping");
            }
            Err(error) => {
                tracing::warn!(%error, ?action, "adapter rejected action; dropping");
            }
        }
    }

    /// Best-effort protocol disconnect on local shutdown.
    async fn graceful_local_close(&mut self) {
        if let Ok(Some((packet_id, payload))) = self
            .adapter
            .encode_action(self.state, &ClientAction::Disconnect)
        {
            let _ = self.conn.write_packet(packet_id, &payload).await;
        }
    }
}

/// Reads the next packet, optionally bounded by a timeout.
///
/// Native builds enforce `timeout` with a runtime timer (see `native_time`).
#[cfg(not(target_arch = "wasm32"))]
async fn read_packet_timed<T: Transport>(
    conn: &mut Connection<T>,
    timeout: Option<Duration>,
) -> Result<Option<(i32, Vec<u8>)>, ReadError> {
    let read = match timeout {
        Some(duration) => match crate::native_time::timeout(duration, conn.read_packet()).await {
            Ok(read) => read,
            Err(_) => return Err(ReadError::TimedOut),
        },
        None => conn.read_packet().await,
    };
    read.map_err(ReadError::Transport)
}

/// Reads the next packet.
///
/// On wasm32 there is no runtime timer, so `timeout` is ignored (the caller
/// warns once); the read is otherwise identical.
#[cfg(target_arch = "wasm32")]
async fn read_packet_timed<T: Transport>(
    conn: &mut Connection<T>,
    _timeout: Option<Duration>,
) -> Result<Option<(i32, Vec<u8>)>, ReadError> {
    conn.read_packet().await.map_err(ReadError::Transport)
}
