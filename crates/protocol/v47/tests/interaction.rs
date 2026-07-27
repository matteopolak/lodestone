//! Hermetic tests for the protocol 47 serverbound interaction encode arms.
//!
//! These assert that each [`ClientAction`] the adapter can encode maps to the
//! right 1.8 packet id and body, and — just as importantly — that the actions
//! 1.8 genuinely cannot express fail **loudly** with [`AdapterError::Unsupported`]
//! rather than silently no-opping. A silent `Ok(None)` for, say,
//! `SwapItemWithOffhand` would let a caller believe an off-hand swap happened on
//! a version that has no off-hand at all.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, BlockPos, ChatMode, ClientAction, ClientSettings,
    ConnectionState, ContainerClickType, DisplayedSkinParts, EntityInteraction, Hand, ItemStack,
    MainHand, ParticleStatus, PlayerCommand, PlayerInput, ResourceKey, Rotation, Vec3, Vec3f,
    VersionAdapter,
};
use lodestone_v47::V47Adapter;
use lodestone_v47::packet_ids::play;
use lodestone_v47::packets::game::{BlockDig, BlockPlace, EntityAction, UseEntity, UseEntityAt};
use lodestone_v47::packets::settings::{BrandPayload, PlayerAbilities, Settings};
use lodestone_v47::packets::slot::Slot;
use lodestone_v47::packets::window::{
    EnchantItem, ServerboundCloseWindow, ServerboundHeldItemSlot, SetCreativeSlot,
};

const CTX: Ctx = Ctx { version: 47 };

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    assert_eq!(reader.remaining(), 0, "trailing bytes after decode");
    value
}

fn encode(action: &ClientAction) -> (i32, Vec<u8>) {
    V47Adapter::new()
        .encode_action(ConnectionState::Play, action)
        .expect("encode")
        .expect("some")
}

fn encode_err(action: &ClientAction) -> AdapterError {
    V47Adapter::new()
        .encode_action(ConnectionState::Play, action)
        .expect_err("expected unsupported")
}

#[test]
fn block_action_maps_to_block_dig_status_codes() {
    for (kind, expected) in [
        (BlockActionKind::StartDestroy, 0),
        (BlockActionKind::AbortDestroy, 1),
        (BlockActionKind::StopDestroy, 2),
    ] {
        let (id, body) = encode(&ClientAction::BlockAction {
            action: kind,
            pos: BlockPos::new(1, 64, 3),
            face: BlockFace::Up,
            sequence: 99,
        });
        assert_eq!(id, play::serverbound::BLOCK_DIG);
        let dig: BlockDig = decode(&body);
        assert_eq!(dig.status, expected);
        assert_eq!(dig.face, 1, "Up ordinal");
        assert_eq!(BlockPos::from(dig.location), BlockPos::new(1, 64, 3));
    }
}

#[test]
fn block_face_ordinals_match_wire_order() {
    for (face, ordinal) in [
        (BlockFace::Down, 0),
        (BlockFace::Up, 1),
        (BlockFace::North, 2),
        (BlockFace::South, 3),
        (BlockFace::West, 4),
        (BlockFace::East, 5),
    ] {
        let (_, body) = encode(&ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(0, 0, 0),
            face,
            sequence: 0,
        });
        let dig: BlockDig = decode(&body);
        assert_eq!(dig.face, ordinal, "face {face:?}");
    }
}

#[test]
fn drop_and_release_ride_on_block_dig() {
    for (action, status) in [
        (ClientAction::DropSelectedItemStack, 3),
        (ClientAction::DropSelectedItem, 4),
        (ClientAction::ReleaseUseItem, 5),
    ] {
        let (id, body) = encode(&action);
        assert_eq!(id, play::serverbound::BLOCK_DIG);
        let dig: BlockDig = decode(&body);
        assert_eq!(dig.status, status, "{action:?}");
    }
}

#[test]
fn use_item_on_block_places_with_empty_slot_and_quantised_cursor() {
    let (id, body) = encode(&ClientAction::UseItemOn {
        hand: Hand::Main,
        pos: BlockPos::new(10, 70, -4),
        face: BlockFace::East,
        cursor: Vec3f {
            x: 0.5,
            y: 1.0,
            z: 0.0,
        },
        inside_block: false,
        sequence: 7,
    });
    assert_eq!(id, play::serverbound::BLOCK_PLACE);
    let place: BlockPlace = decode(&body);
    assert_eq!(BlockPos::from(place.location), BlockPos::new(10, 70, -4));
    assert_eq!(place.direction, 5, "East ordinal");
    assert_eq!(place.held_item, Slot::Empty, "stateless: empty stack");
    assert_eq!(place.cursor_x, 8, "0.5 * 15 rounded");
    assert_eq!(place.cursor_y, 15);
    assert_eq!(place.cursor_z, 0);
}

#[test]
fn use_item_in_air_uses_sentinel_placement() {
    let (id, body) = encode(&ClientAction::UseItem {
        hand: Hand::Main,
        rotation: Rotation {
            yaw: 0.0,
            pitch: 0.0,
        },
        sequence: 0,
    });
    assert_eq!(id, play::serverbound::BLOCK_PLACE);
    let place: BlockPlace = decode(&body);
    assert_eq!(BlockPos::from(place.location), BlockPos::new(-1, -1, -1));
    assert_eq!(place.direction, -1);
    assert_eq!(place.held_item, Slot::Empty);
}

#[test]
fn off_hand_use_is_rejected_loudly() {
    for action in [
        ClientAction::UseItemOn {
            hand: Hand::Off,
            pos: BlockPos::new(0, 0, 0),
            face: BlockFace::Up,
            cursor: Vec3f {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            inside_block: false,
            sequence: 0,
        },
        ClientAction::UseItem {
            hand: Hand::Off,
            rotation: Rotation {
                yaw: 0.0,
                pitch: 0.0,
            },
            sequence: 0,
        },
    ] {
        assert!(matches!(encode_err(&action), AdapterError::Unsupported(_)));
    }
}

#[test]
fn interact_entity_maps_to_use_entity_variants() {
    let (id, body) = encode(&ClientAction::InteractEntity {
        entity_id: 42,
        interaction: EntityInteraction::Attack,
        sneaking: false,
    });
    assert_eq!(id, play::serverbound::USE_ENTITY);
    let attack: UseEntity = decode(&body);
    assert_eq!(attack.target, 42);
    assert_eq!(attack.mouse, 1);

    let (_, body) = encode(&ClientAction::InteractEntity {
        entity_id: 42,
        interaction: EntityInteraction::Interact { hand: Hand::Main },
        sneaking: false,
    });
    let interact: UseEntity = decode(&body);
    assert_eq!(interact.mouse, 0, "interact has no hand on 1.8");

    let (_, body) = encode(&ClientAction::InteractEntity {
        entity_id: 42,
        interaction: EntityInteraction::InteractAt {
            hand: Hand::Main,
            target: Vec3 {
                x: 0.1,
                y: 0.2,
                z: 0.3,
            },
        },
        sneaking: false,
    });
    let at: UseEntityAt = decode(&body);
    assert_eq!(at.mouse, 2);
    assert!((at.x - 0.1).abs() < 1e-6);
}

#[test]
fn player_commands_map_to_entity_action_ids() {
    for (command, action_id) in [
        (PlayerCommand::StopSleeping, 2),
        (PlayerCommand::StartSprinting, 3),
        (PlayerCommand::StopSprinting, 4),
        (PlayerCommand::OpenInventory, 6),
    ] {
        let (id, body) = encode(&ClientAction::PlayerCommand {
            entity_id: 5,
            command,
        });
        assert_eq!(id, play::serverbound::ENTITY_ACTION);
        let ea: EntityAction = decode(&body);
        assert_eq!(ea.entity_id, 5);
        assert_eq!(ea.action_id, action_id, "{command:?}");
        assert_eq!(ea.jump_boost, 0);
    }

    let (_, body) = encode(&ClientAction::PlayerCommand {
        entity_id: 5,
        command: PlayerCommand::StartRidingJump { boost: 80 },
    });
    let ea: EntityAction = decode(&body);
    assert_eq!(ea.action_id, 5);
    assert_eq!(ea.jump_boost, 80);
}

#[test]
fn player_commands_absent_from_1_8_fail_loudly() {
    for command in [
        PlayerCommand::StopRidingJump,
        PlayerCommand::StartFallFlying,
    ] {
        let err = encode_err(&ClientAction::PlayerCommand {
            entity_id: 1,
            command,
        });
        assert!(matches!(err, AdapterError::Unsupported(_)), "{command:?}");
    }
}

#[test]
fn container_close_and_carried_item_encode() {
    let (id, body) = encode(&ClientAction::ContainerClose { window_id: 3 });
    assert_eq!(id, play::serverbound::CLOSE_WINDOW);
    let close: ServerboundCloseWindow = decode(&body);
    assert_eq!(close.window_id, 3);

    let (id, body) = encode(&ClientAction::SetCarriedItem { slot: 4 });
    assert_eq!(id, play::serverbound::HELD_ITEM_SLOT);
    let held: ServerboundHeldItemSlot = decode(&body);
    assert_eq!(held.slot, 4);
}

#[test]
fn clearing_creative_slot_sends_empty_but_setting_needs_registry() {
    let (id, body) = encode(&ClientAction::SetCreativeModeSlot {
        slot: 36,
        item: None,
    });
    assert_eq!(id, play::serverbound::SET_CREATIVE_SLOT);
    let creative: SetCreativeSlot = decode(&body);
    assert_eq!(creative.slot, 36);
    assert_eq!(creative.item, Slot::Empty);

    let err = encode_err(&ClientAction::SetCreativeModeSlot {
        slot: 36,
        item: Some(ItemStack {
            item: "minecraft:stone".parse::<ResourceKey>().expect("key"),
            count: 1,
        }),
    });
    assert!(matches!(err, AdapterError::Unsupported(_)));
}

#[test]
fn actions_absent_from_1_8_fail_loudly() {
    let cases: [ClientAction; 4] = [
        ClientAction::SwapItemWithOffhand,
        ClientAction::Stab,
        ClientAction::SetPlayerInput(PlayerInput::EMPTY),
        ClientAction::ContainerClick {
            window_id: 0,
            state_id: 0,
            slot: 0,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: Vec::new(),
            carried_item: None,
        },
    ];
    for action in cases {
        assert!(
            matches!(encode_err(&action), AdapterError::Unsupported(_)),
            "{action:?} must be Unsupported"
        );
    }
}

#[test]
fn interaction_actions_are_ignored_outside_play() {
    let adapter = V47Adapter::new();
    let out = adapter
        .encode_action(
            ConnectionState::Login,
            &ClientAction::BlockAction {
                action: BlockActionKind::StartDestroy,
                pos: BlockPos::new(0, 0, 0),
                face: BlockFace::Up,
                sequence: 0,
            },
        )
        .expect("encode");
    assert!(
        out.is_none(),
        "no serverbound play packet before Play state"
    );
}

/// Builds a representative client-settings value with two skin bits set so the
/// bitmask packing is exercised.
fn sample_settings() -> ClientSettings {
    ClientSettings {
        locale: "en_us".to_owned(),
        view_distance: 8,
        chat_mode: ChatMode::Full,
        chat_colors: true,
        skin_parts: DisplayedSkinParts {
            cape: true,
            hat: true,
            ..DisplayedSkinParts::default()
        },
        main_hand: MainHand::Right,
        text_filtering: false,
        allow_server_listing: true,
        particle_status: ParticleStatus::All,
    }
}

#[test]
fn client_settings_encode_1_8_shape() {
    let (id, body) = encode(&ClientAction::SetClientSettings(sample_settings()));
    assert_eq!(id, play::serverbound::SETTINGS);
    // Wire layout per minecraft-data pc/1.8 `settings`: string locale, i8 view
    // distance, i8 chat flags, bool chat colors, u8 skin parts. `main_hand` is
    // absent in 1.8 (added 1.9), so the body ends after the skin-parts byte.
    assert_eq!(
        body,
        vec![5, b'e', b'n', b'_', b'u', b's', 8, 0, 1, 0b0100_0001]
    );
    let decoded: Settings = decode(&body);
    assert_eq!(decoded.locale, "en_us");
    assert_eq!(decoded.chat_flags, 0);
    assert_eq!(decoded.skin_parts, 0b0100_0001, "cape (bit0) + hat (bit6)");
}

#[test]
fn chat_mode_maps_to_visibility_value() {
    for (mode, expected) in [
        (ChatMode::Full, 0),
        (ChatMode::CommandsOnly, 1),
        (ChatMode::Hidden, 2),
    ] {
        let mut settings = sample_settings();
        settings.chat_mode = mode;
        let (_, body) = encode(&ClientAction::SetClientSettings(settings));
        let decoded: Settings = decode(&body);
        assert_eq!(decoded.chat_flags, expected);
    }
}

#[test]
fn send_brand_uses_legacy_channel() {
    let (id, body) = encode(&ClientAction::SendBrand {
        brand: "lodestone".to_owned(),
    });
    assert_eq!(id, play::serverbound::CUSTOM_PAYLOAD);
    let decoded: BrandPayload = decode(&body);
    assert_eq!(
        decoded.channel, "MC|Brand",
        "1.8 uses the pipe-namespaced channel"
    );
    assert_eq!(decoded.brand, "lodestone");
}

#[test]
fn container_button_click_maps_to_enchant_item() {
    let (id, body) = encode(&ClientAction::ContainerButtonClick {
        window_id: 3,
        button_id: 1,
    });
    assert_eq!(id, play::serverbound::ENCHANT_ITEM);
    assert_eq!(body, vec![3, 1]);
    let decoded: EnchantItem = decode(&body);
    assert_eq!(decoded.window_id, 3);
    assert_eq!(decoded.button, 1);
}

#[test]
fn set_flying_toggles_the_ability_bit() {
    for (flying, expected_flag) in [(true, 0x02_i8), (false, 0x00_i8)] {
        let (id, body) = encode(&ClientAction::SetFlying { flying });
        assert_eq!(id, play::serverbound::ABILITIES);
        // flags (i8) + flying speed (f32) + walking speed (f32) = 9 bytes.
        assert_eq!(body.len(), 9);
        assert_eq!(body[0] as i8, expected_flag);
        let decoded: PlayerAbilities = decode(&body);
        assert_eq!(decoded.flags, expected_flag);
        // The two speed fields carry the vanilla defaults the server ignores.
        assert_eq!(decoded.flying_speed, 0.05);
        assert_eq!(decoded.walking_speed, 0.1);
    }
}

#[test]
fn resource_pack_response_needs_hash_the_model_lacks() {
    // 1.8 `resource_pack_receive` carries a pack hash string; the model keys the
    // response by a Uuid instead, so it cannot be encoded faithfully and must
    // fail loudly rather than send a wrong/empty hash.
    let err = encode_err(&ClientAction::ResourcePackResponse {
        id: uuid::Uuid::from_u128(0),
        response: lodestone_model::ResourcePackResponseKind::SuccessfullyLoaded,
    });
    assert!(matches!(err, AdapterError::Unsupported(_)));
}
