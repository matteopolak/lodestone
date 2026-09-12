//! Production integrated-server proof that a portal trip reaches generated
//! Nether terrain and that the served payload preserves the external bedrock
//! shell oracle. The fixture source is deliberately cheap Overworld terrain;
//! only the destination is the bundled Nether generator under test.

use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer};
use lodestone_model::{GameMode, Vec3};
use lodestone_net::{Connection, Transport};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_server::dimension::Dimension;
use lodestone_server::world_storage::{WorldStorage, WorldStorageBackend};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::packets::game::{GameLogin, Respawn};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::{LoginFinished, LoginHello};
use lodestone_world::LightData;
use tokio::io::DuplexStream;
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;
const DIMENSION_CHANGE: i32 = 40;
const FORGET_CHUNK: i32 = 41;
const CHUNK_CACHE_CENTER: i32 = 42;
const PLAYER_MOVED: i32 = 43;

const SEED: i64 = -195_764_831;
const TARGET_CHUNK: (i32, i32) = (1, 2);
const TARGET_POSITION: (f64, f64, f64) = (192.5, 64.0, 264.5);

const ORACLE: &str =
    include_str!("../../lodestone-worldgen/tests/support/nether_vanilla_oracle.txt");

/// A minimal source whose only non-air surface is a cheap stone floor. The
/// portal is placed in the target Overworld chunk so the production portal
/// path scales the position into the oracle's Nether chunk `(1, 2)`.
#[derive(Debug, Clone, Copy)]
struct PortalWorld;

impl ChunkSource for PortalWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 256);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 63, z, "minecraft:stone");
            }
        }
        if (cx, cz) == (12, 16) {
            column.set_block(0, 64, 8, "minecraft:nether_portal[axis=x]");
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// The connection codec carries only the fields needed by this gate. Its
/// chunk packet is intentionally a transparent bedrock grid so the test can
/// inspect exact destination state bits rather than querying the source a
/// second time.
#[derive(Debug, Clone, Copy)]
struct ProbeProtocol;

impl ProbeProtocol {
    fn encode_column(cx: i32, cz: i32, column: &ChunkColumn) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.var_i32(cx);
        writer.var_i32(cz);
        writer.var_i32(column.min_y);
        writer.var_i32(column.height);
        for y in column.min_y..column.min_y + column.height {
            for z in 0..16 {
                for x in 0..16 {
                    writer.u8(u8::from(column.block_state(x, y, z) == "minecraft:bedrock"));
                }
            }
        }
        writer.as_slice().to_vec()
    }
}

impl ServerProtocol for ProbeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut reader = Reader::new(payload);
                ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == PLAYER_MOVED => {
                let mut reader = Reader::new(payload);
                ServerBound::PlayerMoved {
                    x: reader.f64().expect("x"),
                    y: reader.f64().expect("y"),
                    z: reader.f64().expect("z"),
                    rotation: None,
                    on_ground: true,
                }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        let mut writer = Writer::default();
        writer.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: writer.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Self::encode_column(cx, cz, column),
        }
    }

    fn end_chunk_batch(&self, count: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(count);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(cx);
        writer.var_i32(cz);
        ServerDirective::Send {
            packet_id: FORGET_CHUNK,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(cx);
        writer.var_i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK_CACHE_CENTER,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let mut writer = Writer::default();
        writer.string(dimension);
        writer.f64(spawn.x);
        writer.f64(spawn.y);
        writer.f64(spawn.z);
        writer.u8(mode as u8);
        vec![ServerDirective::Send {
            packet_id: DIMENSION_CHANGE,
            payload: writer.as_slice().to_vec(),
        }]
    }
}

fn oracle_bedrock_masks(cx: i32, cz: i32) -> Vec<(u8, u8)> {
    let line = ORACLE
        .lines()
        .find(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("bedrock")
                && fields.next().and_then(|value| value.parse().ok()) == Some(cx)
                && fields.next().and_then(|value| value.parse().ok()) == Some(cz)
        })
        .unwrap_or_else(|| panic!("external oracle has no bedrock chunk ({cx}, {cz})"));
    let hex = line.split_whitespace().nth(3).expect("bedrock hex payload");
    assert_eq!(hex.len(), 256 * 4);
    (0..256)
        .map(|index| {
            (
                u8::from_str_radix(&hex[index * 4..index * 4 + 2], 16).expect("floor mask"),
                u8::from_str_radix(&hex[index * 4 + 2..index * 4 + 4], 16)
                    .expect("roof mask"),
            )
        })
        .collect()
}

async fn login(client: &mut Connection<DuplexStream>) {
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut writer = Writer::default();
    writer.string("NetherOracle");
    client
        .write_packet(LOGIN_START, writer.as_slice())
        .await
        .expect("login start");
    let (packet_id, payload) = client
        .read_packet()
        .await
        .expect("login success read")
        .expect("login success packet");
    assert_eq!(packet_id, LOGIN_SUCCESS);
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.string(16).expect("login username"), "NetherOracle");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("configuration finished");
}

const CTX: Ctx = Ctx { version: 776 };

/// Completes the real protocol's login/configuration seam and returns the
/// packets up to the first play chunk. Compression is switched at the packet
/// that announces it, as an authenticated client does.
async fn real_login<T: Transport>(
    client: &mut Connection<T>,
    username: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    let mut writer = Writer::default();
    Intention {
        protocol_version: 776,
        host: "localhost".to_owned(),
        port: 25565,
        next_state: 2,
    }
    .encode(&mut writer, CTX)
    .expect("encode handshake");
    client
        .write_packet(handshaking::serverbound::INTENTION, writer.as_slice())
        .await
        .expect("write handshake");

    let mut writer = Writer::default();
    LoginHello {
        name: username.to_owned(),
        profile_id: uuid,
    }
    .encode(&mut writer, CTX)
    .expect("encode login hello");
    client
        .write_packet(login::serverbound::HELLO, writer.as_slice())
        .await
        .expect("write login hello");

    // The first login-phase read is login_finished; the helper also handles
    // the optional compression announcement that precedes it.
    let (packet_id, payload) = read_login_packet(client).await;
    assert_eq!(packet_id, login::clientbound::LOGIN_FINISHED);
    let mut reader = Reader::new(&payload);
    LoginFinished::decode(&mut reader, CTX).expect("decode login finished");

    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("write login acknowledgement");
    let _ = read_login_packet(client).await;
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .expect("write finish configuration");

    let mut packets = Vec::new();
    let mut saw_chunk = false;
    loop {
        let packet = tokio::time::timeout(Duration::from_secs(120), client.read_packet())
            .await
            .expect("initial real-protocol packet timeout")
            .expect("initial real-protocol packet read")
            .expect("initial real-protocol packet");
        let packet_id = packet.0;
        let payload = packet.1;
        if packet_id == play::clientbound::PLAYER_POSITION {
            let (teleport_id, _) = decode_player_position(&payload);
            let mut ack = Writer::default();
            ack.var_i32(teleport_id);
            client
                .write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice())
                .await
                .expect("accept initial teleport");
        }
        if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
            saw_chunk = true;
        }
        if packet_id == play::clientbound::CHUNK_BATCH_FINISHED && saw_chunk {
            let mut ack = Writer::default();
            ack.f32(64.0);
            client
                .write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, ack.as_slice())
                .await
                .expect("acknowledge initial chunk batch");
            client
                .write_packet(play::serverbound::PLAYER_LOADED, &[])
                .await
                .expect("announce loaded player");
            packets.push((packet_id, payload));
            return packets;
        }
        packets.push((packet_id, payload));
    }
}

async fn read_login_packet<T: Transport>(client: &mut Connection<T>) -> (i32, Vec<u8>) {
    loop {
        let (packet_id, payload) = client
            .read_packet()
            .await
            .expect("login packet read")
            .expect("login packet");
        if packet_id == login::clientbound::LOGIN_COMPRESSION {
            let threshold = Reader::new(&payload)
                .var_i32()
                .expect("login compression threshold");
            client.set_compression(threshold);
            continue;
        }
        return (packet_id, payload);
    }
}

fn decode_player_position(payload: &[u8]) -> (i32, (f64, f64, f64)) {
    let mut reader = Reader::new(payload);
    let teleport_id = reader.var_i32().expect("teleport id");
    let x = reader.f64().expect("teleport x");
    let y = reader.f64().expect("teleport y");
    let z = reader.f64().expect("teleport z");
    (teleport_id, (x, y, z))
}

fn decode_real_chunk(payload: &[u8], shape: &ChunkShape) -> LevelChunkWithLight {
    let mut reader = Reader::new(payload);
    let chunk = LevelChunkWithLight::decode(&mut reader, shape).expect("decode real chunk payload");
    reader.ensure_empty().expect("real chunk has no trailing bytes");
    chunk
}

fn assert_bedrock_shell(chunk: &LevelChunkWithLight, expected: &[(u8, u8)]) {
    assert_eq!(chunk.column.min_y(), 0);
    assert_eq!(chunk.column.max_y(), 256);
    assert_eq!(chunk.light.light_section_count(), 18);
    assert!(
        (0..chunk.light.light_section_count())
            .all(|section| matches!(chunk.light.sky(section), LightData::Missing)),
        "Nether's real light payload must not contain sky-light arrays"
    );
    for z in 0..16 {
        for x in 0..16 {
            for y in 0..5 {
                let want = expected[z * 16 + x].0 & (1 << y) != 0;
                assert_eq!(
                    lodestone_data::block_states::StateId::new(chunk.column.get_block(x, y, z))
                        .expect("decoded block state id")
                        .name()
                        == "minecraft:bedrock",
                    want,
                    "served Nether bedrock mismatch at local ({x},{y},{z})"
                );
            }
            for y in 123..128 {
                let want = expected[z * 16 + x].1 & (1 << (y - 123)) != 0;
                assert_eq!(
                    lodestone_data::block_states::StateId::new(chunk.column.get_block(x, y, z))
                        .expect("decoded block state id")
                        .name()
                        == "minecraft:bedrock",
                    want,
                    "served Nether bedrock mismatch at local ({x},{y},{z})"
                );
            }
        }
    }
}

async fn move_real<T: Transport>(client: &mut Connection<T>, position: (f64, f64, f64)) {
    let mut writer = Writer::default();
    writer.f64(position.0);
    writer.f64(position.1);
    writer.f64(position.2);
    writer.u8(1);
    client
        .write_packet(play::serverbound::MOVE_PLAYER_POS, writer.as_slice())
        .await
        .expect("write real movement");
}

fn persistent_test_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lodestone-758-nether-real-{}",
        Uuid::new_v4().as_u128()
    ));
    std::fs::create_dir_all(&dir).expect("create persistent test world");
    dir
}

/// The production connection fixture uses the actual protocol-776 login,
/// respawn, chunk and light codecs. It travels into generated Nether terrain,
/// restarts while still in that dimension, and then reuses the persisted exit
/// portal to return to the original Overworld portal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_protocol_nether_payload_and_restart_return_round_trip() {
    let _ = lodestone_server::overworld_generator(SEED);
    let dir = persistent_test_dir();
    let native_dir = dir.join("native");
    let username = "NetherReal";
    let uuid = Uuid::from_u128(0x7580_0000_0000_0000_0000_0000_0000_0001);
    let expected = oracle_bedrock_masks(TARGET_CHUNK.0, TARGET_CHUNK.1);
    let mut nether_portal_position = None;

    {
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_dir.clone(),
        })
        .expect("open native player locator store");
        let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
            V770ServerProtocol,
            &dir,
            PortalWorld,
            0,
            256,
            (0..=0, 0..=0),
            (8, 8),
            0,
            0,
            Duration::from_secs(3600),
            storage,
        )
        .expect("open persistent real-protocol world");
        server
            .world_state()
            .set_rule("players_nether_portal_default_delay", "0")
            .expect("portal delay rule");

        let mut client = Connection::new(client_end);
        let initial = real_login(&mut client, username, uuid).await;
        let game_login = initial
            .iter()
            .find(|(packet_id, _)| *packet_id == play::clientbound::LOGIN)
            .map(|(_, payload)| {
                let mut reader = Reader::new(payload);
                GameLogin::decode(&mut reader, CTX).expect("decode initial game login")
            })
            .expect("real join must include a game login");
        assert_eq!(game_login.dimension, Dimension::Overworld.key());

        let travel_started = Instant::now();
        for _ in 0..12 {
            move_real(&mut client, TARGET_POSITION).await;
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        let mut saw_respawn = false;
        let mut saw_nether_chunk = false;
        while !saw_nether_chunk {
            let (packet_id, payload) = tokio::time::timeout(
                Duration::from_secs(120),
                client.read_packet(),
            )
            .await
            .expect("real Nether travel packet timeout")
            .expect("real Nether travel packet read")
            .expect("real Nether travel packet");
            match packet_id {
                play::clientbound::RESPAWN => {
                    let mut reader = Reader::new(&payload);
                    let respawn = Respawn::decode(&mut reader, CTX).expect("decode Nether respawn");
                    assert_eq!(respawn.dimension, Dimension::Nether.key());
                    saw_respawn = true;
                }
                play::clientbound::PLAYER_POSITION => {
                    let (teleport_id, position) = decode_player_position(&payload);
                    nether_portal_position = Some(position);
                    let mut ack = Writer::default();
                    ack.var_i32(teleport_id);
                    client
                        .write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice())
                        .await
                        .expect("accept Nether teleport");
                }
                play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
                    let mut header = Reader::new(&payload);
                    let cx = header.i32().expect("Nether chunk x");
                    let cz = header.i32().expect("Nether chunk z");
                    if (cx, cz) == TARGET_CHUNK {
                        assert!(saw_respawn, "Nether chunk must follow the dimension respawn");
                        let chunk = decode_real_chunk(&payload, &ChunkShape::nether_or_end_1_21());
                        assert_bedrock_shell(&chunk, &expected);
                        eprintln!("real Nether generation and first served payload: {:?}", travel_started.elapsed());
                        saw_nether_chunk = true;
                    }
                }
                _ => {}
            }
        }
        assert!(
            nether_portal_position.is_some(),
            "real travel must provide the persisted Nether portal position"
        );
        std::mem::forget(client);
        server.shutdown().await;
    }

    {
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_dir.clone(),
        })
        .expect("reopen native player locator store");
        let (server, client_end, _world) = IntegratedServer::open_persistent_with_mobs_and_storage(
            V770ServerProtocol,
            &dir,
            PortalWorld,
            0,
            256,
            (0..=0, 0..=0),
            (8, 8),
            0,
            0,
            Duration::from_secs(3600),
            storage,
        )
        .expect("reopen persistent real-protocol world");
        server
            .world_state()
            .set_rule("players_nether_portal_default_delay", "0")
            .expect("portal delay rule after restart");
        let mut client = Connection::new(client_end);
        let initial = real_login(&mut client, username, uuid).await;
        let game_login = initial
            .iter()
            .find(|(packet_id, _)| *packet_id == play::clientbound::LOGIN)
            .map(|(_, payload)| {
                let mut reader = Reader::new(payload);
                GameLogin::decode(&mut reader, CTX).expect("decode restarted game login")
            })
            .expect("restarted real join must include a game login");
        // The initial login frame establishes the primary world's registry;
        // a saved non-primary dimension follows it with the ordinary respawn
        // frame before any destination chunks are sent.
        assert_eq!(game_login.dimension, Dimension::Overworld.key());

        let respawn = initial
            .iter()
            .find(|(packet_id, _)| *packet_id == play::clientbound::RESPAWN)
            .map(|(_, payload)| {
                let mut reader = Reader::new(payload);
                Respawn::decode(&mut reader, CTX).expect("decode restarted Nether respawn")
            })
            .expect("restart must emit a Nether respawn");
        assert_eq!(respawn.dimension, Dimension::Nether.key());
        let restarted_target = initial
            .iter()
            .find(|(packet_id, payload)| {
                if *packet_id != play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
                    return false;
                }
                let mut reader = Reader::new(payload);
                matches!((reader.i32(), reader.i32()), (Ok(cx), Ok(cz)) if (cx, cz) == TARGET_CHUNK)
            })
            .map(|(_, payload)| decode_real_chunk(payload, &ChunkShape::nether_or_end_1_21()))
            .expect("restart must stream the saved Nether target chunk");
        assert_bedrock_shell(&restarted_target, &expected);
        let return_position = nether_portal_position.expect("first trip captured portal position");
        for _ in 0..12 {
            move_real(&mut client, return_position).await;
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        let mut saw_overworld = false;
        let mut saw_overworld_chunk = false;
        while !saw_overworld_chunk {
            let (packet_id, payload) = tokio::time::timeout(
                Duration::from_secs(120),
                client.read_packet(),
            )
            .await
            .expect("return Overworld packet timeout")
            .expect("return Overworld packet read")
            .expect("return Overworld packet");
            match packet_id {
                play::clientbound::RESPAWN => {
                    let mut reader = Reader::new(&payload);
                    let respawn = Respawn::decode(&mut reader, CTX).expect("decode Overworld respawn");
                    assert_eq!(respawn.dimension, Dimension::Overworld.key());
                    saw_overworld = true;
                }
                play::clientbound::PLAYER_POSITION => {
                    let (teleport_id, _) = decode_player_position(&payload);
                    let mut ack = Writer::default();
                    ack.var_i32(teleport_id);
                    client
                        .write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice())
                        .await
                        .expect("accept Overworld teleport");
                }
                play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
                    let mut header = Reader::new(&payload);
                    let cx = header.i32().expect("return Overworld chunk x");
                    let cz = header.i32().expect("return Overworld chunk z");
                    if (cx, cz) == (12, 16) {
                        let _ = decode_real_chunk(&payload, &ChunkShape::overworld_1_21());
                        saw_overworld_chunk = true;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_overworld, "return chunk must follow an Overworld respawn");
        std::mem::forget(client);
        server.shutdown().await;
    }

    std::fs::remove_dir_all(&dir).expect("remove persistent test world");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_portal_trip_serves_external_nether_bedrock_payload() {
    // The production sibling factory takes the active world seed, which the
    // normal Overworld constructor publishes before a real world opens.
    let _ = lodestone_server::overworld_generator(SEED);
    let (server, client_end) = IntegratedServer::open_in_memory(ProbeProtocol, PortalWorld, 0);
    server
        .world_state()
        .set_rule("players_nether_portal_default_delay", "0")
        .expect("portal delay rule");

    let mut client = Connection::new(client_end);
    login(&mut client).await;

    // Initial Overworld join at the origin: batch start, one chunk, batch end.
    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("initial batch start read")
        .expect("initial batch start packet");
    assert_eq!(packet_id, CHUNK_BATCH_START);
    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("initial chunk read")
        .expect("initial chunk packet");
    assert_eq!(packet_id, CHUNK);
    let (packet_id, payload) = client
        .read_packet()
        .await
        .expect("initial batch end read")
        .expect("initial batch end packet");
    assert_eq!(packet_id, CHUNK_BATCH_FINISHED);
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.var_i32().expect("initial batch count"), 1);

    // Move into the pre-built portal. The first resulting chunk is the cheap
    // Overworld recenter; the later chunk with the external shell is the
    // production Nether sibling created by `with_nether`.
    let mut writer = Writer::default();
    writer.f64(TARGET_POSITION.0);
    writer.f64(TARGET_POSITION.1);
    writer.f64(TARGET_POSITION.2);
    client
        .write_packet(PLAYER_MOVED, writer.as_slice())
        .await
        .expect("portal movement");

    let expected = oracle_bedrock_masks(TARGET_CHUNK.0, TARGET_CHUNK.1);
    let mut saw_dimension_change = false;
    let mut saw_forget_after_dimension_change = false;
    let mut saw_cache_center_after_dimension_change = false;
    let mut saw_target = false;
    let mut packets = 0usize;
    while !saw_target {
        packets += 1;
        assert!(packets < 16, "portal trip emitted no target Nether chunk");
        let (packet_id, packet) = tokio::time::timeout(
            Duration::from_secs(120),
            client.read_packet(),
        )
        .await
        .expect("portal packet timeout")
        .expect("portal packet read")
        .expect("portal packet");
        match packet_id {
            DIMENSION_CHANGE => {
                let mut packet_reader = Reader::new(&packet);
                assert_eq!(
                    packet_reader.string(64).expect("destination dimension"),
                    Dimension::Nether.key()
                );
                let _ = packet_reader.f64().expect("destination x");
                let _ = packet_reader.f64().expect("destination y");
                let _ = packet_reader.f64().expect("destination z");
                let _ = packet_reader.u8().expect("destination mode");
                saw_dimension_change = true;
            }
            FORGET_CHUNK if saw_dimension_change => {
                saw_forget_after_dimension_change = true;
            }
            CHUNK_CACHE_CENTER if saw_dimension_change => {
                assert!(
                    saw_forget_after_dimension_change,
                    "the transition must precede old-view forgets, and forgets must precede the destination cache center"
                );
                saw_cache_center_after_dimension_change = true;
            }
            CHUNK => {
                let mut packet_reader = Reader::new(&packet);
                let cx = packet_reader.var_i32().expect("chunk x");
                let cz = packet_reader.var_i32().expect("chunk z");
                let min_y = packet_reader.var_i32().expect("chunk minimum y");
                let height = packet_reader.var_i32().expect("chunk height");
                if (cx, cz) == TARGET_CHUNK {
                    assert!(saw_dimension_change, "Nether chunk preceded dimension change");
                    assert_eq!((min_y, height), (0, 256));
                    for y in min_y..min_y + height {
                        for z in 0..16 {
                            for x in 0..16 {
                                let bedrock = packet_reader.u8().expect("chunk bedrock bit") != 0;
                                let want = if y < 5 {
                                    expected[z * 16 + x].0 & (1 << y) != 0
                                } else if (123..128).contains(&y) {
                                    expected[z * 16 + x].1 & (1 << (y - 123)) != 0
                                } else {
                                    continue;
                                };
                                assert_eq!(
                                    bedrock, want,
                                    "served Nether bedrock mismatch at chunk ({cx},{cz}) local ({x},{y},{z})"
                                );
                            }
                        }
                    }
                    assert_eq!(packet_reader.remaining(), 0, "trailing Nether chunk payload");
                    saw_target = true;
                }
            }
            _ => {}
        }
    }

    assert!(saw_dimension_change, "portal trip emitted no dimension change");
    assert!(
        saw_forget_after_dimension_change,
        "portal transition emitted no old-view forget after the dimension change"
    );
    assert!(
        saw_cache_center_after_dimension_change,
        "portal transition emitted no destination cache center"
    );
    let (packet_id, payload) = tokio::time::timeout(Duration::from_secs(120), client.read_packet())
        .await
        .expect("Nether batch end timeout")
        .expect("Nether batch end read")
        .expect("Nether batch end packet");
    assert_eq!(packet_id, CHUNK_BATCH_FINISHED);
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.var_i32().expect("Nether batch count"), 1);
    assert_eq!(reader.remaining(), 0);

    drop(client);
    server.shutdown().await;
}
