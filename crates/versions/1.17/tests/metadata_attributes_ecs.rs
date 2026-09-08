//! Independent protocol-756 entity fixtures through production ECS ingest.
//!
//! The packet bodies are literal wire bytes, not values produced by this
//! crate's packet encoders. That keeps a shared codec mistake from satisfying
//! both sides of the assertion. The decoded events then enter the same
//! `NetIngest` schedule used by the client driver, where the final entity
//! components are the observable result.

use lodestone_ecs::NetIngest;
use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, EntityKind, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_model::{ConnectionState, Directive, VersionAdapter};
use lodestone_v1_17::packet_ids::play::clientbound;
use lodestone_v1_17::{PROTOCOL_1_17_1, V756Adapter};
use lodestone_world::World;

const ENTITY_ID: i32 = 73;

/// A literal `spawn_entity_living` body for a pig. Coordinates, angles,
/// velocity, UUID, and the protocol-local type id are all non-default so a
/// field-order mistake cannot still produce a plausible ECS entity.
const SPAWN_ENTITY_LIVING: &[u8] = &[
    0x49, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
    0xff, 0x40, 0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x50, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xc0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0xf0, 0x30, 0x03, 0x20, 0xfe,
    0x70, 0x00, 0xa0,
];

/// Shared entity flags, index 0 and serializer type 0, followed by the list
/// terminator. `0xa5` is intentionally not a round number or the default.
const ENTITY_METADATA: &[u8] = &[0x49, 0x00, 0x00, 0xa5, 0xff];

/// Main hand is explicitly cleared and the head carries two iron helmets.
/// The high bit on the first slot is the continuation flag.
const ENTITY_EQUIPMENT: &[u8] = &[0x49, 0x80, 0x00, 0x05, 0x01, 0xea, 0x05, 0x02, 0x00];

/// A textual attribute update with one modifier. The key is the legacy wire
/// spelling, while the adapter must store the canonical model key.
const ENTITY_ATTRIBUTES_HEX: &str = concat!(
    "4901",
    "20",
    "6d696e6563726166743a67656e657269632e6d6f76656d656e745f7370656564",
    "3fd0000000000000",
    "01",
    "00000000000000000000000000000001",
    "3fe8000000000000",
    "02",
);

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(
        input.len() % 2,
        0,
        "fixture has an odd number of hex digits"
    );
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).expect("fixture is hex"))
        .collect()
}

fn app_for(metadata: &[u8], equipment: &[u8], attributes: &[u8]) -> App {
    let adapter = V756Adapter::for_protocol(PROTOCOL_1_17_1);
    let mut events = Vec::new();
    for (packet_id, body) in [
        (clientbound::SPAWN_ENTITY_LIVING, SPAWN_ENTITY_LIVING),
        (clientbound::ENTITY_METADATA, metadata),
        (clientbound::ENTITY_EQUIPMENT, equipment),
        (clientbound::ENTITY_UPDATE_ATTRIBUTES, attributes),
    ] {
        let directives = adapter
            .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, body)
            .expect("literal protocol-756 body decodes");
        events.extend(
            directives
                .into_iter()
                .filter_map(|directive| match directive {
                    Directive::Emit(event) => Some(event),
                    _ => None,
                }),
        );
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
    app
}

fn attributes_body() -> Vec<u8> {
    hex(ENTITY_ATTRIBUTES_HEX)
}

#[test]
fn literal_protocol_756_metadata_equipment_and_attributes_reach_ecs() {
    let app = app_for(ENTITY_METADATA, ENTITY_EQUIPMENT, &attributes_body());
    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("literal spawn must create an indexed ECS entity");
    let entity = world.get_entity(entity).expect("indexed entity must exist");

    assert_eq!(
        entity.get::<EntityKind>().map(|kind| kind.0.to_string()),
        Some("minecraft:pig".to_owned())
    );
    assert_eq!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0xa5));

    let equipment = entity.get::<Equipment>().expect("equipment component");
    assert_eq!(equipment.0.len(), 2);
    assert!(
        equipment.0[0].item.is_none(),
        "the explicit main-hand clear is retained"
    );
    let helmet = &equipment.0[1];
    assert_eq!(helmet.slot, lodestone_model::EquipmentSlot::Head);
    let helmet = helmet.item.as_ref().expect("head item is present");
    assert_eq!(helmet.item.to_string(), "minecraft:iron_helmet");
    assert_eq!(helmet.count, 2);

    let attributes = entity.get::<Attributes>().expect("attribute component");
    assert_eq!(attributes.0.len(), 1);
    let attribute = &attributes.0[0];
    assert_eq!(attribute.attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attribute.base, 0.25);
    assert_eq!(attribute.modifiers.len(), 1);
    assert_eq!(
        attribute.modifiers[0].id.to_string(),
        "minecraft:uuid/00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(attribute.modifiers[0].amount, 0.75);
    assert_eq!(attribute.modifiers[0].operation, 2);
}

#[test]
fn literal_protocol_756_wrong_values_change_the_observed_components() {
    let mut metadata = ENTITY_METADATA.to_vec();
    metadata[3] = 0x04;
    let app = app_for(&metadata, ENTITY_EQUIPMENT, &attributes_body());
    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("entity");
    let entity = world.get_entity(entity).expect("entity exists");
    assert_ne!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0xa5));

    let mut equipment = ENTITY_EQUIPMENT.to_vec();
    equipment[7] = 0x03;
    let app = app_for(ENTITY_METADATA, &equipment, &attributes_body());
    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("entity");
    let entity = world.get_entity(entity).expect("entity exists");
    let helmet = entity
        .get::<Equipment>()
        .expect("equipment")
        .0
        .iter()
        .find(|update| update.slot == lodestone_model::EquipmentSlot::Head)
        .and_then(|update| update.item.as_ref())
        .expect("helmet");
    assert_ne!(helmet.count, 2);

    let mut attributes = attributes_body();
    attributes[36] = 0xc0;
    let app = app_for(ENTITY_METADATA, ENTITY_EQUIPMENT, &attributes);
    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("entity");
    let entity = world.get_entity(entity).expect("entity exists");
    let attribute = &entity.get::<Attributes>().expect("attributes").0[0];
    assert_ne!(attribute.base, 0.25);
}
