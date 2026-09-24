//! Literal protocol-766 container controls through both protocol seams.

use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, ContainerClickType, ContainerSlotChange,
    Directive, ItemStack, VersionAdapter,
};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_20_6::{V766ServerProtocol, adapter_for, packet_ids};
use lodestone_v1_20_6::packets::slot::Slot;
use lodestone_v1_20_6::packets::window::{CloseWindow, OpenWindow, SetSlot, WindowItems};

const CTX: Ctx = Ctx { version: 766 };

fn hex(input: &str) -> Vec<u8> {
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).unwrap())
        .collect()
}

fn stone() -> ItemStack {
    ItemStack::new("minecraft:stone".parse().unwrap(), 1)
}

#[test]
fn server_decodes_literal_click_and_close_controls() {
    let protocol = V766ServerProtocol;
    // window=1, state=1, slot=0, button=0, mode=quick-move (1), one
    // changed slot (stone), and an empty carried stack.
    let click = hex("0101000000010100000101000000");
    assert_eq!(
        protocol.decode(State::Play, packet_ids::play::serverbound::WINDOW_CLICK, &click),
        ServerBound::ContainerClicked {
            window_id: 1,
            state_id: 1,
            slot: 0,
            button: 0,
            click_type: 1,
            changed_slots: vec![(0, Some(stone()))],
            carried_item: None,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, packet_ids::play::serverbound::CLOSE_WINDOW, &[1]),
        ServerBound::ContainerClosed { window_id: 1 }
    );
    assert_eq!(
        protocol.decode(State::Configuration, packet_ids::play::serverbound::CLOSE_WINDOW, &[1]),
        ServerBound::Ignored
    );
}

#[test]
fn server_container_encoders_match_the_external_766_layout() {
    let protocol = V766ServerProtocol;
    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_open_screen(1, "minecraft:generic_9x3", "Chest")
    else { panic!("open screen") };
    assert_eq!(packet_id, packet_ids::play::clientbound::OPEN_WINDOW);
    let expected_open = hex("01020a080004746578740005436865737400");
    assert_eq!(payload, expected_open);
    let decoded = OpenWindow::decode(&mut Reader::new(&payload), CTX).unwrap();
    assert_eq!(decoded.inventory_type, 2);

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_content(1, 1, &[Some(stone()), None], None)
    else { panic!("container content") };
    assert_eq!(packet_id, packet_ids::play::clientbound::WINDOW_ITEMS);
    assert_eq!(payload, hex("010102010100000000"));
    let mut reader = Reader::new(&payload);
    let decoded = WindowItems::decode(&mut reader, CTX).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(decoded.state_id, 1);
    assert_eq!(decoded.items, vec![
        Slot::Item { id: 1, count: 1, components: Vec::new(), removed: Vec::new() },
        Slot::Empty,
    ]);

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_container_slot(1, 2, 0, Some(&stone()))
    else { panic!("container slot") };
    assert_eq!(packet_id, packet_ids::play::clientbound::SET_SLOT);
    assert_eq!(payload, hex("0102000001010000"));
    let mut reader = Reader::new(&payload);
    assert_eq!(SetSlot::decode(&mut reader, CTX).unwrap().slot, 0);
    reader.ensure_empty().unwrap();

    let close = encode_body(&CloseWindow { window_id: 1 }, CTX).unwrap();
    assert_eq!(close, [1]);
}

#[test]
fn client_consumes_literal_content_and_encodes_a_real_click() {
    let adapter = adapter_for(766);
    // open_window: window 1, generic_9x3 menu id 2, literal NBT title.
    let open = adapter
        .handle_packet(
            &mut lodestone_world::World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::OPEN_WINDOW,
            &hex("01020a080004746578740005436865737400"),
        )
        .unwrap();
    assert!(matches!(open.as_slice(), [Directive::Emit(ClientEvent::ScreenOpened { window_id: 1, menu_type, .. })] if menu_type.to_string() == "minecraft:generic_9x3"));

    let content = adapter
        .handle_packet(
            &mut lodestone_world::World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::WINDOW_ITEMS,
            &hex("010102010100000000"),
        )
        .unwrap();
    assert!(matches!(content.as_slice(), [Directive::Emit(ClientEvent::ContainerContent { window_id: 1, state_id, items, carried_item: None })] if state_id.as_wire() == 1 && items[0].as_ref() == Some(&stone()) && items[1].is_none()));

    let click = ClientAction::ContainerClick {
        window_id: 1,
        state_id: lodestone_model::ContainerStateId::new(1),
        slot: 0,
        button: 0,
        click_type: ContainerClickType::QuickMove,
        changed_slots: vec![ContainerSlotChange { slot: 0, item: Some(stone()) }],
        carried_item: None,
    };
    let Some((packet_id, body)) = adapter.encode_action(ConnectionState::Play, &click).unwrap() else {
        panic!("click")
    };
    assert_eq!(packet_id, packet_ids::play::serverbound::WINDOW_CLICK);
    assert_eq!(body, hex("0101000000010100000101000000"));
}
