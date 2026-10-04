//! `canSurvive` for the blocks decoration asks about.

use crate::blocks::State;
use crate::level::Level;

/// Whether `state` could stand at the position (reads the surrounding blocks).
///
/// # Panics
/// For a block whose rule is not yet ported; the decoration oracle never reaches those.
#[must_use]
pub fn can_survive(level: &Level<'_>, state: State, x: i32, y: i32, z: i32) -> bool {
    let env = level.env;
    let name = env.blocks.block_name(env.blocks.block_of(state));
    let below = level.get(x, y - 1, z);
    let supports = |tag: &str| env.tags.get(tag).is_some_and(|t| t.contains(env.blocks.block_of(below)));
    match name {
        n if n.ends_with("_sapling") => supports("supports_vegetation"),
        other => panic!("canSurvive is not ported for {other}"),
    }
}
