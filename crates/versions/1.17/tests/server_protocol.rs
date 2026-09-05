use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_data::block_states;
use lodestone_model::Rotation;
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_17::{V756ServerProtocol, V758ServerProtocol};
use lodestone_v1_17::packet_ids::handshaking;
use lodestone_v1_17::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_17::packets::game::JoinGame;
use lodestone_v1_17::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 756 };
const CTX_758: Ctx = Ctx { version: 758 };

/// These are literal Play packet bodies for the four legacy movement forms.
/// The values deliberately have negative and fractional components so swapping
/// a field, treating a double as a float, or losing the final grounded flag is
/// observable without asking the encoder under test for expected bytes.
const POSITION_BODY: [u8; 25] = [
    0xbf, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // x = -1.5
    0x40, 0x50, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, // y = 64.25
    0x40, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // z = 32.0
    0x01, // on_ground = true
];
const POSITION_LOOK_BODY: [u8; 33] = [
    0x40, 0x30, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, // x = 16.5
    0x40, 0x51, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, // y = 70.5
    0xc0, 0x40, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, // z = -33.25
    0x42, 0xb4, 0x00, 0x00, // yaw = 90.0
    0xc2, 0x34, 0x00, 0x00, // pitch = -45.0
    0x00, // on_ground = false
];

fn assert_movement_lift<P: ServerProtocol>(protocol: &P, position: i32, position_look: i32, look: i32, flying: i32) {
    assert_eq!(
        protocol.decode(State::Play, position, &POSITION_BODY),
        ServerBound::PlayerMoved {
            x: -1.5,
            y: 64.25,
            z: 32.0,
            rotation: None,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, position_look, &POSITION_LOOK_BODY),
        ServerBound::PlayerMoved {
            x: 16.5,
            y: 70.5,
            z: -33.25,
            rotation: Some(Rotation {
                yaw: 90.0,
                pitch: -45.0,
            }),
            on_ground: false,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, look, &[0x40, 0x20, 0x00, 0x00, 0xc1, 0xa0, 0x00, 0x00, 0x01]),
        ServerBound::PlayerRotated {
            yaw: 2.5,
            pitch: -20.0,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, flying, &[0]),
        ServerBound::PlayerStatusOnly { on_ground: false }
    );
}

#[test]
fn protocol_756_lifts_literal_movement_bodies() {
    use lodestone_v1_17::packet_ids::play::serverbound;

    assert_movement_lift(
        &V756ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

#[test]
fn protocol_758_lifts_literal_movement_bodies() {
    use lodestone_v1_17::packet_ids_758::play::serverbound;

    assert_movement_lift(
        &V758ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

#[test]
fn protocol_756_uses_its_capture_ids_and_encodes_a_1_17_chunk() {
    let protocol = V756ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_17_1.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [38, 56, 34] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.17.1 capture must contain play packet id {expected}"
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
            CTX,
        )
        .expect("handshake fixture encodes")
    };
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(756)),
        ServerBound::Handshake { next_state: State::Login }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(758)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, 38);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX).expect("join follows protocol-756 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.view_distance, 8);
    assert_eq!(join.world_name, "minecraft:overworld");
    assert!(matches!(
        play.get(1),
        Some(ServerDirective::Send { packet_id: 56, .. })
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.17.1 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 34);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(756))
        .expect("encoded chunk follows the protocol-756 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(decoded.column.get_block(3, 100, 5), block_states::state_id("minecraft:stone").unwrap());

    assert!(matches!(
        protocol.try_encode_block_update(3, 100, 5, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 12, .. })
    ));
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:sculk")
        .expect_err("a post-1.17 state must not substitute into an older packet");
    assert!(error.to_string().contains("protocol-756"));
}

#[test]
fn protocol_758_uses_its_capture_ids_and_encodes_an_inline_light_chunk() {
    let protocol = V758ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_18_2.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [38, 56, 34] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.18.2 capture must contain play packet id {expected}"
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
            CTX_758,
        )
        .expect("handshake fixture encodes")
    };
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(758)),
        ServerBound::Handshake { next_state: State::Login }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(756)),
        ServerBound::Ignored
    );

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, 38);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX_758).expect("join follows protocol-758 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.simulation_distance, 8);
    assert_eq!(join.world_name, "minecraft:overworld");

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.18.2 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 34);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(758))
        .expect("encoded chunk follows the protocol-758 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(decoded.column.get_block(3, 100, 5), block_states::state_id("minecraft:stone").unwrap());

    assert!(matches!(
        protocol.try_encode_block_update(3, 100, 5, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 12, .. })
    ));
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:sculk")
        .expect_err("a post-1.18 state must not substitute into an older packet");
    assert!(error.to_string().contains("protocol-758"));
}
