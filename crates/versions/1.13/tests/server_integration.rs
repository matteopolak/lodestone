//! In-memory integration coverage for the hosted protocol-404 family.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ChatKind, ClientAction, ClientEvent, ConnectionState,
    Hand, Rotation, Vec3, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound};
use lodestone_v1_13::{adapter, V404Adapter};

const PROTOCOL: i32 = 404;
const TARGET: BlockPos = BlockPos::new(8, 100, 8);

struct FlatFixtureSource {
    column: Mutex<ChunkColumn>,
}

impl FlatFixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(TARGET.x, TARGET.y, TARGET.z, "minecraft:dandelion");
        Self {
            column: Mutex::new(column),
        }
    }
}

impl ChunkSource for FlatFixtureSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.column.lock().expect("fixture column lock poisoned").clone()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column
            .lock()
            .expect("fixture column lock poisoned")
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: &str) {
        self.column
            .lock()
            .expect("fixture column lock poisoned")
            .set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
    }
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "FlatFixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    }
}

#[test]
fn registry_selected_404_host_consumes_adapter_block_use() {
    let action = ClientAction::UseItemOn {
        hand: Hand::Off,
        pos: BlockPos::new(5, -10, -7),
        face: BlockFace::South,
        cursor: Vec3f::new(0.25, 1.0, 0.75),
        inside_block: true,
        sequence: 17,
    };
    let Some((packet_id, payload)) = V404Adapter::new()
        .encode_action(ConnectionState::Play, &action)
        .expect("the protocol-404 adapter encodes a block use")
    else {
        panic!("protocol-404 block use must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("protocol 404 must resolve to its hosted family");
    assert_eq!(
        host.decode(lodestone_core::State::Play, packet_id, &payload),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f::new(0.25, 1.0, 0.75),
            sequence: 0,
            hand: 1,
        },
        "the registry-selected host must deliver the adapter's action to the shared placement consumer"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        ServerBound::Ignored,
        "a Play block-use body must not bypass the connection-state gate"
    );
}

#[tokio::test]
async fn registry_selected_404_host_echoes_legacy_chat_to_the_client_event_stream() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("protocol 404 must resolve to its hosted family");
    let source = Arc::new(FlatFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, mut events) = ClientBuilder::new(address(), profile(), Box::new(adapter()))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-404 chat fixture reaches Play");
    handle
        .send_action(ClientAction::SendChat {
            text: "legacy \"chat\"".to_owned(),
        })
        .expect("joined client accepts chat");

    let (text, kind) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::Chat { text, kind, .. }) = events.recv().await {
                return (text.to_plain_string(), kind);
            }
        }
    })
    .await
    .expect("the hosted server must echo legacy chat before the deadline");
    assert_eq!(text, "<FlatFixture> legacy \"chat\"");
    assert_eq!(kind, ChatKind::System);

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn registry_selected_404_server_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("protocol 404 must resolve to a hosted family");
    let source = Arc::new(FlatFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, Arc::clone(&source), 0);
    let (mut handle, _events) = ClientBuilder::new(address(), profile(), Box::new(adapter()))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-404 login must reach Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("the protocol-404 chunk must arrive");
    let flower = lodestone_data::block_states::state_id("minecraft:dandelion")
        .expect("fixture state exists");
    assert_eq!(handle.block_at(TARGET), Some(flower));

    handle
        .send_action(ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: TARGET,
            face: BlockFace::Up,
            sequence: 0,
        })
        .expect("joined client accepts a block action");
    let air = lodestone_data::block_states::air_state_id();
    handle
        .wait_for(Duration::from_secs(10), move |client| client.block_at(TARGET) == Some(air))
        .await
        .expect("the server block update must replace the known block with air");

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn teleport_confirmation_unblocks_hosted_protocol_404_movement() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("protocol 404 must resolve to a hosted family");
    let source = Arc::new(FlatFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, _events) = ClientBuilder::new(address(), profile(), Box::new(adapter()))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);
    handle.wait_for_spawn(Duration::from_secs(10)).await.expect("must join Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("initial column must arrive");
    // The adapter's teleport-confirm reply is queued while it applies the
    // initial position; this move reaches the server only after that reply.
    handle.send_action(ClientAction::Move {
        pos: Vec3::new(24.0, 100.0, 8.0),
        rotation: Rotation { yaw: 0.0, pitch: 0.0 },
        on_ground: true,
        horizontal_collision: false,
    }).expect("joined client accepts movement");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(1, 0), Duration::from_secs(10))
        .await
        .expect("confirmed teleport must unblock movement and recenter the view");
    handle.shutdown();
    server.shutdown().await;
}

#[test]
fn neighbouring_protocols_are_rejected_before_connection_setup() {
    assert!(lodestone_registry::server_protocol_for_protocol(403).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(405).is_none());
}
