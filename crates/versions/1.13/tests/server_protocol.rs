use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_model::{
    AnimationAction, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_13::{V404Adapter, V404ServerProtocol};
use lodestone_v1_13::packet_ids::{handshaking, play};
use lodestone_world::World;
use lodestone_v1_13::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 404 };

#[test]
fn accepts_only_the_hosted_handshake_protocol() {
    let protocol = V404ServerProtocol;
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
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(404)),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(340)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());
}

#[test]
fn join_position_chunk_and_block_update_match_protocol_404_fixtures() {
    let protocol = V404ServerProtocol;
    let join = protocol.begin_play(8);
    let Some(ServerDirective::Send { packet_id, payload }) = join.first() else {
        panic!("play must begin with a join packet");
    };
    assert_eq!(*packet_id, 37);
    assert_eq!(
        payload,
        &[
            0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 20, 7, b'd', b'e', b'f', b'a', b'u', b'l', b't', 0,
        ]
    );
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 50, payload }) if payload == &[
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0x40, 0x59, 0, 0, 0, 0, 0, 0,
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    ));

    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, "minecraft:dandelion");
    assert_eq!(column.block_state(0, 0, 0), "minecraft:dandelion");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(0, 0, &column)
        .expect("dandelion has protocol-404 state id 1111 in the committed jar report")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 34);
    let mut packet = Reader::new(&payload);
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.bool(), Ok(true));
    assert_eq!(packet.var_i32(), Ok(1));
    let blob_len = usize::try_from(packet.var_i32().expect("chunk data length"))
        .expect("positive length");
    let mut blob = packet.take_reader(blob_len).expect("chunk data");
    assert_eq!(blob.u8(), Ok(4));
    assert_eq!(blob.var_i32(), Ok(2));
    assert_eq!(blob.var_i32(), Ok(1111));
    assert_eq!(blob.var_i32(), Ok(0));
    assert_eq!(blob.var_i32(), Ok(256));
    for index in 0..256 {
        let expected = if index == 0 {
            0x1111_1111_1111_1110_i64
        } else {
            0x1111_1111_1111_1111_i64
        };
        assert_eq!(blob.i64(), Ok(expected), "long {index}");
    }
    assert_eq!(blob.bytes(2048), Ok(&[0; 2048][..]));
    assert_eq!(blob.bytes(2048), Ok(&[u8::MAX; 2048][..]));
    for _ in 0..256 {
        assert_eq!(blob.i32(), Ok(1));
    }
    assert!(blob.ensure_empty().is_ok());
    assert_eq!(packet.var_i32(), Ok(0));
    assert!(packet.ensure_empty().is_ok());

    assert!(matches!(
        protocol.try_encode_block_update(1, 64, -1, "minecraft:dandelion"),
        Ok(ServerDirective::Send { packet_id: 11, payload })
            if payload == vec![0, 0, 0, 0x41, 0x03, 0xFF, 0xFF, 0xFF, 0xD7, 0x08]
    ));
}

#[test]
fn keep_alive_uses_the_protocol_404_i64_body_in_both_directions() {
    let protocol = V404ServerProtocol;
    // An independently specified big-endian i64 body: no packet-codec
    // round-trip can make a wrong field width pass this control.
    let body = [1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(
        protocol.decode(State::Play, lodestone_v1_13::packet_ids::play::serverbound::KEEP_ALIVE, &body),
        ServerBound::KeepAlive {
            id: 0x0102_0304_0506_0708,
        }
    );
    assert!(matches!(
        protocol.encode_keep_alive(0x0102_0304_0506_0708),
        ServerDirective::Send { packet_id, payload }
            if packet_id == lodestone_v1_13::packet_ids::play::clientbound::KEEP_ALIVE && payload == body
    ));
    assert_eq!(
        protocol.decode(
            State::Play,
            lodestone_v1_13::packet_ids::play::serverbound::KEEP_ALIVE,
            &[1, 2, 3, 4, 5, 6, 7, 8, 0],
        ),
        ServerBound::Ignored,
        "a keep-alive body with a trailing byte must not acknowledge the challenge"
    );
}

#[test]
fn client_settings_lift_the_protocol_404_view_distance() {
    let protocol = V404ServerProtocol;
    // `en_us`, distance 6, hidden chat (VarInt 2), colours, every skin part,
    // right main hand. These literal fields separate this layout from 1.8.
    let body = [5, b'e', b'n', b'_', b'u', b's', 6, 2, 1, 0x7f, 1];
    assert_eq!(
        protocol.decode(
            State::Play,
            lodestone_v1_13::packet_ids::play::serverbound::SETTINGS,
            &body,
        ),
        ServerBound::ClientInformationChanged { view_distance: 6 }
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            lodestone_v1_13::packet_ids::play::serverbound::SETTINGS,
            &[5, b'e', b'n', b'_', b'u', b's', 6, 2, 1, 0x7f, 1, 0],
        ),
        ServerBound::Ignored,
        "a settings packet with a trailing byte must not resize the client view"
    );
}

#[test]
fn hosted_chat_lifts_a_literal_body_and_emits_a_json_system_reply() {
    let protocol = V404ServerProtocol;
    // A raw string body for `hi \"x\"\n`; this avoids using the packet codec to
    // specify the serverbound wire layout under test.
    const REQUEST: [u8; 8] = [0x07, b'h', b'i', b' ', b'\"', b'x', b'\"', b'\n'];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::CHAT, &REQUEST),
        ServerBound::Chat {
            message: "hi \"x\"\n".to_owned(),
            timestamp_millis: 0,
            salt: 0,
            signature: None,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::CHAT, &[0x04, b'o']),
        ServerBound::Ignored,
        "a truncated chat string must not reach the shared consumer"
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::CHAT,
            &[REQUEST.as_slice(), &[0]].concat(),
        ),
        ServerBound::Ignored,
        "a trailing byte must not be accepted as part of a protocol-404 chat body"
    );
    assert_eq!(
        protocol.decode(State::Configuration, play::serverbound::CHAT, &REQUEST),
        ServerBound::Ignored,
        "chat must not bypass the direct login-to-Play boundary"
    );

    let ServerDirective::Send { packet_id, payload } =
        protocol.encode_system_chat("plain \"wire\"\n")
    else {
        panic!("protocol-404 system chat must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::CHAT);
    assert_eq!(
        payload,
        vec![
            0x1b, b'{', b'\"', b't', b'e', b'x', b't', b'\"', b':', b'\"', b'p', b'l', b'a',
            b'i', b'n', b' ', b'\\', b'\"', b'w', b'i', b'r', b'e', b'\\', b'\"', b'\\', b'n',
            b'\"', b'}', 0x01,
        ],
        "the clientbound body is a JSON text component followed by ordinary system-chat position"
    );
}

#[test]
fn block_dig_non_breaking_statuses_reach_their_server_consumers() {
    let protocol = V404ServerProtocol;
    let adapter = V404Adapter::new();
    // Independent literal bodies: status VarInt, pre-1.14 packed zero
    // position, then the signed-byte direction. These actions ignore the
    // position/direction, but they remain part of the protocol-404 body and
    // prove the decoder does not accidentally use a later packet shape.
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
        (6, ServerBound::SwapItemInHand),
    ] {
        assert_eq!(
            protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(status)),
            expected,
            "status {status} must survive protocol-404 decoding"
        );
    }

    // Exercise the production hand-off as well: input action -> adapter's
    // protocol-404 body -> server decoder -> a concrete version-free
    // consumer variant. The literal rows above remain the wire-shape oracle;
    // this loop proves the two independently-owned endpoints are connected.
    for (action, expected) in [
        (ClientAction::DropSelectedItemStack, ServerBound::ItemDropped { whole_stack: true }),
        (ClientAction::DropSelectedItem, ServerBound::ItemDropped { whole_stack: false }),
        (ClientAction::ReleaseUseItem, ServerBound::ReleaseUseItem),
        (ClientAction::SwapItemWithOffhand, ServerBound::SwapItemInHand),
    ] {
        let Some((packet_id, payload)) = adapter
            .encode_action(ConnectionState::Play, &action)
            .expect("protocol-404 adapter encodes the supported action")
        else {
            panic!("{action:?} must produce a protocol-404 packet");
        };
        assert_eq!(packet_id, play::serverbound::BLOCK_DIG, "{action:?}");
        assert_eq!(
            protocol.decode(State::Play, packet_id, &payload),
            expected,
            "{action:?} must reach its server consumer variant"
        );
    }

    // The control keeps the break path distinct: a decoder that labels every
    // block-dig status as an item action would still make the four rows above
    // pass while making normal mining unusable.
    assert!(matches!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(2)),
        ServerBound::BlockAction { .. }
    ));
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &body(7)),
        ServerBound::Ignored,
        "an unmodelled status must stay ignored rather than selecting a neighbouring action"
    );
}

#[test]
fn block_place_lifts_the_literal_protocol_404_body() {
    let protocol = V404ServerProtocol;
    // Position is x(26) | y(12) | z(26): (5, -10, -7), then south, off hand,
    // and three IEEE-754 cursor coordinates. This is deliberately assembled
    // without the packet codec so swapping 404's position/direction/hand
    // ordering for a neighbouring layout cannot pass by round-trip agreement.
    const BODY: [u8; 22] = [
        0x00, 0x00, 0x01, 0x7f, 0xdb, 0xff, 0xff, 0xf9, // (5, -10, -7)
        0x03, // south
        0x01, // off hand
        0x3e, 0x80, 0x00, 0x00, // 0.25
        0x3f, 0x80, 0x00, 0x00, // 1.0
        0x3f, 0x40, 0x00, 0x00, // 0.75
    ];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_PLACE, &BODY),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f::new(0.25, 1.0, 0.75),
            sequence: 0,
            hand: 1,
        }
    );

    let mut invalid_face = BODY;
    invalid_face[8] = 0x06;
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_PLACE, &invalid_face),
        ServerBound::Ignored,
        "a seventh direction must not reach the placement consumer"
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::BLOCK_PLACE,
            &[BODY.as_slice(), &[0]].concat(),
        ),
        ServerBound::Ignored,
        "a trailing byte must not be accepted as part of a protocol-404 block use"
    );
}

#[test]
fn registry_selected_arm_animation_connects_protocol_404_to_the_shared_swing_broadcast() {
    let protocol = lodestone_registry::server_protocol_for_protocol(404)
        .expect("protocol 404 must resolve to its hosted server protocol");
    let adapter = V404Adapter::new();

    for (hand, expected_action) in [
        (Hand::Main, AnimationAction::SwingMainHand),
        (Hand::Off, AnimationAction::SwingOffHand),
    ] {
        let action = ClientAction::SwingArm { hand };
        let Some((packet_id, payload)) = adapter
            .encode_action(ConnectionState::Play, &action)
            .expect("protocol-404 adapter encodes arm swings")
        else {
            panic!("{action:?} must produce a protocol-404 packet");
        };
        assert_eq!(packet_id, play::serverbound::ARM_ANIMATION);
        assert_eq!(
            protocol.decode(State::Play, packet_id, &payload),
            ServerBound::Swing { hand },
            "{action:?} must reach the shared-server swing consumer"
        );

        let animation = if hand == Hand::Main { 0 } else { 3 };
        let ServerDirective::Send { packet_id, payload } =
            protocol.encode_animate(321, animation)
        else {
            panic!("the protocol-404 host must encode an animation reply");
        };
        assert_eq!(packet_id, play::clientbound::ANIMATION);
        let mut world = World::new();
        assert_eq!(
            adapter
                .handle_packet(&mut world, ConnectionState::Play, packet_id, &payload)
                .expect("the protocol-404 client decodes host animation"),
            vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id: 321,
                action: expected_action,
            })]
        );
    }

    assert_eq!(
        protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, &[2]),
        ServerBound::Ignored,
        "a third hand ordinal must not become an arbitrary animation"
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, &[0, 0]),
        ServerBound::Ignored,
        "a trailing byte must not be accepted as a swing"
    );
}

#[test]
fn registry_selected_hotbar_selection_reaches_the_inventory_consumer() {
    let protocol = lodestone_registry::server_protocol_for_protocol(404)
        .expect("protocol 404 must resolve to its hosted server protocol");

    // The body is a signed big-endian i16. These literals pin both legal
    // boundaries without asking either adapter direction for the bytes.
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::HELD_ITEM_SLOT, &[0x00, 0x00]),
        ServerBound::CarriedItemChanged { slot: 0 },
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::HELD_ITEM_SLOT, &[0x00, 0x08]),
        ServerBound::CarriedItemChanged { slot: 8 },
    );
    for body in [&[0xff, 0xff][..], &[0x00, 0x09], &[0x00], &[0x00, 0x08, 0x00]] {
        assert_eq!(
            protocol.decode(State::Play, play::serverbound::HELD_ITEM_SLOT, body),
            ServerBound::Ignored,
            "an invalid hotbar body {body:?} must not change inventory state",
        );
    }
    assert_eq!(
        protocol.decode(State::Login, play::serverbound::HELD_ITEM_SLOT, &[0x00, 0x08]),
        ServerBound::Ignored,
        "hotbar selection must not bypass the Play-state boundary",
    );

    let adapter = V404Adapter::new();
    let Some((packet_id, payload)) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SetCarriedItem { slot: 3 },
        )
        .expect("the client adapter must encode hotbar selection")
    else {
        panic!("hotbar selection must produce a packet");
    };
    assert_eq!(packet_id, play::serverbound::HELD_ITEM_SLOT);
    assert_eq!(payload, vec![0x00, 0x03]);
    assert_eq!(
        protocol.decode(State::Play, packet_id, &payload),
        ServerBound::CarriedItemChanged { slot: 3 },
        "the client action must reach the registry-selected hosted consumer",
    );
}

#[test]
fn states_missing_from_the_404_table_are_errors_not_air_substitutions() {
    let protocol = V404ServerProtocol;
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:bamboo")
        .is_err());
}
