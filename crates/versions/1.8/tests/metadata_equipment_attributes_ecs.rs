//! Independent protocol-47 entity fixtures through the production ECS ingest.
//!
//! The packet bodies below are literal wire bytes. They are intentionally not
//! produced by this crate's packet encoders, so a matching codec mistake
//! cannot satisfy both sides of the assertion. Each decoded event then enters
//! the same `NetIngest` schedule used by the client driver.

use lodestone_ecs::NetIngest;
use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, EntityKind, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_8::packet_ids::play::clientbound;
use lodestone_v1_8::V47Adapter;
use lodestone_world::World;

const ENTITY_ID: i32 = 42;

/// A literal `spawn_entity_living` body for a pig. Fixed-point coordinates,
/// angles and velocity are all non-default so a shifted field boundary is
/// visible before the metadata list is even folded.
const SPAWN_ENTITY_LIVING: &[u8] = &[
    0x2a, 0x5a,
    0x00, 0x00, 0x08, 0x00,
    0x00, 0x00, 0x08, 0xc0,
    0xff, 0xff, 0xff, 0xa0,
    0x20, 0xf0, 0x30,
    0x01, 0x90, 0xfc, 0xe0, 0x00, 0xc8,
    0x00, 0x0b,
    0x82, 0x02, b'H', b'i',
    0x03, 0x01,
    0x7f,
];

/// Entity 42, protocol-47 flags `0x0b` (fire, crouching, sprinting), and the
/// metadata-list terminator.
const ENTITY_METADATA: &[u8] = &[0x2a, 0x00, 0x0b, 0x7f];

/// Entity 42's helmet slot with three protocol-47 stone items. The slot uses
/// the pre-flattening signed item id, count, damage and absent-NBT fields.
const ENTITY_EQUIPMENT: &[u8] = &[
    0x2a,
    0x00, 0x04,
    0x00, 0x01, 0x03, 0x00, 0x00, 0x00,
];

/// Entity 42's generic movement-speed snapshot: base `0.125`, one UUID
/// modifier with amount `-0.25`, operation `1`.
const UPDATE_ATTRIBUTES: &[u8] = &[
    0x2a,
    0x00, 0x00, 0x00, 0x01,
    0x15,
    b'g', b'e', b'n', b'e', b'r', b'i', b'c', b'.', b'm', b'o', b'v', b'e', b'm', b'e', b'n',
    b't', b'S', b'p', b'e', b'e', b'd',
    0x3f, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0xbf, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

fn decode_events(adapter: &V47Adapter, packets: &[(i32, &[u8])]) -> Vec<ClientEvent> {
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
                .unwrap_or_else(|error| {
                    panic!("literal protocol-47 packet {packet_id} must decode: {error}")
                })
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
        .expect("literal spawn must create an indexed ECS entity");
    app.world()
        .get_entity(entity)
        .expect("indexed entity must exist")
}

fn fixture_packets() -> [(i32, &'static [u8]); 4] {
    [
        (clientbound::SPAWN_ENTITY_LIVING, SPAWN_ENTITY_LIVING),
        (clientbound::ENTITY_METADATA, ENTITY_METADATA),
        (clientbound::ENTITY_EQUIPMENT, ENTITY_EQUIPMENT),
        (clientbound::UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
    ]
}

#[test]
fn literal_protocol_47_metadata_equipment_and_attributes_reach_ecs() {
    let adapter = V47Adapter::new();
    let app = ingest(decode_events(&adapter, &fixture_packets()));
    let entity = entity_components(&app);

    assert_eq!(
        entity.get::<EntityKind>().map(|kind| kind.0.to_string()),
        Some("minecraft:pig".to_owned())
    );
    assert_eq!(entity.get::<EntityFlags>().map(|flags| flags.0), Some(0x0b));

    let equipment = entity.get::<Equipment>().expect("equipment component");
    let helmet = equipment
        .0
        .iter()
        .find(|update| update.slot == lodestone_model::EquipmentSlot::Head)
        .expect("helmet update must reach ECS");
    let item = helmet.item.as_ref().expect("helmet item must be present");
    assert_eq!(item.item.to_string(), "minecraft:stone");
    assert_eq!(item.count, 3);

    let attributes = entity.get::<Attributes>().expect("attributes component");
    assert_eq!(attributes.0.len(), 1);
    let attribute = &attributes.0[0];
    assert_eq!(attribute.attribute.to_string(), "minecraft:movement_speed");
    assert_eq!(attribute.base, 0.125);
    assert_eq!(attribute.modifiers.len(), 1);
    assert_eq!(attribute.modifiers[0].amount, -0.25);
    assert_eq!(attribute.modifiers[0].operation, 1);
}

#[test]
fn literal_protocol_47_wrong_values_change_observed_components() {
    let mut metadata = ENTITY_METADATA.to_vec();
    metadata[2] = 0x06;
    let adapter = V47Adapter::new();
    let app = ingest(decode_events(
        &adapter,
        &[
            (clientbound::SPAWN_ENTITY_LIVING, SPAWN_ENTITY_LIVING),
            (clientbound::ENTITY_METADATA, &metadata),
            (clientbound::ENTITY_EQUIPMENT, ENTITY_EQUIPMENT),
            (clientbound::UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
        ],
    ));
    assert_ne!(entity_components(&app).get::<EntityFlags>().map(|flags| flags.0), Some(0x0b));

    let mut equipment = ENTITY_EQUIPMENT.to_vec();
    equipment[5] = 0x04;
    let adapter = V47Adapter::new();
    let app = ingest(decode_events(
        &adapter,
        &[
            (clientbound::SPAWN_ENTITY_LIVING, SPAWN_ENTITY_LIVING),
            (clientbound::ENTITY_EQUIPMENT, &equipment),
            (clientbound::UPDATE_ATTRIBUTES, UPDATE_ATTRIBUTES),
        ],
    ));
    let helmet = entity_components(&app)
        .get::<Equipment>()
        .expect("equipment")
        .0
        .iter()
        .find(|update| update.slot == lodestone_model::EquipmentSlot::Head)
        .and_then(|update| update.item.as_ref())
        .expect("helmet");
    assert_ne!(helmet.count, 3);

    let mut attributes = UPDATE_ATTRIBUTES.to_vec();
    attributes[29] = 0x40;
    let adapter = V47Adapter::new();
    let app = ingest(decode_events(
        &adapter,
        &[
            (clientbound::SPAWN_ENTITY_LIVING, SPAWN_ENTITY_LIVING),
            (clientbound::UPDATE_ATTRIBUTES, &attributes),
        ],
    ));
    assert_ne!(
        entity_components(&app).get::<Attributes>().expect("attributes").0[0].base,
        0.125
    );
}

#[test]
fn literal_protocol_47_truncated_spawn_is_rejected_before_ingest() {
    let result = V47Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        clientbound::SPAWN_ENTITY_LIVING,
        &SPAWN_ENTITY_LIVING[..SPAWN_ENTITY_LIVING.len() - 1],
    );
    assert!(result.is_err(), "a truncated metadata terminator must not be accepted");
}
