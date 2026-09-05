use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_data::block_states::{self, block_name, properties};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_9::{V110ServerProtocol, V210ServerProtocol, V316ServerProtocol, V340ServerProtocol};
use lodestone_v1_9::packet_ids::handshaking;
use lodestone_v1_9::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 340 };
const CTX_110: Ctx = Ctx { version: 110 };
const CTX_210: Ctx = Ctx { version: 210 };
const CTX_316: Ctx = Ctx { version: 316 };

#[test]
fn protocol_110_uses_its_captured_ids_and_rejects_1_11_only_states() {
    let protocol = V110ServerProtocol;
    let captured_play_ids: Vec<i32> = include_str!("captures/join_1_9_4.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "play" {
                return None;
            }
            fields.next()?.parse().ok()
        })
        .collect();
    for expected in [35, 46, 32] {
        assert!(
            captured_play_ids.contains(&expected),
            "the committed 1.9.4 capture must contain play packet id {expected}"
        );
    }
    let request = |protocol_version| {
        encode_body(
            &SetProtocol {
                protocol_version,
                server_host: "localhost".to_owned(),
                server_port: 25565,
                next_state: 2,
            },
            CTX_110,
        )
        .expect("handshake fixture encodes")
    };

    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_110::handshaking::serverbound::SET_PROTOCOL,
            &request(110),
        ),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_110::handshaking::serverbound::SET_PROTOCOL,
            &request(210),
        ),
        ServerBound::Ignored
    );

    let join = protocol.begin_play(8);
    assert!(matches!(
        join.first(),
        Some(ServerDirective::Send { packet_id: 35, .. })
    ));
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 46, .. })
    ));

    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, "minecraft:stone");
    assert!(matches!(
        protocol.try_encode_chunk(0, 0, &column),
        Ok(ServerDirective::Send { packet_id: 32, .. })
    ));
    assert!(matches!(
        protocol.try_encode_block_update(1, 64, -1, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 11, .. })
    ));

    // The committed 1.9 registry has structure block at legacy id 255, but
    // the 1.11-only magma-block range begins at 213.
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:structure_block[mode=save]")
        .is_ok());
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:magma_block")
        .is_err());
    column.set_block(1, 0, 0, "minecraft:magma_block");
    assert!(protocol.try_encode_chunk(0, 0, &column).is_err());
}

#[test]
fn protocol_210_uses_its_captured_ids_and_rejects_1_11_only_states() {
    let protocol = V210ServerProtocol;
    let captured_play_ids: Vec<i32> = include_str!("captures/join_1_10_2.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "play" {
                return None;
            }
            fields.next()?.parse().ok()
        })
        .collect();
    for expected in [35, 46, 32] {
        assert!(
            captured_play_ids.contains(&expected),
            "the committed 1.10.2 capture must contain play packet id {expected}"
        );
    }
    let request = |protocol_version| {
        encode_body(
            &SetProtocol {
                protocol_version,
                server_host: "localhost".to_owned(),
                server_port: 25565,
                next_state: 2,
            },
            CTX_210,
        )
        .expect("handshake fixture encodes")
    };

    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_210::handshaking::serverbound::SET_PROTOCOL,
            &request(210),
        ),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_210::handshaking::serverbound::SET_PROTOCOL,
            &request(316),
        ),
        ServerBound::Ignored
    );

    let join = protocol.begin_play(8);
    assert!(matches!(
        join.first(),
        Some(ServerDirective::Send { packet_id: 35, .. })
    ));
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 46, .. })
    ));

    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, "minecraft:stone");
    assert!(matches!(
        protocol.try_encode_chunk(0, 0, &column),
        Ok(ServerDirective::Send { packet_id: 32, .. })
    ));
    assert!(matches!(
        protocol.try_encode_block_update(1, 64, -1, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 11, .. })
    ));

    // The committed 1.10 registry has structure block at legacy id 255, but
    // the 1.11-only magma-block range begins at 213.
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:structure_block[mode=save]")
        .is_ok());
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:magma_block")
        .is_err());
    column.set_block(1, 0, 0, "minecraft:magma_block");
    assert!(protocol.try_encode_chunk(0, 0, &column).is_err());
}

#[test]
fn protocol_316_uses_its_captured_ids_and_rejects_1_12_only_states() {
    let protocol = V316ServerProtocol;
    let captured_play_ids: Vec<i32> = include_str!("captures/join_1_11_2.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "play" {
                return None;
            }
            fields.next()?.parse().ok()
        })
        .collect();
    for expected in [35, 46, 32] {
        assert!(
            captured_play_ids.contains(&expected),
            "the committed 1.11.2 capture must contain play packet id {expected}"
        );
    }
    let request = |protocol_version| {
        encode_body(
            &SetProtocol {
                protocol_version,
                server_host: "localhost".to_owned(),
                server_port: 25565,
                next_state: 2,
            },
            CTX_316,
        )
        .expect("handshake fixture encodes")
    };

    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_316::handshaking::serverbound::SET_PROTOCOL,
            &request(316),
        ),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            lodestone_v1_9::packet_ids_316::handshaking::serverbound::SET_PROTOCOL,
            &request(340),
        ),
        ServerBound::Ignored
    );

    let join = protocol.begin_play(8);
    assert!(matches!(
        join.first(),
        Some(ServerDirective::Send { packet_id: 35, .. })
    ));
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 46, .. })
    ));

    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, "minecraft:stone");
    assert!(matches!(
        protocol.try_encode_chunk(0, 0, &column),
        Ok(ServerDirective::Send { packet_id: 32, .. })
    ));
    assert!(matches!(
        protocol.try_encode_block_update(1, 64, -1, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 11, .. })
    ));

    // The committed 1.11 registry has structure block at legacy id 255, but
    // the 1.12-only glazed-terracotta range 235..=254 is absent.
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:structure_block[mode=save]")
        .is_ok());
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:white_glazed_terracotta[facing=north]")
        .is_err());
    column.set_block(1, 0, 0, "minecraft:white_glazed_terracotta[facing=north]");
    assert!(protocol.try_encode_chunk(0, 0, &column).is_err());
}

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
fn hosted_keep_alive_uses_each_protocols_own_id_and_width() {
    fn varint_wire_control(
        protocol: &dyn ServerProtocol,
        clientbound_id: i32,
        serverbound_id: i32,
    ) {
        // 0x1234 as a VarInt. This literal distinguishes 110/210/316 from
        // protocol 340's eight-byte body below.
        let body = [0xB4, 0x24];
        assert_eq!(
            protocol.decode(State::Play, serverbound_id, &body),
            ServerBound::KeepAlive { id: 0x1234 }
        );
        assert!(matches!(
            protocol.encode_keep_alive(0x1234),
            ServerDirective::Send { packet_id, payload }
                if packet_id == clientbound_id && payload == body
        ));
        assert_eq!(
            protocol.decode(State::Play, serverbound_id, &[0xB4, 0x24, 0]),
            ServerBound::Ignored,
            "a trailing byte must not acknowledge a keep-alive"
        );
    }

    varint_wire_control(
        &V110ServerProtocol,
        lodestone_v1_9::packet_ids_110::play::clientbound::KEEP_ALIVE,
        lodestone_v1_9::packet_ids_110::play::serverbound::KEEP_ALIVE,
    );
    varint_wire_control(
        &V210ServerProtocol,
        lodestone_v1_9::packet_ids_210::play::clientbound::KEEP_ALIVE,
        lodestone_v1_9::packet_ids_210::play::serverbound::KEEP_ALIVE,
    );
    varint_wire_control(
        &V316ServerProtocol,
        lodestone_v1_9::packet_ids_316::play::clientbound::KEEP_ALIVE,
        lodestone_v1_9::packet_ids_316::play::serverbound::KEEP_ALIVE,
    );

    let protocol = V340ServerProtocol;
    let body = [1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(
        protocol.decode(
            State::Play,
            lodestone_v1_9::packet_ids::play::serverbound::KEEP_ALIVE,
            &body,
        ),
        ServerBound::KeepAlive {
            id: 0x0102_0304_0506_0708,
        }
    );
    assert!(matches!(
        protocol.encode_keep_alive(0x0102_0304_0506_0708),
        ServerDirective::Send { packet_id, payload }
            if packet_id == lodestone_v1_9::packet_ids::play::clientbound::KEEP_ALIVE && payload == body
    ));
    assert_eq!(
        protocol.decode(
            State::Play,
            lodestone_v1_9::packet_ids::play::serverbound::KEEP_ALIVE,
            &[1, 2, 3, 4, 5, 6, 7, 8, 0],
        ),
        ServerBound::Ignored,
        "a trailing byte must not acknowledge a keep-alive"
    );
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
