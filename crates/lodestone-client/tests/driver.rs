//! Hermetic driver tests.
//!
//! Every test here uses [`lodestone_net::memory_pair`] and a hand-written fake
//! [`VersionAdapter`]; none require a real server.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{
    ChatAckInfo, ChatKind, ClientAction, ClientBuilder, ClientEvent, ConnectionState, Directive,
    KeepAlivePolicy, LoginProfile, RespawnPolicy, ServerAddress, SessionOutcome, VersionAdapter,
};
use lodestone_model::{AdapterError, GameMode, Identifier, Text};
use lodestone_net::{Connection, memory_pair};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

// --- Fake adapter -----------------------------------------------------------

/// A scriptable, deterministic [`VersionAdapter`] for tests.
///
/// It carries no version knowledge; it just maps `(state, packet_id)` to a
/// canned directive batch, and encodes a handful of actions with a stable,
/// inspectable layout so tests can assert what went on the wire.
#[derive(Debug, Default)]
struct FakeAdapter {
    begin: Vec<Directive>,
    script: HashMap<(ConnectionState, i32), Vec<Directive>>,
    fail: HashSet<(ConnectionState, i32)>,
    keepalive_resp_id: i32,
    respawn_resp_id: Option<i32>,
    calls: Arc<Mutex<Vec<(ConnectionState, i32)>>>,
}

const KEEPALIVE_RESP_ID: i32 = 0x30;
const CHAT_ID: i32 = 0x06;
const RESPAWN_RESP_ID: i32 = 0x0C;
const CHAT_ACK_ID: i32 = 0x07;

fn state_code(state: ConnectionState) -> u8 {
    match state {
        ConnectionState::Handshaking => 0,
        ConnectionState::Status => 1,
        ConnectionState::Login => 2,
        ConnectionState::Configuration => 3,
        ConnectionState::Play => 4,
    }
}

impl FakeAdapter {
    fn new() -> Self {
        Self {
            keepalive_resp_id: KEEPALIVE_RESP_ID,
            ..Self::default()
        }
    }

    fn begin(mut self, directives: Vec<Directive>) -> Self {
        self.begin = directives;
        self
    }

    /// Makes `Respawn` encode to an observable packet so auto-respawn is
    /// visible on the wire; without this it stays unrepresentable (`Ok(None)`).
    fn respawn_to(mut self, id: i32) -> Self {
        self.respawn_resp_id = Some(id);
        self
    }

    fn on(mut self, state: ConnectionState, packet_id: i32, directives: Vec<Directive>) -> Self {
        self.script.insert((state, packet_id), directives);
        self
    }

    fn fail_on(mut self, state: ConnectionState, packet_id: i32) -> Self {
        self.fail.insert((state, packet_id));
        self
    }

    fn calls(&self) -> Arc<Mutex<Vec<(ConnectionState, i32)>>> {
        Arc::clone(&self.calls)
    }
}

impl VersionAdapter for FakeAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["fake"]
    }

    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(self.begin.clone())
    }

    fn handle_packet(
        &self,
        _world: &mut dyn lodestone_model::WorldSink,
        state: ConnectionState,
        packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        self.calls.lock().unwrap().push((state, packet_id));
        if self.fail.contains(&(state, packet_id)) {
            return Err(AdapterError::Decode(format!("boom at {packet_id}")));
        }
        Ok(self
            .script
            .get(&(state, packet_id))
            .cloned()
            .unwrap_or_default())
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        match action {
            ClientAction::KeepAliveResponse { id } => {
                // Payload embeds the state at encode time so tests can prove
                // which state the response was sent under.
                let mut payload = vec![state_code(state)];
                payload.extend_from_slice(&id.to_be_bytes());
                Ok(Some((self.keepalive_resp_id, payload)))
            }
            ClientAction::SendChat { text } => Ok(Some((CHAT_ID, text.clone().into_bytes()))),
            // The standalone signed-chat acknowledgement. Vanilla's packet is a
            // single VarInt offset; the fake encodes it as `[state, offset_be]`
            // so a test can read the offset that actually went on the wire.
            ClientAction::ChatAck { offset } => {
                let mut payload = vec![state_code(state)];
                payload.extend_from_slice(&offset.to_be_bytes());
                Ok(Some((CHAT_ACK_ID, payload)))
            }
            // Encodes to a packet only when a test opts in via `respawn_to`;
            // otherwise deliberately unrepresentable, to exercise quiet dropping.
            ClientAction::Respawn => {
                Ok(self.respawn_resp_id.map(|id| (id, vec![state_code(state)])))
            }
            _ => Ok(None),
        }
    }
}

// --- Helpers ----------------------------------------------------------------

fn profile() -> LoginProfile {
    LoginProfile {
        username: "Tester".into(),
        uuid: Uuid::nil(),
    }
}

fn server() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

fn send(id: i32, bytes: &[u8]) -> Directive {
    Directive::Send {
        packet_id: id,
        payload: bytes.to_vec(),
    }
}

/// A `Directive` emitting a signed player-chat event carrying acknowledgement
/// metadata, so the driver's last-seen tracker advances. `was_shown = false`
/// models a filtered message, which vanilla still counts toward the window.
fn signed_chat(signature: Vec<u8>, was_shown: bool) -> Directive {
    Directive::Emit(ClientEvent::Chat {
        text: Text::literal("hi"),
        kind: ChatKind::Chat,
        ack: Some(ChatAckInfo {
            signature,
            global_index: 0,
            was_shown,
        }),
    })
}

fn start(
    adapter: FakeAdapter,
    policy: KeepAlivePolicy,
) -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<tokio::io::DuplexStream>,
) {
    start_with(adapter, policy, RespawnPolicy::Automatic)
}

fn start_with(
    adapter: FakeAdapter,
    keep_alive: KeepAlivePolicy,
    respawn: RespawnPolicy,
) -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<tokio::io::DuplexStream>,
) {
    let (client_io, server_io) = memory_pair();
    let (handle, events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .keep_alive_policy(keep_alive)
        .respawn_policy(respawn)
        .connect_with(client_io);
    (handle, events, Connection::new(server_io))
}

// --- Tests ------------------------------------------------------------------

/// A `SetState` after a send in the same batch must not retroactively affect
/// the send. Here the "send" is the auto keep-alive response, which is emitted
/// before the `SetState(Play)` and so must go out under the *old* state
/// (Configuration), observable in its payload on the wire.
#[tokio::test]
async fn send_happens_under_old_state_before_transition() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Configuration)])
        .on(
            ConnectionState::Configuration,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::KeepAlive { id: 42 }),
                Directive::SetState(ConnectionState::Play),
            ],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(TRIGGER, &[]).await.unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, KEEPALIVE_RESP_ID);
    // First byte is the state at send time: Configuration (3), not Play (4).
    assert_eq!(payload[0], state_code(ConnectionState::Configuration));
    assert_eq!(&payload[1..], &42i64.to_be_bytes());

    // The event is still surfaced.
    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 42 }));

    drop(handle);
}

/// `SetCompression` mid-stream must apply only to packets written after it, and
/// both halves must round-trip once a peer matches the threshold.
#[tokio::test]
async fn set_compression_applies_after_directive_only() {
    const TRIGGER: i32 = 100;
    let big = vec![7u8; 500];
    let adapter = FakeAdapter::new().begin(vec![send(0x01, &[1, 2, 3])]).on(
        ConnectionState::Handshaking,
        TRIGGER,
        vec![Directive::SetCompression(0), send(0x02, &big)],
    );

    let (handle, _events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    // Packet X, written before compression is enabled, is uncompressed.
    let (id_x, fields_x) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id_x, 0x01);
    assert_eq!(fields_x, vec![1, 2, 3]);

    // Trigger the mid-stream SetCompression + compressed send.
    peer.write_packet(TRIGGER, &[]).await.unwrap();
    peer.set_compression(0);

    let (id_y, fields_y) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id_y, 0x02);
    assert_eq!(fields_y, big);

    drop(handle);
}

/// Automatic keep-alive: the driver writes the encoded response and still
/// surfaces the event.
#[tokio::test]
async fn keep_alive_auto_responds_and_surfaces() {
    const KA: i32 = 0x50;
    let adapter = FakeAdapter::new().on(
        ConnectionState::Handshaking,
        KA,
        vec![Directive::Emit(ClientEvent::KeepAlive { id: 99 })],
    );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(KA, &[]).await.unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, KEEPALIVE_RESP_ID);
    assert_eq!(&payload[1..], &99i64.to_be_bytes());

    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 99 }));

    drop(handle);
}

/// Manual keep-alive: the event is surfaced but nothing is written.
#[tokio::test]
async fn keep_alive_manual_surfaces_but_writes_nothing() {
    const KA: i32 = 0x50;
    let adapter = FakeAdapter::new().on(
        ConnectionState::Handshaking,
        KA,
        vec![Directive::Emit(ClientEvent::KeepAlive { id: 99 })],
    );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    peer.write_packet(KA, &[]).await.unwrap();

    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 99 }));

    // No response should ever arrive.
    let nothing = tokio::time::timeout(Duration::from_millis(100), peer.read_packet()).await;
    assert!(nothing.is_err(), "expected no packet in manual mode");

    drop(handle);
}

/// Automatic respawn: on a `Death` event the driver writes the encoded respawn
/// request and still surfaces the event.
#[tokio::test]
async fn death_auto_respawns_and_surfaces() {
    const DEATH: i32 = 0x52;
    let adapter = FakeAdapter::new().respawn_to(RESPAWN_RESP_ID).on(
        ConnectionState::Handshaking,
        DEATH,
        vec![Directive::Emit(ClientEvent::Death {
            message: lodestone_model::Text::literal("You were slain"),
        })],
    );

    let (handle, mut events, mut peer) = start_with(
        adapter,
        KeepAlivePolicy::Automatic,
        RespawnPolicy::Automatic,
    );

    peer.write_packet(DEATH, &[]).await.unwrap();

    // The respawn request goes out before the event is surfaced.
    let (id, _payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, RESPAWN_RESP_ID);

    assert_eq!(
        events.recv().await,
        Some(ClientEvent::Death {
            message: lodestone_model::Text::literal("You were slain"),
        })
    );

    drop(handle);
}

/// Manual respawn: the `Death` event is surfaced but no respawn is written, so
/// a bot can implement its own death policy.
#[tokio::test]
async fn death_manual_does_not_respawn() {
    const DEATH: i32 = 0x52;
    let adapter = FakeAdapter::new().respawn_to(RESPAWN_RESP_ID).on(
        ConnectionState::Handshaking,
        DEATH,
        vec![Directive::Emit(ClientEvent::Death {
            message: lodestone_model::Text::literal("You were slain"),
        })],
    );

    let (handle, mut events, mut peer) =
        start_with(adapter, KeepAlivePolicy::Automatic, RespawnPolicy::Manual);

    peer.write_packet(DEATH, &[]).await.unwrap();

    assert_eq!(
        events.recv().await,
        Some(ClientEvent::Death {
            message: lodestone_model::Text::literal("You were slain"),
        })
    );

    let nothing = tokio::time::timeout(Duration::from_millis(100), peer.read_packet()).await;
    assert!(
        nothing.is_err(),
        "expected no respawn packet in manual mode"
    );

    drop(handle);
}

/// Signed-chat acknowledgement, tick trigger. Servers disconnect a client at
/// 4096 unacknowledged signed messages; draining that list requires actually
/// transmitting the acknowledgement offset. This proves the *tick* flush
/// (vanilla's `sendChatAcknowledgement`): pending signed chats are acknowledged
/// when the next server heartbeat (keep-alive) arrives. It also pins three
/// semantics that are silent when wrong:
///   - a **filtered** message (`was_shown = false`) still burns an offset,
///   - an **unsigned** system chat (`ack = None`) does not, and
///   - the flush is **independent of keep-alive policy** (Manual here, so the
///     acknowledgement is the only packet that can appear on the wire).
#[tokio::test]
async fn signed_chat_is_acknowledged_on_keep_alive_tick() {
    const C1: i32 = 0x61;
    const C2: i32 = 0x62;
    const C3: i32 = 0x63;
    const KA: i32 = 0x64;

    let adapter = FakeAdapter::new()
        .on(
            ConnectionState::Handshaking,
            C1,
            vec![signed_chat(vec![1, 1, 1, 1], true)],
        )
        .on(
            ConnectionState::Handshaking,
            C2,
            vec![signed_chat(vec![2, 2, 2, 2], false)],
        )
        .on(
            ConnectionState::Handshaking,
            C3,
            vec![Directive::Emit(ClientEvent::Chat {
                text: Text::literal("[Server] hello"),
                kind: ChatKind::System,
                ack: None,
            })],
        )
        .on(
            ConnectionState::Handshaking,
            KA,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 7 })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    peer.write_packet(C1, &[]).await.unwrap();
    peer.write_packet(C2, &[]).await.unwrap();
    peer.write_packet(C3, &[]).await.unwrap();

    // Negative control: below the burst threshold and before any tick, pending
    // chats produce no acknowledgement. Without this, a test that only saw the
    // ack after the keep-alive could not tell the tick flush apart from an
    // eager per-message one.
    let nothing = tokio::time::timeout(Duration::from_millis(100), peer.read_packet()).await;
    assert!(nothing.is_err(), "chats alone must not ack before a tick");

    // The keep-alive is the tick surrogate: it flushes the accumulated offset.
    peer.write_packet(KA, &[]).await.unwrap();
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(
        id, CHAT_ACK_ID,
        "a chat acknowledgement must be transmitted"
    );
    let offset = i32::from_be_bytes(payload[1..5].try_into().unwrap());
    assert_eq!(
        offset, 2,
        "one shown + one filtered signed chat advance the window; the unsigned \
         system chat does not"
    );

    // Every chat and the keep-alive still surface to the user unchanged.
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Chat { ack: Some(_), .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Chat { ack: Some(_), .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::Chat { ack: None, .. })
    ));
    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 7 }));

    drop(handle);
}

/// Signed-chat acknowledgement, burst trigger. Vanilla's `markMessageAsProcessed`
/// sends a standalone acknowledgement the moment more than 64 messages are
/// pending, without waiting for a tick — the safety valve for a burst arriving
/// faster than the heartbeat. This drives 65 distinct-signature chats in a single
/// packet with *no keep-alive anywhere*, so only the burst valve can flush, then
/// proves the valve reset the offset (a later single chat acknowledges 1, not 66).
/// Wiring only the tick trigger would hang this test; wiring only the valve would
/// hang the previous one — both must be live.
#[tokio::test]
async fn chat_ack_burst_valve_fires_without_a_tick() {
    const BURST: i32 = 0x70;
    const MORE: i32 = 0x71;
    const KA: i32 = 0x72;

    // Distinct signatures: the tracker collapses consecutive duplicates, so 65
    // identical ones would advance the window by 1, not 65.
    let burst: Vec<Directive> = (0u16..65)
        .map(|i| signed_chat(i.to_be_bytes().to_vec(), true))
        .collect();

    let adapter = FakeAdapter::new()
        .on(ConnectionState::Handshaking, BURST, burst)
        .on(
            ConnectionState::Handshaking,
            MORE,
            vec![signed_chat(vec![9, 9, 9, 9], true)],
        )
        .on(
            ConnectionState::Handshaking,
            KA,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 1 })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    peer.write_packet(BURST, &[]).await.unwrap();
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHAT_ACK_ID);
    let offset = i32::from_be_bytes(payload[1..5].try_into().unwrap());
    assert_eq!(
        offset, 65,
        "the burst valve flushes the whole pending count"
    );

    // One more signed chat, then a tick: exactly 1 is acknowledged, proving the
    // valve cleared the offset rather than leaving the 65 to be re-counted.
    peer.write_packet(MORE, &[]).await.unwrap();
    peer.write_packet(KA, &[]).await.unwrap();
    let (id2, payload2) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id2, CHAT_ACK_ID);
    let offset2 = i32::from_be_bytes(payload2[1..5].try_into().unwrap());
    assert_eq!(
        offset2, 1,
        "offset reset after the valve; only the new chat counts"
    );

    // Drain the surfaced events (66 chats + 1 keep-alive) so the driver's sends
    // never wedge on a full channel.
    let mut chats = 0;
    let mut keep_alives = 0;
    for _ in 0..67 {
        match events.recv().await {
            Some(ClientEvent::Chat { .. }) => chats += 1,
            Some(ClientEvent::KeepAlive { .. }) => keep_alives += 1,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!((chats, keep_alives), (66, 1));

    drop(handle);
}

/// Full login -> configuration -> play choreography. Asserts the state machine
/// reaches Play and the user sees the expected events in order.
#[tokio::test]
async fn full_join_choreography_reaches_play() {
    const S_LOGIN_SUCCESS: i32 = 0x02;
    const S_REGISTRY: i32 = 0x05;
    const S_FINISH_CFG: i32 = 0x03;
    const S_LOGIN_PLAY: i32 = 0x2B;
    const S_KEEPALIVE: i32 = 0x26;

    let dimension = Identifier::new("minecraft", "overworld").unwrap();
    let login_event = ClientEvent::Login {
        entity_id: 1,
        game_mode: GameMode::Survival,
        dimension: dimension.clone(),
    };

    let adapter = FakeAdapter::new()
        .begin(vec![
            send(0xF0, b"hs"),
            Directive::SetState(ConnectionState::Login),
            send(0xF1, b"ls"),
        ])
        .on(
            ConnectionState::Login,
            S_LOGIN_SUCCESS,
            vec![
                send(0xF2, b"ack"),
                Directive::SetState(ConnectionState::Configuration),
            ],
        )
        .on(ConnectionState::Configuration, S_REGISTRY, vec![])
        .on(
            ConnectionState::Configuration,
            S_FINISH_CFG,
            vec![
                send(0xF3, b"finack"),
                Directive::SetState(ConnectionState::Play),
            ],
        )
        .on(
            ConnectionState::Play,
            S_LOGIN_PLAY,
            vec![Directive::Emit(login_event.clone())],
        )
        .on(
            ConnectionState::Play,
            S_KEEPALIVE,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 7 })],
        );
    let calls = adapter.calls();

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    // Login phase: read handshake + login start (in order).
    assert_eq!(peer.read_packet().await.unwrap().unwrap().0, 0xF0);
    assert_eq!(peer.read_packet().await.unwrap().unwrap().0, 0xF1);

    // Server accepts login; driver sends login-ack and moves to Configuration.
    peer.write_packet(S_LOGIN_SUCCESS, &[]).await.unwrap();
    assert_eq!(peer.read_packet().await.unwrap().unwrap().0, 0xF2);

    // Configuration phase.
    peer.write_packet(S_REGISTRY, &[]).await.unwrap();
    peer.write_packet(S_FINISH_CFG, &[]).await.unwrap();
    assert_eq!(peer.read_packet().await.unwrap().unwrap().0, 0xF3);

    // Play phase.
    peer.write_packet(S_LOGIN_PLAY, &[]).await.unwrap();
    peer.write_packet(S_KEEPALIVE, &[]).await.unwrap();
    // Auto keep-alive response arrives on the wire.
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        KEEPALIVE_RESP_ID
    );

    // Events, in order.
    assert_eq!(events.recv().await, Some(login_event));
    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 7 }));

    // The play packets were handled under Play — the state machine got there.
    let recorded = calls.lock().unwrap().clone();
    assert!(recorded.contains(&(ConnectionState::Play, S_LOGIN_PLAY)));
    assert!(recorded.contains(&(ConnectionState::Play, S_KEEPALIVE)));

    drop(handle);
}

/// A clean EOF at a frame boundary ends the session as "server closed", not an
/// error, and closes the event stream.
#[tokio::test]
async fn clean_eof_is_server_closed() {
    let (handle, mut events, peer) = start(FakeAdapter::new(), KeepAlivePolicy::Automatic);

    drop(peer); // clean close of the write half

    assert_eq!(events.recv().await, None);
    assert!(matches!(handle.join().await, SessionOutcome::ServerClosed));
}

/// A mid-frame EOF surfaces as a transport error.
#[tokio::test]
async fn mid_frame_eof_is_transport_error() {
    let (client_io, mut server_io) = memory_pair();
    let (handle, _events) = ClientBuilder::new(server(), profile(), Box::new(FakeAdapter::new()))
        .connect_with(client_io);

    // Announce a 5-byte frame but send only 2 bytes, then close.
    server_io.write_all(&[0x05, 0x00, 0x01]).await.unwrap();
    drop(server_io);

    match handle.join().await {
        SessionOutcome::Failed(error) => {
            assert!(
                matches!(
                    error,
                    lodestone_client::ClientError::Transport(
                        lodestone_net::NetError::UnexpectedClose(_)
                    )
                ),
                "unexpected error: {error:?}"
            );
        }
        other => panic!("expected transport failure, got {other:?}"),
    }
}

/// An adapter error from `handle_packet` is surfaced, not swallowed.
#[tokio::test]
async fn adapter_error_is_surfaced() {
    const BAD: i32 = 9;
    let adapter = FakeAdapter::new().fail_on(ConnectionState::Handshaking, BAD);

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(BAD, &[]).await.unwrap();

    // Stream closes because the session ended.
    assert_eq!(events.recv().await, None);

    match handle.join().await {
        SessionOutcome::Failed(lodestone_client::ClientError::Adapter(AdapterError::Decode(_))) => {
        }
        other => panic!("expected adapter error, got {other:?}"),
    }
}

/// An action the adapter cannot encode (returns `None`) is dropped without
/// error, and the session keeps working.
#[tokio::test]
async fn unencodable_action_is_dropped_quietly() {
    let (handle, _events, mut peer) = start(FakeAdapter::new(), KeepAlivePolicy::Automatic);

    // Respawn encodes to None and must produce no packet.
    handle.send_action(ClientAction::Respawn).unwrap();
    // A real action after it must still be written.
    handle
        .send_action(ClientAction::SendChat { text: "hi".into() })
        .unwrap();

    // The first (and only) packet on the wire is the chat, proving the Respawn
    // produced nothing.
    let (id, fields) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHAT_ID);
    assert_eq!(fields, b"hi");

    assert!(!handle.is_finished(), "session should still be alive");
    drop(handle);
}

/// Local shutdown ends the session with a `LocalClose` outcome.
#[tokio::test]
async fn local_shutdown_is_local_close() {
    let (mut handle, mut events, _peer) = start(FakeAdapter::new(), KeepAlivePolicy::Automatic);

    handle.shutdown();

    assert_eq!(events.recv().await, None);
    assert!(matches!(handle.join().await, SessionOutcome::LocalClose));
}
