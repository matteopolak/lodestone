//! Independent protocol-766 entity fixtures through the production ingest path.
//!
//! Every packet below is a literal wire fixture.  The test does not encode a
//! packet with this crate's packet structs before decoding it, so a symmetric
//! codec mistake cannot make the assertion pass.  The decoded events are then
//! handed to the same `NetIngest` schedule used by the live client and checked
//! at the ECS component boundary.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_20_6::packet_ids::play::clientbound;
use lodestone_v1_20_6::V766Adapter;
use lodestone_world::World;

const ENTITY_ID: i32 = 17;

/// A captured-shape `spawn_entity` body for a pig (type id 77).
const SPAWN_ENTITY: &[u8] = &[
    0x11,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
    0xee, 0xff,
    0x4d,
    0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x40, 0x40, 0x00,
    0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Entity metadata index 0, serializer byte 0, shared flags `0x05`, terminator.
const ENTITY_METADATA: &[u8] = &[0x11, 0x00, 0x00, 0x05, 0xff];

/// Head slot cleared, followed by a terminal main-hand stone stack.
const ENTITY_EQUIPMENT: &[u8] = &[0x11, 0x85, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00];

/// Movement speed (`generic.movement_speed`, registry id 17), base `0.1`, and
/// one UUID modifier with amount `0.25` and operation 1.
const UPDATE_ATTRIBUTES: &[u8] = &[
    0x11, 0x01, 0x11,
    0x3f, 0xb9, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a,
    0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x3f, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

fn decode_events(
    adapter: &V766Adapter,
    packets: &[(i32, &[u8])],
) -> Vec<ClientEvent> {
    let mut packet_world = World::new();
    packets
        .iter()
        .flat_map(|(packet_id, payload)| {
            adapter
                .handle_packet(
                    &mut packet_world,
                    ConnectionState::Play,
                    *packet_id,
                    payload,
                )
                .expect("literal protocol-766 packet must decode")
                .into_iter()
                .filter_map(|directive| match directive {
                    Directive::Emit(event) => Some(event),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn ingest(events: Vec<ClientEvent>) -> App {
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

fn entity_components(app: &App) -> bevy_ecs::world::EntityRef<'_> {
    let entity = app
        .world()
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .expect("adapter spawn event must create an indexed ECS entity");
    app.world()
        .get_entity(entity)
        .expect("indexed entity must exist")
}

#[test]
fn literal_protocol_766_packets_reach_ecs_metadata_equipment_and_attributes() {
    let adapter = V766Adapter::new();
    let events = decode_events(
        &adapter,
        &[
            (clientbound::SPAWN_ENTITY, SPAWN_ENTITY),
            (clientbound::ENTITY_METADATA, ENTITY_METADATA),
            (clientbound::ENTITY_EQUIPMENT, ENTITY_EQUIPMENT),
            (clientbound::ENTITY_UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
        ],
    );
    let app = ingest(events);
    let entity = entity_components(&app);

    assert_eq!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0x05));

    let equipment = entity
        .get::<Equipment>()
        .expect("entity spawn installs an equipment component");
    assert_eq!(equipment.0.len(), 2);
    assert_eq!(equipment.0[0].slot, lodestone_model::EquipmentSlot::Head);
    assert_eq!(equipment.0[0].item, None);
    assert_eq!(equipment.0[1].slot, lodestone_model::EquipmentSlot::MainHand);
    assert_eq!(
        equipment.0[1]
            .item
            .as_ref()
            .map(|item| item.item.to_string()),
        Some("minecraft:stone".to_owned())
    );

    let attributes = entity
        .get::<Attributes>()
        .expect("entity spawn installs an attributes component");
    assert_eq!(attributes.0.len(), 1);
    assert_eq!(attributes.0[0].attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attributes.0[0].base, 0.1);
    assert_eq!(attributes.0[0].modifiers.len(), 1);
    assert_eq!(attributes.0[0].modifiers[0].amount, 0.25);
    assert_eq!(attributes.0[0].modifiers[0].operation, 1);
}

#[test]
fn literal_protocol_766_unknown_attribute_id_is_not_coerced() {
    let adapter = V766Adapter::new();
    let mut wrong = UPDATE_ATTRIBUTES.to_vec();
    wrong[2] = 0x7f;
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        clientbound::ENTITY_UPDATE_ATTRIBUTES,
        &wrong,
    );
    assert!(result.is_err(), "unknown registry ids must not become a nearby attribute");
}
