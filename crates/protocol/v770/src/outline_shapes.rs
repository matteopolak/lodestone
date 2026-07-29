//! Block **outline** and **interaction** shapes for protocol 776 (Minecraft 26.2).
//!
//! Two more per-block-state shape families beside the collision one
//! ([`crate::collision_shapes`]). They are genuinely different questions with
//! genuinely different answers — **50.9% of the 32,366 states have an outline
//! that differs from their collision shape**, and 5,282 states have an *empty*
//! collision shape and a *non-empty* outline.
//!
//! # The outline shape is what block selection uses
//!
//! `Entity.pick` clips with `ClipContext.Block.OUTLINE` and
//! `ClipContext.Fluid.NONE` (`Entity.java:2012-2017`), and
//! `ClipContext.Block.OUTLINE` is `BlockStateBase::getShape`
//! (`ClipContext.java:57`). The three defaults diverge at the base class:
//!
//! | getter | default | source |
//! | --- | --- | --- |
//! | `getShape` (outline) | `Shapes.block()` | `BlockBehaviour.java:323-325` |
//! | `getCollisionShape` | `hasCollision ? state.getShape(…) : Shapes.empty()` | `BlockBehaviour.java:327-329` |
//! | `getInteractionShape` | `Shapes.empty()` | `BlockBehaviour.java:295-297` |
//!
//! So every `noCollission()` block — kelp, seagrass, torches, cobweb, redstone
//! wire, fire, every plant — has **no collision and a real outline**, and that is
//! why neither "does it collide" nor "does the cell hold a fluid" can stand in
//! for "can I target it":
//!
//! * `LiquidBlock.getShape` → `Shapes.empty()` (`LiquidBlock.java:145-147`), so
//!   open water and lava are never targeted;
//! * `KelpBlock`'s is `Block.column(16, 0, 9)` (`KelpBlock.java:24`) and
//!   `SeagrassBlock`'s `Block.column(12, 0, 12)` (`SeagrassBlock.java:29`) —
//!   non-empty, so both are targetable despite hardcoding `getFluidState` to
//!   water;
//! * `WebBlock` has no `getShape` override at all, so cobweb outlines to a full
//!   unit cube while colliding with nothing.
//!
//! # The interaction shape refines the hit *face*; it does not add a hit
//!
//! Its one caller is `BlockGetter.clipWithInteractionOverride`
//! (`BlockGetter.java:82-94`): it clips the **outline** first, and only if that
//! hit does it clip the interaction shape and — when that hit is nearer —
//! substitute its `Direction` into the outline's hit, keeping the outline's hit
//! location. It can never make an unpickable block pickable. Only the cauldron
//! family, hoppers, scaffolding and composters override it in 26.2 (8 distinct
//! shapes, of which one is empty).
//!
//! # Two shapes are context-dependent and resolve to their default form
//!
//! `getShape` takes a `CollisionContext`; the census passes
//! `CollisionContext.empty()` and `EmptyBlockGetter.INSTANCE`, which is exactly
//! what vanilla's own shape cache does (`getOcclusionShape` →
//! `state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)`,
//! `BlockBehaviour.java:287-289`). Two knowable consequences:
//!
//! * **`minecraft:light` outlines to nothing** — its shape is
//!   `context.isHoldingItem(Items.LIGHT) ? Shapes.block() : Shapes.empty()`
//!   (`LightBlock.java:66-68`). A client that wants vanilla's held-light
//!   behaviour must special-case it above this table; the table's answer (empty)
//!   is the correct *not*-holding-a-light answer.
//! * **`minecraft:scaffolding`** reports its standing rather than its descending
//!   shape.
//!
//! # Precision: 4 coordinates are not exactly `f32`
//!
//! [`BlockAabb`] is `f32`, which is lossless for every value the *collision*
//! census uses. The outline census uses 34 distinct coordinates, of which four —
//! `0.3333333125`, `0.3958333125`, `0.6041666875`, `0.6666666875`, all from
//! `minecraft:lectern` and nothing else — are not exactly representable and are
//! rounded to the nearest `f32`. The error is under `3e-9` blocks
//! (`tests/outline_shapes.rs` pins the bound), i.e. ~5 nanometres of selection
//! box on a lectern, so the narrow type is kept for one type identity across all
//! three shape seams rather than introducing a second, `f64`, box type.
//!
//! Coordinates are **not** confined to the unit cube: the census ranges
//! `-0.25..=1.25` (`pitcher_crop` reaches below zero). Do not clamp.
//!
//! # Memory design
//!
//! Identical to [`crate::collision_shapes`]: pure rodata, zero heap, O(1) by id.
//! The 32,366 states collapse to **860 distinct outline shapes** and **8 distinct
//! interaction shapes**, so each family is a `[u16; 32_366]` index (~63 KiB
//! each) into a de-duplicated shape table.

use lodestone_model::BlockAabb;

use crate::generated_outline_shapes as table;

pub use table::STATE_COUNT;

/// The **outline** boxes for block-state `id`, or `None` if `id` is not in
/// `0..`[`STATE_COUNT`].
///
/// An empty slice is a valid, meaningful result: the state exists and cannot be
/// targeted (air, water, lava, `light`, `moving_piston`, a connectionless wall).
/// Zero-heap — returns a `&'static [BlockAabb]` straight from rodata.
#[must_use]
pub fn outline_boxes(id: u32) -> Option<&'static [BlockAabb]> {
    let &shape = table::STATE_OUTLINE.get(id as usize)?;
    Some(table::OUTLINE_SHAPES[shape as usize])
}

/// The **interaction** boxes for block-state `id`, or `None` if `id` is not in
/// `0..`[`STATE_COUNT`].
///
/// Empty for all but the cauldron family, hoppers, scaffolding and composters —
/// and empty is the meaningful "no face override" answer, not a miss. See the
/// module docs for why this is a face refinement rather than a clip target.
#[must_use]
pub fn interaction_boxes(id: u32) -> Option<&'static [BlockAabb]> {
    let &shape = table::STATE_INTERACTION.get(id as usize)?;
    Some(table::INTERACTION_SHAPES[shape as usize])
}
