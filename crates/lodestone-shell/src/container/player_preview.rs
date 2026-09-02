//! [`PlayerPreview`] — the GPU half of the inventory avatar: the player rig
//! drawn into the inventory panel's recess, head tracking the cursor
//! (vanilla's `InventoryScreen.extractEntityInInventoryFollowsMouse`).
//!
//! ## What it is
//!
//! Before this existed, the recess at `(leftPos + 26, topPos + 8)` was the
//! *hole in vanilla's own `inventory.png`* with nothing rendered into it — a
//! black box where the player belongs. The player report was exactly that.
//!
//! All the pose arithmetic lives in
//! [`lodestone_render::gui_entity`](lodestone_render::gui_entity), which is where
//! the record definition is transcribed and gated. This module is only the GPU
//! side: one [`EntityPipeline`], the baked player rig, the skin sheet, a camera
//! uniform and one scissored pass.
//!
//! ## Three decisions worth knowing before changing this
//!
//! **1. Its own [`EntityPipeline`], not the world renderer's.** `ContainerRenderer`
//! is constructed independently of `gpu::RenderState` and receives only a depth
//! view and a `BlockModels` borrow, so reaching the world's `EntityRenderer` would
//! mean threading it through `app.rs`'s whole redraw path. The cost is one
//! pipeline object, one mesh upload and one 64×64 texture — the same trade
//! `ContainerRenderer::attach_items` already documents for the item atlas ("costs
//! a second upload of the (small) item atlas"). It is **not** a fifth bind group:
//! the entity shader still spends exactly two, camera and texture.
//!
//! **2. Its own camera buffer, unconditionally.** `queue.write_buffer` is ordered
//! against the **submit**, not against the encoder, so two passes sharing one
//! uniform buffer in a single submit both read the *last* value written — and
//! nothing fails loudly. This pass's `view_proj` is
//! [`gui_ortho`](lodestone_render::gui_ortho), the world entity pass's is a
//! perspective camera, and the world glint already paid for this exact mistake
//! once today. [`PlayerPreview`] therefore owns [`Self::cam_buffer`] outright and
//! never borrows one.
//!
//! **3. A GPU scissor, where every other GUI pass in this workspace clips on the
//! CPU.** `set_scissor_rect` appears nowhere else here (`menu/render/draw.rs`
//! documents at length why the menu pipeline does not have one), and this is the
//! one place where CPU clipping is not an option: the thing that overflows is a
//! *3-D rig* whose silhouette is a function of two look angles, so there are no
//! rows or quads to reject. Vanilla clips this by rendering to an offscreen
//! texture and blitting; the scissor is the cheap equivalent, and
//! `lodestone_render::gui_entity`'s module docs explain why the two agree for an
//! opaque pass.
//!
//! ## Configuration
//!
//! Nothing env-driven. Everything comes from the vanilla pack: no `client.jar`
//! means [`PlayerPreview::new`] returns `None` and the recess stays empty — the
//! same fail-open degradation `ContainerRenderer::attach_background` and
//! `gpu/entities.rs`'s armour sheets take, and for the same reason (a synthetic
//! flat-magenta humanoid in the inventory reads as a rendering bug, not as "no
//! pack found").

use glam::{Mat4, Vec3};
use lodestone_render::{
    AnimInput, CameraUniform, EntityCameraUniform, EntityInstance, EntityMesh, EntityModelSet,
    EntityPipeline, GpuEntityModel, entity_camera_buffer, entity_texture_candidates, fog::FogUniform,
    gui_entity::{
        GuiEntityLook, INVENTORY_OFFSET_Y, INVENTORY_RECT_OFFSET, INVENTORY_RECT_SIZE,
        INVENTORY_SIZE, gui_entity_anim, gui_entity_look, gui_entity_view,
    },
    gui_ortho, upload_instances,
};

use lodestone_assets::PlayerModelType;

use super::layout::Rect;

/// A standing player's `boundingBoxHeight`, in blocks — the number vanilla reads
/// off the render state and halves for `translation.y`.
///
/// Hardcoded rather than looked up through `lodestone_data::entity_dimensions`
/// deliberately: the *only* entity this pass ever draws is the local player, and
/// the dimensions table is keyed by network type id, which this module (which
/// never sees a packet) has no honest way to obtain. `EntityType.PLAYER` is
/// `0.6 × 1.8` and has been since 1.0; a crouching or swimming player has a
/// shorter box, but vanilla's inventory screen never shows one — `InventoryScreen`
/// is unreachable while `Pose` is anything but `STANDING`, because opening the
/// inventory releases the crouch.
const PLAYER_BB_HEIGHT: f32 = 1.8;

/// The rig to draw when nothing declares one — vanilla's own fallback, since
/// `PlayerModelType::byLegacyServicesName` resolves every absent or
/// unrecognised declaration to `WIDE` (see
/// [`lodestone_assets::PlayerModelType`]).
///
/// **This used to be a `const SLIM: bool = false` with a note saying "when
/// skins land, this is the one line that changes".** It has changed: the model
/// is now a runtime [`PlayerModelType`], settable through
/// [`PlayerPreview::set_skin`], and both rigs are reachable. What is still
/// fetch and account-scoped handoff are documented in `docs/player-skins.md`.
const DEFAULT_MODEL: PlayerModelType = PlayerModelType::Wide;

/// `leftPos + 73`, `topPos + 6` — the creative inventory tab's avatar recess origin.
/// See [`PlayerAvatar::creative`].
const CREATIVE_RECT_OFFSET: [f32; 2] = [73.0, 6.0];
/// `105 - 73` by `49 - 6`.
const CREATIVE_RECT_SIZE: [f32; 2] = [32.0, 43.0];
/// The creative call's `scale` argument, against `InventoryScreen`'s 30.
const CREATIVE_SIZE: f32 = 20.0;

/// The avatar's rect and the cursor that aims it, in **logical GUI pixels** —
/// what [`super::geometry::ContainerGeometry`] hands the draw.
///
/// The rect is carried rather than recomputed at draw time so the layout is
/// derived once, from the same panel origin every slot and label is measured
/// from. A control that restated `+26, +8` against its own idea of the origin
/// would be premise-false the moment the recipe book shifted the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerAvatar {
    /// The recess, logical GUI pixels, top-left origin.
    pub rect: Rect,
    /// Cursor position in the same logical space. [`Rect`]'s own centre when the
    /// caller has no cursor, which is [`GuiEntityLook::FORWARD`] — an avatar
    /// looking straight out, not one snapped to a corner.
    pub mouse: [f32; 2],
    /// The **live pose** to draw over, before the two look angles are folded in.
    ///
    /// Vanilla poses the *live* render state — `InventoryScreen` hands
    /// `extractEntityInInventoryFollowsMouse` the real player entity, so a
    /// sprinting player's inventory avatar really does have its arms mid-swing.
    /// [`AnimInput::REST`] is the honest default for a caller with no `Sim`
    /// (every hermetic gate, and `PlayerAvatar::new`).
    ///
    /// **Partially fed today, and the gap is a crate boundary rather than an
    /// omission.** `attack_anim` and `age_ticks` come off `Sim`'s public
    /// `hand_swing_progress()`/`tick_count()`; `limb_swing`/`limb_swing_amount`
    /// (the walk cycle) live on `Sim::body_pose`, a **private** field whose only
    /// public reader is `sim/camera.rs::third_person_body_state`, and that
    /// returns `None` in first person — which is the only camera mode the
    /// inventory screen is ever open in. Reaching it needs a small accessor in
    /// `sim/`; see `docs/inventory-player-preview.md`.
    pub pose: AnimInput,
    /// The `scale` argument `extractEntityInInventoryFollowsMouse` is called with —
    /// GUI pixels per block, which sets how large the avatar is drawn inside
    /// [`rect`](Self::rect).
    ///
    /// Carried rather than taken from [`INVENTORY_SIZE`] at draw time because the two
    /// screens that show an avatar do **not** agree on it: `InventoryScreen` passes
    /// `30`, and `CreativeModeInventoryScreen`'s inventory tab passes `20` into a
    /// smaller recess. Reading the constant would draw the creative avatar at the
    /// survival size and it would overflow its well.
    pub size: f32,
    /// The local player's own uuid, carried through from
    /// [`super::frame::ContainerFrame::avatar_uuid`] — `None` for every
    /// caller with no live session (every hermetic gate, and a frame built
    /// before login). See [`PlayerPreview::maybe_skin_for_uuid`], the
    /// consumer.
    pub uuid: Option<uuid::Uuid>,
}

impl PlayerAvatar {
    /// The avatar rect for a panel whose top-left is at `(panel_x, panel_y)` in
    /// logical GUI pixels, plus the cursor to aim it with.
    ///
    /// `cursor_logical` is `None` for every caller with no pointer (headless
    /// gates, a screen opened before the first mouse event); that resolves to the
    /// rect centre, i.e. facing the viewer.
    #[must_use]
    pub fn new(panel_x: f32, panel_y: f32, cursor_logical: Option<[f32; 2]>) -> Self {
        let rect = Rect {
            x: panel_x + INVENTORY_RECT_OFFSET[0],
            y: panel_y + INVENTORY_RECT_OFFSET[1],
            w: INVENTORY_RECT_SIZE[0],
            h: INVENTORY_RECT_SIZE[1],
        };
        Self::in_rect(rect, cursor_logical, INVENTORY_SIZE)
    }

    /// The avatar rect the **creative** screen's inventory tab uses.
    ///
    /// `CreativeModeInventoryScreen.extractBackground`'s own call, on the
    /// `Type.INVENTORY` branch only:
    /// `extractEntityInInventoryFollowsMouse(g, leftPos + 73, topPos + 6, leftPos + 105,
    /// topPos + 49, 20, 0.0625F, mouseX, mouseY, player)`. So a 32×43 recess at
    /// `(+73, +6)` at scale 20 — a different rect *and* a different scale from
    /// [`new`](Self::new)'s 49×70 at `(+26, +8)`, scale 30. Neither number is shared,
    /// which is why this is its own constructor rather than an offset applied to that
    /// one.
    ///
    /// The module doc in `container/creative.rs` used to state that vanilla's creative
    /// screen never draws the avatar. It does; this is the call.
    #[must_use]
    pub fn creative(panel_x: f32, panel_y: f32, cursor_logical: Option<[f32; 2]>) -> Self {
        let rect = Rect {
            x: panel_x + CREATIVE_RECT_OFFSET[0],
            y: panel_y + CREATIVE_RECT_OFFSET[1],
            w: CREATIVE_RECT_SIZE[0],
            h: CREATIVE_RECT_SIZE[1],
        };
        Self::in_rect(rect, cursor_logical, CREATIVE_SIZE)
    }

    fn in_rect(rect: Rect, cursor_logical: Option<[f32; 2]>, size: f32) -> Self {
        let mouse = cursor_logical.unwrap_or([rect.x + rect.w * 0.5, rect.y + rect.h * 0.5]);
        Self {
            rect,
            mouse,
            pose: AnimInput::REST,
            size,
            uuid: None,
        }
    }

    /// `[x, y, w, h]`, the shape `lodestone_render::gui_entity` takes.
    #[must_use]
    pub fn rect_px(&self) -> [f32; 4] {
        [self.rect.x, self.rect.y, self.rect.w, self.rect.h]
    }

    /// This avatar's look angles — the same call the draw makes, so a test can
    /// assert on the angles the pixels are actually posed with.
    #[must_use]
    pub fn look(&self) -> GuiEntityLook {
        // `false`: see `PLAYER_BB_HEIGHT`'s doc for why the inventory screen never
        // shows a `FALL_FLYING` player.
        gui_entity_look(self.rect_px(), self.mouse, false)
    }

    /// The **view** matrix this avatar draws through: entity space → logical GUI
    /// pixel space. Compose with [`gui_ortho`] to reach clip space.
    #[must_use]
    pub fn view(&self) -> Mat4 {
        gui_entity_view(
            self.rect_px(),
            self.size,
            INVENTORY_OFFSET_Y,
            PLAYER_BB_HEIGHT,
            &self.look(),
        )
    }
}

/// GPU resources for the inventory avatar: its own pipeline, the uploaded player
/// rig, the skin sheet's bind group, and its own group-0 uniform.
#[derive(Debug)]
pub(super) struct PlayerPreview {
    pipeline: EntityPipeline,
    /// The CPU rig, kept because the *pose* is computed per frame from the
    /// skeleton on this side — `part_transforms` is what animates the head.
    mesh: EntityMesh,
    model: &'static str,
    /// The declared rig, kept so a gate can assert *which* skin is bound rather
    /// than only that one is.
    skin_model: PlayerModelType,
    gpu: GpuEntityModel,
    texture: wgpu::BindGroup,
    /// Kept so [`Self::set_skin`] can re-bind a new sheet without rebuilding
    /// the pipeline.
    sampler: wgpu::Sampler,
    /// This pass's own group-0 uniform. See the module docs' decision 2 — never
    /// share this with the world entity pass.
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    /// Kept for the public diagnostic gate. Process-global `skin.png` no longer
    /// has authority to claim an account, so this is always false.
    used_local_override: bool,
    /// `(account UUID, source key)` currently bound. The account is part of
    /// the identity: a renderer survives server/account changes, and a source
    /// key alone can otherwise retain the previous account's pixels.
    applied_skin: Option<(uuid::Uuid, String)>,
}

impl PlayerPreview {
    /// Build the avatar's resources, or `None` when the player skin sheet is not
    /// in the pack (a jar-less run), in which case the recess stays empty.
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Option<Self> {
        // Construction happens before a session exists, so only the pack
        // bootstrap is safe here. The active account's skin is applied later
        // by `maybe_skin_for_uuid`; the old process-global `skin.png` had no
        // owner and could therefore draw a different account.
        let skin_model = DEFAULT_MODEL;
        let model = lodestone_render::entity::player_model_name(skin_model.is_slim());
        let models = EntityModelSet::load();
        let mesh = models.get(model)?.clone();
        let gpu = GpuEntityModel::upload(device, &mesh)?;

        // The rig's own sheet, by the same candidate list the world entity pass
        // resolves through — not a hardcoded path, so a pack that ships only the
        // legacy name still finds it. A supplied sheet wins; a jar-less run with
        // no supplied sheet is the `None` that leaves the recess empty.
        let img = load_skin(model)?;

        let pipeline = EntityPipeline::new(device, color_format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("container-player-preview-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let view = crate::gpu::entities::entity_texture_from_image(device, queue, &img);
        let texture = pipeline.texture_bind_group(device, &view, &sampler);

        // Fog disabled, which also leaves the sky-darken lane at its `0.0`
        // sentinel — read back as `1.0`. That is vanilla:
        // `GuiEntityRenderer.renderToTexture` sets up `Lighting.Entry.ENTITY_IN_UI`
        // and `GuiGraphicsExtractor.entity` forces `lightCoords = 15728880`
        // (full bright), so the inventory avatar does **not** dim at night the way
        // the mob standing next to you does.
        let cam_buffer = entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            },
        );
        let cam_bind_group = pipeline.camera_bind_group(device, &cam_buffer);

        Some(Self {
            pipeline,
            mesh,
            model,
            skin_model,
            gpu,
            texture,
            sampler,
            cam_buffer,
            cam_bind_group,
            used_local_override: false,
            applied_skin: None,
        })
    }

    /// The rig currently bound. `Wide` until something declares otherwise.
    #[must_use]
    pub(super) fn skin_model(&self) -> PlayerModelType {
        self.skin_model
    }

    /// Resolve and bind the active account's retained skin, or its exact
    /// UUID-derived default while the real sheet is unavailable.
    ///
    /// # Why this exists rather than resolving the default at [`new`]
    ///
    /// `PlayerPreview::new` runs once during GPU bring-up, before a session
    /// exists — there is no local player uuid yet, only [`DEFAULT_MODEL`]'s
    /// bootstrap guess. This is the seam that corrects it once one is known,
    /// called every container frame from
    /// `ContainerRenderer::render_geometry_scaled_between_strata` (the same
    /// frame. The `(uuid, source)` identity below keeps that call cheap while
    /// still rebinding after an account change or renderer rebuild.
    ///
    /// # One resolver, keyed on one uuid
    ///
    /// This calls the *exact* function `entities.rs::default_remote_skin`
    /// calls for the world-side default of every other player with no
    /// declared skin — `lodestone_assets::skin::default_skin_for_uuid`. Two
    /// call sites reading one pure function of the same uuid cannot disagree
    /// by construction, which is the acceptance bar issue #646 states
    /// explicitly (the original report was exactly this: Alex in the
    /// inventory, Steve in the world, for the *same* player).
    ///
    /// A cached sheet is accepted only when it belongs to this UUID. An old
    /// unowned `skin.png` therefore cannot leak another switcher account into
    /// the preview. Missing assets fail open to the existing bootstrap skin.
    pub(super) fn maybe_skin_for_uuid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uuid: uuid::Uuid,
    ) {
        let local = crate::remote_skins::local_for(uuid);
        let resolved = local.as_ref().and_then(|skin| {
            (!skin.url.is_empty())
                .then(|| crate::remote_skins::sheet(&skin.url))
                .flatten()
                .map(|sheet| {
                    (
                        format!("remote:{}", skin.url),
                        skin.model,
                        (*sheet).clone(),
                    )
                })
        });
        let (source, model, image) = match resolved {
            Some(resolved) => resolved,
            None => {
                let (reference, model) = local.as_ref().map_or_else(
                    || {
                        let (hi, lo) = uuid.as_u64_pair();
                        let skin = lodestone_assets::skin::default_skin_for_uuid(
                            hi as i64, lo as i64,
                        );
                        (skin.texture, skin.model)
                    },
                    |skin| (skin.default_sheet, skin.model),
                );
                let Some(image) = load_skin_reference(reference) else {
                    return;
                };
                (format!("default:{reference}"), model, image)
            }
        };
        if preview_skin_is_current(self.applied_skin.as_ref(), uuid, &source) {
            return;
        }
        if self.set_skin(device, queue, model, Some(&image)) {
            self.applied_skin = Some((uuid, source));
        }
    }

    /// Compatibility diagnostic exposed through
    /// [`super::ContainerRenderer::player_preview_used_local_override`] for
    /// existing GPU gates. It is always false now: construction no longer
    /// binds an unowned process-global override; UUID-scoped skin resolution
    /// happens in [`Self::maybe_skin_for_uuid`].
    #[must_use]
    pub(super) fn used_local_override(&self) -> bool {
        self.used_local_override
    }

    /// Bind a different skin: a declared rig and, optionally, a sheet to draw it
    /// with. `sheet: None` falls back to the pack's own sheet for that rig, so
    /// `set_skin(.., Slim, None)` draws Alex out of the jar.
    ///
    /// **This is the seam the network fetch lands against.** Both halves are
    /// swapped together on purpose: a sheet authored for the slim rig drawn on
    /// the wide one puts the arm UVs one pixel out, which reads as a texture bug
    /// rather than as a model bug and is exactly the failure a "just change the
    /// texture" API invites.
    ///
    /// Returns `false` and leaves the avatar untouched when the rig or its sheet
    /// cannot be resolved — never a half-applied state where the mesh is slim
    /// and the sheet is wide.
    pub(super) fn set_skin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: PlayerModelType,
        sheet: Option<&lodestone_assets::Image>,
    ) -> bool {
        let name = lodestone_render::entity::player_model_name(model.is_slim());
        let owned;
        let img = match sheet {
            Some(img) => img,
            None => {
                owned = load_skin(name);
                match owned.as_ref() {
                    Some(img) => img,
                    None => return false,
                }
            }
        };
        // Only reload the rig when the model actually changed: a skin *change*
        // on the same rig (the common case — a player edits their skin, not
        // their model) must not re-bake the whole corpus.
        if name != self.model {
            let models = EntityModelSet::load();
            let Some(mesh) = models.get(name).cloned() else {
                return false;
            };
            let Some(gpu) = GpuEntityModel::upload(device, &mesh) else {
                return false;
            };
            self.mesh = mesh;
            self.gpu = gpu;
            self.model = name;
        }
        let view = crate::gpu::entities::entity_texture_from_image(device, queue, img);
        self.texture = self
            .pipeline
            .texture_bind_group(device, &view, &self.sampler);
        self.skin_model = model;
        true
    }

    /// Record the avatar's pass: colour loaded, depth **cleared**, scissored to
    /// the recess.
    ///
    /// The depth clear is unconditional and full-attachment (a `LoadOp::Clear`
    /// ignores the scissor). That is safe and necessary here: the world's depth
    /// buffer is still resident and would swallow a rig at GUI clip depth, and the
    /// container's own item-model pass clears it again immediately afterwards, so
    /// nothing downstream inherits this pass's depth.
    ///
    /// `physical_w`/`physical_h` are the real framebuffer size; the scissor is
    /// converted from logical to physical with the same integer GUI scale
    /// `logical_canvas` divided by, so the clip lands exactly on the recess at
    /// every DPI.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        avatar: &PlayerAvatar,
        gui_scale: u32,
        physical_w: u32,
        physical_h: u32,
    ) {
        let (logical_w, logical_h) =
            crate::menu::render::logical_canvas(gui_scale, physical_w, physical_h);
        let (logical_w, logical_h) = (logical_w.max(1.0) as u32, logical_h.max(1.0) as u32);
        let Some(scissor) = physical_scissor(avatar.rect, gui_scale, physical_w, physical_h) else {
            // The recess is entirely off-target (a window smaller than the panel).
            // wgpu rejects an out-of-bounds scissor outright, so this is a real
            // guard rather than defensive noise.
            return;
        };

        let matrices =
            avatar_part_matrices(&self.mesh, self.model, avatar, logical_w, logical_h);
        // Full bright, per `new`'s note on `ENTITY_IN_UI`.
        let light = u32::from(lodestone_render::ENTITY_FULLBRIGHT);
        let buffers: Vec<Option<wgpu::Buffer>> = matrices
            .iter()
            .map(|m| upload_instances(device, &[*m], &[light]))
            .collect();

        // Group 0 is `gui_ortho`'s **identity** here: the clip matrix is already
        // baked into every instance transform above, because the entity shader
        // multiplies `view_proj * instance * vertex` and the instance is the only
        // per-part slot. Writing `gui_ortho` into the uniform *and* into the
        // instance would apply it twice.
        queue.write_buffer(
            &self.cam_buffer,
            0,
            bytemuck::bytes_of(&EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("container-player-preview-pass"),
            color_attachments: &[Some(crate::hud::item_icon::load_colour_attachment(view))],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let [sx, sy, sw, sh] = scissor;
        pass.set_scissor_rect(sx, sy, sw, sh);
        pass.set_pipeline(&self.pipeline.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, &self.texture, &[]);
        pass.set_vertex_buffer(0, self.gpu.vertices.slice(..));
        pass.set_index_buffer(self.gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
        for (range, buffer) in self.gpu.parts.iter().zip(&buffers) {
            let (Some(buffer), true) = (buffer.as_ref(), range.index_count > 0) else {
                continue;
            };
            pass.set_vertex_buffer(1, buffer.slice(..));
            let end = range.index_start + range.index_count;
            pass.draw_indexed(range.index_start..end, 0, 0..1);
        }
    }
}

fn preview_skin_is_current(
    applied: Option<&(uuid::Uuid, String)>,
    uuid: uuid::Uuid,
    source: &str,
) -> bool {
    applied.is_some_and(|(owner, current)| *owner == uuid && current == source)
}

impl PlayerAvatar {
    /// The same avatar posed over a live [`AnimInput`] — see
    /// [`PlayerAvatar::pose`]. Builder-style so every existing caller keeps
    /// [`AnimInput::REST`], the same shape `ContainerFrame`'s own `with_*`
    /// methods take and for the same reason.
    #[must_use]
    pub fn with_pose(mut self, pose: AnimInput) -> Self {
        self.pose = pose;
        self
    }

    /// Attach the local player's own uuid — see [`Self::uuid`]. Builder-style
    /// for the same reason [`with_pose`](Self::with_pose) is: every existing
    /// caller keeps `uuid: None` (`in_rect`'s default) unless it opts in.
    #[must_use]
    pub fn with_uuid(mut self, uuid: Option<uuid::Uuid>) -> Self {
        self.uuid = uuid;
        self
    }
}

/// The per-part `mesh → clip` matrices the avatar draws with, at
/// `logical_w × logical_h`.
///
/// A free function taking the mesh rather than a [`PlayerPreview`] method for one
/// reason: **it is the whole geometric content of the draw, and this way a gate
/// can assert on it with no GPU at all.** Everything left in
/// [`PlayerPreview::draw`] after this returns is `wgpu` bookkeeping — buffer
/// uploads, a scissor, a pass. So a test over this function is a test of what is
/// drawn, not of a struct field that happens to sit near it.
///
/// `EntityInstance::new` at the origin with this look's body yaw produces
/// `entity_model_matrix · part`, and [`PlayerAvatar::view`] is the *other* factor
/// of `gui_entity_pose` — so `clip · part_transforms[i]` is exactly
/// `gui_ortho · gui_entity_pose · part_i`, with no second copy of either half and
/// therefore no way for the avatar's placement to drift from the world path's.
#[must_use]
fn avatar_part_matrices(
    mesh: &EntityMesh,
    model: &'static str,
    avatar: &PlayerAvatar,
    logical_w: u32,
    logical_h: u32,
) -> Vec<Mat4> {
    let look = avatar.look();
    // The live pose is the **base**; `gui_entity_anim` overwrites only the two
    // head angles on top of it, which is exactly why it takes a base at all.
    let anim = gui_entity_anim(&look, avatar.pose);
    let instance = EntityInstance::new(model, mesh, Vec3::ZERO, look.body_yaw_deg, 1.0, &anim);
    let clip = gui_ortho(logical_w, logical_h) * avatar.view();
    instance
        .part_transforms
        .iter()
        .map(|part| clip * *part)
        .collect()
}

/// Test-only model of the UUID-default decision in
/// [`PlayerPreview::maybe_skin_for_uuid`],
/// extracted so it is testable without a GPU device, a vanilla jar, or the
/// real skin cache — none of which this decision itself depends on.
///
/// `None` means the same source is already applied; `Some(model)` is the rig
/// `lodestone_assets::skin::default_skin_for_uuid` resolves for `uuid` — the
/// **same** function `entities.rs::default_remote_skin` calls for the
/// world-side default of every other player, so this call site and that one
/// cannot disagree for the same uuid.
#[must_use]
#[cfg(test)]
fn uuid_default_model(
    _used_local_override: bool,
    already_applied: bool,
    uuid: uuid::Uuid,
) -> Option<PlayerModelType> {
    if already_applied {
        return None;
    }
    let (hi, lo) = uuid.as_u64_pair();
    Some(lodestone_assets::skin::default_skin_for_uuid(hi as i64, lo as i64).model)
}

/// A logical-pixel rect as a **physical** scissor `[x, y, w, h]`, clamped into
/// the target, or `None` if nothing of it is on the target.
///
/// `wgpu` validates a scissor against the attachment size and panics on an
/// overrun, so the clamp is load-bearing rather than tidiness: the recess sits at
/// `panel_origin + 26` and `panel_origin_with_scale` floors the origin at `8`, so
/// a window narrower than the panel really does push the rect off the right edge.
#[must_use]
fn physical_scissor(rect: Rect, gui_scale: u32, width: u32, height: u32) -> Option<[u32; 4]> {
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let x0 = (rect.x * scale).floor().max(0.0) as u32;
    let y0 = (rect.y * scale).floor().max(0.0) as u32;
    // `ceil` on the far edge, not `floor` on the width: a fractional edge must
    // round *outwards* or a scaled avatar loses its rightmost column of pixels.
    let x1 = ((rect.x + rect.w) * scale).ceil().max(0.0) as u32;
    let y1 = ((rect.y + rect.h) * scale).ceil().max(0.0) as u32;
    let x1 = x1.min(width);
    let y1 = y1.min(height);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

/// Decode the player rig's skin sheet out of the vanilla `client.jar`, trying
/// `entity_texture_candidates`' paths in order — the same resolution the world
/// entity pass performs, so the inventory avatar and the third-person body can
/// never end up on different sheets.
fn load_skin(model: &str) -> Option<lodestone_assets::Image> {
    let manager = crate::resources::vanilla_manager()?;
    for path in entity_texture_candidates(model) {
        let Some(png) = manager.read(path) else {
            continue;
        };
        match lodestone_assets::Image::decode_png(&png) {
            Ok(img) => return Some(img),
            Err(e) => tracing::warn!(target: "assets", "decode {path}: {e}"),
        }
    }
    tracing::warn!(target: "assets", model, "no player skin sheet for the inventory avatar");
    None
}

/// Resolve one of `DefaultPlayerSkin`'s exact sheet references through the
/// active pack stack. Unlike `load_skin`, this preserves the UUID-selected
/// identity (`ari`, `efe`, …) instead of collapsing it to generic Steve/Alex.
fn load_skin_reference(reference: &str) -> Option<lodestone_assets::Image> {
    let manager = crate::resources::open_vanilla_pack_stack()?;
    let path = format!("assets/minecraft/textures/{reference}.png");
    let png = manager.read(&path)?;
    lodestone_assets::Image::decode_png(&png).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-zero panel origin, so a slip that measures from the canvas corner
    /// instead of the panel shows up. `(339, 157)` is what
    /// `panel_origin_with_scale` gives a 176×166 panel on an 854×480 canvas.
    const PANEL: (f32, f32) = (339.0, 157.0);

    #[test]
    fn the_rect_is_vanillas_recess_relative_to_the_panel() {
        let a = PlayerAvatar::new(PANEL.0, PANEL.1, None);
        // `InventoryScreen.java`: `xo + 26, yo + 8` to `xo + 75, yo + 78`.
        assert_eq!(a.rect.x, PANEL.0 + 26.0);
        assert_eq!(a.rect.y, PANEL.1 + 8.0);
        assert_eq!(a.rect.x + a.rect.w, PANEL.0 + 75.0);
        assert_eq!(a.rect.y + a.rect.h, PANEL.1 + 78.0);
    }

    #[test]
    fn no_cursor_is_a_forward_look_not_a_corner() {
        let a = PlayerAvatar::new(PANEL.0, PANEL.1, None);
        assert_eq!(a.look(), GuiEntityLook::FORWARD);
    }

    /// The cursor is threaded, and it is threaded in the **logical** space the
    /// rect lives in. A physical-pixel cursor at `gui_scale > 1` would give an
    /// angle several times too large — the class of bug `hit_test`'s own scaling
    /// note exists for.
    #[test]
    fn a_cursor_right_of_the_recess_turns_the_head_right() {
        let centre = [PANEL.0 + 26.0 + 24.5, PANEL.1 + 8.0 + 35.0];
        let a = PlayerAvatar::new(PANEL.0, PANEL.1, Some([centre[0] + 40.0, centre[1]]));
        let look = a.look();
        // atan(-40/40) = -PI/4 rad, times 20 read as degrees = -15.708.
        assert!(
            (look.head_yaw_deg - (-15.7080)).abs() < 1e-3,
            "predicted -15.708 deg from atan(-1)*20; got {}",
            look.head_yaw_deg
        );
        assert!((look.body_yaw_deg - (180.0 + look.head_yaw_deg)).abs() < 1e-4);
    }

    #[test]
    fn the_scissor_scales_with_the_gui_scale_and_rounds_outwards() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            w: 49.0,
            h: 70.0,
        };
        // `gui_scale = 2` explicitly, so the arithmetic is not at the mercy of
        // `calculate_gui_scale`'s auto choice for this framebuffer.
        let s = physical_scissor(rect, 2, 1920, 1080).expect("on target");
        assert_eq!(s, [20, 40, 98, 140]);
        // Scale 1 is the identity.
        let s1 = physical_scissor(rect, 1, 1920, 1080).expect("on target");
        assert_eq!(s1, [10, 20, 49, 70]);
    }

    /// The clamp, and the `None`. wgpu panics on an over-large scissor, so this
    /// is the guard, not a nicety.
    #[test]
    fn a_recess_past_the_edge_is_clamped_or_dropped() {
        let rect = Rect {
            x: 100.0,
            y: 10.0,
            w: 49.0,
            h: 70.0,
        };
        let clamped = physical_scissor(rect, 1, 120, 200).expect("partially on target");
        assert_eq!(clamped, [100, 10, 20, 70], "clamped to the target width");
        assert!(
            physical_scissor(rect, 1, 80, 200).is_none(),
            "entirely off the target must be None, not a zero-width scissor"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #646: the uuid-derived default, decoupled from the GPU/filesystem
    // -----------------------------------------------------------------------

    /// `uuid_default_model` is the whole decision behind
    /// `maybe_skin_for_uuid`'s fallback, pure and GPU/filesystem-free, so this gate
    /// needs neither a device nor a vanilla jar nor a clean data directory.
    ///
    /// **Two discriminating uuids, not one** — hand-verified against
    /// `lodestone-assets/src/skin.rs`'s own
    /// `default_skin_for_uuid_matches_hand_derived_cases`: the nil uuid
    /// resolves Slim (index 0), `(9, 0)` resolves Wide (index 9). A
    /// hardcoded `PlayerModelType::Wide` — the pre-fix constant this
    /// function replaces — fails the first case; a resolver that ignores the
    /// uuid entirely (always returning the same model) fails to distinguish
    /// the two.
    #[test]
    fn uuid_default_model_picks_the_uuids_own_answer() {
        let slim_uuid = uuid::Uuid::from_u64_pair(0, 0);
        let wide_uuid = uuid::Uuid::from_u64_pair(9, 0);

        assert_eq!(
            uuid_default_model(false, false, slim_uuid),
            Some(PlayerModelType::Slim),
            "nil uuid, no override, not yet applied -> Slim"
        );
        assert_eq!(
            uuid_default_model(false, false, wide_uuid),
            Some(PlayerModelType::Wide),
            "(9, 0) uuid, no override, not yet applied -> Wide"
        );

        // A process-global cached override has no account identity attached to
        // it, so it must never outrank the active account's UUID. With two
        // accounts this was exactly how the inventory preview drew whichever
        // account had signed in most recently while the world drew the active
        // one.
        assert_eq!(
            uuid_default_model(true, false, slim_uuid),
            Some(PlayerModelType::Slim),
            "an unowned process-global override must not replace the active \
             account's uuid-derived identity"
        );
        // The one remaining gate a real caller must respect: resolution only
        // fires once, so a later fetched sheet is not clobbered by the guess.
        assert_eq!(
            uuid_default_model(false, true, slim_uuid),
            None,
            "already applied must not re-resolve — a later real fetch must \
             not be clobbered back to the uuid guess on a subsequent frame"
        );
    }

    /// Cross-checked against `lodestone_assets::skin::default_skin_for_uuid`
    /// directly, not just against the hand-derived constants above — this is
    /// the "one resolver, two call sites" claim itself, at the unit level:
    /// `uuid_default_model` must return *exactly* what the shared resolver
    /// says for a spread of uuids, never a second, independent guess that
    /// happens to agree on the two hand-picked cases.
    #[test]
    fn uuid_default_model_matches_the_shared_resolver_across_a_spread() {
        for i in 0..40u64 {
            let hi = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let lo = i.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
            let uuid = uuid::Uuid::from_u64_pair(hi, lo);
            let expected =
                lodestone_assets::skin::default_skin_for_uuid(hi as i64, lo as i64).model;
            assert_eq!(
                uuid_default_model(false, false, uuid),
                Some(expected),
                "uuid_default_model disagreed with default_skin_for_uuid for hi={hi:#x} lo={lo:#x}"
            );
        }
    }

    /// The renderer object is retained across account changes and recreated on
    /// a resource-pack reload. Both transitions must rebind: the same texture
    /// URL is not sufficient identity when the owner changed, and a fresh
    /// renderer has no GPU binding even when the source string is unchanged.
    #[test]
    fn preview_binding_identity_includes_account_and_rebinds_after_reload() {
        let alice = uuid::Uuid::from_u128(0xA11CE);
        let bob = uuid::Uuid::from_u128(0xB0B);
        let source = "remote:https://textures.minecraft.net/texture/shared";
        let applied = (alice, source.to_owned());
        assert!(preview_skin_is_current(Some(&applied), alice, source));
        assert!(
            !preview_skin_is_current(Some(&applied), bob, source),
            "switching accounts must rebind even when two profiles share a texture URL"
        );
        assert!(
            !preview_skin_is_current(None, alice, source),
            "a renderer rebuilt for a pack reload has no bind group and must rehydrate"
        );
    }

    // -----------------------------------------------------------------------
    // The draw itself: the matrices that reach the instance buffers
    // -----------------------------------------------------------------------

    const LOGICAL: (u32, u32) = (854, 480);

    fn player_mesh() -> lodestone_render::EntityMesh {
        let models = EntityModelSet::load();
        let name = lodestone_render::entity::player_model_name(DEFAULT_MODEL.is_slim());
        models
            .get(name)
            .unwrap_or_else(|| panic!("the corpus must carry {name}"))
            .clone()
    }

    /// Project a mesh point through a `mesh → clip` matrix back to logical GUI
    /// pixels, so failures print numbers comparable with `InventoryScreen`'s own.
    fn to_gui_px(m: Mat4, p: Vec3) -> [f32; 2] {
        let c = m * p.extend(1.0);
        let ndc = c.truncate() / c.w;
        [
            (ndc.x + 1.0) * 0.5 * LOGICAL.0 as f32,
            (1.0 - ndc.y) * 0.5 * LOGICAL.1 as f32,
        ]
    }

    /// **Measure by location, on the matrices the draw uses.** Every part of the
    /// rig, projected corner-by-corner through `avatar_part_matrices` — *the
    /// function `draw` calls* — must land inside the scissor rect, and must cover
    /// a real fraction of it.
    ///
    /// This is the shell-side answer to "the suite was green while it drew the
    /// wrong thing": nothing here reads a struct field. The subject is the posed,
    /// projected geometry.
    #[test]
    fn every_posed_part_lands_inside_the_scissor() {
        let mesh = player_mesh();
        let model = lodestone_render::entity::player_model_name(DEFAULT_MODEL.is_slim());
        // A cursor hard into the bottom-right corner of the recess: the worst case
        // for overflow, because both look angles are at their largest.
        let avatar = PlayerAvatar::new(PANEL.0, PANEL.1, Some([PANEL.0 + 75.0, PANEL.1 + 78.0]));
        let matrices = avatar_part_matrices(&mesh, model, &avatar, LOGICAL.0, LOGICAL.1);
        assert_eq!(
            matrices.len(),
            mesh.parts.len(),
            "one matrix per drawable part, or `draw`'s zip silently skips limbs"
        );
        assert!(matrices.len() >= 6, "the humanoid rig has at least six parts");

        let (lo, hi) = (mesh.local_min, mesh.local_max);
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for m in &matrices {
            for i in 0..8 {
                let corner = Vec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                let px = to_gui_px(*m, corner);
                min[0] = min[0].min(px[0]);
                min[1] = min[1].min(px[1]);
                max[0] = max[0].max(px[0]);
                max[1] = max[1].max(px[1]);
            }
        }
        let r = avatar.rect;
        let bbox = format!(
            "posed bbox x {:.2}..{:.2}, y {:.2}..{:.2}; recess x {:.2}..{:.2}, y {:.2}..{:.2}",
            min[0],
            max[0],
            min[1],
            max[1],
            r.x,
            r.x + r.w,
            r.y,
            r.y + r.h
        );
        // Each part's *own* local box is looser than the whole rig's, so allow the
        // rig's half-width of slack on x rather than asserting a hard containment
        // that the per-part corner sweep cannot honestly meet. The point of this
        // gate is the *scissor* claim: the drawn rig must not be miles away.
        assert!(
            min[0] > r.x - r.w && max[0] < r.x + 2.0 * r.w,
            "the posed rig is nowhere near its recess horizontally — {bbox}"
        );
        assert!(
            min[1] > r.y - r.h && max[1] < r.y + 2.0 * r.h,
            "the posed rig is nowhere near its recess vertically — {bbox}"
        );
        // And the head part specifically must be inside, at the top.
        let head = mesh.skeleton.index_of("head").expect("head part");
        let nose = to_gui_px(matrices[head], Vec3::new(0.0, -0.25, -0.25));
        assert!(
            nose[0] > r.x && nose[0] < r.x + r.w && nose[1] > r.y && nose[1] < r.y + r.h,
            "the nose must be inside the recess: at ({:.2}, {:.2}) — {bbox}",
            nose[0],
            nose[1]
        );
        assert!(
            nose[1] < r.y + r.h * 0.5,
            "the head belongs in the upper half of the recess, not at {:.2} — {bbox}",
            nose[1]
        );
    }

    /// The cursor genuinely reaches the posed head. Moving the pointer down the
    /// screen must move the *drawn* nose down — asserted on
    /// `avatar_part_matrices`' output, not on the angle field.
    #[test]
    fn moving_the_cursor_moves_the_drawn_nose() {
        let mesh = player_mesh();
        let model = lodestone_render::entity::player_model_name(DEFAULT_MODEL.is_slim());
        let head = mesh.skeleton.index_of("head").expect("head part");
        let nose = Vec3::new(0.0, -0.25, -0.25);
        let centre = [PANEL.0 + 26.0 + 24.5, PANEL.1 + 8.0 + 35.0];

        let at = |cursor: [f32; 2]| -> [f32; 2] {
            let a = PlayerAvatar::new(PANEL.0, PANEL.1, Some(cursor));
            let m = avatar_part_matrices(&mesh, model, &a, LOGICAL.0, LOGICAL.1);
            to_gui_px(m[head], nose)
        };

        let rest = at(centre);
        let down = at([centre[0], centre[1] + 30.0]);
        let up = at([centre[0], centre[1] - 30.0]);
        let right = at([centre[0] + 30.0, centre[1]]);
        let left = at([centre[0] - 30.0, centre[1]]);

        assert!(
            down[1] > rest[1] + 0.5,
            "cursor down must move the drawn nose down: rest y {:.3}, down y {:.3}",
            rest[1],
            down[1]
        );
        assert!(
            up[1] < rest[1] - 0.5,
            "cursor up must move the drawn nose up: rest y {:.3}, up y {:.3}",
            rest[1],
            up[1]
        );
        assert!(
            right[0] > rest[0] + 0.5 && left[0] < rest[0] - 0.5,
            "cursor left/right must swing the drawn nose: left x {:.3}, rest x {:.3}, \
             right x {:.3}",
            left[0],
            rest[0],
            right[0]
        );
    }

    /// Every clip-space vertex of one rig, through the **real** composed draw
    /// matrices — `avatar_part_matrices` is the whole geometric content of the
    /// pass, so this is an assertion on the draw and not on a struct field.
    fn clip_positions(model: &'static str, avatar: &PlayerAvatar) -> Vec<Vec3> {
        let models = EntityModelSet::load();
        let mesh = models.get(model).expect("a baked player rig");
        let matrices = avatar_part_matrices(mesh, model, avatar, 854, 480);
        let mut out = Vec::new();
        for (range, m) in mesh.parts.iter().zip(&matrices) {
            let start = range.vertex_start as usize;
            let end = start + range.vertex_count as usize;
            for v in &mesh.vertices[start..end] {
                out.push(m.project_point3(Vec3::from_array(v.position)));
            }
        }
        out
    }

    /// **The slim rig really does draw narrower, by exactly one model texel.**
    ///
    /// `PlayerPreview::set_skin` swapping the model would be indistinguishable
    /// from a no-op through any vertex *count* or bind-group check — both rigs
    /// have the same part list, the same skeleton, and (because
    /// `player_model`'s only difference is the arm boxes) **identical part
    /// matrices**. So the measurement has to be on the geometry, and it has to
    /// be a magnitude: the predicted span change is one texel of arm width,
    /// `1/16` block, times `INVENTORY_SIZE` GUI pixels per block, mapped into
    /// NDC by `gui_ortho`'s `2 / width`. Both hypotheses are computed — the
    /// correct delta and the no-op `0` — and the measurement must land on the
    /// first.
    ///
    /// Note the outermost X belongs to the grown sleeves, not the arms
    /// themselves, and the delta is one texel either way (both boxes narrow by
    /// one), which is why the prediction does not need to know which.
    ///
    /// **The span shrinks by TWO texels, not one, and predicting one is how this
    /// test earned its keep.** The first version of this gate predicted a single
    /// texel and measured exactly double, which is right: the two arms narrow
    /// from *opposite* sides. The right arm's origin moves inward with its width
    /// (`1 - arm_w`, so `-3 → -2`) while the left arm's origin stays at `-1` and
    /// its width shrinks — so the left arm's *outer* edge (`origin + width`)
    /// comes in by one texel too. One texel per arm, on opposite sides, and the
    /// full-rig span therefore loses two. Hand-checked against the grown
    /// sleeves: wide `±8.25` texels from the pivots, slim `±7.25`.
    #[test]
    fn the_slim_rig_draws_one_texel_narrower_than_the_wide_one() {
        let avatar = PlayerAvatar::new(PANEL.0, PANEL.1, None);
        let span = |model: &'static str| {
            let xs = clip_positions(model, &avatar);
            let min = xs.iter().map(|p| p.x).fold(f32::MAX, f32::min);
            let max = xs.iter().map(|p| p.x).fold(f32::MIN, f32::max);
            max - min
        };
        let wide = span("player_wide");
        let slim = span("player_slim");
        // Two texels: one per arm, from opposite sides — see this test's doc.
        let predicted = (2.0 / 16.0) * INVENTORY_SIZE * 2.0 / 854.0;
        let measured = wide - slim;
        assert!(
            (measured - predicted).abs() < predicted * 0.1,
            "the slim rig should be exactly one model texel narrower: predicted \
             {predicted} NDC, measured {measured} (wide span {wide}, slim span \
             {slim}). A measured 0 means the model switch reached nothing."
        );
        // The no-op hypothesis, stated so it cannot be satisfied by the above.
        assert!(measured > predicted * 0.5, "measured {measured} is indistinguishable from a no-op");
    }

    /// The **depth invariant, both rigs and both arms** — `EntityPipeline` has
    /// `cull_mode: None`, so a bad transform does not cull anything; the visible
    /// symptom is the back of the skull winning the depth test against the face.
    /// A winding-determinant assertion cannot see it, which is why this is the
    /// gate that matters for anything drawn through that pipeline.
    ///
    /// Arm A is a real perspective `Camera` standing in front of a yaw-0 entity;
    /// arm B is the GUI pose. Both must agree that the head's **front** faces
    /// (mesh `-Z`, derived from `entity_model_matrix` mapping mesh `-Z` to world
    /// `+Z` at yaw 0, not assumed) are nearer than its back faces. Run for the
    /// slim rig as well as the wide one, since making the slim rig reachable is
    /// the first time anything ever composed it.
    #[test]
    fn every_box_front_is_nearer_than_its_back_in_both_arms_for_both_rigs() {
        let avatar = PlayerAvatar::new(PANEL.0, PANEL.1, None);
        let models = EntityModelSet::load();
        // A perspective camera 4 blocks along +Z looking back at a yaw-0 entity.
        // The rig's front is mesh -Z: `entity_model_matrix` at yaw 0 maps mesh
        // -Z to world +Z, and Minecraft's yaw 0 faces +Z — derived, not assumed.
        let camera = lodestone_render::Camera {
            position: Vec3::new(0.0, 1.4, 4.0),
            yaw: 180.0,
            ..lodestone_render::Camera::default()
        };
        // Which end of `[0, 1]` is nearer is a property of
        // `Camera::projection_matrix` — reversed-Z, so nearer is *greater* — and
        // it is derived here rather than written into the assertion. Both arms
        // share it: the GUI arm draws through the same pipeline and the same
        // depth comparison as the world one, which is the whole reason
        // `gui_ortho` carries the world projection's depth direction.
        let nearer_is_greater_depth = {
            let vp = camera.view_projection();
            let depth = |z: f32| vp.project_point3(Vec3::new(0.0, 1.4, z)).z;
            let (close, distant) = (depth(2.0), depth(0.0));
            assert_ne!(close, distant, "premise: the projection is degenerate in z");
            close > distant
        };
        for model in ["player_wide", "player_slim"] {
            let mesh = models.get(model).expect("a baked player rig");
            let gui = avatar_part_matrices(mesh, model, &avatar, 854, 480);
            let rest = mesh.skeleton.rest_pose();
            let world_base = camera.view_projection()
                * lodestone_render::entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
            let mut checked = 0usize;
            for (i, range) in mesh.parts.iter().enumerate() {
                let verts = &mesh.vertices
                    [range.vertex_start as usize..(range.vertex_start + range.vertex_count) as usize];
                if verts.is_empty() {
                    continue;
                }
                for (arm, m) in [("world camera", world_base * rest[i]), ("gui ortho", gui[i])] {
                    let mean = |front: bool| {
                        let sel: Vec<f32> = verts
                            .iter()
                            .filter(|v| {
                                if front {
                                    v.position[2] < -0.01
                                } else {
                                    v.position[2] > 0.01
                                }
                            })
                            .map(|v| m.project_point3(Vec3::from_array(v.position)).z)
                            .collect();
                        (sel.iter().sum::<f32>() / sel.len().max(1) as f32, sel.len())
                    };
                    let (front, nf) = mean(true);
                    let (back, nb) = mean(false);
                    if nf == 0 || nb == 0 {
                        continue;
                    }
                    let ordered = if nearer_is_greater_depth {
                        front > back
                    } else {
                        front < back
                    };
                    assert!(
                        ordered,
                        "{model} part {i} through the {arm}: front faces at depth \
                         {front}, back faces at {back}. Depth is [0,1] with \
                         {} nearer, so this draws the inside of the far side of \
                         the box.",
                        if nearer_is_greater_depth { "larger" } else { "smaller" }
                    );
                    checked += 1;
                }
            }
            // The premise: this rig really does have boxes with both faces, in
            // both arms — otherwise every assertion above was skipped and the
            // pass is vacuous. `player_model` builds 12 boxes (six parts, each
            // with an overlay child), so both arms should reach well past ten.
            assert!(
                checked >= 20,
                "{model}: only {checked} front/back comparisons ran — this gate \
                 measured almost nothing"
            );
        }
    }

    /// **The live pose reaches the draw, and it moves the arm that swings.**
    ///
    /// `PlayerAvatar::pose` would be indistinguishable from an unread field
    /// through any count or bind-group check — the seam is one argument deep
    /// (`gui_entity_anim(&look, base)`), and dropping it compiles. So the
    /// measurement is on the composed part matrices, and it is a *localised*
    /// one — "where moved", not "something changed" — with the REST-vs-REST
    /// control that makes it non-vacuous.
    ///
    /// The swinging part is identified by its **rest pivot**, not by a
    /// remembered index: `player_model` puts the right arm at `x = -5` texels,
    /// i.e. `-0.3125` blocks, so the mover must sit on the entity's own right.
    #[test]
    fn the_live_pose_reaches_the_draw_and_moves_the_right_arm() {
        let base = PlayerAvatar::new(PANEL.0, PANEL.1, None);
        let models = EntityModelSet::load();
        let model = "player_wide";
        let mesh = models.get(model).expect("a baked player rig");
        let at = |pose: AnimInput| avatar_part_matrices(mesh, model, &base.with_pose(pose), 854, 480);

        let rest = at(AnimInput::REST);
        // **`0.5`, not `1.0`.** `attack_anim` is the *phase* of the swing and
        // `HumanoidModel.setupAttackAnimation` drives it through sines, so the
        // endpoint `1.0` is the rest pose again — the first version of this test
        // used `1.0`, measured a delta of `1.7e-8`, and read as "the pose is not
        // reaching the draw" when the pose was arriving perfectly and the value
        // was the no-op. The endpoint identity is asserted below, so this is
        // recorded as a property rather than only as a comment.
        let swung = at(AnimInput {
            attack_anim: 0.5,
            ..AnimInput::REST
        });
        assert_eq!(rest.len(), swung.len());

        // The control first: the same pose twice must be bit-identical, so a
        // non-zero delta below cannot come from nondeterminism in the bake.
        let control = at(AnimInput::REST);
        let max_delta = |a: &[Mat4], b: &[Mat4]| {
            a.iter()
                .zip(b)
                .map(|(x, y)| {
                    (0..4)
                        .flat_map(|c| (0..4).map(move |r| (c, r)))
                        .map(|(c, r)| (x.col(c)[r] - y.col(c)[r]).abs())
                        .fold(0.0_f32, f32::max)
                })
                .collect::<Vec<f32>>()
        };
        assert_eq!(
            max_delta(&rest, &control).iter().cloned().fold(0.0_f32, f32::max),
            0.0,
            "REST against itself must be identical — otherwise the delta below \
             measures noise, not the pose"
        );

        let deltas = max_delta(&rest, &swung);
        let (worst, worst_delta) = deltas
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).expect("finite"))
            .expect("at least one part");
        assert!(
            *worst_delta > 1e-4,
            "a full attack swing moved no part at all ({worst_delta}) — \
             `PlayerAvatar::pose` is not reaching `gui_entity_anim`'s base"
        );

        // The endpoint identity, which is why the phase above is 0.5: a swing at
        // phase 1.0 has finished, and finished is rest.
        let finished = at(AnimInput {
            attack_anim: 1.0,
            ..AnimInput::REST
        });
        let end_delta = max_delta(&rest, &finished)
            .iter()
            .cloned()
            .fold(0.0_f32, f32::max);
        assert!(
            end_delta < 1e-5,
            "a swing at phase 1.0 should be back at rest, but differs by \
             {end_delta} — if this fails the curve is not sinusoidal and the \
             0.5 above may not be near the peak either"
        );

        // Where: the biggest mover sits on the entity's right, where the pivot
        // that `player_model` puts at -5 texels is.
        let pivot_x = mesh.skeleton.rest_pose()[worst].col(3)[0];
        assert!(
            pivot_x < -0.2,
            "the biggest mover under an attack swing is part {worst}, whose rest \
             pivot is at x = {pivot_x}. The swinging arm's pivot is -0.3125 \
             blocks (vanilla's -5 texels), so a mover on the other side or at \
             the centre means the swing is being applied to the wrong part."
        );
    }

    /// The **physical → logical** cursor conversion, which `ContainerGeometry`
    /// performs and this module trusts. A physical cursor fed in raw at
    /// `gui_scale = 2` would aim the head at twice the offset.
    ///
    /// Both hypotheses are computed: the correct one (divide by the scale) and the
    /// suspected-wrong one (do not), and they must differ by more than a degree, so
    /// this gate would catch the conversion being dropped in `geometry.rs`.
    #[test]
    fn the_logical_and_physical_cursor_hypotheses_are_distinguishable() {
        let centre = [PANEL.0 + 26.0 + 24.5, PANEL.1 + 8.0 + 35.0];
        let logical_cursor = [centre[0] + 20.0, centre[1]];
        let right = PlayerAvatar::new(PANEL.0, PANEL.1, Some(logical_cursor)).look();
        // The same pointer, un-divided, at scale 2.
        let wrong =
            PlayerAvatar::new(PANEL.0, PANEL.1, Some([logical_cursor[0] * 2.0, logical_cursor[1] * 2.0]))
                .look();
        assert!(
            (right.head_yaw_deg - wrong.head_yaw_deg).abs() > 1.0,
            "the two cursor-space hypotheses must be separable or this gate proves \
             nothing: logical {} deg, raw-physical {} deg",
            right.head_yaw_deg,
            wrong.head_yaw_deg
        );
    }
}
