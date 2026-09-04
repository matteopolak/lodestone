use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_20_6::{V766ServerProtocol, packet_ids};
use lodestone_v1_20_6::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_20_6::packets::configuration::RegistryData;
use lodestone_v1_20_6::packets::game::{BlockDig, JoinGame};
use lodestone_v1_20_6::packets::position::Position;

const CTX: Ctx = Ctx { version: 766 };

#[test]
fn surface_heightmap_uses_first_free_y_and_non_straddling_nine_bit_longs() {
    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 101, 5, "minecraft:stone");
    column.set_block(3, 201, 5, "minecraft:cave_air");
    let ServerDirective::Send { payload, .. } =
        V766ServerProtocol.try_encode_chunk(0, 0, &column).unwrap() else { panic!("chunk"); };
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.i32().unwrap(), 0);
    assert_eq!(reader.i32().unwrap(), 0);
    let lodestone_core::Nbt::Compound(maps) = lodestone_core::read_network_nbt(&mut reader).unwrap()
        else { panic!("heightmap compound"); };
    let (name, lodestone_core::Nbt::LongArray(longs)) = &maps[0] else { panic!("heightmap longs"); };
    assert_eq!(name, "WORLD_SURFACE");
    assert_eq!(longs.len(), 37);
    assert_eq!(longs[11] as u64, 166_u64 << 54);
}

fn captured_configuration() -> Vec<(i32, Vec<u8>)> {
    include_str!("captures/join_1_20_6.txt").lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "configuration" { return None; }
        let id: i32 = fields.next()?.parse().ok()?;
        if ![7, 12, 13].contains(&id) { return None; }
        let hex = fields.next()?;
        Some((id, (0..hex.len()).step_by(2).map(|index|
            u8::from_str_radix(&hex[index..index + 2], 16).unwrap()).collect()))
    }).collect()
}

#[test]
fn hosted_configuration_is_the_external_capture_with_full_registry_payloads() {
    let protocol = V766ServerProtocol;
    let expected = captured_configuration();
    let actual = protocol.encode_registry_data();
    assert_eq!(actual.len(), expected.len());
    let mut registry_count = 0;
    let mut dimension = None;
    for (directive, (id, body)) in actual.iter().zip(&expected) {
        let ServerDirective::Send { packet_id, payload } = directive else { panic!("expected packet"); };
        assert_eq!((*packet_id, payload), (*id, body));
        if *id == 7 {
            registry_count += 1;
            let mut reader = Reader::new(payload);
            let registry = RegistryData::decode(&mut reader, CTX).unwrap();
            reader.ensure_empty().unwrap();
            assert!(registry.entries.iter().all(|entry| entry.data.is_some()),
                "registry {} must not require known-pack negotiation", registry.registry);
            if registry.registry == "minecraft:dimension_type" {
                dimension = registry.entries.iter().position(|entry| entry.id == "minecraft:overworld");
            }
        }
    }
    assert_eq!(registry_count, 4);
    let play = protocol.begin_play(7);
    let ServerDirective::Send { packet_id, payload } = &play[0] else { panic!("join"); };
    assert_eq!(*packet_id, 43);
    let mut reader = Reader::new(payload);
    let join = JoinGame::decode(&mut reader, CTX).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(join.world_state.dimension, i32::try_from(dimension.unwrap()).unwrap());
    assert_eq!(join.view_distance, 7);
    assert!(protocol.has_configuration_phase());
    assert_eq!(protocol.decode(State::Login, 3, &[]), ServerBound::LoginAcknowledged);
    assert_eq!(protocol.decode(State::Configuration, 3, &[]), ServerBound::ConfigurationFinished);
    assert_eq!(protocol.decode(State::Configuration, 3, &[0]), ServerBound::Ignored);
}

#[test]
fn chunk_framing_and_exact_state_rejection() {
    let protocol = V766ServerProtocol;
    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 101, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } =
        protocol.try_encode_chunk(7, -4, &column).unwrap() else { panic!("chunk"); };
    assert_eq!(packet_id, packet_ids::play::clientbound::MAP_CHUNK);
    let mut reader = Reader::new(&payload);
    let chunk = MapChunk::decode(&mut reader, &ChunkShape::overworld(766)).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(chunk.column.get_block(3, 101, 5), 1);
    assert_eq!(chunk.column.get_block(4, 101, 5), 0);
    let ServerDirective::Send { payload, .. } =
        protocol.try_encode_block_update(3, 101, 5, "minecraft:stone").unwrap() else { panic!("update"); };
    let mut reader = Reader::new(&payload);
    // Packed x/z/y arithmetic is independent of the packet codec.
    assert_eq!(reader.i64().unwrap(), (3_i64 << 38) | (5_i64 << 12) | 101);
    assert_eq!(reader.var_i32().unwrap(), 1);
    reader.ensure_empty().unwrap();
    assert!(protocol.try_encode_chunk(0, 0, &ChunkColumn::new(0, 256)).is_err());
    assert!(protocol.try_encode_block_update(0, 0, 0, "minecraft:does_not_exist").is_err());
    let unsupported = (0..lodestone_data::block_states::STATE_COUNT).find(|state|
        !lodestone_v1_20_6::generated_canonical::STATE_TO_CANONICAL.contains(state)).unwrap();
    let name = lodestone_data::block_states::block_name(unsupported).unwrap();
    let properties = lodestone_data::block_states::properties(unsupported).unwrap().iter()
        .map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(",");
    column.set_block(0, 0, 0, &format!("{name}[{properties}]"));
    assert!(protocol.try_encode_chunk(0, 0, &column).is_err());
}

#[test]
fn block_action_sequence_and_batch_ack_reach_the_server_vocabulary() {
    let protocol = V766ServerProtocol;
    let body = encode_body(&BlockDig {
        status: 0, location: Position::new(3, 101, 5), face: 1, sequence: 17,
    }, CTX).unwrap();
    assert_eq!(protocol.decode(State::Play, packet_ids::play::serverbound::BLOCK_DIG, &body),
        ServerBound::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(3, 101, 5), face: BlockFace::Up, sequence: 17,
        });
    assert_eq!(protocol.decode(State::Play, packet_ids::play::serverbound::CHUNK_BATCH_RECEIVED,
        &3.5_f32.to_be_bytes()),
        ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick: 3.5 });
}
