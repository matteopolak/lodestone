//! Independent protocol-776 entity fixtures through the production ingest path.
//!
//! These bytes are literal packet bodies, not output from the protocol writer.
//! The adapter events are fed to the same `NetIngest` schedule used by the
//! client, then checked as `EntityFlags`, `Equipment`, and `Attributes` ECS
//! components.  The negative control also proves an unknown attribute id is
//! rejected instead of silently becoming a neighbouring registry entry.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770Adapter;
use lodestone_world::World;

const ENTITY_ID: i32 = 17;

/// A literal `add_entity` body for a pig (registry id 100), with the packed
/// zero velocity encoded as its one-byte zero form.
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

/// Terminal main-hand stone stack: slot 0, count 1, item id 1, empty patch.
const SET_EQUIPMENT: &[u8] = &[0x11, 0x00, 0x01, 0x01, 0x00, 0x00];

/// Movement speed (26.2 attribute registry id 26), base `0.1`, and one
/// identifier-keyed modifier with amount `0.25` and operation 1.
const UPDATE_ATTRIBUTES: &[u8] = &[
    0x11, 0x01, 0x1a,
    0x3f, 0xb9, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a,
    0x01,
    0x17,
    b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':',
    b't', b'e', b's', b't', b'_', b'm', b'o', b'd', b'i', b'f', b'i', b'e', b'r',
    0x3f, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

fn decode_events(
    adapter: &V770Adapter,
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
                .expect("literal protocol-776 packet must decode")
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
fn literal_protocol_776_packets_reach_ecs_metadata_equipment_and_attributes() {
    let adapter = V770Adapter::new();
    let events = decode_events(
        &adapter,
        &[
            (play::clientbound::ADD_ENTITY, ADD_ENTITY),
            (play::clientbound::SET_ENTITY_DATA, SET_ENTITY_DATA),
            (play::clientbound::SET_EQUIPMENT, SET_EQUIPMENT),
            (play::clientbound::UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
        ],
    );
    let app = ingest(events);
    let entity = entity_components(&app);

    assert_eq!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0x05));

    let equipment = entity
        .get::<Equipment>()
        .expect("entity spawn installs an equipment component");
    assert_eq!(equipment.0.len(), 1);
    assert_eq!(equipment.0[0].slot, lodestone_model::EquipmentSlot::MainHand);
    assert_eq!(
        equipment.0[0]
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
    assert_eq!(attributes.0[0].modifiers[0].id.to_string(), "minecraft:test_modifier");
    assert_eq!(attributes.0[0].modifiers[0].amount, 0.25);
    assert_eq!(attributes.0[0].modifiers[0].operation, 1);
}

#[test]
fn literal_protocol_776_unknown_attribute_id_is_not_coerced() {
    let adapter = V770Adapter::new();
    let mut wrong = UPDATE_ATTRIBUTES.to_vec();
    wrong[2] = 0x7f;
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::UPDATE_ATTRIBUTES,
            &wrong,
        )
        .expect("the adapter's length-framed attribute path is fail-closed");
    assert!(
        directives.is_empty(),
        "unknown registry ids must not become a nearby attribute event"
    );
}
