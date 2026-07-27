//! Hermetic tests for the protocol 754 serverbound interaction encode arms.
//!
//! Mirrors the v47 suite but asserts the 1.16-specific divergences: the off-hand
//! exists (so `SwapItemWithOffhand` maps to `block_dig` status 6 and
//! interact/interact-at carry a hand), `block_place` uses a varint hand and float
//! cursor with no inline item, using an item in the air is the dedicated
//! `use_item` packet (not the legacy `block_place` sentinel), and the full
//! `entity_action` id set is available. The actions that genuinely cannot be
//! expressed still fail loudly.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, BlockPos, ChatMode, ClientAction, ClientSettings,
    ConnectionState, ContainerClickType, DisplayedSkinParts, EntityInteraction, Hand, ItemStack,
    MainHand, ParticleStatus, PlayerCommand, PlayerInput, ResourceKey, ResourcePackResponseKind,
    Rotation, Vec3, Vec3f, VersionAdapter,
};
use lodestone_v735::V735Adapter;
use lodestone_v735::packet_ids::play;
use lodestone_v735::packets::game::{
    BlockDig, BlockPlace, EntityAction, UseEntity, UseEntityAt, UseEntityInteract, UseItem,
};
use lodestone_v735::packets::settings::{
    BrandPayload, PlayerAbilities, ResourcePackReceive, Settings,
};
use lodestone_v735::packets::slot::Slot;
use lodestone_v735::packets::window::{
    EnchantItem, ServerboundCloseWindow, ServerboundHeldItemSlot, SetCreativeSlot,
};

const CTX: Ctx = Ctx { version: 754 };

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    assert_eq!(reader.remaining(), 0, "trailing bytes after decode");
    value
}

fn encode(action: &ClientAction) -> (i32, Vec<u8>) {
    V735Adapter::new()
        .encode_action(ConnectionState::Play, action)
        .expect("encode")
        .expect("some")
}

fn encode_err(action: &ClientAction) -> AdapterError {
    V735Adapter::new()
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
fn swap_offhand_is_block_dig_status_6() {
    let (id, body) = encode(&ClientAction::SwapItemWithOffhand);
    assert_eq!(id, play::serverbound::BLOCK_DIG);
    let dig: BlockDig = decode(&body);
    assert_eq!(dig.status, 6, "1.9+ has an off-hand");
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
fn use_item_on_block_sends_hand_and_float_cursor_no_item() {
    let (id, body) = encode(&ClientAction::UseItemOn {
        hand: Hand::Off,
        pos: BlockPos::new(10, 70, -4),
        face: BlockFace::East,
        cursor: Vec3f {
            x: 0.5,
            y: 1.0,
            z: 0.25,
        },
        inside_block: false,
        sequence: 7,
    });
    assert_eq!(id, play::serverbound::BLOCK_PLACE);
    let place: BlockPlace = decode(&body);
    assert_eq!(BlockPos::from(place.location), BlockPos::new(10, 70, -4));
    assert_eq!(place.direction, 5, "East ordinal");
    assert_eq!(place.hand, 1, "off-hand carried on 1.16");
    assert!((place.cursor_x - 0.5).abs() < 1e-6);
    assert!((place.cursor_y - 1.0).abs() < 1e-6);
    assert!((place.cursor_z - 0.25).abs() < 1e-6);
}

#[test]
fn use_item_in_air_sends_dedicated_use_item() {
    let (id, body) = encode(&ClientAction::UseItem {
        hand: Hand::Main,
        rotation: Rotation {
            yaw: 0.0,
            pitch: 0.0,
        },
        sequence: 0,
    });
    assert_eq!(id, play::serverbound::USE_ITEM);
    let use_item: UseItem = decode(&body);
    assert_eq!(use_item.hand, 0, "main-hand ordinal");
}

#[test]
fn interact_entity_carries_hand_on_1_16() {
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
        interaction: EntityInteraction::Interact { hand: Hand::Off },
        sneaking: false,
    });
    let interact: UseEntityInteract = decode(&body);
    assert_eq!(interact.mouse, 0);
    assert_eq!(interact.hand, 1, "interact carries the hand on 1.16");

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
    assert_eq!(at.hand, 0);
    assert!((at.y - 0.2).abs() < 1e-6);
}

#[test]
fn player_commands_use_full_1_16_action_set() {
    for (command, action_id) in [
        (PlayerCommand::StopSleeping, 2),
        (PlayerCommand::StartSprinting, 3),
        (PlayerCommand::StopSprinting, 4),
        (PlayerCommand::StopRidingJump, 6),
        (PlayerCommand::OpenInventory, 7),
        (PlayerCommand::StartFallFlying, 8),
    ] {
        let (id, body) = encode(&ClientAction::PlayerCommand {
            entity_id: 5,
            command,
        });
        assert_eq!(id, play::serverbound::ENTITY_ACTION);
        let ea: EntityAction = decode(&body);
        assert_eq!(ea.action_id, action_id, "{command:?}");
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
            components: lodestone_model::ItemComponents::default(),
        }),
    });
    assert!(matches!(err, AdapterError::Unsupported(_)));
}

#[test]
fn actions_absent_from_1_16_fail_loudly() {
    let cases: [ClientAction; 3] = [
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
    let adapter = V735Adapter::new();
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
fn client_settings_encode_1_16_shape() {
    let (id, body) = encode(&ClientAction::SetClientSettings(sample_settings()));
    assert_eq!(id, play::serverbound::SETTINGS);
    // Wire layout per minecraft-data pc/1.16.2 `settings`: string locale, i8 view
    // distance, varint chat flags, bool chat colors, u8 skin parts, varint main
    // hand. Same shape as 1.12 (varint chat flags and main hand).
    assert_eq!(
        body,
        vec![5, b'e', b'n', b'_', b'u', b's', 8, 0, 1, 0b0100_0001, 1]
    );
    let decoded: Settings = decode(&body);
    assert_eq!(decoded.locale, "en_us");
    assert_eq!(decoded.chat_flags, 0);
    assert_eq!(decoded.skin_parts, 0b0100_0001, "cape (bit0) + hat (bit6)");
    assert_eq!(decoded.main_hand, 1, "right hand");
}

#[test]
fn main_hand_and_chat_mode_map_to_wire_values() {
    for (mode, hand, chat, mh) in [
        (ChatMode::Full, MainHand::Left, 0, 0),
        (ChatMode::CommandsOnly, MainHand::Right, 1, 1),
        (ChatMode::Hidden, MainHand::Left, 2, 0),
    ] {
        let mut settings = sample_settings();
        settings.chat_mode = mode;
        settings.main_hand = hand;
        let (_, body) = encode(&ClientAction::SetClientSettings(settings));
        let decoded: Settings = decode(&body);
        assert_eq!(decoded.chat_flags, chat);
        assert_eq!(decoded.main_hand, mh);
    }
}

#[test]
fn send_brand_uses_flattened_channel_on_1_16() {
    let (id, body) = encode(&ClientAction::SendBrand {
        brand: "lodestone".to_owned(),
    });
    assert_eq!(id, play::serverbound::CUSTOM_PAYLOAD);
    let decoded: BrandPayload = decode(&body);
    assert_eq!(
        decoded.channel, "minecraft:brand",
        "1.13 renamed MC|Brand to minecraft:brand"
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
fn set_flying_encodes_flags_only_on_1_16() {
    // 1.16 reduced serverbound abilities to a single flags byte; the two speed
    // floats present in 1.8/1.12 were dropped.
    for (flying, expected_flag) in [(true, 0x02_i8), (false, 0x00_i8)] {
        let (id, body) = encode(&ClientAction::SetFlying { flying });
        assert_eq!(id, play::serverbound::ABILITIES);
        assert_eq!(body.len(), 1, "1.16 abilities is a single flags byte");
        assert_eq!(body[0] as i8, expected_flag);
        let decoded: PlayerAbilities = decode(&body);
        assert_eq!(decoded.flags, expected_flag);
    }
}

#[test]
fn resource_pack_response_encodes_result_only() {
    // 1.16 `resource_pack_receive` is just a result varint — no pack hash — so
    // the four legacy outcomes are encodable from the Uuid-keyed model.
    for (kind, expected) in [
        (ResourcePackResponseKind::SuccessfullyLoaded, 0),
        (ResourcePackResponseKind::Declined, 1),
        (ResourcePackResponseKind::FailedDownload, 2),
        (ResourcePackResponseKind::Accepted, 3),
    ] {
        let (id, body) = encode(&ClientAction::ResourcePackResponse {
            id: uuid::Uuid::from_u128(0),
            response: kind,
        });
        assert_eq!(id, play::serverbound::RESOURCE_PACK_RECEIVE);
        assert_eq!(body, vec![expected]);
        let decoded: ResourcePackReceive = decode(&body);
        assert_eq!(decoded.result, i32::from(expected));
    }
}

#[test]
fn modern_resource_pack_outcomes_fail_loudly_on_1_16() {
    // 1.20.3+ outcomes have no 1.16 result code; they must not be silently
    // mapped onto a wrong legacy value.
    for kind in [
        ResourcePackResponseKind::Downloaded,
        ResourcePackResponseKind::InvalidUrl,
        ResourcePackResponseKind::FailedReload,
        ResourcePackResponseKind::Discarded,
    ] {
        let err = encode_err(&ClientAction::ResourcePackResponse {
            id: uuid::Uuid::from_u128(0),
            response: kind,
        });
        assert!(matches!(err, AdapterError::Unsupported(_)), "{kind:?}");
    }
}

#[test]
fn container_click_fails_loudly_pending_transaction_id_and_registry() {
    let err = encode_err(&ClientAction::ContainerClick {
        window_id: 1,
        state_id: 0,
        slot: 0,
        button: 0,
        click_type: ContainerClickType::Pickup,
        changed_slots: Vec::new(),
        carried_item: None,
    });
    assert!(
        matches!(err, AdapterError::Unsupported(_)),
        "ContainerClick must fail loudly, not silently no-op: {err:?}"
    );
}
