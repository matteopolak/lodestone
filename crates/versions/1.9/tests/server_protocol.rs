use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_data::block_states::{self, block_name, properties};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_9::V340ServerProtocol;
use lodestone_v1_9::packet_ids::handshaking;
use lodestone_v1_9::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 340 };

#[test]
fn accepts_only_the_hosted_handshake_protocol() {
    let protocol = V340ServerProtocol;
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
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(340)),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(316)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());
}

#[test]
fn play_join_chunk_and_block_update_have_340_wire_ids() {
    let protocol = V340ServerProtocol;
    let join = protocol.begin_play(8);
    let Some(ServerDirective::Send { packet_id, payload }) = join.first() else {
        panic!("join must begin with a packet");
    };
    assert_eq!(*packet_id, 35);
    assert_eq!(
        payload,
        &[
            0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 20, 7, b'd', b'e', b'f', b'a', b'u', b'l', b't', 0,
        ]
    );
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send {
            packet_id: 47,
            payload,
        }) if payload == &[
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0x40, 0x59, 0, 0, 0, 0, 0, 0,
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    ));

    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(0, 0, &column)
        .expect("stone has an exact protocol-340 state")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 32);
    let mut packet = Reader::new(&payload);
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.bool(), Ok(true));
    assert_eq!(packet.var_i32(), Ok(1));
    let blob_len = usize::try_from(packet.var_i32().expect("chunk data length"))
        .expect("positive length");
    let mut blob = packet.take_reader(blob_len).expect("chunk data");
    // These wire values are independently pinned by the protocol-340 chunk
    // fixtures in `tests/chunk.rs`: indirect palettes use a four-bit index,
    // and stone's legacy state value is 16.
    assert_eq!(blob.u8(), Ok(4));
    assert_eq!(blob.var_i32(), Ok(2));
    assert_eq!(blob.var_i32(), Ok(16));
    assert_eq!(blob.var_i32(), Ok(0));
    assert_eq!(blob.var_i32(), Ok(256));
    assert_eq!(blob.i64(), Ok(0x1111_1111_1111_1110_i64));
    for _ in 1..256 {
        assert_eq!(blob.i64(), Ok(0x1111_1111_1111_1111_i64));
    }
    assert_eq!(blob.bytes(2048), Ok(&[0; 2048][..]));
    assert_eq!(blob.bytes(2048), Ok(&[u8::MAX; 2048][..]));
    assert_eq!(blob.bytes(256), Ok(&[1; 256][..]));
    assert!(blob.ensure_empty().is_ok());
    assert_eq!(packet.var_i32(), Ok(0));
    assert!(packet.ensure_empty().is_ok());
    assert!(matches!(
        protocol.encode_block_update(1, 64, -1, "minecraft:stone"),
        ServerDirective::Send { packet_id: 11, payload }
            if payload == vec![0, 0, 0, 0x41, 0x03, 0xFF, 0xFF, 0xFF, 16]
    ));
}

#[test]
fn unsupported_states_are_errors_not_air_substitutions() {
    let protocol = V340ServerProtocol;
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
}

#[test]
fn projects_the_legacy_window_from_a_covering_canonical_column() {
    let protocol = V340ServerProtocol;
    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 64, 4, "minecraft:stone");

    assert!(matches!(
        protocol.try_encode_chunk(0, 0, &column),
        Ok(ServerDirective::Send { packet_id: 32, .. })
    ));

    let too_short = ChunkColumn::new(-64, 319);
    assert!(protocol.try_encode_chunk(0, 0, &too_short).is_err());
}
