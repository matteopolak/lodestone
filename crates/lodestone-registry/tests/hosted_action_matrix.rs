//! Bounded end-to-end acceptance coverage for every registry-selected host.
//!
//! The individual version crates own their detailed wire fixtures. This test
//! deliberately owns the cross-family invariant instead: every row declared
//! hostable by `lodestone-registry` must accept its *real client adapter's*
//! Play action, dispatch it into the shared server, and retain a usable session
//! for the following movement action. Rows run serially against a one-column
//! fixture so adding a hosted revision scales linearly rather than multiplying
//! live servers or world generation work.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ConnectionState, Rotation, Vec3,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerProtocol};

const TARGET: BlockPos = BlockPos::new(8, 100, 8);
const DEADLINE: Duration = Duration::from_secs(10);

struct FixtureSource {
    column: Mutex<ChunkColumn>,
}

impl FixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
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

fn break_action() -> ClientAction {
    ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos: TARGET,
        face: BlockFace::Up,
        sequence: 0,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_registry_selected_host_consumes_a_real_adapter_action_and_keeps_play_usable() {
    let hosted = lodestone_registry::hosted_protocols();
    if hosted.is_empty() {
        // The registry's default build deliberately contains no protocol
        // family. Feature-enabled matrix invocations exercise the rows.
        return;
    }

    for protocol_version in hosted {
        let host = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .unwrap_or_else(|| panic!("hosted protocol {protocol_version} must resolve"));
        let adapter = lodestone_registry::adapter_for_protocol(protocol_version)
            .unwrap_or_else(|| panic!("hosted protocol {protocol_version} must have an adapter"));

        // Inspect the actual adapter frame before handing that same adapter to
        // the client driver. This keeps a registry selector returning a box
        // whose decoder ignores Play actions from passing merely by joining.
        let (packet_id, payload) = adapter
            .encode_action(ConnectionState::Play, &break_action())
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} must encode block breaking: {error}")
            })
            .unwrap_or_else(|| panic!("protocol {protocol_version} must emit a block-action frame"));
        assert!(
            matches!(
                host.decode(lodestone_core::State::Play, packet_id, &payload),
                ServerBound::BlockAction {
                    action: BlockActionKind::StartDestroy,
                    pos: TARGET,
                    face: BlockFace::Up,
                    ..
                }
            ),
            "protocol {protocol_version}'s real adapter frame must reach the server block-action consumer"
        );

        let source = Arc::new(FixtureSource::new());
        let (server, client_io) = IntegratedServer::open_in_memory(host, Arc::clone(&source), 0);
        let profile = LoginProfile {
            username: "Matrix".to_owned(),
            uuid: uuid::Uuid::new_v4(),
        };
        let address = ServerAddress {
            host: "memory".to_owned(),
            port: 0,
        };
        let (mut handle, _) = ClientBuilder::new(address, profile, adapter)
            .player_loaded_policy(PlayerLoadedPolicy::Manual)
            .connect_with(client_io);

        handle
            .wait_for_spawn(DEADLINE)
            .await
            .unwrap_or_else(|error| panic!("protocol {protocol_version} must reach Play: {error}"));
        handle
            .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), DEADLINE)
            .await
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} must receive the fixture column: {error}")
            });

        handle
            .send_action(break_action())
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} must send its block-breaking action: {error}")
            });
        let air = lodestone_data::block_states::air_state_id();
        handle
            .wait_for(DEADLINE, move |client| client.block_at(TARGET) == Some(air))
            .await
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} block action must update the client: {error}")
            });

        // A second, independently useful Play action must still cross the
        // same live connection after the state-changing action above. The
        // moved-to chunk is observable server output, so this is stronger than
        // merely checking that `send_action` accepted a queued packet.
        handle
            .move_to(Vec3::new(40.0, 100.0, 8.0), Rotation::default(), true, false)
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} must emit movement after breaking: {error}")
            });
        handle
            .wait_for_chunk(lodestone_client::ChunkPos::new(2, 0), DEADLINE)
            .await
            .unwrap_or_else(|error| {
                panic!("protocol {protocol_version} must stream after post-action movement: {error}")
            });

        handle.shutdown();
        server.shutdown().await;
    }
}
