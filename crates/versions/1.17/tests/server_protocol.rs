use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_data::block_states;
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_17::V756ServerProtocol;
use lodestone_v1_17::packet_ids::handshaking;
use lodestone_v1_17::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_17::packets::game::JoinGame;
use lodestone_v1_17::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 756 };

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
