//! Decoration block entities in the columns the 26.3 production source serves.
//!
//! A monster room attaches a loot-seeded chest and a mob-typed spawner, and a tree's beehive
//! decorator attaches a bee nest with its bees. Each must exist as a block entity in the served
//! column, on the block that carries it, and an entity whose block a later feature replaced must
//! not be served.

use lodestone_server::{ChunkSource, WorldType, overworld_263_chunk_source_of_type};
use lodestone_worldgen::overworld::block_entities::GeneratedBlockEntity;

/// Chunk (-24, -33) of seed 42 holds a monster room (spawner and chest) and a bee nest.
const ROOM_CHUNK: (i32, i32) = (-24, -33);

fn block_at(column: &lodestone_server::ChunkColumn, x: i32, y: i32, z: i32) -> &'static str {
    column.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16)).name()
}

#[test]
fn served_columns_carry_the_decoration_block_entities() {
    let source = overworld_263_chunk_source_of_type(42, WorldType::Overworld);
    let (cx, cz) = ROOM_CHUNK;
    let (_, generated) = source.full_states_with_block_entities(cx, cz);
    let column = source.column(cx, cz);
    let (mut chests, mut spawners, mut hives) = (0, 0, 0);
    for entity in &generated {
        let (x, y, z) = entity.position();
        let (want, count) = match entity {
            GeneratedBlockEntity::DungeonChest { loot_table, loot_table_seed, .. } => {
                assert_eq!(loot_table, "minecraft:chests/simple_dungeon");
                assert_ne!(*loot_table_seed, 0, "the room drew a loot seed for the chest");
                ("minecraft:chest", &mut chests)
            }
            GeneratedBlockEntity::DungeonSpawner { .. } => ("minecraft:spawner", &mut spawners),
            GeneratedBlockEntity::Beehive { bees, .. } => {
                assert!((2..=3).contains(&bees.len()), "a hive holds two or three bees");
                ("minecraft:bee_nest", &mut hives)
            }
            GeneratedBlockEntity::EndGateway { .. } => panic!("the Overworld places no End gateway: {entity:?}"),
        };
        *count += 1;
        assert_eq!(block_at(&column, x, y, z), want, "{entity:?} must sit on its own block");
        assert!(
            column.block_entities().iter().any(|(p, _)| (p.x, p.y, p.z) == (x, y, z)),
            "{entity:?} is missing from the served column"
        );
    }
    assert!(chests >= 1 && spawners == 1 && hives >= 1, "chests {chests} spawners {spawners} hives {hives}");
}
