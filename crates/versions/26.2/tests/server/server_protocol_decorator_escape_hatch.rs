//! Proves the `ServerProtocol` decorator escape hatch documented in
//! `docs/plugin-packet-decorators.md`: a wrapper around a real protocol can
//! see, drop, rewrite, or append to the packets crossing an
//! [`IntegratedServer`] in both directions, with no change to the caller's
//! [`ChunkSource`] or connection plumbing.
//!
//! Every test below drives a real [`lodestone_client`] connection against a
//! real [`V770ServerProtocol`], through [`IntegratedServer::open_in_memory_with_mobs`]
//! — the same constructor a production singleplayer join uses — so the
//! decorator is exercised exactly as an embedding crate would use it, not
//! through a stand-in.
//!
//! # Why each test has a control
//!
//! [`undecorated_protocol_broadcasts_the_players_own_chat_verbatim`] and
//! [`undecorated_protocol_sends_exactly_one_welcome_message`] establish what
//! the wire does with *no* decorator in the path. Every other test below
//! changes exactly one thing relative to one of those two controls — the
//! [`Hooks`] variant — so a passing "drop" or "rewrite" test can only be
//! explained by the decorator actually running, not by a connection that
//! never worked in the first place.
//!
//! # What this does not claim
//!
//! [`ServerProtocol::decode`] returns one [`ServerBound`] per call, not a
//! `Vec`, so there is no inbound counterpart to the outbound append test
//! below: a decorator can drop or rewrite one decoded inbound packet, but it
//! cannot turn one inbound packet into two inbound actions. Only the
//! `Vec<ServerDirective>`-returning methods ([`ServerProtocol::welcome_message`]
//! and its siblings) support append, in the outbound direction only.
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientHandle, EventStream, LoginProfile, ServerAddress};
use lodestone_core::State;
use lodestone_model::ClientEvent;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};

/// A flat, content-free chunk source. These tests are about the protocol
/// wrapper, not terrain.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// Which single behaviour, if any, [`Decorator`] deviates from plain
/// forwarding on. Kept as one struct with a mode switch, rather than one
/// struct per verb, so every test wraps the identical real protocol and the
/// diff between "control" and "decorated" is exactly this enum.
#[derive(Clone, Copy)]
enum Hooks {
    /// Forwards every call unchanged — used only to prove the decorator
    /// shape itself (a struct holding a `P: ServerProtocol` and forwarding
    /// its seven methods with no default body) is not itself what breaks
    /// anything.
    Passthrough,
    /// Outbound: never let a chat broadcast reach the wire.
    DropOutboundChat,
    /// Outbound: broadcast the message with a marker prefix instead of the
    /// real text.
    RewriteOutboundChat,
    /// Outbound: append one extra clientbound directive to the join-time
    /// welcome batch, built from the wrapped protocol's own encoder.
    AppendWelcome,
    /// Inbound: decode a client's chat packet to a no-op before the server's
    /// chat pipeline ever sees it.
    DropInboundChat,
    /// Inbound: decode a client's chat packet with its text rewritten before
    /// the server broadcasts it.
    RewriteInboundChat,
}

/// The decorator itself. `ServerProtocol` declares seven methods with no
/// default body ([`ServerProtocol::decode`], [`login_success`],
/// [`begin_configuration`], [`begin_play`], [`begin_chunk_batch`],
/// [`encode_chunk`], [`end_chunk_batch`]) — those must be forwarded here or
/// the wrapped protocol simply cannot join a client at all. Every other
/// method keeps the trait's own default unless a [`Hooks`] variant overrides
/// it below, which is the same hazard `ServerProtocol`'s own
/// `impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P>` documents:
/// a defaulted method this decorator does not forward silently answers with
/// the trait default instead of the wrapped protocol's real behaviour.
///
/// [`login_success`]: ServerProtocol::login_success
/// [`begin_configuration`]: ServerProtocol::begin_configuration
/// [`begin_play`]: ServerProtocol::begin_play
/// [`begin_chunk_batch`]: ServerProtocol::begin_chunk_batch
/// [`encode_chunk`]: ServerProtocol::encode_chunk
/// [`end_chunk_batch`]: ServerProtocol::end_chunk_batch
struct Decorator<P> {
    inner: P,
    hooks: Hooks,
}

impl<P: ServerProtocol> ServerProtocol for Decorator<P> {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        let decoded = self.inner.decode(state, packet_id, payload);
        match self.hooks {
            Hooks::DropInboundChat => {
                if matches!(decoded, ServerBound::Chat { .. }) {
                    ServerBound::Ignored
                } else {
                    decoded
                }
            }
            Hooks::RewriteInboundChat => {
                if let ServerBound::Chat {
                    message,
                    timestamp_millis,
                    salt,
                    signature,
                } = decoded
                {
                    ServerBound::Chat {
                        message: format!("[rewritten inbound] {message}"),
                        timestamp_millis,
                        salt,
                        signature,
                    }
                } else {
                    decoded
                }
            }
            _ => decoded,
        }
    }

    fn login_success(&self, username: &str, uuid: uuid::Uuid) -> Vec<ServerDirective> {
        self.inner.login_success(username, uuid)
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        self.inner.begin_configuration()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        self.inner.begin_play(view_radius)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        self.inner.begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.inner.encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        self.inner.end_chunk_batch(batch_size)
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        match self.hooks {
            Hooks::DropOutboundChat => ServerDirective::None,
            Hooks::RewriteOutboundChat => self
                .inner
                .encode_system_chat(&format!("[rewritten outbound] {message}")),
            _ => self.inner.encode_system_chat(message),
        }
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        let mut directives = self.inner.welcome_message();
        if matches!(self.hooks, Hooks::AppendWelcome) {
            directives.push(
                self.inner
                    .encode_system_chat("Appended by decorator escape hatch"),
            );
        }
        directives
    }
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Joins one real client against `protocol` and waits for spawn, so every
/// test starts from the same known-good state before it starts asserting on
/// chat traffic.
async fn join(protocol: impl ServerProtocol + 'static, name: &str) -> (ClientHandle, EventStream, IntegratedServer) {
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        AirSource,
        (-1..=1, -1..=1),
        (0, 0),
        0,
        0,
    );
    let (handle, events) =
        ClientBuilder::new(address(), profile(name), Box::new(adapter())).connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    (handle, events, server)
}

/// Drains `events` for up to `timeout`, collecting the plain text of every
/// `ClientEvent::Chat` seen.
///
/// Deliberately **not** run under `start_paused` — `open_in_memory_with_mobs`
/// spawns real background work (mob simulation, chunk generation) that a
/// paused tokio clock can race ahead of, turning a real-time-bound wait into
/// a spurious timeout; `singleplayer_chat_sender_name.rs` makes the same
/// choice for the same constructor. `timeout` is real wall-clock time here,
/// so a "nothing else arrives" control genuinely waits it out.
async fn collect_chat_lines(events: &mut EventStream, timeout: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut lines = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => lines.push(text.to_plain_string()),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    lines
}

/// **Control for the outbound and inbound chat tests below.** With no
/// decorator in the path, a player's own chat comes back exactly as typed —
/// this is the baseline every drop/rewrite test must diverge from.
#[tokio::test]
async fn undecorated_protocol_broadcasts_the_players_own_chat_verbatim() {
    let (mut handle, mut events, server) = join(V770ServerProtocol, "ControlPlayer").await;

    handle
        .chat("hello from the control run")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        lines
            .iter()
            .any(|line| line == "<ControlPlayer> hello from the control run"),
        "an undecorated protocol must broadcast the typed message verbatim; saw {lines:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Sanity check, not a verb.** [`Hooks::Passthrough`] hooks nothing, so
/// wrapping [`V770ServerProtocol`] in [`Decorator`] must behave exactly like
/// the control above — the same welcome line, the same verbatim echo. This
/// is what isolates every other test's assertion to the one [`Hooks`]
/// variant it sets: without this, a passing drop/rewrite/append test could
/// in principle be explained by the `Decorator` wrapper itself changing
/// something, rather than by the specific hook.
#[tokio::test]
async fn decorator_with_no_hook_behaves_like_the_undecorated_protocol() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::Passthrough,
        },
        "Passthrough",
    )
    .await;

    handle
        .chat("hello from the passthrough run")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        lines,
        vec![
            "Welcome to Lodestone".to_string(),
            "<Passthrough> hello from the passthrough run".to_string(),
        ],
        "a decorator with no hook must reproduce the undecorated protocol's traffic exactly"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: drop, outbound.** [`Hooks::DropOutboundChat`] turns every
/// [`ServerProtocol::encode_system_chat`] call into [`ServerDirective::None`]
/// — a plugin author's "swallow this broadcast" case. The control above
/// proves the same message reaches the wire with no decorator; this proves
/// it does not once the decorator drops it.
#[tokio::test]
async fn decorator_drops_the_outbound_chat_broadcast() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::DropOutboundChat,
        },
        "DropOutbound",
    )
    .await;

    handle
        .chat("hello from the drop test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        !lines.iter().any(|line| line.contains("hello from the drop test")),
        "the decorator must drop the outbound broadcast entirely; saw {lines:?}"
    );
    assert!(
        !handle.is_finished(),
        "dropping one directive must not kill the connection"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: rewrite, outbound.** [`Hooks::RewriteOutboundChat`] prefixes the
/// message [`ServerProtocol::encode_system_chat`] is asked to send, then
/// delegates to the wrapped protocol's own encoder — a plugin author's
/// "annotate this broadcast" case (the shape a chat-filter or profanity
/// plugin needs). The control proves the unprefixed text is what an
/// undecorated protocol sends.
#[tokio::test]
async fn decorator_rewrites_the_outbound_chat_broadcast() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::RewriteOutboundChat,
        },
        "RewriteOutbnd",
    )
    .await;

    handle
        .chat("hello from the rewrite test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    // The decorator prefixes whatever string it is asked to encode, and the
    // server already folded the sender's name into that string before
    // calling `encode_system_chat` — so the marker lands in front of the
    // whole `"<name> text"` line, not between the name and the text.
    assert!(
        lines.iter().any(|line| {
            line == "[rewritten outbound] <RewriteOutbnd> hello from the rewrite test"
        }),
        "the decorator must rewrite the outbound broadcast; saw {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "<RewriteOutbnd> hello from the rewrite test"),
        "the unrewritten text must never reach the wire; saw {lines:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Control for the append test below.** An undecorated protocol's join
/// sends exactly one chat line — `welcome_message`'s own "Welcome to
/// Lodestone" — and nothing else in the same window.
#[tokio::test]
async fn undecorated_protocol_sends_exactly_one_welcome_message() {
    let (mut handle, mut events, server) = join(V770ServerProtocol, "WelcomeControl").await;

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        lines,
        vec!["Welcome to Lodestone".to_string()],
        "an undecorated join must send exactly the one welcome line"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: append, outbound.** [`Hooks::AppendWelcome`] forwards the wrapped
/// protocol's real `welcome_message()` batch, then pushes one more
/// [`ServerDirective::Send`] built from the wrapped protocol's own
/// `encode_system_chat` — a plugin author's "announce something at join" case
/// (a MOTD-of-the-day plugin, or a disguise announcing itself). The control
/// above proves the second line does not exist without the decorator.
#[tokio::test]
async fn decorator_appends_an_outbound_directive_to_the_welcome_batch() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::AppendWelcome,
        },
        "AppendWelcome",
    )
    .await;

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        lines,
        vec![
            "Welcome to Lodestone".to_string(),
            "Appended by decorator escape hatch".to_string(),
        ],
        "the decorator must append its own line after the real welcome message"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: drop, inbound.** [`Hooks::DropInboundChat`] turns a decoded
/// [`ServerBound::Chat`] into [`ServerBound::Ignored`] before the server's
/// chat pipeline (`crate::server`'s dispatch, which matches on the decoded
/// [`ServerBound`]) ever sees it — the packet is real, sent, and received;
/// the decorator erases it at decode time. The verbatim-broadcast control
/// above is what proves a message that *did* get through would have come
/// back; here nothing does.
#[tokio::test]
async fn decorator_drops_the_inbound_chat_before_the_server_ever_sees_it() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::DropInboundChat,
        },
        "DropInbound",
    )
    .await;

    handle
        .chat("hello from the inbound drop test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("hello from the inbound drop test")),
        "a chat decoded to Ignored must never be broadcast; saw {lines:?}"
    );
    assert!(
        !handle.is_finished(),
        "dropping one inbound packet must not kill the connection"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: rewrite, inbound.** [`Hooks::RewriteInboundChat`] rewrites the
/// decoded [`ServerBound::Chat::message`] before the server's chat pipeline
/// formats and broadcasts it — the anti-cheat / chat-filter archetype the
/// audit names, expressed on the *decode* side rather than the encode side
/// exercised above. What comes back over the wire is the server broadcasting
/// text the player never typed.
#[tokio::test]
async fn decorator_rewrites_the_inbound_chat_before_the_server_broadcasts_it() {
    let (mut handle, mut events, server) = join(
        Decorator {
            inner: V770ServerProtocol,
            hooks: Hooks::RewriteInboundChat,
        },
        "RewriteInbnd",
    )
    .await;

    handle
        .chat("hello from the inbound rewrite test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        lines.iter().any(|line| {
            line == "<RewriteInbnd> [rewritten inbound] hello from the inbound rewrite test"
        }),
        "the broadcast must reflect the rewritten inbound text; saw {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "<RewriteInbnd> hello from the inbound rewrite test"),
        "the player's original, unrewritten text must never be broadcast; saw {lines:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}
