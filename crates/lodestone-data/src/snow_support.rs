//! The four per-block-state facts vanilla's `freeze_top_layer`
//! (`TOP_LAYER_MODIFICATION`, `SnowAndFreezeFeature`) needs and that no other
//! census in this crate carries, for protocol 776 (Minecraft 26.2).
//!
//! # Why this table has to exist
//!
//! `SnowAndFreezeFeature.place` (`SnowAndFreezeFeature.java:20-49`) asks four
//! questions of the block field, none of them answerable from `blocks.json`:
//!
//! | fn | vanilla expression | used for |
//! |---|---|---|
//! | [`face_full_up`] | `Block.isFaceFull(state.getCollisionShape(…), UP)` (`Block.java:345-348`) | `SnowLayerBlock.canSurvive` (`SnowLayerBlock.java:76-86`) |
//! | [`has_fluid_state`] | `!state.getFluidState().isEmpty()` | the second half of the `MOTION_BLOCKING` heightmap predicate (`Heightmap.java:151`) |
//! | [`is_water_source_liquid_block`] | `getFluidState().is(Fluids.WATER) && block instanceof LiquidBlock` (`Biome.java:153`) | which blocks turn to ice |
//! | [`has_snowy_property`] | `state.hasProperty(BlockStateProperties.SNOWY)` | the `snowy` flip under a placed snow layer (`SnowAndFreezeFeature.java:41-43`) |
//!
//! [`crate::block_solidity::blocks_motion`] already carries the *first* half of
//! the `MOTION_BLOCKING` predicate, and [`crate::collision_shapes`] carries the
//! collision *geometry* — but neither answers the questions above, for reasons
//! the dump makes measurable rather than assertable:
//!
//! * **`face_full_up` is not "is it a full cube".** Only **6,359** of 32,366
//!   states have a full UP collision face — a minority, which is itself
//!   surprising, and it does not coincide with "the collision shape is one
//!   unit box" either. `tests/snow_support.rs` measures both counts and their
//!   disagreement rather than restating them here. The predicate is
//!   `!Shapes.joinIsNotEmpty(Shapes.block(), shape.getFaceShape(UP),
//!   NOT_SAME)` over the *discretised* `DiscreteVoxelShape` grid, after
//!   `VoxelShape.calculateFace`'s three-way branch on `isCubeLikeAlong(Y)` /
//!   empty slice / cube-like slice (`VoxelShape.java:197-245`). Re-deriving that
//!   from the AABB list [`crate::collision_shapes`] holds means re-implementing
//!   `SliceShape`, `CubePointRange` and their `1.0E-7` fuzzy comparisons. That
//!   is a hand-rolled geometry pass, and this repo has already shipped a
//!   hand-rolled lexer that was silently wrong about lifetimes; asking the jar
//!   costs one column.
//!
//! * **`is_water_source_liquid_block` is true for exactly ONE state.** Not
//!   "every water block" and not "every waterlogged block": `Fluids.WATER` is
//!   the *source* fluid, so `water[level=1..15]` is `Fluids.FLOWING_WATER` and
//!   fails `is(Fluids.WATER)`, while a waterlogged stair passes the fluid test
//!   and fails `instanceof LiquidBlock`. The single state is
//!   `minecraft:water[level=0]`. Ice therefore forms only on still source
//!   water, and any hand-written approximation of this predicate ("is it
//!   water?") freezes flowing water that vanilla leaves alone.
//!
//! * **`has_snowy_property` is true for exactly SIX states** — `grass_block`,
//!   `podzol` and `mycelium`, two states each. This one *is* derivable from
//!   [`crate::block_states::properties`], and `tests/snow_support.rs` asserts
//!   exactly that agreement. The column exists so "derivable" is a measurement
//!   rather than a claim.
//!
//! # Known scope
//!
//! The shape behind [`face_full_up`] is read at `EmptyBlockGetter.INSTANCE` /
//! `BlockPos.ZERO`, so neighbour-dependent geometry (fences, walls, panes)
//! reports its no-neighbour shape — the same convention
//! [`crate::collision_shapes`] uses, and the dump carries the `dynamicShape()`
//! candidate set so that scope is checkable rather than asserted.
//!
//! # Data source
//!
//! Dumped from the real 26.2 server (`oracle-java/SnowSupportOracle.java`,
//! walking `Block.BLOCK_STATE_REGISTRY` after `SharedConstants::tryDetectVersion`
//! + `Bootstrap::bootStrap`), same as [`crate::block_solidity`] and
//! [`crate::shade_brightness`]. See `tests/snow_support.rs` for the generator,
//! the drift guard and `LODESTONE_REGEN=1`.
//!
//! # Memory design
//!
//! Four bitsets, 4,046 bytes each — pure rodata, no heap, O(1) by id.

use crate::generated_snow_support as table;

pub use table::STATE_COUNT;

/// Reads bit `id` out of a packed little-endian-within-byte bitset.
fn bit(bits: &[u8], id: u32) -> Option<bool> {
    if id >= STATE_COUNT {
        return None;
    }
    let byte = *bits.get((id / 8) as usize)?;
    Some(byte & (1u8 << (id % 8)) != 0)
}

/// Vanilla `Block.isFaceFull(state.getCollisionShape(level, pos), Direction.UP)`
/// for block-state `id`, or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// This is the geometric half of `SnowLayerBlock.canSurvive`: a snow layer
/// survives on a block whose collision shape presents a full 1×1 square at its
/// top face. **Not** the same question as "is the collision shape a full block"
/// — see the module doc.
#[must_use]
pub fn face_full_up(id: u32) -> Option<bool> {
    bit(&table::FACE_FULL_UP, id)
}

/// Vanilla `!state.getFluidState().isEmpty()` for block-state `id`, or `None`
/// if `id` is not in `0..`[`STATE_COUNT`].
///
/// The second half of the `MOTION_BLOCKING` heightmap predicate
/// (`Heightmap.java:151` — `input.blocksMotion() || !input.getFluidState()
/// .isEmpty()`); combine with [`crate::block_solidity::blocks_motion`] for the
/// whole thing. True for still and flowing water and lava **and** every
/// waterlogged state, which is why it is broader than
/// [`is_water_source_liquid_block`].
#[must_use]
pub fn has_fluid_state(id: u32) -> Option<bool> {
    bit(&table::HAS_FLUID_STATE, id)
}

/// Vanilla `state.getFluidState().is(Fluids.WATER) && state.getBlock()
/// instanceof LiquidBlock` for block-state `id`, or `None` if `id` is not in
/// `0..`[`STATE_COUNT`].
///
/// Exactly the condition `Biome.shouldFreeze` (`Biome.java:153`) puts on a
/// block before it becomes ice. True for **one** state in 26.2,
/// `minecraft:water[level=0]` — see the module doc for why that is not a bug in
/// the dump.
#[must_use]
pub fn is_water_source_liquid_block(id: u32) -> Option<bool> {
    bit(&table::IS_WATER_SOURCE_LIQUID_BLOCK, id)
}

/// Vanilla `state.hasProperty(BlockStateProperties.SNOWY)` for block-state
/// `id`, or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// `SnowAndFreezeFeature` flips this property to `true` on the block it puts a
/// snow layer on (`SnowAndFreezeFeature.java:41-43`); a port that skips the flip
/// leaves visibly wrong terrain (a green grass top under snow) even when the
/// snow itself is placed correctly.
#[must_use]
pub fn has_snowy_property(id: u32) -> Option<bool> {
    bit(&table::HAS_SNOWY_PROPERTY, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_ids_are_none() {
        assert!(face_full_up(STATE_COUNT).is_none());
        assert!(has_fluid_state(STATE_COUNT).is_none());
        assert!(is_water_source_liquid_block(STATE_COUNT).is_none());
        assert!(has_snowy_property(STATE_COUNT).is_none());
        assert!(face_full_up(u32::MAX).is_none());
    }

    /// Every column must be non-degenerate: neither all-zero nor all-one. A
    /// bitset makes shipping an all-zero table easy (a decode bug in the
    /// generator produces one silently), and an all-zero `face_full_up` would
    /// mean snow never survives anywhere — a plausible-looking empty world
    /// rather than a crash.
    #[test]
    fn every_column_has_both_values() {
        for (name, f) in [
            ("face_full_up", face_full_up as fn(u32) -> Option<bool>),
            ("has_fluid_state", has_fluid_state),
            (
                "is_water_source_liquid_block",
                is_water_source_liquid_block,
            ),
            ("has_snowy_property", has_snowy_property),
        ] {
            let set = (0..STATE_COUNT).filter(|&id| f(id) == Some(true)).count();
            assert!(set > 0, "{name} is all-zero across {STATE_COUNT} states");
            assert!(
                set < STATE_COUNT as usize,
                "{name} is all-one across {STATE_COUNT} states"
            );
        }
    }
}
