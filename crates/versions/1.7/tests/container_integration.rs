//! Protocol-5 container flow through the live integrated server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, EventStream, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockFace, BlockPos, ClientAction, ClientEvent, ContainerClickType, ContainerSlotChange,
    ContainerStateId, Vec3f,
};
use lodestone_server::{BlockEntity, ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v1_7::adapter;

const CHEST: BlockPos = BlockPos::new(9, 100, 8);
const TARGET: BlockPos = BlockPos::new(8, 100, 8);

struct FixtureSource {
    column: Mutex<ChunkColumn>,
}

impl FixtureSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(TARGET.x, TARGET.y, TARGET.z, "minecraft:dandelion");
        column.set_block(CHEST.x, CHEST.y, CHEST.z, "minecraft:chest");
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

fn profile() -> LoginProfile {
    LoginProfile {
        username: "Protocol5Chest".to_owned(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".to_owned(),
        port: 0,
    }
}

async fn next_content(
    events: &mut EventStream,
    window_id: i32,
) -> Vec<Option<lodestone_model::ItemStack>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent {
                window_id: id,
                items,
                ..
            }) = events.recv().await
                && id == window_id
            {
                return items;
            }
        }
    })
    .await
    .expect("server must send authoritative container content")
}

#[tokio::test]
async fn protocol_5_chest_move_reaches_the_live_inventory_consumer() {
    let protocol = lodestone_registry::server_protocol_for_protocol(5)
        .expect("protocol 5 must resolve to a hosted server protocol");
    let source = Arc::new(FixtureSource::new());
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
        let mut chest = BlockEntity::container("minecraft:chest");
        chest.set_container_slot(
            0,
            Some(lodestone_model::ItemStack::new(
                "minecraft:stone".parse().expect("stone key"),
                1,
            )),
        );
        registry.insert(CHEST, chest);
    });

    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile(),
        Box::new(adapter()),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(20))
        .await
        .expect("protocol-5 client must reach Play");
    handle
        .send_action(ClientAction::UseItemOn {
            hand: lodestone_model::Hand::Main,
            pos: CHEST,
            face: BlockFace::North,
            cursor: Vec3f::new(0.5, 0.5, 0.5),
            inside_block: false,
            sequence: 0,
        })
        .expect("protocol-5 client must use the chest");

    let window_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ScreenOpened {
                window_id,
                menu_type,
                ..
            }) = events.recv().await
            {
                assert_eq!(menu_type.to_string(), "minecraft:generic_9x3");
                break window_id;
            }
        }
    })
    .await
    .expect("server must open the chest window");
    let initial = next_content(&mut events, window_id).await;
    assert_eq!(initial.len(), 63);
    assert_eq!(initial[0].as_ref().map(|item| item.count), Some(1));

    for (slot, expected) in [(0, None), (27, Some(1))] {
        handle
            .send_action(ClientAction::ContainerClick {
                window_id,
                state_id: ContainerStateId::INITIAL,
                slot,
                button: 0,
                click_type: ContainerClickType::Pickup,
                changed_slots: Vec::<ContainerSlotChange>::new(),
                carried_item: None,
            })
            .expect("protocol-5 client must send the pickup click");
        let content = next_content(&mut events, window_id).await;
        assert_eq!(content[slot as usize].as_ref().map(|item| item.count), expected);
    }

    handle
        .send_action(ClientAction::ContainerClose { window_id })
        .expect("protocol-5 client must close the chest window");
    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
    assert!(entities.with(|registry| match registry.get(CHEST) {
        Some(BlockEntity::Container { slots, .. }) => slots[0].is_none(),
        _ => false,
    }));
}
