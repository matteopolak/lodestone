//! Hermetic tests for protocol 47 entity-metadata, slot and window packets.
//!
//! Every packet round-trips (decode∘encode is identity *and* re-encode
//! reproduces the exact bytes), malformed/truncated input yields a clean
//! `Err` rather than a panic, and the metadata terminator / slot-NBT framing
//! are pinned with byte-level goldens where a symmetric bug could hide.

use lodestone_core::{Ctx, Decode, Encode, Error, Packet, Reader, Writer};
use lodestone_v1_8::packet_ids::{BOUND_CLIENTBOUND, BOUND_SERVERBOUND, STATE_PLAY, id_for, play};
use lodestone_v1_8::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_v1_8::packets::metadata::{EntityMetadata, MetadataEntry, MetadataValue};
use lodestone_v1_8::packets::slot::Slot;
use lodestone_v1_8::packets::window::{
    CloseWindow, HeldItemSlot, OpenWindow, ServerboundCloseWindow, ServerboundHeldItemSlot,
    SetCreativeSlot, SetSlot, WindowClick, WindowItems,
};

const CTX: Ctx = Ctx { version: 47 };

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

/// A minimal valid NBT compound (empty name, immediate `TAG_End`): the exact
/// bytes `read_named_nbt` consumes for the smallest possible tag.
fn tiny_nbt() -> Vec<u8> {
    vec![0x0A, 0x00, 0x00, 0x00]
}

// ---------------------------------------------------------------------------
// Slot
// ---------------------------------------------------------------------------

#[test]
fn slot_empty_round_trips() {
    round_trip(&Slot::Empty);
    // An empty slot is exactly the two bytes of `-1i16`.
    assert_eq!(encode(&Slot::Empty), vec![0xFF, 0xFF]);
}

#[test]
fn slot_without_nbt_round_trips() {
    round_trip(&Slot::Item {
        id: 1,
        count: 64,
        damage: 0,
        nbt: None,
    });
    // No-NBT is signalled by a single trailing 0x00 (TAG_End).
    let bytes = encode(&Slot::Item {
        id: 1,
        count: 64,
        damage: 0,
        nbt: None,
    });
    assert_eq!(*bytes.last().unwrap(), 0x00);
}

#[test]
fn slot_with_nbt_round_trips_bytes_verbatim() {
    let slot = Slot::Item {
        id: 267,
        count: 1,
        damage: 5,
        nbt: Some(tiny_nbt()),
    };
    round_trip(&slot);
    let bytes = encode(&slot);
    // The NBT bytes are appended verbatim after id/count/damage (2+1+2 = 5).
    assert_eq!(&bytes[5..], tiny_nbt().as_slice());
}

#[test]
fn slot_truncated_is_clean_error() {
    // id says occupied, but count/damage/nbt are missing.
    assert!(matches!(
        try_decode::<Slot>(&[0x00, 0x01]),
        Err(Error::UnexpectedEof)
    ));
}

// ---------------------------------------------------------------------------
// Entity metadata (1.8: (type<<5)|key header, 0x7F terminator)
// ---------------------------------------------------------------------------

fn sample_metadata() -> EntityMetadata {
    EntityMetadata(vec![
        MetadataEntry {
            key: 0,
            value: MetadataValue::Byte(-3),
        },
        MetadataEntry {
            key: 1,
            value: MetadataValue::Short(1000),
        },
        MetadataEntry {
            key: 2,
            value: MetadataValue::Int(-70000),
        },
        MetadataEntry {
            key: 3,
            value: MetadataValue::Float(1.5),
        },
        MetadataEntry {
            key: 4,
            value: MetadataValue::String("Zombie".into()),
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
            value: MetadataValue::Position {
                x: 10,
                y: 64,
                z: -20,
            },
        },
        MetadataEntry {
            key: 7,
            value: MetadataValue::Rotation {
                pitch: 0.0,
                yaw: 90.0,
                roll: -45.0,
            },
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
    assert_eq!(bytes, vec![0x7F]);
    round_trip(&EntityMetadata::default());
}

#[test]
fn metadata_header_packs_type_and_key() {
    let meta = EntityMetadata(vec![MetadataEntry {
        key: 5,
        value: MetadataValue::Float(0.0),
    }]);
    let bytes = encode(&meta);
    // type 3 (float) in high 3 bits, key 5 in low 5 bits: (3<<5)|5 = 0x65.
    assert_eq!(bytes[0], 0x65);
    // 4 float bytes, then terminator.
    assert_eq!(*bytes.last().unwrap(), 0x7F);
}

#[test]
fn metadata_truncated_is_clean_error() {
    // A float header (type 3, key 0 => 0x60) with no value bytes.
    assert!(matches!(
        try_decode::<EntityMetadata>(&[0x60]),
        Err(Error::UnexpectedEof)
    ));
    // Missing terminator entirely (empty buffer).
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
        kind: 54, // zombie
        x: 100,
        y: 2048,
        z: -320,
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
// Window packets
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
    // Decoding must yield None for a non-horse window — the wire carries no
    // such field, so `when` never reads it.
    let bytes = encode(&chest);
    let back: OpenWindow = decode(&bytes);
    assert_eq!(back.entity_id, None);
}

#[test]
fn open_window_horse_carries_entity_id() {
    let horse = OpenWindow {
        window_id: 2,
        inventory_type: "EntityHorse".into(),
        window_title: "{\"text\":\"Horse\"}".into(),
        slot_count: 2,
        entity_id: Some(1337),
    };
    round_trip(&horse);
    // The horse variant appends the i32 entity id as the final 4 big-endian
    // bytes: 1337 == 0x0000_0539.
    let bytes = encode(&horse);
    assert_eq!(&bytes[bytes.len() - 4..], &[0x00, 0x00, 0x05, 0x39]);
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
            Slot::Item {
                id: 267,
                count: 1,
                damage: 3,
                nbt: Some(tiny_nbt()),
            },
        ],
    };
    round_trip(&items);
    // The count is an i16 (2 bytes) prefix: window_id then 0x00 0x03.
    let bytes = encode(&items);
    assert_eq!(&bytes[1..3], &[0x00, 0x03]);
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

#[test]
fn window_items_truncated_is_clean_error() {
    // count says 2 slots but the buffer ends after the first empty slot.
    assert!(matches!(
        try_decode::<WindowItems>(&[0x00, 0x00, 0x02, 0xFF, 0xFF]),
        Err(Error::UnexpectedEof)
    ));
}

// ---------------------------------------------------------------------------
// Generated-table wiring: every packet's #[mc(name)] resolves to an id.
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
