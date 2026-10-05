//! Biome-dependent freezing and snowing as the world-generation region answers it.
//!
//! The region's block-light query reads a light engine that has not run for chunks still being
//! generated, so every block-light value here is zero.

use lodestone_worldgen_core::engine::release26_3::biome::cold_enough_to_snow;

use crate::blocks::FluidKind;
use crate::level::Level;

/// Whether the water at `(x, y, z)` should become ice. `check_neighbors` keeps water that is
/// enclosed on all four sides.
#[must_use]
pub fn should_freeze(level: &Level<'_>, x: i32, y: i32, z: i32, check_neighbors: bool) -> bool {
    should_freeze_in(level, y, x, y, z, check_neighbors)
}

/// Like [`should_freeze`], with the biome looked up at height `biome_y` (the top-layer freeze
/// asks the biome of the block above the water).
#[must_use]
pub fn should_freeze_in(level: &Level<'_>, biome_y: i32, x: i32, y: i32, z: i32, check_neighbors: bool) -> bool {
    let info = level.biome_info(x, biome_y, z);
    if !cold_enough_to_snow(info, x, y, z, level.sea_level) {
        return false;
    }
    if level.is_outside_build_height(y) {
        return false;
    }
    let blocks = &level.env.blocks;
    let here = level.get(x, y, z);
    if blocks.fluid(here) != FluidKind::Water || !is_water_block(level, here) {
        return false;
    }
    if !check_neighbors {
        return true;
    }
    let water_at = |dx: i32, dz: i32| blocks.fluid(level.get(x + dx, y, z + dz)) == FluidKind::Water;
    !(water_at(-1, 0) && water_at(1, 0) && water_at(0, -1) && water_at(0, 1))
}

/// Whether `state` is the plain water block (not a waterlogged block or a bubble column).
fn is_water_block(level: &Level<'_>, state: crate::blocks::State) -> bool {
    level.env.blocks.block_of(state) == level.env.blocks.block_of(level.env.known.water)
}

/// Whether a snow layer should be laid at `(x, y, z)`.
#[must_use]
pub fn should_snow(level: &Level<'_>, x: i32, y: i32, z: i32) -> bool {
    let info = level.biome_info(x, y, z);
    if !info.has_precipitation || !cold_enough_to_snow(info, x, y, z, level.sea_level) {
        return false;
    }
    if level.is_outside_build_height(y) {
        return false;
    }
    let here = level.get(x, y, z);
    let env = level.env;
    (env.blocks.is_air(here) || env.blocks.block_of(here) == env.blocks.block_of(env.known.snow)) && snow_layer_can_survive(level, x, y, z)
}

/// A snow layer needs a supporting block below: a full up face, a full snow stack, or a block
/// the tags force either way.
#[must_use]
pub fn snow_layer_can_survive(level: &Level<'_>, x: i32, y: i32, z: i32) -> bool {
    let env = level.env;
    let below = level.get(x, y - 1, z);
    let block = env.blocks.block_of(below);
    let tag = |name: &str| env.tags.get(name).is_some_and(|t| t.contains(block));
    if tag("cannot_support_snow_layer") {
        return false;
    }
    if tag("support_override_snow_layer") {
        return true;
    }
    env.blocks.collision_up_full(below) || (block == env.blocks.block_of(env.known.snow) && env.blocks.get(below, "layers") == Some("8"))
}
