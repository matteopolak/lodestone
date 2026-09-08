//! Independent protocol-404 metadata and attribute fixtures through ECS ingest.
//!
//! The byte arrays in this test are protocol fixtures, not values produced by
//! the packet structs.  That keeps a shared codec mistake from satisfying both
//! sides of the assertion.  The decoded events are then submitted to the same
//! `NetIngest` schedule used by the client driver, where the entity components
//! become the observable result.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{
    Attributes, CustomName, CustomNameVisible, EntityFlags, EntityIndex, Equipment,
};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_13::packet_ids::play::clientbound;
use lodestone_v1_13::{adapter_for, PROTOCOL_1_13_2};
use lodestone_world::World;

const ENTITY_ID: i32 = 17;

/// A captured-shape protocol-404 living-entity spawn body with base metadata.
///
/// The body uses entity type 51, pig, and carries flags `0x05`, the optional
/// name `Hi`, and visible-name `true`. Coordinates, UUID and velocity are
/// deliberately non-default so an incorrect field width or ordering cannot
/// still produce a plausible entity event.
const SPAWN_WITH_METADATA: &[u8] = &[
    0x11,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
    0xee, 0xff,
    0x33,
    0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x05,
    0x02, 0x05, 0x01, 0x0d, b'{', b'"', b't', b'e', b'x', b't', b'"', b':', b'"', b'H', b'i', b'"', b'}',
    0x03, 0x07, 0x01,
    0xff,
];

/// A literal protocol-404 `entity_update_attributes` body.
///
/// It reports `generic.movementSpeed` with base `0.1` and one UUID modifier
/// using operation 1 and amount `0.25`. The legacy name and UUID are both
/// useful controls: the adapter must canonicalize the former and preserve the
/// identity of the latter before ECS stores the snapshot.
const UPDATE_ATTRIBUTES: &[u8] = &[
    0x11,
    0x00, 0x00, 0x00, 0x01,
    0x15, b'g', b'e', b'n', b'e', b'r', b'i', b'c', b'.', b'm', b'o', b'v', b'e', b'm', b'e',
    b'n', b't', b'S', b'p', b'e', b'e', b'd',
    0x3f, 0xb9, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a,
    0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x3f, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

/// Entity 17's head slot with two protocol-404 iron helmets. The item id is
/// the literal 1.13 registry id, followed by count and an absent NBT tag.
const ENTITY_EQUIPMENT: &[u8] = &[0x11, 0x05, 0x01, 0x8b, 0x04, 0x02, 0x00];

#[test]
fn literal_protocol_404_metadata_equipment_and_attributes_reach_ecs_components() {
    let adapter = adapter_for(PROTOCOL_1_13_2);
    let mut packet_world = World::new();
    let mut events = Vec::new();

    for (packet_id, payload) in [
        (clientbound::SPAWN_ENTITY_LIVING, SPAWN_WITH_METADATA),
        (clientbound::ENTITY_EQUIPMENT, ENTITY_EQUIPMENT),
        (clientbound::ENTITY_UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
    ] {
        let directives = adapter
            .handle_packet(
                &mut packet_world,
                ConnectionState::Play,
                packet_id,
                payload,
            )
            .expect("literal protocol-404 packet must decode");
        events.extend(directives.into_iter().filter_map(|directive| match directive {
            Directive::Emit(event) => Some(event),
            _ => None,
        }));
    }

    let mut app = App::new();
    app.add_plugins(IngestPlugin);
    {
        let mut queue = app.world_mut().resource_mut::<IngestQueue>();
        for event in events {
            queue.push(event);
        }
    }
    app.world_mut().run_schedule(NetIngest);

    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("adapter spawn event must create an indexed ECS entity");
    let entity = world.get_entity(entity).expect("indexed entity must exist");
    assert_eq!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0x05));
    assert_eq!(
        entity
            .get::<CustomName>()
            .and_then(|name| name.0.as_ref())
            .map(|name| name.to_plain_string()),
        Some("Hi".to_owned())
    );
    assert_eq!(
        entity.get::<CustomNameVisible>().map(|visible| visible.0),
        Some(true)
    );

    let equipment = entity
        .get::<Equipment>()
        .expect("adapter equipment event must reach the ECS equipment component");
    let helmet = equipment
        .0
        .iter()
        .find(|update| update.slot == lodestone_model::EquipmentSlot::Head)
        .expect("literal head update must be retained");
    let item = helmet.item.as_ref().expect("head item must be present");
    assert_eq!(item.item.to_string(), "minecraft:iron_helmet");
    assert_eq!(item.count, 2);

    let attributes = entity
        .get::<Attributes>()
        .expect("adapter attribute event must reach the ECS attribute component");
    assert_eq!(attributes.0.len(), 1);
    let attribute = &attributes.0[0];
    assert_eq!(attribute.attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attribute.base, 0.1);
    assert_eq!(attribute.modifiers.len(), 1);
    assert_eq!(
        attribute.modifiers[0].id.to_string(),
        "minecraft:uuid/00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(attribute.modifiers[0].amount, 0.25);
    assert_eq!(attribute.modifiers[0].operation, 1);
}

#[test]
fn literal_protocol_404_wrong_values_change_observed_components() {
    let mut equipment = ENTITY_EQUIPMENT.to_vec();
    equipment[5] = 0x03;
    let adapter = adapter_for(PROTOCOL_1_13_2);
    let mut packet_world = World::new();
    let events: Vec<ClientEvent> = [
        (clientbound::SPAWN_ENTITY_LIVING, SPAWN_WITH_METADATA),
        (clientbound::ENTITY_EQUIPMENT, equipment.as_slice()),
    ]
    .into_iter()
    .flat_map(|(packet_id, payload)| {
        adapter
            .handle_packet(&mut packet_world, ConnectionState::Play, packet_id, payload)
            .expect("wrong item count remains a valid packet")
            .into_iter()
            .filter_map(|directive| match directive {
                Directive::Emit(event) => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>()
    })
    .collect();
    let mut app = App::new();
    app.add_plugins(IngestPlugin);
    {
        let mut queue = app.world_mut().resource_mut::<IngestQueue>();
        for event in events {
            queue.push(event);
        }
    }
    app.world_mut().run_schedule(NetIngest);
    let entity = app
        .world()
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("entity");
    let entity = app.world().get_entity(entity).expect("entity exists");
    let helmet = entity
        .get::<Equipment>()
        .expect("equipment")
        .0
        .iter()
        .find(|update| update.slot == lodestone_model::EquipmentSlot::Head)
        .and_then(|update| update.item.as_ref())
        .expect("helmet");
    assert_ne!(helmet.count, 2);
}

#[test]
fn literal_protocol_404_truncated_equipment_is_rejected_before_ingest() {
    let result = adapter_for(PROTOCOL_1_13_2).handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        clientbound::ENTITY_EQUIPMENT,
        &ENTITY_EQUIPMENT[..ENTITY_EQUIPMENT.len() - 1],
    );
    assert!(result.is_err(), "a truncated slot NBT marker must be rejected");
}
