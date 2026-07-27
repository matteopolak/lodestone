//! Hermetic tests for the protocol 47 (Minecraft 1.8.x) join flow.
//!
//! Serverbound expectations are hand-built from the 1.8 wire specification and
//! clientbound bodies are constructed byte-for-byte here, so a symmetric
//! encode/decode bug cannot pass silently. The string-UUID login success and
//! the packed 1.8 position have dedicated byte-level golden tests because a
//! subtly wrong layout is invisible to a round-trip.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    BlockPos, ChatKind, ClientAction, ClientEvent, ConnectionState, Directive, GameMode,
    LoginProfile, Rotation, ServerAddress, Vec3, VersionAdapter,
};
use lodestone_v47::V47Adapter;
use lodestone_v47::packet_ids::{handshaking, login, play};
use lodestone_v47::packets::common::{KeepAliveRequest, KeepAliveResponse};
use lodestone_v47::packets::game::{
    ClientboundChat, ClientboundPositionLook, JoinGame, KickDisconnect, PlaySetCompression,
    Respawn, ServerboundChat, ServerboundFlying, ServerboundLook, ServerboundPosition,
    ServerboundPositionLook, SpawnPosition, UpdateHealth,
};
use lodestone_v47::packets::handshake::SetProtocol;
use lodestone_v47::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, LoginSuccess,
    SetCompression,
};
use lodestone_v47::packets::position::{Position, pack_position, unpack_position};
use lodestone_v47::packets::status::{StatusPing, StatusPong, StatusRequest, StatusResponse};
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 47 };
const PROFILE_UUID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

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
        protocol_version: 47,
        server_host: "localhost".to_owned(),
        server_port: 25565,
        next_state: 2,
    };
    let mut expected = vec![0x2f, 0x09];
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
// Golden: string UUID login success (1.8 differs from modern binary UUID)
// ---------------------------------------------------------------------------

#[test]
fn login_success_uuid_is_a_string_not_128_bits() {
    let packet = LoginSuccess {
        uuid: PROFILE_UUID.to_owned(),
        username: "Tester".to_owned(),
    };
    let mut expected = Vec::new();
    // 36-char dashed UUID string, length-prefixed.
    expected.push(0x24);
    expected.extend_from_slice(PROFILE_UUID.as_bytes());
    // 6-char username, length-prefixed.
    expected.push(0x06);
    expected.extend_from_slice(b"Tester");
    assert_eq!(encode(&packet), expected);
    assert_eq!(expected.len(), 1 + 36 + 1 + 6);
    round_trip(&packet);
}

// ---------------------------------------------------------------------------
// Golden: 1.8 packed position (x:26, y:12 in the MIDDLE, z:26)
// ---------------------------------------------------------------------------

#[test]
fn position_packing_matches_1_8_bit_layout() {
    // Each axis isolated locks the field order: y lives in the middle bits.
    assert_eq!(pack_position(BlockPos::new(0, 0, 1)), 0x0000_0000_0000_0001);
    assert_eq!(pack_position(BlockPos::new(0, 1, 0)), 0x0000_0000_0400_0000);
    assert_eq!(pack_position(BlockPos::new(1, 0, 0)), 0x0000_0040_0000_0000);
    assert_eq!(pack_position(BlockPos::new(1, 2, 3)), 0x0000_0040_0800_0003);
}

#[test]
fn spawn_position_encodes_exact_bytes() {
    let packet = SpawnPosition {
        location: Position::new(1, 2, 3),
    };
    // 0x0000004008000003, big-endian.
    let expected = [0x00, 0x00, 0x00, 0x40, 0x08, 0x00, 0x00, 0x03];
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
        protocol_version: 47,
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
        uuid: PROFILE_UUID.to_owned(),
        username: "Tester".to_owned(),
    });
    round_trip(&SetCompression { threshold: 256 });
    round_trip(&KeepAliveRequest { id: 42 });
    round_trip(&KeepAliveResponse { id: 42 });
    round_trip(&JoinGame {
        entity_id: 1,
        game_mode: 0,
        dimension: 0,
        difficulty: 2,
        max_players: 20,
        level_type: "default".to_owned(),
        reduced_debug_info: false,
    });
    round_trip(&ClientboundChat {
        message: "{\"text\":\"hello\"}".to_owned(),
        position: 0,
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
        dimension: -1,
        difficulty: 2,
        game_mode: 0,
        level_type: "flat".to_owned(),
    });
    round_trip(&KickDisconnect {
        reason: "{\"text\":\"kicked\"}".to_owned(),
    });
    round_trip(&PlaySetCompression { threshold: 128 });
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
fn keep_alive_id_is_a_varint() {
    // 300 encodes as the two-byte varint 0xac 0x02, proving varint (not i32).
    let bytes = encode(&KeepAliveRequest { id: 300 });
    assert_eq!(bytes, [0xac, 0x02]);
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
fn supports_only_protocol_47() {
    let adapter = V47Adapter::new();
    assert!(adapter.supports(47));
    assert!(!adapter.supports(776));
    assert!(!adapter.supports(46));
    assert_eq!(adapter.protocol_version(), 47);
    assert_eq!(adapter.minecraft_versions(), &["1.8.8", "1.8.9"]);
}

#[test]
fn begin_login_sends_handshake_then_login_start() {
    let adapter = V47Adapter::new();
    let directives = adapter.begin_login(&profile(), &server()).expect("begin");
    assert_eq!(directives.len(), 3);

    match &directives[0] {
        Directive::Send { packet_id, payload } => {
            assert_eq!(*packet_id, handshaking::serverbound::SET_PROTOCOL);
            let handshake: SetProtocol = decode(payload);
            assert_eq!(handshake.protocol_version, 47);
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
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
    let payload = encode(&LoginSuccess {
        uuid: PROFILE_UUID.to_owned(),
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
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
    let payload = encode(&JoinGame {
        entity_id: 7,
        game_mode: 0x9, // hardcore | creative
        dimension: -1,
        difficulty: 2,
        max_players: 20,
        level_type: "default".to_owned(),
        reduced_debug_info: false,
    });
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
            // The 0x8 hardcore bit is masked off; the low bits select the mode.
            assert_eq!(*game_mode, GameMode::Creative);
            assert_eq!(dimension.to_string(), "minecraft:the_nether");
        }
        other => panic!("expected login event, got {other:?}"),
    }
}

#[test]
fn play_keep_alive_emits_event_and_response_encodes_varint() {
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
    let payload = encode(&ClientboundChat {
        message: "{\"text\":\"hi \",\"extra\":[{\"text\":\"there\"}]}".to_owned(),
        position: 1,
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
fn play_position_emits_confirmation_then_teleport_with_flags() {
    let adapter = V47Adapter::new();
    let payload = encode(&ClientboundPositionLook {
        x: 1.5,
        y: 64.0,
        z: -3.5,
        yaw: 90.0,
        pitch: -10.0,
        flags: 0x01 | 0x10, // relative x and pitch
    });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::POSITION,
            &payload,
        )
        .expect("handle");
    // 1.8 confirms a teleport by echoing a serverbound `position_look` back;
    // it must be the FIRST directive (before the emit) so the driver sends it
    // while still in Play. There is no teleport-id, unlike 340+.
    match directives.as_slice() {
        [
            Directive::Send {
                packet_id,
                payload: confirm_bytes,
            },
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos,
                rotation,
                flags,
            }),
        ] => {
            assert_eq!(*packet_id, play::serverbound::POSITION_LOOK);
            // The confirmation echoes the received coordinates exactly.
            let confirm: ServerboundPositionLook = decode(confirm_bytes);
            assert_eq!(confirm.x, 1.5);
            assert_eq!(confirm.y, 64.0);
            assert_eq!(confirm.z, -3.5);
            assert_eq!(confirm.yaw, 90.0);
            assert_eq!(confirm.pitch, -10.0);
            assert_eq!(*pos, Vec3::new(1.5, 64.0, -3.5));
            assert_eq!(*rotation, Rotation::new(90.0, -10.0));
            assert!(flags.relative_x);
            assert!(!flags.relative_y);
            assert!(!flags.relative_z);
            assert!(!flags.relative_yaw);
            assert!(flags.relative_pitch);
        }
        other => panic!("expected confirmation + teleport, got {other:?}"),
    }
}

#[test]
fn play_kick_disconnect_surfaces_reason() {
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
    let payload = encode(&PlaySetCompression { threshold: 64 });
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_COMPRESSION,
            &payload,
        )
        .expect("handle");
    assert_eq!(directives, vec![Directive::SetCompression(64)]);
}

#[test]
fn encode_send_chat_and_command() {
    let adapter = V47Adapter::new();

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
    let adapter = V47Adapter::new();
    let action = ClientAction::Move {
        pos: Vec3::new(1.0, 2.0, 3.0),
        rotation: Rotation::new(45.0, -20.0),
        on_ground: true,
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
fn encode_swing_arm_is_the_empty_1_8_packet() {
    let adapter = V47Adapter::new();
    // 1.8's arm_animation is an empty container — the off-hand did not exist,
    // so the `hand` is dropped and the payload carries no bytes.
    for hand in [lodestone_model::Hand::Main, lodestone_model::Hand::Off] {
        let (id, body) = adapter
            .encode_action(ConnectionState::Play, &ClientAction::SwingArm { hand })
            .expect("encode")
            .expect("some");
        assert_eq!(id, play::serverbound::ARM_ANIMATION);
        assert!(
            body.is_empty(),
            "1.8 arm_animation has no body, got {body:?}"
        );
    }
}

#[test]
fn actions_outside_play_are_ignored() {
    let adapter = V47Adapter::new();
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
    let adapter = V47Adapter::new();
    assert!(matches!(
        adapter.handle_packet(&mut World::new(), ConnectionState::Handshaking, 0, &[]),
        Err(lodestone_model::AdapterError::UnsupportedPacketState { .. })
    ));
    assert!(matches!(
        adapter.handle_packet(&mut World::new(), ConnectionState::Configuration, 0, &[]),
        Err(lodestone_model::AdapterError::UnsupportedPacketState { .. })
    ));
}
