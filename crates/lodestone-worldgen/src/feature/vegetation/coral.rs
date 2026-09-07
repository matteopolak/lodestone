//! The three configured coral feature bodies.
//!
//! ## What it is
//!
//! This module places the coral tree, claw, and mushroom geometries used by
//! warm-ocean decoration. The bodies share one coral-block choice and one
//! water/survival gate, while retaining their individual geometry and random
//! draw order.
//!
//! ## How it works
//!
//! [`place_coral`] consumes the caller-owned random stream. It never reseeds
//! or forks that stream. A successful coral block may decorate its cell above
//! with a coral plant or sea pickle, then independently attempts a wall fan in
//! each horizontal direction. Every write goes through [`VegGrid`], so writes
//! outside a caller's footprint are clipped while reads retain the grid's
//! normal clamped-read behavior.
//!
//! ## How to change it
//!
//! Keep the shared helper's draw order stable: select the coral block first;
//! for each placed block draw the plant/pickle arm before the four wall-fan
//! chances. Geometry loops must preserve their reference iteration order.
//! Extend [`CoralKind`] and the dispatch/parser seam in the parent vegetation
//! module when adding another body.
//!
//! ## Configuration
//!
//! These are no-configuration features. Their configured-feature identifiers
//! are `coral_tree`, `coral_claw`, and `coral_mushroom`; the parent parser maps
//! those identifiers to [`CoralKind`].
//!
//! ## Dependencies
//!
//! The module depends only on the vegetation [`VegGrid`] and the shared
//! [`RandomSource`] contract. Coral state names and their canonical properties
//! are fixed by the bundled block-tag data for protocol 776.

use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::grid::VegGrid;

/// The three configured coral geometries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoralKind {
    Tree,
    Claw,
    Mushroom,
}

/// The five states in the coral-block tag, in registry order.
const CORAL_BLOCKS: [&str; 5] = [
    "minecraft:tube_coral_block",
    "minecraft:brain_coral_block",
    "minecraft:bubble_coral_block",
    "minecraft:fire_coral_block",
    "minecraft:horn_coral_block",
];

/// The ten states in the coral tag that may decorate a block's top face. The
/// registry's plant and fan states all receive the explicit
/// `waterlogged=true` property when placed in a water-filled feature.
const CORALS: [&str; 10] = [
    "minecraft:tube_coral",
    "minecraft:brain_coral",
    "minecraft:bubble_coral",
    "minecraft:fire_coral",
    "minecraft:horn_coral",
    "minecraft:tube_coral_fan",
    "minecraft:brain_coral_fan",
    "minecraft:bubble_coral_fan",
    "minecraft:fire_coral_fan",
    "minecraft:horn_coral_fan",
];

/// The five states in the wall-corals tag.
const WALL_CORALS: [&str; 5] = [
    "minecraft:tube_coral_wall_fan",
    "minecraft:brain_coral_wall_fan",
    "minecraft:bubble_coral_wall_fan",
    "minecraft:fire_coral_wall_fan",
    "minecraft:horn_coral_wall_fan",
];

/// The reference horizontal iteration order is north, east, south, west. The
/// tuple is `(x, z)`.
const HORIZONTAL: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Places one configured coral feature using the caller's current random
/// stream. The initial coral-block selection is consumed even when the origin
/// later fails its water gate, matching the feature's outer placement body.
pub(super) fn place_coral<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    kind: CoralKind,
    grid: &mut VegGrid,
) {
    let state = CORAL_BLOCKS[random.next_int_bounded(CORAL_BLOCKS.len() as i32) as usize];
    match kind {
        CoralKind::Tree => place_tree(random, origin, state, grid),
        CoralKind::Claw => place_claw(random, origin, state, grid),
        CoralKind::Mushroom => place_mushroom(random, origin, state, grid),
    }
}

/// Places one coral block and its decorations. A target may be water or one
/// of the ordinary coral states, but the block immediately above must be water.
/// Wall fans are attempted in all four directions in fixed order; only a water
/// neighbor can receive the selected wall-fan state.
fn place_coral_block<R: RandomSource>(
    random: &mut R,
    at: BlockPos,
    state: &str,
    grid: &mut VegGrid,
) -> bool {
    let here = base_at(grid, at.x, at.y, at.z);
    if !(here == "minecraft:water" || CORALS.iter().any(|name| *name == here))
        || !water_at(grid, at.x, at.y + 1, at.z)
    {
        return false;
    }

    grid.set_if_in_bounds(at.x, at.y, at.z, state.to_owned());

    if random.next_float() < 0.25 {
        let coral = CORALS[random.next_int_bounded(CORALS.len() as i32) as usize];
        grid.set_if_in_bounds(at.x, at.y + 1, at.z, coral_state(coral));
    } else if random.next_float() < 0.05 {
        let pickles = random.next_int_bounded(4) + 1;
        grid.set_if_in_bounds(
            at.x,
            at.y + 1,
            at.z,
            format!("minecraft:sea_pickle[pickles={pickles},waterlogged=true]"),
        );
    }

    for (index, &(dx, dz)) in HORIZONTAL.iter().enumerate() {
        if random.next_float() < 0.2 && water_at(grid, at.x + dx, at.y, at.z + dz) {
            let wall = WALL_CORALS[random.next_int_bounded(WALL_CORALS.len() as i32) as usize];
            let facing = ["north", "east", "south", "west"][index];
            grid.set_if_in_bounds(
                at.x + dx,
                at.y,
                at.z + dz,
                format!("{wall}[facing={facing},waterlogged=true]"),
            );
        }
    }
    true
}

fn base_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> &str {
    super::base_id(grid.get(x, y, z))
}

fn water_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    base_at(grid, x, y, z) == "minecraft:water"
}

fn coral_state(name: &str) -> String {
    format!("{name}[waterlogged=true]")
}

fn shuffle<R: RandomSource>(random: &mut R, directions: &mut [(i32, i32)]) {
    for index in (1..directions.len()).rev() {
        let selected = random.next_int_bounded((index + 1) as i32) as usize;
        directions.swap(index, selected);
    }
}

fn place_tree<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    state: &str,
    grid: &mut VegGrid,
) {
    let trunk_height = random.next_int_bounded(3) + 1;
    for offset in 0..trunk_height {
        if !place_coral_block(
            random,
            BlockPos {
                x: origin.x,
                y: origin.y + offset,
                z: origin.z,
            },
            state,
            grid,
        ) {
            return;
        }
    }

    let trunk_top = BlockPos {
        x: origin.x,
        y: origin.y + trunk_height,
        z: origin.z,
    };
    let branch_count = random.next_int_bounded(3) + 2;
    let mut directions = HORIZONTAL;
    shuffle(random, &mut directions);

    for &(dx, dz) in directions.iter().take(branch_count as usize) {
        let mut x = trunk_top.x + dx;
        let mut y = trunk_top.y;
        let mut z = trunk_top.z + dz;
        let branch_height = random.next_int_bounded(5) + 2;
        let mut segment_length = 0;

        for step in 0..branch_height {
            if !place_coral_block(random, BlockPos { x, y, z }, state, grid) {
                break;
            }
            segment_length += 1;
            y += 1;
            if step == 0 || (segment_length >= 2 && random.next_float() < 0.25) {
                x += dx;
                z += dz;
                segment_length = 0;
            }
        }
    }
}

fn place_claw<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    state: &str,
    grid: &mut VegGrid,
) {
    if !place_coral_block(random, origin, state, grid) {
        return;
    }

    let claw_index = random.next_int_bounded(4) as usize;
    let claw_direction = HORIZONTAL[claw_index];
    let branch_count = random.next_int_bounded(2) + 2;
    let mut directions = [
        claw_direction,
        HORIZONTAL[(claw_index + 1) % 4],
        HORIZONTAL[(claw_index + 3) % 4],
    ];
    shuffle(random, &mut directions);

    for &branch_direction in directions.iter().take(branch_count as usize) {
        let mut x = origin.x + branch_direction.0;
        let mut y = origin.y;
        let mut z = origin.z + branch_direction.1;
        let sideway_length = random.next_int_bounded(2) + 1;
        let (segment_direction, inway_length) = if branch_direction == claw_direction {
            (claw_direction, random.next_int_bounded(3) + 2)
        } else {
            y += 1;
            let segment_direction = if random.next_int_bounded(2) == 0 {
                branch_direction
            } else {
                (0, 0)
            };
            (segment_direction, random.next_int_bounded(3) + 3)
        };

        for _ in 0..sideway_length {
            if !place_coral_block(random, BlockPos { x, y, z }, state, grid) {
                break;
            }
            x += segment_direction.0;
            y += i32::from(segment_direction == (0, 0));
            z += segment_direction.1;
        }

        x -= segment_direction.0;
        y -= i32::from(segment_direction == (0, 0));
        z -= segment_direction.1;
        y += 1;

        for _ in 0..inway_length {
            x += claw_direction.0;
            z += claw_direction.1;
            if !place_coral_block(random, BlockPos { x, y, z }, state, grid) {
                break;
            }
            if random.next_float() < 0.25 {
                y += 1;
            }
        }
    }
}

fn place_mushroom<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    state: &str,
    grid: &mut VegGrid,
) {
    let height = random.next_int_bounded(3) + 3;
    let width = random.next_int_bounded(3) + 3;
    let length = random.next_int_bounded(3) + 3;
    let sink = random.next_int_bounded(3) + 1;

    for x in 0..=width {
        for y in 0..=height {
            for z in 0..=length {
                let inner_x = x != 0 && x != width;
                let inner_y = y != 0 && y != height;
                let inner_z = z != 0 && z != length;
                let shell = x == 0 || x == width || y == 0 || y == height || z == 0 || z == length;
                if (inner_x || inner_y)
                    && (inner_z || inner_y)
                    && (inner_x || inner_z)
                    && shell
                    && random.next_float() >= 0.1
                {
                    let _ = place_coral_block(
                        random,
                        BlockPos {
                            x: origin.x + x,
                            y: origin.y + y - sink,
                            z: origin.z + z,
                        },
                        state,
                        grid,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::rng::LegacyRandomSource;

    fn water_grid(local_lo: i32, local_hi: i32) -> VegGrid {
        let mut grid = VegGrid::with_footprint(-64, 384, 0, 0, local_lo, local_hi);
        for x in -16..32 {
            for y in 0..96 {
                for z in -16..32 {
                    grid.seed(x, y, z, "minecraft:water".to_owned());
                }
            }
        }
        grid
    }

    fn fixture_rows(label: &str) -> BTreeMap<String, String> {
        include_str!("../../../tests/support/coral_feature_jvm.txt")
            .lines()
            .filter_map(|line| {
                let mut words = line.splitn(3, ' ');
                let row = words.next()?;
                let position = words.next()?;
                let state = words.next()?;
                (row == label).then_some((position.to_owned(), state.to_owned()))
            })
            .collect()
    }

    #[test]
    fn compiled_server_geometry_and_state_properties_match() {
        for (label, kind) in [
            ("tree.11", CoralKind::Tree),
            ("claw.11", CoralKind::Claw),
            ("mushroom.11", CoralKind::Mushroom),
        ] {
            let expected = fixture_rows(label);
            assert!(!expected.is_empty(), "fixture must exercise {label}");
            let mut grid = water_grid(-16, 32);
            let mut random = LegacyRandomSource::new(11);
            place_coral(
                &mut random,
                BlockPos { x: 0, y: 64, z: 0 },
                kind,
                &mut grid,
            );
            let actual: BTreeMap<_, _> = grid
                .dirty_cells()
                .map(|(x, y, z, state)| (format!("{x},{y},{z}"), state.to_owned()))
                .collect();
            assert_eq!(actual, expected, "changed {label} geometry or draw order");
        }
    }

    #[test]
    fn water_origin_and_above_are_exact_survival_controls() {
        let mut positive = water_grid(0, 16);
        let mut random = LegacyRandomSource::new(11);
        place_coral(
            &mut random,
            BlockPos { x: 8, y: 64, z: 8 },
            CoralKind::Claw,
            &mut positive,
        );
        assert!(positive.dirty_cells().count() > 0, "water/water control must place coral");

        let mut dry_origin = water_grid(0, 16);
        dry_origin.seed(8, 64, 8, "minecraft:air".to_owned());
        let mut random = LegacyRandomSource::new(11);
        place_coral(
            &mut random,
            BlockPos { x: 8, y: 64, z: 8 },
            CoralKind::Claw,
            &mut dry_origin,
        );
        assert_eq!(dry_origin.dirty_cells().count(), 0, "air origin must reject coral");

        let mut dry_above = water_grid(0, 16);
        dry_above.seed(8, 65, 8, "minecraft:air".to_owned());
        let mut random = LegacyRandomSource::new(11);
        place_coral(
            &mut random,
            BlockPos { x: 8, y: 64, z: 8 },
            CoralKind::Claw,
            &mut dry_above,
        );
        assert_eq!(dry_above.dirty_cells().count(), 0, "air above origin must reject coral");
    }

    #[test]
    fn writes_clip_at_the_grid_footprint_boundary() {
        let mut full = water_grid(-16, 32);
        let mut clipped = water_grid(0, 1);
        let mut full_random = LegacyRandomSource::new(11);
        let mut clipped_random = LegacyRandomSource::new(11);
        let origin = BlockPos { x: 0, y: 64, z: 0 };
        place_coral(&mut full_random, origin, CoralKind::Tree, &mut full);
        place_coral(&mut clipped_random, origin, CoralKind::Tree, &mut clipped);
        let full_count = full.dirty_cells().count();
        let clipped_count = clipped.dirty_cells().count();
        assert!(full_count > clipped_count, "out-of-footprint coral writes must be dropped");
        assert!(clipped
            .dirty_cells()
            .all(|(x, _, z, _)| (0..1).contains(&x) && (0..1).contains(&z)));
    }
}
