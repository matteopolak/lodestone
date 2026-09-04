use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_data::block_states;
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_19::V762ServerProtocol;
use lodestone_v1_19::packet_ids::{handshaking, play};
use lodestone_v1_19::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_19::packets::game::{BlockDig, JoinGame};
use lodestone_v1_19::packets::handshake::SetProtocol;
use lodestone_v1_19::packets::position::Position;

const CTX: Ctx = Ctx { version: 762 };

#[test]
fn protocol_762_uses_its_capture_ids_and_encodes_a_registry_shaped_chunk() {
    let protocol = V762ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_19_4.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [36, 60] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.19.4 capture must contain play packet id {expected}"
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
        protocol.decode(
            State::Handshaking,
            handshaking::serverbound::SET_PROTOCOL,
            &request(762)
        ),
        ServerBound::Handshake {
            next_state: State::Login
        }
    );
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            handshaking::serverbound::SET_PROTOCOL,
            &request(761)
        ),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());

    let play_directives = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play_directives[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, play::clientbound::LOGIN);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX).expect("join follows protocol-762 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.view_distance, 8);
    assert_eq!(join.simulation_distance, 8);
    assert_eq!(join.world_type, "minecraft:overworld");
    assert_eq!(join.world_name, "minecraft:overworld");
    let shape = ChunkShape::overworld(762)
        .from_dimension_registry(&join.dimension_codec, &join.world_type)
        .expect("hosted join declares the selected dimension window");
    assert_eq!((shape.min_y, shape.section_count), (-64, 24));
    assert!(matches!(
        play_directives.get(1),
        Some(ServerDirective::Send { packet_id, .. }) if *packet_id == play::clientbound::POSITION
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.19.4 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::MAP_CHUNK);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(762))
        .expect("encoded chunk follows the protocol-762 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(
        decoded.column.get_block(3, 100, 5),
        block_states::state_id("minecraft:stone").unwrap()
    );

    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:stone")
        .expect("stone block update has an exact 1.19.4 representation")
    else {
        panic!("block update encoder must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::BLOCK_CHANGE);
    let mut update_reader = Reader::new(&payload);
    assert_eq!(
        update_reader.i64().expect("block position"),
        lodestone_v1_19::packets::position::pack_position(BlockPos::new(3, 100, 5))
    );
    assert!(update_reader.var_i32().expect("wire state") >= 0);
    update_reader
        .ensure_empty()
        .expect("block update has no trailing bytes");

    let dig = encode_body(
        &BlockDig {
            status: 0,
            location: Position::new(3, 100, 5),
            face: 1,
            sequence: 17,
        },
        CTX,
    )
    .expect("block-dig fixture encodes");
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &dig),
        ServerBound::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(3, 100, 5),
            face: BlockFace::Up,
            sequence: 17,
        }
    );
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:creaking_heart")
        .expect_err("a newer state must not substitute into a protocol-762 packet");
    assert!(error.to_string().contains("protocol-762"));
}
