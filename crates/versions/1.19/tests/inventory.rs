//! Literal protocol-762 container fixtures and adapter routing controls.
//!
//! The bodies below are independently written from the 1.19.4 wire table:
//! window ids and slot locations are big-endian fixed-width values, state and
//! menu ids are VarInts, and item ids come from the committed 1.19.4 jar
//! registry report. Keeping these bytes literal prevents symmetric codec
//! mistakes from becoming the test's oracle.

use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer};
use lodestone_model::{ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_19::adapter_for;
use lodestone_v1_19::packet_ids::play as play_ids;
use lodestone_v1_19::packets::slot::Slot;
use lodestone_v1_19::packets::window::{
    ChangedSlot, OpenWindow, SetSlot, WindowClick, WindowItems,
};

const CTX: Ctx = Ctx { version: 762 };
const DIAMOND_ID_762: i32 = 760;
const OPEN_WINDOW_BODY: [u8; 19] = [
    0x01, 0x02, 0x10, 0x7b, 0x22, 0x74, 0x65, 0x78, 0x74, 0x22, 0x3a, 0x22, 0x43,
    0x68, 0x65, 0x73, 0x74, 0x22, 0x7d,
];
const WINDOW_ITEMS_BODY: [u8; 10] = [0x01, 0x07, 0x02, 0x00, 0x01, 0xf8, 0x05, 0x01, 0x00, 0x00];
const SET_SLOT_EMPTY_BODY: [u8; 5] = [0xff, 0x07, 0x00, 0x00, 0x00];
const SET_SLOT_DIAMOND_BODY: [u8; 9] = [0x01, 0x07, 0x00, 0x05, 0x01, 0xf8, 0x05, 0x01, 0x00];
const CURSOR_DIAMOND_BODY: [u8; 9] = [0xff, 0x07, 0xff, 0xff, 0x01, 0xf8, 0x05, 0x01, 0x00];
const WINDOW_CLICK_BODY: [u8; 18] = [
    0x01, 0x07, 0x00, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x1b, 0x01, 0xf8,
    0x05, 0x01, 0x00, 0x00,
];
const CLOSE_WINDOW_BODY: [u8; 1] = [0x01];

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("fixture encodes");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("fixture decodes");
    reader.ensure_empty().expect("fixture has no trailing bytes");
    value
}

fn diamond() -> Slot {
    Slot::Item {
        id: DIAMOND_ID_762,
        count: 1,
        nbt: None,
    }
}

fn diamond_item() -> ItemStack {
    ItemStack::new("minecraft:diamond".parse::<ResourceKey>().unwrap(), 1)
}

#[test]
fn literal_open_window_fixture() {
    assert_eq!(encode(&OpenWindow {
        window_id: 1,
        inventory_type: 2,
        window_title: r#"{"text":"Chest"}"#.to_owned(),
    }), OPEN_WINDOW_BODY);
    assert_eq!(decode::<OpenWindow>(&OPEN_WINDOW_BODY).window_id, 1);
}

#[test]
fn literal_content_and_slot_fixtures() {
    assert_eq!(
        encode(&WindowItems {
            window_id: 1,
            state_id: 7,
            items: vec![Slot::Empty, diamond()],
            carried_item: Slot::Empty,
        }),
        WINDOW_ITEMS_BODY
    );
    assert_eq!(
        encode(&SetSlot {
            window_id: -1,
            state_id: 7,
            slot: 0,
            item: Slot::Empty,
        }),
        SET_SLOT_EMPTY_BODY
    );
    assert_eq!(decode::<SetSlot>(&SET_SLOT_EMPTY_BODY).window_id, -1);
}

#[test]
fn literal_click_fixture_has_changed_slots_and_cursor() {
    let click = WindowClick {
        window_id: 1,
        state_id: 7,
        slot: 0,
        button: 0,
        mode: 1,
        changed_slots: vec![
            ChangedSlot {
                location: 0,
                item: Slot::Empty,
            },
            ChangedSlot {
                location: 27,
                item: diamond(),
            },
        ],
        cursor_item: Slot::Empty,
    };
    assert_eq!(encode(&click), WINDOW_CLICK_BODY);
    assert_eq!(decode::<WindowClick>(&WINDOW_CLICK_BODY), click);
}

#[test]
fn adapter_consumes_container_packets_and_encodes_click_close() {
    let adapter = adapter_for(762);
    let mut world = lodestone_world::World::new();
    let open = adapter
        .handle_packet(&mut world, ConnectionState::Play, play_ids::clientbound::OPEN_WINDOW, &OPEN_WINDOW_BODY)
        .expect("open window decodes");
    assert!(matches!(open.as_slice(), [Directive::Emit(ClientEvent::ScreenOpened { window_id: 1, .. })]));
    let content = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play_ids::clientbound::WINDOW_ITEMS,
            &WINDOW_ITEMS_BODY,
        )
        .expect("window items decodes");
    assert_eq!(
        content,
        vec![Directive::Emit(ClientEvent::ContainerContent {
            window_id: 1,
            state_id: lodestone_model::ContainerStateId::new(7),
            items: vec![None, Some(diamond_item())],
            carried_item: None,
        })])
    );
    let cursor = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play_ids::clientbound::SET_SLOT,
            &SET_SLOT_EMPTY_BODY,
        )
        .expect("cursor slot decodes");
    assert_eq!(
        cursor,
        vec![Directive::Emit(ClientEvent::CursorItemChanged { item: None })]
    );

    let item = diamond_item();
    let action = lodestone_model::ClientAction::ContainerClick {
        window_id: 1,
        state_id: lodestone_model::ContainerStateId::new(7),
        slot: 0,
        button: 0,
        click_type: lodestone_model::ContainerClickType::QuickMove,
        changed_slots: vec![
            lodestone_model::ContainerSlotChange { slot: 0, item: None },
            lodestone_model::ContainerSlotChange { slot: 27, item: Some(item) },
        ],
        carried_item: None,
    };
    let (packet, payload) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("click encodes")
        .expect("click has a packet");
    assert_eq!(packet, play_ids::serverbound::WINDOW_CLICK);
    assert_eq!(payload, WINDOW_CLICK_BODY);

    let (packet, payload) = adapter
        .encode_action(
            ConnectionState::Play,
            &lodestone_model::ClientAction::ContainerClose { window_id: 1 },
        )
        .expect("close encodes")
        .expect("close has a packet");
    assert_eq!(packet, play_ids::serverbound::CLOSE_WINDOW);
    assert_eq!(payload, CLOSE_WINDOW_BODY);

    let host = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 has a hosted server protocol");
    assert!(matches!(
        host.encode_open_screen(1, "minecraft:generic_9x3", "Chest"),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play_ids::clientbound::OPEN_WINDOW && payload == OPEN_WINDOW_BODY
    ));
    assert!(matches!(
        host.encode_container_content(1, 7, &[None, Some(diamond_item())], None),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play_ids::clientbound::WINDOW_ITEMS && payload == WINDOW_ITEMS_BODY
    ));
    assert!(matches!(
        host.encode_container_slot(-1, 7, 0, None),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play_ids::clientbound::SET_SLOT && payload == SET_SLOT_EMPTY_BODY
    ));
    assert!(matches!(
        host.encode_container_slot(1, 7, 5, Some(&diamond_item())),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play_ids::clientbound::SET_SLOT && payload == SET_SLOT_DIAMOND_BODY
    ));
    assert!(matches!(
        host.encode_container_slot(-1, 7, -1, Some(&diamond_item())),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play_ids::clientbound::SET_SLOT && payload == CURSOR_DIAMOND_BODY
    ));
    assert_eq!(
        host.decode(
            State::Play,
            play_ids::serverbound::WINDOW_CLICK,
            &WINDOW_CLICK_BODY,
        ),
        ServerBound::ContainerClicked {
            window_id: 1,
            state_id: 7,
            slot: 0,
            button: 0,
            click_type: 1,
            changed_slots: vec![(0, None), (27, Some(diamond_item()))],
            carried_item: None,
        }
    );
    assert_eq!(
        host.decode(
            State::Play,
            play_ids::serverbound::CLOSE_WINDOW,
            &CLOSE_WINDOW_BODY,
        ),
        ServerBound::ContainerClosed { window_id: 1 }
    );
}
