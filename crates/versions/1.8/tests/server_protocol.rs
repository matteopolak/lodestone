//! Hosted protocol-47 controls anchored to the existing 1.8 wire fixtures.

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_data::block_states::{self, block_name, properties};
use lodestone_model::{
    AnimationAction, BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent,
    ConnectionState, Directive, Hand, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_8::{V47Adapter, V47ServerProtocol};
use lodestone_v1_8::packet_ids::{handshaking, play};
use lodestone_v1_8::packets::handshake::SetProtocol;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 47 };

#[test]
fn accepts_only_the_hosted_handshake_protocol() {
    let protocol = V47ServerProtocol;
    let request = |protocol_version| {
        encode_body(
            &SetProtocol {
                protocol_version,
                server_host: "localhost".to_owned(),
                server_port: 25565,
                next_state: 2,
            },
            CTX,
        )
        .expect("handshake fixture encodes")
    };

    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(47)),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(46)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());
}

#[test]
fn join_position_chunk_and_block_update_match_protocol_47_layout() {
    let protocol = V47ServerProtocol;
    let join = protocol.begin_play(0);
    let Some(ServerDirective::Send { packet_id, payload }) = join.first() else {
        panic!("join must begin with a packet");
    };
    assert_eq!(*packet_id, 1);
    assert_eq!(
        payload,
        &[0, 0, 0, 1, 0, 0, 2, 20, 7, b'd', b'e', b'f', b'a', b'u', b'l', b't', 0]
    );
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 8, payload }) if payload == &[
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0x40, 0x59, 0, 0, 0, 0, 0, 0,
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(0, 0, 0, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(0, 0, &column)
        .expect("stone has an exact protocol-47 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 33);
    let mut packet = Reader::new(&payload);
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.bool(), Ok(true));
    assert_eq!(packet.u16(), Ok(1));
    let blob_len = usize::try_from(packet.var_i32().expect("chunk data length"))
        .expect("positive length");
    assert_eq!(blob_len, 12_544);
    let mut blob = packet.take_reader(blob_len).expect("chunk data");
    // The committed 1.8 fixture fixes this as a little-endian `(id << 4) | meta`
    // array in YZX order. Stone is therefore the first `0x0010` word.
    assert_eq!(blob.bytes(2), Ok(&[16, 0][..]));
    assert_eq!(blob.bytes(8190), Ok(&[0; 8190][..]));
    assert_eq!(blob.bytes(2048), Ok(&[0; 2048][..]));
    assert_eq!(blob.bytes(2048), Ok(&[u8::MAX; 2048][..]));
    assert_eq!(blob.bytes(256), Ok(&[1; 256][..]));
    assert!(blob.ensure_empty().is_ok());
    assert!(packet.ensure_empty().is_ok());

    assert!(matches!(
        protocol.encode_block_update(1, 64, -1, "minecraft:stone"),
        ServerDirective::Send { packet_id: 35, payload }
            if payload == vec![0, 0, 0, 0x41, 0x03, 0xFF, 0xFF, 0xFF, 16]
    ));
}

#[test]
fn projects_only_the_legacy_vertical_window_and_decodes_break_actions() {
    let protocol = V47ServerProtocol;
    let mut covering = ChunkColumn::new(-64, 384);
    covering.set_block(3, 64, 4, "minecraft:stone");
    assert!(protocol.try_encode_chunk(0, 0, &covering).is_ok());
    assert!(protocol
        .try_encode_chunk(0, 0, &ChunkColumn::new(-64, 319))
        .is_err());

    // Status 0, packed (1, 64, 3), and the Up face are the pre-1.9 break
    // packet's complete body; there is no prediction sequence.
    let payload = [0, 0, 0, 0, 0x41, 0, 0, 0, 3, 1];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &payload),
        ServerBound::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(1, 64, 3),
            face: BlockFace::Up,
            sequence: 0,
        }
    );
}

#[test]
fn legacy_chat_uses_its_single_string_body_for_chat_commands_and_replies() {
    let protocol = V47ServerProtocol;
    // VarInt length 14, then the complete string body. This is deliberately
    // literal rather than encoded by the packet type under test.
    let chat = b"\x0elegacy \"chat\"\n";
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::CHAT, chat),
        ServerBound::Chat {
            message: "legacy \"chat\"\n".to_owned(),
            timestamp_millis: 0,
            salt: 0,
            signature: None,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::CHAT,
            b"\x08/say one",
        ),
        ServerBound::ChatCommand {
            command: "say one".to_owned(),
        },
        "a slash command is carried by the same legacy packet without its slash"
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::CHAT,
            b"\x0elegacy \"chat\"\n\0",
        ),
        ServerBound::Ignored,
        "a chat prefix with a trailing byte must not be accepted as a message"
    );

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_system_chat("legacy \"chat\"\n")
    else {
        panic!("legacy system text must produce a chat packet");
    };
    assert_eq!(packet_id, play::clientbound::CHAT);
    assert_eq!(
        payload,
        b"\x1c{\"text\":\"legacy \\\"chat\\\"\\n\"}\x01",
        "the JSON component is length-prefixed once, followed by position 1"
    );
}

#[test]
fn registry_selected_arm_animation_connects_protocol_47_to_the_shared_swing_broadcast() {
    let protocol = lodestone_registry::server_protocol_for_protocol(47)
        .expect("protocol 47 must resolve to its hosted server protocol");
    let adapter = V47Adapter::new();

    // Protocol 47 has no hand field in its arm-animation request. The
    // adapter's main- and off-hand actions therefore share the same literal
    // empty request body, and the hosted decoder maps both to main hand.
    for hand in [Hand::Main, Hand::Off] {
        let action = ClientAction::SwingArm { hand };
        let Some((packet_id, payload)) = adapter
            .encode_action(ConnectionState::Play, &action)
            .expect("protocol-47 adapter encodes arm swings")
        else {
            panic!("{action:?} must produce a protocol-47 packet");
        };
        assert_eq!(packet_id, play::serverbound::ARM_ANIMATION);
        assert!(payload.is_empty(), "protocol-47 arm animation has no fields");
        assert_eq!(
            protocol.decode(State::Play, packet_id, &[]),
            ServerBound::Swing { hand: 0 },
            "the literal empty body must reach the shared swing consumer"
        );
    }

    // The clientbound animation body is independently assembled: entity id
    // 321 is varint [0xc1, 0x02], followed by the raw action byte.
    for (action, expected) in [
        (0, AnimationAction::SwingMainHand),
        (3, AnimationAction::SwingOffHand),
    ] {
        let ServerDirective::Send { packet_id, payload } =
            protocol.encode_animate(321, action)
        else {
            panic!("the protocol-47 host must encode an animation reply");
        };
        assert_eq!(packet_id, play::clientbound::ANIMATION);
        assert_eq!(payload, vec![0xc1, 0x02, action]);
        assert_eq!(
            adapter
                .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, &payload)
                .expect("the protocol-47 client decodes host animation"),
            vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id: 321,
                action: expected,
            })]
        );
    }

    // Controls distinguish the empty request body from a mistaken hand
    // ordinal, and keep a valid prefix with trailing bytes from being accepted.
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, &[0]),
        ServerBound::Ignored,
        "protocol 47 has no hand ordinal in this packet"
    );
    assert_eq!(
        protocol.decode(State::Login, play::serverbound::ARM_ANIMATION, &[]),
        ServerBound::Ignored,
        "the Play packet must not be accepted before Play"
    );
}

#[test]
fn block_place_lifts_the_protocol_47_body_to_the_shared_placement_consumer() {
    let protocol = V47ServerProtocol;
    // x=258, y=64, z=-3 packed into protocol 47's single i64 position;
    // East face; `0xffff` is the still-present empty inline slot; and cursor
    // sixteenths. There is no hand or prediction sequence in this era.
    let body = [
        0, 0, 0x40, 0x81, 0x03, 0xFF, 0xFF, 0xFD, 5, 0xFF, 0xFF, 4, 8, 12,
    ];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_PLACE, &body),
        ServerBound::UseItemOn {
            pos: BlockPos::new(258, 64, -3),
            face: BlockFace::East,
            cursor: Vec3f::new(0.25, 0.5, 0.75),
            hand: 0,
            sequence: 0,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::BLOCK_PLACE,
            &[0, 0, 0x40, 0x81, 0x03, 0xFF, 0xFF, 0xFD, 6, 0xFF, 0xFF, 4, 8, 12],
        ),
        ServerBound::Ignored,
        "an unknown face must not become a placement against an arbitrary side"
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::BLOCK_PLACE,
            &[0, 0, 0x40, 0x81, 0x03, 0xFF, 0xFF, 0xFD, 5, 0xFF, 0xFF, 16, 8, 12],
        ),
        ServerBound::Ignored,
        "a cursor outside the protocol-47 sixteenth range must not be clamped"
    );
    assert_eq!(
        protocol.decode(
            State::Configuration,
            play::serverbound::BLOCK_PLACE,
            &body,
        ),
        ServerBound::Ignored,
        "the Play packet must not bypass the direct legacy login transition"
    );
}

#[test]
fn adapter_emitted_block_place_reaches_the_hosted_placement_boundary() {
    let action = ClientAction::UseItemOn {
        hand: Hand::Main,
        pos: BlockPos::new(7, 80, -9),
        face: BlockFace::North,
        cursor: Vec3f::new(0.5, 0.25, 0.75),
        inside_block: false,
        sequence: 123,
    };
    let (packet_id, body) = V47Adapter::new()
        .encode_action(ConnectionState::Play, &action)
        .expect("main-hand block use is representable in protocol 47")
        .expect("block use emits one placement frame");
    assert_eq!(packet_id, play::serverbound::BLOCK_PLACE);
    assert_eq!(
        V47ServerProtocol.decode(State::Play, packet_id, &body),
        ServerBound::UseItemOn {
            pos: BlockPos::new(7, 80, -9),
            face: BlockFace::North,
            // The era's cursor is rounded to sixteenths by the adapter.
            cursor: Vec3f::new(0.5, 0.25, 0.6875),
            hand: 0,
            sequence: 0,
        },
        "the real adapter frame must reach the shared server variant consumed by placement"
    );
}

#[test]
fn block_dig_non_breaking_statuses_reach_their_server_consumers() {
    let protocol = V47ServerProtocol;
    let adapter = V47Adapter::new();
    // Independent literal protocol-47 bodies: the status VarInt, the legacy
    // packed zero position, then the signed-byte direction. These actions
    // ignore the position/direction, but those bytes prove this decoder is
    // consuming the protocol-47 block-dig shape rather than a later action
    // packet.
    let body = |status| {
        let mut payload = vec![status];
        payload.extend_from_slice(&[0; 8]);
        payload.push(0);
        payload
    };

    for (status, expected) in [
        (3, ServerBound::ItemDropped { whole_stack: true }),
        (4, ServerBound::ItemDropped { whole_stack: false }),
        (5, ServerBound::ReleaseUseItem),
    ] {
        assert_eq!(
            protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(status)),
            expected,
            "status {status} must survive protocol-47 decoding"
        );
    }

    // The adapter and server own separate ends of the production hand-off.
    // Keep the literals above as the wire-shape oracle, then prove each input
    // action reaches the version-free variant which the integrated server
    // consumes for inventory drop and item-use cancellation/release.
    for (action, expected) in [
        (
            ClientAction::DropSelectedItemStack,
            ServerBound::ItemDropped { whole_stack: true },
        ),
        (
            ClientAction::DropSelectedItem,
            ServerBound::ItemDropped { whole_stack: false },
        ),
        (ClientAction::ReleaseUseItem, ServerBound::ReleaseUseItem),
    ] {
        let Some((packet_id, payload)) = adapter
            .encode_action(ConnectionState::Play, &action)
            .expect("protocol-47 adapter encodes the supported action")
        else {
            panic!("{action:?} must produce a protocol-47 packet");
        };
        assert_eq!(packet_id, play::serverbound::BLOCK_DIG, "{action:?}");
        assert_eq!(
            protocol.decode(State::Play, packet_id, &payload),
            expected,
            "{action:?} must reach its server consumer variant"
        );
    }

    // Controls distinguish ordinary mining from the target actions and prove
    // an adjacent unsupported status cannot select a neighbouring consumer.
    assert!(matches!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(2)),
        ServerBound::BlockAction { .. }
    ));
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(6)),
        ServerBound::Ignored,
        "protocol 47 has no off-hand swap status"
    );
}

#[test]
fn keep_alive_uses_the_protocol_47_varint_body_in_both_directions() {
    let protocol = V47ServerProtocol;
    // 0x1234 as a VarInt. The literal bytes distinguish this wire shape from
    // protocol 5's fixed i32 and the later fixed i64 form.
    let body = [0xB4, 0x24];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::KEEP_ALIVE, &body),
        ServerBound::KeepAlive { id: 0x1234 }
    );
    assert!(matches!(
        protocol.encode_keep_alive(0x1234),
        ServerDirective::Send { packet_id, payload }
            if packet_id == play::clientbound::KEEP_ALIVE && payload == body
    ));
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::KEEP_ALIVE, &[0xB4, 0x24, 0]),
        ServerBound::Ignored,
        "a keep-alive body with a trailing byte must not acknowledge the challenge"
    );
}

#[test]
fn client_settings_lift_the_protocol_47_view_distance() {
    let protocol = V47ServerProtocol;
    // `en_us`, distance 6, full chat with colours, and every displayed skin
    // part. Protocol 47 ends here; it has no main-hand field.
    let body = [5, b'e', b'n', b'_', b'u', b's', 6, 0, 1, 0x7f];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::SETTINGS, &body),
        ServerBound::ClientInformationChanged { view_distance: 6 }
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::SETTINGS, &[5, b'e', b'n', b'_', b'u', b's', 6, 0, 1, 0x7f, 0]),
        ServerBound::Ignored,
        "a settings packet with a trailing byte must not resize the client view"
    );
}

#[test]
fn unsupported_states_are_errors_not_air_substitutions() {
    let protocol = V47ServerProtocol;
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:not_a_real_state")
        .is_err());

    let unsupported = (0..block_states::STATE_COUNT)
        .find(|&state| inverse::resolve(state).is_err())
        .expect("the canonical registry has states outside the legacy image");
    let mut state = block_name(unsupported)
        .expect("state is in the canonical registry")
        .to_owned();
    let props = properties(unsupported).expect("state is in the canonical registry");
    if !props.is_empty() {
        state.push('[');
        for (index, (name, value)) in props.iter().enumerate() {
            if index != 0 {
                state.push(',');
            }
            state.push_str(name);
            state.push('=');
            state.push_str(value);
        }
        state.push(']');
    }
    assert_eq!(block_states::state_id(&state), Some(unsupported));
    assert!(protocol.try_encode_block_update(0, 64, 0, &state).is_err());

    let later = block_states::state_id("minecraft:end_rod")
        .expect("the canonical registry includes the protocol-107 addition");
    assert!(
        inverse::resolve(later).is_ok(),
        "the generic pre-flattening inverse alone cannot rule this state out"
    );
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:end_rod")
        .is_err());
}
