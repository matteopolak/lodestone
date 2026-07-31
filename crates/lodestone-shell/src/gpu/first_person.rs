//! The first-person hand pass: the bare arm or the held item, drawn in its
//! own render pass with the depth buffer cleared (vanilla's
//! `GameRenderer.renderLevel` does the same before `renderItemInHand`). See
//! [`RenderState::prepare_first_person_hand`] for the vanilla parity notes
//! and `docs/arm-swing-animation.md`.
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, GpuEntityModel, GpuModelMesh,
    entity::{
        Arm, first_person_arm_parts, first_person_arm_pose, first_person_item_mesh,
        hand_projection, hand_transform, model_for_type,
    },
    fog::FogUniform,
    update_model_shared_camera_buffer, upload_instances,
};

use super::{RenderState, RenderStats};

/// What the first-person hand pass draws this frame: the held item's model, or
/// the bare arm. **Never both** — see
/// [`RenderState::prepare_first_person_hand`], which is vanilla's own
/// `isEmpty()` branch.
pub(super) enum FirstPersonHand<'a> {
    /// The held item, meshed camera-space and drawn through the *model* pipeline
    /// with the model pass's own `hand_cam_bind_group`.
    Item(GpuModelMesh),
    /// The bare arm, drawn through the *entity* pipeline.
    Arm(FirstPersonArm<'a>),
}

/// The first-person arm's draw for one frame: the uploaded `player_wide` mesh and
/// texture (borrowed — they are uploaded once at startup), plus one
/// single-instance buffer per drawn part.
///
/// Only the arm and its sleeve are listed. Both carry the *same* matrix, so this
/// is two draw calls over one pose and not a pose per part.
pub(super) struct FirstPersonArm<'a> {
    model: &'a GpuEntityModel,
    texture: &'a wgpu::BindGroup,
    parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)>,
}

impl RenderState {
    /// Build this frame's first-person hand draw — **the held item, or the bare
    /// arm**, never both.
    ///
    /// # Which one, and why it is exclusive
    ///
    /// Vanilla's `ItemInHandRenderer.submitArmWithItem` branches on
    /// `itemStack.isEmpty()`: the empty hand gets `renderPlayerArm`, and a
    /// non-empty one gets the *item* through `applyItemArmTransform` **with no arm
    /// drawn at all**. So this returns a [`FirstPersonHand`] and the caller draws
    /// exactly one of its two variants. Drawing both — the tempting "add the item
    /// on top of the arm" reading — puts an item model inside the wrist.
    ///
    /// [`MainHandSource`] decides. Unset yields `None` and the bare-arm branch,
    /// which is what this shell did before the item path existed. An item that is
    /// held but has no baked geometry (a `IconPart::Special` chest or shield) also
    /// falls back to the arm rather than to nothing: vanilla would draw the special
    /// renderer, and a bare arm is closer to that than an empty screen.
    ///
    /// Also rewrites the arm pass's group-0 uniform. That uniform's `view_proj`
    /// is [`hand_projection`] — **the projection alone** — because
    /// `GameRenderer.renderItemInHand` multiplies the pose stack by
    /// `modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)`,
    /// and the shader evaluates `Proj · ModelViewStack · PoseStack`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead would leave the arm parked at the
    /// world origin, visible only when the player stands on it.
    ///
    /// # Unconditional, and why that is right rather than lazy
    ///
    /// This is not gated on anything. `RenderState::render` is only reached
    /// in-world (`app.rs` returns early for every menu screen) and the shell has
    /// no third-person camera, so "first person, in a world" is exactly when this
    /// function runs. Making it opt-in would have needed a setter on `&mut self`
    /// and therefore an `app.rs` call — i.e. it would have shipped as another
    /// zero-pixel island.
    ///
    /// # The swing
    ///
    /// The pose is driven by [`HandSwingSource`] — vanilla's `attackValue`, a
    /// tick-advanced clock read with this frame's partial tick. It is polled here
    /// rather than passed in for the same reason the light and sky-darken samplers
    /// are: `render` takes only `&[EntityDraw]`, and the local player is not in it.
    ///
    /// **With no source installed this is `0.0` and the arm is rested**, which is
    /// the state to suspect first if a swing does not appear — the pass runs and
    /// `first_person_arm_drawn` is `true` either way, so a missing
    /// `set_hand_swing_source` looks exactly like a working rested arm. See
    /// `docs/arm-swing-animation.md`.
    ///
    /// # The two remaining fidelity gaps, both missing *shell state*, not code
    ///
    /// * **`bobView` / `bobHurt` and the `xBob`/`yBob` view lag are absent.** All
    ///   need per-tick player state the shell does not track (walk distance, hurt
    ///   time, the two smoothed view angles). All are the identity standing still.
    /// * **`equipProgress` is absent**, so the arm never dips and rises on a
    ///   hotbar change: `inverseArmHeight` is `swapAnimationScale(item) * (1 -
    ///   lerp(oHeight, height))` and the shell tracks neither height.
    ///
    /// The rig is `player_wide` unconditionally — the shell has no skin-model
    /// signal, and `canonical_model_name` already maps `"player"` to it.
    pub(super) fn prepare_first_person_hand<'a>(
        &'a self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
    ) -> Option<FirstPersonHand<'a>> {
        const ARM: Arm = Arm::Right;

        // Group 0 for *both* branches: `hand_projection` alone. Written before
        // either branch can return, so the arm's uniform is never left holding a
        // stale projection from a frame that drew an item (and vice versa).
        self.write_hand_camera(queue, camera);

        // The item branch first: it needs no entity rig at all, so a missing
        // `player_wide` mesh must not silently suppress a held item too.
        if let Some(item) = self.main_hand.value()
            && let Some(model) = self.model.as_ref()
            && let Some(geometry) = model.items.get(&item)
        {
            // `true`: the *first-person* hand slot. `false` here reads
            // `thirdperson_righthand`, a different rotation and scale, and puts
            // the item at a plausible-but-wrong angle rather than off screen.
            let transform = hand_transform(&geometry.display, ARM, true);
            let mesh = first_person_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                ARM,
                self.hand_swing.value(),
                // `inverseArmHeight` — the equip/swap dip. Zero: the shell tracks
                // neither `mainHandHeight` nor its previous-tick value, the same
                // gap the arm branch documents.
                0.0,
                &transform,
                u8::try_from(self.hand_light(camera)).unwrap_or(u8::MAX),
            );
            if let Some(gpu) = GpuModelMesh::upload(device, &mesh) {
                return Some(FirstPersonHand::Item(gpu));
            }
        }

        let entry = model_for_type("player")?;
        let mesh = self.entities.models.get(entry.name)?;
        let gpu = self.entities.gpu_models.get(entry.name)?;
        let texture = self.entities.textures.get(entry.name)?;
        let pose = first_person_arm_pose(mesh, ARM, self.hand_swing.value())?;

        let light = self.hand_light(camera);

        let parts: Vec<(lodestone_render::entity::PartRange, wgpu::Buffer)> =
            first_person_arm_parts(mesh, ARM)
                .into_iter()
                .filter_map(|index| {
                    let range = *gpu.parts.get(index)?;
                    if range.index_count == 0 {
                        return None;
                    }
                    // One instance, and the *same* matrix for arm and sleeve —
                    // `right_sleeve` is a `PartPose::ZERO` child of `right_arm`,
                    // so they share it exactly.
                    let buffer = upload_instances(device, &[pose], &[light])?;
                    Some((range, buffer))
                })
                .collect();
        if parts.is_empty() {
            return None;
        }

        Some(FirstPersonHand::Arm(FirstPersonArm {
            model: gpu,
            texture,
            parts,
        }))
    }

    /// The packed light byte the first-person hand is lit with, for both branches.
    ///
    /// Exactly `renderItemInHand`'s `getPackedLightCoords(minecraft.player, …)`,
    /// sampled at the **eye** rather than the feet: it is what the player is
    /// looking through, and the two only differ standing in a doorway.
    #[must_use]
    fn hand_light(&self, camera: &Camera) -> u32 {
        u32::from(self.entity_light.sample(camera.position))
    }

    /// Rewrite both hand passes' group-0 uniforms with [`hand_projection`].
    ///
    /// **The projection alone, with no view matrix**, because
    /// `GameRenderer.renderItemInHand` multiplies the pose stack by
    /// `modelViewMatrix.invert()` while pushing `modelViewStack.mul(modelViewMatrix)`
    /// and the shader evaluates `Proj · ModelViewStack · PoseStack`: the view
    /// rotation cancels exactly, leaving a camera-space pose. Feeding
    /// `Camera::view_projection` here instead parks the hand at the world origin,
    /// visible only when the player stands on it.
    ///
    /// Two buffers, one value: the entity pipeline (bare arm) and the model
    /// pipeline (held item) declare different group-0 layouts, so each needs its
    /// own. Written together here so they cannot drift.
    fn write_hand_camera(&self, queue: &wgpu::Queue, camera: &Camera) {
        let camera_uniform = CameraUniform {
            view_proj: hand_projection(camera.aspect).to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        };
        // No distance fog on the hand for either branch (vanilla does not fog
        // it either, and at ~0.7 blocks it could contribute nothing), but the
        // sky-darken lane still rides along — the same lane `fog_with_clock`
        // sets for terrain and mobs, so the hand cannot disagree with the
        // world about what time it is.
        //
        // **Both branches must read this from the same place.** Before this,
        // the arm's uniform carried it (via `EntityCameraUniform::
        // with_sky_darken`) and the item's did not: `update_model_shared_
        // camera_buffer` was called with a bare `FogUniform::disabled()`,
        // which leaves the spare lane at its `0.0`/"unwired" sentinel, and the
        // model shader's `sky_darken()` reads that sentinel as permanent
        // noon. That was issue #74's actual bug — not a missing light sample
        // (`hand_light` already samples real per-position world light for
        // both branches; see its own doc), but the held item's sky component
        // never darkening: at night, in the open, the item stayed lit as if
        // it were noon while the arm right next to it correctly dimmed.
        let mut hand_fog = FogUniform::disabled();
        hand_fog.end_enabled[2] = self.sky_darken.value();
        queue.write_buffer(
            &self.entities.hand_cam_buffer,
            0,
            bytemuck::bytes_of(&EntityCameraUniform {
                camera: camera_uniform,
                fog: hand_fog,
            }),
        );
        if let Some(model) = self.model.as_ref() {
            // The origin binding is untouched here: it always points at the
            // shared arena's reserved zero slot (see the draw site), so only
            // the shared view_proj/fog half needs rewriting.
            update_model_shared_camera_buffer(
                queue,
                &model.hand_cam_buffer,
                camera_uniform.view_proj,
                hand_fog,
            );
        }
    }

    /// Record the first-person arm/held-item pass: its own render pass, with
    /// the depth buffer cleared.
    ///
    /// Vanilla does exactly this, and it is not an optimisation detail:
    /// `GameRenderer.renderLevel` calls
    /// `clearDepthTexture(mainRenderTarget.getDepthTexture(), 0.0)`
    /// immediately before `renderItemInHand`. Vanilla's depth is reversed-Z,
    /// so its `0.0` is *far*; ours is `[0,1]` DirectX-style, so the equivalent
    /// clear value is `1.0`. (This is the sign flip `CLAUDE.md` warns about,
    /// applied to a clear rather than a comparison.)
    ///
    /// Without the clear the arm would be occluded by any block within ~0.75
    /// blocks of the eye — standing in a doorway, or facing the block you are
    /// mining — because the arm genuinely *is* inside that geometry. The
    /// colour attachment loads rather than clears, so the world stays.
    pub(super) fn draw_first_person_hand(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        hand: &FirstPersonHand<'_>,
        stats: &mut RenderStats,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("first-person hand pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        match hand {
            // The held item is item-model geometry, so it draws through the
            // *model* pipeline with that pipeline's four bind groups — the
            // same atlas, palette and animation slots the terrain and the
            // hotbar icons use. Only group 0 differs: the hand projection.
            FirstPersonHand::Item(mesh) => {
                if let Some(model) = self.model.as_ref() {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    // The held item's pose is already camera-space (see
                    // `write_hand_camera`'s doc), so like the dropped-item
                    // pass it has no origin of its own: the shared arena's
                    // reserved zero slot.
                    pass.set_bind_group(
                        0,
                        &model.hand_cam_bind_group,
                        &[model.origin_arena.zero_offset()],
                    );
                    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }
            FirstPersonHand::Arm(arm) => {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                // The *hand* camera uniform: `hand_projection` alone, because
                // the arm pose is already camera-space. Binding the world one
                // here would leave the arm sitting at the world origin.
                pass.set_bind_group(0, &self.entities.hand_cam_bind_group, &[]);
                pass.set_bind_group(1, arm.texture, &[]);
                pass.set_vertex_buffer(0, arm.model.vertices.slice(..));
                pass.set_index_buffer(arm.model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for (range, buffer) in &arm.parts {
                    pass.set_vertex_buffer(1, buffer.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }
        }
    }
}
