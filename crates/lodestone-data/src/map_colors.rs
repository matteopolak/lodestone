//! What each block state paints on a map, and the map colour palette.
//!
//! A state's map colour is one of 62 palette entries (`0` paints nothing), and
//! the colour is a property of the state, not just the block: logs differ by
//! axis, beds by part. A state that holds a fluid has a second answer, the colour
//! of the fluid block a map shows in its place when the state's top face is not
//! sturdy; a top waterlogged slab keeps its own colour, a bottom one shows water.
//!
//! Both tables are captured from the real server's block-state registry by
//! `oracle-java/MapColorOracle.java` and joined to the canonical state ids by
//! resource name and properties. `tests/map_colors.rs` owns generation, the
//! structural checks and witnesses taken from the palette's own definition.
//! Regenerate with `just oracle-map-colors` then `just regen-map-colors`.
//!
//! Pure rodata: two bytes per state and a 64-entry palette.

use crate::block_states::StateId;
use crate::generated_map_colors as table;

pub use table::STATE_COUNT;

/// The palette id a map paints for `state`, `0` when it paints nothing (air).
#[must_use]
pub fn map_color(state: StateId) -> u8 {
    table::COLOR[state.index()]
}

/// Whether `state` holds a fluid (water, lava, a waterlogged block, kelp).
#[must_use]
pub fn holds_fluid(state: StateId) -> bool {
    table::SURFACE[state.index()] & FLUID_BIT != 0
}

/// The palette id a map paints when `state` is the top block of a column.
///
/// Equals [`map_color`] for a state without fluid or with a sturdy top face; for
/// the others it is the colour of the fluid's own block.
#[must_use]
pub fn surface_map_color(state: StateId) -> u8 {
    table::SURFACE[state.index()] & !FLUID_BIT
}

/// The RGB of a palette id (`0x00RRGGBB`); ids the palette leaves undefined are
/// `0`, the same as the empty colour.
#[must_use]
pub fn palette_rgb(id: u8) -> u32 {
    table::PALETTE[usize::from(id & 63)]
}

const FLUID_BIT: u8 = 0x80;
