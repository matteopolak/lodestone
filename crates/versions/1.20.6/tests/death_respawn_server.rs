//! Protocol-766 death/respawn acceptance through the hosted server.
//!
//! The adapter fixture and the server protocol are separate checks: this file
//! also drives the registry-selected production server and client, so a
//! correctly decoded packet cannot pass while the lifecycle consumer is dead.

use std::sync::Arc;
use std::time::Duration;

use lodestone_client::{ClientBuilder, PlayerLoadedPolicy, RespawnPolicy};
use lodestone_model::{route, ClientAction, ClientEvent, ConnectionState, Directive, ServerAddress, Vec3, VersionAdapter};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerProtocol, ServerDirective};
use lodestone_v1_20_6::{adapter_for, packet_ids, V766ServerProtocol, PROTOCOL_1_20_6};

struct AirSource;

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "fixture has an odd number of hex digits");
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).expect("fixture is hex"))
        .collect()
}

#[test]
fn literal_death_fixture_reaches_all_lifecycle_consumers() {
    let body = hex(
        "ac02000000113b7b227472616e736c617465223a2264656174682e61747461636b2e66616c6c222c2277697468223a5b7b2274657874223a225374657665227d5d7d",
    );
    let directives = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut lodestone_world::World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::DEATH_COMBAT_EVENT,
            &body,
        )
        .expect("literal protocol-766 death fixture decodes");
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one death event, got {directives:?}");
    };
    let ClientEvent::Death { message } = event else {
        panic!("expected Death, got {event:?}");
    };
    assert_eq!(message.to_plain_string(), "Steve fell from a high place");
    let consumers = route(event);
    assert!(consumers.client && consumers.session && consumers.shell);
}

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: &str) {}
}

#[test]
fn respawn_action_reaches_protocol_766_server_command_consumer() {
    let adapter = adapter_for(PROTOCOL_1_20_6);
    let Some((packet_id, payload)) = adapter
        .encode_action(ConnectionState::Play, &ClientAction::Respawn)
        .expect("protocol-766 adapter encodes respawn")
    else {
        panic!("respawn must have a serverbound packet");
    };
    assert_eq!(packet_id, packet_ids::play::serverbound::CLIENT_COMMAND);
    assert_eq!(payload, [0]);
    assert_eq!(
        V766ServerProtocol.decode(lodestone_core::State::Play, packet_id, &payload),
        ServerBound::ClientCommand { action: 0 }
    );
}

#[test]
fn death_and_respawn_encoders_use_literal_protocol_766_ids_and_shapes() {
    let protocol = V766ServerProtocol;
    let health = protocol.encode_set_health(0.0, 20, 5.0);
    let ServerDirective::Send { packet_id, payload } = health else {
        panic!("health must be sent");
    };
    assert_eq!(packet_id, packet_ids::play::clientbound::UPDATE_HEALTH);
    assert_eq!(payload.len(), 9, "f32 + VarInt + f32");

    let death = protocol.encode_player_combat_kill(300, &lodestone_model::Text::literal("fell"));
    let ServerDirective::Send { packet_id, payload } = death else {
        panic!("death notification must be sent");
    };
    assert_eq!(packet_id, packet_ids::play::clientbound::DEATH_COMBAT_EVENT);
    assert_eq!(&payload[..6], &[0xac, 0x02, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!(&payload[6..], b"\x0f{\"text\":\"fell\"}");

    let packets = protocol.encode_respawn_with_teleport_id(17, Vec3::new(8.0, 64.0, 8.0));
    assert_eq!(packets.len(), 2, "respawn sends the state frame then placement");
    assert!(matches!(packets[0], ServerDirective::Send { packet_id, .. } if packet_id == packet_ids::play::clientbound::RESPAWN));
    assert!(matches!(packets[1], ServerDirective::Send { packet_id, .. } if packet_id == packet_ids::play::clientbound::POSITION));
}

#[tokio::test]
async fn hosted_client_survives_a_lethal_fall_and_respawns_at_world_spawn() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL_1_20_6)
        .expect("protocol 766 is hosted");
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, Arc::new(AirSource), 0);
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".to_owned(), port: 0 },
        lodestone_model::LoginProfile {
            username: "Respawn766".to_owned(),
            uuid: uuid::Uuid::new_v4(),
        },
        Box::new(adapter_for(PROTOCOL_1_20_6)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Automatic)
    .respawn_policy(RespawnPolicy::Manual)
    .connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(10)).await.expect("join reaches spawn");
    handle.send_action(ClientAction::Move {
        pos: Vec3::new(8.0, 130.0, 8.0),
        rotation: lodestone_model::Rotation::default(),
        on_ground: false,
        horizontal_collision: false,
    }).expect("airborne move is queued");
    handle.send_action(ClientAction::Move {
        pos: Vec3::new(8.0, 100.0, 8.0),
        rotation: lodestone_model::Rotation::default(),
        on_ground: true,
        horizontal_collision: false,
    }).expect("lethal landing is queued");

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.expect("event stream remains open") {
                ClientEvent::Death { .. } => break,
                _ => {}
            }
        }
    }).await.expect("death event arrives");
    handle.wait_for(Duration::from_secs(10), |client| !client.is_alive())
        .await.expect("death updates the client lifecycle");
    let dead_position = handle.position().expect("dead player still has a position");
    assert_eq!(handle.health(), Some(0.0));

    handle.send_action(ClientAction::Respawn).expect("respawn is queued");
    handle.wait_for(Duration::from_secs(10), |client| {
        client.is_alive() && client.health() == Some(20.0)
    }).await.expect("respawn restores authoritative health");
    let respawn_position = handle.position().expect("respawn supplies a position");
    assert_ne!(respawn_position, dead_position, "respawn must restore world position");
    assert_eq!(respawn_position, Vec3::new(8.0, 64.0, 8.0));

    handle.send_action(ClientAction::Move {
        pos: Vec3::new(9.0, 64.0, 8.0),
        rotation: lodestone_model::Rotation::default(),
        on_ground: true,
        horizontal_collision: false,
    }).expect("post-respawn movement is queued");
    handle.wait_for(Duration::from_secs(10), |client| {
        client.position().is_some_and(|pos| pos.x == 9.0)
    }).await.expect("movement continues after respawn");

    handle.shutdown();
    server.shutdown().await;
}
