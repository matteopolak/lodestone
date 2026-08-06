//! The frame graph: the four public `render*` entry points and the single
//! `render_inner` they all funnel into.
//!
//! # Submission order is load-bearing
//!
//! `render_inner` is one long straight line, and the order of the passes in it
//! is the thing to be careful with rather than any individual draw. Two rules
//! account for most of it, both vanilla's:
//!
//! * **Everything that creates a GPU buffer runs before the pass opens.** A
//!   render pass cannot create buffers, so every `prepare_*` call — entities,
//!   armour, wool, flame, block entities, item geometry, cracks, outline,
//!   debug lines, nametags, sign text — is hoisted above
//!   `begin_render_pass`, and the per-frame uniform writes with them.
//! * **Opaque and cutout before translucent water.** Water is alpha-blended
//!   with depth *write* off, so it leaves no depth behind it. Anything opaque
//!   drawn after it passes the depth test against the sea floor and paints
//!   over the surface however deep it is — which is why mobs, armour, wool,
//!   flame, block entities and sign text all sit above the water draw, and
//!   why alpha-blended debris, weather, the outline, debug lines and nametags
//!   all sit below it.
//!
//! The first-person hand then gets its **own** pass with the depth buffer
//! cleared (vanilla's `GameRenderer.renderLevel` does the same before
//! `renderItemInHand`), and the screen overlays get theirs, on `Load`, last.
//! See [`super::first_person`] and `docs/screen-overlays.md`.
use lodestone_render::{
    Camera, CameraUniform, crack_pipeline::GpuCrackMesh, spinning_effect_angle_degrees,
    update_model_shared_camera_buffer, vertex::vram_bytes,
};

use crate::entities::EntityDraw;

use super::first_person::FirstPersonHand;
use super::{CrackTarget, RenderState, RenderStats, ScreenEffects};

impl RenderState {

    /// Render every section into `view` using `camera`. Writes all section
    /// camera uniforms first, then draws. If `outline` names a block, a
    /// wireframe box is drawn around it after the terrain.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            &[],
            ScreenEffects::default(),
        )
    }

    /// Like [`render`](Self::render), but also draws the progressive mining-crack
    /// overlay for every target in `cracks` (issue #410: other players' digs, not
    /// just the local player's own). Each follows its own block's real model
    /// geometry (slabs/stairs/crosses), not a synthetic cube.
    pub fn render_with_crack(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        cracks: &[CrackTarget],
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            cracks,
            ScreenEffects::default(),
        )
    }

    /// Like [`render`](Self::render), but also drives the underwater/fire
    /// screen-overlay pass (issues #108, #112) from `screen_effects`. A
    /// separate method rather than a new required parameter on
    /// [`render`](Self::render)/[`render_with_crack`](Self::render_with_crack)
    /// so the ~15 existing call sites across the test suite need no change —
    /// see `docs/screen-overlays.md`.
    #[must_use]
    pub fn render_with_effects(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        screen_effects: ScreenEffects,
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            &[],
            screen_effects,
        )
    }

    /// [`render_with_crack`](Self::render_with_crack) +
    /// [`render_with_effects`](Self::render_with_effects) together — the shape
    /// `app.rs`'s real per-frame call site needs (mining and the overlays are
    /// both possible at once). `cracks` may hold any number of targets: the
    /// local player's own dig, any number of other players' (issue #410), or
    /// none at all (an empty slice costs nothing extra — see `render_inner`).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_crack_and_effects(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        cracks: &[CrackTarget],
        screen_effects: ScreenEffects,
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            cracks,
            screen_effects,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_inner(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        cracks: &[CrackTarget],
        screen_effects: ScreenEffects,
    ) -> RenderStats {
        // Issues #144/#149's shared world-projection "spinning" warp — see
        // `Camera::view_projection_warped`'s doc for why injecting it here,
        // at the single upstream source every world-space uniform below is
        // rewritten from, reaches the same scope vanilla's own
        // `RenderSystem.setProjectionMatrix` call in `GameRenderer.
        // renderLevel` does (the whole world pass) without a second call
        // site. `nausea_intensity`/`portal_intensity` default to `0.0`
        // (no live producer yet — see `docs/screen-overlays.md`), at which
        // `view_projection_warped` is provably identical to plain
        // `view_projection` (`view_projection_warped_matches_plain_view_projection_when_inactive`),
        // so this is a no-op for every caller today.
        let warp_intensity = screen_effects.portal_intensity.max(screen_effects.nausea_intensity);
        let warp_angle_degrees = spinning_effect_angle_degrees(
            screen_effects.tick,
            screen_effects.portal_intensity,
            screen_effects.nausea_intensity,
        );
        let view_proj = camera
            .view_projection_warped(warp_intensity, warp_angle_degrees)
            .to_cols_array_2d();

        // This frame's fog **and** its `sky_darken` lane, hoisted above both
        // terrain paths so they physically cannot disagree (issue #400). It used
        // to be computed inside the `if let Some(model)` below, which is why the
        // packed path had no way to reach it.
        let fog = self.fog_with_clock(camera.position);

        // The packed sections' shared camera buffer: **one** write, not one per
        // section. Until issue #76 this was a `queue.write_buffer` per resident
        // packed section, every frame, rewriting the whole 80-byte uniform just
        // to re-aim the camera — the same shape issue #75 profiled at 52.9% of
        // main-thread CPU on the model path, left in place here because the
        // packed table only ever holds the demo world. Each section's origin is
        // written once, at upload (`upload_packed_section`), and selected at draw
        // time by a dynamic offset.
        //
        // Carries the real fog since issue #400: the same `FogUniform` the model
        // path gets, so the demo world and every headless gate now fade with
        // distance and darken at night instead of rendering at a permanent noon.
        update_model_shared_camera_buffer(
            queue,
            &self.packed_shared_cam_buffer,
            view_proj,
            fog,
        );

        // The model sections' (live vanilla path) shared camera+fog buffer:
        // **one** write, not one per section. Fog is folded into the group-0
        // uniform: the eye position (for per-fragment view distance) and this
        // frame's fog settings travel with it, keeping the model shader within
        // four bind groups. Each section's own origin was written once, at
        // upload (`upload_section`/`SectionOriginArena::alloc`) — it is
        // constant for the section's life, so there is nothing left to
        // rewrite here. This replaced a `queue.write_buffer` per *section*
        // per frame (up to ~4000/frame at the `sections=3880` measured in
        // issue #75's profile); see the module doc.
        if let Some(model) = &self.model {
            update_model_shared_camera_buffer(queue, &model.shared_cam_buffer, view_proj, fog);
        }

        // Outline vertices/uniform must be written before the pass opens.
        if let Some(block) = outline {
            let boxes = self.outline_shape.sample(block);
            self.outline.prepare(
                queue,
                &view_proj,
                block,
                &boxes,
                (self.depth.width, self.depth.height),
            );
        }

        // Same constraint for the debug-line pass: sample and upload before
        // the pass opens. Zero vertices (the default, until a caller installs
        // `set_debug_lines_source`) is a cheap no-op, not a wasted upload —
        // `prepare` returns early on an empty slice.
        let debug_line_count =
            self.debug_lines
                .prepare(queue, &view_proj, &self.debug_lines_source.sample());

        let mut stats = RenderStats::default();

        // The local player's own third-person body, if a caller has wired one
        // in (see `set_third_person_body_source`). `None` — true for every
        // caller today, since no third-person camera exists — reproduces this
        // function's behaviour before this existed exactly: `entities` passes
        // straight through unmodified and the arm draws unconditionally
        // below.
        let body_state = self.third_person_body.sample();
        stats.third_person_body_drawn = body_state.is_some();
        let mut entities_with_body: Vec<EntityDraw>;
        let entities: &[EntityDraw] = match body_state {
            Some(state) => {
                entities_with_body = entities.to_vec();
                entities_with_body.push(state.into_draw());
                &entities_with_body
            }
            None => entities,
        };

        // Nametag vertices (issue #100), same "upload before the pass opens"
        // constraint as outline/debug-lines above. Reads the same
        // (possibly body-extended) `entities` slice; the local third-person
        // body's own draw always carries `name_tag: None`
        // (`ThirdPersonBodyState::into_draw`), so this is a no-op for it.
        let name_tag_counts = self.nametag.prepare(queue, &view_proj, camera, entities);

        // Sign text (issue #23), same "upload before the pass opens"
        // constraint. Not derived from `entities` — a sign is a *block*,
        // gathered from the world's block-entity records exactly like
        // `block_entity_source`/`skull_source` below, just with no cull or
        // batch step of its own (see `gpu/sign_text.rs`'s module doc for why
        // this is not a billboard and needs no camera basis).
        let signs = self.sign_source.signs(camera.position);
        let sign_text_count = self.sign_text.prepare(queue, &view_proj, &signs);
        stats.sign_text_vertices = sign_text_count;

        // Resolve, frustum-cull and upload entity instances *before* the pass —
        // buffers can't be created mid-pass, and the entity camera uniform (no
        // section origin; the world position lives in each instance matrix) must
        // be written first too.
        let entity_batches = self.prepare_entities(device, queue, camera, entities, &mut stats);

        // Humanoid armour layers, over the same instances — resolved from the
        // same `entities` slice and the same resolver, so a helmet cannot be
        // posed off a head the body pass did not draw. Uploaded here for the
        // usual reason: no buffer creation mid-pass.
        let armour_batches = self.prepare_armour(device, camera, entities, &mut stats);

        // The sheep wool layer (issue #53), over the same instances, for the
        // same reason armour is: no buffer creation mid-pass, and never posed
        // off a pose the body pass did not also draw.
        let wool_batches = self.prepare_wool(device, camera, entities, &mut stats);

        // The mob-fire billboard (issue #434), over the same instances, for
        // the same reason armour/wool are: no buffer creation mid-pass.
        let flame_batches = self.prepare_flame(device, camera, entities, &mut stats);

        // Block entities (chests, issue #23). Not derived from `entities` — a
        // chest is a *block*, gathered from the world's block-entity records by
        // the installed source — but uploaded here for the same reason as
        // everything above: buffers cannot be created mid-pass.
        let block_entity_batches = self.prepare_block_entities(device, queue, camera, &mut stats);

        // Dropped items *and* items in mobs' hands, meshed and uploaded before
        // the pass for the same reason as everything else here (no buffer
        // creation mid-pass). Both are item models through the model pipeline,
        // so they share one buffer and one draw call. This reads the same
        // (possibly body-extended) `entities` slice above, so the local
        // player's own held item renders through `merge_held_items` exactly
        // like a mob's does, for free.
        let item_mesh = self.prepare_item_geometry(device, camera, entities, &mut stats);

        // The first-person arm. Skipped whenever a third-person body drew this
        // frame — see `set_third_person_body_source`'s doc for why the two
        // must never draw together. Prepared here, drawn in its own pass at
        // the end of the frame — see the note there for why it needs a
        // second pass.
        let first_person_hand = if stats.third_person_body_drawn {
            None
        } else {
            self.prepare_first_person_hand(device, queue, camera)
        };
        stats.first_person_arm_drawn = matches!(first_person_hand, Some(FirstPersonHand::Arm(_)));
        stats.first_person_item_drawn = matches!(first_person_hand, Some(FirstPersonHand::Item(..)));

        // Build every mining-crack overlay mesh before the pass (buffers can't be
        // created mid-pass) — one per entry in `cracks`, not just the local
        // player's own dig (issue #410: `CrackPipeline` used to draw at most one
        // target, so another player's crack overlay had nowhere to go even
        // though `SessionBlockDestruction` already carried it). Each follows its
        // own target block's real model geometry; an air or unknown state, an
        // out-of-range stage, or a block whose model has no faces yields no mesh
        // for that entry and the rest still draw. The crack camera uses
        // world-space positions (section origin zero) and is shared by every
        // crack draw call this frame, so its uniform is written at most once,
        // only when there is at least one mesh to draw.
        let crack_meshes: Vec<GpuCrackMesh> = self.model.as_ref().map_or_else(Vec::new, |model| {
            let meshes: Vec<GpuCrackMesh> = cracks
                .iter()
                .filter_map(|target| {
                    let origin = [
                        target.block[0] as f32,
                        target.block[1] as f32,
                        target.block[2] as f32,
                    ];
                    let mesh = model
                        .crack_resolver
                        .mesh_for(target.state_id, target.stage, origin)?;
                    GpuCrackMesh::upload(device, &mesh)
                })
                .collect();
            if !meshes.is_empty() {
                queue.write_buffer(
                    &model.crack_cam_buffer,
                    0,
                    bytemuck::bytes_of(&CameraUniform {
                        view_proj,
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    }),
                );
            }
            meshes
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });

        // The sky pass, if installed — its own render pass with no depth
        // attachment, run *before* the block pass (`SkyRenderer::render`'s own
        // doc: it must run first and take no depth, so it can never occlude
        // terrain and terrain always draws over it normally). It clears the
        // target itself, so the block pass below must switch from its own
        // `Clear` to a `Load` — clearing twice would just discard the sky.
        //
        // **That clear is the below-horizon void, not a scratch value.** The sky
        // disc is a finite overhead plane: everything under the horizon line
        // keeps the clear colour until terrain paints over it, and wherever
        // terrain does not reach (open ocean past the render distance, an
        // unmeshed chunk) the clear is what the player sees. It was
        // `Color::BLACK` for as long as this pass existed, which is the reported
        // "the skybox ends too early and the bottom half is always black" — a
        // hard *pure black* band with a flat top edge at the horizon. Vanilla
        // clears the same target to the fog colour in a separate `"clear"` pass
        // (`LevelRenderer.java:195-204`) and its `SkyRenderer` passes never
        // clear at all. `SkyFrame::clear_color` is that colour, resolved for
        // this frame's clock and eye height so it is identical to the disc's own
        // rim.
        stats.sky_drawn = if let Some(sky) = &self.sky {
            // The disc's *centre* colour is `self.fog.sky_color`, not
            // `self.clear`. Those two were the same value until #96's biome tint:
            // the shell sets the clear colour from `FogSettings::color`, so
            // reading the clear here made the disc centre and the horizon
            // structurally identical and a per-biome tint had nowhere to enter.
            // `sky_color` defaults to `color` in every `FogSettings`
            // constructor, so a caller that never tints is byte-identical to the
            // old behaviour — and the two colours travel in one struct so they
            // cannot be set out of step (see `FogSettings`' own doc).
            let day_sky_color = self.fog.sky_color;
            // The horizon end of the sky dome's gradient is the *fog* colour,
            // not a second sky constant — `self.fog.color` is already whatever
            // `set_fog` last computed for this dimension/submersion state, and
            // `set_clear_color`'s doc records that a second, independently
            // maintained copy of the sky colour is exactly how the horizon has
            // banded in a colour the sky never is. Void fog uses the vanilla
            // overworld geometry (`VoidFog::OVERWORLD`, `min_y = -64`,
            // `onset_range = 32`) because the dimension's real height is not
            // threaded to this layer yet; see `docs/sky-and-air-bubbles.md`.
            let frame = lodestone_render::SkyFrame::new(
                self.time_of_day.value(),
                day_sky_color,
            )
            .with_fog_color(self.fog.color)
            .with_render_distance(self.render_distance_chunks)
            .with_void_fog(lodestone_render::fog::VoidFog::OVERWORLD);
            let clear = frame.clear_color_wgpu(camera.position.y);
            sky.render(device, queue, &mut encoder, view, camera, &frame, clear);
            true
        } else {
            false
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("block pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // `Load` only when the sky actually drew this frame —
                        // never unconditionally. With no sky installed there is
                        // nothing upstream that touched `view` at all, and
                        // `Load` over an untouched/previous-frame target reads
                        // as garbage or smeared history, not as "missing sky".
                        load: if stats.sky_drawn {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(self.clear)
                        },
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
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            for section in self.sections.values() {
                // One bind group for the whole packed table; the section is
                // selected by the dynamic offset of its origin slot (issue #76).
                pass.set_bind_group(
                    0,
                    &self.packed_cam_bind_group,
                    &[section.origin_alloc.offset() as u32],
                );
                pass.set_vertex_buffer(0, section.mesh.vertices.slice(..));
                pass.set_index_buffer(section.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..section.mesh.index_count, 0, 0..1);
                stats.sections_drawn += 1;
                stats.draw_calls += 1;
                stats.total_quads += section.quad_count;
            }

            if let Some(model) = &self.model {
                static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let t = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if t <= 10 || t % 120 == 0 {
                    tracing::error!(
                        "draw tick {t}: {} model sections, {} packed sections",
                        model.sections.len(), self.sections.len(),
                    );
                }
                pass.set_pipeline(&model.pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.palette_bind_group, &[]);
                pass.set_bind_group(3, &model.anim_bind_group, &[]);
                for section in model.sections.values() {
                    let Some(mesh) = section.mesh.as_ref() else {
                        continue;
                    };
                    // One shared bind group for every section; only the
                    // dynamic offset (this section's slot in the origin
                    // arena) changes per draw.
                    pass.set_bind_group(
                        0,
                        &model.cam_bind_group,
                        &[section.origin_alloc.offset() as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.sections_drawn += 1;
                    stats.draw_calls += 1;
                    stats.total_quads += section.quad_count;
                }
            }

            // Entities share the terrain depth buffer (depth test + write on, so
            // a mob behind a wall is correctly occluded and vice versa), drawn
            // after opaque terrain in the same pass so no second clear touches
            // depth.
            //
            // **Before the translucent water below, as vanilla orders it**
            // (`SOLID`/`CUTOUT`, entities, destroy progress, `TRANSLUCENT`).
            // Water is alpha-blended with depth *write* off, so it leaves no
            // depth behind it: a submerged mob drawn afterwards passes the depth
            // test against the sea floor and overwrites the water surface
            // opaquely, so it appears painted on top of the water however deep
            // it is. Drawing entities first puts the mob in the depth buffer,
            // and the water surface then blends over it. Fogging the entity
            // shader is a separate fix and does not achieve this on its own:
            // fog tints a mob by distance, it does not put water in front of it.
            if !entity_batches.is_empty() {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                for batch in &entity_batches {
                    let Some(model) = self.entities.gpu_models.get(batch.model) else {
                        continue;
                    };
                    let Some(texture) = self.entities.textures.get(batch.model) else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, buffer) in model.parts.iter().zip(&batch.parts) {
                        let (Some(buffer), true) = (buffer.as_ref(), range.index_count > 0) else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                        stats.draw_calls += 1;
                    }
                }
            }

            // Humanoid armour, immediately after the bodies it sits on and
            // before anything else — the pieces are physically outside the mob
            // (the smallest inflation is +0.4 texels) so the depth buffer sorts
            // body against armour on its own, but a coplanar *pair* of armour
            // layers does not sort itself. That is why this uses the armour
            // pipeline's `LessEqual` compare, and why `armour_batches` is walked
            // in its accumulation order: leather's untinted `leather_overlay`
            // sits exactly on its dyeable base and only wins by being second.
            //
            // Group 0 is the world entity camera, still bound from the pass
            // above; group 1 is rebound per armour texture.
            if !armour_batches.is_empty() {
                pass.set_pipeline(&self.entities.armour_pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                for batch in &armour_batches {
                    let Some(model) = self.entities.armour_model(batch.slot) else {
                        continue;
                    };
                    let Some(texture) = self.entities.armour_textures.get(&batch.texture) else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, buffer, count) in &batch.parts {
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..*count);
                        stats.draw_calls += 1;
                    }
                }
            }

            // The sheep wool layer (issue #53), right after armour and before
            // dropped items. Through the **base** entity pipeline (`Less`),
            // not `armour_pipeline` (`LessEqual`) — wool has no second layer
            // at the same inflation to correct z-fighting for, so copying
            // armour's compare function here would be picking a pipeline for
            // the wrong reason. See `EntityRenderer::wool_texture`'s doc and
            // `docs/entity-rendering.md`.
            if !wool_batches.is_empty() {
                if let (Some(model), Some(texture)) =
                    (&self.entities.wool_gpu, &self.entities.wool_texture)
                {
                    pass.set_pipeline(&self.entities.pipeline.pipeline);
                    pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, buffer, count) in &wool_batches {
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..*count);
                        stats.draw_calls += 1;
                    }
                }
            }

            // The mob-fire billboard (issue #434), right after wool and
            // before block entities — cutout with depth write on, same as
            // every other opaque-cutout entity layer in this pass.
            if !flame_batches.is_empty() {
                if let Some(texture) = &self.entities.flame_texture {
                    pass.set_pipeline(&self.entities.flame_pipeline);
                    pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                    pass.set_bind_group(1, texture, &[]);
                    for batch in &flame_batches {
                        let Some(model) = self.entities.flame_gpu_models.get(&batch.model) else {
                            continue;
                        };
                        pass.set_vertex_buffer(0, model.vertices.slice(..));
                        pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.set_vertex_buffer(1, batch.buffer.slice(..));
                        pass.draw_indexed(0..model.index_count, 0, 0..batch.count);
                        stats.draw_calls += 1;
                    }
                }
            }

            // Block entities (chests, issue #23) — after the mob layers and
            // **before translucent water**, exactly where the mobs sit and for
            // the same reason: this pass is opaque-cutout with depth write on, so
            // drawing it after water would paint a submerged chest over the water
            // surface however deep it was. Vanilla's own order is the same
            // (`SOLID`/`CUTOUT`, block entities and entities, then `TRANSLUCENT`).
            //
            // Its own group-0 bind group, not `self.entities.cam_bind_group`:
            // both hold the same matrix this frame, but they are separate buffers
            // so the two passes can never silently share a stale write. This is a
            // second bind group over the *existing* two-group layout, not a fifth
            // group — see `gpu/block_entities.rs` on the 4-group floor.
            if !block_entity_batches.is_empty() {
                pass.set_pipeline(&self.block_entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.block_entities.cam_bind_group, &[]);
                for batch in &block_entity_batches {
                    let Some(model) = self.block_entities.gpu_models.get(batch.model) else {
                        continue;
                    };
                    // Keyed by *sheet*, not model: a trapped chest shares the
                    // single-chest mesh and differs only here.
                    let Some(texture) = self.block_entities.textures.get(batch.texture) else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, buffer) in model.parts.iter().zip(&batch.parts) {
                        let (Some(buffer), true) = (buffer.as_ref(), range.index_count > 0) else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                        stats.draw_calls += 1;
                    }
                }
            }

            // Sign text (issue #23), right after the block entities and
            // before translucent water — a sign's board is real terrain
            // (unlike a chest, it has a genuine block model), so by this
            // point in the pass it is already in the depth buffer for the
            // text's own polygon-offset bias to win against. See
            // `gpu/sign_text.rs`'s module doc for the depth pipeline.
            self.sign_text.draw(&mut pass, sign_text_count);

            if let Some(model) = &self.model {
                // Dropped items, through the *model* pipeline rather than the
                // entity one: an item entity is an item model, not a cuboid
                // rig. Same atlas / palette / animation bind groups as terrain,
                // so a dropped block is textured from exactly the pixels the
                // placed block is. Opaque and depth-writing, drawn alongside the
                // mobs and before translucent water for the same reason they
                // are (see the entity note above).
                if let Some(mesh) = &item_mesh {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    // Dropped-item geometry bakes world positions into its own
                    // vertices (spin/bob included), so it has no origin of its
                    // own: bind the shared arena's reserved zero slot.
                    pass.set_bind_group(0, &model.cam_bind_group, &[model.origin_arena.zero_offset()]);
                    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                }

                // Mining-crack overlays, drawn after the opaque terrain they sit
                // on (so the block face is already in the depth buffer) and
                // before translucent water. One draw call per target — the local
                // player's own dig and any number of other players' (issue
                // #410) — each independently textured with its own destroy-stage
                // sprite; the pipeline's negative depth bias pulls every one of
                // them toward the camera so its texels win the depth test
                // against its own coplanar face without z-fighting;
                // alpha-blended, depth-write off. Bind groups are shared and set
                // once outside the loop rather than re-bound per draw.
                if !crack_meshes.is_empty() {
                    pass.set_pipeline(&model.crack_pipeline.pipeline);
                    pass.set_bind_group(0, &model.crack_cam_bind_group, &[]);
                    pass.set_bind_group(1, &model.crack_atlas_bind_group, &[]);
                    for crack in &crack_meshes {
                        pass.set_vertex_buffer(0, crack.vertices.slice(..));
                        pass.set_index_buffer(crack.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..crack.index_count, 0, 0..1);
                        stats.draw_calls += 1;
                        stats.cracks_drawn += 1;
                    }
                }

                // Translucent water, drawn after all opaque model terrain so the
                // sea floor already written to depth shows through the surface
                // (depth test on, depth write off, alpha blend — the fluid
                // pipeline). Same camera + atlas bind groups as the opaque pass.
                pass.set_pipeline(&model.water_pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.water_anim_bind_group, &[]);
                for section in model.sections.values() {
                    let Some(water) = section.water.as_ref() else {
                        continue;
                    };
                    pass.set_bind_group(
                        0,
                        &model.cam_bind_group,
                        &[section.origin_alloc.offset() as u32],
                    );
                    pass.set_vertex_buffer(0, water.vertices.slice(..));
                    pass.set_index_buffer(water.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..water.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.total_quads += section.water_quad_count;
                }
            }

            // Debris last among the world geometry: it is alpha-blended with
            // depth write off, so it must read a depth buffer that already holds
            // every opaque surface, or fragments behind a wall would show
            // through. The outline is drawn after it, as vanilla does.
            self.particles
                .draw(&mut pass, &self.particle_atlas_bind_group);
            stats.particles_drawn = self.particles.count();
            stats.particles_from_sheet = self.particles.sheet_count();
            stats.particle_sheet_atlas_bound = self.particle_sheet_atlas.is_some();

            // Precipitation after the debris, for exactly the reason the debris is
            // after the terrain: alpha-blended with depth write off, so it needs a
            // depth buffer that already holds every opaque surface or rain shows
            // through walls. Before the outline, which is a UI-ish overlay and
            // should read over the weather rather than be rained on.
            //
            // Vanilla runs this as its own pass against a dedicated
            // `WEATHER_TARGET` (`WeatherEffectRenderer.render`) because it feeds
            // its transparency-sorting chain; this client has no such chain, so a
            // second pass would only cost a depth attachment. See
            // `lodestone_render::weather_pipeline`'s module doc.
            if let Some(weather) = &self.weather {
                weather.draw(&mut pass);
                if weather.count() > 0 {
                    // Two draws when the frame is mixed rain and snow, one
                    // otherwise — the pass skips an empty range itself.
                    stats.draw_calls += usize::from(weather.rain_count() > 0)
                        + usize::from(weather.count() > weather.rain_count());
                }
            }

            if outline.is_some() {
                self.outline.draw(&mut pass);
            }

            // After the outline, for the same reason it is after debris: it
            // is a diagnostic overlay, so it should read clearly over
            // everything real that was drawn this frame.
            self.debug_lines.draw(&mut pass, debug_line_count);

            // Nametags (issue #100) last of all, real depth-tested against
            // this same terrain+entity depth buffer — see `gpu/nametag.rs`'s
            // module doc for the normal/see-through split and their exact
            // depth settings.
            self.nametag.draw(&mut pass, name_tag_counts);
        }

        // The first-person arm/held-item pass: its own pass, with the depth
        // buffer cleared. See [`Self::draw_first_person_hand`] for why the
        // clear is there and why it is not optional.
        if let Some(hand) = &first_person_hand {
            self.draw_first_person_hand(&mut encoder, view, hand, &mut stats);
        }

        // The screen overlays (issues #108, #112, #185, #139, #154, #144,
        // #149): their own `Load` passes (see
        // `ScreenEffectRenderer::draw_underwater`'s doc — they must not erase
        // the world/hand just drawn), run last, matching vanilla's own order
        // (`GameRenderer.java:568-577`: the hand, then
        // `screenEffectRenderer.submit`/`Hud.extractCameraOverlays`, then the
        // HUD/feature renderers — this shell's HUD draws in a later, separate
        // pass in `app.rs`).
        //
        // Two independent gate groups, not one — see
        // `ScreenEffects::any_active`'s doc for why: underwater/fire/pumpkin/
        // spyglass are first-person-only in vanilla, freeze/confusion/portal
        // are not (`Hud.java:293-308` are siblings of the `isFirstPerson`
        // block, not nested in it), so each group re-checks its own
        // applicability here rather than relying on the outer `any_active`
        // short-circuit alone — that call only proves *something* should
        // draw, not that a first-person-only flag is safe to act on in third
        // person.
        if let Some(fx) = &self.screen_effects {
            let first_person = !stats.third_person_body_drawn;
            if screen_effects.any_active(first_person) {
                if screen_effects.first_person_group_active(first_person) {
                    if screen_effects.eye_in_water {
                        let light = self.entity_light.sample(camera.position);
                        fx.draw_underwater(queue, &mut encoder, view, camera.yaw, camera.pitch, light);
                        stats.underwater_overlay_drawn = true;
                    }
                    if screen_effects.on_fire {
                        fx.draw_fire(queue, &mut encoder, view, screen_effects.tick);
                        stats.fire_overlay_drawn = true;
                    }
                    if screen_effects.wearing_pumpkin {
                        fx.draw_pumpkin(&mut encoder, view);
                        stats.pumpkin_overlay_drawn = true;
                    }
                    if screen_effects.scoping {
                        fx.draw_spyglass(queue, &mut encoder, view, camera.aspect);
                        stats.spyglass_overlay_drawn = true;
                    }
                }
                if screen_effects.camera_agnostic_group_active() {
                    if screen_effects.freeze_percent > 0.0 {
                        fx.draw_freeze(queue, &mut encoder, view, screen_effects.freeze_percent);
                        stats.freeze_overlay_drawn = true;
                    }
                    // Portal takes priority over confusion when both are
                    // positive — `Hud.java:300-302`'s own `if`/`else if`.
                    if screen_effects.portal_intensity > 0.0 {
                        let frame = (screen_effects.tick % u64::from(fx.portal_frame_count())) as u32;
                        fx.draw_portal(queue, &mut encoder, view, frame, screen_effects.portal_intensity);
                        stats.portal_overlay_drawn = true;
                    } else if screen_effects.nausea_intensity > 0.0 {
                        fx.draw_confusion(queue, &mut encoder, view, screen_effects.nausea_intensity);
                        stats.confusion_overlay_drawn = true;
                    }
                }
            }
        }

        queue.submit(std::iter::once(encoder.finish()));

        stats.vram_bytes = vram_bytes(stats.total_quads);
        stats
    }
}
