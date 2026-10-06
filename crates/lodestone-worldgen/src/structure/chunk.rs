//! Per-chunk structure writes and references.
//!
//! [`StructureRefs`] is one chunk's view of the structure starts that reach it
//! (the save format's `References`, and the beardifier's input). The
//! `place_*` functions write the coded, buried-treasure and ruined-portal
//! pieces whose blocks depend on the receiving chunk's terrain.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
use lodestone_data::block_states::StateId as CanonicalStateId;

use crate::structure::{CodedBlock, StructureStart, VerticalPlacement};

/// One chunk's structure references: every start whose *adjusted* bounding box
/// comes within 12 blocks of this chunk, paired with the chunk that owns it.
///
/// This is both the persisted structure-reference view and the beardifier's
/// input. Kept as one product because they are the
/// same walk over the same 17×17 neighbourhood, and computing them separately
/// would be two chances to disagree about the reach.
#[derive(Debug, Default)]
pub struct StructureRefs {
    /// `(owning chunk x, owning chunk z, start)`, in neighbourhood scan order.
    pub entries: Vec<(i32, i32, Arc<StructureStart>)>,
}

impl StructureRefs {
    /// The `References` NBT view: structure id → the packed chunk keys of the
    /// chunks whose starts this chunk references, deduplicated and sorted.
    ///
    /// The save format writes an array of signed 64-bit keys per id, packed as
    /// `(z as u32 as i64) << 32 | (x as u32 as i64)`.
    #[must_use]
    pub fn packed_by_structure(&self) -> std::collections::BTreeMap<String, Vec<i64>> {
        let mut out: std::collections::BTreeMap<String, Vec<i64>> =
            std::collections::BTreeMap::new();
        for (cx, cz, start) in &self.entries {
            let packed =
                (i64::from(*cz as u32) << 32) | i64::from(*cx as u32);
            let list = out.entry(start.structure.clone()).or_default();
            if !list.contains(&packed) {
                list.push(packed);
            }
        }
        for list in out.values_mut() {
            list.sort_unstable();
        }
        out
    }

    /// The starts the beardifier evaluates: adaptation-bearing, piece-complete,
    /// and in reach.
    ///
    /// **Which structures reach this** is the honest measure of how much of S3 is
    /// observable in a generated world: only the seven adaptation-bearing kinds
    /// with a landed piece generator do. See `docs/worldgen-beardifier.md` for the
    /// current list — while every adaptation-bearing kind is still jigsaw (S4) or
    /// coded (S5), this iterator is empty for every chunk and the fill stage takes
    /// its no-beard branch, which is exactly what the negative control asserts.
    #[must_use]
    pub fn adaptation_bearing(&self) -> impl Iterator<Item = &Arc<StructureStart>> {
        self.entries
            .iter()
            .map(|(_, _, start)| start)
            .filter(|start| {
                start.pieces_complete
                    && start.terrain_adaptation != crate::structure::TerrainAdjustment::None
            })
    }
}

/// Writes a coded piece's ordered block list into the receiving chunk.
///
/// Chest facing is the one state that depends on the receiving grid rather than
/// on the eager start-time list. Reorienting immediately before the chest write
/// preserves the coded walk's last-write-wins order and leaves its loot vector —
/// including every already-spent roll seed — untouched.
pub(crate) fn place_coded_blocks(
    world: &mut crate::dense_grid::DenseBlockGrid,
    blocks: &[CodedBlock],
    solid_render: &dyn Fn(CanonicalStateId) -> bool,
) {
    for block in blocks {
        if block.state.block() == Block::Chest {
            let state = crate::structure::fortress::chest_state(world, block.pos, solid_render);
            world.set_id(block.pos[0], block.pos[1], block.pos[2], state);
        } else {
            world.set_id(block.pos[0], block.pos[1], block.pos[2], block.state);
        }
    }
}

/// Chebyshev chunk radius `structure_refs` reads `structure_starts` over:
/// eight chunks in each direction, forming a 17×17 neighbourhood.
pub const REFS_RADIUS: i32 = 8;

/// Both the reference reach for terrain adaptation and the inflation of an
/// adaptation-bearing start's bounding box, in blocks.
pub const BEARD_REACH: i32 = 12;

/// How far a ruined portal's post-template terrain pass can write beyond its
/// frame. Structure references normally need only the piece box (plus the
/// beardifier halo), but this pass must also reach neighbouring grids so each
/// can regenerate and clip its portion of the skirt.
pub(crate) const PORTAL_TERRAIN_REACH: i32 = 14;

/// `BuriedTreasurePieces.BuriedTreasurePiece.postProcess` — walk a cursor down
/// from the ocean-floor height at `(origin.x, origin.z)` until the block
/// *below* it is one of the five stone-family materials, fill the walk
/// position's six air/liquid neighbours (stone-family straight down, the
/// pre-walk block or sand everywhere else) and place an empty chest.
///
/// Runs against the **real** per-chunk grid at placement time (see
/// [`crate::structure::PieceRefinement::BuriedTreasureChest`]'s own doc for
/// why this cannot be an eager, start-time list like every other coded piece).
/// Draws no random: vanilla's own `random` argument is spent only inside
/// `createChest` on the loot-table roll seed, which is out of scope here the
/// same way every other structure's container loot is (see the
/// `template:block_entity_nbt`/`coded:chests` ledger rows) — the **block** is
/// what this places.
pub(crate) fn place_buried_treasure_chest(
    world: &mut crate::dense_grid::DenseBlockGrid,
    origin: [i32; 3],
) {
    let (min_x, min_y, _min_z, size_x, size_y, _size_z) = world.bounds();
    let (x, z) = (origin[0], origin[2]);
    if x < min_x || x >= min_x + size_x {
        // The piece's own column is always inside its origin chunk
        // (`chunkBlockX(9)`), so this never fires in practice — a defensive
        // bound rather than a reachable one.
        return;
    }
    let top = min_y + size_y - 1;
    // `level.getHeight(OCEAN_FLOOR_WG, x, z)`: one above the topmost block that
    // is neither air nor a fluid, scanned against the *real* grid — sand,
    // sandstone and every surface-rule product are visible here, unlike at
    // structure-start time.
    let Some(ground) = (min_y..=top).rev().find(|&y| {
        !is_air_or_liquid_id(world.get_id(x, y, z))
    }) else {
        return;
    };
    let mut y = ground + 1;
    while y > min_y {
        let below = world.get_id(x, y - 1, z);
        if is_stone_family_id(below) {
            let current = world.get_id(x, y, z);
            let soft = if !is_air_or_liquid_id(current) {
                current
            } else {
                Block::Sand.default_state()
            };
            const NEIGHBOURS: [[i32; 3]; 6] = [
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [-1, 0, 0],
                [1, 0, 0],
            ];
            for delta in NEIGHBOURS {
                let rel = [x + delta[0], y + delta[1], z + delta[2]];
                if !is_air_or_liquid_id(world.get_id(rel[0], rel[1], rel[2])) {
                    continue;
                }
                let below_rel = world.get_id(rel[0], rel[1] - 1, rel[2]);
                let is_up = delta == [0, 1, 0];
                if is_air_or_liquid_id(below_rel) && !is_up {
                    world.set_id(rel[0], rel[1], rel[2], below);
                } else {
                    world.set_id(rel[0], rel[1], rel[2], soft);
                }
            }
            // The four neighbours are now solid by construction (each was either
            // already solid or just filled), so the receiving-grid reorientation
            // fallback lands on north here.
            world.set_id(x, y, z, chest_north_state());
            return;
        }
        y -= 1;
    }
}

#[cfg(test)]
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn state_with_properties(
    block: Block,
    entries: &[(&str, &str)],
) -> CanonicalStateId {
    let properties = entries.iter().fold(
        Properties::from_state_id(block.default_state()),
        |properties, &(key, value)| {
            properties
                .with_builtin(
                    PropertyKey::from_name(key).expect("generated property key"),
                    BuiltinPropertyValue::from_name(value).expect("generated property value"),
                )
                .expect("generated block-state property is valid")
        },
    );
    Properties::state_for_block(block, &properties).expect("generated block-state properties")
}

#[inline]
fn chest_north_state() -> CanonicalStateId {
    state_with_properties(
        Block::Chest,
        &[("facing", "north"), ("type", "single"), ("waterlogged", "false")],
    )
}

#[cfg(test)]
fn is_air_or_liquid(state: &str) -> bool {
    let name = base_name(state);
    name == "minecraft:air" || name == "minecraft:water" || name == "minecraft:lava"
}

fn is_air_or_liquid_id(state: CanonicalStateId) -> bool {
    matches!(state.block(), Block::Air | Block::Water | Block::Lava)
}

/// `belowState.is(SANDSTONE) || .is(STONE) || .is(ANDESITE) || .is(GRANITE) ||
/// .is(DIORITE)`.
#[cfg(test)]
fn is_stone_family(name: &str) -> bool {
    matches!(
        name,
        "minecraft:sandstone"
            | "minecraft:stone"
            | "minecraft:andesite"
            | "minecraft:granite"
            | "minecraft:diorite"
    )
}

fn is_stone_family_id(state: CanonicalStateId) -> bool {
    matches!(
        state.block(),
        Block::Sandstone | Block::Stone | Block::Andesite | Block::Granite | Block::Diorite
    )
}

/// The ruined-portal post-template pass: terrain growth, downward columns and
/// optional overgrowth. It runs against a fully surfaced chunk, after the
/// template itself wrote its frame.
///
/// The reference receives the decorating chunk's mutable `surface_structures`
/// stream. The caller resets that stream once for each portal registry entry and
/// shares it across the starts of that entry, so this pass consumes the same
/// sequence as the surrounding structure lifecycle while the grid still clips
/// writes to the receiving chunk.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_ruined_portal_terrain<R: RandomSource, W: crate::structure::StructureWorld>(
    world: &mut W,
    box_: crate::structure::BoundingBox,
    random: &mut R,
    placement: VerticalPlacement,
    cold: bool,
    overgrown: bool,
    vines: bool,
    features_cannot_replace: &std::collections::HashSet<String>,
) {
    let centre = [
        box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2,
        box_.min[1] + (box_.max[1] - box_.min[1] + 1) / 2,
        box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2,
    ];
    let average_width = (box_.max[0] - box_.min[0] + 1 + box_.max[2] - box_.min[2] + 1) / 2;
    let distance_adjustment = random.next_int_bounded((8 - average_width / 2).max(1));
    const CHANCE_BY_DISTANCE: [f32; 14] = [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
    ];
    let follows_surface = matches!(placement, VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor);
    for x in (centre[0] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[0] + CHANCE_BY_DISTANCE.len() as i32) {
        for z in (centre[2] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[2] + CHANCE_BY_DISTANCE.len() as i32) {
            let distance = (x - centre[0]).abs() + (z - centre[2]).abs();
            let adjusted = (distance + distance_adjustment).max(0) as usize;
            let Some(&chance) = CHANCE_BY_DISTANCE.get(adjusted) else {
                continue;
            };
            if random.next_double() >= f64::from(chance) {
                continue;
            }
            let Some(surface) = portal_surface_y(world, x, z, placement) else {
                continue;
            };
            let y = if follows_surface { surface } else { box_.min[1].min(surface) };
            if (y - box_.min[1]).abs() > 3
                || !portal_replaceable_id(
                    world.get_id(x, y, z),
                    placement,
                    features_cannot_replace,
                )
            {
                continue;
            }
            place_portal_netherrack_or_magma(world, random, [x, y, z], cold);
            if overgrown {
                maybe_add_portal_leaves(world, random, [x, y, z]);
            }
            add_portal_drip_column(world, random, [x, y - 1, z], cold);
        }
    }
    for x in (box_.min[0] + 1)..box_.max[0] {
        for z in (box_.min[2] + 1)..box_.max[2] {
            if world.get_id(x, box_.min[1], z).block() == Block::Netherrack {
                add_portal_drip_column(
                    world,
                    random,
                    [x, box_.min[1] - 1, z],
                    cold,
                );
            }
        }
    }
    if vines || overgrown {
        for x in box_.min[0]..=box_.max[0] {
            for y in box_.min[1]..=box_.max[1] {
                for z in box_.min[2]..=box_.max[2] {
                    if vines {
                        maybe_add_portal_vine(world, random, [x, y, z]);
                    }
                    if overgrown {
                        maybe_add_portal_leaves(world, random, [x, y, z]);
                    }
                }
            }
        }
    }
}

fn portal_surface_y<W: crate::structure::StructureWorld>(
    world: &W,
    x: i32,
    z: i32,
    placement: VerticalPlacement,
) -> Option<i32> {
    let (_, min_y, _, _, size_y, _) = world.bounds();
    let ocean_floor = placement == VerticalPlacement::OnOceanFloor;
    (min_y..(min_y + size_y)).rev().find(|&y| {
        if ocean_floor {
            !is_air_or_liquid_id(world.get_id(x, y, z))
        } else {
            world.get_id(x, y, z).block() != Block::Air
        }
    })
}

fn portal_replaceable_id(
    state: CanonicalStateId,
    placement: VerticalPlacement,
    features_cannot_replace: &std::collections::HashSet<String>,
) -> bool {
    let block = state.block();
    block != Block::Air
        && block != Block::Obsidian
        && !features_cannot_replace.contains(block.name())
        && (placement == VerticalPlacement::InNether || block != Block::Lava)
}

fn place_portal_netherrack_or_magma<R: RandomSource, W: crate::structure::StructureWorld>(
    world: &mut W,
    random: &mut R,
    pos: [i32; 3],
    cold: bool,
) {
    let state = if !cold && random.next_float() < 0.07 {
        Block::MagmaBlock.default_state()
    } else {
        Block::Netherrack.default_state()
    };
    world.set_id(pos[0], pos[1], pos[2], state);
}

fn add_portal_drip_column<R: RandomSource, W: crate::structure::StructureWorld>(
    world: &mut W,
    random: &mut R,
    mut pos: [i32; 3],
    cold: bool,
) {
    place_portal_netherrack_or_magma(world, random, pos, cold);
    for _ in 0..8 {
        if random.next_float() >= 0.5 {
            break;
        }
        pos[1] -= 1;
        place_portal_netherrack_or_magma(world, random, pos, cold);
    }
}

fn maybe_add_portal_leaves<R: RandomSource, W: crate::structure::StructureWorld>(
    world: &mut W,
    random: &mut R,
    pos: [i32; 3],
) {
    if random.next_float() < 0.5
        && world.get_id(pos[0], pos[1], pos[2]).block() == Block::Netherrack
        && world.get_id(pos[0], pos[1] + 1, pos[2]).block() == Block::Air
    {
        let state = state_with_properties(
            Block::JungleLeaves,
            &[("distance", "7"), ("persistent", "true"), ("waterlogged", "false")],
        );
        world.set_id(pos[0], pos[1] + 1, pos[2], state);
    }
}

fn maybe_add_portal_vine<R: RandomSource, W: crate::structure::StructureWorld>(
    world: &mut W,
    random: &mut R,
    pos: [i32; 3],
) {
    let state = world.get_id(pos[0], pos[1], pos[2]).block();
    if matches!(state, Block::Air | Block::Water | Block::Lava | Block::Vine) {
        return;
    }
    let (dx, dz, vine) = match random.next_int_bounded(4) {
        0 => (
            0,
            -1,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "false"), ("south", "true"), ("up", "false"), ("west", "false")],
            ),
        ),
        1 => (
            1,
            0,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "false"), ("south", "false"), ("up", "false"), ("west", "true")],
            ),
        ),
        2 => (
            0,
            1,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "true"), ("south", "false"), ("up", "false"), ("west", "false")],
            ),
        ),
        _ => (
            -1,
            0,
            state_with_properties(
                Block::Vine,
                &[("east", "true"), ("north", "false"), ("south", "false"), ("up", "false"), ("west", "false")],
            ),
        ),
    };
    if world.get_id(pos[0] + dx, pos[1], pos[2] + dz).block() == Block::Air {
        world.set_id(pos[0] + dx, pos[1], pos[2] + dz, vine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A column with `depth` blocks of sand over stone, air above — the shape
    /// that makes the walk actually walk (a beach/ocean-floor surface rule
    /// stacking sand over the real stone), rather than terminating on its
    /// first iteration.
    fn sandy_column(depth: i32) -> crate::dense_grid::DenseBlockGrid {
        let mut map = HashMap::new();
        // Stone from -64 up to (but not including) the sand layer.
        let stone_top = 60 - depth;
        for y in -64..=stone_top {
            map.insert((8, y, 8), "minecraft:stone".to_string());
        }
        for y in (stone_top + 1)..=60 {
            map.insert((8, y, 8), "minecraft:sand".to_string());
        }
        crate::dense_grid::DenseBlockGrid::from_hashmap(0, -64, 0, 16, 384, 16, &map)
    }

    /// The expected facing values come from the independent four-neighbour
    /// direction table in the fixture, not from the production helper. The
    /// final row repeats the asymmetric east-neighbour case with solid cells
    /// above and below; those cells must not affect a horizontal reorientation.
    #[test]
    fn coded_chest_reorientation_matches_external_asymmetric_fixture() {
        const EXTERNAL: &str =
            include_str!("../../tests/support/coded_chest_reorient_external.txt");
        let mut east_face = None;
        let mut east_vertical_control = None;
        for line in EXTERNAL.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 8, "fixture fields: {line}");
            let pos = [1, 65, 1];
            let horizontal = [
                (0, -1, fields[1]),
                (1, 0, fields[2]),
                (0, 1, fields[3]),
                (-1, 0, fields[4]),
            ];
            let mut world = crate::dense_grid::DenseBlockGrid::new(
                0,
                64,
                0,
                3,
                3,
                3,
                "minecraft:air",
            );
            for (dx, dz, state) in horizontal {
                world.set(pos[0] + dx, pos[1], pos[2] + dz, state);
            }
            world.set(pos[0], pos[1] + 1, pos[2], fields[5]);
            world.set(pos[0], pos[1] - 1, pos[2], fields[6]);
            let solid_render = |state: CanonicalStateId| state.block() == Block::Stone;
            place_coded_blocks(
                &mut world,
                &[CodedBlock {
                    pos,
                    state: CanonicalStateId::from_state_str(
                        "minecraft:chest[facing=north,type=single,waterlogged=false]",
                    )
                    .unwrap(),
                }],
                &solid_render,
            );
            let actual = world.get(pos[0], pos[1], pos[2]).to_string();
            let expected = format!(
                "minecraft:chest[facing={},type=single,waterlogged=false]",
                fields[7]
            );
            assert_eq!(actual, expected, "fixture case {}", fields[0]);
            match fields[0] {
                "single_east" => east_face = Some(actual),
                "single_east_vertical_control" => east_vertical_control = Some(actual),
                _ => {}
            }
        }
        assert_eq!(
            east_face.as_deref(),
            Some("minecraft:chest[facing=west,type=single,waterlogged=false]")
        );
        assert_eq!(east_face, east_vertical_control);
    }

    /// The chest lands exactly one block above the first **stone-family**
    /// block, not the first solid block — a beach column with sand on top must
    /// be walked *through*, matching vanilla's own multi-layer descent.
    #[test]
    fn the_chest_lands_on_stone_under_a_sand_beach() {
        let mut world = sandy_column(3);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Stone top is at 60 - 3 = 57, so the chest sits at 58.
        assert_eq!(world.get(8, 58, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
        // Nothing was placed at the sand layer or below the stone surface.
        assert_ne!(world.get(8, 60, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// A column with **no** sand at all (stone straight to the surface) places
    /// the chest one above bare stone — the degenerate case of the same walk.
    #[test]
    fn the_chest_lands_directly_on_bare_stone() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        assert_eq!(world.get(8, 61, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// Every air/liquid neighbour of the chest is filled — straight down with
    /// the stone-family block the walk found, everywhere else with the
    /// pre-existing block (here, air, so it falls back to sand).
    #[test]
    fn every_air_neighbour_of_the_chest_is_filled() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Chest at (8, 61, 8), stone at (8, 60, 8) and below.
        assert_eq!(world.get(8, 60, 8), "minecraft:stone", "the ground itself is untouched");
        // The four horizontal neighbours and the one above were air; each
        // must now be something solid (sand, since there was nothing else to
        // reuse) rather than air.
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0)] {
            let state = world.get(8 + dx, 61 + dy, 8 + dz);
            assert_ne!(state, "minecraft:air", "neighbour ({dx},{dy},{dz}) was left air");
        }
    }

    /// `base_name` strips a bracketed property list; `is_air_or_liquid` and
    /// `is_stone_family` read the five- and three-member sets vanilla's own
    /// `postProcess` names, and nothing else.
    #[test]
    fn the_material_predicates_match_exactly_vanillas_named_sets() {
        assert_eq!(base_name("minecraft:water[level=0]"), "minecraft:water");
        assert!(is_air_or_liquid("minecraft:air"));
        assert!(is_air_or_liquid("minecraft:water[level=3]"));
        assert!(is_air_or_liquid("minecraft:lava"));
        assert!(!is_air_or_liquid("minecraft:stone"));
        for name in [
            "minecraft:sandstone",
            "minecraft:stone",
            "minecraft:andesite",
            "minecraft:granite",
            "minecraft:diorite",
        ] {
            assert!(is_stone_family(name), "{name} should be stone-family");
        }
        for name in ["minecraft:dirt", "minecraft:gravel", "minecraft:sand", "minecraft:deepslate"] {
            assert!(!is_stone_family(name), "{name} should not be stone-family");
        }
    }
}
