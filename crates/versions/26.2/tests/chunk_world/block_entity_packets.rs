//! Dimension-aware block-entity packet gates for protocol 776.
//!
//! The chunk encoder owns the registry lookup and packet framing, while the
//! server column owns the sidecar list. These tests join those two boundaries
//! and decode the resulting bytes again. A sidecar that is present in a
//! generated column but absent from this list is an island: its block can
//! render, but its contents or destination metadata never reach the client.
//!
//! The same records are exercised against all three hosted dimension shapes.
//! The End gateway case is intentionally limited to the block-entity record;
//! End structure attachment is covered by the worldgen lane.

use lodestone_core::{Nbt, Reader};
use lodestone_model::BlockPos;
use lodestone_server::dimension::Dimension;
use lodestone_server::{BlockEntity, ChunkColumn, ChunkSource, ServerDirective, ServerProtocol};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_worldgen::overworld::GeneratedBlockEntity;

fn compound_field<'a>(nbt: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    fields
        .iter()
        .find_map(|(field_name, value)| (field_name == name).then_some(value))
}

fn shape_for_dimension(dimension: Dimension) -> ChunkShape {
    match dimension {
        Dimension::Overworld => ChunkShape::overworld_1_21(),
        Dimension::Nether | Dimension::End => ChunkShape::nether_or_end_1_21(),
    }
}

fn decode_encoded_chunk(dimension: Dimension, source: &ChunkColumn) -> LevelChunkWithLight {
    decode_encoded_chunk_at(0, 0, dimension, source)
}

fn decode_encoded_chunk_at(
    cx: i32,
    cz: i32,
    dimension: Dimension,
    source: &ChunkColumn,
) -> LevelChunkWithLight {
    let ServerDirective::Send { payload, .. } = V770ServerProtocol
        .try_encode_chunk_in_dimension(cx, cz, source, dimension)
        .expect("the 26.2 chunk encoder accepts a valid sidecar")
    else {
        panic!("chunk encoding must produce one send directive");
    };

    let shape = shape_for_dimension(dimension);
    let mut reader = Reader::new(&payload);
    let packet = LevelChunkWithLight::decode(&mut reader, &shape).expect("decode chunk packet");
    reader.ensure_empty().expect("chunk packet has no trailing bytes");
    packet
}

#[test]
fn nether_fortress_loot_and_spawner_sidecars_reach_the_chunk_packet() {
    let source = lodestone_server::nether_chunk_source(42);
    let spawner_column = source.column(-2, 0);
    let spawner_packet = decode_encoded_chunk_at(-2, 0, Dimension::Nether, &spawner_column);

    let spawner = spawner_packet
        .block_entities
        .iter()
        .find(|record| record.type_id == 9)
        .expect("fortress spawner sidecar must be present in the packet");
    assert_eq!((spawner.rel_x, spawner.y, spawner.rel_z), (7, 77, 11));
    assert!(compound_field(&spawner.nbt, "id").is_none());
    assert!(compound_field(&spawner.nbt, "SpawnData").is_some());
    assert!(compound_field(&spawner.nbt, "SpawnPotentials").is_none());

    // The seed-42 fortress's known chest at (-20, 53, 65) is in the adjacent
    // chunk (-2, 4), not the spawner hall's chunk. Keep both coordinates in
    // this gate so the packet path is checked for the two independent sidecar
    // products rather than assuming a structure's pieces share one column.
    let chest_column = source.column(-2, 4);
    let chest_packet = decode_encoded_chunk_at(-2, 4, Dimension::Nether, &chest_column);
    let chest_count = chest_packet
        .block_entities
        .iter()
        .filter(|record| record.type_id == 1)
        .count();
    assert!(chest_count > 0, "fortress loot sidecars must reach the packet");
    assert!(
        chest_packet
            .block_entities
            .iter()
            .filter(|record| record.type_id == 1)
        .all(|record| record.nbt == Nbt::End),
        "container update tags carry no persisted loot fields and use the null network tag"
    );
}

#[test]
fn generated_spawner_sidecars_survive_each_dimension_packet_shape() {
    let position = BlockPos::new(3, 70, 5);

    for dimension in Dimension::ALL {
        let mut source = ChunkColumn::new(dimension.min_y(), dimension.height());
        source.set_block(position.x, position.y, position.z, "minecraft:spawner");
        let generated = GeneratedBlockEntity::DungeonSpawner {
            x: position.x,
            y: position.y,
            z: position.z,
            entity_type: lodestone_data::entity_type::EntityType::CaveSpider.into(),
        };
        source.add_generated_block_entities(std::slice::from_ref(&generated));

        let packet = decode_encoded_chunk(dimension, &source);
        assert_eq!(packet.block_entities.len(), 1, "spawner in {dimension:?}");
        let record = &packet.block_entities[0];
        assert_eq!((record.rel_x, record.y, record.rel_z), (3, 70, 5));
        // Registry id 9 is pinned by the generated 26.2 block-entity table;
        // it is not the block-state id used by the source column.
        assert_eq!(record.type_id, 9, "spawner registry id in {dimension:?}");
        assert!(compound_field(&record.nbt, "id").is_none());
        assert!(compound_field(&record.nbt, "SpawnPotentials").is_none());
        assert_eq!(compound_field(&record.nbt, "Delay"), Some(&Nbt::Short(20)));
        assert_eq!(
            compound_field(&record.nbt, "MinSpawnDelay"),
            Some(&Nbt::Short(200))
        );
        assert_eq!(
            compound_field(&record.nbt, "MaxSpawnDelay"),
            Some(&Nbt::Short(800))
        );
        assert_eq!(compound_field(&record.nbt, "SpawnCount"), Some(&Nbt::Short(4)));
        assert_eq!(
            compound_field(&record.nbt, "MaxNearbyEntities"),
            Some(&Nbt::Short(6))
        );
        assert_eq!(
            compound_field(&record.nbt, "RequiredPlayerRange"),
            Some(&Nbt::Short(16))
        );
        assert_eq!(compound_field(&record.nbt, "SpawnRange"), Some(&Nbt::Short(4)));
        let spawn_data = compound_field(&record.nbt, "SpawnData").expect("spawner SpawnData");
        let expected_entity = Nbt::Compound(vec![(
            "id".to_owned(),
            Nbt::String("minecraft:cave_spider".to_owned()),
        )]);
        assert_eq!(
            compound_field(spawn_data, "entity"),
            Some(&expected_entity)
        );
    }
}

/// External packet captures stabilize the block-entity list by the compact
/// local XZ header, then Y and registry id. Keep this literal three-record
/// control because a source-side insertion sequence (chest, chest, spawner)
/// is not the packet's canonical sequence (chest, spawner, chest).
#[test]
fn generated_overworld_block_entities_use_stable_packet_order_and_nbt_keys() {
    let mut source = ChunkColumn::new(Dimension::Overworld.min_y(), Dimension::Overworld.height());
    source.set_block(2, -24, 12, "minecraft:chest[facing=east,type=single,waterlogged=false]");
    source.set_block(5, -24, 12, "minecraft:spawner");
    source.set_block(6, -24, 14, "minecraft:chest[facing=north,type=single,waterlogged=false]");

    let chest_a = GeneratedBlockEntity::DungeonChest {
        x: 34,
        y: -24,
        z: -20,
        facing: "east".to_owned(),
        loot_table: "minecraft:chests/simple_dungeon".to_owned(),
        loot_table_seed: 1,
    };
    let chest_b = GeneratedBlockEntity::DungeonChest {
        x: 38,
        y: -24,
        z: -18,
        facing: "north".to_owned(),
        loot_table: "minecraft:chests/simple_dungeon".to_owned(),
        loot_table_seed: 2,
    };
    let spawner = GeneratedBlockEntity::DungeonSpawner {
        x: 37,
        y: -24,
        z: -20,
        entity_type: lodestone_data::entity_type::EntityType::CaveSpider.into(),
    };
    // Deliberately use the generation insertion order; the encoder owns the
    // stable packet order and must not depend on this Vec order.
    source.add_generated_block_entities(&[chest_a, chest_b, spawner]);

    let packet = decode_encoded_chunk_at(2, -2, Dimension::Overworld, &source);
    let records = packet
        .block_entities
        .iter()
        .map(|record| (record.rel_x, record.y, record.rel_z, record.type_id))
        .collect::<Vec<_>>();
    assert_eq!(records, vec![(2, -24, 12, 1), (5, -24, 12, 9), (6, -24, 14, 1)]);
    assert_eq!(packet.block_entities[0].nbt, Nbt::End);
    assert_eq!(packet.block_entities[2].nbt, Nbt::End);

    let Nbt::Compound(fields) = &packet.block_entities[1].nbt else {
        panic!("generated spawner update must carry a compound payload");
    };
    let names = fields.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "Delay",
            "MaxNearbyEntities",
            "MaxSpawnDelay",
            "MinSpawnDelay",
            "RequiredPlayerRange",
            "SpawnCount",
            "SpawnData",
            "SpawnRange",
        ],
        "compound keys follow the external packet control's canonical UTF-8 order"
    );
}

#[test]
fn generated_beehive_and_dungeon_chest_sidecars_reach_the_overworld_packet() {
    let beehive = GeneratedBlockEntity::Beehive {
        x: 2,
        y: 70,
        z: 3,
        bees: vec![],
    };
    let chest = GeneratedBlockEntity::DungeonChest {
        x: 4,
        y: 70,
        z: 5,
        facing: "north".to_owned(),
        loot_table: "minecraft:chests/simple_dungeon".to_owned(),
        loot_table_seed: 17,
    };
    let mut source = ChunkColumn::new(Dimension::Overworld.min_y(), Dimension::Overworld.height());
    source.set_block(2, 70, 3, "minecraft:bee_nest[facing=north,honey_level=0]");
    source.set_block(4, 70, 5, "minecraft:chest[facing=north,type=single,waterlogged=false]");
    source.add_generated_block_entities(&[beehive, chest]);

    let packet = decode_encoded_chunk(Dimension::Overworld, &source);
    assert_eq!(packet.block_entities.len(), 2);
    let mut records = packet.block_entities.iter().collect::<Vec<_>>();
    records.sort_unstable_by_key(|record| record.type_id);

    assert_eq!(records[0].type_id, 1, "dungeon chest registry id");
    assert_eq!(records[0].nbt, Nbt::End);
    assert_eq!(records[1].type_id, 33, "beehive registry id");
    assert_eq!(records[1].nbt, Nbt::End);
}

#[test]
fn metadata_only_furnace_fallback_uses_the_literal_null_update_tag() {
    let position = BlockPos::new(2, 70, 2);
    let mut source = ChunkColumn::new(Dimension::Overworld.min_y(), Dimension::Overworld.height());
    source.set_block(
        position.x,
        position.y,
        position.z,
        "minecraft:furnace[facing=north,lit=false]",
    );
    source.set_block_entities(vec![
        (
            position,
            BlockEntity::Opaque {
                id: "minecraft:furnace".to_owned().into(),
                nbt: Nbt::Compound(vec![
                    (
                        "id".to_owned(),
                        Nbt::String("minecraft:furnace".to_owned()),
                    ),
                    ("x".to_owned(), Nbt::Int(position.x)),
                    ("y".to_owned(), Nbt::Int(position.y)),
                    ("z".to_owned(), Nbt::Int(position.z)),
                    ("keepPacked".to_owned(), Nbt::Byte(0)),
                    ("components".to_owned(), Nbt::Compound(Vec::new())),
                ]),
            },
        ),
    ]);

    let packet = decode_encoded_chunk(Dimension::Overworld, &source);
    assert_eq!(packet.block_entities.len(), 1);
    assert_eq!(packet.block_entities[0].type_id, 0, "furnace registry id");
    assert_eq!(packet.block_entities[0].nbt, Nbt::End);
}

#[test]
fn state_owned_potent_sulfur_uses_registry_slot_48_and_an_empty_update_tag() {
    let mut source = ChunkColumn::new(Dimension::Overworld.min_y(), Dimension::Overworld.height());
    source.set_block(4, 14, 1, "minecraft:potent_sulfur");
    source.populate_missing_block_entity_states(-25, -25);

    let packet = decode_encoded_chunk_at(-25, -25, Dimension::Overworld, &source);
    assert_eq!(packet.block_entities.len(), 1);
    let record = &packet.block_entities[0];
    assert_eq!((record.rel_x, record.y, record.rel_z), (4, 14, 1));
    assert_eq!(record.type_id, 48);
    assert_eq!(record.nbt, Nbt::End);

    // Negative control: the adjacent ordinary sulfur state owns no block
    // entity and must not create a trailing packet record.
    let mut control = ChunkColumn::new(Dimension::Overworld.min_y(), Dimension::Overworld.height());
    control.set_block(4, 14, 1, "minecraft:sulfur");
    control.populate_missing_block_entity_states(-25, -25);
    let packet = decode_encoded_chunk_at(-25, -25, Dimension::Overworld, &control);
    assert!(packet.block_entities.is_empty());
}

#[test]
fn end_gateway_sidecar_keeps_registry_id_and_destination_metadata() {
    let position = BlockPos::new(7, 80, 9);
    let exit = BlockPos::new(100, 50, 0);
    let mut source = ChunkColumn::new(Dimension::End.min_y(), Dimension::End.height());
    source.set_block(position.x, position.y, position.z, "minecraft:end_gateway");
    source.set_block_entities(vec![(
        position,
        BlockEntity::EndGateway {
            exit: Some(exit),
            exact: true,
        },
    )]);

    let packet = decode_encoded_chunk(Dimension::End, &source);
    assert_eq!(packet.block_entities.len(), 1);
    let record = &packet.block_entities[0];
    assert_eq!((record.rel_x, record.y, record.rel_z), (7, 80, 9));
    // Registry id 22 is the generated 26.2 End-gateway type id.
    assert_eq!(record.type_id, 22);
    assert!(compound_field(&record.nbt, "id").is_none());
    assert_eq!(compound_field(&record.nbt, "Age"), Some(&Nbt::Long(0)));
    assert_eq!(
        compound_field(&record.nbt, "exit_portal"),
        Some(&Nbt::IntArray(vec![100, 50, 0]))
    );
    assert_eq!(compound_field(&record.nbt, "ExactTeleport"), Some(&Nbt::Byte(1)));
}
