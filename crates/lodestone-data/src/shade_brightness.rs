//! Per-block-state **ambient-occlusion occluder** census for protocol 776
//! (Minecraft 26.2) — vanilla's `BlockStateBase.getShadeBrightness`, reduced to
//! the one bit a smooth-lighting mesher needs.
//!
//! # What this answers, and why it is not the culling predicate
//!
//! `BlockModelLighter.prepareQuadAmbientOcclusion` darkens a
//! smooth-lit vertex by averaging `cache.getShadeBrightness(state, level, pos)`
//! over the three cells around each of a quad's corners. That value is
//! `BlockBehaviour.getShadeBrightness`:
//!
//! ```text
//! return state.isCollisionShapeFullBlock(level, pos) ? 0.2F : 1.0F;
//! ```
//!
//! — a **collision** question. It is *not* the renderer's face-culling
//! occlusion, which is what a client naturally has to hand
//! (`lodestone_render::BlockModels::occludes`, derived from baked geometry plus
//! sprite opacity). The two agree on stone, on slabs, on water and on glass, and
//! that near-agreement is exactly the trap: they disagree on **every full
//! collision cube whose model does not occlude for culling**, which is the whole
//! leaf population. Vanilla darkens the underside of a tree canopy; a client
//! using its culling predicate does not.
//!
//! Measured from the dump this table is generated from: 3,254 of 32,366 states
//! occlude for AO, against 3,287 that are full collision cubes — the seven
//! `getShadeBrightness` overrides move **39 states across 30 blocks**, and they
//! move them in *both* directions (see below).
//!
//! # Why a dump and not a derivation
//!
//! Seven classes override `getShadeBrightness` in the whole 26.2 tree, and they
//! are *classes*, not blocks — `TransparentBlock` alone covers **26** registered
//! blocks (glass, all 16 stained glasses, tinted glass, and the eight copper
//! grates via `WaterloggedTransparentBlock`). A hand-written block list would
//! have had to expand that family by hand, which is the mistake this repo has
//! shipped twice ([`crate::entity_census`]'s header records the two off-by-one
//! metadata indices). So the census is dumped per state from the real server
//! (`oracle-java/ShadeBrightnessOracle.java`) and the override set is emitted
//! *mechanically* alongside it, so "exactly seven" is a measurement.
//!
//! The overrides do not all point the same way, which is the other reason a
//! derivation from [`crate::collision_shapes`] would be wrong:
//!
//! | class | returns | net effect vs. the shape |
//! | --- | --- | --- |
//! | `TransparentBlock` (26 blocks) | `1.0` | shape says occlude, override says no |
//! | `BarrierBlock`, `LightBlock`, `StructureVoidBlock` | `1.0` | same direction |
//! | `MudBlock`, `SoulSandBlock` | `0.2` | shape says **no** (both sink an entity, so neither collision box is a full cube), override says occlude |
//! | `SnowLayerBlock` | `LAYERS == 8 ? 0.2 : 1.0` | per **state**, not per block |
//!
//! # Memory design
//!
//! One bitset, 4,046 bytes of pure rodata — O(1) by id, no heap. Same layout
//! and the same reasoning as [`crate::block_solidity`].

use crate::generated_shade_brightness as table;

pub use table::STATE_COUNT;

/// Vanilla's occluded ambient-occlusion shade sample. Not `0.0`: a corner with
/// all three neighbours occluding still averages `0.4` once the always-open
/// front cell's `1.0` is folded in.
pub const OCCLUDED_SHADE: f32 = 0.2;

/// Vanilla's unoccluded ambient-occlusion shade sample.
pub const OPEN_SHADE: f32 = 1.0;

/// Reads bit `id` out of a packed little-endian-within-byte bitset.
fn bit(bits: &[u8], id: u32) -> Option<bool> {
    if id >= STATE_COUNT {
        return None;
    }
    let byte = *bits.get((id / 8) as usize)?;
    Some(byte & (1u8 << (id % 8)) != 0)
}

/// Whether block-state `id` darkens an adjacent ambient-occlusion corner —
/// vanilla `BlockStateBase.getShadeBrightness(..) == 0.2F`. `None` if `id` is
/// not in `0..`[`STATE_COUNT`].
///
/// This is the predicate a smooth-lighting mesher wants for its AO term. It is
/// **not** interchangeable with a face-culling occlusion test; see the module
/// docs for the 39 states where the two part company, and note that the *light*
/// half of vanilla's smooth blend uses a third predicate again
/// (`isViewBlocking` / `getLightDampening`, via `translucentN` in
/// `BlockModelLighter`), so this table must not be substituted there either.
#[must_use]
pub fn occludes_ambient_light(id: u32) -> Option<bool> {
    bit(&table::SHADE_OCCLUDES, id)
}

/// Vanilla `BlockStateBase.getShadeBrightness` for block-state `id` — the float
/// itself, for a consumer that wants to multiply rather than branch.
///
/// The dump this is generated from carries a histogram of every value the game
/// actually returns across all 32,366 states, and it has exactly two entries
/// ([`OCCLUDED_SHADE`] and [`OPEN_SHADE`]) — which is what makes the one-bit
/// encoding lossless rather than merely convenient. Unknown ids fall back to
/// [`OPEN_SHADE`], matching air.
#[must_use]
pub fn shade_brightness(id: u32) -> f32 {
    if occludes_ambient_light(id) == Some(true) {
        OCCLUDED_SHADE
    } else {
        OPEN_SHADE
    }
}

/// How many states occlude for ambient occlusion.
///
/// Exposed as an **anti-vacuity check**: an all-zero bitset satisfies "no state
/// wrongly darkens a corner", so a gate that reads this table has to be able to
/// prove the table is populated at all.
#[must_use]
pub fn occluding_state_count() -> u32 {
    table::SHADE_OCCLUDES
        .iter()
        .map(|byte| u32::from(byte.count_ones()))
        .sum()
}
