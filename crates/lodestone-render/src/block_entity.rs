//! Block-entity renderers: the cuboid rigs vanilla draws, per block entity
//! type, for blocks whose block model does not describe them.
//!
//! Chest today. The module is shaped so a second type is an entry in
//! [`lodestone_assets::block_entity_models::BLOCK_ENTITY_MODELS`] plus a
//! `*_spawns → BlockEntityInstance` resolver, not a new pipeline.
//!
//! # Why this is not `crate::entity`, when it shares every primitive
//!
//! The bake is identical — vanilla's model-part rig has no idea whether its
//! owner is a mob or a chest, and this module reuses `CubeDef`/`PartDef`/`bake_entity_parts`
//! and [`crate::entity`]'s winding rule verbatim. **Placement is the difference,
//! and it is total:**
//!
//! | | entity | block entity |
//! |---|---|---|
//! | model space | Y-**down** | Y-**up** |
//! | placement | `entity_model_matrix`: `translate(feet) · rotY(180°−yaw) · scale(−s,−s,s) · translate(0,−1.501,0)` | [`block_entity_placement_matrix`]: `translate(pos) · rotateAround(−yaw, ½,0,½)` |
//! | anchor | the entity's feet | the block's corner |
//!
//! Vanilla's *entire* placement prologue for a chest is a single rotation
//! about the block's vertical centre, by the facing's yaw — no flip and no
//! lift, because the chest's own texels are already block-space:
//! `bottom` spans y `0..10` texels (`0..0.625` blocks off the floor) and the
//! `lid` pivot at y `9` puts the closed lid's top at `14/16`, the real chest
//! height. Feeding a chest through the entity matrix buries it 1.5 blocks down,
//! upside down. `placement_does_not_flip_or_lift` is the assertion that catches
//! that, and it compares against a real matrix rather than restating a constant.
//!
//! # Determinant, and why there is nothing to get backwards here
//!
//! `CLAUDE.md`'s winding rule says to derive the front-facing sign from a real
//! camera rather than asserting a polarity. That warning applies where a
//! *handedness flip* is in play — the GUI item pose, and the entity path's
//! `scale(−1,−1,1)`. This placement matrix is a translation composed with a
//! rotation: `det = +1` exactly, for every facing, so it cannot reverse winding
//! and the quads' baked outward normals reach the rasteriser unchanged.
//! `placement_preserves_orientation` measures the determinant rather than
//! asserting it is "positive because rotations are".
//!
//! # The lid animation, and the two easings that are not the same one
//!
//! Vanilla applies **two** transforms to the raw openness and they live in
//! different classes, which is exactly the kind of thing a summary loses:
//!
//! 1. Vanilla's renderer eases the *progress*:
//!    `open = 1 - open; open = 1 - open*open*open` — a cubic ease-out
//!    ([`chest_lid_openness`]).
//! 2. Vanilla's model turns the eased value into an *angle*:
//!    `lid.xRot = -(open * PI/2)`, and then `lock.xRot = lid.xRot`
//!    ([`chest_lid_x_rot`]).
//!
//! Collapsing these into one function that takes raw openness and returns an
//! angle would still look right at the two endpoints (0 and 1 are fixed points
//! of the ease) and be wrong for every frame in between — an animation bug that
//! a screenshot at rest cannot see. They are separate, and both are unit-tested
//! against the endpoints *and* the midpoint.
//!
//! # How to change it
//!
//! * A new block-entity type needs: a model in `lodestone-assets`, a texture-stem
//!   resolver here (see [`chest_texture_stem`]), a `*Spawn` input struct, and an
//!   arm in the shell's prepare. It does **not** need a new pipeline — everything
//!   here draws through [`crate::entity_pipeline::EntityPipeline`], which spends
//!   exactly two bind groups (camera+fog / texture) and so leaves the model
//!   shader's 4-group floor alone.
//! * Part names are the animation's only handle. [`BlockEntityMesh::index_of`]
//!   resolves `"lid"`/`"lock"` by name; renaming either in the asset corpus
//!   silently freezes the lid shut (the mesh still draws, so a coverage-only gate
//!   stays green). `lodestone-assets`'
//!   `lid_and_lock_share_the_pivot_the_animation_rotates_about` is the guard.
//! * [`chest_texture_stems`] is what the shell preloads. A material added to
//!   [`ChestMaterial`] and *not* to that list resolves to a stem with no bind
//!   group, and the shell falls back — visible, but wrong. Both are derived from
//!   the same match, so add the arm and the list entry together.

mod batching;
mod model_families;
mod model_set;

pub use batching::*;
pub use model_families::*;
pub use model_set::*;

#[cfg(test)]
mod tests;
