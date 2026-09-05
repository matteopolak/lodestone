//! External block-update packets replayed through the real adapter and public world state.
#![cfg(feature = "v26-2")]

use std::time::Duration;

use lodestone_client::{
    BlockPos, ClientBuilder, ClientEvent, ConnectionState, Directive, EventStream, LoginProfile,
    ServerAddress, VersionAdapter,
};
use lodestone_data::block_states::{air_state_id, state_id};
use lodestone_model::{AdapterError, ClientAction, WorldSink};
use lodestone_net::{Connection, memory_pair};
use lodestone_v26_2::{V770Adapter, packet_ids::play};
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind,
};
use serde::{Deserialize, Serialize};
use tokio::io::DuplexStream;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

const CHUNK: [i32; 2] = [0, 0];
#[cfg(feature = "rcon-oracle")]
const CAPTURE_ORIGIN: (i32, i32, i32) = (0, 100, 0);
const MIN_Y: i32 = -64;
const SECTION_COUNT: usize = 24;

#[derive(Debug, Serialize, Deserialize)]
struct Capture {
    source: String,
    protocol: i32,
    chunk: [i32; 2],
    packets: Vec<CapturedPacket>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedPacket {
    packet_id: i32,
    payload: Vec<u8>,
    writes: Vec<CapturedWrite>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedWrite {
    pos: [i32; 3],
    state: String,
}

/// Starts the normal client driver in Play without reimplementing the full
/// login exchange. Packet handling itself remains the production 26.2 adapter.
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
            .expect("captured block client runtime");
        let (client_io, server_io) = memory_pair();
        let (handle, events) = runtime.block_on(async {
            ClientBuilder::new(
                ServerAddress {
                    host: "captured-block-update".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "CapturedBlockFixture".into(),
                    uuid: Uuid::nil(),
                },
                Box::new(PlayAdapter(V770Adapter::new())),
            )
            .connect_with(client_io)
        });
        let chunk = LoadedChunk::new(
            ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air_state_id(),
                0,
            ),
            ColumnLight::new(SECTION_COUNT),
            Heightmaps::new(),
            Vec::new(),
        );
        handle
            .chunk_world_write()
            .write()
            .load(WorldChunkPos::new(CHUNK[0], CHUNK[1]), chunk);
        Self {
            handle,
            events,
            peer: Connection::new(server_io),
            runtime,
        }
    }

    fn replay(&mut self, packet: &CapturedPacket) -> Result<(), String> {
        self.runtime.block_on(async {
            self.peer
                .write_packet(packet.packet_id, &packet.payload)
                .await
                .map_err(|error| format!("write captured block packet: {error}"))?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = self
                        .events
                        .recv()
                        .await
                        .ok_or_else(|| "client driver ended before block update".to_owned())?;
                    if matches!(event, ClientEvent::SectionBlocksChanged { .. }) {
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| "captured block update event deadline".to_owned())?
        })
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
        .join("tests/fixtures/block_updates_26_2.json");
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .expect("run the ignored capture acquisition test to create the external fixture"),
    )
    .expect("parse captured block-update fixture")
}

#[test]
fn captured_block_updates_reach_the_public_client_world() {
    let capture = captured_fixture();
    assert_eq!(capture.protocol, 776);
    assert_eq!(capture.chunk, CHUNK);
    assert_eq!(capture.packets.len(), 2, "single and bulk updates are required");
    assert_eq!(capture.packets[0].packet_id, play::clientbound::BLOCK_UPDATE);
    assert_eq!(
        capture.packets[1].packet_id,
        play::clientbound::SECTION_BLOCKS_UPDATE
    );

    let mut client = CapturedClient::new();
    for packet in &capture.packets {
        client.replay(packet).expect("captured block update replay");
        for write in &packet.writes {
            let expected = state_id(&write.state)
                .unwrap_or_else(|| panic!("fixture state {} is absent from generated data", write.state));
            assert_eq!(
                client.block_at(write.pos),
                Some(expected),
                "captured packet {} must update {} at {:?} in the public client world",
                packet.packet_id,
                write.state,
                write.pos
            );
        }
    }
}

#[test]
fn omitting_a_captured_bulk_update_leaves_its_cells_at_the_baseline() {
    let capture = captured_fixture();
    let single = &capture.packets[0];
    let omitted = &capture.packets[1];
    let mut client = CapturedClient::new();
    client.replay(single).expect("captured single block update replay");
    for write in &omitted.writes {
        let expected = state_id(&write.state).expect("fixture state in generated data");
        assert_eq!(
            client.block_at(write.pos),
            Some(air_state_id()),
            "without packet {}, {:?} must remain at the loaded all-air baseline",
            omitted.packet_id,
            write.pos
        );
        assert_ne!(
            client.block_at(write.pos),
            Some(expected),
            "the endpoint oracle must reject the state written by the omitted update"
        );
    }
}

/// Prints unmodified update payloads from the local external 26.2 oracle.
/// The annotations are the commands sent to that oracle, not a re-encoding.
#[cfg(feature = "rcon-oracle")]
#[tokio::test]
#[ignore = "requires the headless 26.2 creative oracle on 25570/25571"]
async fn acquire_block_updates_from_external_server() {
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

    async fn read_until(
        conn: &mut Connection<TcpStream>,
        adapter: &V770Adapter,
        state: &mut ConnectionState,
        world: &mut lodestone_world::World,
        wanted: i32,
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
                if packet_id == wanted {
                    return CapturedPacket {
                        packet_id,
                        payload,
                        writes: Vec::new(),
                    };
                }
            }
        })
        .await
        .expect("captured block update deadline")
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
                username,
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
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (packet_id, payload) = conn.read_packet().await.expect("join read").expect("join closed");
            let directives = adapter
                .handle_packet(&mut world, state, packet_id, &payload)
                .expect("join decode");
            for directive in directives {
                apply(&mut conn, &mut state, directive).await;
            }
            if state == ConnectionState::Play && packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                break;
            }
        }
    })
    .await
    .expect("join deadline");

    let mut rcon = RconOracle::connect("127.0.0.1:25571", "lodestone", CAPTURE_ORIGIN)
        .expect("RCON");
    rcon.apply(&Action::RunCommand("forceload add 0 0".into()))
        .expect("force load capture chunk");
    rcon.apply(&Action::RunCommand("setblock 8 100 8 minecraft:gold_block".into()))
        .expect("single block command");
    let mut single = read_until(
        &mut conn,
        &adapter,
        &mut state,
        &mut world,
        play::clientbound::BLOCK_UPDATE,
    )
    .await;
    single.writes = vec![CapturedWrite {
        pos: [8, 100, 8],
        state: "minecraft:gold_block".into(),
    }];

    rcon.apply(&Action::RunCommand(
        "fill 9 100 8 10 100 8 minecraft:diamond_block".into(),
    ))
    .expect("bulk block command");
    let mut bulk = read_until(
        &mut conn,
        &adapter,
        &mut state,
        &mut world,
        play::clientbound::SECTION_BLOCKS_UPDATE,
    )
    .await;
    bulk.writes = vec![
        CapturedWrite {
            pos: [9, 100, 8],
            state: "minecraft:diamond_block".into(),
        },
        CapturedWrite {
            pos: [10, 100, 8],
            state: "minecraft:diamond_block".into(),
        },
    ];
    rcon.apply(&Action::RunCommand("fill 8 100 8 10 100 8 minecraft:air".into()))
        .expect("capture cleanup");
    rcon.apply(&Action::RunCommand("forceload remove 0 0".into()))
        .expect("release capture chunk");

    let capture = Capture {
        source: "Unmodified payloads captured from the external 26.2 creative server on 25570, protocol 776. The capture client had chunk (0,0) loaded, then RCON set (8,100,8) to gold_block and filled (9,100,8)..(10,100,8) with diamond_block. Capture entry point: acquire_block_updates_from_external_server. State annotations are the source commands.".into(),
        protocol: 776,
        chunk: CHUNK,
        packets: vec![single, bulk],
    };
    println!(
        "BLOCK_UPDATE_CAPTURE={}",
        serde_json::to_string(&capture).expect("capture JSON")
    );
}
