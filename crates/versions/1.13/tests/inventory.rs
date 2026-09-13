//! Literal protocol-404 container fixtures through the production bridge.

use lodestone_core::{Ctx, State, encode_body};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, ContainerClickType, ContainerStateId, Directive,
    ItemStack, VersionAdapter,
};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_13::packet_ids::play;
use lodestone_v1_13::packets::window::{CloseWindow, OpenWindow, WindowItems};
use lodestone_v1_13::{V404Adapter, V404ServerProtocol};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 404 };

fn stone(count: u32) -> ItemStack {
    ItemStack::new("minecraft:stone".parse().expect("stone key"), count)
}

#[test]
fn protocol_404_container_packets_use_literal_server_and_client_fixtures() {
    let protocol = V404ServerProtocol;
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
            b'e', b's', b't', 16, b'{', b'"', b't', b'e', b'x', b't', b'"', b':', b'"', b'C',
            b'h', b'e', b's', b't', b'"', b'}', 27,
        ]
    );
    let directives = V404Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &payload)
        .expect("open-window fixture reaches the client adapter");
    assert!(matches!(
        directives.as_slice(),
        [Directive::Emit(ClientEvent::ScreenOpened { window_id: 1, menu_type, title })]
            if menu_type.to_string() == "minecraft:generic_9x3" && title.to_plain_string() == "Chest"
    ));

    let items = [Some(stone(1)), None];
    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_content(1, 9, &items, None)
    else {
        panic!("container contents must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::WINDOW_ITEMS);
    assert_eq!(payload, [1, 0, 2, 1, 1, 1, 0, 0]);
    let directives = V404Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &payload)
        .expect("window-items fixture reaches the client adapter");
    assert!(matches!(
        directives.as_slice(),
        [Directive::Emit(ClientEvent::ContainerContent {
            window_id: 1,
            state_id,
            items,
            carried_item: None,
        })] if *state_id == ContainerStateId::INITIAL
            && items[0].as_ref().is_some_and(|item| item.item.to_string() == "minecraft:stone" && item.count == 1)
            && items[1].is_none()
    ));

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_slot(1, 10, 0, Some(&stone(1)))
    else {
        panic!("container slot updates must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::SET_SLOT);
    assert_eq!(payload, [1, 0, 0, 1, 1, 1, 0]);
    let directives = V404Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &payload)
        .expect("set-slot fixture reaches the client adapter");
    assert!(matches!(
        directives.as_slice(),
        [Directive::Emit(ClientEvent::ContainerSlot {
            window_id: 1,
            state_id,
            slot: 0,
            item: Some(item),
        })] if *state_id == ContainerStateId::INITIAL
            && item.item.to_string() == "minecraft:stone"
            && item.count == 1
    ));
}

#[test]
fn protocol_404_click_and_close_fixtures_reach_production_consumers() {
    let adapter = V404Adapter::new();
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
        .expect("protocol-404 pickup click must encode")
        .expect("pickup click is a wire action");
    assert_eq!(packet_id, play::serverbound::WINDOW_CLICK);
    assert_eq!(payload, [1, 0, 0, 0, 0, 7, 0, 0]);
    assert_eq!(
        V404ServerProtocol.decode(State::Play, packet_id, &payload),
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
        .expect("protocol-404 close must encode")
        .expect("close is a wire action");
    assert_eq!(packet_id, play::serverbound::CLOSE_WINDOW);
    assert_eq!(payload, [1]);
    assert_eq!(
        V404ServerProtocol.decode(State::Play, packet_id, &payload),
        ServerBound::ContainerClosed { window_id: 1 }
    );

    let open = OpenWindow {
        window_id: 1,
        inventory_type: "minecraft:chest".to_owned(),
        window_title: "{\"text\":\"Chest\"}".to_owned(),
        slot_count: 27,
        entity_id: None,
    };
    assert_eq!(encode_body(&open, CTX).unwrap(), [
        1, 15, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'c', b'h',
        b'e', b's', b't', 16, b'{', b'"', b't', b'e', b'x', b't', b'"', b':', b'"', b'C',
        b'h', b'e', b's', b't', b'"', b'}', 27,
    ]);
    let items = WindowItems {
        window_id: 1,
        items: vec![lodestone_v1_13::packets::slot::Slot::Item {
            id: 1,
            count: 1,
            nbt: None,
        }],
    };
    assert_eq!(encode_body(&items, CTX).unwrap(), [1, 0, 1, 1, 1, 1, 0]);
    let close_packet = CloseWindow { window_id: 1 };
    assert_eq!(encode_body(&close_packet, CTX).unwrap(), [1]);
}
