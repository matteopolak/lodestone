//! Hermetic tests for the protocol 776 join flow.
//!
//! Golden clientbound byte vectors were captured from a real Minecraft 26.2
//! server; serverbound expectations are hand-built from the wire specification
//! so a symmetric encode/decode bug cannot pass silently.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    ChatKind, ClientAction, ClientEvent, ConnectionState, Directive, GameMode, LoginProfile,
    ServerAddress, Text, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::{configuration, handshaking, login, play};
use lodestone_v770::packets::common::{ClientInformation, KeepAlive};
use lodestone_v770::packets::configuration::{
    ClientboundKnownPacks, KnownPack, ServerboundKnownPacks,
};
use lodestone_v770::packets::game::{ChatMessage, GameLogin, MessageSignature};
use lodestone_v770::packets::handshake::Intention;
use lodestone_v770::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginCompression, LoginFinished, LoginHello,
};
use lodestone_testsupport::assert_emits_set;
use lodestone_world::World;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    T::decode(&mut reader, CTX).expect("decode")
}

/// A minimal well-formed `registry_data` body with data-less entries. Only
/// enough to prove the join flow accepts and consumes the packet; the real wire
/// format is validated against captured server bytes in `tests/registry_data.rs`.
fn registry_data_payload(registry: &str, entries: &[&str]) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(registry);
    w.var_i32(i32::try_from(entries.len()).expect("entry count"));
    for id in entries {
        w.string(id);
        w.bool(false);
    }
    w.into_vec()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

// Golden clientbound vectors captured from a live 26.2 server.
const LOGIN_COMPRESSION_HEX: &str = "8002";
const LOGIN_FINISHED_HEX: &str =
    "f3d28cb072253cb1baeb2dadd2be89ae0654657374657200e64b80f2005a4ed0a3cbad02f9bfd781";
const KNOWN_PACKS_HEX: &str = "01096d696e65637261667404636f72650432362e32";
const GAME_LOGIN_HEX: &str = "000000010003136d696e6563726166743a6f766572776f726c64116d696e6563726166743a7468655f656e64146d696e6563726166743a7468655f6e65746865720a080a00010000136d696e6563726166743a6f766572776f726c64aacae430d9186e3900ff00010000c1ffffff0f0000";
const PLAY_KEEP_ALIVE_HEX: &str = "00000000002d84b6";

// ---------------------------------------------------------------------------
// Byte-exact serverbound encoding
// ---------------------------------------------------------------------------

#[test]
fn intention_encodes_exact_bytes() {
    let packet = Intention {
        protocol_version: 776,
        host: "127.0.0.1".to_owned(),
        port: 25565,
        next_state: 2,
    };
    let mut expected = vec![0x88, 0x06, 0x09];
    expected.extend_from_slice(b"127.0.0.1");
    expected.extend_from_slice(&[0x63, 0xdd, 0x02]);
    assert_eq!(encode(&packet), expected);
}

#[test]
fn login_hello_encodes_exact_bytes() {
    let uuid = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    let packet = LoginHello {
        name: "Tester".to_owned(),
        profile_id: uuid,
    };
    let mut expected = vec![0x06];
    expected.extend_from_slice(b"Tester");
    expected.extend_from_slice(uuid.as_bytes());
    assert_eq!(encode(&packet), expected);
}

#[test]
fn client_information_default_encodes_exact_bytes() {
    let mut expected = vec![0x05];
    expected.extend_from_slice(b"en_us");
    // view_distance=8, chat_visibility=0, chat_colors=true, model=0,
    // main_hand=1, text_filtering=false, allows_listing=false, particles=0.
    expected.extend_from_slice(&[0x08, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00]);
    assert_eq!(encode(&ClientInformation::default()), expected);
}

#[test]
fn empty_known_packs_encodes_single_zero() {
    assert_eq!(
        encode(&ServerboundKnownPacks { packs: Vec::new() }),
        vec![0x00]
    );
}

#[test]
fn keep_alive_encodes_big_endian_i64() {
    assert_eq!(
        encode(&KeepAlive { id: 0x002d_84b6 }),
        vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x2d, 0x84, 0xb6]
    );
}

// ---------------------------------------------------------------------------
// Clientbound round-trip / decode
// ---------------------------------------------------------------------------

#[test]
fn login_compression_decodes_threshold() {
    let packet: LoginCompression = decode(&hex(LOGIN_COMPRESSION_HEX));
    assert_eq!(packet.threshold, 256);
}

#[test]
fn login_finished_decodes_profile() {
    let packet: LoginFinished = decode(&hex(LOGIN_FINISHED_HEX));
    assert_eq!(packet.name, "Tester");
    assert!(packet.properties.is_empty());
}

#[test]
fn known_packs_decodes_single_pack() {
    let packet: ClientboundKnownPacks = decode(&hex(KNOWN_PACKS_HEX));
    assert_eq!(
        packet.packs,
        vec![KnownPack {
            namespace: "minecraft".to_owned(),
            id: "core".to_owned(),
            version: "26.2".to_owned(),
        }]
    );
}

#[test]
fn game_login_decodes_join_fields() {
    let packet: GameLogin = decode(&hex(GAME_LOGIN_HEX));
    assert_eq!(packet.entity_id, 1);
    assert_eq!(packet.dimension, "minecraft:overworld");
    assert_eq!(packet.game_type, 0);
    assert_eq!(
        packet.levels,
        vec![
            "minecraft:overworld".to_owned(),
            "minecraft:the_end".to_owned(),
            "minecraft:the_nether".to_owned(),
        ]
    );
}

#[test]
fn encryption_request_round_trips() {
    let packet = EncryptionRequest {
        server_id: "server".to_owned(),
        public_key: vec![1, 2, 3, 4],
        challenge: vec![9, 8, 7],
        should_authenticate: true,
    };
    let bytes = encode(&packet);
    assert_eq!(decode::<EncryptionRequest>(&bytes), packet);
}

// ---------------------------------------------------------------------------
// Adapter choreography
// ---------------------------------------------------------------------------

fn adapter() -> V770Adapter {
    V770Adapter::new()
}

#[test]
fn begin_login_emits_handshake_then_hello() {
    let profile = LoginProfile {
        username: "Tester".to_owned(),
        uuid: Uuid::from_u128(0x11),
    };
    let server = ServerAddress {
        host: "127.0.0.1".to_owned(),
        port: 25565,
    };
    let directives = adapter().begin_login(&profile, &server).unwrap();

    let intention_payload = encode(&Intention {
        protocol_version: 776,
        host: "127.0.0.1".to_owned(),
        port: 25565,
        next_state: 2,
    });
    let hello_payload = encode(&LoginHello {
        name: "Tester".to_owned(),
        profile_id: Uuid::from_u128(0x11),
    });

    assert_eq!(
        directives,
        vec![
            Directive::Send {
                packet_id: handshaking::serverbound::INTENTION,
                payload: intention_payload,
            },
            Directive::SetState(ConnectionState::Login),
            Directive::Send {
                packet_id: login::serverbound::HELLO,
                payload: hello_payload,
            },
        ]
    );
}

#[test]
fn full_login_sequence_produces_expected_directives() {
    let adapter = adapter();

    // Compression.
    assert_eq!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Login,
                login::clientbound::LOGIN_COMPRESSION,
                &hex(LOGIN_COMPRESSION_HEX),
            )
            .unwrap(),
        vec![Directive::SetCompression(256)]
    );

    // Login finished -> ack, switch to configuration, send client information.
    let after_finished = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Login,
            login::clientbound::LOGIN_FINISHED,
            &hex(LOGIN_FINISHED_HEX),
        )
        .unwrap();
    assert_eq!(
        after_finished,
        vec![
            Directive::Send {
                packet_id: login::serverbound::LOGIN_ACKNOWLEDGED,
                payload: Vec::new(),
            },
            Directive::SetState(ConnectionState::Configuration),
            Directive::Send {
                packet_id: configuration::serverbound::CLIENT_INFORMATION,
                payload: encode(&ClientInformation::default()),
            },
        ]
    );

    // Known packs -> reply with empty list.
    assert_eq!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Configuration,
                configuration::clientbound::SELECT_KNOWN_PACKS,
                &hex(KNOWN_PACKS_HEX),
            )
            .unwrap(),
        vec![Directive::Send {
            packet_id: configuration::serverbound::SELECT_KNOWN_PACKS,
            payload: vec![0x00],
        }]
    );

    // Registry data is now *ingested* (issue #288) rather than skipped: it
    // produces no directive — nothing is sent, no event is emitted from
    // Configuration — but it is decoded, folded into the connection's registry
    // store, and checked for trailing bytes. `tests/registry_data.rs` asserts
    // the decode against captured server bytes; here the point is only that the
    // join flow is unchanged by it.
    //
    // This used to assert the opposite ("skipped wholesale") with a payload of
    // `deadbeef`, which is exactly what made #288 invisible: the packet reaching
    // the unhandled fall-through was pinned as intended behaviour.
    assert!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Configuration,
                configuration::clientbound::REGISTRY_DATA,
                &registry_data_payload("minecraft:world_clock", &["minecraft:overworld"]),
            )
            .unwrap()
            .is_empty()
    );

    // …and a malformed one is a decode error rather than a silent skip. The
    // control for the assertion above: without this, "produced no directive"
    // would also be true of the fall-through that ignored the packet entirely.
    assert!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Configuration,
                configuration::clientbound::REGISTRY_DATA,
                &[0xde, 0xad, 0xbe, 0xef],
            )
            .is_err(),
        "a truncated registry_data must fail to decode, not be skipped"
    );

    // An *unhandled* configuration packet is still skipped wholesale.
    assert!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Configuration,
                9999,
                &[0xde, 0xad, 0xbe, 0xef],
            )
            .unwrap()
            .is_empty()
    );

    // Finish configuration -> ack and switch to play.
    assert_eq!(
        adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Configuration,
                configuration::clientbound::FINISH_CONFIGURATION,
                &[],
            )
            .unwrap(),
        vec![
            Directive::Send {
                packet_id: configuration::serverbound::FINISH_CONFIGURATION,
                payload: Vec::new(),
            },
            Directive::SetState(ConnectionState::Play),
        ]
    );

    // Game join -> login event.
    //
    // Five `Emit`s, order **not** asserted: this used to be an exact `vec![…]`
    // whose own comment claimed the order was load-bearing ("a consumer
    // folding them sees the dimension's geometry and the biome colour table
    // *before* the `Login` that makes a session out of them"). That claim was
    // never true of any actual consumer — `crates/lodestone-ecs/src/session.rs`
    // folds `DimensionTypeChanged`/`BiomeVisuals`/`Login` into disjoint
    // resources (`dimension_type.0`, `biome_sky_colors.0`, and
    // `id`/`game_mode`/`dimension`/`alive` respectively), and the fold is
    // commutative — and it is exactly the assertion that reddened `main` for
    // one commit when `ClientEvent::BiomeVisuals` (#96) landed a second Emit
    // next to `DimensionTypeChanged`, changing the sequence a prior exact-vec
    // assertion pinned. `assert_emits_set` asserts the same four events land,
    // with no ordering claim this test cannot actually back up.
    //
    // `dimension_type: None` (issue #288) and `sky_colors`/`temperatures`/
    // `downfall`/`has_precipitation`/`names` all empty (issues #96, #25/#26,
    // and the biome-registry-names follow-up to `eb423ac`) because this flow
    // feeds only a `world_clock` registry above, so neither the
    // dimension-type holder id nor the biome registry resolves. All of them
    // must report as **unresolved** rather than defaulting to the overworld —
    // an empty biome table means "the server sent no biome registry", which
    // the shell renders as its dimension colour, never as a
    // plausible-looking overworld blue.
    assert_emits_set(
        &adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                play::clientbound::LOGIN,
                &hex(GAME_LOGIN_HEX),
            )
            .unwrap(),
        &[
            ClientEvent::DimensionTypeChanged {
                holder_id: 0,
                dimension_type: None,
            },
            ClientEvent::BiomeVisuals {
                sky_colors: Vec::new(),
            },
            ClientEvent::BiomeClimates {
                temperatures: Vec::new(),
                downfall: Vec::new(),
                has_precipitation: Vec::new(),
            },
            ClientEvent::BiomeRegistryNames { names: Vec::new() },
            // Empty because this fixture sends no `registry_data` at all, which is
            // the honest expectation: `entry_names` returns nothing and the
            // consumer's contract is that empty means "fall back", not "this
            // server has no enchantments".
            ClientEvent::EnchantmentRegistryNames { names: Vec::new() },
            ClientEvent::Login {
                entity_id: 1,
                game_mode: GameMode::Survival,
                dimension: "minecraft:overworld".parse().unwrap(),
            },
        ],
    );
}

#[test]
fn configuration_keep_alive_is_answered() {
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            configuration::clientbound::KEEP_ALIVE,
            &hex(PLAY_KEEP_ALIVE_HEX),
        )
        .unwrap();
    assert_eq!(
        directives,
        vec![Directive::Send {
            packet_id: configuration::serverbound::KEEP_ALIVE,
            payload: hex(PLAY_KEEP_ALIVE_HEX),
        }]
    );
}

#[test]
fn code_of_conduct_is_accepted() {
    // A short code-of-conduct string body: length-prefixed "hi".
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            configuration::clientbound::CODE_OF_CONDUCT,
            &[0x02, b'h', b'i'],
        )
        .unwrap();
    assert_eq!(
        directives,
        vec![Directive::Send {
            packet_id: configuration::serverbound::ACCEPT_CODE_OF_CONDUCT,
            payload: Vec::new(),
        }]
    );
}

#[test]
fn play_keep_alive_emits_event() {
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::KEEP_ALIVE,
            &hex(PLAY_KEEP_ALIVE_HEX),
        )
        .unwrap();
    assert_emits_set(
        &directives,
        &[ClientEvent::KeepAlive { id: 0x002d_84b6 }],
    );
}

#[test]
fn configuration_disconnect_decodes_nbt_reason() {
    // Network NBT: TAG_String (0x08) root with value "bye".
    let payload = [0x08, 0x00, 0x03, b'b', b'y', b'e'];
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            configuration::clientbound::DISCONNECT,
            &payload,
        )
        .unwrap();
    assert_eq!(
        directives,
        vec![Directive::Disconnect(Text::literal("bye"))]
    );
}

#[test]
fn unknown_play_packet_is_ignored() {
    // An id **outside the protocol's play clientbound table entirely**, and that
    // is the point. This fixture has now been rewritten twice for the same
    // reason: it named `BUNDLE_DELIMITER` until #299 gave that a real decode arm,
    // then `AWARD_STATS` until #26 gave that one — each time asserting the exact
    // defect the next issue fixed, and each time only noticed because the test
    // went red. Any *real* packet id is a fixture with an expiry date, so this
    // uses one no version of this protocol can ever assign.
    const NOT_A_PACKET: i32 = 0x7FFF;
    const {
        assert!(
            NOT_A_PACKET > play::clientbound::AWARD_STATS,
            "the sentinel must sit above every assigned play clientbound id"
        );
    }
    assert!(
        adapter()
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                NOT_A_PACKET,
                &[]
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn system_chat_emits_chat_event() {
    // Network NBT TAG_String "hello" then overlay=false.
    let payload = [0x08, 0x00, 0x05, b'h', b'e', b'l', b'l', b'o', 0x00];
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SYSTEM_CHAT,
            &payload,
        )
        .unwrap();
    assert_emits_set(
        &directives,
        &[ClientEvent::Chat {
            text: Text::literal("hello"),
            kind: ChatKind::System,
            sender: None,
            ack: None,
        }],
    );
}

#[test]
fn system_chat_overlay_is_game_info() {
    // Network NBT TAG_String "hi" then overlay=true.
    let payload = [0x08, 0x00, 0x02, b'h', b'i', 0x01];
    let directives = adapter()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SYSTEM_CHAT,
            &payload,
        )
        .unwrap();
    assert_emits_set(
        &directives,
        &[ClientEvent::Chat {
            text: Text::literal("hi"),
            kind: ChatKind::GameInfo,
            sender: None,
            ack: None,
        }],
    );
}

#[test]
fn keep_alive_response_encodes_config_packet() {
    let (id, payload) = adapter()
        .encode_action(
            ConnectionState::Configuration,
            &ClientAction::KeepAliveResponse { id: 0x002d_84b6 },
        )
        .unwrap()
        .unwrap();
    assert_eq!(id, configuration::serverbound::KEEP_ALIVE);
    assert_eq!(payload, hex(PLAY_KEEP_ALIVE_HEX));
}

#[test]
fn crate_root_adapter_constructor_matches_new() {
    assert_eq!(
        lodestone_v770::adapter().protocol_version(),
        V770Adapter::new().protocol_version()
    );
}

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

#[test]
fn hello_begins_encryption_passing_through_the_request() {
    // The HELLO branch no longer rejects online-mode; it hands the driver the
    // protocol-shaped crypto inputs as a BeginEncryption directive. The adapter
    // performs no crypto and no I/O — that lives in the driver.
    for should_authenticate in [true, false] {
        let payload = encode(&EncryptionRequest {
            server_id: "srv".to_owned(),
            public_key: vec![1, 2, 3, 4],
            challenge: vec![9, 8, 7],
            should_authenticate,
        });
        let out = adapter()
            .handle_packet(
                &mut World::new(),
                ConnectionState::Login,
                login::clientbound::HELLO,
                &payload,
            )
            .expect("hello is handled, not rejected");
        assert_eq!(
            out,
            vec![Directive::BeginEncryption {
                server_id: "srv".to_owned(),
                public_key: vec![1, 2, 3, 4],
                verify_token: vec![9, 8, 7],
                should_authenticate,
            }],
            "should_authenticate must pass through, not be hard-coded"
        );
    }
}

#[test]
fn build_encryption_response_frames_the_key_packet() {
    // The driver hands back the already-encrypted secret and token; the adapter
    // owns only the version-specific packet id and its two-byte-array framing.
    let secret = [0xAAu8; 128];
    let token = [0xBBu8; 128];
    let directive = adapter()
        .build_encryption_response(&secret, &token)
        .expect("v770 builds the key packet");
    let Directive::Send { packet_id, payload } = directive else {
        panic!("expected a Send directive, got {directive:?}");
    };
    assert_eq!(packet_id, login::serverbound::KEY);
    let expected = encode(&EncryptionResponse {
        shared_secret: secret.to_vec(),
        verify_token: token.to_vec(),
    });
    assert_eq!(payload, expected, "secret then token, each length-prefixed");
    let decoded = decode::<EncryptionResponse>(&payload);
    assert_eq!(decoded.shared_secret, secret);
    assert_eq!(decoded.verify_token, token);
}

#[test]
fn truncated_login_finished_errors_without_panic() {
    let err = adapter().handle_packet(
        &mut World::new(),
        ConnectionState::Login,
        login::clientbound::LOGIN_FINISHED,
        &[0x00, 0x01, 0x02],
    );
    assert!(err.is_err());
}

#[test]
fn truncated_game_login_errors_without_panic() {
    let err = adapter().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::LOGIN,
        &[0x00, 0x00],
    );
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------
// encode_action
// ---------------------------------------------------------------------------

#[test]
fn keep_alive_response_encodes_play_packet() {
    let (id, payload) = adapter()
        .encode_action(
            ConnectionState::Play,
            &ClientAction::KeepAliveResponse { id: 0x002d_84b6 },
        )
        .unwrap()
        .unwrap();
    assert_eq!(id, play::serverbound::KEEP_ALIVE);
    assert_eq!(payload, hex(PLAY_KEEP_ALIVE_HEX));
}

#[test]
fn send_command_encodes_chat_command() {
    let (id, payload) = adapter()
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendCommand {
                command: "help".to_owned(),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(id, play::serverbound::CHAT_COMMAND);
    let mut expected = vec![0x04];
    expected.extend_from_slice(b"help");
    assert_eq!(payload, expected);
}

#[test]
fn send_chat_encodes_unsigned_message() {
    let (id, payload) = adapter()
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendChat {
                text: "hi".to_owned(),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(id, play::serverbound::CHAT);
    let mut expected = vec![0x02];
    expected.extend_from_slice(b"hi");
    // timestamp(0), salt(0), no signature, empty last-seen (offset 0, 3-byte
    // fixed bit set, ignore-checksum marker).
    expected.extend_from_slice(&[0; 8]);
    expected.extend_from_slice(&[0; 8]);
    expected.push(0x00);
    expected.push(0x00);
    expected.extend_from_slice(&[0, 0, 0]);
    expected.push(0x00);
    assert_eq!(payload, expected);
}

#[test]
fn unsupported_action_returns_none() {
    // Pin this negative assertion to something *structurally* unsupported by the
    // protocol, not merely unimplemented. `Disconnect` qualifies: vanilla has no
    // serverbound disconnect packet in any state — a client leaves by closing the
    // TCP socket — so this action can never lower to a packet in v770 (or any
    // version). Contrast `SwingArm`, which maps to the real `swing` packet and is
    // just a roadmap item; asserting *that* lowers to `None` would be a tripwire
    // that fires against our own to-do list the moment it's implemented. The
    // positive behaviour for `Respawn` is pinned separately in
    // `death_respawn::encode_action_respawn_targets_client_command`.
    assert_eq!(
        adapter()
            .encode_action(ConnectionState::Play, &ClientAction::Disconnect)
            .unwrap(),
        None
    );
}

#[test]
fn chat_message_round_trips_with_signature() {
    // Exercises the `#[mc(fixed = N)]` path on both the 256-byte signature and
    // the 3-byte acknowledgement, including the present-signature branch.
    let message = ChatMessage {
        message: "hey".to_owned(),
        timestamp: 5,
        salt: 7,
        signature: Some(MessageSignature([0x42; 256])),
        last_seen_offset: 2,
        acknowledged: [1, 2, 3],
        checksum: 9,
    };
    let bytes = encode(&message);
    // Presence flag + 256 signature bytes are written with no length prefix.
    assert_eq!(bytes.iter().filter(|&&b| b == 0x42).count(), 256);
    assert_eq!(decode::<ChatMessage>(&bytes), message);
}
