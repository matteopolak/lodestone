//! End-to-end: a real `lodestone-client` (running the real [`V770Adapter`])
//! observes a live-mutating entity source through the real
//! [`V770ServerProtocol`], `IntegratedServer`, and `serve_connection`'s
//! per-connection entity-streaming diff loop.
//!
//! `tests/entity_encoders.rs` already proves the three encoder methods
//! (`encode_add_entity`/`encode_entity_update`/`encode_remove_entity`) produce
//! wire bytes the real adapter decodes correctly, from hand-built
//! [`EntitySnapshot`]s. `lodestone-server`'s own `tests` unit-test the diff
//! logic (`EntityStreamer::sync`) against a stub source. Neither proves the
//! *pipeline*: that `IntegratedServer::open_in_memory_with_entities` really
//! calls the streamer, that its directives really reach a joined socket, and
//! that a real client's own state fold (`ClientHandle::entity`) really
//! reflects them. This test closes that gap by driving a live, externally
//! mutated `EntitySource` and asserting a real client observes the spawn, a
//! subsequent position change, and a removal.
//!
//! This does **not** use a real `MobSim` — see the doc comment on
//! `SharedSnapshotSource` below for why, and the reported blocker that
//! explains it. The `EntitySnapshot` values here are hand-mutated by the test
//! rather than ticked by an AI simulation; what is genuinely end-to-end is
//! everything downstream of "the source changed" (the diff, the encode, the
//! decode, the client-side fold) — the same chain a real simulation would
//! drive once the blocker is fixed.
//!
//! What would have to break for this to fail: a missing/incorrect
//! `add_entity` (no spawn ever appears), a missing/incorrect
//! `teleport_entity` (the position never updates), a missing/incorrect
//! `remove_entities`, or `serve_connection` not calling the streamer at all
//! (same symptom as the first failure).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ResourceKey, Rotation, Vec3};
use lodestone_server::{EntitySnapshot, EntitySource, IntegratedServer, StoneFloorSource};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::adapter;
use std::str::FromStr;
use uuid::Uuid;

fn profile() -> LoginProfile {
    LoginProfile {
        username: "SinglePlayer".into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}


/// A live, externally mutable [`EntitySource`] standing in for a real
/// `MobSim`.
///
/// A real `MobSim` cannot be used here: `IntegratedServer::open_in_memory_with_entities`
/// spawns its serving task with (native) `tokio::spawn`, which requires the
/// future — and so everything it captures, including the `EntitySource` — to
/// be `Send`. `MobSim` embeds a `GoalSelector`, which stores goals as
/// `Box<dyn Goal>`, and `lodestone_entity::ai::Goal` carries no `Send` bound,
/// so `Box<dyn Goal>` is `!Send`, so `MobSim` is `!Send`, so no wrapper
/// (`Arc<Mutex<MobSim>>` included) can satisfy `EntitySource: Send + Sync`.
/// That means the entity-streaming feature landed in `lodestone-server` is
/// currently **unusable with a real mob simulation** — reported upstream to
/// `impl-entity` rather than worked around by weakening this test or reaching
/// into `lodestone-entity` to add the bound myself (a trait-object bound
/// change cascades through `GoalSelector`/`SimMob`/`MobSim` and is squarely
/// their crate to land).
struct SharedSnapshotSource(Arc<Mutex<Vec<EntitySnapshot>>>);

impl EntitySource for SharedSnapshotSource {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0.lock().expect("snapshot lock poisoned").clone()
    }
}

#[tokio::test]
async fn a_real_client_observes_a_live_entity_spawn_then_move() {
    let min_y = -64;
    let height = 384; // matches `ChunkShape::overworld_1_21()`, as in server_integration.rs
    let view_radius = 0; // single chunk (0,0)

    let source = StoneFloorSource::new(min_y, height, 0);

    let mob_id = 4242;
    let mob_uuid = Uuid::new_v4();
    let entity_type = ResourceKey::from_str("minecraft:zombie").unwrap();
    let start_pos = Vec3::new(8.5, 1.0, 8.5);
    let snapshot = |pos: Vec3| EntitySnapshot {
        id: mob_id,
        uuid: mob_uuid,
        entity_type: entity_type.clone(),
        position: pos,
        rotation: Rotation {
            yaw: 0.0,
            pitch: 0.0,
        },
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        equipment: Vec::new(),
        leash_link: None,
    };

    let entities = Arc::new(Mutex::new(vec![snapshot(start_pos)]));

    let (server, client_io) = IntegratedServer::open_in_memory_with_entities(
        V770ServerProtocol,
        source,
        SharedSnapshotSource(entities.clone()),
        view_radius,
    );

    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    // Join: wait for the initial chunk view (proves Play was reached at all).
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while handle.loaded_chunk_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "client never received a chunk within 60s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Spawn: poll for the entity to appear. Each of the client's own inbound
    // join-sequence packets (accept-teleportation, client information,
    // chunk-batch-received) already drives a `serve_connection` streaming
    // pass; the `chat` nudge below just guarantees at least one inbound
    // packet keeps arriving if that traffic ever runs dry before the entity
    // is observed.
    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut initial = None;
    while std::time::Instant::now() < spawn_deadline {
        if let Some(view) = handle.entity_from_wire(mob_id) {
            initial = Some(view);
            break;
        }
        let _ = handle.chat("poke");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let initial = initial.expect("entity never appeared to the client within 30s");

    assert_eq!(initial.entity_id, mob_id);
    assert_eq!(initial.uuid, Some(mob_uuid));
    assert_eq!(initial.entity_type, entity_type);
    assert_eq!(initial.position, start_pos, "spawn position mismatch");

    // Move: mutate the live source (standing in for a simulation tick), then
    // poll for the client's own fold to report the *new* position — proof
    // the update path (not just the spawn path) is connected end to end.
    let moved_pos = Vec3::new(start_pos.x - 3.0, start_pos.y, start_pos.z - 3.0);
    *entities.lock().unwrap() = vec![snapshot(moved_pos)];

    let move_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut observed = None;
    while std::time::Instant::now() < move_deadline {
        let _ = handle.chat("poke");
        if let Some(view) = handle.entity_from_wire(mob_id)
            && view.position != start_pos
        {
            observed = Some(view.position);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let observed = observed
        .expect("client never observed the entity move within 30s after mutating the source");

    assert_eq!(
        observed, moved_pos,
        "client's folded position should exactly match the mutated snapshot (absolute teleport, f64 position)"
    );

    // Remove: clear the source, then poll for the client to drop the entity.
    entities.lock().unwrap().clear();
    let remove_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut removed = false;
    while std::time::Instant::now() < remove_deadline {
        let _ = handle.chat("poke");
        if handle.entity_from_wire(mob_id).is_none() {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        removed,
        "client never observed the entity removal within 30s after clearing the source"
    );

    println!(
        "real client observed a live entity: spawn={start_pos:?} moved_to={observed:?} then removed"
    );

    drop(handle);
    server.shutdown().await;
}

/// A mob's held items and worn banner reach a real client over the wire, and a
/// later change to one slot reaches it too (control: a slot nobody touched
/// keeps its item).
#[tokio::test]
async fn a_real_client_sees_mob_equipment_on_spawn_and_after_a_change() {
    use lodestone_model::item::BannerPatternLayer;
    use lodestone_model::{EntityEquipment, EquipmentSlot, ItemStack};

    let source = StoneFloorSource::new(-64, 384, 0);
    let key = |s: &str| ResourceKey::from_str(s).unwrap();
    let mut banner = ItemStack::new(key("minecraft:white_banner"), 1);
    banner.components.banner_patterns = vec![
        BannerPatternLayer { pattern_asset_id: "rhombus".into(), color: "cyan".into() },
        BannerPatternLayer { pattern_asset_id: "border".into(), color: "black".into() },
    ];
    let equipment = |hand: &str| {
        vec![
            EntityEquipment { slot: EquipmentSlot::MainHand, item: Some(ItemStack::new(key(hand), 1)) },
            EntityEquipment { slot: EquipmentSlot::OffHand, item: None },
            EntityEquipment { slot: EquipmentSlot::Head, item: Some(banner.clone()) },
        ]
    };
    let mob_id = 4343;
    let snapshot = |hand: &str| EntitySnapshot {
        id: mob_id,
        uuid: Uuid::new_v4(),
        entity_type: key("minecraft:pillager"),
        position: Vec3::new(8.5, 1.0, 8.5),
        rotation: Rotation { yaw: 0.0, pitch: 0.0 },
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        equipment: equipment(hand),
        leash_link: None,
    };
    let mut first = snapshot("minecraft:crossbow");
    let uuid = first.uuid;
    let entities = Arc::new(Mutex::new(vec![first.clone()]));
    let (server, client_io) = IntegratedServer::open_in_memory_with_entities(
        V770ServerProtocol,
        source,
        SharedSnapshotSource(entities.clone()),
        0,
    );
    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    let held = |handle: &lodestone_client::ClientHandle, slot: EquipmentSlot| {
        handle
            .entity_from_wire(mob_id)
            .and_then(|view| view.equipment.into_iter().find(|e| e.slot == slot))
            .and_then(|e| e.item)
    };
    macro_rules! wait {
        ($what:expr, $done:expr) => {{
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !$done {
                assert!(std::time::Instant::now() < deadline, $what);
                let _ = handle.chat("poke");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }};
    }
    wait!(
        "the crossbow never reached the client",
        held(&handle, EquipmentSlot::MainHand).is_some_and(|i| i.item == key("minecraft:crossbow"))
    );
    let head = held(&handle, EquipmentSlot::Head).expect("the banner reached the client with the crossbow");
    assert_eq!(head.item, key("minecraft:white_banner"));
    assert_eq!(head.components.banner_patterns, banner.components.banner_patterns);

    // Change only the main hand.
    first = snapshot("minecraft:iron_sword");
    first.uuid = uuid;
    *entities.lock().unwrap() = vec![first];
    wait!(
        "the swapped main-hand item never reached the client",
        held(&handle, EquipmentSlot::MainHand).is_some_and(|i| i.item == key("minecraft:iron_sword"))
    );
    let head = held(&handle, EquipmentSlot::Head).expect("the untouched head slot keeps its banner");
    assert_eq!(head.components.banner_patterns, banner.components.banner_patterns);

    drop(handle);
    server.shutdown().await;
}
