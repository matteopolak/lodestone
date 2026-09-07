//! Cave root-column configured-feature placement.
//!
//! A root-system call first finds a viable elevated candidate, runs its nested
//! placed feature on the same random stream, then scatters replacement roots
//! through the intervening column and hanging roots around the original
//! position. The nested placement is a callback because it belongs to the
//! parent configured-feature dispatcher; this module must not create a second
//! feature-routing path or re-seed the caller's generator.

use std::collections::HashSet;

use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::config::{
    BlockPredicate, BlockStateProvider, PlacedRef, VegTags, blocks_motion, is_air, is_fluid,
};
use super::grid::VegGrid;

/// Parsed configuration for the cave root-column feature.
///
/// `feature` remains a placed-feature reference so its modifiers run at the
/// selected elevated candidate, in the same way they do when reached directly
/// from a biome's decoration list.
#[derive(Clone, Debug)]
pub struct RootSystemCfg {
    pub feature: PlacedRef,
    pub required_vertical_space_for_tree: i32,
    pub level_test_distance: i32,
    pub max_level_deviation: i32,
    pub root_radius: i32,
    pub root_replaceable: HashSet<String>,
    pub root_state_provider: BlockStateProvider,
    pub root_placement_attempts: i32,
    pub root_column_max_height: i32,
    pub hanging_root_radius: i32,
    pub hanging_roots_vertical_span: i32,
    pub hanging_root_state_provider: BlockStateProvider,
    pub hanging_root_placement_attempts: i32,
    pub allowed_vertical_water_for_tree: i32,
    pub allowed_tree_position: BlockPredicate,
}

/// Places one root system, returning the feature body's unconditional success
/// result. An occupied outer origin is the one false result; every other
/// attempted placement reports success even when no nested feature lands.
///
/// `place_nested` is deliberately supplied by the vegetation dispatcher. It
/// receives the exact mutable `random` passed to this function, rather than a
/// derived or re-seeded source, so a successful nested placement and both root
/// passes retain their one shared draw sequence.
pub(super) fn place_root_system<R, F>(
    random: &mut R,
    origin: BlockPos,
    cfg: &RootSystemCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
    mut place_nested: F,
) -> bool
where
    R: RandomSource,
    F: FnMut(&mut R, BlockPos, &PlacedRef, &mut VegGrid, &VegTags),
{
    if !air_at(grid, origin) {
        return false;
    }

    let Some(target_height) = find_and_place_nested(
        random,
        origin,
        cfg,
        grid,
        tags,
        &mut place_nested,
    ) else {
        return true;
    };

    place_column_roots(random, origin, target_height, cfg, grid, tags);
    place_hanging_roots(random, origin, cfg, grid, tags);
    true
}

fn find_and_place_nested<R, F>(
    random: &mut R,
    origin: BlockPos,
    cfg: &RootSystemCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
    place_nested: &mut F,
) -> Option<i32>
where
    R: RandomSource,
    F: FnMut(&mut R, BlockPos, &PlacedRef, &mut VegGrid, &VegTags),
{
    for y in 0..cfg.root_column_max_height {
        let candidate = BlockPos {
            x: origin.x,
            y: origin.y + y + 1,
            z: origin.z,
        };
        if grid.height_world_surface(candidate.x, candidate.z) < candidate.y {
            return None;
        }
        if !cfg.allowed_tree_position.test(grid, tags, candidate)
            || !space_for_nested_feature(candidate, cfg, grid)
        {
            continue;
        }

        let below = BlockPos {
            x: candidate.x,
            y: candidate.y - 1,
            z: candidate.z,
        };
        let below_base = base_at(grid, below);
        if below_base == "minecraft:lava" || !solid_at(grid, below) {
            return None;
        }

        // The nested feature's own boolean success is not available through
        // the generic dispatcher. A dirty-overlay delta is the local success
        // signal: it correctly admits cross-chunk writes that land inside this
        // caller-owned region and rejects a no-op nested feature.
        let writes_before = grid.dirty_cells().count();
        place_nested(random, candidate, &cfg.feature, grid, tags);
        if grid.dirty_cells().count() != writes_before {
            return Some(origin.y + y);
        }
        return None;
    }
    None
}

fn place_column_roots<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    target_height: i32,
    cfg: &RootSystemCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    for y in origin.y..target_height {
        for _ in 0..cfg.root_placement_attempts {
            // These are four independent draws per attempt. Keep this shape:
            // sampling a combined offset changes every following provider and
            // feature draw.
            let pos = BlockPos {
                x: origin.x + random.next_int_bounded(cfg.root_radius)
                    - random.next_int_bounded(cfg.root_radius),
                y,
                z: origin.z + random.next_int_bounded(cfg.root_radius)
                    - random.next_int_bounded(cfg.root_radius),
            };
            if !cfg.root_replaceable.contains(base_at(grid, pos)) {
                continue;
            }
            if let Some(state) = cfg.root_state_provider.get_state_id(grid, tags, random, pos) {
                grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
            }
        }
    }
}

fn place_hanging_roots<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &RootSystemCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    for _ in 0..cfg.hanging_root_placement_attempts {
        let pos = BlockPos {
            x: origin.x + random.next_int_bounded(cfg.hanging_root_radius)
                - random.next_int_bounded(cfg.hanging_root_radius),
            y: origin.y + random.next_int_bounded(cfg.hanging_roots_vertical_span)
                - random.next_int_bounded(cfg.hanging_roots_vertical_span),
            z: origin.z + random.next_int_bounded(cfg.hanging_root_radius)
                - random.next_int_bounded(cfg.hanging_root_radius),
        };
        if !air_at(grid, pos) {
            continue;
        }

        // State selection precedes its survival test. In particular, a
        // weighted provider still consumes its draw at an unsupported ceiling;
        // moving this call after `hanging_state_can_survive` shifts every later
        // attempt and is externally observable.
        let Some(state) = cfg
            .hanging_root_state_provider
            .get_state_id(grid, tags, random, pos)
        else {
            continue;
        };
        if hanging_state_can_survive(grid, state, pos) {
            grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
        }
    }
}

/// The state family reachable from the bundled root-system records either
/// hangs from a sturdy ceiling or has no placement support requirement. The
/// latter is represented by the sulfur spring's provider. New state families
/// need their own survival rule here; accepting one by default would make a
/// newly added provider silently place through an invalid ceiling.
fn hanging_state_can_survive(
    grid: &VegGrid,
    state: crate::interner::StateId,
    pos: BlockPos,
) -> bool {
    match super::base_id(grid.interner().name_of(state)) {
        "minecraft:hanging_roots" => solid_at(
            grid,
            BlockPos {
                x: pos.x,
                y: pos.y + 1,
                z: pos.z,
            },
        ),
        "minecraft:sulfur" => true,
        _ => false,
    }
}

fn space_for_nested_feature(pos: BlockPos, cfg: &RootSystemCfg, grid: &VegGrid) -> bool {
    for distance in 1..=cfg.required_vertical_space_for_tree {
        let at = BlockPos {
            x: pos.x,
            y: pos.y + distance,
            z: pos.z,
        };
        if !allowed_tree_space(base_at(grid, at), distance, cfg.allowed_vertical_water_for_tree) {
            return false;
        }
    }

    if cfg.level_test_distance == 0 {
        return true;
    }
    for (dx, dz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let probe_x = pos.x + dx * cfg.level_test_distance;
        let probe_z = pos.z + dz * cfg.level_test_distance;
        if air_at(
            grid,
            BlockPos {
                x: probe_x,
                y: pos.y - cfg.max_level_deviation,
                z: probe_z,
            },
        ) || !air_at(
            grid,
            BlockPos {
                x: probe_x,
                y: pos.y + cfg.max_level_deviation,
                z: probe_z,
            },
        ) {
            return false;
        }
    }
    true
}

fn allowed_tree_space(base: &str, distance: i32, allowed_water_height: i32) -> bool {
    is_air(base)
        || (distance + 1 <= allowed_water_height && (base == "minecraft:water"))
}

fn base_at(grid: &VegGrid, pos: BlockPos) -> &str {
    super::base_id(grid.get(pos.x, pos.y, pos.z))
}

fn air_at(grid: &VegGrid, pos: BlockPos) -> bool {
    is_air(base_at(grid, pos))
}

/// The root column's support and hanging-root ceiling both need a full solid
/// surface. The grid only carries base ids, so its existing motion capability
/// is the narrowest reusable approximation of that state query.
fn solid_at(grid: &VegGrid, pos: BlockPos) -> bool {
    let base = base_at(grid, pos);
    !is_air(base) && !is_fluid(base) && blocks_motion(base)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use crate::LegacyRandomSource;
    use crate::feature::BlockPos;

    use super::*;
    use super::super::config::{BlockPredicate, BlockStateProvider, ConfiguredFeature, PlacedRef};

    fn fixture_grid(origin_state: &str) -> VegGrid {
        fixture_grid_at(0, origin_state)
    }

    fn fixture_grid_at(origin_x: i32, origin_state: &str) -> VegGrid {
        let mut grid = VegGrid::with_footprint(-64, 384, 0, 0, -16, 32);
        for x in -16..32 {
            for y in -64..=64 {
                for z in -16..32 {
                    grid.seed(x, y, z, "minecraft:stone".to_string());
                }
            }
        }
        for x in origin_x - 3..=origin_x + 3 {
            for z in -3..=3 {
                grid.seed(x, 62, z, "minecraft:air".to_string());
            }
        }
        grid.seed(origin_x, 63, 0, origin_state.to_string());
        grid
    }

    fn fixture_cfg() -> RootSystemCfg {
        RootSystemCfg {
            feature: PlacedRef {
                registry_id: None,
                placements: Vec::new(),
                feature: Box::new(ConfiguredFeature::SimpleBlock(BlockStateProvider::Simple(
                    "minecraft:oak_log".to_string(),
                ))),
            },
            required_vertical_space_for_tree: 3,
            level_test_distance: 0,
            max_level_deviation: 0,
            root_radius: 3,
            root_replaceable: HashSet::from(["minecraft:stone".to_string()]),
            root_state_provider: BlockStateProvider::Simple("minecraft:rooted_dirt".to_string()),
            root_placement_attempts: 20,
            root_column_max_height: 8,
            hanging_root_radius: 3,
            hanging_roots_vertical_span: 2,
            hanging_root_state_provider: BlockStateProvider::Simple(
                "minecraft:hanging_roots".to_string(),
            ),
            hanging_root_placement_attempts: 20,
            allowed_vertical_water_for_tree: 2,
            allowed_tree_position: BlockPredicate::MatchingBlocks {
                blocks: vec!["minecraft:air".to_string()],
                offset: (0, 0, 0),
            },
        }
    }

    fn fixture_tags(grid: &VegGrid) -> VegTags {
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert("minecraft:stone".to_string());
        tags.bind(grid.interner());
        tags
    }

    #[test]
    fn compiled_runtime_fixture_matches_column_roots_and_hanging_roots() {
        let fixture = include_str!("../../../tests/support/root_system_jvm.txt");
        let expected: BTreeMap<_, _> = fixture
            .lines()
            .filter_map(|line| {
                let mut words = line.splitn(3, ' ');
                let row = words.next()?;
                let pos = words.next()?;
                let state = words.next()?;
                (row == "normal").then_some((pos.to_string(), state.to_string()))
            })
            .collect();
        assert!(
            expected.keys().any(|pos| pos.contains(",62,")),
            "external fixture must exercise a hanging root below the root column"
        );

        let mut grid = fixture_grid("minecraft:air");
        let tags = fixture_tags(&grid);
        let mut random = LegacyRandomSource::new(19);
        let result = place_root_system(
            &mut random,
            BlockPos { x: 0, y: 63, z: 0 },
            &fixture_cfg(),
            &mut grid,
            &tags,
            |_, pos, _, grid, _| {
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, "minecraft:oak_log".to_string());
            },
        );
        let got: BTreeMap<_, _> = grid
            .dirty_cells()
            .map(|(x, y, z, state)| (format!("{x},{y},{z}"), state.to_string()))
            .collect();
        assert!(result);
        assert_eq!(got, expected, "root-system placement diverged from the external prediction");
    }

    #[test]
    fn occupied_outer_origin_is_a_negative_control_with_no_nested_call_or_draws() {
        let fixture = include_str!("../../../tests/support/root_system_jvm.txt");
        assert!(
            fixture.contains("blocked 0,63,0 minecraft:stone"),
            "external fixture must retain the occupied-origin negative control"
        );
        let mut grid = fixture_grid("minecraft:stone");
        let tags = fixture_tags(&grid);
        let mut random = LegacyRandomSource::new(19);
        let mut nested_calls = 0;
        assert!(!place_root_system(
            &mut random,
            BlockPos { x: 0, y: 63, z: 0 },
            &fixture_cfg(),
            &mut grid,
            &tags,
            |_, _, _, _, _| nested_calls += 1,
        ));
        assert_eq!(nested_calls, 0);
        assert_eq!(grid.dirty_cells().count(), 0);
        assert_eq!(random.next_int(), LegacyRandomSource::new(19).next_int());
    }

    #[test]
    fn root_writes_cross_the_source_chunk_edge_when_the_region_owns_the_neighbour() {
        let mut grid = fixture_grid_at(15, "minecraft:air");
        let tags = fixture_tags(&grid);
        let mut random = LegacyRandomSource::new(19);
        assert!(place_root_system(
            &mut random,
            BlockPos { x: 15, y: 63, z: 0 },
            &fixture_cfg(),
            &mut grid,
            &tags,
            |_, pos, _, grid, _| {
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, "minecraft:oak_log".to_string());
            },
        ));
        assert!(
            grid.dirty_cells().any(|(x, _, _, _)| x >= 16),
            "the region overlay must retain roots that cross the source chunk edge"
        );
    }
}
