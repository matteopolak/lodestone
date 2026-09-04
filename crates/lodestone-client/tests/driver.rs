//! Hermetic driver tests.
//!
//! Every test here uses [`lodestone_net::memory_pair`] and a hand-written fake
//! [`VersionAdapter`]; none require a real server.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{
    ChatAckInfo, ChatKind, ClientAction, ClientBuilder, ClientEvent, ConnectionState, Directive,
    KeepAlivePolicy, LoginProfile, PlayerLoadedPolicy, RespawnPolicy, ServerAddress,
    SessionOutcome, VersionAdapter,
};
use lodestone_model::{
    AdapterError, GameMode, Identifier, ResourcePackResponseKind, Rotation, TeleportFlags, Text,
    Vec3,
};
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
    player_loaded_resp_id: Option<i32>,
    brand_resp_id: Option<i32>,
    pong_resp_id: Option<i32>,
    resource_pack_resp_id: Option<i32>,
    cookie_resp_id: Option<i32>,
    calls: Arc<Mutex<Vec<(ConnectionState, i32)>>>,
    /// Chunk columns to write into the [`lodestone_model::WorldSink`] when a
    /// scripted `(state, packet_id)` arrives, as `(chunk_x, chunk_z)`.
    ///
    /// Real adapters put chunk data through the sink and **not** through the event
    /// stream, so any test about the client's decoded chunk store has to reach the
    /// store the same way — asserting on a `ChunkLoaded` event instead would prove
    /// only that a notification travelled.
    chunks: HashMap<(ConnectionState, i32), Vec<(i32, i32)>>,
}

const KEEPALIVE_RESP_ID: i32 = 0x30;
const CHAT_ID: i32 = 0x06;
const RESPAWN_RESP_ID: i32 = 0x0C;
const CHAT_ACK_ID: i32 = 0x07;
const PONG_RESP_ID: i32 = 0x0D;
const RESOURCE_PACK_RESP_ID: i32 = 0x0E;

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

    /// Makes `PlayerLoaded` encode to an observable packet so the driver's
    /// automatic client-loaded signal is visible on the wire; without this it
    /// stays unrepresentable (`Ok(None)`).
    fn player_loaded_to(mut self, id: i32) -> Self {
        self.player_loaded_resp_id = Some(id);
        self
    }

    /// Makes `SendBrand` encode to an observable packet (id, brand-bytes) so the
    /// automatic brand announcement is visible on the wire.
    fn brand_to(mut self, id: i32) -> Self {
        self.brand_resp_id = Some(id);
        self
    }

    /// Makes `PongResponse` encode to an observable packet so the driver's
    /// automatic reply to a server `Ping` challenge is visible on the wire;
    /// without this it stays unrepresentable (`Ok(None)`).
    fn pong_to(mut self, id: i32) -> Self {
        self.pong_resp_id = Some(id);
        self
    }

    /// Makes `ResourcePackResponse` encode to an observable packet so the
    /// driver's automatic answer to a server-pushed resource pack is visible
    /// on the wire; without this it stays unrepresentable (`Ok(None)`).
    fn resource_pack_response_to(mut self, id: i32) -> Self {
        self.resource_pack_resp_id = Some(id);
        self
    }

    /// Makes `CookieResponse` encode to an observable packet so a test can
    /// prove the key and the (seeded-store-or-absent) payload that actually
    /// went out. The payload is `[state, key_len_be, key_bytes,
    /// present_flag, cookie_bytes?]` so a test can distinguish a seeded
    /// cookie from a genuinely absent one without a second channel.
    fn cookie_response_to(mut self, id: i32) -> Self {
        self.cookie_resp_id = Some(id);
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

    /// Loads one all-air column per position into the world sink when
    /// `(state, packet_id)` arrives, and emits a `ChunkLoaded` for each so a test
    /// has something to await before reading the store.
    fn chunks_on(mut self, state: ConnectionState, packet_id: i32, at: &[(i32, i32)]) -> Self {
        self.chunks.insert((state, packet_id), at.to_vec());
        let emits = at
            .iter()
            .map(|(x, z)| {
                Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: lodestone_model::ChunkPos { x: *x, z: *z },
                })
            })
            .collect();
        self.script.insert((state, packet_id), emits);
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
        world: &mut dyn lodestone_model::WorldSink,
        state: ConnectionState,
        packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        self.calls.lock().unwrap().push((state, packet_id));
        if self.fail.contains(&(state, packet_id)) {
            return Err(AdapterError::Decode(format!("boom at {packet_id}")));
        }
        if let Some(positions) = self.chunks.get(&(state, packet_id)) {
            for (x, z) in positions {
                world.load(
                    lodestone_world::ChunkPos { x: *x, z: *z },
                    air_column(),
                );
            }
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
            // Observable only when a test opts in via `player_loaded_to`; the
            // payload carries the encode-time state so tests can prove the state
            // it was sent under.
            ClientAction::PlayerLoaded => Ok(self
                .player_loaded_resp_id
                .map(|id| (id, vec![state_code(state)]))),
            // Observable only when a test opts in via `brand_to`; the payload is
            // the raw brand bytes so a test can assert the announced string.
            ClientAction::SendBrand { brand } => Ok(self
                .brand_resp_id
                .map(|id| (id, brand.clone().into_bytes()))),
            // Observable only when a test opts in via `pong_to`; the payload
            // carries the encode-time state plus the echoed id so a test can
            // prove both the state it was sent under and which challenge it
            // answers.
            ClientAction::PongResponse { id } => Ok(self.pong_resp_id.map(|resp_id| {
                let mut payload = vec![state_code(state)];
                payload.extend_from_slice(&id.to_be_bytes());
                (resp_id, payload)
            })),
            // Observable only when a test opts in via `resource_pack_response_to`;
            // the payload carries the encode-time state, the echoed pack id (the
            // 16 bytes a real server keys its own task on) and a one-byte kind tag
            // so a test can prove the id was not transposed with anything else in
            // the action and which outcome the driver actually reported.
            ClientAction::ResourcePackResponse { id, response } => {
                Ok(self.resource_pack_resp_id.map(|resp_id| {
                    let mut payload = vec![state_code(state)];
                    payload.extend_from_slice(id.as_bytes());
                    payload.push(match response {
                        ResourcePackResponseKind::SuccessfullyLoaded => 0,
                        ResourcePackResponseKind::Declined => 1,
                        ResourcePackResponseKind::FailedDownload => 2,
                        ResourcePackResponseKind::Accepted => 3,
                        ResourcePackResponseKind::Downloaded => 4,
                        ResourcePackResponseKind::InvalidUrl => 5,
                        ResourcePackResponseKind::FailedReload => 6,
                        ResourcePackResponseKind::Discarded => 7,
                    });
                    (resp_id, payload)
                }))
            }
            // Observable only when a test opts in via `cookie_response_to`;
            // see that method's doc for the payload layout.
            ClientAction::CookieResponse { key, payload } => Ok(self.cookie_resp_id.map(|id| {
                let mut out = vec![state_code(state)];
                let key_bytes = key.to_string().into_bytes();
                out.extend_from_slice(&(key_bytes.len() as u32).to_be_bytes());
                out.extend_from_slice(&key_bytes);
                match payload {
                    Some(bytes) => {
                        out.push(1);
                        out.extend_from_slice(bytes);
                    }
                    None => out.push(0),
                }
                (id, out)
            })),
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
        // The driver's last-seen tracker keys on `ack` only, so the sender is
        // out of this test's scope.
        sender: None,
        ack: Some(ChatAckInfo {
            signature,
            global_index: 0,
            was_shown,
            message_index: 0,
            timestamp_millis: 1_700_000_000_000,
            salt: 1,
            raw_content: "hi".to_string(),
            last_seen: Vec::new(),
            verified: false,
        }),
    })
}

/// A `PLAYER_CHAT` that carried **no** signature — the wire's optional
/// signature was absent, which the adapter reports as an empty byte string
/// with `ack` still `Some` (the decoder reports the packet, not a judgement
/// about it). A server produces these for any player without a chat session,
/// including this client's own message echoed back while it sends unsigned.
///
/// Distinct from `signed_chat`'s argument only in that the signature is empty,
/// which is exactly the distinction the last-seen window turns on.
fn unsigned_chat() -> Directive {
    signed_chat(Vec::new(), true)
}

/// The `Login` event that puts us in the world (entering Play). The driver arms
/// its client-loaded latch on this.
fn login_event() -> ClientEvent {
    ClientEvent::Login {
        entity_id: 1,
        game_mode: GameMode::Survival,
        dimension: Identifier::new("minecraft", "overworld").unwrap(),
    }
}

/// The server's placement teleport. The first one after a `Login` (or after a
/// respawn) is the driver's "ready to be moved" trigger for `player_loaded`.
fn teleport_event() -> ClientEvent {
    ClientEvent::TeleportPlayer {
        pos: Vec3::new(0.0, 64.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        flags: TeleportFlags {
            relative_x: false,
            relative_y: false,
            relative_z: false,
            relative_yaw: false,
            relative_pitch: false,
        },
    }
}

/// A respawn that is not a death — portal travel, a dimension change, or
/// `/respawn`. The server re-seeds its client-load timer on any of these.
fn respawned_event() -> ClientEvent {
    respawned_in("the_nether")
}

/// A `Respawned` naming `minecraft:<path>` as the destination. Both the
/// same-dimension (death) and different-dimension (portal) cases arrive on this
/// one event, which is exactly why the driver has to compare rather than react.
fn respawned_in(path: &str) -> ClientEvent {
    ClientEvent::Respawned {
        dimension: Identifier::new("minecraft", path).unwrap(),
        game_mode: GameMode::Survival,
        previous_game_mode: None,
        last_death_location: None,
    }
}

/// An empty single-section column, the cheapest thing that occupies a slot in the
/// chunk store. Nothing here reads block data — the subject is whether the store
/// is emptied at the right moment.
fn air_column() -> lodestone_world::LoadedChunk {
    let column = lodestone_world::ChunkColumn::new(
        0,
        16,
        lodestone_world::PaletteKind::block_states(),
        lodestone_world::PaletteKind::biomes(),
        0,
        0,
    );
    lodestone_world::LoadedChunk::new(
        column,
        lodestone_world::ColumnLight::new(16),
        lodestone_world::Heightmaps::new(),
        Vec::new(),
    )
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

fn start_with_player_loaded(
    adapter: FakeAdapter,
    player_loaded: PlayerLoadedPolicy,
) -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<tokio::io::DuplexStream>,
) {
    let (client_io, server_io) = memory_pair();
    let (handle, events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .player_loaded_policy(player_loaded)
        .connect_with(client_io);
    (handle, events, Connection::new(server_io))
}

/// Starts a session with a prior session's cookie store seeded via
/// [`ClientBuilder::seed_cookies`] — the reconnect leg of
/// [`SessionOutcome::Transferred`], simulated here without a real transfer so
/// the seeding itself can be pinned hermetically.
fn start_with_cookies(
    adapter: FakeAdapter,
    cookies: HashMap<lodestone_model::ResourceKey, Vec<u8>>,
) -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<tokio::io::DuplexStream>,
) {
    let (client_io, server_io) = memory_pair();
    let (handle, events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .seed_cookies(cookies)
        .connect_with(client_io);
    (handle, events, Connection::new(server_io))
}

// --- Tests ------------------------------------------------------------------

/// An opted-in caller world receives the exact inbound packet before the fake
/// adapter turns it into a decoded event. The payload includes a zero byte and
/// a high byte so a string-like or lossy observation cannot pass this gate.
#[tokio::test]
async fn raw_packet_bus_observes_the_wire_bytes_before_decoding() {
    const PACKET_ID: i32 = 0x44;
    let payload = [0x00, 0xff, 0x7f, 0x01];

    let mut app = lodestone_ecs::app::App::new();
    app.add_plugins((
        lodestone_ecs::ingest::IngestPlugin,
        lodestone_ecs::SessionPlugin,
        lodestone_ecs::RawPacketBusPlugin,
    ));
    let session = lodestone_ecs::spawn_session(app.world_mut());
    let world = Arc::new(lodestone_ecs::parking_lot::RwLock::new(
        std::mem::take(app.world_mut()),
    ));

    let adapter = FakeAdapter::new().on(
        ConnectionState::Handshaking,
        PACKET_ID,
        vec![Directive::Emit(ClientEvent::Ping { id: 9 })],
    );
    let (client_io, server_io) = memory_pair();
    let (handle, mut events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .ecs(world.clone(), session)
        .connect_with(client_io);
    let mut peer = Connection::new(server_io);

    peer.write_packet(PACKET_ID, &payload).await.unwrap();
    assert_eq!(events.recv().await, Some(ClientEvent::Ping { id: 9 }));

    let ecs = world.read();
    let messages = ecs
        .resource::<lodestone_ecs::ecs::message::Messages<lodestone_ecs::RawPacket>>();
    let packet = messages
        .iter_current_update_messages()
        .next()
        .expect("the opted-in raw packet bus must receive the inbound packet");
    assert_eq!(packet.state, ConnectionState::Handshaking);
    assert_eq!(packet.packet_id, PACKET_ID);
    assert_eq!(packet.payload, payload);

    drop(handle);
}

/// [`ClientBuilder::seed_cookies`]: a `cookie_request` for a key the *prior*
/// session had stored answers from the seeded map, and a key that was never
/// stored — seeded or otherwise — still answers `None`, exactly like an
/// ordinary unseeded session (`self.cookies.get(key).cloned()` in `Driver`'s
/// `emit`, unchanged). This is the transfer reconnect leg
/// `docs/secure-chat.md`'s "Transfer, and what a reconnect must and must not
/// carry" section documents: without seeding, every post-transfer
/// `cookie_request` answered `None` even for a cookie the previous server had
/// stored, because there was no way to hand `SessionOutcome::Transferred`'s
/// cookie map to a new `ClientBuilder` at all.
#[tokio::test]
async fn seeded_cookies_answer_a_request_and_an_unseeded_key_still_answers_none() {
    const SEEDED_REQUEST: i32 = 0x50;
    const MISSING_REQUEST: i32 = 0x51;
    const COOKIE_RESP: i32 = 0x60;

    let seeded_key = lodestone_model::Identifier::new("lodestone", "seeded").unwrap();
    let missing_key = lodestone_model::Identifier::new("lodestone", "missing").unwrap();
    let mut cookies = HashMap::new();
    cookies.insert(seeded_key.clone(), vec![0xAA, 0xBB, 0xCC]);

    let adapter = FakeAdapter::new()
        .cookie_response_to(COOKIE_RESP)
        .on(
            ConnectionState::Handshaking,
            SEEDED_REQUEST,
            vec![Directive::Emit(ClientEvent::CookieRequested {
                key: seeded_key.clone(),
            })],
        )
        .on(
            ConnectionState::Handshaking,
            MISSING_REQUEST,
            vec![Directive::Emit(ClientEvent::CookieRequested {
                key: missing_key.clone(),
            })],
        );

    let (handle, mut events, mut peer) = start_with_cookies(adapter, cookies);

    peer.write_packet(SEEDED_REQUEST, &[]).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::CookieRequested { .. })
    ));
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, COOKIE_RESP);
    let key_len = u32::from_be_bytes(payload[1..5].try_into().unwrap()) as usize;
    let key_end = 5 + key_len;
    assert_eq!(&payload[5..key_end], seeded_key.to_string().as_bytes());
    assert_eq!(
        payload[key_end],
        1,
        "the seeded cookie must be present, not answered as absent"
    );
    assert_eq!(
        &payload[key_end + 1..],
        &[0xAA, 0xBB, 0xCC],
        "the seeded bytes must be answered verbatim"
    );

    peer.write_packet(MISSING_REQUEST, &[]).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::CookieRequested { .. })
    ));
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, COOKIE_RESP);
    let key_len = u32::from_be_bytes(payload[1..5].try_into().unwrap()) as usize;
    let key_end = 5 + key_len;
    assert_eq!(&payload[5..key_end], missing_key.to_string().as_bytes());
    assert_eq!(
        payload[key_end], 0,
        "a key with no cookie — seeded or otherwise — must answer absent, \
         never a guess"
    );

    drop(handle);
}

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

/// A server `Ping` challenge (distinct from `KeepAlive`) is answered with
/// `ClientAction::PongResponse` echoing the same id, unconditionally — vanilla's
/// own ping handler has no policy gate, unlike
/// its keep-alive handler's send-when gate. Before the driver had this arm, the event
/// decoded and surfaced but nothing ever answered it: an outbound island of the
/// same shape as `ClientAction::SetFlying`.
#[tokio::test]
async fn ping_auto_responds_and_surfaces() {
    const PING: i32 = 0x51;
    let adapter = FakeAdapter::new()
        .pong_to(PONG_RESP_ID)
        .on(
            ConnectionState::Handshaking,
            PING,
            vec![Directive::Emit(ClientEvent::Ping { id: 7 })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(PING, &[]).await.unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, PONG_RESP_ID);
    assert_eq!(payload[0], state_code(ConnectionState::Handshaking));
    assert_eq!(&payload[1..], &7i32.to_be_bytes());

    assert_eq!(events.recv().await, Some(ClientEvent::Ping { id: 7 }));

    drop(handle);
}

/// A `Ping` reply is unconditional — there is no policy that suppresses it the
/// way `KeepAlivePolicy::Manual` suppresses the keep-alive response, matching
/// vanilla's own ping handler having no equivalent gate. Without the driver's arm
/// in `Driver::emit`, this test would see no packet at all and the read below
/// would hang (it has no timeout, unlike the manual-keep-alive test's, so a
/// real neuter run needs a `tokio::time::timeout` wrapped around it first).
#[tokio::test]
async fn ping_answered_regardless_of_keep_alive_policy() {
    const PING: i32 = 0x51;
    let adapter = FakeAdapter::new()
        .pong_to(PONG_RESP_ID)
        .on(
            ConnectionState::Handshaking,
            PING,
            vec![Directive::Emit(ClientEvent::Ping { id: 13 })],
        );

    // Manual keep-alive policy must not affect the ping reply.
    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    peer.write_packet(PING, &[]).await.unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, PONG_RESP_ID);
    assert_eq!(&payload[1..], &13i32.to_be_bytes());

    assert_eq!(events.recv().await, Some(ClientEvent::Ping { id: 13 }));

    drop(handle);
}

/// A server-pushed resource pack is answered automatically, from the driver
/// rather than from anything shell-side.
///
/// `route(&ClientEvent::ResourcePackPushed)` is `Route::NOWHERE` in
/// `lodestone-model` — deliberately, per `docs/event-routing.md` and the doc
/// on `Driver::emit`'s own `ResourcePackPushed` arm: the shell's event loop
/// does not start until after login, so a shell-side producer would be
/// correct-looking and permanently too late for a pack pushed during
/// Configuration. A prior audit flagged `ClientAction::ResourcePackResponse`
/// as an island on the strength of that `NOWHERE` routing alone — the same
/// false-negative shape `docs/event-routing.md` already documents for `Ping`
/// (also `Route::NOWHERE`, also answered here, also currently mis-flagged as
/// unconsumed by that same doc's prose). This is the regression gate the
/// existing wiring never had: the v26-2 round-trip test in
/// `resource_pack_push.rs` sends the reply by calling `send_action` by hand,
/// so it cannot tell an automatic answer from a manual one. This test never
/// calls `send_action`.
///
/// Two different pack ids are pushed rather than one, so a driver that hands
/// back a fixed id (or the wrong field) cannot pass by coincidence — the
/// same pairwise-distinct-fixture reasoning as any other single-typed-field
/// round trip.
#[tokio::test]
async fn resource_pack_push_is_auto_answered_and_surfaces() {
    const PUSH_A: i32 = 0x53;
    const PUSH_B: i32 = 0x54;
    let id_a = Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111);
    let id_b = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222);
    let adapter = FakeAdapter::new()
        .resource_pack_response_to(RESOURCE_PACK_RESP_ID)
        .on(
            ConnectionState::Handshaking,
            PUSH_A,
            vec![Directive::Emit(ClientEvent::ResourcePackPushed {
                id: id_a,
                url: "https://example.invalid/a.zip".into(),
                hash: String::new(),
                required: true,
                prompt: None,
            })],
        )
        .on(
            ConnectionState::Handshaking,
            PUSH_B,
            vec![Directive::Emit(ClientEvent::ResourcePackPushed {
                id: id_b,
                url: "https://example.invalid/b.zip".into(),
                hash: String::new(),
                required: false,
                prompt: None,
            })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(PUSH_A, &[]).await.unwrap();
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, RESOURCE_PACK_RESP_ID);
    assert_eq!(&payload[1..17], id_a.as_bytes(), "the reply must echo the pushed pack's own id");
    assert_eq!(
        payload[17], 2,
        "FailedDownload (a terminal `Action`) is the honest answer for a client \
         that applies no packs -- see `Driver::emit`'s own doc on why not `Declined`"
    );
    assert_eq!(
        events.recv().await,
        Some(ClientEvent::ResourcePackPushed {
            id: id_a,
            url: "https://example.invalid/a.zip".into(),
            hash: String::new(),
            required: true,
            prompt: None,
        }),
        "the notification must still surface even though the driver already answered it"
    );

    // A second, distinct push must get its *own* id echoed back, not the
    // first push's -- the control a single-push test cannot run.
    peer.write_packet(PUSH_B, &[]).await.unwrap();
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, RESOURCE_PACK_RESP_ID);
    assert_eq!(
        &payload[1..17],
        id_b.as_bytes(),
        "a second push's reply must echo its own id, not the first push's"
    );

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

/// Automatic client-loaded signal. Vanilla's own player-packet-listener
/// silently ignores our movement until its own client-loaded timeout timer (~60
/// ticks) expires, unless the client zeroes it early with `player_loaded`. The
/// driver must send it on its own — otherwise every session's first ~3 s of
/// movement is discarded and a gate that measures movement in that window
/// measures nothing (a false green). The trigger is the first `TeleportPlayer`
/// after entering the world (`Login`): that teleport is the server placing us,
/// i.e. the moment we are genuinely ready to be moved (sending it the instant
/// `Login` arrives, before placement, would be too early). The send precedes the
/// surfaced teleport event, like the other auto-responders.
#[tokio::test]
async fn player_loaded_auto_sent_on_first_teleport_after_login() {
    const LOGIN_PKT: i32 = 0x2B;
    const TP_PKT: i32 = 0x40;
    const PLAYER_LOADED_ID: i32 = 0x2A;

    let adapter = FakeAdapter::new()
        .player_loaded_to(PLAYER_LOADED_ID)
        .on(
            ConnectionState::Handshaking,
            LOGIN_PKT,
            vec![Directive::Emit(login_event())],
        )
        .on(
            ConnectionState::Handshaking,
            TP_PKT,
            vec![Directive::Emit(teleport_event())],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(LOGIN_PKT, &[]).await.unwrap();
    assert_eq!(events.recv().await, Some(login_event()));

    peer.write_packet(TP_PKT, &[]).await.unwrap();
    // player_loaded is written before the teleport event is surfaced.
    let (id, _payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(
        id, PLAYER_LOADED_ID,
        "driver should auto-send player_loaded on the first placement teleport"
    );
    assert_eq!(events.recv().await, Some(teleport_event()));

    drop(handle);
}

/// The signal fires exactly once per load-epoch and re-arms on respawn. The
/// server re-seeds the load timer whenever it respawns us, so a client that only
/// sent `player_loaded` at join would have its post-respawn movement ignored.
/// A second teleport in the same epoch must NOT re-send it (otherwise we would
/// spam the server); a `Death` re-arms so the next placement teleport does.
#[tokio::test]
async fn player_loaded_fires_once_then_rearms_on_death() {
    const LOGIN_PKT: i32 = 0x2B;
    const TP_PKT: i32 = 0x40;
    const DEATH_PKT: i32 = 0x52;
    const PLAYER_LOADED_ID: i32 = 0x2A;

    let adapter = FakeAdapter::new()
        .player_loaded_to(PLAYER_LOADED_ID)
        .respawn_to(RESPAWN_RESP_ID)
        .on(
            ConnectionState::Handshaking,
            LOGIN_PKT,
            vec![Directive::Emit(login_event())],
        )
        .on(
            ConnectionState::Handshaking,
            TP_PKT,
            vec![Directive::Emit(teleport_event())],
        )
        .on(
            ConnectionState::Handshaking,
            DEATH_PKT,
            vec![Directive::Emit(ClientEvent::Death {
                message: Text::literal("slain"),
            })],
        );

    let (handle, mut events, mut peer) = start_with(
        adapter,
        KeepAlivePolicy::Automatic,
        RespawnPolicy::Automatic,
    );

    // Join: login + first teleport => exactly one player_loaded.
    peer.write_packet(LOGIN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Login
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        PLAYER_LOADED_ID
    );
    let _ = events.recv().await; // TeleportPlayer

    // A second teleport in the same epoch sends nothing; the next wire packet is
    // therefore the respawn produced by the following Death. If the epoch guard
    // were missing, a stray player_loaded would appear here instead.
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // TeleportPlayer
    peer.write_packet(DEATH_PKT, &[]).await.unwrap();
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        RESPAWN_RESP_ID,
        "second teleport must not re-send player_loaded; respawn is the next packet"
    );
    let _ = events.recv().await; // Death

    // Death re-armed the latch, so the respawn's placement teleport re-sends it.
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        PLAYER_LOADED_ID,
        "player_loaded should re-fire on the post-respawn placement teleport"
    );

    drop(handle);
}

/// A respawn that is NOT a death — portal travel, a dimension change, `/respawn`
/// — still re-seeds the server's client-load timer, so the latch must re-arm on
/// `Respawned`, not only on `Death`. Keying re-arm on `Death` alone would leave
/// every non-death transition silently re-entering the ignore-movement window.
#[tokio::test]
async fn player_loaded_rearms_on_respawn_without_death() {
    const LOGIN_PKT: i32 = 0x2B;
    const TP_PKT: i32 = 0x40;
    const RESPAWN_PKT: i32 = 0x4B;
    const PLAYER_LOADED_ID: i32 = 0x2A;

    let adapter = FakeAdapter::new()
        .player_loaded_to(PLAYER_LOADED_ID)
        .on(
            ConnectionState::Handshaking,
            LOGIN_PKT,
            vec![Directive::Emit(login_event())],
        )
        .on(
            ConnectionState::Handshaking,
            TP_PKT,
            vec![Directive::Emit(teleport_event())],
        )
        .on(
            ConnectionState::Handshaking,
            RESPAWN_PKT,
            vec![Directive::Emit(respawned_event())],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    // Join: login + first teleport consume the latch (exactly one player_loaded).
    peer.write_packet(LOGIN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Login
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        PLAYER_LOADED_ID
    );
    let _ = events.recv().await; // TeleportPlayer

    // A portal/dimension respawn (no preceding Death) re-arms the latch, so the
    // next placement teleport re-sends player_loaded.
    peer.write_packet(RESPAWN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Respawned
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    assert_eq!(
        peer.read_packet().await.unwrap().unwrap().0,
        PLAYER_LOADED_ID,
        "a non-death respawn must re-arm player_loaded"
    );

    drop(handle);
}

/// **The dimension-change gate.** A `Respawned` naming a *different* dimension
/// must empty the decoded chunk store; one naming the same dimension must not
/// touch it.
///
/// # Why this belongs on the net thread and not at the shell
///
/// The store is filled by the adapter's `WorldSink` while a packet decodes, i.e.
/// on the driver's own task. The shell's dimension reset runs on the render thread
/// when it next drains, and by then the new dimension's columns can already be in
/// the store — so a clear there deletes terrain no server resends. This test drives
/// the packets in the order the wire delivers them and reads the store through the
/// handle, so it is measuring the real ordering rather than a call to a function.
///
/// # The same-dimension arm is the important one
///
/// A death-respawn reports the *same* dimension id. If the comparison were dropped,
/// every death in the game would empty the chunk store and force a full terrain
/// reload — a far worse bug than the leftover geometry this fixes, and one a test
/// asserting only the portal arm would certify as working. The two ids are both
/// `minecraft:` and differ only in path, so a comparison that accidentally compared
/// namespaces would pass the same-dimension arm and fail the portal one rather than
/// the other way round.
///
/// Four counts are collected and asserted together, so one wrong arm does not hide
/// the other three, and each expected value is stated as a number rather than as a
/// direction.
#[tokio::test]
async fn a_dimension_change_empties_the_chunk_store_and_a_death_respawn_does_not() {
    const LOGIN_PKT: i32 = 0x2B;
    const OVERWORLD_CHUNKS_PKT: i32 = 0x27;
    const NETHER_CHUNKS_PKT: i32 = 0x28;
    const SAME_DIM_RESPAWN_PKT: i32 = 0x4A;
    const NEW_DIM_RESPAWN_PKT: i32 = 0x4B;

    // Three then two, so no arm's expected count is a repeat of another's and a
    // stale read cannot pass by coincidence.
    const OVERWORLD: [(i32, i32); 3] = [(0, 0), (1, 0), (0, 1)];
    const NETHER: [(i32, i32); 2] = [(-4, 7), (-4, 8)];

    let adapter = FakeAdapter::new()
        .on(
            ConnectionState::Handshaking,
            LOGIN_PKT,
            vec![Directive::Emit(login_event())],
        )
        .chunks_on(ConnectionState::Handshaking, OVERWORLD_CHUNKS_PKT, &OVERWORLD)
        .chunks_on(ConnectionState::Handshaking, NETHER_CHUNKS_PKT, &NETHER)
        // `login_event()` is `minecraft:overworld`, so this one is a death.
        .on(
            ConnectionState::Handshaking,
            SAME_DIM_RESPAWN_PKT,
            vec![Directive::Emit(respawned_in("overworld"))],
        )
        .on(
            ConnectionState::Handshaking,
            NEW_DIM_RESPAWN_PKT,
            vec![Directive::Emit(respawned_in("the_nether"))],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(LOGIN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Login — records the baseline dimension.

    // Fill the store with the overworld.
    peer.write_packet(OVERWORLD_CHUNKS_PKT, &[]).await.unwrap();
    for _ in 0..OVERWORLD.len() {
        let _ = events.recv().await; // ChunkLoaded
    }
    let after_load = handle.loaded_chunk_count();

    // A death in the overworld. Awaiting the `Respawned` event is what makes the
    // read below happen *after* the driver's arm has run — the clear is on the
    // driver's task, and `emit` forwards the event only once it has returned.
    peer.write_packet(SAME_DIM_RESPAWN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Respawned (same dimension)
    let after_death = handle.loaded_chunk_count();

    // A portal trip.
    peer.write_packet(NEW_DIM_RESPAWN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Respawned (new dimension)
    let after_portal = handle.loaded_chunk_count();

    // The Nether's own columns arrive, then a death *in the Nether*: the edge must
    // be consumed, or the second respawn would throw away terrain that has already
    // been streamed and will not be sent again.
    peer.write_packet(NETHER_CHUNKS_PKT, &[]).await.unwrap();
    for _ in 0..NETHER.len() {
        let _ = events.recv().await; // ChunkLoaded
    }
    peer.write_packet(NEW_DIM_RESPAWN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Respawned (the Nether again)
    let after_nether_death = handle.loaded_chunk_count();

    let mut wrong = Vec::new();
    for (what, got, want) in [
        ("after the overworld's columns loaded", after_load, OVERWORLD.len()),
        (
            "after a death-respawn in the same dimension (must be untouched)",
            after_death,
            OVERWORLD.len(),
        ),
        ("after a portal trip to the Nether (must be empty)", after_portal, 0),
        (
            "after a death in the Nether (the edge must be consumed)",
            after_nether_death,
            NETHER.len(),
        ),
    ] {
        if got != want {
            wrong.push(format!("{what}: want {want} columns, got {got}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");

    drop(handle);
}

/// A teleport before any `Login` must not trigger `player_loaded`: the latch is
/// disarmed until we actually enter the world, so a stray pre-Play teleport
/// cannot make us announce readiness prematurely.
#[tokio::test]
async fn player_loaded_not_sent_before_login() {
    const TP_PKT: i32 = 0x40;
    const KEEPALIVE_PKT: i32 = 0x41;
    const PLAYER_LOADED_ID: i32 = 0x2A;

    let adapter = FakeAdapter::new()
        .player_loaded_to(PLAYER_LOADED_ID)
        .on(
            ConnectionState::Handshaking,
            TP_PKT,
            vec![Directive::Emit(teleport_event())],
        )
        .on(
            ConnectionState::Handshaking,
            KEEPALIVE_PKT,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 9 })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(TP_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // TeleportPlayer (no player_loaded)

    // The keep-alive response is the first packet on the wire, which is only true
    // if no stray player_loaded preceded it.
    peer.write_packet(KEEPALIVE_PKT, &[]).await.unwrap();
    let (id, _payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(
        id, KEEPALIVE_RESP_ID,
        "no player_loaded should be sent before entering the world"
    );

    drop(handle);
}

/// Negative control for the `player_loaded` mechanism: under
/// [`PlayerLoadedPolicy::Manual`] the driver must NOT announce readiness, even on
/// the placement teleport that would normally trigger it. This is the suppression
/// path a live gate uses to demonstrate the server's client-load window — with it
/// the server keeps ignoring movement; with the default automatic policy it does
/// not. Proven the same way as the pre-login control: the keep-alive response is
/// the first packet on the wire, which only holds if no `player_loaded` preceded
/// it despite login + a placement teleport.
#[tokio::test]
async fn player_loaded_suppressed_under_manual_policy() {
    const LOGIN_PKT: i32 = 0x2B;
    const TP_PKT: i32 = 0x40;
    const KEEPALIVE_PKT: i32 = 0x41;
    const PLAYER_LOADED_ID: i32 = 0x2A;

    let adapter = FakeAdapter::new()
        .player_loaded_to(PLAYER_LOADED_ID)
        .on(
            ConnectionState::Handshaking,
            LOGIN_PKT,
            vec![Directive::Emit(login_event())],
        )
        .on(
            ConnectionState::Handshaking,
            TP_PKT,
            vec![Directive::Emit(teleport_event())],
        )
        .on(
            ConnectionState::Handshaking,
            KEEPALIVE_PKT,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 7 })],
        );

    let (handle, mut events, mut peer) =
        start_with_player_loaded(adapter, PlayerLoadedPolicy::Manual);

    peer.write_packet(LOGIN_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // Login
    peer.write_packet(TP_PKT, &[]).await.unwrap();
    let _ = events.recv().await; // TeleportPlayer — no player_loaded under Manual

    // If a player_loaded had leaked, it would be the first wire packet; instead
    // the keep-alive response is, proving the placement teleport sent nothing.
    peer.write_packet(KEEPALIVE_PKT, &[]).await.unwrap();
    let (id, _payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(
        id, KEEPALIVE_RESP_ID,
        "Manual policy must suppress player_loaded entirely"
    );

    drop(handle);
}

/// The client announces its brand on entering Configuration, as vanilla does.
/// This is protocol hygiene with no game/UI input; the driver injects it on the
/// state transition, and `encode_action` maps it to the state-appropriate packet
/// (older protocols with no Configuration state simply never hit this path).
#[tokio::test]
async fn brand_announced_on_entering_configuration() {
    const LOGIN_SUCCESS: i32 = 0x02;
    const BRAND_ID: i32 = 0x0D;
    const LOGIN_ACK_ID: i32 = 0xF2;

    let adapter = FakeAdapter::new()
        .brand_to(BRAND_ID)
        .begin(vec![Directive::SetState(ConnectionState::Login)])
        .on(
            ConnectionState::Login,
            LOGIN_SUCCESS,
            vec![
                send(LOGIN_ACK_ID, b"ack"),
                Directive::SetState(ConnectionState::Configuration),
            ],
        );

    let (handle, _events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(LOGIN_SUCCESS, &[]).await.unwrap();

    // login-acknowledged goes out first (still in Login state), then the brand is
    // announced immediately on entering Configuration.
    assert_eq!(peer.read_packet().await.unwrap().unwrap().0, LOGIN_ACK_ID);
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(
        id, BRAND_ID,
        "brand should be announced on entering Configuration"
    );
    assert_eq!(
        payload, b"vanilla",
        "default brand is the vanilla-compatible string"
    );

    drop(handle);
}

/// Signed-chat acknowledgement, tick trigger. Servers disconnect a client at
/// 4096 unacknowledged signed messages; draining that list requires actually
/// transmitting the acknowledgement offset. This proves the *tick* flush
/// (vanilla's own chat-acknowledgement send on tick): pending signed chats are acknowledged
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
                sender: None,
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

/// The signed-message verification half, end to end: a valid signed message reaches
/// `events` with `ack.verified == true` and untouched text; a tampered one
/// (same sender, same session, same chain, one signature byte flipped)
/// reaches it `verified == false` with the not-secure tag prepended. Neither
/// case touches a real session server or the keychain: `ClientBuilder::new`
/// here never sets an `auth_session`, and `lodestone_auth::ChatKeyPair::for_tests`
/// is the same offline fixture builder `driver.rs`'s own unit tests use.
#[tokio::test]
async fn incoming_signed_chat_is_verified_against_the_announced_public_key() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    const PLAYER_INFO: i32 = 0x50;
    const VALID_CHAT: i32 = 0x51;
    const TAMPERED_CHAT: i32 = 0x52;

    // Same fixed PKCS#8-DER RSA-2048 test key `driver.rs`'s own chat-signing
    // unit tests use — a public, throwaway fixture, no secrecy shared with
    // anything real.
    let der = BASE64
        .decode(concat!(
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
        ))
        .expect("valid base64 fixture");
    let private_key = RsaPrivateKey::from_pkcs8_der(&der).expect("valid PKCS#8 DER fixture");
    let public_key = RsaPublicKey::from(&private_key);
    let public_key_der = public_key
        .to_public_key_der()
        .expect("encode test public key")
        .into_vec();
    let key_pair = lodestone_auth::ChatKeyPair::for_tests(
        private_key,
        public_key_der.clone(),
        vec![0xAA, 0xBB],
        i64::MAX,
        i64::MAX,
    );
    let sender = Uuid::from_u128(77);
    let mut chat_session = lodestone_auth::ChatSession::new(sender, key_pair);
    let session_id = chat_session.session_id();

    let content = "hi from a real key";
    let timestamp_millis = 1_700_000_000_456i64;
    let salt = 24_681_357i64;
    let (signature, message_index) = chat_session
        .sign(content, timestamp_millis / 1000, salt, &[])
        .expect("signing must succeed")
        .expect("a fresh session has chain left");

    let ack = |sig: Vec<u8>| ChatAckInfo {
        signature: sig,
        global_index: 0,
        was_shown: true,
        message_index,
        timestamp_millis,
        salt,
        raw_content: content.to_string(),
        last_seen: Vec::new(),
        verified: false,
    };

    let mut tampered = signature.to_vec();
    tampered[0] ^= 0xFF;

    let adapter = FakeAdapter::new()
        .on(
            ConnectionState::Handshaking,
            PLAYER_INFO,
            vec![Directive::Emit(ClientEvent::PlayerListUpdate {
                entries: vec![lodestone_model::event::PlayerListEntry {
                    uuid: Some(sender),
                    name: Some("Signer".to_string()),
                    game_mode: None,
                    latency: None,
                    display_name: None,
                    listed: None,
                    properties: None,
                    chat_session: Some(lodestone_model::event::ChatSessionInfo {
                        session_id,
                        public_key: public_key_der.clone(),
                        expires_at: i64::MAX,
                    }),
                    list_order: None,
                    hat_visible: None,
                }],
            })],
        )
        .on(
            ConnectionState::Handshaking,
            VALID_CHAT,
            vec![Directive::Emit(ClientEvent::Chat {
                text: Text::literal(content),
                kind: ChatKind::Chat,
                sender: Some(sender),
                ack: Some(ack(signature.to_vec())),
            })],
        )
        .on(
            ConnectionState::Handshaking,
            TAMPERED_CHAT,
            vec![Directive::Emit(ClientEvent::Chat {
                text: Text::literal(content),
                kind: ChatKind::Chat,
                sender: Some(sender),
                ack: Some(ack(tampered)),
            })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Automatic);

    peer.write_packet(PLAYER_INFO, &[]).await.unwrap();
    peer.write_packet(VALID_CHAT, &[]).await.unwrap();
    peer.write_packet(TAMPERED_CHAT, &[]).await.unwrap();

    assert!(matches!(
        events.recv().await,
        Some(ClientEvent::PlayerListUpdate { .. })
    ));

    match events.recv().await {
        Some(ClientEvent::Chat {
            ack: Some(info),
            text,
            ..
        }) => {
            assert!(info.verified, "a genuinely signed message must verify");
            assert_eq!(
                text,
                Text::literal(content),
                "a verified message's text must be untouched"
            );
        }
        other => panic!("expected the valid signed chat event, got {other:?}"),
    }

    match events.recv().await {
        Some(ClientEvent::Chat {
            ack: Some(info),
            text,
            ..
        }) => {
            assert!(!info.verified, "a tampered signature must not verify");
            // The message body must be untouched either way — an unverified
            // message is a classification (`info.verified`), never a rewrite
            // of `text` itself. This used to prepend a literal
            // `"[Not Secure] "` here, which was a mis-port of vanilla's
            // separate not-secure indicator sprite into the message content;
            // see `driver.rs`'s own note at the verification call site.
            assert_eq!(
                text,
                Text::literal(content),
                "an unverified message's text must still be untouched — only \
                 `info.verified` carries the trust verdict"
            );
        }
        other => panic!("expected the tampered signed chat event, got {other:?}"),
    }

    drop(handle);
}

/// Signed-chat acknowledgement, burst trigger. Vanilla's own
/// message-processed marker
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

/// A decode error from `handle_packet` is **not** fatal: the packet is dropped
/// and the session keeps running. Each packet is transport-framed, so a payload
/// the adapter cannot parse never desyncs the stream — killing the session on
/// one would turn every future forward-compatible wire addition into an outage.
#[tokio::test]
async fn decode_error_drops_packet_and_keeps_session() {
    const BAD: i32 = 9;
    const GOOD: i32 = 0x50;
    let adapter = FakeAdapter::new().fail_on(ConnectionState::Handshaking, BAD).on(
        ConnectionState::Handshaking,
        GOOD,
        vec![Directive::Emit(ClientEvent::KeepAlive { id: 7 })],
    );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    // The undecodable packet must be dropped, not fatal.
    peer.write_packet(BAD, &[]).await.unwrap();
    // A well-formed packet after it must still be processed, proving the read
    // loop survived and stayed in sync.
    peer.write_packet(GOOD, &[]).await.unwrap();

    assert_eq!(events.recv().await, Some(ClientEvent::KeepAlive { id: 7 }));
    assert!(!handle.is_finished(), "decode error must not end the session");
    drop(handle);
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

/// A mid-session failure reaches the **event stream**, and a clean close does
/// not — the two endings must be distinguishable to a consumer that only ever
/// sees the stream.
///
/// # What this is a regression test for
///
/// `SessionOutcome::Failed(ClientError)` is only readable through
/// `ClientHandle::join`, which takes the handle **by value**. A shell holding an
/// `Arc<ClientHandle>` cannot call it, so every mid-session failure used to
/// present to it as the stream simply ending — identical to a clean close — and
/// the shell synthesised the string `"stream closed"` for both. The real cause
/// went only to the driver's log.
///
/// # Why both arms are in one test
///
/// A gate that only covers the failure case passes just as well if *every*
/// ending emits `SessionFailed`, which would label a clean server close a
/// client-side failure and put the wrong title on the screen. The discriminating
/// property is the **difference** between the two arms, so both are measured
/// here and the mismatches are collected rather than asserted inside the flow —
/// so a run reports everything that is wrong, not just the first thing.
#[tokio::test]
async fn a_mid_session_failure_reaches_the_event_stream_and_a_clean_close_does_not() {
    let mut mismatches: Vec<String> = Vec::new();

    // ---- arm A: mid-frame EOF, i.e. a genuine transport failure -------------
    let (client_io, mut server_io) = memory_pair();
    let (handle, mut events) =
        ClientBuilder::new(server(), profile(), Box::new(FakeAdapter::new()))
            .connect_with(client_io);

    // Announce a 5-byte frame and send 2 of its bytes, then close: the same
    // input `mid_frame_eof_is_transport_error` uses.
    server_io.write_all(&[0x05, 0x00, 0x01]).await.unwrap();
    drop(server_io);

    // Predicted exactly, from the two `#[error(…)]` record definitions plus
    // `Codec::buffered_len` (which is `rx.len()`, so all three fed bytes are
    // still buffered when EOF lands): `ClientError::Transport`'s
    // `"transport error: {0}"` wrapping `NetError::UnexpectedClose`'s
    // `"connection closed mid-frame ({0} bytes buffered)"`. Not a round number
    // and not a direction — the wrong hypothesis this replaces is the literal
    // string `"stream closed"`, which shares no substring with it.
    let expected = "transport error: connection closed mid-frame (3 bytes buffered)";
    match events.recv().await {
        Some(ClientEvent::SessionFailed { reason }) => {
            if reason != expected {
                mismatches.push(format!(
                    "failure arm: reason was {reason:?}, expected {expected:?}"
                ));
            }
        }
        other => mismatches.push(format!(
            "failure arm: expected a SessionFailed event, got {other:?} — a consumer \
             holding an Arc<ClientHandle> can read nothing else, so this is the whole \
             difference between a real cause and a synthesised \"stream closed\""
        )),
    }
    // And the event agrees with the outcome rather than being a second, freely
    // drifting rendering of it.
    match handle.join().await {
        SessionOutcome::Failed(error) => {
            if error.cause_chain() != expected {
                mismatches.push(format!(
                    "failure arm: outcome chain was {:?}, expected {expected:?}",
                    error.cause_chain()
                ));
            }
        }
        other => mismatches.push(format!("failure arm: outcome was {other:?}")),
    }

    // ---- arm B: clean EOF at a frame boundary ------------------------------
    let (handle, mut events, peer) = start(FakeAdapter::new(), KeepAlivePolicy::Automatic);
    drop(peer);
    match events.recv().await {
        None => {}
        other => mismatches.push(format!(
            "clean arm: a clean close must emit no SessionFailed, got {other:?} — \
             otherwise every ending is labelled a client-side failure and this gate \
             would pass without discriminating anything"
        )),
    }
    if !matches!(handle.join().await, SessionOutcome::ServerClosed) {
        mismatches.push("clean arm: outcome was not ServerClosed".to_owned());
    }

    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// **Only a signed message advances the last-seen window** — the regression
/// that got the repo owner disconnected once this client started transmitting
/// a real acknowledgement offset.
///
/// Both peers must count the same messages. The server's own last-seen
/// tracker only records a pending message when the wire's signature was
/// present, and vanilla's own client mirrors that null
/// check around its own message-processed marker. Counting an unsigned `PLAYER_CHAT`
/// advances our offset past a server count that never moved.
///
/// **The expected value comes from outside this repo.** Measured against the
/// live vanilla 26.2 oracle: sending one unsigned chat message and reading the
/// echo back showed `signature present = False`, and acknowledging that single
/// phantom message produced, in the server's own log,
/// *"Failed to validate message acknowledgement offset … Advanced last seen
/// window by 1 messages, but expected at most 0"* followed by
/// *"lost connection: Chat message validation failure"* — vanilla's
/// `multiplayer.disconnect.chat_validation_failed`.
///
/// The two hypotheses are made to differ by construction: `SIGNED` and
/// `UNSIGNED` are **pairwise distinct and neither is zero**, so a gate with
/// only signed messages — or only unsigned ones — could not tell them apart.
/// Signed signatures are distinct because the tracker collapses consecutive
/// duplicates.
///
/// **Measured under the neuter** (guard removed): this reports **6**, not
/// `SIGNED + UNSIGNED = 8`, because every unsigned message carries the *same*
/// empty signature and the tracker's consecutive-duplicate collapse
/// (vanilla's own last-seen tracker checks the last-tracked message before
/// adding a new one) folds
/// the trailing run into one. 6 is the honest wrong-hypothesis value here; the
/// assertion below does not name it, because it depends on the interleaving
/// pattern rather than on the counts alone.
#[tokio::test]
async fn only_signed_chat_advances_the_last_seen_window() {
    const SIGNED: i32 = 3;
    const UNSIGNED: i32 = 5;
    const BATCH: i32 = 0x7A;
    const KA: i32 = 0x7B;

    // Interleaved rather than grouped: a guard that bailed on the first
    // unsigned message would still pass a signed-then-unsigned ordering.
    let mut batch: Vec<Directive> = Vec::new();
    for i in 0..UNSIGNED.max(SIGNED) {
        if i < SIGNED {
            batch.push(signed_chat((i as u16).to_be_bytes().to_vec(), true));
        }
        if i < UNSIGNED {
            batch.push(unsigned_chat());
        }
    }

    let adapter = FakeAdapter::new()
        .on(ConnectionState::Handshaking, BATCH, batch)
        .on(
            ConnectionState::Handshaking,
            KA,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 3 })],
        );

    let (handle, mut events, mut peer) = start(adapter, KeepAlivePolicy::Manual);

    peer.write_packet(BATCH, &[]).await.unwrap();
    peer.write_packet(KA, &[]).await.unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, CHAT_ACK_ID, "the tick flush must send a chat_ack");
    let offset = i32::from_be_bytes(payload[1..5].try_into().unwrap());
    assert_eq!(
        offset, SIGNED,
        "the window must advance by the {SIGNED} signed messages only and \
         ignore the {UNSIGNED} unsigned ones; any larger offset is one a real \
         server rejects with multiplayer.disconnect.chat_validation_failed"
    );

    // Drain so the driver's send side cannot wedge on a full channel.
    let total = SIGNED + UNSIGNED + 1;
    for _ in 0..total {
        assert!(events.recv().await.is_some());
    }

    drop(handle);
}
