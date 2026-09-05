//! Hosted protocol-47 controls anchored to the existing 1.8 wire fixtures.

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Reader, State, encode_body};
use lodestone_data::block_states::{self, block_name, properties};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_8::V47ServerProtocol;
use lodestone_v1_8::packet_ids::{handshaking, play};
use lodestone_v1_8::packets::handshake::SetProtocol;

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
