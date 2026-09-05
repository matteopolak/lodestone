use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Vec3f};
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
fn block_action_and_initial_chunk_batch_use_the_hosted_wire_shapes() {
    let protocol = V766ServerProtocol;
    let body = encode_body(&BlockDig {
        status: 0, location: Position::new(3, 101, 5), face: 1, sequence: 17,
    }, CTX).unwrap();
    assert_eq!(protocol.decode(State::Play, packet_ids::play::serverbound::BLOCK_DIG, &body),
        ServerBound::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(3, 101, 5), face: BlockFace::Up, sequence: 17,
        });
    // External protocol-766 play ids: start=0x0d, finished=0x0c,
    // serverbound acknowledgement=0x08. The acknowledgement body is the
    // externally specified big-endian f32 3.5, not a local encoder round trip.
    assert_eq!(protocol.begin_chunk_batch(), ServerDirective::Send {
        packet_id: 0x0d,
        payload: Vec::new(),
    });
    assert_eq!(protocol.end_chunk_batch(9), ServerDirective::Send {
        packet_id: 0x0c,
        payload: vec![0x09],
    });
    assert_eq!(protocol.decode(State::Play, 0x08, &[0x40, 0x60, 0x00, 0x00]),
        ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick: 3.5 });
}

#[test]
fn block_place_literal_reaches_the_server_use_on_consumer_shape() {
    let protocol = V766ServerProtocol;
    // This is a protocol-766 `use_item_on` body assembled from the wire fields,
    // not this crate's BlockPlace encoder: off hand, (5, -10, -7), south face,
    // cursor (0.25, 1.0, 0.75), inside-block true, prediction sequence 17.
    let packed = ((5_i64 & 0x3ff_ffff) << 38)
        | ((-7_i64 & 0x3ff_ffff) << 12)
        | (-10_i64 & 0xfff);
    let body = [
        vec![0x01],
        packed.to_be_bytes().to_vec(),
        vec![0x03],
        0.25_f32.to_be_bytes().to_vec(),
        1.0_f32.to_be_bytes().to_vec(),
        0.75_f32.to_be_bytes().to_vec(),
        vec![0x01, 0x11],
    ]
    .concat();
    assert_eq!(
        protocol.decode(
            State::Play,
            packet_ids::play::serverbound::BLOCK_PLACE,
            &body,
        ),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f {
                x: 0.25,
                y: 1.0,
                z: 0.75,
            },
            sequence: 17,
            hand: 1,
        }
    );

    let mut invalid_face = body.clone();
    invalid_face[9] = 0x06;
    assert_eq!(
        protocol.decode(
            State::Play,
            packet_ids::play::serverbound::BLOCK_PLACE,
            &invalid_face,
        ),
        ServerBound::Ignored,
        "a non-face direction must not reach placement"
    );
    assert_eq!(
        protocol.decode(
            State::Configuration,
            packet_ids::play::serverbound::BLOCK_PLACE,
            &body,
        ),
        ServerBound::Ignored,
        "configuration packets must not leak into Play consumers"
    );
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
    let all_neighbours = (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| (dx, dz, ChunkColumn::new(-64, 384)))
        .collect::<Vec<_>>();
    assert_eq!(all_neighbours.len(), 8, "the full fixture must supply every neighbour");
    let with_all = protocol
        .compute_column_light_with_neighbours(&center, &all_neighbours)
        .unwrap();
    let without_east = all_neighbours
        .iter()
        .filter(|(dx, dz, _)| (*dx, *dz) != (1, 0))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(without_east.len(), 7, "control must remove only the open east column");
    let without_east = protocol
        .compute_column_light_with_neighbours(&center, &without_east)
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
    assert_eq!(
        with_all.section_sky_light(10, 15, 4, 8),
        Some(14),
        "a fully resident open 3x3 must retain the east sky path"
    );
    assert_eq!(
        without_east.section_sky_light(10, 15, 4, 8),
        Some(7),
        "control: the north/south sky paths reach the east border only after eight steps"
    );
    let ServerDirective::Send { payload, .. } = protocol
        .try_encode_chunk_with_neighbours(0, 0, &center, &all_neighbours)
        .unwrap() else { panic!("chunk"); };
    let mut reader = Reader::new(&payload);
    let chunk = MapChunk::decode(&mut reader, &ChunkShape::overworld(766)).unwrap();
    reader.ensure_empty().unwrap();
    assert_eq!(
        chunk.light.section_sky_light(10, 15, 4, 8),
        Some(14),
        "the full-neighbour answer must reach protocol-766 bytes"
    );
}
