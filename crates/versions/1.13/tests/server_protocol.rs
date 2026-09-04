use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_13::V404ServerProtocol;
use lodestone_v1_13::packet_ids::handshaking;
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
fn states_missing_from_the_404_table_are_errors_not_air_substitutions() {
    let protocol = V404ServerProtocol;
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:bamboo")
        .is_err());
}
