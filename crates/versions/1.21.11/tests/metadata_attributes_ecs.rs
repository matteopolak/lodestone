//! Independent protocol-774 entity fixtures through the production ingest path.
//!
//! The packet bodies are literal bytes captured in the protocol's field order,
//! rather than values encoded by the packet structs in this crate.  That keeps
//! the fixture independent of the decoder's own representation.  Each decoded
//! event then travels through the production `NetIngest` schedule and is
//! asserted at the ECS component boundary.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_21_11::packet_ids::play::clientbound;
use lodestone_v1_21_11::V774Adapter;
use lodestone_world::World;

const ENTITY_ID: i32 = 17;

/// A captured-shape `add_entity` body for a pig (type id 100).  The one-byte
/// zero velocity is the 1.21.11 packed zero vector, not the six-byte short
/// vector used by the preceding era.
const ADD_ENTITY: &[u8] = &[
    0x11,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
    0xee, 0xff,
    0x64,
    0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00,
    0x00, 0x40, 0x40, 0x00,
];

/// `set_entity_data`: shared flags index 0, byte serializer 0, value `0x05`.
const SET_ENTITY_DATA: &[u8] = &[0x11, 0x00, 0x00, 0x05, 0xff];

/// Head slot cleared, followed by a terminal main-hand stone stack.
const SET_EQUIPMENT: &[u8] = &[0x11, 0x85, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00];

/// Movement speed (protocol-774 mapper id 20), base `0.1`, and one UUID-keyed
/// modifier with amount `0.25` and operation 1.
const UPDATE_ATTRIBUTES: &[u8] = &[
    0x11, 0x01, 0x14,
    0x3f, 0xb9, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a,
    0x01,
    0x24,
    b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'-', b'0', b'0', b'0', b'0', b'-',
    b'0', b'0', b'0', b'0', b'-', b'0', b'0', b'0', b'0', b'-', b'0', b'0', b'0', b'0',
    b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'1',
    0x3f, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

fn decode_events(
    adapter: &V774Adapter,
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
                .unwrap_or_else(|error| panic!("literal protocol-774 packet {packet_id} must decode: {error}"))
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
fn literal_protocol_774_packets_reach_ecs_metadata_equipment_and_attributes() {
    let adapter = V774Adapter::new();
    let events = decode_events(
        &adapter,
        &[
            (clientbound::ADD_ENTITY, ADD_ENTITY),
            (clientbound::SET_ENTITY_DATA, SET_ENTITY_DATA),
            (clientbound::SET_EQUIPMENT, SET_EQUIPMENT),
            (clientbound::UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
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
    assert_eq!(
        attributes.0[0].modifiers[0].id.to_string(),
        "lodestone:protocol_774_modifier_00000000000000000000000000000001"
    );
    assert_eq!(attributes.0[0].modifiers[0].amount, 0.25);
    assert_eq!(attributes.0[0].modifiers[0].operation, 1);
}

#[test]
fn literal_protocol_774_unknown_attribute_id_is_not_coerced() {
    let adapter = V774Adapter::new();
    let mut wrong = UPDATE_ATTRIBUTES.to_vec();
    wrong[2] = 0x7f;
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        clientbound::UPDATE_ATTRIBUTES,
        &wrong,
    );
    assert!(result.is_err(), "unknown mapper ids must not become a nearby attribute");
}
