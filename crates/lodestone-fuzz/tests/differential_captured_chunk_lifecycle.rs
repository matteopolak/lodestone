//! A captured chunk load, block update, and unload through public client state.
#![cfg(feature = "v26-2")]

use std::time::Duration;

use lodestone_client::{
    BlockPos, ChunkPos, ClientBuilder, ClientEvent, ConnectionState, Directive, EventStream,
    LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_core::Reader;
use lodestone_data::block_states::state_id;
use lodestone_model::{AdapterError, ClientAction, WorldSink};
use lodestone_net::{Connection, memory_pair};
use lodestone_v26_2::{V770Adapter, packet_ids::play};
use serde::{Deserialize, Serialize};
use tokio::io::DuplexStream;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

const CHUNK: [i32; 2] = [0, 0];
const UPDATED_POS: [i32; 3] = [8, 100, 8];
const UPDATED_STATE: &str = "minecraft:gold_block";
#[cfg(feature = "rcon-oracle")]
const CAPTURE_ORIGIN: (i32, i32, i32) = (0, 100, 0);

#[derive(Debug, Serialize, Deserialize)]
struct Capture {
    source: String,
    protocol: i32,
    chunk: [i32; 2],
    update: CapturedUpdate,
    packets: Vec<CapturedPacket>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedUpdate {
    pos: [i32; 3],
    state: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedPacket {
    packet_id: i32,
    #[serde(with = "compressed_bytes")]
    payload: Vec<u8>,
}

/// Stores a captured packet as zlib-compressed base64 rather than a long JSON
/// byte list. This is fixture transport only; replay restores the exact bytes
/// before handing them unchanged to the adapter.
mod compressed_bytes {
    use std::io::{Read, Write};

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::Error as _};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut compressed = ZlibEncoder::new(Vec::new(), Compression::best());
        compressed.write_all(bytes).map_err(S::Error::custom)?;
        let compressed = compressed.finish().map_err(S::Error::custom)?;
        serializer.serialize_str(&BASE64.encode(compressed))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        let compressed = BASE64.decode(input).map_err(D::Error::custom)?;
        let mut decompressed = Vec::new();
        ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decompressed)
            .map_err(D::Error::custom)?;
        Ok(decompressed)
    }
}

/// Enters Play without reproducing login, leaving every captured packet to the
/// normal production adapter and client driver.
#[derive(Debug)]
struct PlayAdapter(V770Adapter);

impl VersionAdapter for PlayAdapter {
    fn protocol_version(&self) -> i32 {
        self.0.protocol_version()
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        self.0.minecraft_versions()
    }

    fn supports(&self, protocol: i32) -> bool {
        self.0.supports(protocol)
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Play)])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        self.0.handle_packet(world, state, packet_id, payload)
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.0.encode_action(state, action)
    }
}

struct CapturedClient {
    handle: lodestone_client::ClientHandle,
    events: EventStream,
    peer: Connection<DuplexStream>,
    runtime: Runtime,
}

impl CapturedClient {
    fn new() -> Self {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("captured chunk client runtime");
        let (client_io, server_io) = memory_pair();
        let (handle, events) = runtime.block_on(async {
            ClientBuilder::new(
                ServerAddress {
                    host: "captured-chunk-lifecycle".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "CapturedChunkFixture".into(),
                    uuid: Uuid::nil(),
                },
                Box::new(PlayAdapter(V770Adapter::new())),
            )
            .connect_with(client_io)
        });
        Self {
            handle,
            events,
            peer: Connection::new(server_io),
            runtime,
        }
    }

    fn replay_until(
        &mut self,
        packet: &CapturedPacket,
        expected: fn(&ClientEvent) -> bool,
    ) -> Result<(), String> {
        self.runtime.block_on(async {
            self.peer
                .write_packet(packet.packet_id, &packet.payload)
                .await
                .map_err(|error| format!("write captured chunk packet: {error}"))?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = self
                        .events
                        .recv()
                        .await
                        .ok_or_else(|| {
                            "client driver ended before chunk lifecycle event".to_owned()
                        })?;
                    if expected(&event) {
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| "captured chunk lifecycle event deadline".to_owned())?
        })
    }

    fn is_chunk_loaded(&self, chunk: [i32; 2]) -> bool {
        self.handle.is_chunk_loaded(ChunkPos::new(chunk[0], chunk[1]))
    }

    fn block_at(&self, pos: [i32; 3]) -> Option<u32> {
        self.handle.block_at(BlockPos::new(pos[0], pos[1], pos[2]))
    }
}

impl Drop for CapturedClient {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

fn captured_fixture() -> Capture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/chunk_lifecycle_26_2.json");
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .expect("run the ignored capture acquisition test to create the external fixture"),
    )
    .expect("parse captured chunk lifecycle fixture")
}

/// Reads the two fixed-width coordinates at the start of a level-chunk body.
///
/// The packet's length-prefixed fields begin after these eight bytes, so using
/// a VarInt here can select a different column while still leaving a payload
/// that looks structurally valid to the rest of the capture loop.
fn chunk_pos_from_level_chunk(payload: &[u8]) -> (i32, i32) {
    let mut reader = Reader::new(payload);
    (
        reader.i32().expect("level chunk x"),
        reader.i32().expect("level chunk z"),
    )
}

fn is_chunk_loaded_event(event: &ClientEvent) -> bool {
    matches!(
        event,
        ClientEvent::ChunkLoaded { pos } if (pos.x, pos.z) == (CHUNK[0], CHUNK[1])
    )
}

fn is_block_update_event(event: &ClientEvent) -> bool {
    matches!(
        event,
        ClientEvent::SectionBlocksChanged { section, .. }
            if (section.x, section.z) == (CHUNK[0], CHUNK[1])
    )
}

fn is_chunk_unloaded_event(event: &ClientEvent) -> bool {
    matches!(
        event,
        ClientEvent::ChunkUnloaded { pos } if (pos.x, pos.z) == (CHUNK[0], CHUNK[1])
    )
}

#[test]
fn level_chunk_coordinate_probe_uses_fixed_width_integers() {
    let payload = [
        0x12, 0x34, 0x56, 0x78, // x = 0x12345678
        0x89, 0xab, 0xcd, 0xef, // z = 0x89abcdef
    ];
    let expected = (0x1234_5678_i32, i32::from_be_bytes([0x89, 0xab, 0xcd, 0xef]));
    assert_eq!(chunk_pos_from_level_chunk(&payload), expected);

    // This is the negative control for the old capture selector: a VarInt
    // reader consumes only the first byte of each coordinate and therefore
    // cannot identify this same packet as `expected`.
    let mut varint_reader = Reader::new(&payload);
    let varint_pair = (
        varint_reader.var_i32().expect("varint x"),
        varint_reader.var_i32().expect("varint z"),
    );
    assert_ne!(varint_pair, expected);
}

#[test]
fn captured_chunk_lifecycle_reaches_public_client_state() {
    let capture = captured_fixture();
    assert_eq!(capture.protocol, 776);
    assert_eq!(capture.chunk, CHUNK);
    assert_eq!(capture.update.pos, UPDATED_POS);
    assert_eq!(capture.update.state, UPDATED_STATE);
    assert_eq!(
        capture.packets.len(),
        3,
        "load, update, and unload are required"
    );
    assert_eq!(
        capture.packets[0].packet_id,
        play::clientbound::LEVEL_CHUNK_WITH_LIGHT
    );
    assert_eq!(
        capture.packets[1].packet_id,
        play::clientbound::BLOCK_UPDATE
    );
    assert_eq!(
        capture.packets[2].packet_id,
        play::clientbound::FORGET_LEVEL_CHUNK
    );

    let expected = state_id(&capture.update.state).expect("fixture state in generated data");
    let mut client = CapturedClient::new();
    client
        .replay_until(&capture.packets[0], is_chunk_loaded_event)
        .expect("captured chunk load replay");
    assert!(client.is_chunk_loaded(capture.chunk));
    client
        .replay_until(&capture.packets[1], is_block_update_event)
        .expect("captured block update replay");
    assert_eq!(client.block_at(capture.update.pos), Some(expected));
    client
        .replay_until(&capture.packets[2], is_chunk_unloaded_event)
        .expect("captured chunk unload replay");
    assert!(!client.is_chunk_loaded(capture.chunk));
    assert_eq!(client.block_at(capture.update.pos), None);
}

#[test]
fn omitting_captured_chunk_unload_preserves_the_loaded_update() {
    let capture = captured_fixture();
    let expected = state_id(&capture.update.state).expect("fixture state in generated data");
    let mut client = CapturedClient::new();
    client
        .replay_until(&capture.packets[0], is_chunk_loaded_event)
        .expect("captured chunk load replay");
    client
        .replay_until(&capture.packets[1], is_block_update_event)
        .expect("captured block update replay");
    assert!(
        client.is_chunk_loaded(capture.chunk),
        "without packet {}, the captured chunk must remain loaded",
        capture.packets[2].packet_id
    );
    assert_eq!(
        client.block_at(capture.update.pos),
        Some(expected),
        "without the unload, the captured update must remain visible through the public read model"
    );
}

/// Prints three unmodified payloads from the local external 26.2 oracle.
#[cfg(feature = "rcon-oracle")]
#[tokio::test]
#[ignore = "requires the headless 26.2 creative oracle on 25570/25571"]
async fn acquire_chunk_lifecycle_from_external_server() {
    use lodestone_fuzz::differential::{Action, WorldOracle, rcon::RconOracle};
    use lodestone_world::World;
    use tokio::net::TcpStream;

    async fn apply(
        conn: &mut Connection<TcpStream>,
        state: &mut ConnectionState,
        directive: Directive,
    ) {
        match directive {
            Directive::Send { packet_id, payload } => conn.write_packet(packet_id, &payload).await.expect("send"),
            Directive::SetState(next) => *state = next,
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            Directive::Disconnect(reason) => panic!("capture disconnected: {}", reason.to_plain_string()),
            _ => {}
        }
    }

    fn chunk_pos_from_forget(payload: &[u8]) -> (i32, i32) {
        let mut reader = Reader::new(payload);
        let packed = reader.i64().expect("forget chunk position");
        (packed as i32, (packed >> 32) as i32)
    }

    async fn next_matching(
        conn: &mut Connection<TcpStream>,
        adapter: &V770Adapter,
        state: &mut ConnectionState,
        world: &mut World,
        wanted: i32,
        matches: impl Fn(&[u8]) -> bool,
    ) -> CapturedPacket {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (packet_id, payload) = conn
                    .read_packet()
                    .await
                    .expect("capture read")
                    .expect("capture closed");
                let directives = adapter
                    .handle_packet(world, *state, packet_id, &payload)
                    .expect("capture decode");
                for directive in directives {
                    apply(conn, state, directive).await;
                }
                if *state == ConnectionState::Play && packet_id == wanted && matches(&payload) {
                    return CapturedPacket { packet_id, payload };
                }
            }
        })
        .await
        .expect("captured chunk lifecycle deadline")
    }

    let username = lodestone_testsupport::unique_username();
    let adapter = V770Adapter::new();
    let mut conn = tokio::time::timeout(
        Duration::from_secs(5),
        Connection::connect("127.0.0.1:25570"),
    )
    .await
    .expect("connect deadline")
    .expect("start scripts/live-oracles/creative.sh");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter
        .begin_login(
            &LoginProfile {
                username: username.clone(),
                uuid: Uuid::new_v4(),
            },
            &ServerAddress {
                host: "127.0.0.1".into(),
                port: 25570,
            },
        )
        .expect("login")
    {
        apply(&mut conn, &mut state, directive).await;
    }
    let mut world = World::new();
    let level_chunk = next_matching(
        &mut conn,
        &adapter,
        &mut state,
        &mut world,
        play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
        |payload| chunk_pos_from_level_chunk(payload) == (CHUNK[0], CHUNK[1]),
    )
    .await;

    let mut rcon = RconOracle::connect("127.0.0.1:25571", "lodestone", CAPTURE_ORIGIN)
        .expect("RCON");
    rcon.apply(&Action::RunCommand("forceload add 0 0".into()))
        .expect("force load capture chunk");
    rcon.apply(&Action::RunCommand(
        "setblock 8 100 8 minecraft:gold_block".into(),
    ))
    .expect("captured block update command");
    let block_update = next_matching(
        &mut conn,
        &adapter,
        &mut state,
        &mut world,
        play::clientbound::BLOCK_UPDATE,
        |_| true,
    )
    .await;
    rcon.apply(&Action::RunCommand(format!("tp {username} 1600 100 0")))
        .expect("move capture client away from original chunk");
    let forget = next_matching(
        &mut conn,
        &adapter,
        &mut state,
        &mut world,
        play::clientbound::FORGET_LEVEL_CHUNK,
        |payload| chunk_pos_from_forget(payload) == (CHUNK[0], CHUNK[1]),
    )
    .await;
    rcon.apply(&Action::RunCommand("setblock 8 100 8 minecraft:air".into()))
        .expect("capture cleanup");
    rcon.apply(&Action::RunCommand("forceload remove 0 0".into()))
        .expect("release capture chunk");

    let capture = Capture {
        source: "Unmodified payloads captured from the external 26.2 creative server on 25570, protocol 776. The capture client received chunk (0,0), then RCON set (8,100,8) to gold_block and moved that client to x=1600 so the server withdrew chunk (0,0). Capture entry point: acquire_chunk_lifecycle_from_external_server. The update annotation is the source command.".into(),
        protocol: 776,
        chunk: CHUNK,
        update: CapturedUpdate {
            pos: UPDATED_POS,
            state: UPDATED_STATE.into(),
        },
        packets: vec![level_chunk, block_update, forget],
    };
    let capture = serde_json::to_string_pretty(&capture).expect("capture JSON");
    if let Ok(path) = std::env::var("LODESTONE_CHUNK_LIFECYCLE_CAPTURE_OUT") {
        std::fs::write(&path, &capture).expect("write explicitly requested capture output");
        println!("CHUNK_LIFECYCLE_CAPTURE_WRITTEN={path}");
    } else {
        println!("CHUNK_LIFECYCLE_CAPTURE={capture}");
    }
}
