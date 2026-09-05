use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_model::{ClientAction, ConnectionState, VersionAdapter};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_13::{V404Adapter, V404ServerProtocol};
use lodestone_v1_13::packet_ids::{handshaking, play};
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
fn states_missing_from_the_404_table_are_errors_not_air_substitutions() {
    let protocol = V404ServerProtocol;
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:bamboo")
        .is_err());
}
