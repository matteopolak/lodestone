//! The server entity capability must be a live producer, not only a typed
//! wrapper around an isolated mob test.
//!
//! This drives `ServerEntityApi` through the public in-memory serving
//! constructor. The API's `EntitySource` implementation is therefore the
//! source consumed by the real entity streamer, and the protocol callbacks
//! below are the egress witness for spawn, update, removal and directed player
//! teleport.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::str::FromStr;
use std::time::Duration;

use lodestone_core::State;
use lodestone_model::{ResourceKey, Rotation, Vec3};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, EntityMutation, EntityMutationResult, EntityObservation,
    EntitySnapshot, IntegratedServer, PlayerRegistry, ServerBound, ServerDirective,
    ServerEntityApi, ServerProtocol,
};
use uuid::Uuid;

const MOVE_PACKET: i32 = 40;

#[derive(Debug, Default)]
struct Seen {
    adds: AtomicUsize,
    updates: AtomicUsize,
    removals: AtomicUsize,
    teleports: Mutex<Vec<Vec3>>,
}

#[derive(Debug)]
struct ProbeProtocol(Arc<Seen>);

impl ServerProtocol for ProbeProtocol {
    fn decode(&self, state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == 0 => ServerBound::Handshake { next_state: State::Login },
            State::Login if packet_id == 0 => ServerBound::LoginStart {
                username: "EntityApi".to_owned(),
                uuid: Uuid::new_v4(),
            },
            State::Login if packet_id == 3 => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == 3 => ServerBound::ConfigurationFinished,
            State::Play if packet_id == MOVE_PACKET => ServerBound::PlayerMoved {
                x: 8.0,
                y: 8.0,
                z: 8.0,
                rotation: None,
                on_ground: true,
            },
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send { packet_id: 2, payload: Vec::new() }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> { Vec::new() }

    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }

    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective { ServerDirective::None }

    fn encode_add_entity(&self, _entity: &EntitySnapshot) -> ServerDirective {
        self.0.adds.fetch_add(1, Ordering::SeqCst);
        ServerDirective::None
    }

    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        _current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        self.0.updates.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }

    fn encode_remove_entity(&self, _ids: &[i32]) -> ServerDirective {
        self.0.removals.fetch_add(1, Ordering::SeqCst);
        ServerDirective::None
    }

    fn encode_teleport_with_id(
        &self,
        _teleport_id: i32,
        x: f64,
        y: f64,
        z: f64,
        _yaw: f32,
        _pitch: f32,
    ) -> ServerDirective {
        self.0
            .teleports
            .lock()
            .expect("teleport witness lock")
            .push(Vec3::new(x, y, z));
        ServerDirective::None
    }
}

#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

async fn nudge(client: &mut Connection<tokio::io::DuplexStream>) {
    client
        .write_packet(MOVE_PACKET, &[])
        .await
        .expect("movement nudge");
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn typed_mutations_use_authoritative_stores_and_reach_protocol_egress() {
    let mobs = lodestone_server::MobHandle::default();
    let players = PlayerRegistry::new();
    let api = ServerEntityApi::new(mobs.clone(), players.clone());
    let mob_id = api.spawn(
        ResourceKey::from_str("minecraft:cow").expect("cow key"),
        Vec3::new(4.0, 8.0, 4.0),
    );
    let seen = Arc::new(Seen::default());
    let (server, client_io) = IntegratedServer::open_in_memory_with_entities(
        ProbeProtocol(Arc::clone(&seen)),
        FlatWorld,
        api.clone(),
        0,
    );
    let mut client = Connection::new(client_io);
    client.write_packet(0, &[2]).await.expect("handshake");
    client.write_packet(0, &[]).await.expect("login start");
    client.read_packet().await.expect("login success read");
    client.write_packet(3, &[]).await.expect("login acknowledgement");
    client.write_packet(3, &[]).await.expect("configuration finished");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.adds.load(Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "spawn never reached protocol egress");
        nudge(&mut client).await;
    }

    let observed = api.observe(mob_id).expect("spawned mob observation");
    assert_eq!(observed.id, mob_id);
    assert_eq!(observed.entity_type, ResourceKey::from_str("minecraft:cow").unwrap());
    assert_eq!(observed.position, Vec3::new(4.0, 8.0, 4.0));
    assert_eq!(observed.health, Some(20.0));

    assert_eq!(
        api.mutate(mob_id, EntityMutation::ApplyKnockback(Vec3::new(0.25, 0.0, 0.0))),
        EntityMutationResult::Applied
    );
    let update_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.updates.load(Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < update_deadline, "knockback never reached entity update egress");
        nudge(&mut client).await;
    }

    let player_id = players
        .candidates()
        .into_iter()
        .next()
        .expect("the joined player must be registered")
        .entity_id;
    let EntityObservation { uuid, .. } = api.observe(player_id).expect("player observation");
    assert_eq!(
        api.mutate(
            player_id,
            EntityMutation::Teleport {
                position: Vec3::new(11.0, 9.0, -2.0),
                rotation: Some(Rotation::new(90.0, 5.0)),
            },
        ),
        EntityMutationResult::Applied
    );
    assert_eq!(players.candidates().into_iter().find(|p| p.uuid == uuid).map(|p| p.entity_id), Some(player_id));
    let teleport_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.teleports.lock().expect("teleport witness lock").is_empty() {
        assert!(tokio::time::Instant::now() < teleport_deadline, "player teleport never reached protocol egress");
        nudge(&mut client).await;
    }
    assert_eq!(seen.teleports.lock().unwrap().as_slice(), &[Vec3::new(11.0, 9.0, -2.0)]);

    assert_eq!(api.mutate(mob_id, EntityMutation::Despawn), EntityMutationResult::Applied);
    let remove_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.removals.load(Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < remove_deadline, "despawn never reached removal egress");
        nudge(&mut client).await;
    }

    assert_eq!(api.mutate(i32::MAX, EntityMutation::Despawn), EntityMutationResult::UnknownEntity);
    server.shutdown().await;
}
