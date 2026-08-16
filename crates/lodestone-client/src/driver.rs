//! The connection driver: executes adapter directives against a [`Connection`].

use std::collections::HashMap;
use std::time::Duration;

use lodestone_game::chat_ack::{LastSeenTracker, MessageSignature};
use lodestone_model::{
    AdapterError, ClientAction, ClientEvent, ConnectionState, Directive, DimensionId, LoginProfile,
    PackedMessageSignature, ResourceKey, ResourcePackResponseKind, ServerAddress, VersionAdapter,
};
#[cfg(not(target_arch = "wasm32"))]
use lodestone_model::{Text, TextColor, TextContent, TextStyle};
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
    /// The live chat-signing session, once its key pair has been fetched from
    /// the same authenticated surface `auth_session` proves ownership with
    /// (`docs/secure-chat.md`) — `None` for offline play, or online play
    /// before the fetch on `ClientEvent::Login` completes (or if it failed;
    /// chat degrades to unsigned rather than the session dying for it).
    /// `Some` is this driver's signal to turn an outgoing
    /// [`ClientAction::SendChat`] into a signed
    /// [`ClientAction::SendSignedChat`] — see [`Self::maybe_sign_chat`].
    /// Native-only for the same reason `auth_session` is: signing needs
    /// `lodestone-auth`'s RSA, which does not build for wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    chat_session: Option<lodestone_auth::ChatSession>,
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
    /// The dimension whose decoded columns are currently in the chunk store —
    /// **an edge detector, not a second source of truth.**
    ///
    /// "Which dimension we are in" is owned by
    /// `lodestone_ecs::session::ServerDimension`, folded from `Login` and
    /// `Respawned` and readable through the handle. This field exists only so
    /// [`Driver::emit`]'s `Respawned` arm can answer a different question — "did
    /// the dimension *change*?" — because a death-respawn and a portal trip arrive
    /// on the same packet and only one of them may empty the store. An edge
    /// detector that disagrees with the source of truth is at worst a missed or
    /// repeated clear; a second copy of the identity would be a render path
    /// reading the wrong dimension forever. Same shape, and the same reasoning, as
    /// the shell's `Sim::applied_dimension`.
    ///
    /// `None` before login, which is why the first `Respawned` of a session cannot
    /// be mistaken for a change: there is nothing loaded to throw away.
    dimension: Option<DimensionId>,
}

/// The client brand announced on entering Configuration, matching vanilla's
/// `minecraft:brand` custom-payload default. A real client is free to advertise
/// its own brand; this vanilla-compatible string keeps headless bots
/// indistinguishable from the reference client.
const CLIENT_BRAND: &str = "vanilla";

/// The environment variable that opts a session in to secure (signed) chat.
///
/// **Off by default, and this is a mitigation rather than a preference.** A
/// real server kicked the repo owner with a chat-validation failure the first
/// time this client signed for real: an unsigned message a server merely marks
/// unverified is strictly better than a signed one it rejects with a
/// disconnect, so the whole path — key fetch, session announcement, and
/// per-message signing — stays dormant unless a caller deliberately turns it
/// on to work on it. Set to `1`/`true` to enable.
#[cfg(not(target_arch = "wasm32"))]
pub const SECURE_CHAT_ENV: &str = "LODESTONE_SECURE_CHAT";

/// Whether this process opts in to signing chat — see [`SECURE_CHAT_ENV`].
///
/// Read at every call site rather than cached in a `OnceLock` so a test can
/// set and clear it around one case without poisoning the rest of the binary.
#[cfg(not(target_arch = "wasm32"))]
fn secure_chat_enabled() -> bool {
    matches!(
        std::env::var(SECURE_CHAT_ENV).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes")
    )
}

/// A random per-message signing salt (`ClientAction::SendSignedChat::salt`).
///
/// No new RNG dependency: `Uuid::new_v4()`'s bytes are already a CSPRNG draw
/// (`uuid`'s `v4` feature is already on for the whole workspace), so the first
/// 8 of its 16 random bytes make a perfectly good `i64` salt without pulling
/// in a second source of randomness just for this. Native-only along with
/// every other chat-signing call site — see [`Driver::maybe_sign_chat`].
#[cfg(not(target_arch = "wasm32"))]
fn random_chat_salt() -> i64 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    i64::from_be_bytes(bytes[..8].try_into().expect("a uuid is 16 bytes"))
}

/// Signs `text` over `chat_session`'s **current** last-seen chain (drained
/// from `chat_tracker`), or `None` when it cannot — a malformed last-seen
/// entry, an exhausted signing chain (vanilla's own `i32::MAX`-message signal
/// to start a new session, unhandled here), or a signing failure — in which
/// case the caller falls back to unsigned rather than dropping the message.
///
/// **Never a stale or cached chain**: `generate_and_apply_update` both reads
/// the window *and* marks its entries no-longer-pending, matching vanilla's
/// own `LocalPlayer.sendChat`. A signature built from a reordered or stale
/// window looks fine locally and is exactly what a real server rejects
/// (`docs/secure-chat.md`) — the reason this reads the tracker itself rather
/// than taking a pre-computed window from its caller.
///
/// A free function, not a `Driver` method, so it needs no live `Connection`
/// to unit-test — see
/// `tests::sign_chat_action_signs_over_the_real_last_seen_chain_with_correct_units`.
#[cfg(not(target_arch = "wasm32"))]
fn sign_chat_action(
    text: &str,
    chat_session: &mut lodestone_auth::ChatSession,
    chat_tracker: &mut LastSeenTracker,
) -> Option<ClientAction> {
    let update = chat_tracker.generate_and_apply_update();
    let mut last_seen = Vec::with_capacity(update.last_seen.len());
    for entry in &update.last_seen {
        let Ok(bytes) = <[u8; lodestone_auth::SIGNATURE_BYTES]>::try_from(entry.as_bytes()) else {
            // Cannot happen with a v770 adapter — it only ever tracks real
            // 256-byte RSA signatures — but a malformed entry must not
            // silently sign over a truncated window; degrade to unsigned
            // rather than emit a signature the server would reject anyway.
            tracing::warn!("last-seen entry was not 256 bytes; sending unsigned");
            return None;
        };
        last_seen.push(bytes);
    }

    let timestamp_millis = lodestone_time::epoch_duration().as_millis() as i64;
    let timestamp_seconds = timestamp_millis / 1000;
    let salt = random_chat_salt();

    match chat_session.sign(text, timestamp_seconds, salt, &last_seen) {
        Ok(Some((signature, _index))) => {
            let acknowledged: [u8; 3] = update.acknowledged_bytes().try_into().unwrap_or([0u8; 3]);
            Some(ClientAction::SendSignedChat {
                text: text.to_owned(),
                timestamp_millis,
                salt,
                signature: signature.to_vec(),
                last_seen_offset: update.offset,
                acknowledged,
                checksum: update.checksum as i8,
            })
        }
        Ok(None) => {
            // Chain exhausted (`i32::MAX` messages this session) — vanilla's
            // own signal to start a new session, which nothing here does
            // yet. Falling back to unsigned keeps the message delivered
            // rather than silently dropping it.
            tracing::warn!("chat-signing chain exhausted; sending unsigned");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "chat signing failed; sending unsigned");
            None
        }
    }
}

/// Verifies one incoming signed chat message's signature against its
/// sender's announced public key (issue #283's remaining half) —
/// [`lodestone_auth::verify_signature`]'s one production call site.
///
/// `false` on any malformed input (a signature or last-seen entry that is
/// not exactly [`lodestone_auth::SIGNATURE_BYTES`] long, or a public key that
/// does not parse) rather than propagating an error: a server that sends a
/// well-formed but forged signature and one that sends garbage bytes are the
/// same case from the player's point of view — neither is trustworthy.
///
/// A free function for the same reason [`sign_chat_action`] is — it needs no
/// live `Connection` to unit-test.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn verify_chat_message(
    sender: uuid::Uuid,
    session_id: uuid::Uuid,
    public_key_der: &[u8],
    message_index: i32,
    raw_content: &str,
    timestamp_millis: i64,
    salt: i64,
    last_seen: &[Vec<u8>],
    signature: &[u8],
) -> bool {
    let Ok(signature) = <[u8; lodestone_auth::SIGNATURE_BYTES]>::try_from(signature) else {
        return false;
    };
    let mut chain = Vec::with_capacity(last_seen.len());
    for entry in last_seen {
        let Ok(bytes) = <[u8; lodestone_auth::SIGNATURE_BYTES]>::try_from(entry.as_slice()) else {
            return false;
        };
        chain.push(bytes);
    }
    let link = lodestone_auth::SignedMessageLink {
        index: message_index,
        sender,
        session_id,
    };
    // Same `/ 1000` truncation `sign_chat_action` performs on the way out —
    // the signed payload is built over epoch **seconds**; the wire (and
    // `ChatAckInfo::timestamp_millis`) carries epoch **milliseconds**.
    let timestamp_seconds = timestamp_millis / 1000;
    lodestone_auth::verify_signature(
        public_key_der,
        &link,
        raw_content,
        timestamp_seconds,
        salt,
        &chain,
        &signature,
    )
    .unwrap_or(false)
}

/// Vanilla's `chat.tag.not_secure` treatment (`ChatTrustLevel.NOT_SECURE`,
/// `GuiMessageTag.chatNotSecure`): a light-grey `[Not Secure] ` prefix ahead
/// of the message, matching `GuiMessageTag`'s own indicator colour
/// (`0xD0D0D0`) since this client has no separate per-line tag/tooltip widget
/// to draw vanilla's coloured bar in.
///
/// Vanilla also has a `MODIFIED` trust level (a signed message whose
/// displayed text no longer contains what was signed) — not reproduced here;
/// every message that is not verified reads as `NOT_SECURE`. See
/// `ChatTrustLevel.evaluate` for the finer distinction this collapses.
#[cfg(not(target_arch = "wasm32"))]
fn tag_not_secure(text: Text) -> Text {
    Text {
        content: TextContent::Literal("[Not Secure] ".to_string()),
        style: TextStyle {
            color: Some(TextColor::Rgb(0x00D0_D0D0)),
            ..TextStyle::default()
        },
        extra: vec![text],
        ..Text::default()
    }
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
            chat_session: None,
            #[cfg(not(target_arch = "wasm32"))]
            http: reqwest::Client::new(),
            bundling: false,
            bundle_buffer: Vec::new(),
            cookies: HashMap::new(),
            dimension: None,
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
    /// Empty the decoded chunk store when a `Respawned` names a **different**
    /// dimension, and do nothing at all when it does not. Returns whether the
    /// store was cleared, so a gate can assert the edge rather than infer it.
    ///
    /// # Why this lives here and not at the shell
    ///
    /// The store is written on **this** thread, by the adapter's `WorldSink`, as
    /// each chunk packet decodes. The shell's own dimension reset
    /// (`Sim::reset_for_dimension_change`) runs on the render thread when it next
    /// drains `NetClient::poll`, and by then columns for the *new* dimension can
    /// already be in the store — so a bulk clear there would delete terrain no
    /// server resends, trading leftover geometry for a permanent hole. Dropping
    /// *meshes* is safe at the shell precisely because a column still in the store
    /// re-meshes; the store itself has no such second chance, so its clear has to
    /// happen on the thread that fills it, before the next packet decodes. This
    /// call site is that point: `emit` runs from `execute`, after the per-packet
    /// world-write guard has been dropped and before the next `read_packet_timed`.
    ///
    /// # A dimension change is not a respawn
    ///
    /// The two share one packet. Everything player-scoped — inventory, XP, health,
    /// hunger, air — survives both, and nothing here touches any of it: this
    /// function's entire reach is `World`'s chunk map. What it must not do is fire
    /// on a *death*-respawn, which reports the same [`DimensionId`] and would
    /// otherwise turn every death in the game into a full terrain reload. That is
    /// what the comparison buys, and it is the whole safety argument.
    ///
    /// `self.dimension == None` (pre-login, or an adapter family whose `Login` we
    /// never saw) is treated as "no change": a clear we cannot justify is worse
    /// than one we skip, because the skipped case is the pre-existing behaviour and
    /// the unjustified one deletes terrain.
    ///
    /// # This does not make the integrated server's `forget_chunk` sweep redundant
    ///
    /// `lodestone-server` sweeps `encode_forget_chunk` over every loaded column
    /// before it sends the respawn, and that sweep is *protocol*, not client
    /// bookkeeping: it is what a real vanilla client needs to unload the old
    /// dimension, and it is the only thing that tells any other client anything at
    /// all. Deleting it because our own client now also clears locally would break
    /// every non-Lodestone client against our server. The two are belt and braces
    /// against different failures, and it is the *vanilla* server — which sends no
    /// such sweep — that this function exists for.
    fn forget_previous_dimension(&mut self, dimension: &DimensionId) -> bool {
        let changed = self
            .dimension
            .as_ref()
            .is_some_and(|before| before != dimension);
        self.dimension = Some(dimension.clone());
        if !changed {
            return false;
        }
        // Enumerated then unloaded, rather than through a `World::clear`, so this
        // stays inside `lodestone-world`'s existing public surface. Once per
        // dimension change, so the extra `Vec` is not worth a new API.
        {
            let mut world = self.read_model.world_write();
            let loaded: Vec<_> = world.iter().map(|(pos, _)| *pos).collect();
            for pos in loaded {
                world.unload(pos);
            }
        }
        // The world lost every chunk, so anything blocked on a world query must be
        // woken — the same reason the per-packet path wakes unconditionally rather
        // than relying on the adapter to emit a notification directive.
        self.read_model.wake();
        tracing::debug!(
            dimension = %dimension,
            "dimension changed: cleared the decoded chunk store"
        );
        true
    }

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
                    // A mid-session reconfigure (dimension change, resource-pack
                    // push) drops `state` back to `Configuration` while the
                    // shell's movement queue is still live, and it has no way to
                    // know that without this. Kept in lockstep here rather than
                    // read straight off `self.state`, because the shell reads it
                    // from another thread through `ClientHandle`.
                    self.read_model.set_in_play(next == ConnectionState::Play);
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
    /// carries (issue #65): generate the shared secret, and — only if the
    /// server asked for it — prove ownership to the session server via
    /// [`lodestone_auth::join_server`] *first*. Only once that succeeds (or
    /// was not required) does it RSA-wrap the secret and verify token, hand
    /// the ciphertext to the adapter to frame the reply, send that reply in
    /// the clear, and flip the connection's cipher on.
    ///
    /// Two orderings matter here, both load-bearing:
    ///
    /// * The session-server join must complete *before* the
    ///   `EncryptionResponse` packet reaches the wire, matching
    ///   `ClientHandshakePacketListenerImpl.handleHello` (`authenticateServer`
    ///   runs to completion before `setEncryption` ever sends the
    ///   `ServerboundKeyPacket`). A hosting server's own
    ///   `ServerLoginPacketListenerImpl.handleKey` starts its
    ///   `hasJoinedServer` check the instant it receives that packet, with no
    ///   wait for the client — sending it first races our own join against
    ///   the server's check of it, and losing that race reads as a genuine
    ///   unverified session on the far side (Velocity's own online-mode gate
    ///   answers exactly this with `velocity.error.online-mode-only`).
    /// * The `EncryptionResponse` packet must reach the wire *before* the
    ///   cipher is enabled locally, matching [`Connection::enable_encryption`]'s
    ///   own contract: the server switches its cipher on the instant it
    ///   accepts that packet, so everything after must already be enciphered
    ///   on our side too.
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

        // Prove ownership to the session server *before* the
        // `EncryptionResponse` ever reaches the wire — matching
        // `ClientHandshakePacketListenerImpl.handleHello`, which runs
        // `authenticateServer` (our `join_server`) to completion and only
        // then calls `setEncryption`, which is what actually sends the
        // `ServerboundKeyPacket`. That ordering is load-bearing, not
        // stylistic: a hosting server's own `ServerLoginPacketListenerImpl
        // .handleKey` starts its `hasJoinedServer` check the instant it
        // receives that packet, on its own thread, with no wait for the
        // client. Sending our response first (as this used to) races our own
        // HTTP POST to Mojang against the server's HTTP GET to the same
        // session server — and losing that race is indistinguishable from a
        // real unverified-session failure from the far side: Velocity's own
        // online-mode check answers exactly this case with
        // `velocity.error.online-mode-only`. Doing the join first, and only
        // building/sending the encryption response after it succeeds, makes
        // the join durable at the session server before the host can
        // possibly ask about it — the same guarantee vanilla's ordering
        // gives.
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
    async fn emit(&mut self, mut event: ClientEvent) -> Step {
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
                // Secure chat key refresh (`docs/secure-chat.md`'s "still not
                // built" list): `ChatKeyPair::due_refresh` was never polled
                // anywhere, so a session outliving its key's lifetime kept
                // signing with a stale key indefinitely. The keep-alive is
                // the same periodic tick the last-seen flush immediately
                // above already piggybacks on — vanilla's own
                // `AccountProfileKeyPairManager` polls on a timer with no
                // dedicated packet of its own either, so there is no "real"
                // event to hang this off instead.
                //
                // Whether vanilla keeps the same session UUID and chain
                // position across a refresh, or starts a fresh session, was
                // unread as of `docs/secure-chat.md`'s last update. This
                // takes the conservative reading: a new key pair becomes a
                // **new** `ChatSession` (fresh session id, chain reset to
                // link 0), announced exactly like the join-time one — mixing
                // an old chain position with a key the server has not yet
                // associated with it seemed the riskier of the two guesses.
                #[cfg(not(target_arch = "wasm32"))]
                if secure_chat_enabled()
                    && let Some(session) = self.auth_session.as_ref()
                    && self.chat_session.as_ref().is_some_and(|chat_session| {
                        let now_millis = lodestone_time::epoch_duration().as_millis() as i64;
                        chat_session.key_pair().due_refresh(now_millis)
                    })
                {
                    let access_token = session.access_token.clone();
                    let sender = session.profile.id;
                    match lodestone_auth::fetch_key_pair(&self.http, &access_token).await {
                        Ok(key_pair) => {
                            let chat_session = lodestone_auth::ChatSession::new(sender, key_pair);
                            auto_actions.push(ClientAction::AnnounceChatSession {
                                session_id: chat_session.session_id(),
                                expires_at_millis: chat_session.key_pair().expires_at_millis(),
                                public_key: chat_session.key_pair().public_key_der().to_vec(),
                                key_signature: chat_session.key_pair().key_signature().to_vec(),
                            });
                            self.chat_session = Some(chat_session);
                        }
                        Err(error) => {
                            // Best-effort, matching the join-time fetch's own
                            // failure handling: keep signing with the
                            // (now-stale) key rather than dropping to
                            // unsigned chat, which would trip the same
                            // last-seen-window mismatch `docs/secure-chat.md`'s
                            // "chat-validation kick" section documents for an
                            // unsigned message sent mid-session.
                            tracing::warn!(
                                %error,
                                "chat-signing key refresh failed; continuing with the stale key"
                            );
                        }
                    }
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
            ClientEvent::Login { dimension, .. } => {
                // Entering the world arms the client-loaded signal; the first
                // placement teleport that follows zeroes the server's
                // load-timeout timer so our movement stops being ignored.
                self.awaiting_player_load = true;
                // The baseline for the `Respawned` arm's comparison, recorded
                // without clearing anything: a fresh session has nothing of a
                // previous dimension in the store. Without it the first portal trip
                // of a session would compare against `None` and skip its clear.
                self.dimension = Some(dimension.clone());

                // Secure chat: fetch this account's Mojang-issued signing key
                // pair and announce the session, mirroring
                // `AccountProfileKeyPairManager`'s own join-time timing. Only
                // for an online-mode join (`auth_session` is `None` for
                // offline play, and this is native-only for the same reason
                // `auth_session` itself is — see `docs/secure-chat.md`).
                // Best-effort: a failed fetch degrades to unsigned chat for
                // the rest of the session rather than ending it, the same
                // choice `emit`'s other auto-responses make for a failure
                // that is not fatal to the connection.
                //
                // Gated off by default behind `LODESTONE_SECURE_CHAT` — see
                // [`secure_chat_enabled`]. Announcing a session commits this
                // connection to the signed path on the server's side, so the
                // mitigation has to skip the announcement too, not merely the
                // per-message signing.
                #[cfg(not(target_arch = "wasm32"))]
                if secure_chat_enabled()
                    && self.chat_session.is_none()
                    && let Some(session) = self.auth_session.as_ref()
                {
                    let access_token = session.access_token.clone();
                    let sender = session.profile.id;
                    match lodestone_auth::fetch_key_pair(&self.http, &access_token).await {
                        Ok(key_pair) => {
                            let chat_session = lodestone_auth::ChatSession::new(sender, key_pair);
                            auto_actions.push(ClientAction::AnnounceChatSession {
                                session_id: chat_session.session_id(),
                                expires_at_millis: chat_session.key_pair().expires_at_millis(),
                                public_key: chat_session.key_pair().public_key_der().to_vec(),
                                key_signature: chat_session.key_pair().key_signature().to_vec(),
                            });
                            self.chat_session = Some(chat_session);
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "chat-signing key fetch failed; chat will be sent unsigned"
                            );
                        }
                    }
                }
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
            ClientEvent::Respawned { dimension, .. } => {
                // Any respawn — death, portal travel, dimension change, `/respawn`
                // — re-seeds the server's load-timeout timer, so re-arm for the
                // post-respawn placement teleport. `Death` also re-arms above, but
                // that only covers death-respawn; this covers every non-death
                // transition, which emits `Respawned` with no preceding `Death`.
                self.awaiting_player_load = true;
                self.forget_previous_dimension(dimension);
            }
            // **Only a message that carried a signature advances the window.**
            // `ChatAckInfo::signature` is empty when the wire's optional
            // signature was absent, and `ack` is `Some` either way — the
            // decoder reports the packet, not a judgement about it.
            //
            // Both peers count the same messages or the window desyncs, and
            // both count *signed* ones only: the server calls
            // `LastSeenMessagesValidator.addPending` under
            // `if (signature != null)` in `ServerGamePacketListenerImpl`, and
            // vanilla's own client mirrors it with the same null check around
            // `ClientPacketListener.markMessageAsProcessed` in
            // `ChatListener.handlePlayerChatMessage`. Counting an unsigned
            // `PLAYER_CHAT` here — a message from a player with no chat
            // session, or this client's own message echoed back while it sends
            // unsigned — advances our offset past a server count that never
            // moved. The server then throws `ValidationException` ("Advanced
            // last seen window by N messages, but expected at most M") from
            // `applyOffset` and disconnects with
            // `multiplayer.disconnect.chat_validation_failed`.
            ClientEvent::Chat {
                ack: Some(info), ..
            } if !info.signature.is_empty() => {
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

        // Incoming signed-chat verification (issue #283's remaining half).
        // Placed after every `PlayerListUpdate` this session has already
        // folded (`self.read_model` below) so a sender's `INITIALIZE_CHAT`
        // announced on an earlier packet is visible here — the ordering the
        // wire itself guarantees (a server announces a session before
        // sending chat signed with it). `ack.verified` starts `false` (see
        // its own doc — the adapter fails closed) and is only ever raised
        // here, never lowered; an unverified message is tagged in `text`
        // itself, this client's stand-in for vanilla's separate
        // `GuiMessageTag` widget (see `tag_not_secure`'s doc for why).
        #[cfg(not(target_arch = "wasm32"))]
        if let ClientEvent::Chat {
            sender: Some(sender_id),
            ack: Some(info),
            text,
            ..
        } = &mut event
        {
            let verified = (info.signature.len() == lodestone_auth::SIGNATURE_BYTES)
                .then(|| self.read_model.chat_session_of(sender_id))
                .flatten()
                .is_some_and(|session| {
                    verify_chat_message(
                        *sender_id,
                        session.session_id,
                        &session.public_key,
                        info.message_index,
                        &info.raw_content,
                        info.timestamp_millis,
                        info.salt,
                        &info.last_seen,
                        &info.signature,
                    )
                });
            info.verified = verified;
            if !verified {
                *text = tag_not_secure(std::mem::take(text));
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

        #[cfg(not(target_arch = "wasm32"))]
        let action = self.maybe_sign_chat(action);

        match self.adapter.encode_action(self.state, &action) {
            Ok(Some((packet_id, payload))) => {
                if let Err(error) = self.conn.write_packet(packet_id, &payload).await {
                    tracing::warn!(%error, "failed to write client action");
                }
            }
            Ok(None) => {
                // The state is the whole diagnosis here: `Move` (for example)
                // has an encode arm gated on `ConnectionState::Play`, so the
                // interesting question when this fires is *which* state the
                // action was queued in — was it produced somewhere that
                // should not be producing it in this state (e.g. `Move`
                // still ticking during a 26.2 dimension-change-via-
                // configuration respawn), or is the adapter genuinely
                // missing an arm it should have. Without the state, both
                // read identically.
                tracing::debug!(
                    ?action,
                    state = ?self.state,
                    "action has no packet in current state; dropping"
                );
            }
            Err(error) => {
                tracing::warn!(%error, ?action, "adapter rejected action; dropping");
            }
        }
    }

    /// Turns a plain [`ClientAction::SendChat`] into a signed
    /// [`ClientAction::SendSignedChat`] when this session has a live
    /// `chat_session`. Every other action, and `SendChat` when there is no
    /// session (offline play, the key fetch hasn't completed, or it failed),
    /// passes through unchanged, so unsigned chat keeps working exactly as
    /// it did before this existed. `chat_session` is only ever populated
    /// behind [`secure_chat_enabled`], so with the opt-in unset this is the
    /// identity function and every message goes out unsigned. The signing itself is
    /// [`sign_chat_action`], a free function so it is unit-testable without a
    /// live `Connection` — see this module's `tests`.
    #[cfg(not(target_arch = "wasm32"))]
    fn maybe_sign_chat(&mut self, action: ClientAction) -> ClientAction {
        let ClientAction::SendChat { text } = &action else {
            return action;
        };
        let Some(chat_session) = self.chat_session.as_mut() else {
            return action;
        };
        sign_chat_action(text, chat_session, &mut self.chat_tracker).unwrap_or(action)
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

/// Unit tests for the signed-chat producer wiring
/// ([`sign_chat_action`]/[`Driver::maybe_sign_chat`]). Native-only: chat
/// signing needs `lodestone-auth`'s RSA, which does not build for wasm32 —
/// same reason every other chat-signing symbol in this file is gated.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    use super::*;

    /// The same fixed PKCS#8-DER RSA-2048 test key `lodestone-auth`'s own
    /// `chat_session` tests use (`openssl genpkey`-generated, no code or
    /// secrecy shared with anything real) — reused rather than generating a
    /// fresh key per test run, which would need `rsa`'s `rand_core` line
    /// wired through and cost real wall-clock time for no benefit here.
    const TEST_PRIVATE_KEY_DER_B64: &str = concat!(
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDtM+q+4UwoW3cZ",
        "Q8TVkfa9TfGdxpl13PlfNei77mmWz+kLCxeOXpF2hX/VXSoxj3yBjjhtGHZB59eX",
        "0VW2zw+G913ZMmtT+9phKBA9BOID4c4hNpz852wJ5sp2pFOyrrg47UTrakey9iQT",
        "+ckO4qfeMR13NTDP44cLFBwa1/ot80Fwq00xg5KHJK6WeWmjPayc+lf3FSPC+cNO",
        "aOJ3oaWK16b2LFqvzwwkl53e0yyHFgffA5AdClVJgZc7pEDScO0zLHLqe8ySrbsJ",
        "yZ9PQSTNC7cmXkJPQjlYJJ2M4/+HJRtjY/CQyP5C7sTdu/Lhn1nUawhj74Egyvg8",
        "HXeysPeZAgMBAAECggEBAMg+0ee+jupq/MpJWbvqc2Awks7dP+QuXh8whX9Rr7Xv",
        "Yw89l+9KioaCAP8AnYQlW7iLdbszsXHF5U13HWMsvjD0VzfqxoypyxvGFJ9Opfcd",
        "A0Uqs7EVNTHOshEifL4VndQBCfOrT0gXXzG15zQ3x/tdf0CJmOGHdRO3MFrBBaUP",
        "XJgVcGCWyKK9/p+uV9lolnQprotiuctX6nI5hYAX7PG1XFJlPAW5k9DLE4W31+8Y",
        "FiJgsS/WTRAsvjs7zJefGwUNE0+86ylREEmSvHWqjS6pgxf7REZed0208kTHC1P8",
        "aGP9nnrHZfiKBDtxt2usRbG00Whf9NVTOZBeC9ExKjkCgYEA948Wr8q0lFVMZ7xt",
        "u5Dx8Mvjvz2Bl5wclX27qrqeu7T3aGnP2EwVSQW5xUB/KpYpxoMFiJIy9cVqo1XT",
        "Vege7i8WsGRK+D9xpd6QEhME79nIbltmxTVP9Ue9foBev0S0QM5n1Qk6L4hKUnva",
        "dwQ1Ow6XoPejGcu2BhYzywUPrJsCgYEA9UpuTzZgMg7CVCIRH6Ze8jNP56GADXYB",
        "8BH5hSuaKO67PukLa/iqSo38w1uZSVLvNgLxts5Q+pinSglJlZ8mRrLVFI8qkcIg",
        "j/qZKpVP0mfOuBYu/DNkX0VO4nG1pBSKgT1dmUiVVAvBfgbUHeG1vVEENKh0NbSH",
        "nswL84z8XdsCgYBpnapYJWsVPa7zMvi95QDTcqkfleYMAJZRUOsX07aU7of/C+WY",
        "qh0Kol63QOUADkCUaKGbuoPzRt5QAPXA2N8ZTw2nA6LYdnjOAz4D+AlLKubP7j7S",
        "NASA6LJ3ndzOTUl5vJWf1ef1D3hl6GE0FZ+AKqGWExCKmNZ3klFWdDpTsQKBgFG9",
        "FttApHep4WoF3Czu1O7i2Hq4n6Jcs7KbWsncyMdhHnaNVCgLujuT6ynyiTcc8ufN",
        "vVyMjgGkAwMx6xp36Vpf14+9UZM23ID+IjJFhU75FrLTeZ7DRWxV/T6KY9wkmC8P",
        "EvS0ckaKkFT904uNnnFS4RLnG6qV2Se6mTT0w1hHAoGADIwcasJrU/5xnBPICA6f",
        "u43x6dk1/v+GeRLz0N0aVADsj7tInJ+7pHV1/NrHaGONJKIQ0uWIKxVdHufDmYVU",
        "KY0Oh6wzS/m5Z2tmxK24z0UJyXvAu67ETx5QUhqH63i5km9a2Au+zkwGXBBg6Bvh",
        "7kWCpm322pipbRs6hKc7klQ=",
    );

    /// Builds a throwaway [`lodestone_auth::ChatSession`] (via the test-only
    /// [`lodestone_auth::ChatKeyPair::for_tests`] fixture builder) and
    /// returns it alongside its DER-encoded public key, needed to
    /// independently re-verify what [`sign_chat_action`] produces.
    fn test_chat_session() -> (lodestone_auth::ChatSession, Vec<u8>) {
        let der = BASE64.decode(TEST_PRIVATE_KEY_DER_B64).expect("valid base64 fixture");
        let private_key = RsaPrivateKey::from_pkcs8_der(&der).expect("valid PKCS#8 DER fixture");
        let public_key = RsaPublicKey::from(&private_key);
        let public_key_der = public_key
            .to_public_key_der()
            .expect("encode test public key")
            .into_vec();
        let key_pair = lodestone_auth::ChatKeyPair::for_tests(
            private_key,
            public_key_der.clone(),
            vec![0xAA, 0xBB], // key_signature: opaque to this client, never checked here
            i64::MAX,
            i64::MAX,
        );
        let sender = uuid::Uuid::from_u128(1);
        (lodestone_auth::ChatSession::new(sender, key_pair), public_key_der)
    }

    /// The half of the discriminating pair this issue's verification section
    /// asks for that is new code: signed-in signs, against the **real**
    /// last-seen chain rather than an empty or cached one. The other half —
    /// signed-out sends unsigned — is [`Driver::maybe_sign_chat`]'s one-line
    /// early return when `chat_session` is `None`, which is exactly the path
    /// every pre-existing `SendChat` test in `tests/driver.rs` already takes
    /// (none of them ever populate `auth_session`/`chat_session`) and which
    /// stayed green across this change, so it is not re-asserted here.
    #[test]
    fn sign_chat_action_signs_over_the_real_last_seen_chain_with_correct_units() {
        let (mut chat_session, public_key_der) = test_chat_session();
        let sender = uuid::Uuid::from_u128(1);
        let session_id = chat_session.session_id();

        let mut tracker = LastSeenTracker::vanilla();
        // A message the tracker has already recorded as pending — what makes
        // "signs over the real chain" checkable: an implementation that
        // always signed over `&[]` would still produce *a* signature, just
        // not one that verifies against this entry.
        let prior = [0x7Au8; lodestone_auth::SIGNATURE_BYTES];
        tracker.add_pending(MessageSignature::from(prior.as_slice()), true);

        let action = sign_chat_action("hello", &mut chat_session, &mut tracker)
            .expect("a live session must produce a signed action");
        let ClientAction::SendSignedChat {
            text,
            timestamp_millis,
            salt,
            signature,
            last_seen_offset,
            acknowledged,
            checksum,
        } = action
        else {
            panic!("expected ClientAction::SendSignedChat");
        };

        assert_eq!(text, "hello");
        assert_eq!(last_seen_offset, 1, "one prior message was pending");
        // Exactly one bit set across the 3-byte ack window — not asserting
        // *which* bit, since `generate_and_apply_update` walks the ring
        // starting at `tail` (oldest-first), so a single tracked slot's bit
        // position depends on how many entries have been added, not on
        // being bit 0.
        let set_bits: u32 = acknowledged.iter().map(|b| b.count_ones()).sum();
        assert_eq!(set_bits, 1, "exactly the one tracked slot is acknowledged: {acknowledged:?}");
        assert_ne!(checksum, 0, "a non-empty last-seen window has a real checksum");

        // Wire unit is epoch millis, and it is a plausible "now" — re-derived
        // from the same portable clock this test itself calls, not trusted
        // blindly.
        let now_millis = lodestone_time::epoch_duration().as_millis() as i64;
        assert!(
            (now_millis - timestamp_millis).abs() < 5_000,
            "timestamp_millis should be close to now: {timestamp_millis}"
        );
        // Pairwise-distinct-units control: the signature payload's timestamp
        // is epoch *seconds*, a different value from the wire's epoch
        // *millis* field it is derived from, never the same i64 reused.
        let timestamp_seconds = timestamp_millis / 1000;
        assert_ne!(
            timestamp_seconds, timestamp_millis,
            "seconds and millis must be distinct, not the same field twice"
        );

        // Re-derive the signature independently through `verify_signature`
        // against the real link/last-seen entry, rather than only asserting
        // "a signature is present".
        let link = lodestone_auth::SignedMessageLink::root(sender, session_id);
        let signature_bytes: [u8; lodestone_auth::SIGNATURE_BYTES] =
            signature.try_into().expect("signature must be 256 bytes");
        assert!(
            lodestone_auth::verify_signature(
                &public_key_der,
                &link,
                "hello",
                timestamp_seconds,
                salt,
                &[prior],
                &signature_bytes,
            )
            .expect("verify against the real last-seen entry"),
            "signature must verify against the last-seen chain it was actually signed over"
        );

        // Transposition control: the same signature must NOT verify against
        // an empty last-seen window — proving the real chain reached the
        // payload rather than the implementation happening to sign over
        // `&[]` regardless of what the tracker held.
        assert!(
            !lodestone_auth::verify_signature(
                &public_key_der,
                &link,
                "hello",
                timestamp_seconds,
                salt,
                &[],
                &signature_bytes,
            )
            .expect("verify against an empty window"),
            "must not also verify against a different (empty) last-seen window"
        );
    }

    /// Two successive sends from the same session must land on different
    /// chain indices — signing "same text" twice must not produce the same
    /// signature, or replay/reordering on the wire would be undetectable.
    #[test]
    fn successive_signed_sends_advance_the_chain_and_differ() {
        let (mut chat_session, _public_key_der) = test_chat_session();
        let mut tracker = LastSeenTracker::vanilla();

        let first = sign_chat_action("same text", &mut chat_session, &mut tracker)
            .expect("first send must sign");
        let second = sign_chat_action("same text", &mut chat_session, &mut tracker)
            .expect("second send must sign");

        let ClientAction::SendSignedChat { signature: sig1, .. } = first else {
            panic!("expected signed chat");
        };
        let ClientAction::SendSignedChat { signature: sig2, .. } = second else {
            panic!("expected signed chat");
        };
        assert_ne!(
            sig1, sig2,
            "identical content sent twice must sign differently (chain index advanced)"
        );
    }

    /// Issue #283's discriminating pair for the *receiving* half: a message
    /// whose signature is valid, and the same message with one byte of the
    /// signature flipped — same sender, same session, same chain, same
    /// content. A `verify_chat_message` that always returned `true` would
    /// pass the first assertion and fail only the second; both must hold.
    #[test]
    fn verify_chat_message_accepts_valid_and_rejects_tampered() {
        let (mut chat_session, public_key_der) = test_chat_session();
        let sender = uuid::Uuid::from_u128(1);
        let session_id = chat_session.session_id();

        // Pairwise-distinct, non-round-number fields — the values a
        // transposed pair of same-typed adjacent arguments could hide.
        let last_seen_entry = [0x11u8; lodestone_auth::SIGNATURE_BYTES];
        let last_seen = vec![last_seen_entry.to_vec()];
        let timestamp_millis = 1_700_000_000_123i64;
        let salt = 99_887_766i64;
        let content = "hello world";

        let (signature, index) = chat_session
            .sign(content, timestamp_millis / 1000, salt, &[last_seen_entry])
            .expect("signing must succeed")
            .expect("a fresh session has chain left");

        assert!(
            verify_chat_message(
                sender,
                session_id,
                &public_key_der,
                index,
                content,
                timestamp_millis,
                salt,
                &last_seen,
                &signature,
            ),
            "the genuinely signed message must verify"
        );

        let mut tampered = signature;
        tampered[0] ^= 0xFF;
        assert!(
            !verify_chat_message(
                sender,
                session_id,
                &public_key_der,
                index,
                content,
                timestamp_millis,
                salt,
                &last_seen,
                &tampered,
            ),
            "a single flipped signature byte must fail verification"
        );
    }

    /// The unit trap `CLAUDE.md` names by name: the wire's timestamp is
    /// epoch **millis**, but the signed payload is built over epoch
    /// **seconds**. `verify_chat_message` performs the `/ 1000` internally —
    /// this proves that conversion is load-bearing by showing what happens
    /// without it, one level below `verify_chat_message` where the seam
    /// actually lives.
    #[test]
    fn verify_signature_rejects_millis_where_seconds_are_expected() {
        let (mut chat_session, public_key_der) = test_chat_session();
        let sender = uuid::Uuid::from_u128(1);
        let session_id = chat_session.session_id();
        let link = lodestone_auth::SignedMessageLink::root(sender, session_id);

        let timestamp_millis = 1_700_000_000_123i64;
        let timestamp_seconds = timestamp_millis / 1000;
        assert_ne!(
            timestamp_seconds, timestamp_millis,
            "the fixture must actually exercise two different i64 values"
        );
        let salt = 55_443_322i64;
        let content = "unit trap";

        let (signature, _index) = chat_session
            .sign(content, timestamp_seconds, salt, &[])
            .expect("signing must succeed")
            .expect("a fresh session has chain left");

        assert!(
            lodestone_auth::verify_signature(
                &public_key_der, &link, content, timestamp_seconds, salt, &[], &signature,
            )
            .expect("verify with the correct unit"),
            "the real signed unit (seconds) must verify"
        );
        assert!(
            !lodestone_auth::verify_signature(
                &public_key_der, &link, content, timestamp_millis, salt, &[], &signature,
            )
            .expect("verify with the wrong unit"),
            "millis passed where seconds belong must not verify — the two \
             values differ and only one is what was actually signed"
        );
    }
}
