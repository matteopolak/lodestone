//! In-memory integration coverage for the hosted protocol-340 family.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ChatKind, ClientAction, ClientEvent,
    ContainerClickType, ContainerSlotChange, ContainerStateId, EntityInteraction, Hand, Rotation,
    Vec3, Vec3f,
};
use lodestone_server::{BlockEntity, ChunkColumn, ChunkSource, IntegratedServer, MobOwner};
use lodestone_v1_9::adapter_for;

const TARGET: BlockPos = BlockPos::new(8, 100, 8);
const CHEST: BlockPos = BlockPos::new(9, 100, 8);

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

async fn assert_registry_selected_server_consumes_block_use(protocol_version: i32) {
    let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("hosted legacy protocol must resolve");
    let source = Arc::new(LegacyFixtureSource::new());
    source.set_block(
        TARGET.x,
        TARGET.y,
        TARGET.z,
        "minecraft:lever[face=wall,facing=north,powered=false]",
    );
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
        .expect("lever column must arrive");
    let unpowered = lodestone_data::block_states::state_id(
        "minecraft:lever[face=wall,facing=north,powered=false]",
    )
    .expect("unpowered lever state exists");
    assert_eq!(handle.block_at(TARGET), Some(unpowered));

    // This crosses the adapter's version-specific encoding, the server
    // protocol decoder, and the shared server's hand-use world mutation.
    handle
        .send_action(ClientAction::UseItemOn {
            hand: lodestone_model::Hand::Main,
            pos: TARGET,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.5, 0.5),
            inside_block: false,
            sequence: 0,
        })
        .expect("joined client accepts block use");
    let powered = lodestone_data::block_states::state_id(
        "minecraft:lever[face=wall,facing=north,powered=true]",
    )
    .expect("powered lever state exists");
    handle
        .wait_for(Duration::from_secs(10), move |client| client.block_at(TARGET) == Some(powered))
        .await
        .expect("the server must publish the hand-use lever mutation");

    handle.shutdown();
    server.shutdown().await;
}

async fn assert_registry_selected_server_consumes_a_chest_click(protocol_version: i32) {
    let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
        .expect("hosted legacy protocol must resolve");
    let source = Arc::new(LegacyFixtureSource::new());
    source.set_block(CHEST.x, CHEST.y, CHEST.z, "minecraft:chest");
    let world_dir = std::env::temp_dir().join(format!(
        "lodestone-v1-9-container-{protocol_version}-{}",
        std::process::id()
    ));
    let (server, client_io, source) = IntegratedServer::open_persistent_with_mobs(
        protocol,
        &world_dir,
        source,
        -64,
        384,
        (0..=-1, 0..=-1),
        (0, 0),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent legacy fixture world");
    let entities = source.block_entities();
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
        Box::new(adapter_for(protocol_version)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(20))
        .await
        .expect("joined client must reach Play");
    handle
        .send_action(ClientAction::UseItemOn {
            hand: lodestone_model::Hand::Main,
            pos: CHEST,
            face: BlockFace::North,
            cursor: Vec3f::new(0.5, 0.5, 0.5),
            inside_block: false,
            sequence: 0,
        })
        .expect("joined client accepts chest use");

    let window_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ScreenOpened { window_id, menu_type, .. }) = events.recv().await
            {
                assert_eq!(menu_type.to_string(), "minecraft:generic_9x3");
                break window_id;
            }
        }
    })
    .await
    .expect("server must open the chest window");
    let initial = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
            {
                if id == window_id {
                    break items;
                }
            }
        }
    })
    .await
    .expect("server must send the chest contents");
    assert_eq!(initial.len(), 63, "27 chest slots plus 36 player slots");
    assert_eq!(initial[0].as_ref().map(|item| item.count), Some(1));

    for (slot, expected) in [(0_usize, None), (27_usize, Some(1))] {
        handle
            .send_action(ClientAction::ContainerClick {
                window_id,
                state_id: ContainerStateId::INITIAL,
                slot: i32::try_from(slot).expect("fixture slot fits i32"),
                button: 0,
                click_type: ContainerClickType::Pickup,
                changed_slots: Vec::<ContainerSlotChange>::new(),
                carried_item: None,
            })
            .expect("joined client accepts a legacy window click");
        let items = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(ClientEvent::ContainerContent { window_id: id, items, .. }) = events.recv().await
                {
                    if id == window_id {
                        break items;
                    }
                }
            }
        })
        .await
        .expect("server must send authoritative content after the click");
        assert_eq!(items[slot].as_ref().map(|item| item.count), expected);
    }

    handle
        .send_action(ClientAction::ContainerClose { window_id })
        .expect("joined client accepts a legacy window close");
    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
    // The second authoritative content packet is emitted after the server has
    // already persisted the click into the block entity, so this read checks
    // the production consumer rather than only the protocol decoder.
    assert!(entities.with(|registry| match registry.get(CHEST) {
        Some(BlockEntity::Container { slots, .. }) => slots[0].is_none(),
        _ => false,
    }));
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
async fn registry_selected_protocol_340_echoes_legacy_chat_to_the_client_event_stream() {
    let protocol = lodestone_registry::server_protocol_for_protocol(340)
        .expect("protocol 340 must resolve to the hosted legacy family");
    let source = Arc::new(LegacyFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, source, 0);
    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile(),
        Box::new(adapter_for(340)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(10)).await.expect("must join Play");
    handle
        .send_action(ClientAction::SendChat {
            text: "legacy chat \"escapes\"".to_owned(),
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
    .expect("the server must echo legacy chat through the client event stream");
    assert_eq!(text, "<LegacyFixture> legacy chat \"escapes\"");
    assert_eq!(kind, ChatKind::System);

    handle.shutdown();
    server.shutdown().await;
}

#[tokio::test]
async fn protocol_340_entity_interaction_reaches_the_shared_mob_consumer() {
    let protocol = lodestone_registry::server_protocol_for_protocol(340)
        .expect("protocol 340 must resolve to the hosted legacy family");
    let source = Arc::new(LegacyFixtureSource::new());
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
    );
    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile(),
        Box::new(adapter_for(340)),
    )
    .player_loaded_policy(PlayerLoadedPolicy::Manual)
    .connect_with(client_io);
    handle
        .wait_for_spawn(Duration::from_secs(10))
        .await
        .expect("legacy login must reach Play");

    let mobs = server.mobs().expect("mob-backed host must expose its live sim");
    let ready = tokio::time::Instant::now() + Duration::from_secs(10);
    while mobs.with(|sim| sim.next_id()) < 1000 && tokio::time::Instant::now() < ready {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        mobs.with(|sim| sim.next_id()) >= 1000,
        "the mob reseed must finish before the interaction fixture is inserted"
    );

    // Protocol 340's login_start has no UUID. Its server decoder therefore
    // supplies nil, which is the actor identity used by the shared consumer.
    let player_uuid = uuid::Uuid::nil();
    let wolf = mobs.with(|sim| {
        let id = sim
            .spawn_species(
                "minecraft:wolf".parse().expect("wolf resource key"),
                Vec3::new(9.0, 100.0, 8.0),
            )
            .id();
        sim.get_mut(id)
            .expect("just-spawned wolf")
            .tame(MobOwner::Player(player_uuid));
        id
    });
    handle
        .send_action(ClientAction::InteractEntity {
            entity_id: wolf,
            interaction: EntityInteraction::Interact { hand: Hand::Main },
            sneaking: false,
        })
        .expect("the hosted client must encode the entity interaction");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !mobs.with(|sim| sim.get(wolf).is_some_and(|mob| mob.is_ordered_to_sit()))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        mobs.with(|sim| sim.get(wolf).is_some_and(|mob| mob.is_ordered_to_sit())),
        "protocol-340 use_entity must reach the shared tamed-mob interaction consumer"
    );

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
async fn registry_selected_protocol_110_consumes_byte_cursor_block_use() {
    assert_registry_selected_server_consumes_block_use(110).await;
}

#[tokio::test]
async fn registry_selected_protocol_110_consumes_a_chest_click() {
    assert_registry_selected_server_consumes_a_chest_click(110).await;
}

#[tokio::test]
async fn registry_selected_protocol_210_consumes_a_chest_click() {
    assert_registry_selected_server_consumes_a_chest_click(210).await;
}

#[tokio::test]
async fn registry_selected_protocol_316_consumes_a_chest_click() {
    assert_registry_selected_server_consumes_a_chest_click(316).await;
}

#[tokio::test]
async fn registry_selected_protocol_340_consumes_a_chest_click() {
    assert_registry_selected_server_consumes_a_chest_click(340).await;
}

#[tokio::test]
async fn registry_selected_protocol_340_consumes_float_cursor_block_use() {
    assert_registry_selected_server_consumes_block_use(340).await;
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
