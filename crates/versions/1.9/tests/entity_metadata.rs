//! Literal-wire ingress tests for the v1-9 entity-metadata consumer.
//!
//! These bytes are the protocol's actual clientbound body layout: entity id
//! `42` as a VarInt, metadata index `0`, serializer `0` (byte), flags `0x49`,
//! and the `0xFF` list terminator. They intentionally do not use this crate's
//! encoder, so a symmetric codec mistake cannot make the production dispatch
//! test pass.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_9::adapter::{adapter_for, PROTOCOLS};
use lodestone_v1_9::{packet_ids, packet_ids_110, packet_ids_210, packet_ids_316};
use lodestone_world::World;

const ENTITY_METADATA_BODY: &[u8] = &[0x2A, 0x00, 0x00, 0x49, 0xFF];

fn packet_id(protocol: i32) -> i32 {
    match protocol {
        110 => packet_ids_110::play::clientbound::ENTITY_METADATA,
        210 => packet_ids_210::play::clientbound::ENTITY_METADATA,
        316 => packet_ids_316::play::clientbound::ENTITY_METADATA,
        340 => packet_ids::play::clientbound::ENTITY_METADATA,
        other => panic!("unexpected v1-9 protocol {other}"),
    }
}

#[test]
fn literal_entity_metadata_reaches_the_canonical_consumer_for_every_protocol() {
    for &protocol in PROTOCOLS {
        let adapter = adapter_for(protocol);
        let directives = adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                packet_id(protocol),
                ENTITY_METADATA_BODY,
            )
            .unwrap_or_else(|error| panic!("protocol {protocol} metadata must decode: {error}"));

        match directives.as_slice() {
            [Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id,
                metadata,
            })] => {
                assert_eq!(*entity_id, 42, "protocol {protocol}");
                assert_eq!(metadata.flags, Some(0x49), "protocol {protocol}");
            }
            other => panic!("protocol {protocol} did not emit the metadata event: {other:?}"),
        }
    }
}

#[test]
fn literal_entity_metadata_rejects_trailing_bytes() {
    let adapter = adapter_for(340);
    let mut body = ENTITY_METADATA_BODY.to_vec();
    body.push(0x00);
    assert!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                packet_ids::play::clientbound::ENTITY_METADATA,
                &body,
            )
            .is_err(),
        "a valid metadata prefix followed by another byte must not be accepted"
    );
}
