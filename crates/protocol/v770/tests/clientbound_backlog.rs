//! Hermetic tests for a batch of previously-undispatched protocol 776
//! clientbound packets: `player_combat_enter`, `player_combat_end`,
//! `open_sign_editor`, `select_advancements_tab`, `projectile_power`,
//! `mount_screen_open`, `game_rule_values`, `transfer`, `cookie_request`,
//! `store_cookie`, `resource_pack_push`, `resource_pack_pop`,
//! `custom_payload`, `server_data`, `pong_response`, `delete_chat`, and
//! `player_look_at`.
//!
//! Golden byte vectors are hand-built from the wire specification (26.2
//! decompiled Mojang source), not round-tripped through this crate's own
//! encoder, so a self-consistent misreading cannot pass silently. Every test
//! asserts zero trailing bytes via `ensure_empty`.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LookAnchor, PackedMessageSignature,
    PlayerLookAtEntity, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use uuid::Uuid;

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

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

/// Independently packs block coordinates the way vanilla `BlockPos.asLong`
/// does: `x` in the high 26 bits, `z` in the middle 26 bits, `y` in the low 12.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    let x = (i64::from(x)) & 0x3FF_FFFF;
    let y = (i64::from(y)) & 0xFFF;
    let z = (i64::from(z)) & 0x3FF_FFFF;
    (x << 38) | (z << 12) | y
}

/// Network-NBT bare string component: `TAG_String` tag, big-endian u16
/// length, then the UTF-8 bytes.
fn nbt_string(text: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

fn utf(s: &str) -> Vec<u8> {
    let mut out = var_i32(s.len() as i32);
    out.extend_from_slice(s.as_bytes());
    out
}

fn uuid_bytes(uuid: Uuid) -> Vec<u8> {
    let (most, least) = uuid.as_u64_pair();
    let mut out = most.to_be_bytes().to_vec();
    out.extend_from_slice(&least.to_be_bytes());
    out
}

// ---- player_combat_enter ------------------------------------------------

#[test]
fn player_combat_enter_emits_with_empty_payload() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::PLAYER_COMBAT_ENTER, &[]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PlayerCombatEntered)]
    );
}

#[test]
fn player_combat_enter_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::PLAYER_COMBAT_ENTER, &[0x00]);
}

// ---- player_combat_end ---------------------------------------------------

#[test]
fn player_combat_end_decodes_duration() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::PLAYER_COMBAT_END,
        &var_i32(240),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PlayerCombatEnded {
            duration_ticks: 240
        })]
    );
}

#[test]
fn player_combat_end_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::PLAYER_COMBAT_END, &[]);
}

// ---- open_sign_editor -----------------------------------------------------

#[test]
fn open_sign_editor_decodes_pos_and_front_flag() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(10, 65, -20).to_be_bytes().to_vec();
    payload.push(1);
    let directives = handle(&adapter, play::clientbound::OPEN_SIGN_EDITOR, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::SignEditorOpened { pos, is_front_text })] => {
            assert_eq!((pos.x, pos.y, pos.z), (10, 65, -20));
            assert!(*is_front_text);
        }
        other => panic!("expected a single SignEditorOpened event, got {other:?}"),
    }
}

#[test]
fn open_sign_editor_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::OPEN_SIGN_EDITOR, &payload);
}

// ---- select_advancements_tab ----------------------------------------------

#[test]
fn select_advancements_tab_decodes_present_tab() {
    let adapter = V770Adapter::new();
    let mut payload = vec![1u8];
    payload.extend(utf("minecraft:story/root"));
    let directives = handle(
        &adapter,
        play::clientbound::SELECT_ADVANCEMENTS_TAB,
        &payload,
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::AdvancementsTabSelected { tab: Some(tab) })] => {
            assert_eq!(tab.to_string(), "minecraft:story/root");
        }
        other => panic!("expected a single AdvancementsTabSelected event, got {other:?}"),
    }
}

#[test]
fn select_advancements_tab_decodes_absent_tab() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::SELECT_ADVANCEMENTS_TAB, &[0]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
            tab: None
        })]
    );
}

#[test]
fn select_advancements_tab_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(
        &adapter,
        play::clientbound::SELECT_ADVANCEMENTS_TAB,
        &[0, 0xFF],
    );
}

// ---- projectile_power ------------------------------------------------------

#[test]
fn projectile_power_decodes_id_and_power() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(99);
    payload.extend_from_slice(&1.5f64.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::PROJECTILE_POWER, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ProjectilePowerChanged {
            entity_id: 99,
            acceleration_power: 1.5,
        })]
    );
}

#[test]
fn projectile_power_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::PROJECTILE_POWER, &var_i32(1));
}

// ---- mount_screen_open ------------------------------------------------------

#[test]
fn mount_screen_open_decodes_all_fields() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(3);
    payload.extend(var_i32(5));
    payload.extend_from_slice(&77i32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::MOUNT_SCREEN_OPEN, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::MountScreenOpened {
            container_id: 3,
            inventory_columns: 5,
            entity_id: 77,
        })]
    );
}

#[test]
fn mount_screen_open_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(3);
    payload.extend(var_i32(5));
    payload.extend_from_slice(&77i32.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::MOUNT_SCREEN_OPEN, &payload);
}

// ---- game_rule_values -------------------------------------------------------

#[test]
fn game_rule_values_decodes_pairs() {
    // 26.2 renamed game rules to snake_case registry keys (e.g.
    // `minecraft:advance_time`, `minecraft:random_tick_speed`), unlike the
    // legacy camelCase command names (`doDaylightCycle`); confirmed against
    // `GameRules.java`'s `registerBoolean`/`registerInteger` call sites.
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2);
    payload.extend(utf("minecraft:advance_time"));
    payload.extend(utf("false"));
    payload.extend(utf("minecraft:random_tick_speed"));
    payload.extend(utf("3"));
    let directives = handle(&adapter, play::clientbound::GAME_RULE_VALUES, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::GameRulesChanged { values })] => {
            assert_eq!(values.len(), 2);
            assert_eq!(values[0].0.to_string(), "minecraft:advance_time");
            assert_eq!(values[0].1, "false");
            assert_eq!(values[1].0.to_string(), "minecraft:random_tick_speed");
            assert_eq!(values[1].1, "3");
        }
        other => panic!("expected a single GameRulesChanged event, got {other:?}"),
    }
}

#[test]
fn game_rule_values_decodes_empty_map() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::GAME_RULE_VALUES, &var_i32(0));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::GameRulesChanged {
            values: vec![]
        })]
    );
}

#[test]
fn game_rule_values_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend(utf("minecraft:advance_time"));
    // missing value string
    expect_err(&adapter, play::clientbound::GAME_RULE_VALUES, &payload);
}

// ---- transfer ---------------------------------------------------------------

#[test]
fn transfer_decodes_host_and_port() {
    let adapter = V770Adapter::new();
    let mut payload = utf("play.example.com");
    payload.extend(var_i32(25566));
    let directives = handle(&adapter, play::clientbound::TRANSFER, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TransferRequested {
            host: "play.example.com".to_owned(),
            port: 25566,
        })]
    );
}

#[test]
fn transfer_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = utf("host");
    payload.extend(var_i32(1));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::TRANSFER, &payload);
}

// ---- cookie_request -----------------------------------------------------

#[test]
fn cookie_request_decodes_key() {
    let adapter = V770Adapter::new();
    let payload = utf("example:settings");
    let directives = handle(&adapter, play::clientbound::COOKIE_REQUEST, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::CookieRequested { key })] => {
            assert_eq!(key.to_string(), "example:settings");
        }
        other => panic!("expected a single CookieRequested event, got {other:?}"),
    }
}

#[test]
fn cookie_request_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = utf("example:settings");
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::COOKIE_REQUEST, &payload);
}

// ---- store_cookie ---------------------------------------------------------

#[test]
fn store_cookie_decodes_key_and_payload() {
    let adapter = V770Adapter::new();
    let mut payload = utf("example:settings");
    payload.extend(var_i32(3));
    payload.extend_from_slice(&[1, 2, 3]);
    let directives = handle(&adapter, play::clientbound::STORE_COOKIE, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::CookieStored {
            key,
            payload: bytes,
        })] => {
            assert_eq!(key.to_string(), "example:settings");
            assert_eq!(bytes, &[1, 2, 3]);
        }
        other => panic!("expected a single CookieStored event, got {other:?}"),
    }
}

#[test]
fn store_cookie_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = utf("example:settings");
    payload.extend(var_i32(3));
    payload.extend_from_slice(&[1, 2]); // missing one byte
    expect_err(&adapter, play::clientbound::STORE_COOKIE, &payload);
}

// ---- resource_pack_push ----------------------------------------------------

#[test]
fn resource_pack_push_decodes_without_prompt() {
    let adapter = V770Adapter::new();
    let id = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
    let mut payload = uuid_bytes(id);
    payload.extend(utf("https://example.com/pack.zip"));
    payload.extend(utf("0123456789abcdef0123456789abcdef01234567"));
    payload.push(1); // required
    payload.push(0); // no prompt
    let directives = handle(&adapter, play::clientbound::RESOURCE_PACK_PUSH, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ResourcePackPushed {
            id: got_id,
            url,
            hash,
            required,
            prompt,
        })] => {
            assert_eq!(*got_id, id);
            assert_eq!(url, "https://example.com/pack.zip");
            assert_eq!(hash, "0123456789abcdef0123456789abcdef01234567");
            assert!(*required);
            assert!(prompt.is_none());
        }
        other => panic!("expected a single ResourcePackPushed event, got {other:?}"),
    }
}

#[test]
fn resource_pack_push_decodes_with_prompt() {
    let adapter = V770Adapter::new();
    let id = Uuid::nil();
    let mut payload = uuid_bytes(id);
    payload.extend(utf("https://example.com/pack.zip"));
    payload.extend(utf(""));
    payload.push(0); // not required
    payload.push(1); // prompt present
    payload.extend(nbt_string("Accept this pack?"));
    let directives = handle(&adapter, play::clientbound::RESOURCE_PACK_PUSH, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ResourcePackPushed {
            required, prompt, ..
        })] => {
            assert!(!*required);
            assert_eq!(
                prompt.as_ref().map(|t| t.to_plain_string()),
                Some("Accept this pack?".to_owned())
            );
        }
        other => panic!("expected a single ResourcePackPushed event, got {other:?}"),
    }
}

#[test]
fn resource_pack_push_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let payload = uuid_bytes(Uuid::nil());
    expect_err(&adapter, play::clientbound::RESOURCE_PACK_PUSH, &payload);
}

// ---- resource_pack_pop ------------------------------------------------------

#[test]
fn resource_pack_pop_decodes_present_id() {
    let adapter = V770Adapter::new();
    let id = Uuid::from_u128(0xdead_beef);
    let mut payload = vec![1u8];
    payload.extend(uuid_bytes(id));
    let directives = handle(&adapter, play::clientbound::RESOURCE_PACK_POP, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ResourcePackPopped {
            id: Some(id)
        })]
    );
}

#[test]
fn resource_pack_pop_decodes_absent_id() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::RESOURCE_PACK_POP, &[0]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ResourcePackPopped { id: None })]
    );
}

#[test]
fn resource_pack_pop_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::RESOURCE_PACK_POP, &[0, 0xFF]);
}

// ---- custom_payload ---------------------------------------------------------

#[test]
fn custom_payload_carries_channel_and_raw_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = utf("minecraft:brand");
    payload.extend(utf("vanilla"));
    let directives = handle(&adapter, play::clientbound::CUSTOM_PAYLOAD, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::CustomPayload { channel, data })] => {
            assert_eq!(channel.to_string(), "minecraft:brand");
            assert_eq!(data, &utf("vanilla"));
        }
        other => panic!("expected a single CustomPayload event, got {other:?}"),
    }
}

#[test]
fn custom_payload_handles_empty_body() {
    let adapter = V770Adapter::new();
    let payload = utf("example:empty");
    let directives = handle(&adapter, play::clientbound::CUSTOM_PAYLOAD, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::CustomPayload { channel, data })] => {
            assert_eq!(channel.to_string(), "example:empty");
            assert!(data.is_empty());
        }
        other => panic!("expected a single CustomPayload event, got {other:?}"),
    }
}

// ---- server_data ---------------------------------------------------------

#[test]
fn server_data_decodes_motd_without_icon() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("A Minecraft Server");
    payload.push(0); // no icon
    let directives = handle(&adapter, play::clientbound::SERVER_DATA, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ServerDataReceived { motd, icon })] => {
            assert_eq!(motd.to_plain_string(), "A Minecraft Server");
            assert!(icon.is_none());
        }
        other => panic!("expected a single ServerDataReceived event, got {other:?}"),
    }
}

#[test]
fn server_data_decodes_motd_with_icon() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("A Minecraft Server");
    payload.push(1); // icon present
    payload.extend(var_i32(4));
    payload.extend_from_slice(&[0x89, 0x50, 0x4e, 0x47]);
    let directives = handle(&adapter, play::clientbound::SERVER_DATA, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ServerDataReceived { icon, .. })] => {
            assert_eq!(icon.as_deref(), Some(&[0x89, 0x50, 0x4e, 0x47][..]));
        }
        other => panic!("expected a single ServerDataReceived event, got {other:?}"),
    }
}

#[test]
fn server_data_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("hi");
    payload.push(0);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SERVER_DATA, &payload);
}

// ---- pong_response (play state) --------------------------------------------

#[test]
fn play_pong_response_decodes_time() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::PONG_RESPONSE,
        &123_456_789i64.to_be_bytes(),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PongReceived {
            time: 123_456_789
        })]
    );
}

#[test]
fn play_pong_response_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::PONG_RESPONSE, &[0; 4]);
}

// ---- delete_chat ------------------------------------------------------------

#[test]
fn delete_chat_decodes_full_signature() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(0);
    let signature_bytes = vec![7u8; 256];
    payload.extend_from_slice(&signature_bytes);
    let directives = handle(&adapter, play::clientbound::DELETE_CHAT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChatMessageDeleted {
            signature: PackedMessageSignature::Full(signature_bytes),
        })]
    );
}

#[test]
fn delete_chat_decodes_cached_index() {
    let adapter = V770Adapter::new();
    let payload = var_i32(6); // id + 1 == 6 -> cached index 5
    let directives = handle(&adapter, play::clientbound::DELETE_CHAT, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChatMessageDeleted {
            signature: PackedMessageSignature::Cached(5),
        })]
    );
}

#[test]
fn delete_chat_rejects_truncated_full_signature() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(0);
    payload.extend_from_slice(&[0u8; 255]); // one byte short
    expect_err(&adapter, play::clientbound::DELETE_CHAT, &payload);
}

// ---- player_look_at ---------------------------------------------------------

#[test]
fn player_look_at_decodes_fixed_position() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(0); // FEET
    payload.extend_from_slice(&1.0f64.to_be_bytes());
    payload.extend_from_slice(&2.0f64.to_be_bytes());
    payload.extend_from_slice(&3.0f64.to_be_bytes());
    payload.push(0); // not at entity
    let directives = handle(&adapter, play::clientbound::PLAYER_LOOK_AT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::PlayerLookAt {
            from_anchor,
            target,
            at_entity,
        })] => {
            assert_eq!(*from_anchor, LookAnchor::Feet);
            assert_eq!((target.x, target.y, target.z), (1.0, 2.0, 3.0));
            assert!(at_entity.is_none());
        }
        other => panic!("expected a single PlayerLookAt event, got {other:?}"),
    }
}

#[test]
fn player_look_at_decodes_entity_target() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1); // EYES
    payload.extend_from_slice(&10.0f64.to_be_bytes());
    payload.extend_from_slice(&65.0f64.to_be_bytes());
    payload.extend_from_slice(&(-5.0f64).to_be_bytes());
    payload.push(1); // at entity
    payload.extend(var_i32(42));
    payload.extend(var_i32(1)); // EYES
    let directives = handle(&adapter, play::clientbound::PLAYER_LOOK_AT, &payload);
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::PlayerLookAt {
            from_anchor,
            at_entity,
            ..
        })] => {
            assert_eq!(*from_anchor, LookAnchor::Eyes);
            assert_eq!(
                *at_entity,
                Some(PlayerLookAtEntity {
                    entity_id: 42,
                    to_anchor: LookAnchor::Eyes,
                })
            );
        }
        other => panic!("expected a single PlayerLookAt event, got {other:?}"),
    }
}

#[test]
fn player_look_at_rejects_invalid_anchor_ordinal() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2); // invalid anchor ordinal
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.push(0);
    expect_err(&adapter, play::clientbound::PLAYER_LOOK_AT, &payload);
}

#[test]
fn player_look_at_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(0);
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    // missing y, z, at_entity flag
    expect_err(&adapter, play::clientbound::PLAYER_LOOK_AT, &payload);
}
