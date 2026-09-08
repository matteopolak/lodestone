use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    AnimationAction, BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent,
    ConnectionState, ContainerClickType, ContainerSlotChange, Hand, ItemStack, Rotation, Vec3,
    Vec3f, VersionAdapter,
};
use lodestone_server::{
    BlockEntity, ChunkColumn, ChunkSource, IntegratedServer, PLAYER_ENTITY_ID_BASE,
};
use lodestone_v1_19::adapter_for;

const TARGET: BlockPos = BlockPos::new(8, 100, 8);

#[test]
fn adapter_block_use_reaches_protocol_762_host_consumer() {
    let action = ClientAction::UseItemOn {
        hand: Hand::Off,
        pos: BlockPos::new(5, -10, -7),
        face: BlockFace::South,
        cursor: Vec3f {
            x: 0.25,
            y: 1.0,
            z: 0.75,
        },
        inside_block: true,
        sequence: lodestone_model::PredictionSequence::new(17),
    };
    let Some((packet_id, payload)) = adapter_for(762)
        .encode_action(ConnectionState::Play, &action)
        .expect("the era adapter must encode a block use")
    else {
        panic!("block use must have a serverbound packet");
    };
    let host = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to the hosted family");
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
            sequence: lodestone_model::PredictionSequence::new(17),
            hand: 1,
        },
        "the adapter and registry-selected host must agree on the placement consumer input"
    );
    assert_eq!(
        host.decode(lodestone_core::State::Configuration, packet_id, &payload),
        lodestone_server::ServerBound::Ignored,
        "the same bytes must not bypass the Play-state gate"
    );
}

struct FixtureSource {
    column: Mutex<ChunkColumn>,
    chest: Option<(BlockPos, BlockEntity)>,
}

impl FixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(TARGET.x, TARGET.y, TARGET.z, "minecraft:dandelion");
        Self {
            column: Mutex::new(column),
            chest: None,
        }
    }

    fn with_chest() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(
            TARGET.x,
            TARGET.y,
            TARGET.z,
            "minecraft:chest[facing=north,type=single,waterlogged=false]",
        );
        let mut slots = vec![None; 27];
        slots[0] = Some(ItemStack::new("minecraft:diamond".parse().unwrap(), 1));
        Self {
            column: Mutex::new(column),
            chest: Some((
                TARGET,
                BlockEntity::Container {
                    id: "minecraft:chest".to_owned(),
                    slots,
                },
            )),
        }
    }
}

impl ChunkSource for FixtureSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self
            .column
            .lock()
            .expect("fixture column lock poisoned")
            .clone()
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

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<BlockEntity> {
        self.chest
            .as_ref()
            .filter(|(pos, _)| *pos == BlockPos::new(x, y, z))
            .map(|(_, entity)| entity.clone())
    }
}

#[tokio::test]
async fn registry_selected_protocol_762_reaches_play_and_confirms_a_block_break() {
    let protocol = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to the hosted family");
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
    let (mut handle, _) = ClientBuilder::new(address, profile, Box::new(adapter_for(762)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-762 login reaches Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-762 chunk arrives");
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
        .expect("block update reaches the protocol-762 client");

    // `move_to` takes the position-and-look path here. The registry-selected
    // host must lift it, update the view centre, and serve the newly centred
    // column; without the serverbound movement decoder this times out at the
    // original spawn column even though the client predicts the move locally.
    handle
        .move_to(Vec3::new(40.0, 100.0, 8.0), Rotation::new(90.0, 0.0), true, false)
        .expect("joined client accepts a movement action");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(2, 0), Duration::from_secs(10))
        .await
        .expect("serverbound movement recentres the protocol-762 view stream");

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn registry_selected_protocol_762_broadcasts_an_arm_swing_to_another_client() {
    let protocol = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to the hosted family");
    let source = Arc::new(FixtureSource::new());
    let (mut server, sender_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        source,
        (0..=0, 0..=0),
        (8, 8),
        0,
        0,
    );
    let address = server
        .publish(("127.0.0.1", 0), None)
        .await
        .expect("the shared in-memory world must accept a second client");
    let sender_profile = LoginProfile {
        username: "SwingSender".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let observer_profile = LoginProfile {
        username: "SwingObserver".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let sender_address = ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    };
    let observer_address = ServerAddress {
        host: "127.0.0.1".to_owned(),
        port: address.port(),
    };
    let (mut sender, _sender_events) = ClientBuilder::new(
        sender_address,
        sender_profile,
        Box::new(adapter_for(762)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(sender_io);
    let (mut observer, mut observer_events) = ClientBuilder::new(
        observer_address,
        observer_profile,
        Box::new(adapter_for(762)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect()
    .await
    .expect("the observer must connect through the published shared host");

    sender
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("the swing sender must reach Play");
    observer
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("the swing observer must reach Play");
    sender
        .send_action(ClientAction::SwingArm { hand: Hand::Off })
        .expect("the joined sender accepts an off-hand swing");

    let (entity_id, action) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::EntityAnimation { entity_id, action }) =
                observer_events.recv().await
            {
                return (entity_id, action);
            }
        }
    })
    .await
    .expect("the observer must receive the hosted swing broadcast");
    assert_eq!(
        entity_id, PLAYER_ENTITY_ID_BASE,
        "the first shared-world player must retain the registry's first entity id"
    );
    assert_eq!(action, AnimationAction::SwingOffHand);

    sender.shutdown();
    observer.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn joined_protocol_762_chest_moves_a_slot_and_corrects_prediction() {
    let protocol = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to the hosted family");
    let source = Arc::new(FixtureSource::with_chest());
    assert_eq!(
        source
            .block_entity(TARGET.x, TARGET.y, TARGET.z)
            .map(|entity| {
                entity.container_slots()[0]
                    .as_ref()
                    .map(|item| item.item.to_string())
            }),
        Some(Some("minecraft:diamond".to_owned()))
    );
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        Arc::clone(&source),
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
    );
    let entities = server
        .block_entities()
        .expect("the live integrated server owns block entities")
        .clone();
    entities.with(|registry| {
        let mut slots = vec![None; 27];
        slots[0] = Some(ItemStack::new("minecraft:diamond".parse().unwrap(), 1));
        registry.insert(
            TARGET,
            BlockEntity::Container {
                id: "minecraft:chest".to_owned(),
                slots,
            },
        );
    });
    let profile = LoginProfile {
        username: "ChestFixture".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    };
    let address = ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    };
    let (mut handle, mut events) = ClientBuilder::new(address, profile, Box::new(adapter_for(762)))
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("protocol-762 login reaches Play");
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .expect("protocol-762 chunk arrives");
    assert_eq!(
        entities.with(|registry| {
            registry
                .get(TARGET)
                .map(BlockEntity::container_slots)
                .and_then(|slots| slots.first().cloned())
                .and_then(|item| item.map(|item| item.item.to_string()))
        }),
        Some("minecraft:diamond".to_owned())
    );
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: TARGET,
            face: BlockFace::Up,
            cursor: Vec3f {
                x: 0.5,
                y: 0.5,
                z: 0.5,
            },
            inside_block: false,
            sequence: lodestone_model::PredictionSequence::INITIAL,
        })
        .expect("joined client accepts the chest interaction");
    let window_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ScreenOpened { window_id, .. }) = events.recv().await {
                break window_id;
            }
        }
    })
    .await
    .expect("the chest opens through the joined protocol path");
    let initial = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
                && id == window_id
            {
                break items;
            }
        }
    })
    .await
    .expect("the chest's initial content reaches the joined client");
    assert_eq!(initial[0].as_ref().map(|item| item.count), Some(1));
    let opened = handle.open_menu().expect("the chest menu remains open");
    assert_eq!(opened.window_id, 1);
    assert_eq!(opened.menu.slot_item(0).map(|item| item.count()), Some(1));
    let state_id = opened.menu.state_id();

    handle
        .send_action(ClientAction::ContainerClick {
            window_id: opened.window_id,
            state_id,
            slot: 0,
            button: 0,
            click_type: ContainerClickType::QuickMove,
            // Deliberately omit the predicted diff: the host must derive the
            // transfer and send a full correction, not trust the claim.
            changed_slots: Vec::<ContainerSlotChange>::new(),
            carried_item: None,
        })
        .expect("joined client accepts the chest click");
    let corrected = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
                && id == window_id
            {
                break items;
            }
        }
    })
    .await
    .expect("the host mutation and corrective content reach the client");
    assert!(corrected[0].is_none());
    assert_eq!(corrected[27].as_ref().map(|item| item.count), Some(1));

    handle
        .send_action(ClientAction::ContainerClose {
            window_id: opened.window_id,
        })
        .expect("joined client accepts the chest close");
    handle
        .wait_for(Duration::from_secs(10), |client| client.open_menu().is_none())
        .await
        .expect("the host closes the tracked container");

    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: TARGET,
            face: BlockFace::Up,
            cursor: Vec3f {
                x: 0.5,
                y: 0.5,
                z: 0.5,
            },
            inside_block: false,
            sequence: lodestone_model::PredictionSequence::new(1),
        })
        .expect("joined client reopens the chest");
    let reopened_window_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ScreenOpened { window_id, .. }) = events.recv().await {
                break window_id;
            }
        }
    })
    .await
    .expect("the chest reopens through the joined protocol path");
    let reopened = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
                && id == reopened_window_id
            {
                break items;
            }
        }
    })
    .await
    .expect("the authoritative slot mutation persists after reopening");
    assert!(reopened[0].is_none());
    assert_eq!(reopened[27].as_ref().map(|item| item.count), Some(1));

    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
}

#[test]
fn protocol_761_is_not_hosted() {
    assert!(lodestone_registry::server_protocol_for_protocol(761).is_none());
}
