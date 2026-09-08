//! Independent v1.9-era entity fixtures through the production ECS ingest.
//!
//! Each body below is a literal protocol payload.  The test does not encode a
//! packet with this crate's own structs first: a shared codec mistake therefore
//! cannot make both sides agree.  The bytes go through the selected protocol's
//! `VersionAdapter`, then through the real `NetIngest` schedule, and finally
//! into the components consumed by the client.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, EntityFlags, EntityIndex, Equipment};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ClientEvent, ConnectionState, Directive, EquipmentSlot, ItemStack, VersionAdapter};
use lodestone_v1_9::adapter::{adapter_for, PROTOCOLS};
use lodestone_v1_9::{
    packet_ids as packet_ids_340, packet_ids_110, packet_ids_210, packet_ids_316,
};
use lodestone_world::World;

const ENTITY_ID: i32 = 42;

/// Named-player spawn body shared by protocols 110, 210, 316 and 340.
///
/// The final `0xff` is the empty metadata list.  The non-integral position,
/// non-zero UUID and non-zero rotation make a wrong field width or order
/// observable before the later component assertions run.
const NAMED_ENTITY_SPAWN: &[u8] = &[
    0x2a,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
    0xee,
    0xff,
    0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x50, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xc0, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40,
    0xe0,
    0xff,
];

/// Entity id 42, shared flags index 0, byte serializer, and flags `0xc9`.
const ENTITY_METADATA: &[u8] = &[0x2a, 0x00, 0x00, 0xc9, 0xff];

/// Entity id 42, main-hand slot, legacy item id 1 (stone), count 3, damage 0,
/// and the empty NBT marker.
const ENTITY_EQUIPMENT: &[u8] = &[0x2a, 0x00, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00];

/// Entity id 42, one movement-speed snapshot, and one multiply-base modifier.
/// The fixed i32 property count and the VarInt modifier count are deliberately
/// represented separately in these bytes.
const ENTITY_ATTRIBUTES: &[u8] = &[
    0x2a,
    0x00, 0x00, 0x00, 0x01,
    0x15, b'g', b'e', b'n', b'e', b'r', b'i', b'c', b'.', b'm', b'o', b'v', b'e', b'm', b'e',
    b'n', b't', b'S', b'p', b'e', b'e', b'd',
    0x3f, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
    0xbf, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01,
];

fn packet_ids(protocol: i32) -> (i32, i32, i32, i32) {
    let (named_entity_spawn, entity_metadata, entity_equipment, entity_attributes) = match protocol {
        110 => (
            packet_ids_110::play::clientbound::NAMED_ENTITY_SPAWN,
            packet_ids_110::play::clientbound::ENTITY_METADATA,
            packet_ids_110::play::clientbound::ENTITY_EQUIPMENT,
            packet_ids_110::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        210 => (
            packet_ids_210::play::clientbound::NAMED_ENTITY_SPAWN,
            packet_ids_210::play::clientbound::ENTITY_METADATA,
            packet_ids_210::play::clientbound::ENTITY_EQUIPMENT,
            packet_ids_210::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        316 => (
            packet_ids_316::play::clientbound::NAMED_ENTITY_SPAWN,
            packet_ids_316::play::clientbound::ENTITY_METADATA,
            packet_ids_316::play::clientbound::ENTITY_EQUIPMENT,
            packet_ids_316::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        340 => (
            packet_ids_340::play::clientbound::NAMED_ENTITY_SPAWN,
            packet_ids_340::play::clientbound::ENTITY_METADATA,
            packet_ids_340::play::clientbound::ENTITY_EQUIPMENT,
            packet_ids_340::play::clientbound::ENTITY_UPDATE_ATTRIBUTES,
        ),
        other => panic!("unexpected v1.9 protocol {other}"),
    };
    (named_entity_spawn, entity_metadata, entity_equipment, entity_attributes)
}

fn decode_events<A: VersionAdapter>(
    adapter: &A,
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
                .expect("literal v1.9 packet must decode")
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
        let mut queue = app
            .world_mut()
            .resource_mut::<IngestQueue>();
        for event in events {
            queue.push(event);
        }
    }
    app.world_mut().run_schedule(NetIngest);
    app
}

fn assert_components(app: &App, protocol: i32) {
    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(ENTITY_ID)
        .unwrap_or_else(|| panic!("protocol {protocol} spawn did not populate EntityIndex"));
    let entity = world
        .get_entity(entity)
        .unwrap_or_else(|_| panic!("protocol {protocol} indexed entity is missing"));

    let flags = entity
        .get::<EntityFlags>()
        .map(|flags| flags.0)
        .unwrap_or_else(|| panic!("protocol {protocol} metadata did not reach EntityFlags"));
    assert_ne!(flags, 0, "control: the fixture must distinguish flags from a default value");
    assert_eq!(flags, 0xc9, "protocol {protocol} flags");

    let equipment = entity
        .get::<Equipment>()
        .unwrap_or_else(|| panic!("protocol {protocol} equipment component is missing"));
    let main_hand = equipment
        .0
        .iter()
        .find(|entry| entry.slot == EquipmentSlot::MainHand)
        .unwrap_or_else(|| panic!("protocol {protocol} main-hand update was not retained"));
    let expected_item = ItemStack::new("minecraft:stone".parse().unwrap(), 3);
    assert_ne!(main_hand.item.as_ref(), None, "control: an empty-slot decode must differ");
    assert_eq!(main_hand.item.as_ref(), Some(&expected_item), "protocol {protocol} item");

    let attributes = entity
        .get::<Attributes>()
        .unwrap_or_else(|| panic!("protocol {protocol} attributes component is missing"));
    let movement_speed = attributes
        .0
        .iter()
        .find(|attribute| attribute.attribute == "minecraft:movement_speed".parse().unwrap())
        .unwrap_or_else(|| panic!("protocol {protocol} movement-speed update was not retained"));
    assert_ne!(movement_speed.base, 0.0, "control: the fixture must distinguish an absent base");
    assert_eq!(movement_speed.base, 0.125, "protocol {protocol} attribute base");
    assert_eq!(movement_speed.modifiers.len(), 1, "protocol {protocol} modifier count");
    assert_eq!(movement_speed.modifiers[0].amount, -0.25, "protocol {protocol} modifier amount");
    assert_eq!(movement_speed.modifiers[0].operation, 1, "protocol {protocol} modifier operation");
}

#[test]
fn literal_v1_9_entity_updates_reach_ecs_for_every_protocol() {
    for &protocol in PROTOCOLS {
        let adapter = adapter_for(protocol);
        let (spawn, metadata, equipment, attributes) = packet_ids(protocol);
        let events = decode_events(
            &adapter,
            &[
                (spawn, NAMED_ENTITY_SPAWN),
                (metadata, ENTITY_METADATA),
                (equipment, ENTITY_EQUIPMENT),
                (attributes, ENTITY_ATTRIBUTES),
            ],
        );
        let app = ingest(events);
        assert_components(&app, protocol);
    }
}

#[test]
fn literal_v1_9_metadata_rejects_trailing_bytes_before_ingest() {
    let adapter = adapter_for(340);
    let (_, metadata, _, _) = packet_ids(340);
    let mut body = ENTITY_METADATA.to_vec();
    body.push(0x00);
    assert!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                metadata,
                &body,
            )
            .is_err(),
        "control: a valid metadata prefix followed by another byte must not reach ECS"
    );
}
