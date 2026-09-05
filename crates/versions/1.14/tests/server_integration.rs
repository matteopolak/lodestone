//! In-memory integration coverage for the hosted protocol-498 selector.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, ClientAction};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v1_14::adapter_for;

const PROTOCOL: i32 = 498;
const TARGET: BlockPos = BlockPos::new(8, 100, 8);

struct FixtureSource {
    column: Mutex<ChunkColumn>,
}

impl FixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(0, 256);
        column.set_block(TARGET.x, TARGET.y, TARGET.z, "minecraft:dandelion");
        Self {
            column: Mutex::new(column),
        }
    }
}

impl ChunkSource for FixtureSource {
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

#[tokio::test]
async fn registry_selected_protocol_498_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("protocol 498 must resolve to a hosted family");
    let source = Arc::new(FixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let profile = LoginProfile {
        username: "Fixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let address = ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    };
    let (mut handle, _events) = ClientBuilder::new(address, profile, Box::new(adapter_for(PROTOCOL)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-498 login must reach Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-498 chunk must arrive");
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
        .expect("protocol-498 block update must replace the known block with air");

    handle.shutdown();
    server.shutdown().await;
}

#[test]
fn neighbouring_protocols_are_rejected_before_connection_setup() {
    assert!(lodestone_registry::server_protocol_for_protocol(497).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(499).is_none());
}
