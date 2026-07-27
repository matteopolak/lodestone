//! Hermetic tests for protocol 340 entity-metadata, slot and window packets.
//!
//! Every packet round-trips (decode∘encode is identity *and* re-encode
//! reproduces the exact bytes), malformed/truncated input yields a clean
//! `Err` rather than a panic, and the modern metadata framing (`0xFF`
//! terminator, per-entry `type: varint`) is pinned with byte-level goldens.
//! Unlike 1.8's 3-bit type field, 1.12's varint type can name a nonexistent
//! serializer, so there is an explicit invalid-type-id test here.

use lodestone_core::{Ctx, Decode, Encode, Error, Packet, Reader, Writer};
use lodestone_v735::packet_ids::{BOUND_CLIENTBOUND, BOUND_SERVERBOUND, STATE_PLAY, id_for, play};
use lodestone_v735::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_v735::packets::metadata::{EntityMetadata, MetadataEntry, MetadataValue};
use lodestone_v735::packets::position::Position;
use lodestone_v735::packets::slot::Slot;
use lodestone_v735::packets::window::{
    CloseWindow, HeldItemSlot, OpenWindow, ServerboundCloseWindow, ServerboundHeldItemSlot,
    SetCreativeSlot, SetSlot, WindowClick, WindowItems,
};
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 340 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    value
}

fn try_decode<T: Decode>(bytes: &[u8]) -> Result<T, Error> {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX)
}

fn round_trip<T>(value: &T)
where
    T: Encode + Decode + PartialEq + std::fmt::Debug,
{
    let bytes = encode(value);
    let decoded: T = decode(&bytes);
    assert_eq!(&decoded, value, "round trip mismatch");
    assert_eq!(encode(&decoded), bytes, "re-encode mismatch");
}

fn tiny_nbt() -> Vec<u8> {
    vec![0x0A, 0x00, 0x00, 0x00]
}

// ---------------------------------------------------------------------------
// Slot (identical wire format to 1.8)
// ---------------------------------------------------------------------------

#[test]
fn slot_variants_round_trip() {
    round_trip(&Slot::Empty);
    round_trip(&Slot::Item {
        id: 1,
        count: 64,
        damage: 0,
        nbt: None,
    });
    round_trip(&Slot::Item {
        id: 267,
        count: 1,
        damage: 5,
        nbt: Some(tiny_nbt()),
    });
}

#[test]
fn slot_truncated_is_clean_error() {
    assert!(matches!(
        try_decode::<Slot>(&[0x00, 0x01]),
        Err(Error::UnexpectedEof)
    ));
}

// ---------------------------------------------------------------------------
// Entity metadata (1.12: key:u8, type:varint, 0xFF terminator)
// ---------------------------------------------------------------------------

fn sample_metadata() -> EntityMetadata {
    EntityMetadata(vec![
        MetadataEntry {
            key: 0,
            value: MetadataValue::Byte(-3),
        },
        MetadataEntry {
            key: 1,
            value: MetadataValue::VarInt(-70000),
        },
        MetadataEntry {
            key: 2,
            value: MetadataValue::Float(1.5),
        },
        MetadataEntry {
            key: 3,
            value: MetadataValue::String("Zombie".into()),
        },
        MetadataEntry {
            key: 4,
            value: MetadataValue::Chat("{\"text\":\"hi\"}".into()),
        },
        MetadataEntry {
            key: 5,
            value: MetadataValue::Slot(Slot::Item {
                id: 276,
                count: 1,
                damage: 0,
                nbt: None,
            }),
        },
        MetadataEntry {
            key: 6,
            value: MetadataValue::Bool(true),
        },
        MetadataEntry {
            key: 7,
            value: MetadataValue::Rotation {
                pitch: 0.0,
                yaw: 90.0,
                roll: -45.0,
            },
        },
        MetadataEntry {
            key: 8,
            value: MetadataValue::Position(Position::new(10, 64, -20)),
        },
        MetadataEntry {
            key: 9,
            value: MetadataValue::OptPosition(Some(Position::new(-1, 2, 3))),
        },
        MetadataEntry {
            key: 10,
            value: MetadataValue::OptPosition(None),
        },
        MetadataEntry {
            key: 11,
            value: MetadataValue::Direction(4),
        },
        MetadataEntry {
            key: 12,
            value: MetadataValue::OptUuid(Some(Uuid::from_u128(0x1234_5678_9abc_def0))),
        },
        MetadataEntry {
            key: 13,
            value: MetadataValue::OptUuid(None),
        },
        MetadataEntry {
            key: 14,
            value: MetadataValue::BlockId(0),
        },
        MetadataEntry {
            key: 15,
            value: MetadataValue::Nbt(Some(tiny_nbt())),
        },
        MetadataEntry {
            key: 16,
            value: MetadataValue::Nbt(None),
        },
    ])
}

#[test]
fn metadata_every_type_round_trips() {
    round_trip(&sample_metadata());
}

#[test]
fn metadata_empty_is_just_the_terminator() {
    let bytes = encode(&EntityMetadata::default());
    assert_eq!(bytes, vec![0xFF]);
    round_trip(&EntityMetadata::default());
}

#[test]
fn metadata_entry_layout_is_key_then_type() {
    let meta = EntityMetadata(vec![MetadataEntry {
        key: 5,
        value: MetadataValue::Bool(true),
    }]);
    let bytes = encode(&meta);
    // key (5), type (6 = bool), value (0x01), terminator (0xFF).
    assert_eq!(bytes, vec![0x05, 0x06, 0x01, 0xFF]);
}

#[test]
fn metadata_invalid_type_id_is_clean_error() {
    // key 0, type 99 (no such serializer), then a byte.
    assert!(matches!(
        try_decode::<EntityMetadata>(&[0x00, 99, 0x00]),
        Err(Error::InvalidEnumVariant { .. })
    ));
}

#[test]
fn metadata_truncated_is_clean_error() {
    // key 0, type 2 (float), no value bytes.
    assert!(matches!(
        try_decode::<EntityMetadata>(&[0x00, 0x02]),
        Err(Error::UnexpectedEof)
    ));
    // No terminator at all.
    assert!(matches!(
        try_decode::<EntityMetadata>(&[]),
        Err(Error::UnexpectedEof)
    ));
}

// ---------------------------------------------------------------------------
// Entity packets carrying metadata
// ---------------------------------------------------------------------------

#[test]
fn spawn_entity_living_round_trips() {
    round_trip(&SpawnEntityLiving {
        entity_id: 42,
        entity_uuid: Uuid::from_u128(0xdead_beef),
        kind: 54,
        x: 100.5,
        y: 64.0,
        z: -320.25,
        yaw: 12,
        pitch: -4,
        head_pitch: 3,
        velocity_x: 1,
        velocity_y: 0,
        velocity_z: -2,
        metadata: sample_metadata(),
    });
}

#[test]
fn entity_metadata_packet_round_trips() {
    round_trip(&EntityMetadataPacket {
        entity_id: 7,
        metadata: sample_metadata(),
    });
}

// ---------------------------------------------------------------------------
// Window packets (structurally identical to 1.8)
// ---------------------------------------------------------------------------

#[test]
fn open_window_chest_has_no_entity_id() {
    let chest = OpenWindow {
        window_id: 1,
        inventory_type: "minecraft:chest".into(),
        window_title: "{\"text\":\"Chest\"}".into(),
        slot_count: 27,
        entity_id: None,
    };
    round_trip(&chest);
    let back: OpenWindow = decode(&encode(&chest));
    assert_eq!(back.entity_id, None);
}

#[test]
fn open_window_horse_carries_entity_id() {
    round_trip(&OpenWindow {
        window_id: 2,
        inventory_type: "EntityHorse".into(),
        window_title: "{\"text\":\"Horse\"}".into(),
        slot_count: 2,
        entity_id: Some(1337),
    });
}

#[test]
fn open_window_horse_without_entity_id_is_an_encode_error() {
    // The `when` invariant: a horse window with no entity id cannot be
    // faithfully encoded, so it fails loudly rather than writing a bogus 0.
    let invalid = OpenWindow {
        window_id: 2,
        inventory_type: "EntityHorse".into(),
        window_title: "{\"text\":\"Horse\"}".into(),
        slot_count: 2,
        entity_id: None,
    };
    let mut writer = Writer::default();
    assert!(
        invalid.encode(&mut writer, CTX).is_err(),
        "EntityHorse with entity_id None must be an encode error"
    );
}

#[test]
fn window_items_round_trips_with_i16_count() {
    let items = WindowItems {
        window_id: 0,
        items: vec![
            Slot::Empty,
            Slot::Item {
                id: 1,
                count: 32,
                damage: 0,
                nbt: None,
            },
        ],
    };
    round_trip(&items);
    let bytes = encode(&items);
    assert_eq!(&bytes[1..3], &[0x00, 0x02]);
}

#[test]
fn simple_window_packets_round_trip() {
    round_trip(&SetSlot {
        window_id: 0,
        slot: 36,
        item: Slot::Item {
            id: 5,
            count: 1,
            damage: 0,
            nbt: None,
        },
    });
    round_trip(&HeldItemSlot { slot: 3 });
    round_trip(&CloseWindow { window_id: 4 });
    round_trip(&ServerboundCloseWindow { window_id: 4 });
    round_trip(&ServerboundHeldItemSlot { slot: 8 });
    round_trip(&WindowClick {
        window_id: 1,
        slot: 10,
        button: 0,
        action: 5,
        mode: 0,
        item: Slot::Empty,
    });
    round_trip(&SetCreativeSlot {
        slot: 9,
        item: Slot::Item {
            id: 264,
            count: 64,
            damage: 0,
            nbt: None,
        },
    });
}

// ---------------------------------------------------------------------------
// Generated-table wiring
// ---------------------------------------------------------------------------

fn assert_wired<P: Packet>(expected: i32) {
    let bound = match P::BOUND {
        lodestone_core::Bound::Client => BOUND_CLIENTBOUND,
        lodestone_core::Bound::Server => BOUND_SERVERBOUND,
    };
    assert_eq!(
        id_for(STATE_PLAY, bound, P::NAME),
        Some(expected),
        "packet {} did not resolve to an id",
        P::NAME
    );
}

#[test]
fn packets_resolve_to_generated_ids() {
    assert_wired::<SpawnEntityLiving>(play::clientbound::SPAWN_ENTITY_LIVING);
    assert_wired::<EntityMetadataPacket>(play::clientbound::ENTITY_METADATA);
    assert_wired::<OpenWindow>(play::clientbound::OPEN_WINDOW);
    assert_wired::<WindowItems>(play::clientbound::WINDOW_ITEMS);
    assert_wired::<SetSlot>(play::clientbound::SET_SLOT);
    assert_wired::<HeldItemSlot>(play::clientbound::HELD_ITEM_SLOT);
    assert_wired::<CloseWindow>(play::clientbound::CLOSE_WINDOW);
    assert_wired::<WindowClick>(play::serverbound::WINDOW_CLICK);
    assert_wired::<ServerboundCloseWindow>(play::serverbound::CLOSE_WINDOW);
    assert_wired::<ServerboundHeldItemSlot>(play::serverbound::HELD_ITEM_SLOT);
    assert_wired::<SetCreativeSlot>(play::serverbound::SET_CREATIVE_SLOT);
}
