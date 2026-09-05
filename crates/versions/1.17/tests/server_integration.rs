use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ChatKind, ClientAction, ClientEvent, ConnectionState,
    Rotation, Vec3, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v1_17::adapter_for;

const TARGET: BlockPos = BlockPos::new(8, 100, 8);

fn assert_adapter_block_use_reaches_host(protocol_version: i32) {
    let adapter = adapter_for(protocol_version);
    let action = ClientAction::UseItemOn {
        hand: lodestone_model::Hand::Off,
        pos: BlockPos::new(5, -10, -7),
        face: BlockFace::South,
        cursor: Vec3f {
            x: 0.25,
            y: 1.0,
            z: 0.75,
        },
        inside_block: true,
        sequence: 17,
    };
    let Some((packet_id, payload)) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("the era adapter must encode a block use")
    else {
        panic!("block use must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("the hosted protocol must resolve from the registry");
    assert_eq!(
        host.decode(lodestone_core::State::Play, packet_id, &payload),
        lodestone_server::ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f {
                x: 0.25,
                y: 1.0,
                z: 0.75,
            },
            sequence: 0,
            hand: 1,
        },
        "the adapter and registry-selected host must agree on the server consumer input"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        lodestone_server::ServerBound::Ignored,
        "the same bytes must not bypass the Play-state gate"
    );
}

#[test]
fn adapter_block_use_reaches_protocol_756_host_consumer() {
    assert_adapter_block_use_reaches_host(756);
}

#[test]
fn adapter_block_use_reaches_protocol_758_host_consumer() {
    assert_adapter_block_use_reaches_host(758);
}

fn assert_adapter_chat_reaches_host(protocol_version: i32) {
    let adapter = adapter_for(protocol_version);
    let Some((packet_id, payload)) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendChat {
                text: "adapter legacy chat".to_owned(),
            },
        )
        .expect("the era adapter must encode chat")
    else {
        panic!("chat must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("the hosted protocol must resolve from the registry");
    assert_eq!(
        host.decode(lodestone_core::State::Play, packet_id, &payload),
        lodestone_server::ServerBound::Chat {
            message: "adapter legacy chat".to_owned(),
            timestamp_millis: 0,
            salt: 0,
            signature: None,
        },
        "the adapter and registry-selected host must agree on legacy chat"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        lodestone_server::ServerBound::Ignored,
        "legacy chat must remain unavailable before Play"
    );
}

#[test]
fn adapter_chat_reaches_protocol_756_host_consumer() {
    assert_adapter_chat_reaches_host(756);
}

#[test]
fn adapter_chat_reaches_protocol_758_host_consumer() {
    assert_adapter_chat_reaches_host(758);
}

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

async fn assert_registry_selected_host_echoes_legacy_chat(protocol_version: i32) {
    let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("the hosted protocol must resolve from the registry");
    let source = Arc::new(FixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let username = format!("ChatFixture{protocol_version}");
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "memory".to_owned(),
            port: 0,
        },
        LoginProfile {
            username: username.clone(),
            uuid: uuid::Uuid::new_v4(),
        },
        Box::new(adapter_for(protocol_version)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("legacy chat fixture reaches Play");
    handle
        .send_action(ClientAction::SendChat {
            text: "end to end \"quoted\" chat".to_owned(),
        })
        .expect("joined client accepts chat");

    let expected = format!("<{username}> end to end \"quoted\" chat");
    let (text, kind) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::Chat { text, kind, .. }) = events.recv().await {
                return (text.to_plain_string(), kind);
            }
        }
    })
    .await
    .expect("the host must echo chat before the deadline");
    assert_eq!(text, expected);
    assert_eq!(kind, ChatKind::System);

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn registry_selected_protocol_756_echoes_legacy_chat_to_the_client_event_stream() {
    assert_registry_selected_host_echoes_legacy_chat(756).await;
}

#[tokio::test]
async fn registry_selected_protocol_758_echoes_legacy_chat_to_the_client_event_stream() {
    assert_registry_selected_host_echoes_legacy_chat(758).await;
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
async fn registry_selected_protocol_756_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(756)
        .expect("protocol 756 must resolve to the hosted family");
    let source = Arc::new(FixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, Arc::clone(&source), 0);
    let profile = LoginProfile {
        username: "Fixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let address = ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    };
    let (mut handle, _) = ClientBuilder::new(address, profile, Box::new(adapter_for(756)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-756 login reaches Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-756 chunk arrives");
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
        .expect("block update reaches the protocol-756 client");

    handle
        .move_to(Vec3::new(24.0, 100.0, 8.0), Rotation::default(), true, false)
        .expect("the 1.17 client emits a position packet");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(1, 0), Duration::from_secs(10))
        .await
        .expect("the protocol-756 host recenters the view after movement");

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn registry_selected_protocol_758_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(758)
        .expect("protocol 758 must resolve to the hosted family");
    let source = Arc::new(FixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, Arc::clone(&source), 0);
    let profile = LoginProfile {
        username: "Fixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let address = ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    };
    let (mut handle, _) = ClientBuilder::new(address, profile, Box::new(adapter_for(758)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-758 login reaches Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-758 chunk arrives");
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
        .expect("block update reaches the protocol-758 client");

    handle
        .move_to(Vec3::new(24.0, 100.0, 8.0), Rotation::default(), true, false)
        .expect("the 1.18 client emits a position packet");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(1, 0), Duration::from_secs(10))
        .await
        .expect("the protocol-758 host recenters the view after movement");

    handle.shutdown();
    server.shutdown().await;
}

#[test]
fn protocol_755_is_not_hosted() {
    assert!(lodestone_registry::server_protocol_for_protocol(755).is_none());
}
