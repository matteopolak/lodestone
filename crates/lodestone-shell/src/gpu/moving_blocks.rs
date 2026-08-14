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
//! | `PistonHeadRenderer` | the piston head and the block it pushes | **yes** |
//! | `TntRenderer` | primed TNT's block model | **yes** |
//! | `AbstractMinecartRenderer`'s `displayBlockModel` branch | a minecart's contents (chest/furnace/TNT/hopper) | **yes** |
//!
//! Both renderers have the same shape and it is not the shape the rest of the
//! entity pass has: **no `bakeLayer` call in the constructor**, so they own no
//! cuboid mesh and pose existing *block models* instead. That means they cannot go
//! through [`EntityPipeline`](lodestone_render::EntityPipeline) however
//! entity-shaped they look, and it is why the piston head was left unbuilt when the
//! block-entity renderers landed — the machinery it needed is this file.
//!
//! # The two producers differ only in where their requests come from
//!
//! A falling block is an **entity**, so [`merge_falling_blocks`] filters the
//! `&[EntityDraw]` slice `render` is already handed. A moving piston is a **block
//! entity**, so [`merge_piston_heads`] reads a polled
//! [`MovingPistonSource`](super::MovingPistonSource) exactly as every other
//! block-entity type in `gpu/sources.rs` does. Neither touches a pipeline, a bind
//! group or a draw call: both end at [`RenderState::merge_moving_block`], which is
//! the whole seam.
//!
//! **Our own server never produces a `moving_piston` block entity**
//! (`lodestone_server::piston` applies a push in one step and says so), so the
//! piston producer draws against a real 26.2 server and not against singleplayer
//! until that lands. That is a property of the server side, not of this file: the
//! gather reads the same `World::block_entities` records the chunk packet fills in,
//! whoever sent them.
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

/// `EntityTypes.TNT`'s registry path, as [`EntityDraw::type_path`] carries it.
///
/// `TntRenderer` is the same shape as `FallingBlockRenderer` — no `bakeLayer`
/// call, a block model posed by hand — so primed TNT belongs in this file
/// beside the falling block rather than in the cuboid-rig entity pass. See
/// [`merge_primed_tnt`] for the deviations from vanilla this port accepts.
pub(super) const PRIMED_TNT_TYPE_PATH: &str = "tnt";

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

/// `TntRenderer.submit`'s pose, minus the fuse-driven scale swell (see this
/// module's own doc for why that piece is not ported).
///
/// Vanilla, in `poseStack` call order:
///
/// ```text
/// translate(0, 0.5, 0)
/// [scale(s, s, s) — swell in the last 10 ticks, not ported]
/// mulPose(YP.rotationDegrees(-90))
/// translate(-0.5, -0.5, 0.5)
/// mulPose(YP.rotationDegrees(90))
/// ```
///
/// on top of the entity's own `translate(x, y, z)`. Composed in the same
/// order rather than hand-simplified — the two `Ry` calls do **not** cancel,
/// because a translation sits between them, and the whole point of writing
/// out every term is that a reader can check this against `TntRenderer.java`
/// line for line instead of trusting an algebraic shortcut.
#[must_use]
fn primed_tnt_pose(feet: glam::Vec3) -> glam::Mat4 {
    glam::Mat4::from_translation(feet)
        * glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.5, 0.0))
        * glam::Mat4::from_rotation_y((-90.0f32).to_radians())
        * glam::Mat4::from_translation(glam::Vec3::new(-0.5, -0.5, 0.5))
        * glam::Mat4::from_rotation_y(90.0f32.to_radians())
}

/// `PistonMovingBlockEntity.getExtendedProgress` — the **signed** fraction of a
/// cell the moved geometry is displaced by, along the raw `direction` axis.
///
/// Vanilla: `extending ? progress - 1.0F : 1.0F - progress`. Both arms end at
/// `0.0` when `progress` reaches `1.0`, which is what makes the moved block land
/// exactly on its destination cell — but they start from opposite ends, and
/// **they agree at `progress == 0.5`**, so that value cannot discriminate this
/// from the plausible wrong reading (`progress` used raw, with the sign folded
/// into the movement direction instead). The tests below use `0.25`.
///
/// Its own function, next to [`falling_block_pose`], because it is the one piece
/// of `PistonHeadRenderer` a screenshot cannot check: a sign error puts the head
/// one cell *past* its destination, which still looks like a piston extending.
#[must_use]
fn piston_extended_progress(progress: f32, extending: bool) -> f32 {
    if extending {
        progress - 1.0
    } else {
        1.0 - progress
    }
}

/// `PistonHeadRenderer.submit`'s pose: the block entity's own cell corner plus
/// `translate(xOff, yOff, zOff)`.
///
/// **No `-0.5` here, unlike [`falling_block_pose`], and that asymmetry is the
/// whole difference between the two producers.** A block entity's pose stack is
/// already at its cell *corner* (`BlockEntityRenderDispatcher` translates by
/// `pos.getX() - camX`, not by the centre), while an entity's position is its
/// centre in `x`/`z` — so the shift that is mandatory for a falling block would
/// slide every piston head half a cell diagonally.
///
/// `direction` is the raw `PistonMovingBlockEntity.direction` step, **not** the
/// movement direction: `getXOff` multiplies `direction.getStepX()` by
/// [`piston_extended_progress`], whose sign already encodes retraction.
#[must_use]
fn piston_head_pose(cell: [i32; 3], direction: [i32; 3], progress: f32, extending: bool) -> glam::Mat4 {
    let extended = piston_extended_progress(progress, extending);
    let corner = glam::Vec3::new(cell[0] as f32, cell[1] as f32, cell[2] as f32);
    let step = glam::Vec3::new(
        direction[0] as f32,
        direction[1] as f32,
        direction[2] as f32,
    );
    glam::Mat4::from_translation(corner + step * extended)
}

/// `AbstractMinecart.getDefaultDisplayBlockState()`/`getDefaultDisplayOffset()`
/// for the four `MinecartKind` variants whose default cart contents are
/// non-air, keyed by [`EntityDraw::type_path`]. `minecraft:minecart` itself
/// (`AbstractMinecart`'s own default, `Blocks.AIR`) carries no entry, which is
/// exactly "the plain cart draws nothing inside" — the caller's `None` arm
/// skips [`merge_moving_block`] rather than needing an air special case.
///
/// **Only the default state, never `getCustomDisplayBlockState()`.** That field
/// is entity data set by `/data merge` on a placed minecart NBT, and nothing on
/// this side of the wire decodes it (`crates/protocol/v770/src/packets/
/// metadata.rs` has no `AbstractMinecart` arm) — every survival-obtained cart
/// never sets it, so the default is the overwhelming common case, not a
/// placeholder standing in for a real decode.
///
/// `furnace_minecart`'s `lit` is vanilla's `hasFuel()`, which is server-tick
/// fuel state with the same no-decoder gap, so it is pinned to `false` — the
/// vanilla *unfuelled* default, not a guess — matching how [`merge_primed_tnt`]
/// already omits the fuse-driven swell/flash for the identical reason.
#[must_use]
fn default_minecart_contents(type_path: &str) -> Option<(&'static str, i32)> {
    match type_path {
        // `MinecartChest.getDefaultDisplayBlockState`/`getDefaultDisplayOffset`.
        "chest_minecart" => Some(("minecraft:chest[facing=north]", 8)),
        // `MinecartFurnace.getDefaultDisplayBlockState`; `getDefaultDisplayOffset`
        // is not overridden, so `AbstractMinecart`'s base `6` applies.
        "furnace_minecart" => Some(("minecraft:furnace[facing=north,lit=false]", 6)),
        // `MinecartTNT.getDefaultDisplayBlockState`; offset not overridden (`6`).
        "tnt_minecart" => Some(("minecraft:tnt", 6)),
        // `MinecartHopper.getDefaultDisplayBlockState`/`getDefaultDisplayOffset`.
        "hopper_minecart" => Some(("minecraft:hopper", 1)),
        _ => None,
    }
}

/// `AbstractMinecartRenderer.submit`'s content-block pose.
///
/// Transcribed from `submit` in composition order: the same bob+yaw term the
/// cart frame itself gets via `non_living_vehicle_matrix` — see that
/// function's own doc for why this port substitutes `180 - yaw` for vanilla's
/// bare `rotationDegrees(state.yRot)` throughout, a substitution applied
/// consistently here so the content sits aligned with the frame this engine
/// actually draws, not with vanilla's — followed by vanilla's own
/// `scale(0.75) → translate(-0.5, (displayOffset-8)/16, 0.5) →
/// rotateY(90)` chain.
///
/// **Deliberately excludes the frame's `scale(-1, -1, 1)` flip.** In
/// `submit`, the content block is pushed and popped *before* that flip is
/// applied (the flip sits between the popped content push and
/// `submitModel(this.model, …)`), so unlike the cart frame the content block
/// keeps ordinary winding. Folding the flip in here would mirror the chest
/// through its own middle.
///
/// Not ported: the sub-millimetre per-entity jitter (`offsetX/Y/Z`, keyed off
/// `entity.getId()`, amplitude `0.004` blocks) and the hurt-time wobble — both
/// genuinely invisible at any camera distance that matters, unlike the
/// `0.375` bob or the `(displayOffset-8)/16` term, which move the content by
/// up to a quarter block and are exactly the "still looks plausible" class
/// this file's other pose functions guard against.
#[must_use]
fn minecart_content_pose(feet: glam::Vec3, yaw_deg: f32, display_offset: i32) -> glam::Mat4 {
    let translate_feet = glam::Mat4::from_translation(feet);
    let bob = glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.375, 0.0));
    let rotate = glam::Mat4::from_rotation_y((180.0 - yaw_deg).to_radians());
    let scale = glam::Mat4::from_scale(glam::Vec3::splat(0.75));
    let offset_y = (display_offset - 8) as f32 / 16.0;
    let translate = glam::Mat4::from_translation(glam::Vec3::new(-0.5, offset_y, 0.5));
    let spin = glam::Mat4::from_rotation_y(90.0f32.to_radians());
    translate_feet * bob * rotate * scale * translate * spin
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
        // A third producer of the same shape as the falling block: no rig, a
        // block model posed by hand. See `merge_primed_tnt`'s own doc for why
        // primed TNT belongs here rather than in the entity pass.
        self.merge_primed_tnt(model, entities, &frustum, &mut combined, stats);
        // A fourth producer of the same shape: no rig, a block model posed by
        // hand. `merge_primed_tnt`'s block draws at the *entity's* pose; this
        // one draws at the *cart's* pose, nested one level deeper (see
        // `minecart_content_pose`'s own doc).
        self.merge_minecart_contents(model, entities, &frustum, &mut combined, stats);
        // The second producer, sharing this buffer for the same reason the campfire
        // shares the item one — the placement is in the vertices, so there is
        // nothing to batch on.
        self.merge_piston_heads(model, camera.position, &frustum, &mut combined, stats);
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

    /// Merge every primed TNT entity on screen — vanilla's `TntRenderer`, minus
    /// two pieces named below.
    ///
    /// # Why this is the entity render path's missing hop
    ///
    /// Primed TNT physics (the 80-tick fuse, the launch impulse, gravity, drag,
    /// bounce) is server/physics work and lands correct regardless of this
    /// function; this is only the last hop, placing the already-correct entity
    /// on screen. `TntRenderer` has no `bakeLayer` call and poses an existing
    /// block model, exactly like `FallingBlockRenderer` — it is not a cuboid
    /// rig, so it cannot go through the entity pipeline's `resolve_animated`
    /// (which silently skips any `type_path` with no baked model, `"tnt"`
    /// included) and belongs beside [`merge_falling_blocks`] instead.
    ///
    /// # State id: hardcoded, not read off the wire
    ///
    /// Unlike a falling block, whose block state is genuinely variable (sand,
    /// gravel, concrete powder, …) and arrives in the spawn packet's Object
    /// Data field, `PrimedTnt.blockState` is always `Blocks.TNT.defaultBlockState()`
    /// in practice and nothing on our wire carries it — so this looks the
    /// default state up directly with [`lodestone_data::block_states::state_id`]
    /// rather than routing through [`EntityDraw::block_state`], which exists
    /// for the *variable* case and would be one more hop for a constant.
    ///
    /// # Two named deviations from `TntRenderer`, both because the fuse count
    /// has no client-side home yet
    ///
    /// * **No swell scale.** Vanilla scales the block up during the last 10
    ///   ticks of the fuse (`TntRenderer.getSwellAmount`). `EntityDraw` carries
    ///   no fuse value — `PrimedTnt.DATA_FUSE_ID` is decoded server-side
    ///   (`lodestone_server::mobs::tnt`) and put on the wire as metadata index
    ///   8, but nothing on this side of the wire folds it into an ingest
    ///   component yet (`metadata_class` has no `Tnt` arm), so there is nothing
    ///   to read here. A static, un-swelling block is the identity case of the
    ///   swell formula (`getSwellAmount` at a fuse this function cannot see is
    ///   indistinguishable from "not yet swelling"), not a fabricated value.
    /// * **No white "isLit" flash.** Same root cause: vanilla blinks
    ///   `submitWhiteSolidBlock`'s overlay on/off every 5 ticks of the fuse,
    ///   and [`MovingBlock`] carries no tint/overlay channel at all today — see
    ///   its own doc for why (this seam's producers so far have been fully
    ///   opaque, un-tinted geometry). Adding one is a `lodestone-render`
    ///   change, out of scope here.
    ///
    /// Both are cosmetic: the block that draws is the *correct* one, at the
    /// *correct* pose, for the whole 80-tick fuse — the swell and the flash
    /// are polish on top of a real TNT block rather than the difference
    /// between a TNT block and nothing.
    fn merge_primed_tnt(
        &self,
        model: &ModelRenderer,
        entities: &[EntityDraw],
        frustum: &Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let Some(state_id) = lodestone_data::block_states::state_id("minecraft:tnt") else {
            return;
        };
        for draw in entities {
            if draw.type_path != PRIMED_TNT_TYPE_PATH {
                continue;
            }
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(1.0),
                draw.feet + glam::Vec3::splat(1.0),
            ) {
                continue;
            }
            // Sampled at the same height `primed_tnt_pose` renders the block
            // centred on (`translate(0, 0.5, 0)` above the entity's feet), not
            // at the feet themselves — the same reasoning `FALLING_BLOCK_HEIGHT`
            // documents: a resting block's feet cell can read as solid (dark)
            // right as it is about to draw there.
            let light = self
                .entity_light
                .sample(draw.feet + glam::Vec3::new(0.0, 0.5, 0.0));
            if self.merge_moving_block(
                model,
                MovingBlock {
                    state_id,
                    transform: primed_tnt_pose(draw.feet),
                    light,
                },
                combined,
            ) {
                stats.moving_blocks_drawn += 1;
            }
        }
    }

    /// Merge every minecart's displayed contents — vanilla's
    /// `AbstractMinecartRenderer.submit`'s `displayBlockModel` branch.
    ///
    /// # Why this belongs beside [`merge_primed_tnt`] and not in the entity pass
    ///
    /// The cart **frame** does go through the ordinary cuboid-rig entity pass
    /// (`gpu/entity_passes.rs`'s `prepare_entities`, via
    /// `lodestone_render::entity::model_for_type`'s `"minecart"` corpus entry) —
    /// unlike primed TNT, a minecart genuinely has a baked rig. But the
    /// **contents** are a block model, not a second rig, so drawing them wants
    /// this seam's block-model machinery exactly as the falling-block and
    /// primed-TNT producers do, not a second corpus entry.
    ///
    /// # Which subtypes draw something
    ///
    /// [`default_minecart_contents`] is `None` for `minecraft:minecart` itself
    /// (`AbstractMinecart`'s own default display state is `Blocks.AIR`), so a
    /// plain cart contributes nothing here — matching vanilla's "the plain cart
    /// draws nothing inside" exactly, with no separate air-state special case.
    fn merge_minecart_contents(
        &self,
        model: &ModelRenderer,
        entities: &[EntityDraw],
        frustum: &Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        for draw in entities {
            let Some((block, display_offset)) = default_minecart_contents(&draw.type_path) else {
                continue;
            };
            let Some(state_id) = lodestone_data::block_states::state_id(block) else {
                continue;
            };
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(1.0),
                draw.feet + glam::Vec3::splat(1.0),
            ) {
                continue;
            }
            // Same reasoning as `merge_primed_tnt`'s probe: sample above the
            // feet, at roughly the content block's own height, not at the feet
            // themselves.
            let light = self
                .entity_light
                .sample(draw.feet + glam::Vec3::new(0.0, 0.5, 0.0));
            if self.merge_moving_block(
                model,
                MovingBlock {
                    state_id,
                    transform: minecart_content_pose(draw.feet, draw.yaw, display_offset),
                    light,
                },
                combined,
            ) {
                stats.moving_blocks_drawn += 1;
            }
        }
    }

    /// Merge every moving piston in range — vanilla's `PistonHeadRenderer`.
    ///
    /// # Two requests per piston, and only one of them is offset
    ///
    /// `submit` translates the pose by `(xOff, yOff, zOff)`, submits
    /// [`MovingPistonSpawn::state_id`](lodestone_render::MovingPistonSpawn::state_id),
    /// then **pops the pose** before submitting the base. So a retracting source
    /// piston draws its synthesised head pulled back toward the base *and* its own
    /// base block sitting still at the block entity's own cell — the popped pose is
    /// the reason the base does not slide with the head, and folding both into one
    /// transform is the mistake that makes a retracting sticky piston look like it
    /// is eating itself.
    ///
    /// The base is drawn only when there is a head to draw, matching `submit`'s
    /// nested `if (state.base != null)` inside `if (state.block != null)` — an
    /// ordering that cannot be flattened, because the spawn carries no base at all
    /// unless the head branch produced one.
    ///
    /// # Which branch synthesised the state is not this layer's business
    ///
    /// `extractRenderState` has three arms and two of them build a state that is
    /// nowhere in the world (a `piston_head` whose `short` follows the progress).
    /// All of that is block-state arithmetic against
    /// [`lodestone_data::block_states`], so it lives in the gather
    /// (`crate::block_entities::moving_piston_spawns`) and arrives here already
    /// resolved. This function is a transform and a frustum test.
    ///
    /// # Culling
    ///
    /// A two-cell box around the block entity's own cell rather than a one-cell
    /// one: the offset moves geometry up to a full cell away from `pos` in either
    /// direction along the push axis, so the falling-block producer's
    /// `feet ± 1.0` slack would clip a head at the far end of its travel.
    fn merge_piston_heads(
        &self,
        model: &ModelRenderer,
        eye: glam::Vec3,
        frustum: &Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        for spawn in self.moving_piston_source.pistons(eye) {
            let cell = glam::Vec3::new(
                spawn.pos[0] as f32,
                spawn.pos[1] as f32,
                spawn.pos[2] as f32,
            );
            if !frustum.intersects_aabb(cell - glam::Vec3::splat(2.0), cell + glam::Vec3::splat(2.0))
            {
                continue;
            }
            if self.merge_moving_block(
                model,
                MovingBlock {
                    state_id: spawn.state_id,
                    transform: piston_head_pose(
                        spawn.pos,
                        spawn.direction,
                        spawn.progress,
                        spawn.extending,
                    ),
                    light: spawn.light,
                },
                combined,
            ) {
                stats.moving_blocks_drawn += 1;
            }
            // The base, unoffset — `submit` pops the pose first. `from_translation`
            // of the bare cell corner is `piston_head_pose` with a zero offset, and
            // is written out rather than called with `progress = 1.0` so that a
            // future change to the offset rule cannot silently start moving it.
            let Some(base_state_id) = spawn.base_state_id else {
                continue;
            };
            if self.merge_moving_block(
                model,
                MovingBlock {
                    state_id: base_state_id,
                    transform: glam::Mat4::from_translation(cell),
                    light: spawn.base_light,
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

    /// `getExtendedProgress`'s two arms, predicted exactly, at an input where the
    /// plausible wrong reading **disagrees**.
    ///
    /// The wrong hypothesis is "the offset is `direction * progress`, with the
    /// retraction handled by using the movement direction instead" — which is what
    /// you get by reading `getXOff` as `step * getProgress(a)` and forgetting
    /// `getExtendedProgress` entirely. Evaluated here rather than described:
    ///
    /// | progress | extending | correct | wrong hypothesis |
    /// |---|---|---|---|
    /// | 0.25 | yes | `-0.75` | `+0.25` |
    /// | 0.25 | no | `+0.75` | `-0.25` (movement dir is `-direction`) |
    /// | 0.5 | either | `∓0.5` | `±0.5` — **agrees in magnitude**, so useless |
    ///
    /// The last row is why `0.25` is the input and `0.5` is not: at half progress
    /// the two hypotheses differ only in sign, and a sign flip on a symmetric
    /// contraption is exactly the mistake a screenshot cannot catch.
    #[test]
    fn the_extended_progress_is_signed_and_disagrees_with_the_raw_progress_reading() {
        let mut wrong: Vec<String> = Vec::new();
        // Predicted values, from `extending ? progress - 1 : 1 - progress`.
        for (progress, extending, expected) in [
            (0.0_f32, true, -1.0_f32),
            (0.25, true, -0.75),
            (1.0, true, 0.0),
            (0.0, false, 1.0),
            (0.25, false, 0.75),
            (1.0, false, 0.0),
        ] {
            let got = piston_extended_progress(progress, extending);
            if (got - expected).abs() > 1e-6 {
                wrong.push(format!(
                    "progress {progress} extending {extending}: expected {expected}, got {got}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");

        // Both arms land on exactly 0 at full progress — the property that puts the
        // moved block on its destination cell rather than one short of it.
        assert_eq!(piston_extended_progress(1.0, true), 0.0);
        assert_eq!(piston_extended_progress(1.0, false), 0.0);
    }

    /// The control for the gate above: at `progress == 0.5` the wrong hypothesis is
    /// indistinguishable from the truth by magnitude, so the chosen input must not
    /// be `0.5`. Run, and required to hold, rather than asserted in prose.
    #[test]
    fn half_progress_cannot_discriminate_the_offset_rule() {
        // The wrong reading: raw progress along the movement direction, which is
        // `direction` while extending and `-direction` while retracting.
        let wrong = |progress: f32, extending: bool| {
            if extending { progress } else { -progress }
        };
        for extending in [true, false] {
            let correct = piston_extended_progress(0.5, extending);
            assert!(
                (correct.abs() - wrong(0.5, extending).abs()).abs() < 1e-6,
                "control failed: at progress 0.5 the two hypotheses already differ \
                 in magnitude for extending={extending}, so the 0.25 input in the \
                 gate above was not necessary and this control is measuring the \
                 wrong thing"
            );
            // And at 0.25 they must differ, or the gate above proves nothing.
            let correct_q = piston_extended_progress(0.25, extending);
            assert!(
                (correct_q - wrong(0.25, extending)).abs() > 0.4,
                "control failed: at progress 0.25 the hypotheses agree for \
                 extending={extending}"
            );
        }
    }

    /// A piston head at **full** progress sits exactly on the block entity's own
    /// cell corner, and at a mid-progress it sits a predicted fraction of a cell
    /// back along the push axis.
    ///
    /// Both halves are needed. The first is what makes the moved block line up with
    /// the terrain grid the instant the server replaces the cell — a half-cell error
    /// there reads as a model-origin quirk exactly as it does for a falling block.
    /// The second is what proves the axis and the sign.
    ///
    /// The cell has three distinct coordinates and mixed signs so an axis swap or a
    /// truncating cast cannot pass.
    #[test]
    fn the_piston_pose_lands_on_the_cell_at_full_progress_and_a_predicted_fraction_before_it() {
        const CELL: [i32; 3] = [-13, 70, 4];
        let corner = glam::Vec3::new(-13.0, 70.0, 4.0);
        // `UP`'s step, i.e. `facing=up`.
        const UP: [i32; 3] = [0, 1, 0];

        let landed = piston_head_pose(CELL, UP, 1.0, true).transform_point3(glam::Vec3::ZERO);
        assert!(
            (landed - corner).length() < 1e-5,
            "at full progress the head must sit on its own cell, got {landed}"
        );

        // Extending, quarter progress: 0.75 of a cell *below* the destination.
        let mid = piston_head_pose(CELL, UP, 0.25, true).transform_point3(glam::Vec3::ZERO);
        assert!(
            (mid - (corner - glam::Vec3::new(0.0, 0.75, 0.0))).length() < 1e-5,
            "expected 0.75 below the cell, got {mid}"
        );

        // Retracting, quarter progress: 0.75 of a cell *above* it — the same
        // magnitude, the opposite side, which is the whole content of the sign rule.
        let mid_back = piston_head_pose(CELL, UP, 0.25, false).transform_point3(glam::Vec3::ZERO);
        assert!(
            (mid_back - (corner + glam::Vec3::new(0.0, 0.75, 0.0))).length() < 1e-5,
            "expected 0.75 above the cell, got {mid_back}"
        );

        // The scale is unit: the far corner is one cell away on every axis, so the
        // pose is a translation and not a scaled one.
        let far = piston_head_pose(CELL, UP, 1.0, true).transform_point3(glam::Vec3::ONE);
        assert!((far - (corner + glam::Vec3::ONE)).length() < 1e-5, "{far}");
    }

    /// A block entity's pose is at its cell **corner**, so the falling block's
    /// mandatory `-0.5` x/z shift must *not* appear here.
    ///
    /// The control that makes the gate above discriminating: applying
    /// [`falling_block_pose`] to the same cell lands somewhere else entirely, and
    /// half a cell diagonally is precisely the error that still looks like a piston.
    #[test]
    fn the_piston_pose_is_not_the_falling_block_pose() {
        const CELL: [i32; 3] = [-13, 70, 4];
        let corner = glam::Vec3::new(-13.0, 70.0, 4.0);
        let piston = piston_head_pose(CELL, [0, 1, 0], 1.0, true).transform_point3(glam::Vec3::ZERO);
        let falling = falling_block_pose(corner).transform_point3(glam::Vec3::ZERO);
        let d = (piston - falling).length();
        assert!(
            d > 0.5,
            "control failed: the two poses are {d} apart, so the corner-vs-centre \
             distinction the gate above rests on is not being measured"
        );
    }

    /// `primed_tnt_pose` rotates the block about its own centre, half a block
    /// above the entity's feet, and the rotation genuinely happens.
    ///
    /// Two properties, computed rather than assumed (values cross-checked
    /// against a standalone Python transcription of the same four-matrix
    /// chain, not derived from this file):
    ///
    /// * The block-local cube centre `(0.5, 0.5, 0.5)` — the one point every
    ///   term of `TntRenderer.submit`'s rotate-about-centre dance keeps fixed —
    ///   lands at exactly `feet + (0, 0.5, 0)`. Getting only the outer
    ///   `translate(0, 0.5, 0)` right and dropping the dance would still pass a
    ///   test that only checked this point, which is why the second assertion
    ///   exists.
    /// * Local `(1, 0, 0)` lands at `feet + (0.5, 0.0, -0.5)`, not at
    ///   `feet + (0.5, 0.0, 0.5)` — the value an implementation that kept only
    ///   `translate(-0.5, -0.5, 0.5)` and dropped both `Ry` calls would produce.
    ///   The two disagree on `z`'s sign, which a "some pose was produced"
    ///   check cannot see but this one does.
    #[test]
    fn the_primed_tnt_pose_rotates_about_its_own_centre_half_a_block_above_the_feet() {
        let feet = glam::Vec3::new(4.0, 70.0, 9.0);
        let pose = primed_tnt_pose(feet);

        let centre = pose.transform_point3(glam::Vec3::splat(0.5));
        let expected_centre = feet + glam::Vec3::new(0.0, 0.5, 0.0);
        assert!(
            (centre - expected_centre).length() < 1e-4,
            "block centre landed at {centre}, expected {expected_centre}"
        );

        let rotated = pose.transform_point3(glam::Vec3::new(1.0, 0.0, 0.0));
        let expected_rotated = feet + glam::Vec3::new(0.5, 0.0, -0.5);
        let unrotated_wrong = feet + glam::Vec3::new(0.5, 0.0, 0.5);
        assert!(
            (rotated - expected_rotated).length() < 1e-4,
            "local (1,0,0) landed at {rotated}, expected {expected_rotated} \
             (got the no-rotation hypothesis {unrotated_wrong} instead: the two \
             `Ry` calls did not fire)"
        );
    }

    /// A zero direction step — which no real `facing` byte produces, but a decode
    /// bug would — degrades to drawing on the block entity's own cell rather than
    /// to a NaN or an off-world translation.
    #[test]
    fn a_zero_direction_draws_on_the_cell_itself() {
        let p = piston_head_pose([2, 3, 4], [0, 0, 0], 0.25, true)
            .transform_point3(glam::Vec3::ZERO);
        assert_eq!(p, glam::Vec3::new(2.0, 3.0, 4.0));
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

    /// `minecart_content_pose`'s bob+yaw prefix: the vertical component comes
    /// only from the `0.375` bob and the `(displayOffset-8)/16` term, both
    /// `Y`-only, so rotating about `Y` must never move it — and the rotation
    /// genuinely has to happen, or the content would sit dead centre in every
    /// cart regardless of heading.
    #[test]
    fn the_minecart_content_pose_keeps_the_bob_height_across_yaw_and_actually_rotates() {
        let feet = glam::Vec3::new(4.0, 70.0, -9.0);
        // Chest minecart: displayOffset 8, so `(8 - 8) / 16 == 0` — the bob
        // alone decides the height, with no offset term to obscure the check.
        let offset = 8;
        let expected_y = feet.y + 0.375;

        let mut xz_points = Vec::new();
        for yaw in [0.0_f32, 37.0, 90.0, 180.0, -55.0] {
            let p = minecart_content_pose(feet, yaw, offset).transform_point3(glam::Vec3::ZERO);
            assert!(
                (p.y - expected_y).abs() < 1e-4,
                "yaw {yaw}: content height was {}, expected {expected_y} (feet.y + 0.375) — \
                 rotation about Y must not move the vertical component",
                p.y
            );
            xz_points.push(glam::Vec2::new(p.x, p.z));
        }
        // And it does actually rotate: not every yaw can land on the same x/z,
        // or the `rotate(180 - yaw_deg)` term above is not being applied.
        let distinct = xz_points
            .windows(2)
            .any(|w| (w[0] - w[1]).length() > 1e-3);
        assert!(
            distinct,
            "content pose's x/z did not change across yaw values {xz_points:?} — the \
             rotate(180 - yaw) term is not firing"
        );
    }

    /// The `(displayOffset - 8) / 16` term, re-derived independently for each
    /// subtype's real default offset (chest 8, furnace/TNT 6, hopper 1) and
    /// checked against the pose's own output — not merely "some offset was
    /// applied", but the exact vanilla arithmetic.
    #[test]
    fn the_display_offset_term_matches_vanillas_displayoffset_minus_8_over_16() {
        let feet = glam::Vec3::ZERO;
        let y_for = |offset: i32| minecart_content_pose(feet, 0.0, offset).transform_point3(glam::Vec3::ZERO).y;
        let chest_y = y_for(8);
        let hopper_y = y_for(1);
        let furnace_y = y_for(6);

        assert!(
            (chest_y - 0.375).abs() < 1e-5,
            "chest (offset 8): expected bob-only 0.375, got {chest_y}"
        );
        assert!(
            (hopper_y - (0.375 + (1.0 - 8.0) / 16.0)).abs() < 1e-5,
            "hopper (offset 1): got {hopper_y}, expected {}",
            0.375 + (1.0 - 8.0) / 16.0
        );
        assert!(
            (furnace_y - (0.375 + (6.0 - 8.0) / 16.0)).abs() < 1e-5,
            "furnace/TNT (offset 6): got {furnace_y}, expected {}",
            0.375 + (6.0 - 8.0) / 16.0
        );
        // Distinct, not coincidentally equal — the offset term genuinely has to
        // vary the height, or this whole gate is measuring the bob alone.
        assert_ne!(chest_y, hopper_y);
        assert_ne!(chest_y, furnace_y);
        assert_ne!(hopper_y, furnace_y);
    }

    /// A synthetic top-face quad, block-local `0..1` space — the same shape
    /// `crack_resolver.rs`'s own `cube_top()` fixture uses. Not a fetched
    /// `client.jar`'s real geometry (`block_models_gate.rs`'s job): this test
    /// exists to prove the minecart-contents *wiring* reaches real
    /// world-space quads, not that a chest looks like a chest.
    fn synthetic_top_quad() -> lodestone_assets::BakedQuad {
        lodestone_assets::BakedQuad {
            positions: [
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, 0.0],
            ],
            uvs: [[0.0; 2]; 4],
            direction: lodestone_assets::Direction::Up,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
        }
    }

    /// The island this agent closed, at the geometry level: every
    /// content-bearing minecart subtype reaches real, bounded world-space
    /// quads through the production pipeline
    /// (`default_minecart_contents` → `lodestone_data::block_states::state_id`
    /// → `CrackResolver::state_quads` → `minecart_content_pose` →
    /// `mesh_moving_block_quads`) — and the plain `minecraft:minecart` is the
    /// **negative control**, run through the exact same code, producing none.
    ///
    /// Before this change `default_minecart_contents` did not exist at all, so
    /// every subtype (plain included) took this file's `None` arm — the
    /// assertions below would have failed identically for chest/furnace/
    /// tnt/hopper, which is what makes the plain-cart control meaningful
    /// rather than vacuously true: it is not "nothing here draws", it is
    /// "these four draw and this one specifically does not".
    #[test]
    fn minecart_contents_reach_quads_and_the_plain_cart_does_not() {
        let subtypes: [(&str, &str, i32); 4] = [
            ("chest_minecart", "minecraft:chest[facing=north]", 8),
            (
                "furnace_minecart",
                "minecraft:furnace[facing=north,lit=false]",
                6,
            ),
            ("tnt_minecart", "minecraft:tnt", 6),
            ("hopper_minecart", "minecraft:hopper", 1),
        ];

        let ids: Vec<u32> = subtypes
            .iter()
            .map(|(_, block, _)| {
                lodestone_data::block_states::state_id(block)
                    .unwrap_or_else(|| panic!("{block} must resolve to a real state id"))
            })
            .collect();
        let mut quads = vec![Vec::new(); ids.iter().copied().max().unwrap() as usize + 1];
        for &id in &ids {
            quads[id as usize] = vec![synthetic_top_quad()];
        }
        let resolver = lodestone_render::crack_resolver::CrackResolver::new(
            quads,
            [[0.0; 4]; lodestone_render::CRACK_STAGE_COUNT],
        );

        let feet = glam::Vec3::new(4.0, 70.0, -9.0);
        let yaw = 35.0;
        let expected_min = feet - glam::Vec3::splat(1.0);
        let expected_max = feet + glam::Vec3::splat(2.0);

        let mut bad = Vec::new();
        for (type_path, block, offset) in subtypes {
            let (mapped_block, mapped_offset) = default_minecart_contents(type_path)
                .unwrap_or_else(|| panic!("{type_path} must resolve to a default content block"));
            if mapped_block != block || mapped_offset != offset {
                bad.push(format!(
                    "{type_path}: mapped to ({mapped_block}, {mapped_offset}), expected \
                     ({block}, {offset})"
                ));
                continue;
            }

            let state_id = lodestone_data::block_states::state_id(mapped_block).unwrap();
            let src_quads = resolver.state_quads(state_id);
            let pose = minecart_content_pose(feet, yaw, mapped_offset);
            let mesh = mesh_moving_block_quads(src_quads, pose, 0xF0);
            if mesh.vertices.is_empty() {
                bad.push(format!("{type_path}: produced zero vertices — the island reopened"));
                continue;
            }
            let mut min = glam::Vec3::splat(f32::INFINITY);
            let mut max = glam::Vec3::splat(f32::NEG_INFINITY);
            for v in &mesh.vertices {
                let p = glam::Vec3::from(v.position);
                min = min.min(p);
                max = max.max(p);
            }
            if min.cmplt(expected_min).any() || max.cmpgt(expected_max).any() {
                bad.push(format!(
                    "{type_path}: content quad bounds [{min}, {max}] fall outside the \
                     expected box [{expected_min}, {expected_max}] around feet {feet} — \
                     the pose put the content somewhere implausible, not just \"off\""
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "{} of {} minecart contents are wrong: {bad:#?}",
            bad.len(),
            subtypes.len()
        );

        // The negative control: the plain cart's own type path maps to no
        // content block at all, so it never even reaches `state_id` or the
        // pose function. If this returned `Some`, the corpus-alias fix in
        // `entity.rs` would be drawing a block inside a cart vanilla renders
        // empty.
        assert!(
            default_minecart_contents("minecart").is_none(),
            "the plain cart must not resolve to any content block"
        );
    }
}
