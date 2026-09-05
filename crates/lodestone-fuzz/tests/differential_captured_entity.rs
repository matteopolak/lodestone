//! A server-authored entity lifecycle, replayed through the public client state.
#![cfg(feature = "v26-2")]

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, ConnectionState, Directive, LoginProfile,
    ServerAddress, VersionAdapter};
use lodestone_model::{AdapterError, ClientAction, Vec3, WorldSink};
use lodestone_net::{Connection, memory_pair};
use lodestone_v26_2::V770Adapter;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const START: Vec3 = Vec3 { x: 0.25, y: 100.5, z: 0.75 };
const END: Vec3 = Vec3 { x: 10.25, y: 101.5, z: 1.75 };
const ENTITY_UUID: Uuid = Uuid::from_u128((123_u128 << 96) | (456_u128 << 64) | (789_u128 << 32) | 1011);

#[derive(Debug, Serialize, Deserialize)]
struct Capture {
    protocol: i32,
    entity_id: i32,
    packets: Vec<CapturedPacket>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedPacket {
    packet_id: i32,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct PlayAdapter(V770Adapter);

impl VersionAdapter for PlayAdapter {
    fn protocol_version(&self) -> i32 { self.0.protocol_version() }
    fn minecraft_versions(&self) -> &'static [&'static str] { self.0.minecraft_versions() }
    fn supports(&self, protocol: i32) -> bool { self.0.supports(protocol) }
    fn begin_login(&self, _: &LoginProfile, _: &ServerAddress) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Play)])
    }
    fn handle_packet(&self, world: &mut dyn WorldSink, state: ConnectionState,
        packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        self.0.handle_packet(world, state, packet_id, payload)
    }
    fn encode_action(&self, state: ConnectionState, action: &ClientAction)
        -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.0.encode_action(state, action)
    }
}

async fn replay_lifecycle(capture: &Capture, omit_move: bool) {
    assert_eq!(capture.protocol, 776);
    assert_eq!(capture.packets.len(), 3, "spawn, move, removal are all required");
    let (client_io, server_io) = memory_pair();
    let (mut client, mut events) = ClientBuilder::new(
        ServerAddress { host: "captured-entity".into(), port: 0 },
        LoginProfile { username: "EntityReplay".into(), uuid: Uuid::nil() },
        Box::new(PlayAdapter(V770Adapter::new())),
    ).connect_with(client_io);
    let mut peer = Connection::new(server_io);
    assert!(client.entity(capture.entity_id).is_none());
    for (index, packet) in capture.packets.iter().enumerate() {
        if index == 1 && omit_move {
            let actual = client.entity(capture.entity_id).expect("spawn reached client");
            assert_eq!(actual.position, START);
            assert_ne!(actual.position, END, "omitted movement must fail the endpoint oracle");
            continue;
        }
        peer.write_packet(packet.packet_id, &packet.payload).await.expect("write capture");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.expect("client event stream ended");
                let matches = match event {
                    ClientEvent::EntitySpawned { entity_id, .. } => index == 0 && entity_id == capture.entity_id,
                    ClientEvent::EntityMoved { entity_id, .. } => index == 1 && entity_id == capture.entity_id,
                    ClientEvent::EntityRemoved { entity_ids } => index == 2 && entity_ids.contains(&capture.entity_id),
                    _ => false,
                };
                if matches { break; }
            }
        }).await.expect("captured lifecycle event deadline");
        if index < 2 {
            let actual = client.entity(capture.entity_id).expect("event must reach public entity state");
            assert_eq!(actual.uuid, Some(ENTITY_UUID));
            assert_eq!(actual.entity_type.to_string(), "minecraft:armor_stand");
            assert_eq!(actual.position, if index == 0 { START } else { END });
        } else {
            assert!(client.entity(capture.entity_id).is_none(), "removal must reach public state");
        }
    }
    client.shutdown();
}

#[tokio::test]
async fn captured_entity_spawn_move_remove_reaches_public_state() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/entity_lifecycle_26_2.json");
    let capture: Capture = serde_json::from_str(&std::fs::read_to_string(path)
        .expect("run the ignored capture acquisition test to create the external fixture"))
        .expect("captured lifecycle JSON");
    replay_lifecycle(&capture, false).await;
    replay_lifecycle(&capture, true).await;
}

/// Prints the three unmodified server payloads as JSON for fixture review.
/// The only writes to the oracle affect this test's player and tagged stand.
#[cfg(feature = "rcon-oracle")]
#[tokio::test]
#[ignore = "requires the headless 26.2 creative oracle on 25570/25571"]
async fn acquire_entity_lifecycle_from_external_server() {
    use lodestone_core::Writer;
    use lodestone_fuzz::differential::{Action, WorldOracle, rcon::RconOracle};
    use lodestone_v26_2::packet_ids::play;
    use lodestone_world::World;
    use tokio::net::TcpStream;

    async fn apply(conn: &mut Connection<TcpStream>, state: &mut ConnectionState, directive: Directive) {
        match directive {
            Directive::Send { packet_id, payload } => conn.write_packet(packet_id, &payload).await.expect("send"),
            Directive::SetState(next) => *state = next,
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            Directive::Disconnect(reason) => panic!("capture disconnected: {}", reason.to_plain_string()),
            _ => {}
        }
    }

    let username = lodestone_testsupport::unique_username();
    let adapter = V770Adapter::new();
    let mut conn = tokio::time::timeout(Duration::from_secs(5), Connection::connect("127.0.0.1:25570"))
        .await.expect("connect deadline").expect("start scripts/live-oracles/creative.sh");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(
        &LoginProfile { username: username.clone(), uuid: Uuid::new_v4() },
        &ServerAddress { host: "127.0.0.1".into(), port: 25570 },
    ).expect("login") {
        apply(&mut conn, &mut state, directive).await;
    }
    let mut world = World::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (id, payload) = conn.read_packet().await.expect("join read").expect("join closed");
            if state == ConnectionState::Play && id == play::clientbound::CHUNK_BATCH_FINISHED {
                let mut writer = Writer::default();
                writer.f32(32.0);
                conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &writer.into_vec()).await.expect("batch ack");
                break;
            }
            for directive in adapter.handle_packet(&mut world, state, id, &payload).expect("join decode") {
                apply(&mut conn, &mut state, directive).await;
            }
        }
    }).await.expect("join deadline");
    let mut rcon = RconOracle::connect("127.0.0.1:25571", "lodestone", (0, 0, 0)).expect("RCON");
    for command in [
        "kill @e[tag=lodestone_fuzz_lifecycle]".to_owned(),
        format!("gamemode spectator {username}"),
        format!("tp {username} 0 100 0"),
        "summon minecraft:armor_stand 0.25 100.5 0.75 {NoGravity:1b,UUID:[I;123,456,789,1011],Tags:[\"lodestone_fuzz_lifecycle\"]}".to_owned(),
    ] {
        rcon.apply(&Action::RunCommand(command)).expect("capture setup command");
    }
    let mut capture = Capture { protocol: 776, entity_id: -1, packets: vec![] };
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        while capture.packets.len() < 3 {
            let (id, payload) = conn.read_packet().await.expect("capture read").expect("capture closed");
            if id == play::clientbound::CHUNK_BATCH_FINISHED {
                let mut writer = Writer::default();
                writer.f32(32.0);
                conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &writer.into_vec()).await.expect("batch ack");
            }
            let directives = adapter.handle_packet(&mut world, state, id, &payload).expect("capture decode");
            let mut matched = false;
            for directive in directives {
                if let Directive::Emit(ref event) = directive {
                    matched |= match event {
                        ClientEvent::EntitySpawned { entity_id, uuid, pos, .. }
                            if capture.packets.is_empty() && *uuid == Some(ENTITY_UUID) => {
                                assert_eq!(*pos, START);
                                capture.entity_id = *entity_id;
                                true
                            }
                        ClientEvent::EntityMoved { entity_id, .. } =>
                            capture.packets.len() == 1 && *entity_id == capture.entity_id,
                        ClientEvent::EntityRemoved { entity_ids } =>
                            capture.packets.len() == 2 && entity_ids.contains(&capture.entity_id),
                        _ => false,
                    };
                }
                apply(&mut conn, &mut state, directive).await;
            }
            if matched {
                capture.packets.push(CapturedPacket { packet_id: id, payload });
                let command = if capture.packets.len() == 1 {
                    "tp @e[tag=lodestone_fuzz_lifecycle,limit=1] 10.25 101.5 1.75"
                } else {
                    "kill @e[tag=lodestone_fuzz_lifecycle]"
                };
                rcon.apply(&Action::RunCommand(command.into())).expect("capture action");
            }
        }
    }).await;
    rcon.apply(&Action::RunCommand("kill @e[tag=lodestone_fuzz_lifecycle]".into())).expect("cleanup stand");
    result.expect("capture lifecycle deadline");
    replay_lifecycle(&capture, false).await;
    replay_lifecycle(&capture, true).await;
    println!("ENTITY_LIFECYCLE_CAPTURE={}", serde_json::to_string(&capture).expect("capture JSON"));
}
