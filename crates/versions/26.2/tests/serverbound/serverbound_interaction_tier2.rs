//! Hermetic byte-exact tests for the "interaction completeness" serverbound
//! encoders: `container_button_click`, `player_abilities` (flying toggle),
//! `rename_item`, `select_trade`, `pick_item_from_block`,
//! `pick_item_from_entity`, `set_beacon`, `edit_book`, `sign_update`, and
//! `set_command_block`.
//!
//! Expected payloads are built from the wire specification with independent
//! VarInt / big-endian encoders (never the adapter's own codec), so a
//! symmetric bug cannot pass. Layouts are verified against 26.2's
//! `ServerboundContainerButtonClickPacket`, `ServerboundPlayerAbilitiesPacket`,
//! `ServerboundRenameItemPacket`, `ServerboundSelectTradePacket`,
//! `ServerboundPickItemFromBlockPacket`, `ServerboundPickItemFromEntityPacket`,
//! `ServerboundSetBeaconPacket`, `ServerboundEditBookPacket`,
//! `ServerboundSignUpdatePacket`, and `ServerboundSetCommandBlockPacket`.

use lodestone_model::{BlockPos, ClientAction, CommandBlockMode, ConnectionState, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
}

fn encode(action: &ClientAction) -> (i32, Vec<u8>) {
    V770Adapter::new()
        .encode_action(ConnectionState::Play, action)
        .expect("encode succeeds")
        .expect("action is encoded in play state")
}

#[test]
fn container_button_click_is_two_varints() {
    let (id, bytes) = encode(&ClientAction::ContainerButtonClick {
        window_id: 5,
        button_id: 2,
    });
    assert_eq!(id, play::serverbound::CONTAINER_BUTTON_CLICK);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(5));
    want.extend_from_slice(&varint(2));
    assert_eq!(bytes, want);
}

#[test]
fn set_flying_true_sets_the_flying_bit() {
    let (id, bytes) = encode(&ClientAction::SetFlying { flying: true });
    assert_eq!(id, play::serverbound::PLAYER_ABILITIES);
    assert_eq!(bytes, vec![0x02]);
}

#[test]
fn set_flying_false_is_a_zero_flags_byte() {
    let (id, bytes) = encode(&ClientAction::SetFlying { flying: false });
    assert_eq!(id, play::serverbound::PLAYER_ABILITIES);
    assert_eq!(bytes, vec![0x00]);
}

#[test]
fn rename_item_is_a_plain_utf_string() {
    let (id, bytes) = encode(&ClientAction::RenameItem {
        name: "Excalibur".to_owned(),
    });
    assert_eq!(id, play::serverbound::RENAME_ITEM);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(9));
    want.extend_from_slice(b"Excalibur");
    assert_eq!(bytes, want);
}

#[test]
fn select_trade_is_a_single_varint() {
    let (id, bytes) = encode(&ClientAction::SelectTrade { index: 3 });
    assert_eq!(id, play::serverbound::SELECT_TRADE);
    assert_eq!(bytes, varint(3));
}

#[test]
fn pick_item_from_block_is_packed_pos_plus_bool() {
    let (id, bytes) = encode(&ClientAction::PickItemFromBlock {
        pos: BlockPos {
            x: 100,
            y: 64,
            z: -50,
        },
        include_data: true,
    });
    assert_eq!(id, play::serverbound::PICK_ITEM_FROM_BLOCK);
    let mut want = Vec::new();
    want.extend_from_slice(&pack_block_pos(100, 64, -50).to_be_bytes());
    want.push(1);
    assert_eq!(bytes, want);
}

#[test]
fn pick_item_from_entity_is_varint_entity_id_plus_bool() {
    let (id, bytes) = encode(&ClientAction::PickItemFromEntity {
        entity_id: 100,
        include_data: false,
    });
    assert_eq!(id, play::serverbound::PICK_ITEM_FROM_ENTITY);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(100));
    want.push(0);
    assert_eq!(bytes, want);
}

#[test]
fn set_beacon_with_primary_only_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::SetBeaconEffects {
        primary: Some("minecraft:speed".parse().expect("valid identifier")),
        secondary: None,
    });
    assert_eq!(id, play::serverbound::SET_BEACON);
    let mut want = Vec::new();
    want.push(1); // primary present
    want.extend_from_slice(&varint(0)); // minecraft:speed registry id
    want.push(0); // secondary absent
    assert_eq!(bytes, want);
}

#[test]
fn set_beacon_unknown_effect_fails_loudly() {
    let err = V770Adapter::new()
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SetBeaconEffects {
                primary: Some(
                    "minecraft:does_not_exist"
                        .parse()
                        .expect("valid identifier"),
                ),
                secondary: None,
            },
        )
        .expect_err("an unknown mob effect must not silently encode as some wrong id");
    assert!(
        matches!(err, lodestone_model::AdapterError::Encode(_)),
        "got {err:?}"
    );
}

#[test]
fn edit_book_with_title_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::EditBook {
        slot: 0,
        pages: vec!["Page one".to_owned(), "Page two".to_owned()],
        title: Some("My Book".to_owned()),
    });
    assert_eq!(id, play::serverbound::EDIT_BOOK);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(0)); // slot
    want.extend_from_slice(&varint(2)); // page count
    want.extend_from_slice(&varint(8));
    want.extend_from_slice(b"Page one");
    want.extend_from_slice(&varint(8));
    want.extend_from_slice(b"Page two");
    want.push(1); // title present
    want.extend_from_slice(&varint(7));
    want.extend_from_slice(b"My Book");
    assert_eq!(bytes, want);
}

#[test]
fn edit_book_draft_has_no_title() {
    let (id, bytes) = encode(&ClientAction::EditBook {
        slot: 1,
        pages: vec!["Draft page".to_owned()],
        title: None,
    });
    assert_eq!(id, play::serverbound::EDIT_BOOK);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(1)); // slot
    want.extend_from_slice(&varint(1)); // page count
    want.extend_from_slice(&varint(10));
    want.extend_from_slice(b"Draft page");
    want.push(0); // title absent
    assert_eq!(bytes, want);
}

#[test]
fn sign_update_is_packed_pos_plus_bool_plus_four_lines() {
    let (id, bytes) = encode(&ClientAction::SignUpdate {
        pos: BlockPos { x: 1, y: 2, z: 3 },
        is_front_text: true,
        lines: [
            "Line1".to_owned(),
            "Line2".to_owned(),
            "Line3".to_owned(),
            "Line4".to_owned(),
        ],
    });
    assert_eq!(id, play::serverbound::SIGN_UPDATE);
    let mut want = Vec::new();
    want.extend_from_slice(&pack_block_pos(1, 2, 3).to_be_bytes());
    want.push(1); // is_front_text
    for line in ["Line1", "Line2", "Line3", "Line4"] {
        want.extend_from_slice(&varint(line.len() as i32));
        want.extend_from_slice(line.as_bytes());
    }
    assert_eq!(bytes, want);
}

#[test]
fn set_command_block_packs_flags_and_mode() {
    let (id, bytes) = encode(&ClientAction::SetCommandBlock {
        pos: BlockPos { x: 5, y: 6, z: 7 },
        command: "say hi".to_owned(),
        mode: CommandBlockMode::Auto,
        track_output: true,
        conditional: false,
        automatic: true,
    });
    assert_eq!(id, play::serverbound::SET_COMMAND_BLOCK);
    let mut want = Vec::new();
    want.extend_from_slice(&pack_block_pos(5, 6, 7).to_be_bytes());
    want.extend_from_slice(&varint(6));
    want.extend_from_slice(b"say hi");
    want.extend_from_slice(&varint(1)); // vanilla's own command-block mode: auto
    want.push(0x01 | 0x04); // track_output | automatic
    assert_eq!(bytes, want);
}

#[test]
fn tier2_actions_are_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::SetFlying { flying: true }
            )
            .expect("encode"),
        None,
        "player abilities is a play-state action only"
    );
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::SelectTrade { index: 0 }
            )
            .expect("encode"),
        None,
        "select trade is a play-state action only"
    );
}
