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


#[test]
fn hosted_configuration_matches_the_full_oracle_registry_manifest() {
    let protocol = V766ServerProtocol;
    let actual = protocol.encode_registry_data();
    let mut registries = std::collections::BTreeMap::new();
    let mut dimension = None;
    for directive in &actual {
        let ServerDirective::Send { packet_id, payload } = directive else { panic!("expected packet"); };
        if *packet_id == 7 {
            let mut reader = Reader::new(payload);
            let registry = RegistryData::decode(&mut reader, CTX).unwrap();
            reader.ensure_empty().unwrap();
            assert!(registries.insert(registry.registry.clone(), registry.entries.len()).is_none());
            assert!(registry.entries.iter().all(|entry| entry.data.is_some()),
                "registry {} must not require known-pack negotiation", registry.registry);
            if registry.registry == "minecraft:dimension_type" {
                dimension = registry.entries.iter().position(|entry| entry.id == "minecraft:overworld");
            }
        }
    }
    let expected_registry_sizes = std::collections::BTreeMap::from([
        ("minecraft:banner_pattern".to_owned(), 41),
        ("minecraft:chat_type".to_owned(), 7),
        ("minecraft:damage_type".to_owned(), 45),
        ("minecraft:dimension_type".to_owned(), 4),
        ("minecraft:trim_material".to_owned(), 10),
        ("minecraft:trim_pattern".to_owned(), 16),
        ("minecraft:wolf_variant".to_owned(), 9),
        ("minecraft:worldgen/biome".to_owned(), 64),
    ]);
    assert_eq!(registries, expected_registry_sizes);
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

#[test]
fn movement_shapes_preserve_position_rotation_and_ground_status() {
    let protocol = V766ServerProtocol;
    let position = [24.0_f64, 100.0, 8.0]
        .into_iter()
        .flat_map(f64::to_be_bytes)
        .chain([1])
        .collect::<Vec<_>>();
    assert_eq!(
        protocol.decode(State::Play, packet_ids::play::serverbound::POSITION, &position),
        ServerBound::PlayerMoved {
            x: 24.0,
            y: 100.0,
            z: 8.0,
            rotation: None,
            on_ground: true,
        }
    );
    let position_look = [24.0_f64, 100.0, 8.0]
        .into_iter()
        .flat_map(f64::to_be_bytes)
        .chain(90.0_f32.to_be_bytes())
        .chain((-15.0_f32).to_be_bytes())
        .chain([0])
        .collect::<Vec<_>>();
    assert_eq!(
        protocol.decode(
            State::Play,
            packet_ids::play::serverbound::POSITION_LOOK,
            &position_look,
        ),
        ServerBound::PlayerMoved {
            x: 24.0,
            y: 100.0,
            z: 8.0,
            rotation: Some(lodestone_model::Rotation::new(90.0, -15.0)),
            on_ground: false,
        }
    );
    let look = 45.0_f32
        .to_be_bytes()
        .into_iter()
        .chain(30.0_f32.to_be_bytes())
        .chain([1])
        .collect::<Vec<_>>();
    assert_eq!(
        protocol.decode(State::Play, packet_ids::play::serverbound::LOOK, &look),
        ServerBound::PlayerRotated {
            yaw: 45.0,
            pitch: 30.0,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, packet_ids::play::serverbound::FLYING, &[0]),
        ServerBound::PlayerStatusOnly { on_ground: false }
    );
}

#[test]
fn a_loaded_neighbour_contributes_sky_light_across_the_east_border() {
    let protocol = V766ServerProtocol;
    let mut center = ChunkColumn::new(-64, 384);
    for z in 0..16 {
        for x in 0..16 {
            center.set_block(x, 101, z, "minecraft:stone");
        }
    }
    let isolated = protocol.compute_column_light(&center).unwrap();
    let with_east = protocol
        .compute_column_light_with_neighbours(
            &center,
            &[(1, 0, ChunkColumn::new(-64, 384))],
        )
        .unwrap();
    assert_eq!(
        isolated.section_sky_light(10, 15, 4, 8),
        Some(0),
        "the isolated control must not invent sky through its east border"
    );
    assert_eq!(
        with_east.section_sky_light(10, 15, 4, 8),
        Some(14),
        "the adjacent open column is one horizontal step from local x=15"
    );
    let ServerDirective::Send { payload, .. } = protocol
        .try_encode_chunk_with_neighbours(0, 0, &center, &[(1, 0, ChunkColumn::new(-64, 384))])
        .unwrap() else { panic!("chunk"); };
    let mut reader = Reader::new(&payload);
    let chunk = MapChunk::decode(&mut reader, &ChunkShape::overworld(766)).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(chunk.light.section_sky_light(10, 15, 4, 8), Some(14));
}
