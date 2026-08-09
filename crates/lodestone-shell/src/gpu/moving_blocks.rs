//! **Moving block models**: a block's own baked geometry drawn somewhere other
//! than its own cell.
//!
//! This is the shell's side of vanilla's `SubmitNodeCollector.submitMovingBlock`,
//! and it is deliberately a *seam* with more than one intended producer rather
//! than a falling-block feature:
//!
//! | vanilla renderer | what it moves | built here? |
//! |---|---|---|
//! | `FallingBlockRenderer` | the falling sand/gravel entity | **yes** |
//! | `PistonHeadRenderer` | the piston head and the block it pushes | not yet — see below |
//!
//! Both renderers have the same shape and it is not the shape the rest of the
//! entity pass has: **no `bakeLayer` call in the constructor**, so they own no
//! cuboid mesh and pose existing *block models* instead. That means they cannot go
//! through [`EntityPipeline`](lodestone_render::EntityPipeline) however
//! entity-shaped they look, and it is why the piston head was left unbuilt when the
//! block-entity renderers landed — the machinery it needed is this file.
//!
//! # What a producer supplies, and what it gets
//!
//! A [`MovingBlock`] is `(state id, transform, light)`. Geometry comes from
//! [`CrackResolver::state_quads`](lodestone_render::CrackResolver::state_quads) —
//! the per-state baked-quad snapshot taken while
//! [`BlockModels`](lodestone_render::BlockModels) was still borrowable, which the
//! crack pass already keeps resident — so **any** block state resolves, including
//! ones with no full-cube geometry. There is no per-block table here to rot.
//!
//! Meshing is [`mesh_moving_block_quads`], next to the GUI item path it is a
//! sibling of; see that function for the three-way comparison against the terrain
//! and GUI meshers and for why `cullface` is ignored.
//!
//! # Why this draws through the model pipeline and not the entity pipeline
//!
//! A block model is not a cuboid rig and not an item model. It is the same baked
//! quads the terrain mesher emits, so it wants the terrain shader's atlas, tint
//! palette and animation bind groups — which is exactly what
//! [`ModelPipeline`](lodestone_render::ModelPipeline) already binds, at wgpu's
//! four-group floor with no room for a fifth. `gpu/world_items.rs` is the
//! precedent for reaching that pipeline from outside the terrain pass; this file
//! is a second, independent consumer of it, not an extension of that one (an item
//! model is posed by a `display` transform and lit full-bright or per-drop; a
//! moving block is posed in world space and lit from the world).
//!
//! # One buffer, one draw call
//!
//! Each request's placement is folded into its **vertex positions** by the
//! transform, so there is no per-instance matrix to batch on and no shared
//! geometry between two different block states. Every request is therefore
//! concatenated into a single [`GpuModelMesh`] — one upload and one draw per frame
//! however many moving blocks exist, versus one of each per block.

use lodestone_render::{Camera, Frustum, GpuModelMesh, ModelMesh, mesh_moving_block_quads};

use crate::entities::EntityDraw;

use super::terrain::ModelRenderer;
use super::{RenderState, RenderStats};

/// `EntityTypes.FALLING_BLOCK`'s registry path, as
/// [`EntityDraw::type_path`] carries it (bare path, no namespace).
pub(super) const FALLING_BLOCK_TYPE_PATH: &str = "falling_block";

/// `FallingBlockEntity`'s hitbox height — `EntityTypes.FALLING_BLOCK` is
/// `0.98 × 0.98`.
///
/// Used for the light sample, not for collision: `FallingBlockRenderer`'s
/// `extractRenderState` reads light at `BlockPos.containing(entity.getX(),
/// entity.getBoundingBox().maxY, entity.getZ())` — the **top** of the box, not the
/// feet. That matters at the moment of landing: a block resting on the floor has
/// its feet inside the cell it is about to occupy, and sampling there would read
/// the light of a solid block (dark) rather than of the air the block is falling
/// through.
const FALLING_BLOCK_HEIGHT: f32 = 0.98;

/// `FallingBlockRenderer.submit`'s pose: the entity's own position with
/// `poseStack.translate(-0.5, 0.0, -0.5)` applied.
///
/// Its own function, tested below, because it is the single most likely thing here
/// to be wrong in a way a screenshot does not obviously show — a block offset by
/// half a cell reads as a model-origin quirk, and both the presence of the `x`/`z`
/// shift and the *absence* of a `y` shift are load-bearing:
///
/// * `x`/`z`: the entity is at the block **centre** (`FallingBlockEntity.fall`
///   spawns at `pos.getX() + 0.5`) and the quads are in block-local `0..1`, so the
///   `-0.5`s put local `(0,0,0)` back at the cell's own corner.
/// * `y`: an entity's position is already its feet, so shifting it would float
///   every falling block half a block high — the plausible symmetric mistake.
///
/// A pure translation, with no rotation: `fall` sets no `yRot`/`xRot` and nothing
/// writes them afterwards, so a falling block never turns.
#[must_use]
fn falling_block_pose(feet: glam::Vec3) -> glam::Mat4 {
    glam::Mat4::from_translation(feet - glam::Vec3::new(0.5, 0.0, 0.5))
}

/// One block-model draw request: which state, where, and how lit.
///
/// The whole vocabulary of this seam. A producer that can fill this in gets block
/// geometry on screen with no further plumbing — which is the point, and the test
/// of whether the seam is general: `PistonHeadRenderer` needs a state (the head, or
/// the pushed block), a transform (a translation along the push axis, interpolated
/// by `progress`) and a light sample, and nothing else.
#[derive(Debug, Clone, Copy)]
pub(super) struct MovingBlock {
    /// The global block-state id whose baked quads to draw. A state with no
    /// geometry (air, `RenderShape.INVISIBLE`) draws nothing, which is
    /// `FallingBlockRenderer.submit`'s own
    /// `getRenderShape() == RenderShape.MODEL` guard reached by a different route.
    pub state_id: u32,
    /// Block-local `0..1` space → world space. For a falling block this is a pure
    /// translation; for a piston head it will also carry the push offset.
    pub transform: glam::Mat4,
    /// The packed sky/block light byte for the whole mesh — vanilla samples
    /// `MovingBlockRenderState`'s single `blockPos` once, not per corner.
    pub light: u8,
}

impl RenderState {
    /// Mesh this frame's moving block models into one world-space
    /// [`GpuModelMesh`].
    ///
    /// Returns `None` — and draws nothing — when there is no vanilla model pass, or
    /// when nothing on screen resolves to block geometry. Both are ordinary states,
    /// not errors: the demo palette has no `BlockModels` at all, and a frame with no
    /// falling block and no moving piston is the common case.
    ///
    /// No camera write: this draws through `model.cam_bind_group`, the same shared
    /// view-projection + fog buffer every terrain section uses, written once per
    /// frame at the top of the frame body. Baking world positions into the vertices
    /// is what makes that possible.
    pub(super) fn prepare_moving_blocks(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Option<GpuModelMesh> {
        let model = self.model.as_ref()?;
        let frustum = camera.frustum();
        let mut combined = ModelMesh::default();
        self.merge_falling_blocks(model, entities, &frustum, &mut combined, stats);
        // A second producer goes here: `merge_piston_heads(model, …, &mut combined)`,
        // sharing this buffer for the same reason the campfire shares the item one —
        // the placement is in the vertices, so there is nothing to batch on.
        if combined.quad_count() == 0 {
            return None;
        }
        stats.total_quads += combined.quad_count();
        GpuModelMesh::upload(device, &combined)
    }

    /// Merge every falling block on screen — vanilla's `FallingBlockRenderer`,
    /// which is the whole of that renderer.
    ///
    /// # The transform, and the `-0.5` that is not a centring fudge
    ///
    /// `submit` is `poseStack.translate(-0.5, 0.0, -0.5)` then `submitMovingBlock`,
    /// applied on top of the entity's own pose. The entity's `x`/`z` are the block
    /// *centre* (`FallingBlockEntity.fall` spawns at `pos.getX() + 0.5`) and the
    /// quads are in block-local `0..1`, so the two `-0.5`s undo the centring and put
    /// local `(0,0,0)` back at the block's own corner. `y` is **not** shifted,
    /// because an entity's `y` is already its feet.
    ///
    /// Net effect: a falling block that has not moved yet draws exactly over the
    /// cell it left. Getting either half wrong produces a block offset by half a
    /// cell, which reads as a plausible model-origin bug rather than an obvious one.
    ///
    /// # Two named deviations from `FallingBlockRenderer`
    ///
    /// * **`shouldRender`'s double-draw guard is not ported.** Vanilla refuses to
    ///   draw the entity when `entity.getBlockState() ==
    ///   level.getBlockState(entity.blockPosition())` — i.e. when the real world
    ///   block at the entity's cell is already the same block. That is what hides
    ///   the packet race at both ends of a fall: if `ADD_ENTITY` arrives before the
    ///   block update that cleared the origin cell, the guard suppresses the entity
    ///   rather than showing the block twice, and symmetrically on landing. This
    ///   layer has no world block-state lookup (there is no such polled source on
    ///   `RenderState`, unlike [`EntityLightSource`](super::EntityLightSource)), so
    ///   the guard has nothing to consult. Cost: for as long as the two packets are
    ///   apart the player may see both the block and its falling copy, or briefly
    ///   neither. The server drains block updates and entity syncs on two separate
    ///   ~50 ms timer arms, so that window is bounded by one tick — see
    ///   `lodestone_server::gravity_tick`'s module doc, which records the same
    ///   ordering from the other side.
    /// * **`randomSeedPos` is the current cell, not the start cell.** Vanilla passes
    ///   `entity.getStartPos()` so a model with a random per-position offset does
    ///   not shimmer as it falls. Nothing here applies a random model offset at all,
    ///   so there is no observable difference for the three states that fall today —
    ///   but a producer that adds one (a moving *grass block* would want it) has to
    ///   revisit this.
    fn merge_falling_blocks(
        &self,
        model: &ModelRenderer,
        entities: &[EntityDraw],
        frustum: &Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        for draw in entities {
            if draw.type_path != FALLING_BLOCK_TYPE_PATH {
                continue;
            }
            // `block_state`'s absence is the switch: an entity whose spawn packet
            // has not been folded yet draws nothing rather than a stand-in, exactly
            // as a drop with no reported stack does.
            let Some(state_id) = draw.block_state else {
                continue;
            };
            // A full block plus a little slack, tested before any mesh work.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(1.0),
                draw.feet + glam::Vec3::splat(1.0),
            ) {
                continue;
            }
            // The light sample at the **top** of the hitbox — see
            // `FALLING_BLOCK_HEIGHT` for why the feet are the wrong probe.
            //
            // `EntityLightSource::sample` directly rather than
            // `entity_passes::entity_light`, and both differences are deliberate:
            // that helper resolves its probe height from the entity *type*'s eye
            // height (wrong rule for a block, and `falling_block` has no eye height
            // to resolve), and it force-lights an entity whose fire flag is set —
            // which a falling block never wants, because
            // `FallingBlockEntity.displayFireAnimation` returns `false`. Adding a
            // row to a table of eye heights for something without eyes would be the
            // worse fix.
            let light = self
                .entity_light
                .sample(draw.feet + glam::Vec3::new(0.0, FALLING_BLOCK_HEIGHT, 0.0));
            if self.merge_moving_block(
                model,
                MovingBlock {
                    state_id,
                    transform: falling_block_pose(draw.feet),
                    light,
                },
                combined,
            ) {
                stats.moving_blocks_drawn += 1;
            }
        }
    }

    /// Merge one [`MovingBlock`] into `combined`. Returns whether anything was
    /// added.
    ///
    /// **The general seam.** Every producer goes through here, so a new one is a
    /// request-building loop and nothing else — no pipeline, no bind group, no
    /// buffer, no draw call.
    ///
    /// `false` means the state has no baked geometry, which is a legitimate answer
    /// (air, and any `RenderShape.INVISIBLE` block) rather than a failure. Callers
    /// use it to decide whether to count the draw.
    fn merge_moving_block(
        &self,
        model: &ModelRenderer,
        request: MovingBlock,
        combined: &mut ModelMesh,
    ) -> bool {
        let quads = model.crack_resolver.state_quads(request.state_id);
        if quads.is_empty() {
            return false;
        }
        combined.merge(&mesh_moving_block_quads(
            quads,
            request.transform,
            request.light,
        ));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pose puts a falling block that has not moved yet **exactly over the
    /// cell it left**, and the two candidate readings of each axis are evaluated
    /// rather than assumed.
    ///
    /// A block at `(x, y, z)` becomes an entity at `(x + 0.5, y, z + 0.5)`, so the
    /// pose applied to block-local `(0, 0, 0)` must land back on `(x, y, z)`, and
    /// applied to `(1, 1, 1)` on `(x + 1, y + 1, z + 1)`.
    ///
    /// Negative coordinates are the discriminating input for the same reason
    /// `MobSim`'s landing gate uses them: a `+0.5`/`-0.5` pair that happens to be
    /// written as a truncating cast somewhere agrees with `floor` for positive
    /// values only.
    #[test]
    fn the_falling_block_pose_lands_on_the_cell_the_entity_came_from() {
        // Both signs, and a cell where every axis is distinct so an axis swap
        // cannot pass.
        for block in [
            glam::Vec3::new(4.0, 70.0, 9.0),
            glam::Vec3::new(-8.0, -13.0, -3.0),
        ] {
            let entity_feet = block + glam::Vec3::new(0.5, 0.0, 0.5);
            let pose = falling_block_pose(entity_feet);
            let corner = pose.transform_point3(glam::Vec3::ZERO);
            let far = pose.transform_point3(glam::Vec3::ONE);
            assert!(
                (corner - block).length() < 1e-5,
                "block-local origin landed at {corner} but the cell is {block}"
            );
            assert!(
                (far - (block + glam::Vec3::ONE)).length() < 1e-5,
                "the far corner landed at {far}, so the pose is not a unit-scale \
                 translation"
            );
        }
    }

    /// The two wrong poses, evaluated, so the gate above is known to discriminate.
    ///
    /// `no_shift` is "forgot `translate(-0.5, 0, -0.5)` entirely" and `y_shifted`
    /// is "shifted all three axes". Both are off by half a block in a direction
    /// that still looks like an isometric-plausible cube in a screenshot, which is
    /// why they are computed here instead of described.
    #[test]
    fn both_wrong_poses_miss_the_cell_by_half_a_block() {
        let block = glam::Vec3::new(4.0, 70.0, 9.0);
        let feet = block + glam::Vec3::new(0.5, 0.0, 0.5);
        let correct = falling_block_pose(feet).transform_point3(glam::Vec3::ZERO);

        let no_shift = glam::Mat4::from_translation(feet).transform_point3(glam::Vec3::ZERO);
        let y_shifted = glam::Mat4::from_translation(feet - glam::Vec3::splat(0.5))
            .transform_point3(glam::Vec3::ZERO);

        let mut wrong: Vec<(&str, f32)> = Vec::new();
        for (name, candidate) in [("no_shift", no_shift), ("y_shifted", y_shifted)] {
            let d = (candidate - correct).length();
            if d < 0.4 {
                wrong.push((name, d));
            }
        }
        assert!(
            wrong.is_empty(),
            "control failed: a wrong pose is within 0.4 blocks of the correct one, \
             so the pose gate above proves nothing: {wrong:?}"
        );
        // And the correct one is not simply one of them under another name.
        assert_ne!(correct, no_shift);
        assert_ne!(correct, y_shifted);
    }

    /// The light probe is at the **top** of the hitbox, not the feet.
    ///
    /// The predicted value is `0.98` above the feet — `EntityTypes.FALLING_BLOCK`'s
    /// own height, which `FallingBlockRenderer.extractRenderState` reads as
    /// `getBoundingBox().maxY`. Not `1.0` (the plausible round number for a block)
    /// and not `0.0` (the feet), and the distinction matters at the moment of
    /// landing: a probe at the feet is inside the cell the block is about to
    /// occupy.
    #[test]
    fn the_light_probe_is_the_top_of_the_hitbox_not_the_feet() {
        assert_eq!(
            FALLING_BLOCK_HEIGHT, 0.98,
            "`EntityTypes.FALLING_BLOCK` is 0.98 x 0.98, not a full block"
        );
        assert_ne!(FALLING_BLOCK_HEIGHT, 1.0, "the plausible round number, excluded");
        assert_ne!(FALLING_BLOCK_HEIGHT, 0.0, "the feet, which is the wrong probe");
    }

    /// The type path this pass filters on is the bare registry path
    /// [`EntityDraw::type_path`] carries, not the namespaced key.
    ///
    /// A namespaced `"minecraft:falling_block"` here would match nothing, every
    /// frame, forever — the island shape, with a green build and zero pixels. The
    /// expectation comes from `lodestone_model::ResourceKey::path` rather than from
    /// this module's own constant.
    #[test]
    fn the_filtered_type_path_is_what_the_wire_key_reduces_to() {
        let key: lodestone_model::ResourceKey = "minecraft:falling_block"
            .parse()
            .expect("a valid resource key");
        assert_eq!(
            key.path(),
            FALLING_BLOCK_TYPE_PATH,
            "the filter must match what `net`'s fold stores in `EntityDraw::type_path`"
        );
        assert_ne!(
            FALLING_BLOCK_TYPE_PATH, "minecraft:falling_block",
            "the namespaced form would match nothing, every frame"
        );
    }
}
