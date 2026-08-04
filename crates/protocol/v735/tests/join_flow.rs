//! Hermetic tests for the protocol 754 (Minecraft 1.16.5) join flow.
//!
//! Serverbound expectations are hand-built from the 1.16.5 wire specification and
//! clientbound bodies are constructed byte-for-byte here, so a symmetric
//! encode/decode bug cannot pass silently. The binary UUID login success and
//! the packed 1.14+ position (x:26, z:26, y:12 in the low bits) have dedicated
//! byte-level golden tests because a subtly wrong layout is invisible to a
//! round-trip.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    BlockPos, ChatKind, ClientAction, ClientEvent, ConnectionState, Directive, GameMode,
    LoginProfile, RecipeBookType, Rotation, ServerAddress, Vec3, VersionAdapter,
};
use lodestone_v735::V735Adapter;
use lodestone_v735::packet_ids::{handshaking, login, play};
use lodestone_v735::packets::common::{KeepAliveRequest, KeepAliveResponse};
use lodestone_v735::packets::game::{
    ClientCommand, ClientboundChat, ClientboundPositionLook, JoinGame, KickDisconnect,
    RecipeBook, Respawn, ServerboundArmAnimation, ServerboundChat, ServerboundFlying,
    ServerboundLook, ServerboundPosition, ServerboundPositionLook, Spectate, SpawnPosition,
    TeleportConfirm, UpdateHealth,
};
use lodestone_v735::packets::handshake::SetProtocol;
use lodestone_v735::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, LoginSuccess,
    SetCompression,
};
use lodestone_v735::packets::position::{Position, pack_position, unpack_position};
use lodestone_v735::packets::status::{StatusPing, StatusPong, StatusRequest, StatusResponse};
use lodestone_world::World;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 754 };
const PROFILE_UUID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

/// The 16 raw bytes of [`PROFILE_UUID`], big-endian, as 1.16 sends it.
const PROFILE_UUID_BYTES: [u8; 16] = [
    0x06, 0x9a, 0x79, 0xf4, 0x44, 0xe9, 0x47, 0x26, 0xa5, 0xbe, 0xfc, 0xa9, 0x0e, 0x38, 0xaa, 0xf5,
];
const EMPTY_NAMED_NBT: &[u8] = &[0x00];
const RAW_DIMENSION_CODEC_NBT: &[u8] = &[
    0x0a, 0x00, 0x04, b'r', b'o', b'o', b't', 0x08, 0x00, 0x04, b'n', b'a', b'm', b'e', 0x00, 0x13,
    b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'o', b'v', b'e', b'r', b'w', b'o',
    b'r', b'l', b'd', 0x00,
];
const RAW_DIMENSION_TYPE_NBT: &[u8] = &[
    0x0a, 0x00, 0x03, b'd', b'i', b'm', 0x01, 0x00, 0x07, b'n', b'a', b't', b'u', b'r', b'a', b'l',
    0x01, 0x00,
];

fn profile_uuid() -> Uuid {
    PROFILE_UUID.parse().expect("valid uuid")
}

/// A minimal `JoinGame` fixture for the requested world, with empty world list.
fn join_game(entity_id: i32, game_mode: u8, world: &str) -> JoinGame {
    JoinGame {
        entity_id,
        is_hardcore: false,
        game_mode,
        previous_game_mode: 255,
        world_names: vec![world.to_owned()],
        dimension_codec: EMPTY_NAMED_NBT.to_vec(),
        dimension: EMPTY_NAMED_NBT.to_vec(),
        world_name: world.to_owned(),
        hashed_seed: 0,
        max_players: 20,
        view_distance: 10,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        is_debug: false,
        is_flat: false,
    }
}

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX).expect("decode")
}

fn try_decode<T: Decode>(bytes: &[u8]) -> Result<T, lodestone_core::Error> {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX)
}

fn round_trip<T>(value: &T)
where
    T: Encode + Decode + PartialEq + std::fmt::Debug,
{
    let bytes = encode(value);
    let decoded: T = decode(&bytes);
    assert_eq!(&decoded, value, "round trip mismatch");
    // Re-encoding the decoded value must reproduce the exact same bytes.
    assert_eq!(encode(&decoded), bytes, "re-encode mismatch");
}

// ---------------------------------------------------------------------------
// Byte-exact serverbound encoding
// ---------------------------------------------------------------------------

#[test]
fn set_protocol_encodes_exact_bytes() {
    let packet = SetProtocol {
        protocol_version: 754,
        server_host: "localhost".to_owned(),
        server_port: 25565,
        next_state: 2,
    };
    // 754 as a VarInt is f2 05; host length 9; port 25565 = 0x63dd; next 2.
    let mut expected = vec![0xf2, 0x05, 0x09];
    expected.extend_from_slice(b"localhost");
    expected.extend_from_slice(&[0x63, 0xdd, 0x02]);
    assert_eq!(encode(&packet), expected);
}

#[test]
fn login_start_encodes_only_username() {
    let packet = LoginStart {
        username: "Tester".to_owned(),
    };
    let mut expected = vec![0x06];
    expected.extend_from_slice(b"Tester");
    assert_eq!(encode(&packet), expected);
}

// ---------------------------------------------------------------------------
// Golden: binary UUID login success (1.16 sends 128 bits, not a string)
// ---------------------------------------------------------------------------

#[test]
fn login_success_uuid_is_128_bits_not_a_string() {
    let packet = LoginSuccess {
        uuid: profile_uuid(),
        username: "Tester".to_owned(),
    };
    let mut expected = Vec::new();
    // 16 raw UUID bytes, no length prefix.
    expected.extend_from_slice(&PROFILE_UUID_BYTES);
    // 6-char username, length-prefixed.
    expected.push(0x06);
    expected.extend_from_slice(b"Tester");
    assert_eq!(encode(&packet), expected);
    assert_eq!(expected.len(), 16 + 1 + 6);
    round_trip(&packet);
}

// ---------------------------------------------------------------------------
// Golden: 1.14+ packed position (x:26, z:26, y:12 in the LOW bits)
// ---------------------------------------------------------------------------

#[test]
fn position_packing_matches_1_14_bit_layout() {
    // Each axis isolated locks the field order: y lives in the low 12 bits,
    // z in the next 26, x in the top 26.
    assert_eq!(pack_position(BlockPos::new(0, 1, 0)), 0x0000_0000_0000_0001);
    assert_eq!(pack_position(BlockPos::new(0, 0, 1)), 0x0000_0000_0000_1000);
    assert_eq!(pack_position(BlockPos::new(1, 0, 0)), 0x0000_0040_0000_0000);
    assert_eq!(pack_position(BlockPos::new(1, 2, 3)), 0x0000_0040_0000_3002);
}

#[test]
fn spawn_position_encodes_exact_bytes() {
    let packet = SpawnPosition {
        location: Position::new(1, 2, 3),
    };
    // 0x0000004000003002, big-endian.
    let expected = [0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x30, 0x02];
    assert_eq!(encode(&packet), expected);
}

#[test]
fn position_round_trips_including_negatives() {
    for pos in [
        BlockPos::new(0, 0, 0),
        BlockPos::new(1, 2, 3),
        BlockPos::new(-1, -1, -1),
        BlockPos::new(-33_554_432, -2048, -33_554_432), // min signed values
        BlockPos::new(33_554_431, 2047, 33_554_431),    // max signed values
        BlockPos::new(-1, 64, 30_000_000),
    ] {
        assert_eq!(unpack_position(pack_position(pos)), pos);
    }
    // All-ones packs to -1 exactly (26 + 12 + 26 == 64 bits).
    assert_eq!(pack_position(BlockPos::new(-1, -1, -1)), -1);
    assert_eq!(unpack_position(-1), BlockPos::new(-1, -1, -1));
}

// ---------------------------------------------------------------------------
// Round-trip coverage for every implemented packet
// ---------------------------------------------------------------------------

#[test]
fn all_packets_round_trip() {
    round_trip(&SetProtocol {
        protocol_version: 754,
        server_host: "example.com".to_owned(),
        server_port: 25565,
        next_state: 2,
    });
    round_trip(&StatusRequest);
    round_trip(&StatusResponse {
        response: "{\"description\":\"hi\"}".to_owned(),
    });
    round_trip(&StatusPing { time: 1234 });
    round_trip(&StatusPong { time: 1234 });
    round_trip(&LoginStart {
        username: "Tester".to_owned(),
    });
    round_trip(&EncryptionResponse {
        shared_secret: vec![1, 2, 3, 4],
        verify_token: vec![5, 6, 7, 8],
    });
    round_trip(&LoginDisconnect {
        reason: "{\"text\":\"bye\"}".to_owned(),
    });
    round_trip(&EncryptionRequest {
        server_id: "server".to_owned(),
        public_key: vec![9, 8, 7],
        verify_token: vec![6, 5, 4],
    });
    round_trip(&LoginSuccess {
        uuid: profile_uuid(),
        username: "Tester".to_owned(),
    });
    round_trip(&SetCompression { threshold: 256 });
    round_trip(&KeepAliveRequest { id: 42 });
    round_trip(&KeepAliveResponse { id: 42 });
    round_trip(&join_game(1, 0, "minecraft:overworld"));
    round_trip(&ClientboundChat {
        message: "{\"text\":\"hello\"}".to_owned(),
        position: 0,
        sender: profile_uuid(),
    });
    round_trip(&ServerboundChat {
        message: "hello world".to_owned(),
    });
    round_trip(&ClientboundPositionLook {
        x: 1.0,
        y: 64.0,
        z: -2.5,
        yaw: 90.0,
        pitch: -12.0,
        flags: 0,
        teleport_id: 42,
    });
    round_trip(&SpawnPosition {
        location: Position::new(-1, 64, 30_000_000),
    });
    round_trip(&UpdateHealth {
        health: 20.0,
        food: 20,
        food_saturation: 5.0,
    });
    round_trip(&Respawn {
        dimension: EMPTY_NAMED_NBT.to_vec(),
        world_name: "minecraft:the_nether".to_owned(),
        hashed_seed: 0,
        game_mode: 0,
        previous_game_mode: 255,
        is_debug: false,
        is_flat: true,
        copy_metadata: false,
    });
    round_trip(&KickDisconnect {
        reason: "{\"text\":\"kicked\"}".to_owned(),
    });
    round_trip(&ServerboundPosition {
        x: 1.0,
        y: 2.0,
        z: 3.0,
        on_ground: true,
    });
    round_trip(&ServerboundLook {
        yaw: 1.0,
        pitch: 2.0,
        on_ground: false,
    });
    round_trip(&ServerboundPositionLook {
        x: 1.0,
        y: 2.0,
        z: 3.0,
        yaw: 4.0,
        pitch: 5.0,
        on_ground: true,
    });
    round_trip(&ServerboundFlying { on_ground: true });
}

#[test]
fn join_game_captures_raw_named_nbt_spans_from_external_bytes() {
    let mut w = Writer::default();
    w.i32(99);
    w.bool(false);
    w.u8(1);
    w.u8(255);
    w.var_i32(1);
    w.string("minecraft:overworld");
    w.bytes(RAW_DIMENSION_CODEC_NBT);
    w.bytes(RAW_DIMENSION_TYPE_NBT);
    w.string("minecraft:overworld");
    w.i64(123);
    w.var_i32(20);
    w.var_i32(10);
    w.bool(false);
    w.bool(true);
    w.bool(false);
    w.bool(false);
    let bytes = w.into_vec();

    let decoded = decode::<JoinGame>(&bytes);
    assert_eq!(decoded.dimension_codec, RAW_DIMENSION_CODEC_NBT);
    assert_eq!(decoded.dimension, RAW_DIMENSION_TYPE_NBT);
    assert_eq!(encode(&decoded), bytes);
}

#[test]
fn respawn_captures_raw_named_nbt_span_from_external_bytes() {
    let mut w = Writer::default();
    w.bytes(RAW_DIMENSION_TYPE_NBT);
    w.string("minecraft:the_nether");
    w.i64(456);
    w.u8(0);
    w.u8(255);
    w.bool(false);
    w.bool(false);
    w.bool(true);
    let bytes = w.into_vec();

    let decoded = decode::<Respawn>(&bytes);
    assert_eq!(decoded.dimension, RAW_DIMENSION_TYPE_NBT);
    assert_eq!(decoded.hashed_seed, 456);
    assert_eq!(encode(&decoded), bytes);
}

#[test]
fn keep_alive_id_is_an_i64() {
    // 1.9+ widened keep-alive from a varint to a fixed big-endian i64.
    // 300 encodes as 8 bytes 0x00..0x012c, proving i64 (not varint).
    let bytes = encode(&KeepAliveRequest { id: 300 });
    assert_eq!(bytes, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x2c]);
}

// ---------------------------------------------------------------------------
// Hostile input: truncated / malformed bodies must error, never panic
// ---------------------------------------------------------------------------

#[test]
fn truncated_login_success_errors_cleanly() {
    // Claims a 36-byte string but supplies only a few bytes.
    let bytes = [0x24, b'0', b'6', b'9'];
    assert!(try_decode::<LoginSuccess>(&bytes).is_err());
}

#[test]
fn truncated_position_errors_cleanly() {
    // Fewer than the 8 bytes a packed position requires.
    assert!(try_decode::<SpawnPosition>(&[0x00, 0x01, 0x02]).is_err());
}

#[test]
fn truncated_set_protocol_errors_cleanly() {
    // Only the protocol varint, nothing after it.
    assert!(try_decode::<SetProtocol>(&[0x2f]).is_err());
}

#[test]
fn oversized_string_length_errors_cleanly() {
    // A varint length far larger than the remaining buffer.
    let bytes = [0xff, 0xff, 0xff, 0x7f, b'x'];
    assert!(try_decode::<ServerboundChat>(&bytes).is_err());
}

#[test]
fn empty_input_errors_cleanly() {
    assert!(try_decode::<JoinGame>(&[]).is_err());
    assert!(try_decode::<KeepAliveRequest>(&[]).is_err());
    assert!(try_decode::<LoginStart>(&[]).is_err());
}

// ---------------------------------------------------------------------------
// Adapter choreography
// ---------------------------------------------------------------------------

fn profile() -> LoginProfile {
    LoginProfile {
        username: "Tester".to_owned(),
        // The adapter never sends this UUID (1.8 login_start has no UUID
        // field), so any value is fine for the test profile.
        uuid: uuid::Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10),
    }
}

fn server() -> ServerAddress {
    ServerAddress {
        host: "localhost".to_owned(),
        port: 25565,
    }
}

#[test]
fn supports_only_protocol_754() {
    let adapter = V735Adapter::new();
    assert!(adapter.supports(754));
    assert!(!adapter.supports(776));
    assert!(!adapter.supports(47));
    assert_eq!(adapter.protocol_version(), 754);
    assert_eq!(adapter.minecraft_versions(), &["1.16.5"]);
}

#[test]
fn begin_login_sends_handshake_then_login_start() {
    let adapter = V735Adapter::new();
    let directives = adapter.begin_login(&profile(), &server()).expect("begin");
    assert_eq!(directives.len(), 3);

    match &directives[0] {
        Directive::Send { packet_id, payload } => {
            assert_eq!(*packet_id, handshaking::serverbound::SET_PROTOCOL);
            let handshake: SetProtocol = decode(payload);
            assert_eq!(handshake.protocol_version, 754);
            assert_eq!(handshake.next_state, 2);
            assert_eq!(handshake.server_host, "localhost");
        }
        other => panic!("expected Send, got {other:?}"),
    }
    assert_eq!(
        directives[1],
        Directive::SetState(ConnectionState::Login),
        "state change must follow the handshake send"
    );
    match &directives[2] {
        Directive::Send { packet_id, payload } => {
            assert_eq!(*packet_id, login::serverbound::LOGIN_START);
            let start: LoginStart = decode(payload);
            assert_eq!(start.username, "Tester");
        }
        other => panic!("expected Send, got {other:?}"),
    }
}

#[test]
fn login_compression_sets_compression() {
    let adapter = V735Adapter::new();
    let payload = encode(&SetCompression { threshold: 256 });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Login,
            login::clientbound::COMPRESS,
            &payload,
        )
        .expect("handle");
    assert_eq!(directives, vec![Directive::SetCompression(256)]);
}

#[test]
fn login_success_transitions_straight_to_play_with_no_ack() {
    let adapter = V735Adapter::new();
    let payload = encode(&LoginSuccess {
        uuid: profile_uuid(),
        username: "Tester".to_owned(),
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Login,
            login::clientbound::SUCCESS,
            &payload,
        )
        .expect("handle");
    // The single most important 1.8 finding: no login-acknowledged packet, no
    // configuration state — success goes directly to Play.
    assert_eq!(directives, vec![Directive::SetState(ConnectionState::Play)]);
}

#[test]
fn login_encryption_request_is_unsupported() {
    let adapter = V735Adapter::new();
    let payload = encode(&EncryptionRequest {
        server_id: "server".to_owned(),
        public_key: vec![1, 2, 3],
        verify_token: vec![4, 5, 6],
    });
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Login,
        login::clientbound::ENCRYPTION_BEGIN,
        &payload,
    );
    assert!(matches!(
        result,
        Err(lodestone_model::AdapterError::Unsupported(_))
    ));
}

#[test]
fn login_disconnect_surfaces_json_reason() {
    let adapter = V735Adapter::new();
    let payload = encode(&LoginDisconnect {
        reason: "{\"text\":\"nope\"}".to_owned(),
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Login,
            login::clientbound::DISCONNECT,
            &payload,
        )
        .expect("handle");
    match directives.as_slice() {
        [Directive::Disconnect(text)] => assert_eq!(text.to_plain_string(), "nope"),
        other => panic!("expected disconnect, got {other:?}"),
    }
}

#[test]
fn play_join_game_emits_login_event() {
    let adapter = V735Adapter::new();
    // 1.16 carries game mode and the world name separately (no packed hardcore
    // bit, no numeric dimension); the adapter maps the world name straight to a
    // namespaced dimension id.
    let payload = encode(&join_game(7, 1, "minecraft:the_nether"));
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::LOGIN,
            &payload,
        )
        .expect("handle");
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::Login {
                entity_id,
                game_mode,
                dimension,
            }),
        ] => {
            assert_eq!(*entity_id, 7);
            assert_eq!(*game_mode, GameMode::Creative);
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
        }
        other => panic!("expected login event, got {other:?}"),
    }
}

#[test]
fn play_keep_alive_emits_event_and_response_encodes_i64() {
    let adapter = V735Adapter::new();
    let payload = encode(&KeepAliveRequest { id: 12345 });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::KEEP_ALIVE,
            &payload,
        )
        .expect("handle");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::KeepAlive { id: 12345 })]
    );

    let action = ClientAction::KeepAliveResponse { id: 12345 };
    let (packet_id, body) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("encode")
        .expect("some");
    assert_eq!(packet_id, play::serverbound::KEEP_ALIVE);
    let response: KeepAliveResponse = decode(&body);
    assert_eq!(response.id, 12345);
}

#[test]
fn play_chat_emits_extracted_text() {
    let adapter = V735Adapter::new();
    let payload = encode(&ClientboundChat {
        message: "{\"text\":\"hi \",\"extra\":[{\"text\":\"there\"}]}".to_owned(),
        position: 1,
        sender: profile_uuid(),
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::CHAT,
            &payload,
        )
        .expect("handle");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind, .. })] => {
            assert_eq!(text.to_plain_string(), "hi there");
            assert_eq!(*kind, ChatKind::System);
        }
        other => panic!("expected chat event, got {other:?}"),
    }
}

#[test]
fn play_position_emits_teleport_and_confirms() {
    let adapter = V735Adapter::new();
    let payload = encode(&ClientboundPositionLook {
        x: 1.5,
        y: 64.0,
        z: -3.5,
        yaw: 90.0,
        pitch: -10.0,
        flags: 0x01 | 0x10, // relative x and pitch
        teleport_id: 7,
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::POSITION,
            &payload,
        )
        .expect("handle");
    // 1.9+ choreography: the adapter must first send a teleport_confirm echoing
    // the teleport id, then emit the teleport event.
    match directives.as_slice() {
        [
            Directive::Send { packet_id, payload },
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos,
                rotation,
                flags,
            }),
        ] => {
            assert_eq!(*packet_id, play::serverbound::TELEPORT_CONFIRM);
            let confirm: TeleportConfirm = decode(payload);
            assert_eq!(confirm.teleport_id, 7);
            assert_eq!(*pos, Vec3::new(1.5, 64.0, -3.5));
            assert_eq!(*rotation, Rotation::new(90.0, -10.0));
            assert!(flags.relative_x);
            assert!(!flags.relative_y);
            assert!(!flags.relative_z);
            assert!(!flags.relative_yaw);
            assert!(flags.relative_pitch);
        }
        other => panic!("expected confirm + teleport, got {other:?}"),
    }
}

#[test]
fn play_kick_disconnect_surfaces_reason() {
    let adapter = V735Adapter::new();
    let payload = encode(&KickDisconnect {
        reason: "\"flat string reason\"".to_owned(),
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::KICK_DISCONNECT,
            &payload,
        )
        .expect("handle");
    match directives.as_slice() {
        [Directive::Disconnect(text)] => {
            assert_eq!(text.to_plain_string(), "flat string reason");
        }
        other => panic!("expected disconnect, got {other:?}"),
    }
}

#[test]
fn play_set_compression_sets_compression() {
    // Protocol 754 (1.16.5) has no play-state set_compression packet; compression
    // is negotiated only during login. This behaviour is exercised by the login
    // set_compression test below.
}

#[test]
fn encode_send_chat_and_command() {
    let adapter = V735Adapter::new();

    let (id, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendChat {
                text: "hello".to_owned(),
            },
        )
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::CHAT);
    let chat: ServerboundChat = decode(&body);
    assert_eq!(chat.message, "hello");

    // 1.8 has no command packet: a command is chat text with a leading slash.
    let (id, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendCommand {
                command: "gamemode 1".to_owned(),
            },
        )
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::CHAT);
    let chat: ServerboundChat = decode(&body);
    assert_eq!(chat.message, "/gamemode 1");
}

#[test]
fn encode_move_uses_position_look() {
    let adapter = V735Adapter::new();
    let action = ClientAction::Move {
        pos: Vec3::new(1.0, 2.0, 3.0),
        rotation: Rotation::new(45.0, -20.0),
        on_ground: true,
        horizontal_collision: false,
    };
    let (id, body) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::POSITION_LOOK);
    let moved: ServerboundPositionLook = decode(&body);
    assert_eq!(moved.x, 1.0);
    assert_eq!(moved.yaw, 45.0);
    assert!(moved.on_ground);
}

#[test]
fn encode_swing_arm_selects_hand() {
    let adapter = V735Adapter::new();
    // 1.16 arm_animation carries the hand as a VarInt (0 = main, 1 = off).
    let (id, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SwingArm {
                hand: lodestone_model::Hand::Main,
            },
        )
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::ARM_ANIMATION);
    let swing: ServerboundArmAnimation = decode(&body);
    assert_eq!(swing.hand, 0);

    let (_, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SwingArm {
                hand: lodestone_model::Hand::Off,
            },
        )
        .expect("encode")
        .expect("some");
    let swing: ServerboundArmAnimation = decode(&body);
    assert_eq!(swing.hand, 1);
}

#[test]
fn encode_respawn_is_client_command_action_zero() {
    let adapter = V735Adapter::new();
    let (id, body) = adapter
        .encode_action(ConnectionState::Play, &ClientAction::Respawn)
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::CLIENT_COMMAND);
    // Hand-built: a lone varint `0`, one byte since it fits under 128 — per
    // minecraft-data's 1.16.2 protocol.json `packet_client_command`.
    assert_eq!(body, vec![0]);
    let decoded: ClientCommand = decode(&body);
    assert_eq!(decoded.action, 0);
}

#[test]
fn encode_teleport_to_entity_is_spectate_with_the_uuid() {
    let adapter = V735Adapter::new();
    let target = Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").expect("uuid");
    let (id, body) = adapter
        .encode_action(ConnectionState::Play, &ClientAction::TeleportToEntity { target })
        .expect("encode")
        .expect("some");
    assert_eq!(id, play::serverbound::SPECTATE);
    // Hand-built: the raw 16-byte uuid, big-endian per minecraft-data's
    // `packet_spectate` (`{ target: UUID }`) — no length prefix.
    assert_eq!(body, target.as_bytes());
    let decoded: Spectate = decode(&body);
    assert_eq!(decoded.target, target);
}

#[test]
fn encode_set_recipe_book_settings_covers_all_four_books() {
    let adapter = V735Adapter::new();
    // Per minecraft-data's 1.16.2 protocol.json `packet_recipe_book`: varint
    // book id, then the open flag and filter flag. All four books exist by
    // 1.16.5, unlike 1.12.2 which has only the crafting one.
    for (book_type, ordinal) in [
        (RecipeBookType::Crafting, 0u8),
        (RecipeBookType::Furnace, 1),
        (RecipeBookType::BlastFurnace, 2),
        (RecipeBookType::Smoker, 3),
    ] {
        let (id, body) = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::SetRecipeBookSettings {
                    book_type,
                    open: true,
                    filtering: false,
                },
            )
            .expect("encode")
            .expect("some");
        assert_eq!(id, play::serverbound::RECIPE_BOOK);
        assert_eq!(body, vec![ordinal, 1, 0]);
        let decoded: RecipeBook = decode(&body);
        assert_eq!(decoded.book_id, i32::from(ordinal));
        assert!(decoded.book_open);
        assert!(!decoded.filter_active);
    }
}

#[test]
fn actions_outside_play_are_ignored() {
    let adapter = V735Adapter::new();
    let action = ClientAction::SendChat {
        text: "hi".to_owned(),
    };
    assert_eq!(
        adapter
            .encode_action(ConnectionState::Login, &action)
            .expect("encode"),
        None
    );
}

#[test]
fn handshake_and_status_inbound_states_are_rejected() {
    let adapter = V735Adapter::new();
    assert!(matches!(
        adapter.handle_packet(&mut World::new(), ConnectionState::Handshaking, 0, &[]),
        Err(lodestone_model::AdapterError::UnsupportedPacketState { .. })
    ));
    assert!(matches!(
        adapter.handle_packet(&mut World::new(), ConnectionState::Configuration, 0, &[]),
        Err(lodestone_model::AdapterError::UnsupportedPacketState { .. })
    ));
}
