//! Hermetic tests for protocol 754 entity-metadata, slot and window packets.
//!
//! Every packet round-trips (decode∘encode is identity *and* re-encode
//! reproduces the exact bytes), malformed/truncated input yields a clean
//! `Err` rather than a panic, and the 1.16 metadata framing (`0xFF`
//! terminator, per-entry `type: varint`) is pinned with byte-level goldens.
//! The varint type can name a nonexistent serializer, so there is an explicit
//! invalid-type-id test here.

use lodestone_core::{Ctx, Decode, Encode, Error, Packet, Reader, Writer, State};
use lodestone_model::{ClientAction, ClientEvent, ConnectionState, ContainerClickType, ContainerStateId, Directive, ItemStack, VersionAdapter};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_14::{adapter_for, packet_ids_498, packet_ids_578};
use lodestone_v1_14::packet_ids::{BOUND_CLIENTBOUND, BOUND_SERVERBOUND, STATE_PLAY, id_for, play};
use lodestone_v1_14::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_v1_14::packets::metadata::{EntityMetadata, MetadataEntry, MetadataValue};
use lodestone_v1_14::packets::position::Position;
use lodestone_v1_14::packets::slot::Slot;
use lodestone_v1_14::packets::window::{
    CloseWindow, HeldItemSlot, OpenWindow, ServerboundCloseWindow, ServerboundHeldItemSlot,
    SetCreativeSlot, SetSlot, WindowClick, WindowItems,
};
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 754 };

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
// Slot (1.13.1 format: present bool, varint id, count, nbt — no damage)
// ---------------------------------------------------------------------------

#[test]
fn slot_variants_round_trip() {
    round_trip(&Slot::Empty);
    round_trip(&Slot::Item {
        id: 1,
        count: 64,
        nbt: None,
    });
    round_trip(&Slot::Item {
        id: 267,
        count: 1,
        nbt: Some(tiny_nbt()),
    });
}

#[test]
fn slot_truncated_is_clean_error() {
    // present == true, then the varint id is truncated → clean EOF.
    assert!(matches!(
        try_decode::<Slot>(&[0x01]),
        Err(Error::UnexpectedEof)
    ));
}

// ---------------------------------------------------------------------------
// Entity metadata (1.16: key:u8, type:varint, 0xFF terminator)
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
    // key (5), type (7 = bool in 1.16), value (0x01), terminator (0xFF).
    assert_eq!(bytes, vec![0x05, 0x07, 0x01, 0xFF]);
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
        metadata: EntityMetadata::default(),
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
// Window packets (1.14+ open_window: varint id/type, chat title)
// ---------------------------------------------------------------------------

#[test]
fn open_window_is_a_flat_varint_triple() {
    let chest = OpenWindow {
        window_id: 1,
        inventory_type: 2,
        window_title: "{\"text\":\"Chest\"}".into(),
    };
    round_trip(&chest);
    // window_id (1), menu type (2), then the length-prefixed title.
    let bytes = encode(&chest);
    assert_eq!(&bytes[..2], &[0x01, 0x02]);
}

#[test]
fn open_window_round_trips_various_menu_types() {
    for (window_id, menu) in [(2, 20), (127, 0), (5, 11)] {
        round_trip(&OpenWindow {
            window_id,
            inventory_type: menu,
            window_title: "{\"text\":\"W\"}".into(),
        });
    }
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
            nbt: None,
        },
    });
}

fn stone() -> ItemStack {
    ItemStack::new("minecraft:stone".parse().expect("stone key"), 1)
}

#[test]
fn every_hosted_protocol_routes_literal_container_fixtures_through_production() {
    for (protocol, open, content, slot, click, close) in [
        (
            498,
            packet_ids_498::play::clientbound::OPEN_WINDOW,
            packet_ids_498::play::clientbound::WINDOW_ITEMS,
            packet_ids_498::play::clientbound::SET_SLOT,
            packet_ids_498::play::serverbound::WINDOW_CLICK,
            packet_ids_498::play::serverbound::CLOSE_WINDOW,
        ),
        (
            578,
            packet_ids_578::play::clientbound::OPEN_WINDOW,
            packet_ids_578::play::clientbound::WINDOW_ITEMS,
            packet_ids_578::play::clientbound::SET_SLOT,
            packet_ids_578::play::serverbound::WINDOW_CLICK,
            packet_ids_578::play::serverbound::CLOSE_WINDOW,
        ),
        (
            754,
            play::clientbound::OPEN_WINDOW,
            play::clientbound::WINDOW_ITEMS,
            play::clientbound::SET_SLOT,
            play::serverbound::WINDOW_CLICK,
            play::serverbound::CLOSE_WINDOW,
        ),
    ] {
        let adapter = adapter_for(protocol);
        let host = lodestone_registry::server_protocol_for_protocol(protocol)
            .expect("hosted protocol must resolve");
        let open_body = [
            0x01, 0x02, 0x10, b'{', b'"', b't', b'e', b'x', b't', b'"', b':', b'"', b'C',
            b'h', b'e', b's', b't', b'"', b'}',
        ];
        let events = adapter
            .handle_packet(&mut lodestone_world::World::new(), ConnectionState::Play, open, &open_body)
            .expect("open fixture must reach the adapter");
        assert!(matches!(
            events.as_slice(),
            [Directive::Emit(ClientEvent::ScreenOpened { window_id: 1, menu_type, title })]
                if menu_type.to_string() == "minecraft:generic_9x3" && title.to_plain_string() == "Chest"
        ));

        let content_body = [1, 0, 2, 1, 1, 1, 0, 0];
        let events = adapter
            .handle_packet(&mut lodestone_world::World::new(), ConnectionState::Play, content, &content_body)
            .expect("content fixture must reach the adapter");
        assert!(matches!(
            events.as_slice(),
            [Directive::Emit(ClientEvent::ContainerContent { window_id: 1, state_id, items, carried_item: None })]
                if *state_id == ContainerStateId::INITIAL
                    && items.len() == 2
                    && items[0].as_ref().is_some_and(|item| item.item.to_string() == "minecraft:stone" && item.count == 1)
                    && items[1].is_none()
        ));

        let slot_body = [1, 0, 0, 1, 1, 1, 0];
        let events = adapter
            .handle_packet(&mut lodestone_world::World::new(), ConnectionState::Play, slot, &slot_body)
            .expect("slot fixture must reach the adapter");
        assert!(matches!(
            events.as_slice(),
            [Directive::Emit(ClientEvent::ContainerSlot { window_id: 1, state_id, slot: 0, item: Some(item) })]
                if *state_id == ContainerStateId::INITIAL
                    && item.item.to_string() == "minecraft:stone"
                    && item.count == 1
        ));

        let click_action = ClientAction::ContainerClick {
            window_id: 1,
            state_id: ContainerStateId::new(7),
            slot: 0,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: Vec::new(),
            carried_item: None,
        };
        let (encoded_click, click_payload) = adapter
            .encode_action(ConnectionState::Play, &click_action)
            .expect("click must encode")
            .expect("click has a wire packet");
        assert_eq!(encoded_click, click);
        assert_eq!(click_payload, [1, 0, 0, 0, 0, 7, 0, 0]);
        assert_eq!(
            host.decode(State::Play, click, &click_payload),
            ServerBound::ContainerClicked {
                window_id: 1,
                state_id: 0,
                slot: 0,
                button: 0,
                click_type: 0,
                changed_slots: Vec::new(),
                carried_item: None,
            }
        );

        let (encoded_close, close_payload) = adapter
            .encode_action(ConnectionState::Play, &ClientAction::ContainerClose { window_id: 1 })
            .expect("close must encode")
            .expect("close has a wire packet");
        assert_eq!(encoded_close, close);
        assert_eq!(close_payload, [1]);
        assert_eq!(host.decode(State::Play, close, &close_payload), ServerBound::ContainerClosed { window_id: 1 });

        let ServerDirective::Send { packet_id, payload } = host.encode_open_screen(1, "minecraft:generic_9x3", "Chest") else {
            panic!("protocol {protocol} must emit open-window");
        };
        assert_eq!(packet_id, open);
        assert_eq!(payload, open_body);
        let ServerDirective::Send { packet_id, payload } = host.encode_container_content(1, 0, &[Some(stone()), None], None) else {
            panic!("protocol {protocol} must emit window-items");
        };
        assert_eq!(packet_id, content);
        assert_eq!(payload, content_body);
        let ServerDirective::Send { packet_id, payload } = host.encode_container_slot(1, 0, 0, Some(&stone())) else {
            panic!("protocol {protocol} must emit set-slot");
        };
        assert_eq!(packet_id, slot);
        assert_eq!(payload, slot_body);
    }
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
