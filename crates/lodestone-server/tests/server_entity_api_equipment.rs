//! Production-facing gate for the copied player-equipment part of
//! [`lodestone_server::ServerEntityApi`]. The test uses the same
//! `PlayerRegistry` inventory mirror that a live connection updates; it never
//! reaches into a connection task or keeps a registry guard across the API
//! call.

use std::str::FromStr;

use lodestone_model::{EquipmentSlot, ItemStack, ResourceKey, Vec3};
use lodestone_server::entity_api::{EntityLifecycleCursor, EntityLifecycleEvent};
use lodestone_server::{
    EntityMutation, EntityMutationResult, MobHandle, PlayerRegistry, ServerEntityApi,
};
use uuid::Uuid;

#[test]
fn player_observation_copies_authoritative_equipment_slots() {
    let players = PlayerRegistry::new();
    let uuid = Uuid::from_u128(7);
    let ticket = players.join("EquipmentObserver", uuid, Vec3::new(1.0, 2.0, 3.0));
    let player_id = ticket.entity_id();

    let mut inventory = lodestone_server::PlayerInventory::new();
    inventory.set_native(
        3,
        Some(ItemStack::new(
            ResourceKey::from_str("minecraft:diamond_sword").expect("valid item key"),
            1,
        )),
    );
    inventory.set_native(
        39, // the player's native head-equipment slot
        Some(ItemStack::new(
            ResourceKey::from_str("minecraft:iron_helmet").expect("valid item key"),
            1,
        )),
    );
    inventory.set_selected_hotbar_slot(3);
    players.set_inventory(uuid, &inventory);

    let api = ServerEntityApi::new(MobHandle::default(), players.clone());
    let observation = api.observe(player_id).expect("joined player is observable");
    assert_eq!(observation.equipment.len(), 6);
    assert_eq!(
        observation.equipment[0].slot,
        EquipmentSlot::MainHand,
        "main hand must use the selected hotbar slot"
    );
    assert_eq!(
        observation.equipment[0]
            .item
            .as_ref()
            .map(|item| item.item.to_string()),
        Some("minecraft:diamond_sword".to_owned())
    );
    assert_eq!(
        observation.equipment[2]
            .item
            .as_ref()
            .map(|item| item.item.to_string()),
        Some("minecraft:iron_helmet".to_owned())
    );
    assert!(
        observation.equipment[1].item.is_none(),
        "an empty off-hand must remain an explicit empty slot"
    );

    // The observer owns its copy. Mutating the original inventory after the
    // API call cannot rewrite a previously returned plugin value.
    inventory.set_native(3, None);
    players.set_inventory(uuid, &inventory);
    assert_eq!(
        observation.equipment[0]
            .item
            .as_ref()
            .map(|item| item.item.to_string()),
        Some("minecraft:diamond_sword".to_owned())
    );

    drop(ticket);
    assert_eq!(
        api.mutate(player_id, lodestone_server::EntityMutation::Despawn),
        EntityMutationResult::UnknownEntity,
        "a disconnected player id must not be mistaken for a mob"
    );
}

#[test]
fn lifecycle_cursor_reports_authoritative_spawn_and_despawn_edges() {
    let players = PlayerRegistry::new();
    let api = ServerEntityApi::new(MobHandle::default(), players.clone());
    let mut cursor = EntityLifecycleCursor::new();
    let id = api.spawn(
        ResourceKey::from_str("minecraft:cow").expect("valid entity key"),
        Vec3::new(4.0, 8.0, 4.0),
    );
    let ticket = players.join("LifecycleObserver", Uuid::from_u128(8), Vec3::new(0.0, 2.0, 0.0));
    let player_id = ticket.entity_id();

    let events = cursor.poll(&api);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        EntityLifecycleEvent::Spawned(observation) if observation.id == id
    ));
    assert!(matches!(
        &events[1],
        EntityLifecycleEvent::Spawned(observation) if observation.id == player_id
    ));
    assert!(cursor.poll(&api).is_empty(), "a stable entity has no duplicate edge");

    assert_eq!(
        api.mutate(id, EntityMutation::Despawn),
        EntityMutationResult::Applied
    );
    drop(ticket);
    let events = cursor.poll(&api);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        EntityLifecycleEvent::Despawned(observation) if observation.id == id
    ));
    assert!(matches!(
        &events[1],
        EntityLifecycleEvent::Despawned(observation) if observation.id == player_id
    ));
    assert!(cursor.poll(&api).is_empty(), "a removed entity has one edge");
}
