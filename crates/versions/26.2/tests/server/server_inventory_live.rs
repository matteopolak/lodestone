//! Inventory actions and background container updates over a real client/server
//! transport. The final server summary checks authoritative state independently
//! of the client's event-driven menu projection.

use std::time::Duration;

use lodestone_client::{ClientBuilder, EventStream, LoginProfile, ServerAddress};
use lodestone_data::block_states::StateId;
use lodestone_game::menus::Menus;
use lodestone_model::{
    BlockFace, BlockPos, ClientAction, ClientEvent, ContainerClickType, GameMode, Hand, ItemStack,
    PredictionSequence, Vec3f,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, serve_connection,
    BlockEntity, Furnace, FurnaceKind,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use uuid::Uuid;

struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state_id(lx, y, lz)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

fn stack(name: &str, count: u32) -> ItemStack {
    ItemStack::new(name.parse().expect("valid resource key"), count)
}

fn spawn_inventory_server<S: ChunkSource + 'static>(
    server_io: tokio::io::DuplexStream,
    source: S,
    block_entities: BlockEntityHandle,
) -> tokio::task::JoinHandle<Result<lodestone_server::ServeSummary, lodestone_server::ServerError>> {
    tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        let mobs = MobHandle::default();
        let serving = Box::pin(serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &block_entities,
            &mobs,
        ));
        serving.await
    })
}

/// A real client selects a hotbar slot and performs a container click
/// against its own inventory (window `0`); both land in the server's
/// [`PlayerInventory`](lodestone_server::PlayerInventory) once the
/// connection closes.
#[tokio::test]
async fn real_client_hotbar_select_and_container_click_reach_the_server_model() {
    let (client_io, server_io) = memory_pair();

    let server_task = spawn_inventory_server(server_io, AirSource, BlockEntityHandle::default());

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("InventoryWatcher"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    handle
        .send_action(ClientAction::SetCarriedItem { slot: 4 })
        .expect("client still connected");

    // Two **real** pickup clicks moving a pickaxe from hotbar slot 0 (menu slot 36)
    // into main storage (menu slot 9): take onto the cursor, then put down. The
    // server derives both from `(slot, button, click_type)` alone
    // (`container_click::do_click`) — a diff *claiming* slot 9 now holds a pickaxe
    // mints nothing, which is what this test used to do.
    handle
        .send_action(ClientAction::ChangeGameMode {
            mode: GameMode::Creative,
        })
        .expect("client still connected");
    handle
        .send_action(ClientAction::SetCreativeModeSlot {
            slot: 36,
            item: Some(stack("minecraft:diamond_pickaxe", 1)),
        })
        .expect("client still connected");
    for slot in [36, 9] {
        handle
            .send_action(ClientAction::ContainerClick {
                window_id: 0,
                state_id: lodestone_model::ContainerStateId::new(1),
                slot,
                button: 0,
                click_type: ContainerClickType::Pickup,
                changed_slots: Vec::new(),
                carried_item: None,
            })
            .expect("client still connected");
    }

    // `send_action` only enqueues onto the driver's channel; give the driver
    // task a moment to actually perform the writes before closing the
    // connection out from under it.
    tokio::time::sleep(Duration::from_millis(200)).await;

    handle.shutdown();
    let _ = handle.join().await;

    let summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    assert_eq!(
        summary.inventory.selected_hotbar_slot(),
        4,
        "SET_CARRIED_ITEM must select hotbar slot 4 server-side"
    );
    assert_eq!(
        summary.inventory.native(9),
        Some(&stack("minecraft:diamond_pickaxe", 1)),
        "two derived pickup clicks must move the pickaxe into native slot 9"
    );

    // Non-vacuity / negative control: a native slot the click never touched
    // must still read empty — proves the assertions above are checking a
    // real, localized write, not a coincidence of every slot already
    // holding the same value.
    assert!(summary.inventory.native(10).is_none());
}

struct CraftingSource;

const TABLE_POS: BlockPos = BlockPos::new(2, 4, 2);

impl ChunkSource for CraftingSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = AirSource.column(cx, cz);
        if (cx, cz) == (0, 0) {
            column.set_block_id(2, 4, 2, lodestone_data::block::Block::CraftingTable.default_state());
        }
        column
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        if BlockPos::new(x, y, z) == TABLE_POS {
            lodestone_data::block::Block::CraftingTable.default_state()
        } else {
            StateId::AIR
        }
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        AirSource.biome_state_at(x, y, z)
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

async fn fold_until(
    events: &mut EventStream,
    menus: &mut Menus,
    expected: &'static str,
    ready: impl Fn(&Menus) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready(menus) {
            let event = events.recv().await.expect("client event stream closed");
            menus.apply(&event);
        }
    })
    .await
    .expect(expected);
}

fn open_block(handle: &lodestone_client::ClientHandle, pos: BlockPos) {
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.5, 0.5),
            inside_block: false,
            sequence: PredictionSequence::new(1),
        })
        .expect("send open block");
}

#[tokio::test]
async fn crafting_close_publishes_grid_and_cursor_returns_without_another_click() {
    let (client_io, server_io) = memory_pair();
    let server_task = spawn_inventory_server(server_io, CraftingSource, BlockEntityHandle::default());
    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("CraftingReturn"),
        Box::new(adapter()),
    )
    .connect_with(client_io);
    let mut menus = Menus::new();
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client spawn");
    handle
        .send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative })
        .expect("send creative mode");
    handle
        .send_action(ClientAction::SetCreativeModeSlot {
            slot: 36,
            item: Some(stack("minecraft:oak_planks", 11)),
        })
        .expect("seed inventory");
    open_block(&handle, TABLE_POS);
    fold_until(&mut events, &mut menus, "crafting snapshot never arrived", |menus| {
        menus.opened().is_some()
            && menus.player_native(0).is_some_and(|item| item.count() == 11)
    }).await;
    let window_id = menus.opened_window_id().expect("open window");
    for (slot, button) in [(37, 0), (1, 1), (2, 1)] {
        handle.send_action(ClientAction::ContainerClick {
            window_id,
            state_id: lodestone_model::ContainerStateId::new(1),
            slot,
            button,
            click_type: ContainerClickType::Pickup,
            changed_slots: Vec::new(),
            carried_item: None,
        }).expect("send crafting click");
    }
    fold_until(&mut events, &mut menus, "grid and cursor never filled", |menus| {
        menus.opened().is_some_and(|menu| {
            menu.slot_item(1).is_some_and(|item| item.count() == 1)
                && menu.slot_item(2).is_some_and(|item| item.count() == 1)
                && menu.carried().is_some_and(|item| item.count() == 9)
        })
    }).await;
    assert!(menus.player_native(0).is_none(), "the planks must have left the hotbar");

    menus.apply(&ClientEvent::ScreenClosed { window_id });
    assert!(menus.opened().is_none());
    assert!(menus.player_native(0).is_none(), "close must await the authoritative return");
    handle.send_action(ClientAction::ContainerClose { window_id }).expect("send close");
    fold_until(&mut events, &mut menus, "close never published returned planks", |menus| {
        menus.player_native(0).is_some_and(|item| item.count() == 11)
    }).await;
    let player = menus.player();
    let returned = player.player_native(0).expect("returned stack");
    assert_eq!(returned.item().to_string(), "minecraft:oak_planks");
    assert_eq!(returned.count(), 11);
    assert!(player.player_native(1).is_none());
    assert!(player.carried().is_none());
    assert!(player.slot_item(1).is_none());

    handle.shutdown();
    let _ = handle.join().await;
    let summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await.expect("server timeout").expect("server panic").expect("server error");
    assert_eq!(summary.inventory.native(0), Some(&stack("minecraft:oak_planks", 11)));
    assert!(summary.inventory.table_crafting().is_none());
    assert!(summary.inventory.click_state().carried.is_none());
}

#[tokio::test]
async fn open_furnace_publishes_background_slots_and_properties_without_clicks() {
    let (client_io, server_io) = memory_pair();
    let block_entities = BlockEntityHandle::default();
    block_entities.with(|registry| {
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_input(Some(stack("minecraft:iron_ore", 3)));
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        registry.insert(TABLE_POS, BlockEntity::Furnace(furnace));
    });
    let server_task = spawn_inventory_server(server_io, AirSource, block_entities.clone());
    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("FurnaceUpdates"),
        Box::new(adapter()),
    )
    .connect_with(client_io);
    let mut menus = Menus::new();
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client spawn");
    open_block(&handle, TABLE_POS);
    fold_until(&mut events, &mut menus, "furnace never opened", |menus| {
        menus.opened().is_some() && menus.container_data(3) == Some(200)
    }).await;
    let window_id = menus.opened_window_id();
    assert_eq!(menus.opened().unwrap().slot_item(0).unwrap().count(), 3);
    assert!(menus.opened().unwrap().slot_item(1).is_some());
    assert!(menus.opened().unwrap().slot_item(2).is_none());
    assert_eq!(menus.container_data(2), Some(0));

    let tick = |count| block_entities.with(|registry| {
        let Some(BlockEntity::Furnace(furnace)) = registry.get_mut(TABLE_POS) else {
            panic!("furnace missing");
        };
        for _ in 0..count {
            furnace.tick();
        }
    });
    tick(37);
    fold_until(&mut events, &mut menus, "furnace progress never reached the open screen", |menus| {
        menus.container_data(0) == Some(1564) && menus.container_data(2) == Some(37)
    }).await;
    assert_eq!(menus.container_data(1), Some(1600));
    assert_eq!(menus.container_data(3), Some(200));
    assert!(menus.opened().unwrap().slot_item(1).is_none());
    assert!(menus.opened().unwrap().slot_item(2).is_none());

    tick(163);
    fold_until(&mut events, &mut menus, "smelting result never reached the open screen", |menus| {
        menus.opened().is_some_and(|menu| {
            menu.slot_item(0).is_some_and(|item| item.count() == 2)
                && menu.slot_item(2).is_some_and(|item| item.count() == 1)
        }) && menus.container_data(0) == Some(1401) && menus.container_data(2) == Some(0)
    }).await;
    assert_eq!(menus.opened_window_id(), window_id);
    assert_eq!(
        menus.opened().unwrap().slot_item(2).unwrap().item().to_string(),
        "minecraft:iron_ingot",
    );
    let client_menu = handle.open_menu().expect("client furnace remains open");
    assert_eq!(client_menu.menu.slot_item(0).unwrap().count(), 2);
    assert_eq!(client_menu.menu.slot_item(2).unwrap().count(), 1);
    handle.shutdown();
    let _ = handle.join().await;
    tokio::time::timeout(Duration::from_secs(10), server_task)
        .await.expect("server timeout").expect("server panic").expect("server error");
}
