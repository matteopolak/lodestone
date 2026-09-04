//! The client twin of `server_protocol_decorator_escape_hatch.rs`: proves the
//! `VersionAdapter` decorator escape hatch documented in
//! `docs/plugin-packet-decorators.md`. A wrapper around a real adapter,
//! passed to [`ClientBuilder::new`] exactly as a headless bot would, can see
//! and manipulate both directions of the connection — inbound
//! ([`VersionAdapter::handle_packet`]) and outbound
//! ([`VersionAdapter::encode_action`]) — with no server-side change at all:
//! every test here joins a real, **undecorated** [`V770ServerProtocol`], so
//! anything a test observes is purely the client decorator's doing.
//!
//! # Why each test has a control
//!
//! [`undecorated_adapter_delivers_everything_verbatim`] establishes what an
//! undecorated [`V770Adapter`] does with no wrapper in the path: the join-time
//! welcome line arrives unmodified, and a player's own chat comes back
//! exactly as typed. Every other test changes exactly one thing relative to
//! that baseline — the [`Hooks`] variant — so a passing drop/rewrite/append
//! test can only be explained by the decorator actually running.
//!
//! # The asymmetry this uncovers
//!
//! [`VersionAdapter::handle_packet`] returns `Vec<Directive>` — a batch — so
//! its inbound direction supports all three verbs, exactly like
//! [`ServerProtocol::welcome_message`] does on the server's outbound side.
//! [`VersionAdapter::encode_action`] returns `Result<Option<(i32, Vec<u8>)>,
//! AdapterError>` — at most **one** packet — so its outbound direction
//! supports drop and rewrite but structurally cannot append: there is no way
//! to return two packets from one call. This is the mirror image of the
//! server decorator's own limit (`ServerProtocol::decode` returns one
//! `ServerBound`, not a `Vec`, so *its* inbound direction cannot append
//! either). Read together, the rule is: whichever direction's method returns
//! a batch supports append; whichever returns a single value does not.
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientHandle, EventStream, LoginProfile, ServerAddress};
use lodestone_model::{
    AdapterError, ChatKind, ClientAction, ClientEvent, ConnectionState, Directive, Text,
    VersionAdapter, WorldSink,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v26_2::{V770ServerProtocol, adapter};

/// A flat, content-free chunk source. These tests are about the adapter
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
/// forwarding on.
#[derive(Clone, Copy, Debug)]
enum Hooks {
    /// Forwards every call unchanged.
    Passthrough,
    /// Inbound: drop every `ClientEvent::Chat` directive before the
    /// application ever sees it.
    DropInboundChat,
    /// Inbound: rewrite every `ClientEvent::Chat` directive's text before the
    /// application sees it.
    RewriteInboundChat,
    /// Inbound: append one synthetic `ClientEvent::Chat` directive whenever
    /// the wrapped adapter's own decode produces one — a chat event the real
    /// server never sent.
    AppendInboundChat,
    /// Outbound: never let `ClientAction::SendChat` reach the wire.
    DropOutboundChat,
    /// Outbound: rewrite `ClientAction::SendChat`'s text before the wrapped
    /// adapter encodes it.
    RewriteOutboundChat,
}

/// The decorator itself. `VersionAdapter` declares seven methods with no
/// default body (`protocol_version`, `minecraft_versions`, `supports`,
/// `begin_login`, `handle_packet`, `encode_action`,
/// `build_encryption_response`) — those must be forwarded here or the
/// wrapped adapter cannot join a server at all. Every other method (the
/// block/entity/item data queries) keeps the trait's own default, which is
/// the same forwarding hazard `ServerProtocol`'s decorator doc names: a
/// method this wrapper does not forward silently answers with the trait's
/// default instead of the wrapped adapter's real data.
#[derive(Debug)]
struct Decorator<A> {
    inner: A,
    hooks: Hooks,
}

/// One chat directive, rewritten to carry `text` instead of its own.
fn with_text(directive: Directive, text: Text) -> Directive {
    match directive {
        Directive::Emit(ClientEvent::Chat { kind, sender, ack, .. }) => {
            Directive::Emit(ClientEvent::Chat { text, kind, sender, ack })
        }
        other => other,
    }
}

fn is_chat(directive: &Directive) -> bool {
    matches!(directive, Directive::Emit(ClientEvent::Chat { .. }))
}

impl<A: VersionAdapter> VersionAdapter for Decorator<A> {
    fn protocol_version(&self) -> i32 {
        self.inner.protocol_version()
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        self.inner.minecraft_versions()
    }

    fn supports(&self, protocol: i32) -> bool {
        self.inner.supports(protocol)
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        self.inner.begin_login(profile, server)
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let directives = self.inner.handle_packet(world, state, packet_id, payload)?;
        Ok(match self.hooks {
            Hooks::DropInboundChat => directives.into_iter().filter(|d| !is_chat(d)).collect(),
            Hooks::RewriteInboundChat => directives
                .into_iter()
                .map(|d| {
                    if is_chat(&d) {
                        let rewritten = if let Directive::Emit(ClientEvent::Chat { text, .. }) = &d
                        {
                            Text::literal(format!(
                                "[rewritten by client decorator] {}",
                                text.to_plain_string()
                            ))
                        } else {
                            unreachable!()
                        };
                        with_text(d, rewritten)
                    } else {
                        d
                    }
                })
                .collect(),
            Hooks::AppendInboundChat => {
                let append_one = directives.iter().any(is_chat);
                let mut directives = directives;
                if append_one {
                    directives.push(Directive::Emit(ClientEvent::Chat {
                        text: Text::literal("Appended by client decorator"),
                        kind: ChatKind::System,
                        sender: None,
                        ack: None,
                    }));
                }
                directives
            }
            _ => directives,
        })
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        match (self.hooks, action) {
            (Hooks::DropOutboundChat, ClientAction::SendChat { .. }) => Ok(None),
            (Hooks::RewriteOutboundChat, ClientAction::SendChat { text }) => {
                let rewritten = ClientAction::SendChat {
                    text: format!("[rewritten by client decorator] {text}"),
                };
                self.inner.encode_action(state, &rewritten)
            }
            _ => self.inner.encode_action(state, action),
        }
    }

    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        self.inner
            .build_encryption_response(encrypted_secret, encrypted_token)
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

/// Joins one real client, wrapping [`adapter()`] in `wrap`, against a real,
/// **undecorated** [`V770ServerProtocol`] — so anything a test observes is
/// purely the client-side decorator's doing.
async fn join(
    wrap: impl FnOnce(lodestone_v26_2::V770Adapter) -> Box<dyn VersionAdapter>,
    name: &str,
) -> (ClientHandle, EventStream, IntegratedServer) {
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        AirSource,
        (-1..=1, -1..=1),
        (0, 0),
        0,
        0,
    );
    let (handle, events) =
        ClientBuilder::new(address(), profile(name), wrap(adapter())).connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    (handle, events, server)
}

/// Drains `events` for up to `timeout`, collecting the plain text of every
/// `ClientEvent::Chat` seen. Not run under `start_paused`, for the same
/// reason `server_protocol_decorator_escape_hatch.rs` gives.
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

/// **Control.** With no decorator in the path, the join-time welcome line
/// arrives unmodified and a player's own chat comes back exactly as typed.
#[tokio::test]
async fn undecorated_adapter_delivers_everything_verbatim() {
    let (mut handle, mut events, server) =
        join(|inner| Box::new(inner), "ControlClient").await;

    let welcome = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        welcome,
        vec!["Welcome to Lodestone".to_string()],
        "an undecorated adapter must deliver the welcome line verbatim"
    );

    handle
        .chat("hello from the client control run")
        .expect("client still connected");
    let echoed = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        echoed
            .iter()
            .any(|line| line == "<ControlClient> hello from the client control run"),
        "an undecorated adapter must send and receive chat verbatim; saw {echoed:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Sanity check, not a verb.** [`Hooks::Passthrough`] hooks nothing, so
/// wrapping [`adapter()`] in [`Decorator`] must behave exactly like the
/// control above.
#[tokio::test]
async fn decorator_with_no_hook_behaves_like_the_undecorated_adapter() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::Passthrough,
            })
        },
        "Passthrough",
    )
    .await;

    handle
        .chat("hello from the passthrough run")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert_eq!(
        lines,
        vec![
            "Welcome to Lodestone".to_string(),
            "<Passthrough> hello from the passthrough run".to_string(),
        ],
        "a decorator with no hook must reproduce the undecorated adapter's traffic exactly"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: drop, outbound.** [`Hooks::DropOutboundChat`] turns every
/// `ClientAction::SendChat` into `Ok(None)` instead of delegating to the
/// wrapped adapter's encoder — the action is never turned into bytes, so the
/// server never receives it and never broadcasts an echo.
#[tokio::test]
async fn decorator_drops_the_outbound_chat_action() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::DropOutboundChat,
            })
        },
        "DropOutClient",
    )
    .await;

    handle
        .chat("hello from the drop-outbound test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("hello from the drop-outbound test")),
        "the decorator must drop the action before it becomes a packet; saw {lines:?}"
    );
    assert!(
        !handle.is_finished(),
        "dropping one outbound action must not kill the connection"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: rewrite, outbound.** [`Hooks::RewriteOutboundChat`] rewrites
/// `ClientAction::SendChat`'s text before delegating to the wrapped adapter's
/// encoder — the server only ever sees the rewritten bytes.
#[tokio::test]
async fn decorator_rewrites_the_outbound_chat_action() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::RewriteOutboundChat,
            })
        },
        "RewriteOutClnt",
    )
    .await;

    handle
        .chat("hello from the rewrite-outbound test")
        .expect("client still connected");

    let lines = collect_chat_lines(&mut events, Duration::from_secs(5)).await;
    assert!(
        lines.iter().any(|line| {
            line == "<RewriteOutClnt> [rewritten by client decorator] hello from the rewrite-outbound test"
        }),
        "the server must have received the rewritten text, not the original; saw {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "<RewriteOutClnt> hello from the rewrite-outbound test"),
        "the unrewritten text must never reach the wire; saw {lines:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: drop, inbound.** [`Hooks::DropInboundChat`] filters every
/// `ClientEvent::Chat` out of `handle_packet`'s returned batch — the server
/// really did send the welcome line, and the decorator erases it before the
/// application's event stream ever sees it.
#[tokio::test]
async fn decorator_drops_the_inbound_chat_event() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::DropInboundChat,
            })
        },
        "DropInClient",
    )
    .await;

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert!(
        lines.is_empty(),
        "the decorator must drop every inbound chat event; saw {lines:?}"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: rewrite, inbound.** [`Hooks::RewriteInboundChat`] rewrites every
/// `ClientEvent::Chat` directive's text before `handle_packet` returns it —
/// the application sees text the server never sent.
#[tokio::test]
async fn decorator_rewrites_the_inbound_chat_event() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::RewriteInboundChat,
            })
        },
        "RewriteInClnt",
    )
    .await;

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        lines,
        vec!["[rewritten by client decorator] Welcome to Lodestone".to_string()],
        "the decorator must rewrite the inbound chat event's text"
    );

    handle.shutdown();
    server.shutdown().await;
}

/// **Verb: append, inbound.** [`Hooks::AppendInboundChat`] pushes one extra
/// `ClientEvent::Chat` directive alongside any real one `handle_packet`
/// produces — a chat event the real, undecorated server never sent.
#[tokio::test]
async fn decorator_appends_an_inbound_chat_event() {
    let (mut handle, mut events, server) = join(
        |inner| {
            Box::new(Decorator {
                inner,
                hooks: Hooks::AppendInboundChat,
            })
        },
        "AppendInClnt",
    )
    .await;

    let lines = collect_chat_lines(&mut events, Duration::from_secs(2)).await;
    assert_eq!(
        lines,
        vec![
            "Welcome to Lodestone".to_string(),
            "Appended by client decorator".to_string(),
        ],
        "the decorator must append its own event after the real one"
    );

    handle.shutdown();
    server.shutdown().await;
}
