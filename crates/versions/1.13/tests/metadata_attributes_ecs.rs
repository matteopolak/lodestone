//! Independent protocol-404 metadata and attribute fixtures through ECS ingest.
//!
//! The byte arrays in this test are protocol fixtures, not values produced by
//! the packet structs.  That keeps a shared codec mistake from satisfying both
//! sides of the assertion.  The decoded events are then submitted to the same
//! `NetIngest` schedule used by the client driver, where the entity components
//! become the observable result.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{Attributes, CustomName, CustomNameVisible, EntityFlags, EntityIndex};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::NetIngest;
use lodestone_model::{ConnectionState, Directive, VersionAdapter};
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

#[test]
fn literal_protocol_404_metadata_and_attributes_reach_ecs_components() {
    let adapter = adapter_for(PROTOCOL_1_13_2);
    let mut packet_world = World::new();
    let mut events = Vec::new();

    for (packet_id, payload) in [
        (clientbound::SPAWN_ENTITY_LIVING, SPAWN_WITH_METADATA),
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
