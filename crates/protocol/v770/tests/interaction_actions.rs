//! Hermetic byte-exact tests for the serverbound interaction encoders:
//! `player_action`, `use_item_on`, `use_item`, `attack`, `interact`,
//! `player_input`, `player_command`, `set_carried_item`, `container_close`,
//! `container_click`, and `set_creative_mode_slot`.
//!
//! Expected payloads are built from the wire specification with independent
//! VarInt / big-endian encoders (never the adapter's own codec), so a symmetric
//! bug cannot pass. Layouts are verified against 26.2's `Serverbound*Packet`
//! stream codecs.

use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, ClientAction, ConnectionState, EntityInteraction,
    Hand, PlayerCommand, PlayerInput, Rotation, Vec3, Vec3f, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;

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
fn block_action_start_destroy_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos: lodestone_model::BlockPos {
            x: 10,
            y: 70,
            z: -3,
        },
        face: BlockFace::Up,
        sequence: 7,
    });
    assert_eq!(id, play::serverbound::PLAYER_ACTION);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(0)); // START_DESTROY_BLOCK
    want.extend_from_slice(&pack_block_pos(10, 70, -3).to_be_bytes());
    want.push(1); // Direction.UP.get3DDataValue()
    want.extend_from_slice(&varint(7));
    assert_eq!(bytes, want);
}

#[test]
fn player_action_ordinals_and_faces_are_correct() {
    for (kind, ordinal) in [
        (BlockActionKind::StartDestroy, 0),
        (BlockActionKind::AbortDestroy, 1),
        (BlockActionKind::StopDestroy, 2),
    ] {
        let (_, bytes) = encode(&ClientAction::BlockAction {
            action: kind,
            pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
            face: BlockFace::Down,
            sequence: 0,
        });
        assert_eq!(bytes[0], ordinal as u8, "ordinal for {kind:?}");
    }
    for (face, value) in [
        (BlockFace::Down, 0u8),
        (BlockFace::Up, 1),
        (BlockFace::North, 2),
        (BlockFace::South, 3),
        (BlockFace::West, 4),
        (BlockFace::East, 5),
    ] {
        let (_, bytes) = encode(&ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: lodestone_model::BlockPos { x: 0, y: 0, z: 0 },
            face,
            sequence: 0,
        });
        // action(1) + packed pos(8) => face byte at index 9.
        assert_eq!(bytes[9], value, "face byte for {face:?}");
    }
}

#[test]
fn item_actions_map_to_player_action_with_zeroed_target() {
    for (action, ordinal) in [
        (ClientAction::DropSelectedItemStack, 3),
        (ClientAction::DropSelectedItem, 4),
        (ClientAction::ReleaseUseItem, 5),
        (ClientAction::SwapItemWithOffhand, 6),
        (ClientAction::Stab, 7),
    ] {
        let (id, bytes) = encode(&action);
        assert_eq!(id, play::serverbound::PLAYER_ACTION);
        let mut want = Vec::new();
        want.extend_from_slice(&varint(ordinal));
        want.extend_from_slice(&0i64.to_be_bytes()); // BlockPos.ZERO
        want.push(0); // Direction.DOWN
        want.extend_from_slice(&varint(0)); // sequence 0
        assert_eq!(bytes, want, "payload for {action:?}");
    }
}

#[test]
fn use_item_on_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::UseItemOn {
        hand: Hand::Off,
        pos: lodestone_model::BlockPos { x: 1, y: 2, z: 3 },
        face: BlockFace::South,
        cursor: Vec3f {
            x: 0.5,
            y: 0.25,
            z: 0.75,
        },
        inside_block: true,
        sequence: 42,
    });
    assert_eq!(id, play::serverbound::USE_ITEM_ON);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(1)); // off hand
    want.extend_from_slice(&pack_block_pos(1, 2, 3).to_be_bytes());
    want.extend_from_slice(&varint(3)); // Direction.SOUTH
    want.extend_from_slice(&0.5_f32.to_be_bytes());
    want.extend_from_slice(&0.25_f32.to_be_bytes());
    want.extend_from_slice(&0.75_f32.to_be_bytes());
    want.push(1); // inside_block = true
    want.extend_from_slice(&varint(42));
    assert_eq!(bytes, want);
}

#[test]
fn use_item_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::UseItem {
        hand: Hand::Main,
        rotation: Rotation {
            yaw: 12.5,
            pitch: -7.0,
        },
        sequence: 9,
    });
    assert_eq!(id, play::serverbound::USE_ITEM);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(0)); // main hand
    want.extend_from_slice(&varint(9)); // sequence
    want.extend_from_slice(&12.5_f32.to_be_bytes()); // yaw
    want.extend_from_slice(&(-7.0_f32).to_be_bytes()); // pitch
    assert_eq!(bytes, want);
}

#[test]
fn attack_is_entity_id_only() {
    let (id, bytes) = encode(&ClientAction::InteractEntity {
        entity_id: 4096,
        interaction: EntityInteraction::Attack,
        sneaking: true, // dropped by the attack packet
    });
    assert_eq!(id, play::serverbound::ATTACK);
    assert_eq!(bytes, varint(4096));
}

#[test]
fn interact_without_target_uses_zero_lp_vec3() {
    let (id, bytes) = encode(&ClientAction::InteractEntity {
        entity_id: 5,
        interaction: EntityInteraction::Interact { hand: Hand::Off },
        sneaking: true,
    });
    assert_eq!(id, play::serverbound::INTERACT);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(5)); // entity id
    want.extend_from_slice(&varint(1)); // off hand
    want.push(0); // LpVec3 zero vector = single 0 byte
    want.push(1); // usingSecondaryAction = true
    assert_eq!(bytes, want);
}

#[test]
fn interact_at_encodes_lp_vec3_known_vector() {
    let (id, bytes) = encode(&ClientAction::InteractEntity {
        entity_id: 5,
        interaction: EntityInteraction::InteractAt {
            hand: Hand::Main,
            target: Vec3 {
                x: 0.5,
                y: -0.3,
                z: 1.0,
            },
        },
        sneaking: false,
    });
    assert_eq!(id, play::serverbound::INTERACT);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(5)); // entity id
    want.extend_from_slice(&varint(0)); // main hand
    // (0.5, -0.3, 1.0) LpVec3 bytes, computed from vanilla's algorithm with
    // Java `Math.round` (half-up) — an independent Python port of `LpVec3`.
    want.extend_from_slice(&[249, 255, 255, 252, 179, 50]);
    want.push(0); // usingSecondaryAction = false
    assert_eq!(bytes, want);
}

#[test]
fn player_input_packs_flag_bits() {
    let (id, bytes) = encode(&ClientAction::SetPlayerInput(PlayerInput {
        forward: true,
        backward: false,
        left: false,
        right: true,
        jump: false,
        shift: false,
        sprint: true,
    }));
    assert_eq!(id, play::serverbound::PLAYER_INPUT);
    // forward(1) | right(8) | sprint(64) = 73.
    assert_eq!(bytes, vec![73]);

    let (_, empty) = encode(&ClientAction::SetPlayerInput(PlayerInput::EMPTY));
    assert_eq!(empty, vec![0]);

    let (_, all) = encode(&ClientAction::SetPlayerInput(PlayerInput {
        forward: true,
        backward: true,
        left: true,
        right: true,
        jump: true,
        shift: true,
        sprint: true,
    }));
    assert_eq!(all, vec![127]);
}

#[test]
fn player_command_ordinals_and_boost() {
    let (id, bytes) = encode(&ClientAction::PlayerCommand {
        entity_id: 3,
        command: PlayerCommand::StartSprinting,
    });
    assert_eq!(id, play::serverbound::PLAYER_COMMAND);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(3)); // entity id
    want.extend_from_slice(&varint(1)); // START_SPRINTING
    want.extend_from_slice(&varint(0)); // no data
    assert_eq!(bytes, want);

    let (_, boosted) = encode(&ClientAction::PlayerCommand {
        entity_id: 3,
        command: PlayerCommand::StartRidingJump { boost: 80 },
    });
    let mut want = Vec::new();
    want.extend_from_slice(&varint(3));
    want.extend_from_slice(&varint(3)); // START_RIDING_JUMP
    want.extend_from_slice(&varint(80)); // boost data
    assert_eq!(boosted, want);
}

#[test]
fn set_carried_item_is_big_endian_short() {
    let (id, bytes) = encode(&ClientAction::SetCarriedItem { slot: 8 });
    assert_eq!(id, play::serverbound::SET_CARRIED_ITEM);
    assert_eq!(bytes, 8i16.to_be_bytes().to_vec());
}

#[test]
fn container_close_is_varint_window_id() {
    let (id, bytes) = encode(&ClientAction::ContainerClose { window_id: 1 });
    assert_eq!(id, play::serverbound::CONTAINER_CLOSE);
    assert_eq!(bytes, varint(1));
}

#[test]
fn container_click_pickup_is_byte_exact_with_hashed_stacks() {
    let (id, bytes) = encode(&ClientAction::ContainerClick {
        window_id: 1,
        state_id: 7,
        slot: 36,
        button: 0,
        click_type: lodestone_model::ContainerClickType::Pickup,
        changed_slots: vec![lodestone_model::ContainerSlotChange {
            slot: 36,
            item: Some(lodestone_model::ItemStack {
                item: "minecraft:stone".parse().expect("valid identifier"),
                count: 5,
            }),
        }],
        carried_item: None,
    });
    assert_eq!(id, play::serverbound::CONTAINER_CLICK);
    let mut want = Vec::new();
    want.extend_from_slice(&varint(1)); // container id
    want.extend_from_slice(&varint(7)); // state id
    want.extend_from_slice(&36i16.to_be_bytes()); // slot num
    want.push(0); // button num
    want.extend_from_slice(&varint(0)); // ContainerInput.PICKUP
    want.extend_from_slice(&varint(1)); // one changed slot
    want.extend_from_slice(&36i16.to_be_bytes()); // changed slot key
    want.push(1); // HashedStack present
    want.extend_from_slice(&varint(1)); // minecraft:stone item id
    want.extend_from_slice(&varint(5)); // count
    want.extend_from_slice(&varint(0)); // added components
    want.extend_from_slice(&varint(0)); // removed components
    want.push(0); // carried item HashedStack: absent
    assert_eq!(bytes, want);
}

#[test]
fn container_click_unknown_item_fails_loudly() {
    let err = V770Adapter::new()
        .encode_action(
            ConnectionState::Play,
            &ClientAction::ContainerClick {
                window_id: 1,
                state_id: 0,
                slot: 0,
                button: 0,
                click_type: lodestone_model::ContainerClickType::Pickup,
                changed_slots: Vec::new(),
                carried_item: Some(lodestone_model::ItemStack {
                    item: "minecraft:does_not_exist"
                        .parse()
                        .expect("valid identifier"),
                    count: 1,
                }),
            },
        )
        .expect_err("an unknown item key must not silently encode as some wrong id");
    assert!(matches!(err, AdapterError::Encode(_)), "got {err:?}");
}

#[test]
fn set_creative_mode_slot_with_item_is_byte_exact() {
    let (id, bytes) = encode(&ClientAction::SetCreativeModeSlot {
        slot: 36,
        item: Some(lodestone_model::ItemStack {
            item: "minecraft:stone".parse().expect("valid identifier"),
            count: 64,
        }),
    });
    assert_eq!(id, play::serverbound::SET_CREATIVE_MODE_SLOT);
    let mut want = Vec::new();
    want.extend_from_slice(&36i16.to_be_bytes()); // slot num
    want.extend_from_slice(&varint(64)); // count (non-empty since > 0)
    want.extend_from_slice(&varint(1)); // minecraft:stone item id
    want.extend_from_slice(&varint(0)); // added components
    want.extend_from_slice(&varint(0)); // removed components
    assert_eq!(bytes, want);
}

#[test]
fn set_creative_mode_slot_empty_is_a_single_zero_count() {
    let (id, bytes) = encode(&ClientAction::SetCreativeModeSlot {
        slot: 36,
        item: None,
    });
    assert_eq!(id, play::serverbound::SET_CREATIVE_MODE_SLOT);
    let mut want = Vec::new();
    want.extend_from_slice(&36i16.to_be_bytes()); // slot num
    want.extend_from_slice(&varint(0)); // empty stack: count <= 0
    assert_eq!(bytes, want);
}

#[test]
fn interaction_actions_are_ignored_outside_play() {
    // A play-only action in the configuration state must not encode.
    let encoded = V770Adapter::new()
        .encode_action(
            ConnectionState::Configuration,
            &ClientAction::InteractEntity {
                entity_id: 1,
                interaction: EntityInteraction::Attack,
                sneaking: false,
            },
        )
        .expect("no error");
    assert!(encoded.is_none(), "must be None outside play");
}
