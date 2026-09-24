//! Hermetic tests for the protocol 776 `player_rotation`, `set_camera`,
//! `open_book`, and `tab_list` packets.
//!
//! Golden bytes are hand-built from the wire specification; every decode
//! asserts zero trailing bytes so a wrong field order that happens to parse
//! is still caught.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

fn expect_err(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) {
    let result =
        adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload);
    assert!(
        result.is_err(),
        "expected packet {packet_id} to be rejected"
    );
}

/// Network-NBT bare string component: `TAG_String` tag, big-endian u16
/// length, then the UTF-8 bytes.
fn nbt_string(text: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

// ---- player_rotation --------------------------------------------------

#[test]
fn player_rotation_decodes_absolute_and_relative_flags() {
    let adapter = V770Adapter::new();
    let mut payload = 90.0f32.to_be_bytes().to_vec(); // yRot
    payload.push(0x00); // relativeY = false
    payload.extend_from_slice(&(-30.0f32).to_be_bytes()); // xRot
    payload.push(0x01); // relativeX = true
    let directives = handle(&adapter, play::clientbound::PLAYER_ROTATION, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PlayerRotationSet {
            y_rot: 90.0,
            relative_y: false,
            x_rot: -30.0,
            relative_x: true,
        })]
    );
}

#[test]
fn player_rotation_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f32.to_be_bytes().to_vec();
    payload.push(0x00);
    payload.extend_from_slice(&0.0f32.to_be_bytes());
    payload.push(0x00);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::PLAYER_ROTATION, &payload);
}

#[test]
fn player_rotation_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f32.to_be_bytes().to_vec();
    payload.push(0x00);
    // missing xRot and relativeX
    expect_err(&adapter, play::clientbound::PLAYER_ROTATION, &payload);
}

// ---- set_camera ---------------------------------------------------------

#[test]
fn set_camera_decodes_entity_id() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::SET_CAMERA, &[0xD0, 0x0F]); // VarInt 2000
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::CameraSet { entity_id: 2000 })]
    );
}

#[test]
fn set_camera_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::SET_CAMERA, &[0x01, 0xFF]);
}

// ---- open_book ------------------------------------------------------------

#[test]
fn open_book_decodes_main_hand() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::OPEN_BOOK, &[0x00]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BookOpened { main_hand: true })]
    );
}

#[test]
fn open_book_decodes_off_hand() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::OPEN_BOOK, &[0x01]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::BookOpened {
            main_hand: false
        })]
    );
}

#[test]
fn open_book_rejects_unknown_hand_ordinal() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::OPEN_BOOK, &[0x02]);
}

#[test]
fn open_book_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::OPEN_BOOK, &[0x00, 0xFF]);
}

// ---- tab_list -------------------------------------------------------------

#[test]
fn tab_list_decodes_header_and_footer() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("Welcome");
    payload.extend(nbt_string("Goodbye"));
    let directives = handle(&adapter, play::clientbound::TAB_LIST, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TabListChanged { header, footer })] => {
            assert_eq!(header.to_plain_string(), "Welcome");
            assert_eq!(footer.to_plain_string(), "Goodbye");
        }
        other => panic!("expected a single TabListChanged event, got {other:?}"),
    }
}

#[test]
fn tab_list_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("h");
    payload.extend(nbt_string("f"));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::TAB_LIST, &payload);
}

#[test]
fn tab_list_rejects_truncated_footer() {
    let adapter = V770Adapter::new();
    // header decodes fine, footer is cut off mid-string.
    let mut payload = nbt_string("h");
    payload.push(0x08);
    payload.extend_from_slice(&5u16.to_be_bytes());
    payload.extend_from_slice(b"ab"); // promised 5 bytes, only 2 present
    expect_err(&adapter, play::clientbound::TAB_LIST, &payload);
}
