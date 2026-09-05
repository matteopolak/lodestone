//! In-memory integration coverage for the hosted protocol-340 family.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent, Rotation, Vec3};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v1_9::adapter_for;

const TARGET: BlockPos = BlockPos::new(8, 100, 8);

struct LegacyFixtureSource {
    column: Mutex<ChunkColumn>,
}

impl LegacyFixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(TARGET.x, TARGET.y, TARGET.z, "minecraft:dandelion");
        Self {
            column: Mutex::new(column),
        }
    }
}

impl ChunkSource for LegacyFixtureSource {
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
        username: "LegacyFixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    }
}

async fn assert_registry_selected_server_reaches_play_and_confirms_a_block_break(
    protocol_version: i32,
) {
    let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("selected legacy protocol must resolve to a hosted family");
    let source = Arc::new(LegacyFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, Arc::clone(&source), 0);
    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile(),
        Box::new(adapter_for(protocol_version)),
    )
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    if let Err(error) = handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
    {
        let failure = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(ClientEvent::SessionFailed { reason }) = events.recv().await {
                    return reason;
                }
            }
        })
        .await
        .ok();
        panic!("legacy login must reach Play: {error}; session failure: {failure:?}");
    }
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("the projected legacy chunk must arrive");
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
        .expect("the server block-update must replace the known block with air");

    handle.shutdown();
    server.shutdown().await;
}

async fn assert_teleport_confirmation_unblocks_movement(protocol_version: i32) {
    let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("hosted legacy protocol must resolve");
    let source = Arc::new(LegacyFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile(),
        Box::new(adapter_for(protocol_version)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle.wait_for_spawn(Duration::from_secs(10)).await.expect("must join Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("initial column must arrive");
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

#[tokio::test]
async fn registry_selected_protocol_340_reaches_play_and_confirms_a_block_break() {
    assert_registry_selected_server_reaches_play_and_confirms_a_block_break(340).await;
}

#[tokio::test]
async fn registry_selected_protocol_210_reaches_play_and_confirms_a_block_break() {
    assert_registry_selected_server_reaches_play_and_confirms_a_block_break(210).await;
}

#[tokio::test]
async fn registry_selected_protocol_110_reaches_play_and_confirms_a_block_break() {
    assert_registry_selected_server_reaches_play_and_confirms_a_block_break(110).await;
}

#[tokio::test]
async fn registry_selected_protocol_316_reaches_play_and_confirms_a_block_break() {
    assert_registry_selected_server_reaches_play_and_confirms_a_block_break(316).await;
}

#[tokio::test]
async fn teleport_confirmation_unblocks_protocol_340_movement() {
    assert_teleport_confirmation_unblocks_movement(340).await;
}

#[tokio::test]
async fn teleport_confirmation_unblocks_protocol_316_movement() {
    assert_teleport_confirmation_unblocks_movement(316).await;
}

#[tokio::test]
async fn teleport_confirmation_unblocks_protocol_210_movement() {
    assert_teleport_confirmation_unblocks_movement(210).await;
}

#[tokio::test]
async fn teleport_confirmation_unblocks_protocol_110_movement() {
    assert_teleport_confirmation_unblocks_movement(110).await;
}

#[test]
fn a_non_hosted_legacy_protocol_is_rejected_before_connection_setup() {
    assert!(lodestone_registry::server_protocol_for_protocol(109).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(209).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(315).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(341).is_none());
}
