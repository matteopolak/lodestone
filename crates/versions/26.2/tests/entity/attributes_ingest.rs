//! Adapter-to-ingest coverage for protocol 776 `update_attributes`.
//!
//! The payload is a literal clientbound packet body, written from the wire
//! field order. It is intentionally not produced by an encoder in this crate:
//! a decoder and encoder sharing the same mistake would make a round trip pass.

use lodestone_ecs::entity::{Attributes, EntityIndex};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{app::App, NetIngest};
use lodestone_model::{ClientEvent, ConnectionState, Directive, Rotation, Vec3, VersionAdapter};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770Adapter;
use lodestone_world::World as ProtocolWorld;

/// Literal 26.2 `update_attributes` body:
///
/// - entity id `1471` (`bf 0b`)
/// - one `movement_speed` attribute (registry id `26`)
/// - base `0.25`
/// - one `minecraft:test_speed` modifier, amount `0.3`, operation `2`
///
/// These expected bytes come from the packet's field widths and independent
/// IEEE-754 encodings, not from `write_update_attributes`.
const UPDATE_ATTRIBUTES_PAYLOAD: &[u8] = &[
    0xbf, 0x0b, // entity id 1471
    0x01, // one attribute
    0x1a, // movement_speed registry id 26
    0x3f, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // base 0.25
    0x01, // one modifier
    0x14, // string length 20
    b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':',
    b't', b'e', b's', b't', b'_', b's', b'p', b'e', b'e', b'd',
    0x3f, 0xd3, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, // amount 0.3
    0x02, // ADD_MULTIPLIED_TOTAL
];

#[test]
fn adapter_update_attributes_reaches_ingest_attributes_component() {
    let adapter = V770Adapter::new();
    let mut protocol_world = ProtocolWorld::new();
    let directives = adapter
        .handle_packet(
            &mut protocol_world,
            ConnectionState::Play,
            play::clientbound::UPDATE_ATTRIBUTES,
            UPDATE_ATTRIBUTES_PAYLOAD,
        )
        .expect("literal update_attributes payload decodes");
    let [Directive::Emit(event @ ClientEvent::EntityAttributesUpdated { .. })] =
        directives.as_slice()
    else {
        panic!("expected one EntityAttributesUpdated directive, got {directives:?}");
    };

    let mut app = App::new();
    app.add_plugins(IngestPlugin);
    {
        let world = app.world_mut();
        // Seed the id index through the same event-driven ingest path. The
        // attribute packet itself must still come from the real adapter above.
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: 1471,
                uuid: None,
                entity_type: "minecraft:pig".parse().expect("valid entity key"),
                pos: Vec3::new(1.0, 64.0, 2.0),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            });
        world.resource_mut::<IngestQueue>().push(event.clone());
        world.run_schedule(NetIngest);
    }

    let world = app.world();
    let entity = world
        .resource::<EntityIndex>()
        .get(1471)
        .expect("spawn event indexed the entity id");
    let attributes = world
        .get::<Attributes>(entity)
        .expect("spawned entity carries Attributes")
        .0
        .clone();
    assert_eq!(attributes.len(), 1);
    assert_eq!(
        attributes[0].attribute.to_string(),
        "minecraft:movement_speed"
    );
    assert!((attributes[0].base - 0.25).abs() < 1.0e-12);
    assert_eq!(attributes[0].modifiers.len(), 1);
    assert_eq!(
        attributes[0].modifiers[0].id.to_string(),
        "minecraft:test_speed"
    );
    assert!((attributes[0].modifiers[0].amount - 0.3).abs() < 1.0e-12);
    assert_eq!(attributes[0].modifiers[0].operation, 2);
}
