//! Hosted protocol-5 controls anchored to the committed 1.7 wire fixtures.

use std::io::Read as _;

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_data::block_states::{self, block_name, properties};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_7::V5ServerProtocol;
use lodestone_v1_7::packet_ids::{handshaking, play};
use lodestone_v1_7::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 5 };

#[test]
fn accepts_only_the_hosted_handshake_protocol() {
    let protocol = V5ServerProtocol;
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
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(5)),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(4)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());
}

#[test]
fn join_position_chunk_and_block_update_match_protocol_5_layout() {
    let protocol = V5ServerProtocol;
    let join = protocol.begin_play(0);
    let Some(ServerDirective::Send { packet_id, payload }) = join.first() else {
        panic!("join must begin with a packet");
    };
    assert_eq!(*packet_id, 1);
    assert_eq!(
        payload,
        &[0, 0, 0, 1, 0, 0, 2, 20, 7, b'd', b'e', b'f', b'a', b'u', b'l', b't']
    );
    assert!(matches!(
        join.get(1),
        Some(ServerDirective::Send { packet_id: 8, payload }) if payload == &[
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0x40, 0x59, 0x67, 0xAE, 0x14, 0x7A, 0xE1, 0x48,
            0x40, 0x20, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(0, 0, 0, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(0, 0, &column)
        .expect("stone has an exact protocol-5 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 33);
    let mut packet = Reader::new(&payload);
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.i32(), Ok(0));
    assert_eq!(packet.bool(), Ok(true));
    assert_eq!(packet.u16(), Ok(1));
    assert_eq!(packet.u16(), Ok(0));
    let compressed_len = usize::try_from(packet.i32().expect("compressed chunk length"))
        .expect("positive compressed chunk length");
    let compressed = packet.bytes(compressed_len).expect("compressed chunk body");
    assert!(packet.ensure_empty().is_ok());
    let mut inflated = Vec::new();
    flate2::read::ZlibDecoder::new(compressed)
        .read_to_end(&mut inflated)
        .expect("protocol-5 chunk body inflates");
    // The committed 1.7 fixture fixes this as type, metadata, block-light,
    // sky-light, then biome arrays. Stone is the first type byte.
    assert_eq!(inflated.len(), 10_496);
    assert_eq!(inflated[0], 1);
    assert_eq!(&inflated[1..4096], &[0; 4095]);
    assert_eq!(&inflated[4096..6144], &[0; 2048]);
    assert_eq!(&inflated[6144..8192], &[0; 2048]);
    assert_eq!(&inflated[8192..10240], &[u8::MAX; 2048]);
    assert_eq!(&inflated[10240..], &[1; 256]);

    assert!(matches!(
        protocol.encode_block_update(1, 64, -1, "minecraft:stone"),
        ServerDirective::Send { packet_id: 35, payload }
            if payload == vec![0, 0, 0, 1, 64, 0xFF, 0xFF, 0xFF, 0xFF, 1, 0]
    ));
}

#[test]
fn projects_the_legacy_window_and_decodes_break_actions() {
    let protocol = V5ServerProtocol;
    let mut covering = ChunkColumn::new(-64, 384);
    covering.set_block(3, 64, 4, "minecraft:stone");
    assert!(protocol.try_encode_chunk(0, 0, &covering).is_ok());
    assert!(protocol
        .try_encode_chunk(0, 0, &ChunkColumn::new(-64, 319))
        .is_err());

    // Status 0, unpacked (1, 64, 3), and the Up face are the complete
    // protocol-5 break body; there is no prediction sequence.
    let payload = [0, 0, 0, 0, 1, 64, 0, 0, 0, 3, 1];
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
fn unsupported_states_are_errors_not_air_substitutions() {
    let protocol = V5ServerProtocol;
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

    let later = block_states::state_id("minecraft:slime_block")
        .expect("the canonical registry includes the protocol-47 addition");
    assert!(
        inverse::resolve(later).is_ok(),
        "the generic pre-flattening inverse alone cannot rule this state out"
    );
    assert!(protocol
        .try_encode_block_update(0, 64, 0, "minecraft:slime_block")
        .is_err());

    assert!(matches!(
        protocol.try_encode_block_update(0, 64, 0, "minecraft:packed_ice"),
        Ok(ServerDirective::Send { packet_id: 35, payload })
            if payload == vec![0, 0, 0, 0, 64, 0, 0, 0, 0, 0xAE, 1, 0]
    ));
}
