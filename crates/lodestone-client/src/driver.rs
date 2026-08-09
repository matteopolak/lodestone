//! The connection driver: executes adapter directives against a [`Connection`].

use std::collections::HashMap;
use std::time::Duration;

use lodestone_game::chat_ack::{LastSeenTracker, MessageSignature};
use lodestone_model::{
    AdapterError, ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile,
    PackedMessageSignature, ResourceKey, ResourcePackResponseKind, ServerAddress, VersionAdapter,
};
use lodestone_net::{Connection, NetError, Transport};
#[cfg(not(target_arch = "wasm32"))]
use lodestone_net::{generate_shared_secret, rsa_encrypt};
use tokio::sync::{mpsc, oneshot};

use crate::config::{KeepAlivePolicy, PlayerLoadedPolicy, RespawnPolicy};
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
    player_loaded: PlayerLoadedPolicy,
    read_timeout: Option<Duration>,
    profile: LoginProfile,
    server: ServerAddress,
    /// Vanilla's 20-slot last-seen tracker for signed chat. The driver drives
    /// its two flush triggers (burst valve + tick) and transmits the resulting
    /// [`ClientAction::ChatAck`] so the server's pending-message list drains and
    /// never reaches the 4096 disconnect ceiling. Stateful and session-long,
    /// which is why it lives here and not in the stateless adapter. Version-free:
    /// it only advances when the adapter feeds signed chat, and only reaches the
    /// wire when the adapter encodes a `chat_ack` packet (older versions encode
    /// `None`).
    chat_tracker: LastSeenTracker,
    /// Latch for the automatic `player_loaded` signal. Vanilla ignores our
    /// movement until its per-join/-respawn `clientLoadedTimeoutTimer` elapses
    /// unless the client zeroes it early with `player_loaded`. Armed on entering
    /// the world (`Login`) and re-armed on `Death` (the server re-seeds the timer
    /// on respawn); consumed by the first placement `TeleportPlayer`, so the
    /// signal fires exactly once per load-epoch. Version-free: it only reaches
    /// the wire when the adapter encodes a `player_loaded` packet (older versions
    /// encode `None`).
    awaiting_player_load: bool,
    /// The authenticated Microsoft/Minecraft session to prove ownership with
    /// during a `Directive::BeginEncryption { should_authenticate: true, .. }`
    /// (issue #65), or `None` for an offline-mode connection. Offline
    /// connections that hit an online-mode server fail fast with
    /// [`ClientError::OnlineModeSessionRequired`] rather than completing the
    /// crypto handshake and only then failing the session-server join.
    #[cfg(not(target_arch = "wasm32"))]
    auth_session: Option<lodestone_auth::Session>,
    /// `(account, detail)` when the caller had an account selected and could not
    /// resolve a session for it — see
    /// [`crate::ClientBuilder::online_session_unavailable`]. Read only on the
    /// `auth_session.is_none()` path, to choose
    /// [`ClientError::OnlineModeSessionUnavailable`] over
    /// [`ClientError::OnlineModeSessionRequired`]. Never consulted otherwise: a
    /// resolved session makes it irrelevant, and an offline-mode server never
    /// reaches the check at all.
    #[cfg(not(target_arch = "wasm32"))]
    auth_unavailable: Option<(String, String)>,
    /// The HTTP client the session-server `join` call goes through. Built once
    /// per driver rather than per join attempt (there is at most one per
    /// session anyway, but a fresh `reqwest::Client` per call would rebuild
    /// its connection pool/TLS config for no reason).
    #[cfg(not(target_arch = "wasm32"))]
    http: reqwest::Client,
    /// Whether an opening `Directive::BundleDelimiter` (issue #299) has been
    /// seen with no closing one yet. While `true`, directives decoded from
    /// further packets are diverted into `bundle_buffer` instead of running
    /// immediately, so the shell never observes a bundle half-applied.
    bundling: bool,
    /// Directives decoded while `bundling` is `true`, released as one batch
    /// to [`Driver::execute`] on the closing delimiter. See
    /// [`Driver::absorb_bundle`].
    bundle_buffer: Vec<Directive>,
    /// Cookies this connection has been asked to persist, keyed by
    /// [`lodestone_model::ClientEvent::CookieStored`]'s `key` (issue #291).
    /// Consulted, never the network, on a matching
    /// [`lodestone_model::ClientEvent::CookieRequested`] — see
    /// [`Driver::execute`]'s `Directive::Emit` arm, which mirrors vanilla's
    /// own `ClientCommonPacketListenerImpl.serverCookies`: an in-memory map
    /// with no persistence and no UI, answered immediately with whatever is
    /// on hand (or nothing).
    cookies: HashMap<ResourceKey, Vec<u8>>,
}

/// The client brand announced on entering Configuration, matching vanilla's
/// `minecraft:brand` custom-payload default. A real client is free to advertise
/// its own brand; this vanilla-compatible string keeps headless bots
/// indistinguishable from the reference client.
const CLIENT_BRAND: &str = "vanilla";

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
        player_loaded: PlayerLoadedPolicy,
        read_timeout: Option<Duration>,
        profile: LoginProfile,
        server: ServerAddress,
        #[cfg(not(target_arch = "wasm32"))] auth_session: Option<lodestone_auth::Session>,
        #[cfg(not(target_arch = "wasm32"))] auth_unavailable: Option<(String, String)>,
    ) -> Self {
        // reqwest is built with `rustls-no-provider` (issue #446), which leaves
        // the rustls crypto provider for the application to choose. The
        // `reqwest::Client::new()` below PANICS if none is installed, and that is
        // a *runtime* panic no `cargo check` can see — so the install sits
        // immediately next to the construction it protects rather than in some
        // distant `main`, which also means every test binary reaching here is
        // covered. Idempotent; see `lodestone_auth::tls`.
        #[cfg(not(target_arch = "wasm32"))]
        lodestone_auth::install_crypto_provider();

        Self {
            conn,
            adapter,
            state: ConnectionState::Handshaking,
            read_model,
            events,
            keep_alive,
            respawn,
            player_loaded,
            read_timeout,
            profile,
            server,
            chat_tracker: LastSeenTracker::vanilla(),
            awaiting_player_load: false,
            #[cfg(not(target_arch = "wasm32"))]
            auth_session,
            #[cfg(not(target_arch = "wasm32"))]
            auth_unavailable,
            #[cfg(not(target_arch = "wasm32"))]
            http: reqwest::Client::new(),
            bundling: false,
            bundle_buffer: Vec::new(),
            cookies: HashMap::new(),
        }
    }

    /// Runs the session to completion, returning why it ended — and, when it
    /// ended badly, **saying so on the event stream** before the stream closes.
    ///
    /// # Why the emit lives here and not at each failure site
    ///
    /// [`SessionOutcome::Failed`] is returned from a dozen places in
    /// [`Self::run_session`] and from every `Step::Stop` [`Self::execute`] can
    /// produce, and the *only* consumer able to read it is one that can call
    /// [`crate::ClientHandle::join`] — which takes the handle by value, so a
    /// shell holding an `Arc<ClientHandle>` structurally cannot. What that
    /// consumer sees is the channel closing, which is byte-for-byte what a
    /// clean [`SessionOutcome::ServerClosed`] looks like; the shell used to
    /// synthesise `"stream closed"` for both and the real cause reached only
    /// the log.
    ///
    /// Wrapping the whole session in one place makes that structural rather
    /// than a habit: a failure path added later cannot forget to emit, because
    /// there is nowhere else for it to return through. The one failure this
    /// cannot cover is [`crate::error::ClientError::DriverPanicked`], which is
    /// synthesised by `crate::spawn` from a join error — by then this function
    /// has already unwound and there is no sender left.
    pub(crate) async fn run(
        mut self,
        actions: mpsc::UnboundedReceiver<ClientAction>,
        shutdown: oneshot::Receiver<()>,
    ) -> SessionOutcome {
        let outcome = self.run_session(actions, shutdown).await;
        if let SessionOutcome::Failed(error) = &outcome {
            // `let _`: a consumer that has already dropped its receiver is not
            // an error, and there is nothing left to do about it either way.
            let _ = self
                .events
                .send(ClientEvent::SessionFailed {
                    reason: error.cause_chain(),
                })
                .await;
        }
        outcome
    }

    /// The session loop itself. See [`Self::run`] for why it is wrapped.
    async fn run_session(
        &mut self,
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
                                    // Issue #299: hold anything decoded inside a
                                    // bundle back from `execute` until the
                                    // closing delimiter, so the shell only ever
                                    // sees a bundle applied whole.
                                    let ready = self.absorb_bundle(directives);
                                    if let Step::Stop(outcome) = self.execute(ready).await {
                                        return *outcome;
                                    }
                                }
                                Err(AdapterError::Decode(message)) => {
                                    // Fail-open on decode errors. Each packet is
                                    // length-framed by the transport, so a
                                    // payload we cannot parse never desyncs the
                                    // next packet — dropping it keeps the session
                                    // alive. This matters because the wire is
                                    // forward-compatible and open-ended (item
                                    // data components, entity metadata, …): a
                                    // client that dies on the first unrecognised
                                    // structure turns every future server-side
                                    // addition into an outage. Genuinely
                                    // unrecoverable errors (unsupported feature
                                    // or state) still fall through and end it.
                                    tracing::error!(
                                        packet_id,
                                        %message,
                                        "dropping undecodable packet and continuing session",
                                    );
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

    /// Splits a packet's decoded directives around `Directive::BundleDelimiter`
    /// boundaries (issue #299), returning only the directives that should run
    /// now.
    ///
    /// Vanilla's own client pipeline (`BundlerInfo.java`) collects every
    /// packet between an opening and closing `minecraft:bundle_delimiter`
    /// into one `BundlePacket` and applies it as a single atomic step, most
    /// commonly around a batch of entity add/move/remove packets on chunk
    /// load — the point is that the client's game loop never observes a tick
    /// where only some of the batch has landed. Our transport already frames
    /// every physical packet independently of bundling (the delimiter itself
    /// decodes to a real, harmless no-op either way — see the adapter's own
    /// arm), so nothing about *decoding* was ever at risk; what was missing
    /// is this atomicity. Each bundled packet still arrives as its own read,
    /// so the fix is to defer execution — most importantly
    /// [`Directive::Emit`], the only kind a bundle in practice carries —
    /// rather than run every packet's directives the moment it is decoded.
    /// Bundling is `false` outside a bundle, so the common case (no
    /// delimiter in the batch) is a single `push` per directive with no
    /// buffering at all.
    fn absorb_bundle(&mut self, directives: Vec<Directive>) -> Vec<Directive> {
        let mut ready = Vec::with_capacity(directives.len());
        for directive in directives {
            if matches!(directive, Directive::BundleDelimiter) {
                if self.bundling {
                    // Closing delimiter: release everything buffered since
                    // the opening one as one batch.
                    self.bundling = false;
                    ready.append(&mut self.bundle_buffer);
                } else {
                    self.bundling = true;
                }
                continue;
            }
            if self.bundling {
                self.bundle_buffer.push(directive);
            } else {
                ready.push(directive);
            }
        }
        ready
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
                    let entering_configuration = next == ConnectionState::Configuration;
                    self.state = next;
                    if entering_configuration {
                        // Announce our brand on entering Configuration, as
                        // vanilla does. Protocol hygiene with no game/UI input;
                        // the adapter maps it to the state-appropriate packet and
                        // versions without a Configuration state never reach here.
                        if let Step::Stop(outcome) = self
                            .write_auto_action(ClientAction::SendBrand {
                                brand: CLIENT_BRAND.to_owned(),
                            })
                            .await
                        {
                            return Step::Stop(outcome);
                        }
                    }
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
                Directive::BeginEncryption {
                    server_id,
                    public_key,
                    verify_token,
                    should_authenticate,
                } => {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        if let Step::Stop(outcome) = self
                            .begin_encryption(server_id, public_key, verify_token, should_authenticate)
                            .await
                        {
                            return Step::Stop(outcome);
                        }
                    }
                    #[cfg(target_arch = "wasm32")]
                    {
                        // No wasm32 story exists yet (see `lodestone-net::crypto`'s
                        // and `lodestone-auth`'s docs): `rsa`/`rand`/the session-
                        // server HTTP call are all native-only. A browser build
                        // reaching this directive at all would mean a version
                        // adapter is being used somewhere it structurally cannot
                        // complete the handshake; log and drop rather than panic.
                        let _ = (server_id, public_key, verify_token, should_authenticate);
                        tracing::warn!(
                            "online-mode encryption is not supported on wasm32; ignoring BeginEncryption"
                        );
                    }
                }
                // `Directive` is `#[non_exhaustive]` (crosses the
                // `lodestone-model` crate boundary), so a wildcard is required
                // even though every variant that exists today is named above.
                // This is exactly the arm that used to silently swallow
                // `BeginEncryption` before this change — now it can only ever
                // catch a variant added to `lodestone-model` after this crate
                // was last updated, which is worth a loud warning.
                other => {
                    tracing::warn!(?other, "ignoring unknown directive variant");
                }
            }
        }
        Step::Continue
    }

    /// Drives the online-mode encryption handshake a `Directive::BeginEncryption`
    /// carries (issue #65): generate the shared secret, RSA-wrap it and the
    /// verify token, hand the ciphertext to the adapter to frame the reply,
    /// send that reply in the clear, flip the connection's cipher on, then —
    /// only if the server asked for it — prove ownership to the session
    /// server via [`lodestone_auth::join_server`].
    ///
    /// Ordering matters and mirrors [`Connection::enable_encryption`]'s own
    /// contract: the `EncryptionResponse` packet must reach the wire
    /// *before* the cipher is enabled (the server switches its cipher on the
    /// instant it accepts that packet, so everything after must already be
    /// enciphered on our side too).
    #[cfg(not(target_arch = "wasm32"))]
    async fn begin_encryption(
        &mut self,
        server_id: String,
        public_key: Vec<u8>,
        verify_token: Vec<u8>,
        should_authenticate: bool,
    ) -> Step {
        // Fail fast, before spending a round trip on crypto the server will
        // reject anyway: an offline profile has nothing to prove ownership
        // with, and completing the handshake first would only trade a clear
        // client-side error for the server's generic "unverified username"
        // disconnect (see `lodestone-net`'s `online_handshake` test for what
        // that looks like from the other side of exactly this gap).
        //
        // Two distinct errors, not one: the check fires for a player who has
        // never signed in *and* for a player whose saved session went stale, and
        // reporting both as "no session was configured" is what made a working
        // account switcher look like a broken build. `auth_unavailable` is set
        // only on the second, by the caller that tried and failed to resolve it.
        if should_authenticate && self.auth_session.is_none() {
            let error = match self.auth_unavailable.take() {
                Some((account, detail)) => {
                    ClientError::OnlineModeSessionUnavailable { account, detail }
                }
                None => ClientError::OnlineModeSessionRequired,
            };
            return Step::Stop(Box::new(SessionOutcome::Failed(error)));
        }

        let secret = generate_shared_secret();
        let enc_secret = match rsa_encrypt(&public_key, &secret) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Transport(
                    error,
                ))));
            }
        };
        let enc_token = match rsa_encrypt(&public_key, &verify_token) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Transport(
                    error,
                ))));
            }
        };

        // The adapter owns only the version-specific packet id and byte-array
        // framing (`docs/`); it performs no crypto and no I/O.
        let directive = match self.adapter.build_encryption_response(&enc_secret, &enc_token) {
            Ok(directive) => directive,
            Err(error) => {
                return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Adapter(
                    error,
                ))));
            }
        };
        let Directive::Send { packet_id, payload } = directive else {
            tracing::error!(
                ?directive,
                "build_encryption_response returned a non-Send directive"
            );
            return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Adapter(
                AdapterError::Unsupported(
                    "build_encryption_response must return Directive::Send".to_owned(),
                ),
            ))));
        };

        // Cleartext: written before the cipher is enabled below.
        if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
            return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Transport(
                error,
            ))));
        }
        if let Err(error) = self.conn.enable_encryption(&secret) {
            return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Transport(
                error,
            ))));
        }

        if should_authenticate {
            // `.expect()` is safe: the early return above guarantees `Some`
            // whenever `should_authenticate` is true.
            let session = self
                .auth_session
                .as_ref()
                .expect("should_authenticate implies auth_session is Some (checked above)");
            let hash = lodestone_auth::server_hash(&server_id, &secret, &public_key);
            if let Err(error) = lodestone_auth::join_server(&self.http, session, &hash).await {
                return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Auth(error))));
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
        // Automatic protocol responses the driver injects in reaction to an
        // event, written in order before the event is surfaced. A single event
        // can produce more than one: a keep-alive both answers the heartbeat and
        // is the tick that flushes any pending chat acknowledgement.
        let mut auto_actions: Vec<ClientAction> = Vec::new();
        // Set by `TransferRequested` below; checked after the event is
        // forwarded so a caller still observes it before the session ends.
        let mut transfer: Option<SessionOutcome> = None;

        match &event {
            ClientEvent::KeepAlive { id } => {
                if self.keep_alive.is_automatic() {
                    auto_actions.push(ClientAction::KeepAliveResponse { id: *id });
                }
                // Tick flush (vanilla's `sendChatAcknowledgement`, called on the
                // client tick). The driver has no client tick of its own, so the
                // keep-alive — the server's regular heartbeat — is the tick
                // surrogate. This is deliberately independent of the keep-alive
                // *policy*: even a bot suppressing auto keep-alive responses must
                // still drain the server's pending-chat list.
                if let Some(offset) = self.chat_tracker.take_acknowledgement() {
                    auto_actions.push(ClientAction::ChatAck { offset });
                }
            }
            // Vanilla answers a server-initiated ping challenge with a `pong`
            // unconditionally, with no user-facing policy to suppress it —
            // `ClientCommonPacketListenerImpl.handlePing` just calls
            // `this.send(new ServerboundPongPacket(packet.getId()))`
            // (`ClientCommonPacketListenerImpl.java:149-152`), unlike
            // `handleKeepAlive` a few lines above it, which goes through
            // `sendWhen` gated on `!RenderSystem.isFrozenAtPollEvents()`. That is
            // why this arm is unconditional where `KeepAlive` above is gated on
            // `self.keep_alive`. Before this arm existed, the packet decoded
            // cleanly into `ClientEvent::Ping` and reached no consumer and no
            // `ClientAction::PongResponse` producer, so the server never saw a
            // reply — the same outbound-island shape `ClientAction::SetFlying`
            // had (it was applied locally and never sent, so the server kicked
            // us with `multiplayer.disconnect.flying`).
            ClientEvent::Ping { id } => {
                auto_actions.push(ClientAction::PongResponse { id: *id });
            }
            // A pushed pack MUST be answered, and it must be answered from here
            // rather than from the shell.
            //
            // In the **configuration** phase,
            // `ServerConfigurationPacketListenerImpl.handleResourcePackResponse`
            // only calls `finishCurrentTask` once the response's
            // `Action.isTerminal()` — so an unanswered push means configuration
            // never completes and the connection simply stalls. The shell's event
            // loop does not start until after login, so it structurally cannot
            // answer in time; a shell-side producer would be correct-looking and
            // permanently too late.
            //
            // `FailedDownload` is the honest answer for a client that applies no
            // packs, and it is deliberately not `Declined`. Two clauses decide
            // that, both read off the vanilla listeners rather than guessed:
            // `Action.isTerminal()` is `this != ACCEPTED && this != DOWNLOADED`,
            // so `FAILED_DOWNLOAD` terminates the task; and the *play*-phase
            // handler disconnects only on `DECLINED` when the pack is `required`.
            // So this reply both completes configuration and keeps us connected,
            // where `Declined` would get us kicked by any server that requires
            // its pack.
            ClientEvent::ResourcePackPushed { id, .. } => {
                auto_actions.push(ClientAction::ResourcePackResponse {
                    id: *id,
                    response: ResourcePackResponseKind::FailedDownload,
                });
            }
            ClientEvent::Login { .. } => {
                // Entering the world arms the client-loaded signal; the first
                // placement teleport that follows zeroes the server's
                // load-timeout timer so our movement stops being ignored.
                self.awaiting_player_load = true;
            }
            ClientEvent::TeleportPlayer { .. } if self.awaiting_player_load => {
                // The first teleport after entering the world (or after a
                // respawn) is the server placing us — the moment vanilla is
                // genuinely ready to be moved and sends `player_loaded`. Consume
                // the latch here regardless of policy (it tracks the load-epoch),
                // but only announce readiness when the policy is automatic; a
                // later teleport in the same epoch finds the latch disarmed and
                // falls through untouched.
                self.awaiting_player_load = false;
                if self.player_loaded.is_automatic() {
                    auto_actions.push(ClientAction::PlayerLoaded);
                }
            }
            ClientEvent::Death { .. } => {
                // The server re-seeds its load-timeout timer on respawn, so
                // re-arm `player_loaded` for the post-respawn placement teleport.
                self.awaiting_player_load = true;
                if self.respawn.is_automatic() {
                    auto_actions.push(ClientAction::Respawn);
                }
            }
            ClientEvent::Respawned { .. } => {
                // Any respawn — death, portal travel, dimension change, `/respawn`
                // — re-seeds the server's load-timeout timer, so re-arm for the
                // post-respawn placement teleport. `Death` also re-arms above, but
                // that only covers death-respawn; this covers every non-death
                // transition, which emits `Respawned` with no preceding `Death`.
                self.awaiting_player_load = true;
            }
            ClientEvent::Chat {
                ack: Some(info), ..
            } => {
                // Burst valve (vanilla's `markMessageAsProcessed`): record the
                // signed message and, if more than 64 are now pending, flush
                // immediately rather than waiting for the next tick. A filtered
                // message (`was_shown == false`) still advances the window and
                // burns an offset — skipping it would silently desync the offset
                // from the server's count.
                let signature = MessageSignature::from(info.signature.as_slice());
                if let Some(offset) = self.chat_tracker.mark_processed(signature, info.was_shown) {
                    auto_actions.push(ClientAction::ChatAck { offset });
                }
            }
            // Issue #291: vanilla's own client answers a cookie request
            // immediately from its local `serverCookies` map
            // (`ClientCommonPacketListenerImpl.handleRequestCookie`), with no
            // UI and no player input — `None` when nothing was ever stored
            // for this `key`, which the wire carries as a nullable payload
            // rather than an error. Unconditional, like `Ping`/`Pong` above,
            // not gated on any policy: there is no reason a caller would want
            // to leave a `cookie_request` unanswered.
            ClientEvent::CookieRequested { key } => {
                let payload = self.cookies.get(key).cloned();
                auto_actions.push(ClientAction::CookieResponse {
                    key: key.clone(),
                    payload,
                });
            }
            // The write side of the same map: `store_cookie` populates it, a
            // later `cookie_request` reads it back. No action to send here —
            // vanilla's `handleStoreCookie` is a plain map insert.
            ClientEvent::CookieStored { key, payload } => {
                self.cookies.insert(key.clone(), payload.clone());
            }
            // Issue #291: this used to reach no consumer at all. Vanilla
            // (`ClientPacketListener.handleTransfer`) tears the connection
            // down and opens a new one to `host:port`, carrying its cookie
            // store across (`TransferState`). The driver has no generic way
            // to open a new transport from inside itself — see
            // `SessionOutcome::Transferred`'s own doc for why — so this ends
            // the session with everything the caller needs to reconnect: the
            // target address and this session's collected cookies.
            ClientEvent::TransferRequested { host, port } => {
                transfer = Some(SessionOutcome::Transferred {
                    host: host.clone(),
                    port: *port,
                    cookies: self.cookies.clone(),
                });
            }
            ClientEvent::ChatMessageDeleted { signature } => {
                // Withdraw a still-pending entry (vanilla's `ChatScreen` calling
                // `lastSeenMessages.ignorePendingSignature` when a `delete_chat`
                // arrives), so the next acknowledgement neither reports nor
                // acknowledges a message the server has withdrawn. The adapter
                // resolves cache references to full signatures before emitting,
                // so a still-`Cached` variant here means the id was
                // unresolvable and should not have been emitted; treat it as a
                // no-op.
                if let PackedMessageSignature::Full(bytes) = signature {
                    self.chat_tracker
                        .ignore_pending(&MessageSignature::from(bytes.as_slice()));
                }
            }
            _ => {}
        }

        for action in auto_actions {
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
        if let Some(outcome) = transfer {
            return Step::Stop(Box::new(outcome));
        }
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

    /// Encodes and writes a driver-injected protocol action, mirroring the
    /// auto-response semantics in [`Self::emit`]: an unrepresentable action
    /// (`Ok(None)`) is dropped quietly, while a transport or adapter failure is
    /// fatal to the session and surfaced rather than swallowed.
    async fn write_auto_action(&mut self, action: ClientAction) -> Step {
        match self.adapter.encode_action(self.state, &action) {
            Ok(Some((packet_id, payload))) => {
                if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
                    return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Transport(
                        error,
                    ))));
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, ?action, "failed to encode automatic action");
                return Step::Stop(Box::new(SessionOutcome::Failed(ClientError::Adapter(
                    error,
                ))));
            }
        }
        Step::Continue
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
