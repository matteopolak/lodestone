//! Protocol-766 container flow through the real integrated server.
//!
//! This deliberately sends a wrong post-click prediction. The server must
//! derive the quick-move result from its own chest and player inventories,
//! then the client must consume the correcting content packet before closing.

use std::sync::Mutex;
use std::time::Duration;

use lodestone_client::{ClientBuilder, EventStream, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{BlockFace, BlockPos, ClientAction, ClientEvent, Vec3f};
use lodestone_server::{BlockEntity, ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v1_20_6::adapter_for;

const CHEST: BlockPos = BlockPos::new(8, 100, 8);

struct ChestSource {
    column: Mutex<ChunkColumn>,
}

impl ChestSource {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block(CHEST.x, CHEST.y, CHEST.z, "minecraft:chest");
        Self { column: Mutex::new(column) }
    }
}

impl ChunkSource for ChestSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.column.lock().unwrap().clone()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column
            .lock()
            .unwrap()
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: &str) {
        self.column
            .lock()
            .unwrap()
            .set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
    }
}

async fn next_content(
    events: &mut EventStream,
    window_id: i32,
) -> Vec<Option<lodestone_model::ItemStack>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
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
async fn protocol_766_chest_move_is_authoritative_and_corrected_before_close() {
    let protocol = lodestone_registry::server_protocol_for_protocol(766)
        .expect("protocol 766 host is registered");
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        ChestSource::new(),
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
        chest.set_container_slot(0, Some(lodestone_model::ItemStack::new(
            "minecraft:stone".parse().unwrap(),
            1,
        )));
        registry.insert(CHEST, chest);
    });
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".to_owned(), port: 0 },
        LoginProfile { username: "ChestFixture".to_owned(), uuid: uuid::Uuid::new_v4() },
        Box::new(adapter_for(766)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(10)).await.unwrap();
    handle
        .wait_for_chunk(lodestone_client::ChunkPos::new(0, 0), Duration::from_secs(10))
        .await
        .unwrap();
    handle
        .send_action(ClientAction::UseItemOn {
            hand: lodestone_model::Hand::Main,
            pos: CHEST,
            face: BlockFace::Up,
            cursor: Vec3f { x: 0.5, y: 1.0, z: 0.5 },
            inside_block: false,
            sequence: 1,
        })
        .unwrap();
    handle
        .wait_for(Duration::from_secs(10), |client| client.open_menu().is_some())
        .await
        .expect("server opens the chest window");
    let window_id = handle.open_menu().unwrap().window_id;
    let initial = next_content(&mut events, window_id).await;
    assert_eq!(initial[0].as_ref().map(|item| item.item.to_string()), Some("minecraft:stone".to_owned()));

    let state_id = handle.open_menu().unwrap().menu.state_id();
    handle
        .send_action(ClientAction::ContainerClick {
            window_id,
            state_id,
            slot: 0,
            button: 0,
            click_type: lodestone_model::ContainerClickType::QuickMove,
            // Deliberately lie about the changed chest slot. The server must
            // derive the move and send a full correction rather than trusting
            // this client-provided item.
            changed_slots: vec![lodestone_model::ContainerSlotChange {
                slot: 0,
                item: Some(lodestone_model::ItemStack::new(
                    "minecraft:stone".parse().unwrap(),
                    1,
                )),
            }],
            carried_item: None,
        })
        .unwrap();
    let corrected = next_content(&mut events, window_id).await;
    assert!(corrected[0].is_none(), "the correction clears the chest slot");
    assert_eq!(
        corrected[62].as_ref().map(|item| item.item.to_string()),
        Some("minecraft:stone".to_owned()),
        "the authoritative quick-move result uses the reverse player-tail scan",
    );
    handle
        .wait_for(Duration::from_secs(10), |client| {
            client.open_menu().is_some_and(|menu| {
                menu.menu.slot_item(0).is_none()
                    && menu.menu.slot_item(62).is_some_and(|item| item.item().to_string() == "minecraft:stone")
            })
        })
        .await
        .expect("the client folds the correcting content packet");

    handle
        .send_action(ClientAction::ContainerClose { window_id })
        .unwrap();
    // The serverbound close is a notification, not a request for a
    // clientbound echo. A normal UI closes its local screen before sending
    // this action; this test injects the action directly, so the read model
    // remains open until the client itself shuts down.
    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
    assert!(entities.with(|registry| match registry.get(CHEST) {
        Some(BlockEntity::Container { slots, .. }) => slots[0].is_none(),
        _ => false,
    }));
}
