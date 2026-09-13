//! Literal protocol-5 container fixtures through the production bridge.

use lodestone_core::{State, encode_body};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, ContainerClickType, ContainerStateId, Directive,
    ItemStack, VersionAdapter,
};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_7::V5Adapter;
use lodestone_v1_7::V5ServerProtocol;
use lodestone_v1_7::packet_ids::play;
use lodestone_v1_7::packets::window::{CloseWindow, OpenWindow, WindowItems};
use lodestone_world::World;

fn stone(count: u32) -> ItemStack {
    ItemStack::new("minecraft:stone".parse().expect("stone key"), count)
}

#[test]
fn protocol_5_container_packets_use_literal_server_and_client_fixtures() {
    let protocol = V5ServerProtocol;

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_open_screen(1, "minecraft:generic_9x3", "Chest")
    else {
        panic!("opening a chest must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::OPEN_WINDOW);
    assert_eq!(
        payload,
        [
            1, 15, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'c', b'h',
            b'e', b's', b't', 5, b'C', b'h', b'e', b's', b't', 27, 1,
        ]
    );
    let decoded = V5Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_id,
            &payload,
        )
        .expect("open-window fixture reaches the client adapter");
    assert!(matches!(
        decoded.as_slice(),
        [Directive::Emit(ClientEvent::ScreenOpened { window_id: 1, menu_type, title })]
            if menu_type.to_string() == "minecraft:generic_9x3" && title.to_plain_string() == "Chest"
    ));

    let items = [Some(stone(4)), None];
    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_content(1, 9, &items, None)
    else {
        panic!("container contents must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::WINDOW_ITEMS);
    assert_eq!(payload, [1, 0, 2, 0, 1, 4, 0, 0, 0xff, 0xff, 0xff, 0xff]);
    let decoded = V5Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_id,
            &payload,
        )
        .expect("window-items fixture reaches the client adapter");
    assert!(matches!(
        decoded.as_slice(),
        [Directive::Emit(ClientEvent::ContainerContent {
            window_id: 1,
            state_id,
            items,
            carried_item: None,
        })] if *state_id == ContainerStateId::INITIAL
            && items[0].as_ref().is_some_and(|item| item.item.to_string() == "minecraft:stone" && item.count == 4)
            && items[1].is_none()
    ));

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_slot(1, 10, 0, Some(&stone(4)))
    else {
        panic!("container slot updates must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::SET_SLOT);
    assert_eq!(payload, [1, 0, 0, 0, 1, 4, 0, 0, 0xff, 0xff]);
    let decoded = V5Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_id,
            &payload,
        )
        .expect("set-slot fixture reaches the client adapter");
    assert!(matches!(
        decoded.as_slice(),
        [Directive::Emit(ClientEvent::ContainerSlot {
            window_id: 1,
            state_id,
            slot: 0,
            item: Some(item),
        })] if *state_id == ContainerStateId::INITIAL
            && item.item.to_string() == "minecraft:stone"
            && item.count == 4
    ));
}

#[test]
fn protocol_5_click_and_close_fixtures_reach_production_consumers() {
    let adapter = V5Adapter::new();
    let click = ClientAction::ContainerClick {
        window_id: 1,
        state_id: ContainerStateId::new(7),
        slot: 0,
        button: 0,
        click_type: ContainerClickType::Pickup,
        changed_slots: Vec::new(),
        carried_item: None,
    };
    let (packet_id, payload) = adapter
        .encode_action(ConnectionState::Play, &click)
        .expect("protocol-5 pickup click must encode")
        .expect("pickup click is a wire action");
    assert_eq!(packet_id, play::serverbound::WINDOW_CLICK);
    // window id, slot, button, action number, mode, then an empty legacy slot.
    assert_eq!(payload, [1, 0, 0, 0, 0, 7, 0, 0xff, 0xff]);
    assert_eq!(
        V5ServerProtocol.decode(State::Play, packet_id, &payload),
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

    let close = ClientAction::ContainerClose { window_id: 1 };
    let (packet_id, payload) = adapter
        .encode_action(ConnectionState::Play, &close)
        .expect("protocol-5 close must encode")
        .expect("close is a wire action");
    assert_eq!(packet_id, play::serverbound::CLOSE_WINDOW);
    assert_eq!(payload, [1]);
    assert_eq!(
        V5ServerProtocol.decode(State::Play, packet_id, &payload),
        ServerBound::ContainerClosed { window_id: 1 }
    );

    // The packet definitions themselves are also checked against the same
    // literal bytes used by the production bridge, preventing a symmetric
    // packet-only round trip from hiding a wrong field order.
    let open = OpenWindow {
        window_id: 1,
        inventory_type: "minecraft:chest".to_owned(),
        window_title: "Chest".to_owned(),
        slot_count: 27,
        use_provided_title: true,
        entity_id: None,
    };
    assert_eq!(encode_body(&open, lodestone_core::Ctx { version: 5 }).unwrap(), [
        1, 15, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'c', b'h',
        b'e', b's', b't', 5, b'C', b'h', b'e', b's', b't', 27, 1,
    ]);
    let items = WindowItems {
        window_id: 1,
        items: vec![lodestone_v1_7::packets::slot::Slot {
            id: Some(1),
            count: 4,
            damage: 0,
            nbt_bytes: None,
        }],
    };
    assert_eq!(encode_body(&items, lodestone_core::Ctx { version: 5 }).unwrap(), [1, 0, 1, 0, 1, 4, 0, 0, 0xff, 0xff]);
    let close_packet = CloseWindow { window_id: 1 };
    assert_eq!(encode_body(&close_packet, lodestone_core::Ctx { version: 5 }).unwrap(), [1]);
}
