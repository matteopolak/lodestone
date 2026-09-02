//! The wire gate for the thirteen operator/debug serverbound encoders,
//! with expected bytes taken from the **record definition** in
//! `.cache/mc/26.2/src` rather than from any decoder of ours.
//!
//! # Why the expectations are hand-built byte literals
//!
//! There is no serverbound *decoder* for most of these in this crate
//! (`server_protocol.rs` routes them to `ServerBound::Ignored`), so
//! `decode(encode(x)) == x` is not merely weak evidence here — it is not
//! available. Every `expected` slice below was written out from the Java
//! `write` method or `StreamCodec` composition, field by field, which is
//! `CLAUDE.md`'s "hand-decoded spec example".
//!
//! # The three that a transliterating encoder gets wrong
//!
//! These are the reason this file exists at all, rather than a comment:
//!
//! | packet | trap |
//! |---|---|
//! | `set_structure_block` | offset/size are six **signed bytes**, not two `Vec3i`s of VarInts, and the flags byte is **last**, after `seed` |
//! | `set_jigsaw_block` | `joint` is `getSerializedName()`, a **string** — every other enum field in the family is a VarInt ordinal |
//! | `custom_click_action` | **double-framed**: an outer VarInt *byte* length wrapping the optional-NBT body |
//!
//! Each has an assertion that fails under the plausible wrong encoding, not
//! just one that passes under the right one.

use lodestone_model::{
    ClientAction, ConnectionState, Difficulty, JigsawJoint, ResourceKey,
    StructureBlockMode, StructureBlockUpdateType, StructureMirror, StructureRotation,
    TestBlockMode, TestInstanceAction, TestInstanceData, TestInstanceStatus, VersionAdapter,
};
use lodestone_v770::packet_ids::play;

/// Encodes `action` in the Play state and returns `(packet_id, body)`.
fn encode(action: &ClientAction) -> (i32, Vec<u8>) {
    lodestone_v770::adapter()
        .encode_action(ConnectionState::Play, action)
        .expect("encode must not error")
        .expect("every action in this file has a Play-state encoder")
}

fn key(name: &str) -> ResourceKey {
    name.parse().expect("test key parses")
}

#[test]
fn query_block_entity_tag_is_transaction_then_packed_pos() {
    let (id, body) = encode(&ClientAction::QueryBlockEntityTag {
        transaction_id: 7,
        // BlockPos::asLong packs x:26 | y:12 | z:26. (1, 2, 3) is a value the
        // pack helper is already independently gated on elsewhere, so this
        // asserts only the *framing* around it.
        pos: lodestone_model::BlockPos { x: 1, y: 2, z: 3 },
    });
    assert_eq!(id, play::serverbound::BLOCK_ENTITY_TAG_QUERY);
    assert_eq!(body[0], 7, "VarInt transaction id comes first");
    assert_eq!(body.len(), 1 + 8, "then exactly one packed i64 and nothing else");
}

#[test]
fn query_entity_tag_is_two_var_ints() {
    let (id, body) = encode(&ClientAction::QueryEntityTag {
        transaction_id: 9,
        entity_id: 300,
    });
    assert_eq!(id, play::serverbound::ENTITY_TAG_QUERY);
    // VarInt 9 = 0x09; VarInt 300 = 0xAC 0x02.
    assert_eq!(body, vec![0x09, 0xAC, 0x02]);
}

#[test]
fn change_difficulty_writes_the_registry_id_not_the_declaration_index_of_our_enum() {
    // `vanilla's own difficulty's own get id()` is PEACEFUL=0, EASY=1, NORMAL=2, HARD=3 — the same
    // order as ours, which is exactly why this needs asserting rather than
    // assuming: if our enum is ever reordered the ids must not move with it.
    for (difficulty, expected) in [
        (Difficulty::Peaceful, 0u8),
        (Difficulty::Easy, 1),
        (Difficulty::Normal, 2),
        (Difficulty::Hard, 3),
    ] {
        let (id, body) = encode(&ClientAction::ChangeDifficulty { difficulty });
        assert_eq!(id, play::serverbound::CHANGE_DIFFICULTY);
        assert_eq!(body, vec![expected], "{difficulty:?}");
    }
}

#[test]
fn lock_difficulty_is_one_boolean() {
    assert_eq!(
        encode(&ClientAction::LockDifficulty { locked: true }),
        (play::serverbound::LOCK_DIFFICULTY, vec![0x01])
    );
    assert_eq!(
        encode(&ClientAction::LockDifficulty { locked: false }).1,
        vec![0x00]
    );
}

#[test]
fn set_game_rules_is_a_counted_list_of_identifier_value_string_pairs() {
    let (id, body) = encode(&ClientAction::SetGameRules {
        entries: vec![
            (key("minecraft:keep_inventory"), "true".to_owned()),
            (key("minecraft:random_tick_speed"), "7".to_owned()),
        ],
    });
    assert_eq!(id, play::serverbound::SET_GAME_RULE);
    let mut expected = vec![0x02u8]; // VarInt count
    for (k, v) in [
        ("minecraft:keep_inventory", "true"),
        ("minecraft:random_tick_speed", "7"),
    ] {
        expected.push(u8::try_from(k.len()).unwrap());
        expected.extend_from_slice(k.as_bytes());
        expected.push(u8::try_from(v.len()).unwrap());
        expected.extend_from_slice(v.as_bytes());
    }
    assert_eq!(body, expected);
}

#[test]
fn set_command_minecart_is_entity_command_track_output() {
    let (id, body) = encode(&ClientAction::SetCommandMinecart {
        entity_id: 5,
        command: "say hi".to_owned(),
        track_output: true,
    });
    assert_eq!(id, play::serverbound::SET_COMMAND_MINECART);
    assert_eq!(
        body,
        [vec![0x05, 0x06], b"say hi".to_vec(), vec![0x01]].concat()
    );
}

/// **Trap 1.** Offset and size are six signed bytes and the flags byte is last.
///
/// The wrong-encoding hypothesis a transliterator reaches for is two `Vec3i`s,
/// i.e. six VarInts, with the flags packed next to the booleans they came from.
/// The length assertion below distinguishes them: six bytes vs six VarInts is
/// the same length only while every component is in `0..=127`, so the test uses
/// a **negative** offset, which a VarInt encodes in five bytes.
#[test]
fn set_structure_block_writes_signed_bytes_and_puts_flags_last() {
    let (id, body) = encode(&ClientAction::SetStructureBlock {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        update_type: StructureBlockUpdateType::LoadArea,
        mode: StructureBlockMode::Load,
        name: "t".to_owned(),
        offset: (-3, 4, -5),
        size: (6, 7, 8),
        mirror: StructureMirror::FrontBack,
        rotation: StructureRotation::Clockwise180,
        data: "".to_owned(),
        integrity: 1.0,
        seed: 42,
        ignore_entities: true,
        show_air: false,
        show_bounding_box: true,
        strict: false,
    });
    assert_eq!(id, play::serverbound::SET_STRUCTURE_BLOCK);

    let expected: Vec<u8> = [
        &0i64.to_be_bytes()[..],       // packed pos
        &[0x02],                       // UpdateType::LOAD_AREA
        &[0x01],                       // StructureMode::LOAD
        &[0x01, b't'],                 // name
        &[0xFDu8, 0x04, 0xFB],         // offset: -3, 4, -5 as *bytes*
        &[0x06, 0x07, 0x08],           // size
        &[0x02],                       // Mirror::FRONT_BACK
        &[0x02],                       // Rotation::CLOCKWISE_180
        &[0x00],                       // empty data string
        &1.0f32.to_be_bytes()[..],     // integrity
        &[0x2A],                       // VarLong seed
        &[0b0000_0101],                // flags: ignoreEntities | showBoundingBox
    ]
    .concat();
    assert_eq!(body, expected);

    // The control for the "six bytes, not six VarInts" claim: under the wrong
    // hypothesis a negative component costs five bytes, so the body would be
    // four bytes longer per negative axis. Two negatives here.
    assert_eq!(
        body.len(),
        expected.len(),
        "length changed — see the wrong-encoding note on this test"
    );
    assert_ne!(
        body.len(),
        expected.len() + 8,
        "offset/size were encoded as VarInts, not signed bytes"
    );
    // And flags really are last, not adjacent to the mirror/rotation block.
    assert_eq!(*body.last().unwrap(), 0b0000_0101);
}

/// Clamping matches vanilla's *read* side, which narrows rather than refusing.
#[test]
fn set_structure_block_clamps_offset_and_size_the_way_vanilla_reads_them() {
    let (_, body) = encode(&ClientAction::SetStructureBlock {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        update_type: StructureBlockUpdateType::UpdateData,
        mode: StructureBlockMode::Save,
        name: String::new(),
        offset: (-100, 100, 0),
        size: (-5, 100, 0),
        mirror: StructureMirror::None,
        rotation: StructureRotation::None,
        data: String::new(),
        integrity: 5.0,
        seed: 0,
        ignore_entities: false,
        show_air: false,
        show_bounding_box: false,
        strict: false,
    });
    // 8 pos + 1 update + 1 mode + 1 empty name = offset starts at index 11.
    assert_eq!(&body[11..17], &[0xD0, 0x30, 0x00, 0x00, 0x30, 0x00]);
    //           -48 = 0xD0        48 = 0x30      0 (from -5)   48   0
    // Integrity is clamped into 0..=1 as well.
    let integrity_at = 17 + 1 /* mirror */ + 1 /* rotation */ + 1 /* empty data */;
    assert_eq!(
        &body[integrity_at..integrity_at + 4],
        &1.0f32.to_be_bytes()
    );
}

/// **Trap 2.** `joint` is a string, not an ordinal.
#[test]
fn set_jigsaw_block_writes_the_joint_as_its_serialized_name() {
    let (id, body) = encode(&ClientAction::SetJigsawBlock {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        name: key("minecraft:a"),
        target: key("minecraft:b"),
        pool: key("minecraft:c"),
        final_state: "minecraft:air".to_owned(),
        joint: JigsawJoint::Rollable,
        selection_priority: 1,
        placement_priority: 2,
    });
    assert_eq!(id, play::serverbound::SET_JIGSAW_BLOCK);

    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("rollable"),
        "joint must appear as the literal string \"rollable\"; body was {body:?}"
    );
    // The wrong hypothesis: a VarInt ordinal, which for Rollable would be a
    // single 0x01 byte and would leave no "rollable" anywhere in the payload.
    // A length-prefixed "rollable" is nine bytes, so the two differ by eight.
    let expected: Vec<u8> = [
        &0i64.to_be_bytes()[..],
        &[0x0B],
        b"minecraft:a",
        &[0x0B],
        b"minecraft:b",
        &[0x0B],
        b"minecraft:c",
        &[0x0D],
        b"minecraft:air",
        &[0x08],
        b"rollable",
        &[0x01, 0x02],
    ]
    .concat();
    assert_eq!(body, expected);
    assert_eq!(
        JigsawJoint::Aligned.serialized_name(),
        "aligned",
        "the other variant's name is the one the server defaults to"
    );
}

#[test]
fn jigsaw_generate_is_pos_levels_keep() {
    let (id, body) = encode(&ClientAction::GenerateJigsawStructure {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        levels: 3,
        keep_jigsaws: false,
    });
    assert_eq!(id, play::serverbound::JIGSAW_GENERATE);
    assert_eq!(body, [&0i64.to_be_bytes()[..], &[0x03, 0x00]].concat());
}

#[test]
fn set_test_block_is_pos_mode_message() {
    let (id, body) = encode(&ClientAction::SetTestBlock {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        mode: TestBlockMode::Accept,
        message: "ok".to_owned(),
    });
    assert_eq!(id, play::serverbound::SET_TEST_BLOCK);
    assert_eq!(
        body,
        [&0i64.to_be_bytes()[..], &[0x03, 0x02], b"ok".as_slice()].concat()
    );
}

#[test]
fn test_instance_block_action_carries_the_nested_data_record() {
    let (id, body) = encode(&ClientAction::TestInstanceBlockAction {
        pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
        action: TestInstanceAction::Run,
        data: TestInstanceData {
            test: Some(key("minecraft:t")),
            size: (1, 2, 3),
            rotation: StructureRotation::Clockwise90,
            ignore_entities: true,
            status: TestInstanceStatus::Running,
            error_message: None,
        },
    });
    assert_eq!(id, play::serverbound::TEST_INSTANCE_BLOCK_ACTION);
    let expected: Vec<u8> = [
        &0i64.to_be_bytes()[..],
        &[0x06],       // Action::RUN
        &[0x01, 0x0B], // test present, identifier length
        b"minecraft:t",
        &[0x01, 0x02, 0x03], // size, three VarInts
        &[0x01],             // Rotation::CLOCKWISE_90
        &[0x01],             // ignoreEntities
        &[0x01],             // Status::RUNNING
        &[0x00],             // no error message
    ]
    .concat();
    assert_eq!(body, expected);
}

#[test]
fn debug_subscription_request_resolves_names_to_registry_ids_and_drops_unknowns() {
    let (id, body) = encode(&ClientAction::SubscribeDebug {
        subscriptions: vec![
            key("minecraft:entity_paths"),
            key("lodestone:not_a_real_feed"),
            key("minecraft:bees"),
        ],
    });
    assert_eq!(id, play::serverbound::DEBUG_SUBSCRIPTION_REQUEST);
    // entity_paths = 5, bees = 1, per registries.json. The unknown key is
    // dropped rather than failing the whole subscription, so the count is 2.
    assert_eq!(body, vec![0x02, 0x05, 0x01]);
}

#[test]
fn an_empty_debug_subscription_is_a_valid_unsubscribe_not_an_error() {
    let (_, body) = encode(&ClientAction::SubscribeDebug {
        subscriptions: Vec::new(),
    });
    assert_eq!(body, vec![0x00]);
}

/// **Trap 3.** The payload is length-prefixed inside the packet.
#[test]
fn custom_click_action_length_prefixes_its_payload() {
    // A three-byte inner body: present byte, then a two-byte NBT stand-in. The
    // point of the test is the framing, so the NBT itself is opaque here — which
    // is exactly how the encoder treats it.
    let payload = vec![0x01, 0x0A, 0x00];
    let (id, body) = encode(&ClientAction::CustomClickAction {
        id: key("minecraft:my_button"),
        payload: payload.clone(),
    });
    assert_eq!(id, play::serverbound::CUSTOM_CLICK_ACTION);
    let expected: Vec<u8> = [
        &[0x13][..],
        b"minecraft:my_button",
        &[0x03], // VarInt *byte* length of the payload
        &payload,
    ]
    .concat();
    assert_eq!(body, expected);

    // The wrong hypothesis is writing the payload bare, with no length prefix.
    // That body is exactly one byte shorter, so the length alone separates them.
    assert_eq!(
        body.len(),
        1 + "minecraft:my_button".len() + 1 + payload.len(),
        "payload was written without its VarInt byte-length prefix"
    );
}

#[test]
fn custom_click_action_refuses_a_payload_over_the_wire_limit() {
    let adapter = lodestone_v770::adapter();
    let result = adapter.encode_action(
        ConnectionState::Play,
        &ClientAction::CustomClickAction {
            id: key("minecraft:big"),
            payload: vec![0u8; 65_537],
        },
    );
    assert!(
        result.is_err(),
        "a payload over 65536 bytes must be refused, not silently truncated"
    );
    // The control: one byte under the limit is accepted, so the assertion above
    // is about the limit and not about large payloads in general.
    let ok = adapter.encode_action(
        ConnectionState::Play,
        &ClientAction::CustomClickAction {
            id: key("minecraft:big"),
            payload: vec![0u8; 65_536],
        },
    );
    assert!(ok.is_ok(), "the limit itself must be accepted");
}

/// `custom_click_action` exists in Configuration too, because `show_dialog`
/// does. Everything else in this file is Play-only and must encode to nothing
/// outside it — the control that the `state` guards are real.
#[test]
fn state_guards_are_real() {
    let adapter = lodestone_v770::adapter();
    assert!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::CustomClickAction {
                    id: key("minecraft:b"),
                    payload: vec![0x00],
                },
            )
            .expect("no error")
            .is_some(),
        "custom_click_action must encode in Configuration"
    );
    assert!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::LockDifficulty { locked: true },
            )
            .expect("no error")
            .is_none(),
        "lock_difficulty is Play-only; a Configuration encode would be a wrong packet id"
    );
}
