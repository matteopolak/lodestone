use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ConnectionState, Rotation, Vec3, Vec3f,
    VersionAdapter,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerProtocol};
use lodestone_v1_21_11::adapter_for;

const TARGET: BlockPos = BlockPos::new(8, 100, 8);

#[test]
fn adapter_block_use_reaches_the_registry_selected_host_consumer() {
    let adapter = adapter_for(774);
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
        .expect("the protocol-774 adapter must encode a block use")
    else {
        panic!("block use must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(774)
        .expect("protocol 774 must resolve to the hosted family");
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
            sequence: 17,
            hand: 1,
        },
        "the real adapter and registry-selected host must agree on the server consumer input"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        lodestone_server::ServerBound::Ignored,
        "the same bytes must not bypass the configuration-to-Play handoff"
    );
}

#[test]
fn adapter_air_use_reaches_the_registry_selected_host_consumer() {
    let adapter = adapter_for(774);
    let action = ClientAction::UseItem {
        hand: lodestone_model::Hand::Off,
        rotation: Rotation::new(90.0, -15.0),
        sequence: 17,
    };
    let Some((packet_id, payload)) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("the protocol-774 adapter must encode an air use")
    else {
        panic!("air use must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(774)
        .expect("protocol 774 must resolve to the hosted family");
    assert_eq!(
        host.decode(lodestone_core::State::Play, packet_id, &payload),
        lodestone_server::ServerBound::UseItem {
            hand: 1,
            yaw: 90.0,
            pitch: -15.0,
        },
        "the adapter's held-item use reaches the server's projectile and consumption input"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        lodestone_server::ServerBound::Ignored,
        "the same bytes must not bypass the configuration-to-Play handoff"
    );
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

struct BoundaryFixtureSource {
    center: Mutex<ChunkColumn>,
}

impl ChunkSource for BoundaryFixtureSource {
    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        ((-1..=1).contains(&cx) && (-1..=1).contains(&cz) && (cx, cz) != (0, 0))
            .then(|| self.column(cx, cz))
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        if (cx, cz) == (0, 0) {
            self.center.lock().expect("boundary column lock poisoned").clone()
        } else {
            ChunkColumn::new(-64, 384)
        }
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if (x.div_euclid(16), z.div_euclid(16)) == (0, 0) {
            self.center
                .lock()
                .expect("boundary column lock poisoned")
                .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_owned()
        } else {
            "minecraft:air".to_owned()
        }
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: &str) {
        assert_eq!((x.div_euclid(16), z.div_euclid(16)), (0, 0));
        self.center
            .lock()
            .expect("boundary column lock poisoned")
            .set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
    }
}

#[tokio::test]
async fn registry_selected_protocol_774_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(774)
        .expect("protocol 774 must resolve to the hosted family");
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
    let (mut handle, _) = ClientBuilder::new(address, profile, Box::new(adapter_for(774)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-774 login reaches Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-774 chunk arrives");
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
        .expect("block update reaches the protocol-774 client");

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn movement_recenters_the_hosted_view_onto_the_next_chunk() {
    let protocol = lodestone_registry::server_protocol_for_protocol(774).unwrap();
    let source = Arc::new(FixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, _) = ClientBuilder::new(
        ServerAddress { host: "memory".to_owned(), port: 0 },
        LoginProfile { username: "MoveFixture".to_owned(), uuid: uuid::Uuid::new_v4() },
        Box::new(adapter_for(774)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle.wait_for_spawn(Duration::from_secs(10)).await.unwrap();
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .unwrap();
    handle.send_action(ClientAction::Move {
        pos: Vec3::new(24.0, 100.0, 8.0),
        rotation: Rotation::new(90.0, 0.0),
        on_ground: true,
        horizontal_collision: false,
    })
    .unwrap();
    let next = lodestone_client::ChunkPos::new(1, 0);
    handle
        .wait_for_chunk(next, Duration::from_secs(10))
        .await
        .expect("the position packet recenters the hosted view on chunk (1, 0)");
    let flower = lodestone_data::block_states::state_id("minecraft:dandelion").unwrap();
    assert_eq!(handle.block_at(BlockPos::new(24, 100, 8)), Some(flower));
    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn initial_and_live_border_light_use_the_open_east_column() {
    let target = BlockPos::new(15, 100, 8);
    let probe = BlockPos::new(15, 100, 7);
    assert!(lodestone_registry::server_protocol_for_protocol(774)
        .expect("protocol 774 must resolve to the hosted family")
        .uses_cross_column_light());
    let mut center = ChunkColumn::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            center.set_block(x, 101, z, "minecraft:stone");
        }
    }
    center.set_block(target.x, target.y, target.z, "minecraft:dirt");
    let source = Arc::new(BoundaryFixtureSource { center: Mutex::new(center) });
    let protocol = lodestone_registry::server_protocol_for_protocol(774).unwrap();
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, _) = ClientBuilder::new(
        ServerAddress { host: "memory".to_owned(), port: 0 },
        LoginProfile { username: "BorderLight".to_owned(), uuid: uuid::Uuid::new_v4() },
        Box::new(adapter_for(774)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    let chunk = lodestone_client::ChunkPos::new(0, 0);
    handle.wait_for_spawn(Duration::from_secs(10)).await.unwrap();
    handle.wait_for_chunk(chunk, Duration::from_secs(10)).await.unwrap();
    assert_eq!(
        handle.section_light(chunk, 11).unwrap().sky_at(probe.x as usize, 4, probe.z as usize),
        14,
        "the initial chunk must retain the open-east path with all eight neighbours resident"
    );
    handle.send_action(ClientAction::Move {
        pos: Vec3::new(12.0, 100.0, 8.0),
        rotation: Rotation::new(90.0, 0.0),
        on_ground: true,
        horizontal_collision: false,
    }).unwrap();
    handle.send_action(ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos: target,
        face: BlockFace::Up,
        sequence: 29,
    }).unwrap();
    handle.send_action(ClientAction::BlockAction {
        action: BlockActionKind::StopDestroy,
        pos: target,
        face: BlockFace::Up,
        sequence: 30,
    }).unwrap();
    handle.wait_for(Duration::from_secs(10), move |client| {
        client.block_at(target) == Some(lodestone_data::block_states::air_state_id())
    }).await.expect("the client observes the border block break");
    handle.wait_for(Duration::from_secs(10), move |client| {
        client.section_light(chunk, 11).is_some_and(|light| {
            light.sky_at(target.x as usize, 4, target.z as usize) == 14
        })
    }).await.expect("the relight opens the broken east-border cell with all eight resident");
    handle.shutdown();
    server.shutdown().await;
}

#[test]
fn protocol_773_is_not_hosted() {
    assert!(lodestone_registry::server_protocol_for_protocol(773).is_none());
}

#[tokio::test]
async fn hosted_lighting_reaches_client_and_extinguishes_after_a_block_break() {
    let optics: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../vendor/minecraft-data/data/pc/1.21.11/blocks.json"
    )).unwrap();
    let property = |name: &str, field: &str| {
        optics.as_array().unwrap().iter().find(|block| block["name"] == name).unwrap()[field]
            .as_u64().unwrap()
    };
    assert_eq!(property("torch", "emitLight"), 14);
    assert_eq!(property("stone", "filterLight"), 15);
    assert_eq!(property("air", "filterLight"), 0);
    let protocol = lodestone_registry::server_protocol_for_protocol(774).unwrap();
    let mut room = ChunkColumn::new(-64, 384);
    for y in 98..=104 {
        for z in 5..=11 {
            for x in 5..=11 {
                if y == 98 || y == 104 || x == 5 || x == 11 || z == 5 || z == 11 {
                    room.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
    }
    room.set_block(8, 100, 8, "minecraft:torch");
    let source = Arc::new(FixtureSource { column: Mutex::new(room) });
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, _) = ClientBuilder::new(
        ServerAddress { host: "memory".to_owned(), port: 0 },
        LoginProfile { username: "LightFixture".to_owned(), uuid: uuid::Uuid::new_v4() },
        Box::new(adapter_for(774)),
    ).player_loaded_policy(PlayerLoadedPolicy::Manual).connect_with(client_io);
    handle.wait_for_spawn(Duration::from_secs(10)).await.unwrap();
    let pos = lodestone_client::ChunkPos::new(0, 0);
    handle.wait_for_chunk(pos, Duration::from_secs(10)).await.unwrap();
    // World y=100 is block section 10 and local y=4; its light section is 11.
    let light = handle.section_light(pos, 11).expect("received chunk light");
    assert_eq!(light.sky_at(8, 4, 8), 0, "sealed room bbox=(5,98,5)..(11,104,11)");
    assert_eq!(light.sky_at(8, 9, 8), 15, "open sky bbox=(8,105,8)..(8,105,8)");
    assert_eq!(light.block_at(8, 4, 8), 14, "torch bbox=(8,100,8)..(8,100,8)");
    assert_eq!(light.block_at(9, 4, 8), 13, "adjacent air bbox=(9,100,8)..(9,100,8)");
    handle.send_action(ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos: TARGET,
        face: BlockFace::Up,
        sequence: 23,
    }).unwrap();
    handle.wait_for(Duration::from_secs(10), move |client| {
        client.block_at(TARGET) == Some(lodestone_data::block_states::air_state_id())
            && client.section_light(pos, 11).is_some_and(|light| light.block_at(9, 4, 8) == 0)
    }).await.expect("torch removal extinguishes bbox=(9,100,8)..(9,100,8)");
    handle.shutdown();
    server.shutdown().await;
}
