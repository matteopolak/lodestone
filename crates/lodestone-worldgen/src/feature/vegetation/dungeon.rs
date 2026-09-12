//! Underground dungeon placement.
//!
//! A dungeon is a small, sealed room carved into existing terrain. The room
//! carries two kinds of state that plain block writes cannot express: up to two
//! deferred-loot chests and one monster spawner. This module keeps the feature's
//! geometry, draw order and block-entity hand-off together so a caller cannot
//! accidentally add the blocks without the corresponding metadata.

use lodestone_data::entity_type::{EntityType, EntityTypeRef};

use crate::feature::{BlockPos, vegetation::config::is_air};
use crate::rng::RandomSource;

use super::{base_id, ConfiguredFeature, VegGrid, VegTags};
use crate::interner::StateId;

const DUNGEON_LOOT_TABLE: &str = "minecraft:chests/simple_dungeon";
const CAVE_AIR: &str = "minecraft:cave_air";
const COBBLESTONE: &str = "minecraft:cobblestone";
const MOSSY_COBBLESTONE: &str = "minecraft:mossy_cobblestone";
const CHEST: &str = "minecraft:chest";
const SPAWNER: &str = "minecraft:spawner";

/// Places one configured dungeon at `origin`.
///
/// The bounded coordinate draws for chest candidates, the bottom-wall moss
/// decisions, deferred-loot seeds and final spawner entity selection
/// intentionally stay in the same loops as their corresponding writes. A
/// failed room gate consumes no later draws, and a failed chest/spawner write
/// does not manufacture an entity for a block that was not placed.
pub(super) fn place_monster_room<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    _feature: &ConfiguredFeature,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let x_radius = random.next_int_bounded(2) + 2;
    let z_radius = random.next_int_bounded(2) + 2;
    let min_x = -x_radius - 1;
    let max_x = x_radius + 1;
    let min_y = -1;
    let max_y = 4;
    let min_z = -z_radius - 1;
    let max_z = z_radius + 1;

    // The room needs at least one and at most five two-block openings along
    // its horizontal shell. This is the feature's terrain/air acceptance gate;
    // it runs before any block write or later random draw.
    let mut openings = 0;
    for dx in min_x..=max_x {
        for dy in min_y..=max_y {
            for dz in min_z..=max_z {
                let pos = translated(origin, dx, dy, dz);
                let solid = is_solid(grid, tags, pos);
                if dy == min_y && !solid || dy == max_y && !solid {
                    return;
                }
                if (dx == min_x || dx == max_x || dz == min_z || dz == max_z)
                    && dy == 0
                    && is_empty(grid, pos)
                    && is_empty(grid, translated(pos, 0, 1, 0))
                {
                    openings += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&openings) {
        return;
    }

    // Carve the room from top to bottom, matching the feature's write order.
    // `max_y` was inclusive in the acceptance scan above, but is the exclusive
    // upper bound here: the dy=4 ceiling is solidity-check-only, and placement
    // writes dy=3..=-1. The shell is cobblestone, with a three-in-four chance
    // of mossy cobblestone on the bottom row.
    for dx in min_x..=max_x {
        for dy in (min_y..max_y).rev() {
            for dz in min_z..=max_z {
                let pos = translated(origin, dx, dy, dz);
                let current = base_at(grid, pos);
                let shell = dx == min_x
                    || dx == max_x
                    || dz == min_z
                    || dz == max_z
                    || dy == min_y
                    || dy == max_y;
                if shell {
                    if pos.y >= grid.min_y && !is_solid(grid, tags, translated(pos, 0, -1, 0)) {
                        // The source feature deliberately uses an unchecked air
                        // write for this branch; its protected-block predicate
                        // applies to the materialized wall branches below.
                        set_unchecked(grid, pos, CAVE_AIR);
                    } else if is_solid(grid, tags, pos) && current != CHEST {
                        let material = if dy == min_y && random.next_int_bounded(4) != 0 {
                            MOSSY_COBBLESTONE
                        } else {
                            COBBLESTONE
                        };
                        safe_set(grid, tags, pos, material);
                    }
                } else if current != CHEST && current != SPAWNER {
                    safe_set(grid, tags, pos, CAVE_AIR);
                }
            }
        }
    }

    // Try two chests, with up to three candidate positions for each. A chest
    // is accepted only when exactly one horizontal neighbour is solid; that
    // neighbour determines the chest's opposite-facing orientation.
    for _ in 0..2 {
        for _ in 0..3 {
            let chest_pos = BlockPos {
                x: origin.x + random.next_int_bounded(x_radius * 2 + 1) - x_radius,
                y: origin.y,
                z: origin.z + random.next_int_bounded(z_radius * 2 + 1) - z_radius,
            };
            if !is_empty(grid, chest_pos) {
                continue;
            }
            let mut solid_neighbour = None;
            for (dx, dz, facing) in [
                (0, -1, "south"),
                (1, 0, "west"),
                (0, 1, "north"),
                (-1, 0, "east"),
            ] {
                if is_solid(grid, tags, translated(chest_pos, dx, 0, dz)) {
                    if solid_neighbour.is_some() {
                        solid_neighbour = None;
                        break;
                    }
                    solid_neighbour = Some(facing);
                }
            }
            let Some(_) = solid_neighbour else {
                continue;
            };
            let facing = reorient_chest(grid, tags, chest_pos);
            let chest_state = chest_state(grid, facing);
            if safe_set_id(grid, tags, chest_pos, chest_state) {
                grid.push_block_entity(
                    crate::overworld::block_entities::GeneratedBlockEntity::DungeonChest {
                        x: chest_pos.x,
                        y: chest_pos.y,
                        z: chest_pos.z,
                        facing: facing.to_owned(),
                        loot_table: DUNGEON_LOOT_TABLE.to_owned(),
                        loot_table_seed: random.next_long(),
                    },
                );
                break;
            }
        }
    }

    let spawner_pos = origin;
    if safe_set(grid, tags, spawner_pos, SPAWNER) {
        let entity_type = match random.next_int_bounded(4) {
            0 => EntityType::Skeleton,
            1 | 2 => EntityType::Zombie,
            _ => EntityType::Spider,
        };
        grid.push_block_entity(
            crate::overworld::block_entities::GeneratedBlockEntity::DungeonSpawner {
                x: spawner_pos.x,
                y: spawner_pos.y,
                z: spawner_pos.z,
                entity_type: EntityTypeRef::from(entity_type),
            },
        );
    }
}

fn translated(pos: BlockPos, x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        x: pos.x + x,
        y: pos.y + y,
        z: pos.z + z,
    }
}

fn state_at(grid: &VegGrid, pos: BlockPos) -> &str {
    grid.interner().name_of(grid.get_id(pos.x, pos.y, pos.z))
}

fn base_at(grid: &VegGrid, pos: BlockPos) -> &str {
    base_id(state_at(grid, pos))
}

fn is_empty(grid: &VegGrid, pos: BlockPos) -> bool {
    is_air(base_at(grid, pos))
}

fn is_solid(grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> bool {
    tags.solid.test(state_at(grid, pos))
}

fn safe_set(grid: &mut VegGrid, tags: &VegTags, pos: BlockPos, state: &str) -> bool {
    safe_set_id(grid, tags, pos, grid.interner().id_of(state))
}

fn set_unchecked(grid: &mut VegGrid, pos: BlockPos, state: &str) {
    let state = grid.interner().id_of(state);
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
}

fn safe_set_id(grid: &mut VegGrid, tags: &VegTags, pos: BlockPos, state: StateId) -> bool {
    let current = base_at(grid, pos);
    if tags.features_cannot_replace.contains(current) {
        return false;
    }
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state)
}

fn chest_state(grid: &VegGrid, facing: &str) -> StateId {
    // The canonical state order is stable across the bundled block-state table.
    let state = match facing {
        "north" => "minecraft:chest[facing=north,type=single,waterlogged=false]",
        "south" => "minecraft:chest[facing=south,type=single,waterlogged=false]",
        "east" => "minecraft:chest[facing=east,type=single,waterlogged=false]",
        "west" => "minecraft:chest[facing=west,type=single,waterlogged=false]",
        _ => "minecraft:chest[facing=north,type=single,waterlogged=false]",
    };
    grid.interner().id_of(state)
}

fn reorient_chest(grid: &VegGrid, tags: &VegTags, pos: BlockPos) -> &'static str {
    // A neighbouring chest forces the default orientation. This branch is rare
    // but is observable when two candidate rolls land side by side.
    for (dx, dz, _) in [
        (0, -1, "north"),
        (1, 0, "east"),
        (0, 1, "south"),
        (-1, 0, "west"),
    ] {
        if base_at(grid, translated(pos, dx, 0, dz)) == CHEST {
            return "north";
        }
    }

    // The accepted candidate has exactly one collision-solid neighbour, so the
    // opposite-facing result is the normal path. The fallback below preserves
    // the block-state helper's deterministic behaviour if the render-solid
    // predicate disagrees with that coarse collision test.
    let render_solid = |dx: i32, dz: i32| {
        let state = grid.interner().name_of(grid.get_id(pos.x + dx, pos.y, pos.z + dz));
        if tags.simple_block_support.solid_render.is_empty() {
            is_solid(grid, tags, translated(pos, dx, 0, dz))
        } else {
            tags.simple_block_support.solid_render.test(state)
        }
    };
    let mut solid = None;
    for (dx, dz, facing) in [
        (0, -1, "south"),
        (1, 0, "west"),
        (0, 1, "north"),
        (-1, 0, "east"),
    ] {
        if render_solid(dx, dz) {
            if solid.is_some() {
                solid = None;
                break;
            }
            solid = Some(facing);
        }
    }
    if let Some(facing) = solid {
        return facing;
    }

    // The accepted candidate normally takes the branch above. This fallback
    // is still part of the production algorithm: it is reached when the
    // collision-solid and render-solid predicates disagree, or when a nearby
    // block is not render-solid despite being counted by the candidate gate.
    let mut lock = "north";
    if render_solid_at(grid, tags, pos, lock) {
        lock = opposite(lock);
    }
    if render_solid_at(grid, tags, pos, lock) {
        lock = clockwise(lock);
    }
    if render_solid_at(grid, tags, pos, lock) {
        lock = opposite(lock);
    }
    lock
}

fn render_solid_at(grid: &VegGrid, tags: &VegTags, pos: BlockPos, facing: &str) -> bool {
    let (dx, dz) = match facing {
        "north" => (0, -1),
        "south" => (0, 1),
        "east" => (1, 0),
        "west" => (-1, 0),
        _ => (0, 0),
    };
    let state = grid.interner().name_of(grid.get_id(pos.x + dx, pos.y, pos.z + dz));
    if tags.simple_block_support.solid_render.is_empty() {
        is_solid(grid, tags, translated(pos, dx, 0, dz))
    } else {
        tags.simple_block_support.solid_render.test(state)
    }
}

fn opposite(facing: &str) -> &'static str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        _ => "north",
    }
}

fn clockwise(facing: &str) -> &'static str {
    match facing {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        "west" => "north",
        _ => "north",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use super::*;
    use crate::density::{NoiseParams, Resolver};
    use crate::feature::top_layer::StatePredicate;
    use crate::rng::LegacyRandomSource;
    use serde_json::Value;

    const NON_SOLID_STATE: &str = "minecraft:ladder[facing=north,waterlogged=false]";

    struct EmptyResolver;

    impl Resolver for EmptyResolver {
        fn density_function(&self, _id: &str) -> Value {
            Value::Null
        }

        fn noise(&self, _id: &str) -> NoiseParams {
            unreachable!("the dungeon parser fixture has no noise references")
        }
    }

    fn room_grid(opening: bool) -> VegGrid {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in 3..14 {
            for y in 62..71 {
                for z in 3..14 {
                    grid.seed(x, y, z, "minecraft:stone".to_owned());
                }
            }
        }
        if opening {
            for x in [4, 5] {
                for y in [64, 65] {
                    grid.seed(x, y, 8, "minecraft:cave_air".to_owned());
                }
            }
        }
        grid
    }

    fn solid_tags() -> VegTags {
        let mut tags = VegTags::default();
        tags.solid = StatePredicate::new(
            HashSet::from([
                "minecraft:stone".to_owned(),
                "minecraft:cobblestone".to_owned(),
                "minecraft:mossy_cobblestone".to_owned(),
            ]),
            HashMap::from([(NON_SOLID_STATE.to_owned(), false)]),
        );
        tags
    }

    fn external_value(prefix: &str) -> String {
        include_str!("../../../tests/support/monster_room_feature_external.txt")
            .lines()
            .find_map(|line| line.strip_prefix(prefix).map(str::trim_start).map(str::to_owned))
            .unwrap_or_else(|| panic!("external monster-room fixture is missing {prefix:?}"))
    }

    fn fixture_grid(opening: bool) -> VegGrid {
        let mut grid = VegGrid::new(0, 128, -8, -8);
        for x in -8..=8 {
            for y in 0..128 {
                for z in -8..=8 {
                    grid.seed(x, y, z, "minecraft:stone".to_owned());
                }
            }
        }
        if opening {
            grid.seed(-4, 64, 0, "minecraft:cave_air".to_owned());
            grid.seed(-4, 65, 0, "minecraft:cave_air".to_owned());
        }
        grid
    }

    fn fixture_digest(cells: &BTreeMap<String, String>) -> u64 {
        let mut hash = 1_469_598_103_934_665_603u64;
        for (position, state) in cells {
            for byte in format!("{position} {state}\n").bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        hash
    }

    fn final_cells(grid: &VegGrid) -> BTreeMap<String, String> {
        grid.dirty_cells()
            .map(|(x, y, z, state)| (format!("{x},{y},{z}"), state.to_owned()))
            .collect()
    }

    #[test]
    fn compiled_runtime_fixture_matches_room_geometry_and_entities() {
        let tags = solid_tags();
        let mut grid = fixture_grid(true);
        let mut random = LegacyRandomSource::new(19);
        place_monster_room(
            &mut random,
            BlockPos { x: 0, y: 64, z: 0 },
            &ConfiguredFeature::MonsterRoom,
            &mut grid,
            &tags,
        );

        let cells = final_cells(&grid);
        assert_eq!(external_value("positive.result"), "true");
        assert_eq!(cells.len().to_string(), external_value("positive.writes"));
        assert_eq!(format!("{:x}", fixture_digest(&cells)), external_value("positive.digest"));
        for (state, expected_count) in [
            ("minecraft:cave_air", "139"),
            ("minecraft:chest[facing=north,type=single,waterlogged=false]", "1"),
            ("minecraft:chest[facing=west,type=single,waterlogged=false]", "1"),
            ("minecraft:cobblestone", "122"),
            ("minecraft:mossy_cobblestone", "50"),
            ("minecraft:spawner", "1"),
        ] {
            let actual = cells.values().filter(|actual| actual.as_str() == state).count();
            assert_eq!(actual.to_string(), external_value(&format!("positive.count {state}")));
            assert_eq!(actual.to_string(), expected_count);
        }
        for (position, state) in [
            ("0,64,0", "minecraft:spawner"),
            ("0,64,2", "minecraft:chest[facing=north,type=single,waterlogged=false]"),
            ("3,64,1", "minecraft:chest[facing=west,type=single,waterlogged=false]"),
            ("-4,65,0", "minecraft:cave_air"),
        ] {
            assert_eq!(cells.get(position).map(String::as_str), Some(state));
            assert_eq!(external_value(&format!("positive.cell {position}")), state);
        }

        let entities = grid.take_block_entities();
        let mut chest_count = 0;
        let mut spawner_count = 0;
        for entity in entities {
            match entity {
                crate::overworld::block_entities::GeneratedBlockEntity::DungeonChest {
                    x,
                    y,
                    z,
                    facing,
                    loot_table,
                    loot_table_seed,
                } => {
                    chest_count += 1;
                    let prefix = format!("positive.entity.chest {x},{y},{z} {facing}");
                    let expected = external_value(&prefix);
                    let expected_seed = expected
                        .rsplit_once(' ')
                        .expect("external chest row includes a loot seed")
                        .1
                        .parse::<i64>()
                        .expect("external chest loot seed");
                    assert_eq!(loot_table, "minecraft:chests/simple_dungeon");
                    assert_eq!(loot_table_seed, expected_seed);
                    assert!(expected.contains("ResourceKey[minecraft:loot_table / minecraft:chests/simple_dungeon]"));
                }
                crate::overworld::block_entities::GeneratedBlockEntity::DungeonSpawner { x, y, z, .. } => {
                    spawner_count += 1;
                    assert_eq!(external_value(&format!("positive.entity.spawner {x},{y},{z}")), "");
                }
                _ => {}
            }
        }
        assert_eq!(chest_count, 2);
        assert_eq!(spawner_count, 1);
    }

    #[test]
    fn sealed_external_control_rejects_room_before_any_write() {
        let tags = solid_tags();
        let mut grid = fixture_grid(false);
        let mut random = LegacyRandomSource::new(19);
        place_monster_room(
            &mut random,
            BlockPos { x: 0, y: 64, z: 0 },
            &ConfiguredFeature::MonsterRoom,
            &mut grid,
            &tags,
        );
        assert_eq!(grid.dirty_len(), 0);
        assert!(grid.take_block_entities().is_empty());
        assert_eq!(external_value("control.sealed result=false writes="), "0");
    }

    #[test]
    fn monster_room_document_is_modelled_and_unknown_feature_stays_unsupported() {
        let resolver = EmptyResolver;
        let feature = super::super::config::parse_configured_feature_doc(
            &resolver,
            &serde_json::json!({"type": "minecraft:monster_room", "config": {}}),
        );
        assert!(matches!(feature, ConfiguredFeature::MonsterRoom));

        let unknown = super::super::config::parse_configured_feature_doc(
            &resolver,
            &serde_json::json!({"type": "minecraft:not_a_dungeon", "config": {}}),
        );
        assert!(matches!(unknown, ConfiguredFeature::Unsupported(reason) if reason == "not_a_dungeon"));
    }

    #[test]
    fn monster_room_carves_entities_and_rejects_a_sealed_control() {
        let tags = solid_tags();
        let mut sealed = room_grid(false);
        let mut sealed_random = LegacyRandomSource::new(1);
        place_monster_room(
            &mut sealed_random,
            BlockPos { x: 8, y: 64, z: 8 },
            &ConfiguredFeature::MonsterRoom,
            &mut sealed,
            &tags,
        );
        assert_eq!(sealed.dirty_len(), 0, "a room without openings must be rejected");
        assert!(sealed.take_block_entities().is_empty());

        let mut generated = None;
        for seed in 0..128 {
            let mut grid = room_grid(true);
            let mut random = LegacyRandomSource::new(seed);
            place_monster_room(
                &mut random,
                BlockPos { x: 8, y: 64, z: 8 },
                &ConfiguredFeature::MonsterRoom,
                &mut grid,
                &tags,
            );
            let has_chest = grid
                .take_block_entities()
                .into_iter()
                .any(|entity| matches!(entity, crate::overworld::block_entities::GeneratedBlockEntity::DungeonChest { .. }));
            if has_chest {
                generated = Some(grid);
                break;
            }
        }
        let grid = generated.expect("at least one deterministic room seed must roll a chest");
        assert!(
            (3..14).any(|x| (3..14).any(|z| base_at(&grid, BlockPos { x, y: 64, z }) == SPAWNER)),
            "accepted room must place its spawner"
        );
        assert_eq!(
            base_at(&grid, BlockPos { x: 8, y: 68, z: 8 }),
            "minecraft:stone",
            "the top scan row is a gate only and must not be carved"
        );
        assert!(
            grid.dirty_cells().all(|(_, y, _, _)| y != 68),
            "placement must not write the scan-only top row"
        );

        // This state has a collision shape that looks substantial but is
        // canonically non-solid. The old base-name helper would call its
        // `minecraft:ladder` base solid; the exact state census must reject it
        // when it occupies the scan-only ceiling row.
        assert!(super::super::config::blocks_motion(base_id(NON_SOLID_STATE)));
        let mut non_solid = room_grid(true);
        non_solid.seed(8, 68, 8, NON_SOLID_STATE.to_owned());
        let mut non_solid_random = LegacyRandomSource::new(1);
        place_monster_room(
            &mut non_solid_random,
            BlockPos { x: 8, y: 64, z: 8 },
            &ConfiguredFeature::MonsterRoom,
            &mut non_solid,
            &tags,
        );
        assert_eq!(
            non_solid.dirty_len(),
            0,
            "a non-solid property state must fail the room gate"
        );
        assert!(non_solid.take_block_entities().is_empty());
    }
}
