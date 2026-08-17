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
//!   why weather, the outline, debug lines and nametags all sit below it.
//! * **Particles straddle that line, exactly as vanilla's do.** Vanilla submits
//!   one particle group twice — into the `solid` phase and into `afterTerrain`
//!   — and each draw keeps only the `SingleQuadParticle.Layer`s whose
//!   `translucent()` matches, so `Layer::Opaque` particles land *before*
//!   translucent terrain and `Layer::Translucent` ones after. Both halves were
//!   below the water draw here, which is why breaking a block underwater threw
//!   debris that drew on top of the surface. The opaque half now draws with the
//!   water, from a depth-writing pipeline; see `crate::particles`.
//!
//! The first-person hand then gets its **own** pass with the depth buffer
//! cleared (vanilla's `GameRenderer.renderLevel` does the same before
//! `renderItemInHand`), and the screen overlays get theirs, on `Load`, last.
//! See [`super::first_person`] and `docs/screen-overlays.md`.
use lodestone_render::{
    Camera, CameraUniform, CullVerdict, TerrainCull, crack_pipeline::GpuCrackMesh,
    spinning_effect_angle_degrees, update_model_shared_camera_buffer,
};

use crate::entities::EntityDraw;

use super::first_person::FirstPersonHand;
use super::terrain::TerrainDraw;
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
    /// overlay for every target in `cracks` (other players' digs, not
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
    /// screen-overlay pass from `screen_effects`. A
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
    /// local player's own dig, any number of other players', or
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
        // Cache this frame's camera block position for `upload_section`'s
        // near-distance fade skip — see `RenderState::last_camera_block_pos`'s
        // doc for why this is the one write site rather than a threaded
        // parameter, and why one frame of staleness is harmless here. Vanilla
        // reads the same rounding (`BlockPos cameraPosition = camera.blockPos`,
        // i.e. floored world coordinates), not the eye position's fractional
        // part.
        self.last_camera_block_pos.set(Some([
            camera.position.x.floor() as i32,
            camera.position.y.floor() as i32,
            camera.position.z.floor() as i32,
        ]));

        // Shared world-projection "spinning" warp fix — see
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
        // `P · bobHurt · warp · V`, in vanilla's own order: `renderLevel` does
        // `projectionMatrix.mul(bobStack)` **first** and applies the spin after,
        // so the bob sits to the left of the warp. Reversing them would put the
        // spin's skew on the unbobbed axis — subtly wrong rather than obviously.
        let view_proj = camera
            .view_projection_eye_space(
                self.eye_bob() * lodestone_render::nausea_portal_warp(warp_intensity, warp_angle_degrees),
            )
            .to_cols_array_2d();

        // This frame's fog **and** its `sky_darken` lane, hoisted above both
        // terrain paths so they physically cannot disagree. It used
        // to be computed inside the `if let Some(model)` below, which is why the
        // packed path had no way to reach it.
        let fog = self.fog_with_clock(camera.position);

        // The packed sections' shared camera buffer: **one** write, not one per
        // section. Until that fix this was a `queue.write_buffer` per resident
        // packed section, every frame, rewriting the whole 80-byte uniform just
        // to re-aim the camera — the same shape that fix profiled at 52.9% of
        // main-thread CPU on the model path, left in place here because the
        // packed table only ever holds the demo world. Each section's origin is
        // written once, at upload (`upload_packed_section`), and selected at draw
        // time by a dynamic offset.
        //
        // Carries the real fog since that fix: the same `FogUniform` the model
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
        // that fix's profile); see the module doc.
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

        // Same constraint for the plugin-billboard pass (issue #161): sample
        // and upload before the pass opens. Zero instances (the default,
        // until a caller installs `set_plugin_billboards_source`) is a cheap
        // no-op, not a wasted upload — `prepare` returns early on an empty
        // slice, mirroring the debug-line pass immediately above.
        let plugin_billboard_count = self.plugin_billboards.prepare(
            queue,
            &view_proj,
            camera,
            &self.plugin_billboards_source.sample(),
        );

        let mut stats = RenderStats::default();

        // This frame's terrain cull, computed **once** and consulted by all three
        // terrain loops below (packed table, live opaque, live water) so water and
        // terrain physically cannot disagree about what exists. Vanilla's circular
        // view membership ∩ the camera-cube-offset frustum ∩ (when a walk is
        // installed) the occlusion graph's reachable set — see
        // `lodestone_render::cull`. Before this, every resident section issued a
        // draw at every heading: 19,024 instructions per section, 17.7M per frame
        // at the shipped render distance 8.
        //
        // Deliberately *not* the warped `view_proj` above: the nausea/portal warp
        // is a post-projection screen distortion with no live producer, and
        // culling against a warped frustum would drop geometry the warp pulls back
        // into view. `camera.frustum()` is the honest view volume.
        //
        // The reachable set is the occlusion graph's camera walk (U3), cached
        // across frames and re-walked only on an 8-block camera-cell crossing or
        // a graph change — vanilla's cadence, and the reason rotation is free.
        // `None` (no graph yet, the packed demo path, render distance 0, or
        // `TerrainOcclusion::Off`) leaves the cull at distance ∩ frustum, which
        // draws *more*; `occlusion_active` is what tells that apart from a frame
        // with nothing occluded. See `gpu/occlusion.rs`.
        let reachable = self.frame_reachable(camera);
        let occlusion_mode = match self.terrain_occlusion() {
            super::TerrainOcclusion::Shadow => lodestone_render::OcclusionMode::Shadow,
            super::TerrainOcclusion::Off | super::TerrainOcclusion::On => {
                lodestone_render::OcclusionMode::Enforce
            }
        };
        let terrain_cull = TerrainCull::new(camera, self.render_distance_chunks)
            .with_reachable_mode(reachable, occlusion_mode)
            .disabled(!self.terrain_culling);
        stats.occlusion_active = terrain_cull.occlusion_active();
        stats.occlusion_graph_sections = self.occlusion_graph_sections();
        stats.occlusion_walks = self.occlusion_walks();

        // The local player's own third-person body, if a caller has wired one
        // in (see `set_third_person_body_source`). `None` reproduces this
        // function's behaviour before this existed exactly: `entities` passes
        // straight through unmodified and the arm draws unconditionally
        // below.
        //
        // `None` is `CameraType::isFirstPerson()`, which is **not** "there is no
        // third-person camera" — that note was true when this was written and is
        // not now. `Sim::third_person_body_state` returns `Some` in *both* of
        // vanilla's detached modes, back and front, so `third_person_body_drawn`
        // below correctly suppresses the arm and the first-person overlay group
        // in the front view too.
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

        // Nametag vertices, same "upload before the pass opens"
        // constraint as outline/debug-lines above. Reads the same
        // (possibly body-extended) `entities` slice; the local third-person
        // body's own draw always carries `name_tag: None`
        // (`ThirdPersonBodyState::into_draw`), so this is a no-op for it.
        let name_tag_counts = self.nametag.prepare(queue, &view_proj, camera, entities);

        // Sign text, same "upload before the pass opens"
        // constraint. Not derived from `entities` — a sign is a *block*,
        // gathered from the world's block-entity records exactly like
        // `block_entity_source`/`skull_source` below, just with no cull or
        // batch step of its own (see `gpu/sign_text.rs`'s module doc for why
        // this is not a billboard and needs no camera basis).
        let signs = self.sign_source.signs(camera.position);
        let sign_text_count = self.sign_text.prepare(queue, &view_proj, &signs);
        stats.sign_text_vertices = sign_text_count;

        // Beacon beams, same "upload before the pass opens" constraint and
        // the same not-derived-from-`entities` shape as sign text above — a
        // beacon is a *block*, gathered from world state. See
        // `gpu/beacon_beam.rs`'s module doc for why this returns two counts
        // (solid core / outer glow) rather than one.
        let beacons = self.beacon_source.beacons(camera.position);
        let (beacon_solid_count, beacon_glow_count) =
            self.beacon_beam.prepare(queue, &view_proj, &beacons);
        stats.beacon_beam_solid_vertices = beacon_solid_count;
        stats.beacon_beam_glow_vertices = beacon_glow_count;

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

        // The sheep wool layer, over the same instances, for the
        // same reason armour is: no buffer creation mid-pass, and never posed
        // off a pose the body pass did not also draw.
        let wool_batches = self.prepare_wool(device, camera, entities, &mut stats);

        // Player capes, over the same instances, for the same reason
        // armour/wool are: no buffer creation mid-pass, and never posed off a
        // pose the body pass did not also draw. Grouped by cape URL rather
        // than by wearer part — see `RenderState::prepare_cape`'s doc.
        let cape_batches = self.prepare_cape(device, camera, entities, &mut stats);

        // The mob-fire billboard, over the same instances, for
        // the same reason armour/wool are: no buffer creation mid-pass.
        let flame_batches = self.prepare_flame(device, camera, entities, &mut stats);

        // Experience-orb billboards, over the same `entities` slice and for the
        // same reason as everything above: no buffer creation mid-pass. An orb has
        // no cuboid rig, so `prepare_entities` above skips it entirely — this is
        // the only thing that puts an orb on screen.
        let orb_batches = self.prepare_orbs(device, camera, entities, &mut stats);

        // Block entities (chests, that fix). Not derived from `entities` — a
        // chest is a *block*, gathered from the world's block-entity records by
        // the installed source — but uploaded here for the same reason as
        // everything above: buffers cannot be created mid-pass.
        // The `entities` slice is passed in for the three `minecraft:special` item
        // surfaces (a dropped chest, a chest in a mob's hand, a chest in an item
        // frame): those are entities, but they draw through the block-entity rig,
        // so they belong to this pass and not `prepare_item_geometry`'s.
        let (block_entity_batches, banner_layer_batches) =
            self.prepare_block_entities(device, queue, camera, entities, &mut stats);

        // Dropped items *and* items in mobs' hands, meshed and uploaded before
        // the pass for the same reason as everything else here (no buffer
        // creation mid-pass). Both are item models through the model pipeline,
        // so they share one buffer and one draw call. This reads the same
        // (possibly body-extended) `entities` slice above, so the local
        // player's own held item renders through `merge_held_items` exactly
        // like a mob's does, for free.
        let (item_mesh, item_glint_mesh) =
            self.prepare_item_geometry(device, camera, entities, &mut stats);
        // Moving block models — falling sand/gravel today (`gpu/moving_blocks.rs`).
        // Its **own** buffer rather than merged into `item_mesh`, even though both
        // draw through the same model pipeline: an item model and a block model are
        // different geometry sources with different pose and light rules, and the
        // seam has a second intended producer (piston heads) that has nothing to do
        // with items. Prepared here for the reason everything here is: buffers
        // cannot be created mid-pass.
        let moving_block_mesh = self.prepare_moving_blocks(device, camera, entities, &mut stats);
        // Maps in item frames. Built here rather than inside the pass
        // for the reason above — it creates a texture and a bind group — and kept
        // separate from `item_mesh` because it draws with a different group 1.
        let framed_maps = self.prepare_framed_maps(device, queue, camera, entities);
        // The world glint's group 0, written here (the `&self` + queue point of the
        // frame) and consumed inside the pass below. Item geometry bakes world
        // positions into its vertices, so the matrix is the plain camera one — the
        // same clip positions the base draw produces, which is what depth-`EQUAL`
        // requires.
        if item_glint_mesh.is_some() {
            self.write_world_glint_uniform(queue, self.world_view_projection(camera).to_cols_array_2d());
        }

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
        // `Special` counts as an item drawn, not as a third state: the question this
        // flag answers is "is the hand holding something visible", and a held chest is
        // as much an item in the hand as a pickaxe. It only *draws* through a
        // different pipeline. Leaving it out would report `false` for exactly the case
        // this branch exists to fix, which is the shape of an island counter.
        stats.first_person_item_drawn = matches!(
            first_person_hand,
            Some(FirstPersonHand::Item(..) | FirstPersonHand::Special(..))
        );

        // Build every mining-crack overlay mesh before the pass (buffers can't be
        // created mid-pass) — one per entry in `cracks`, not just the local
        // player's own dig (`CrackPipeline` used to draw at most one
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
        // (`LevelRenderer.java`) and its `SkyRenderer` passes never
        // clear at all. `SkyFrame::clear_color` is that colour, resolved for
        // this frame's clock and eye height so it is identical to the disc's own
        // rim.
        stats.sky_drawn = if let Some(sky) = &self.sky {
            // The disc's *centre* colour is `self.fog.sky_color`, not
            // `self.clear`. Those two were the same value until that fix's biome tint:
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
            .with_void_fog(lodestone_render::fog::VoidFog::OVERWORLD)
            // Vanilla's Clouds option. This builder had **zero** production
            // callers, so the pass always drew `CloudStatus::default()` (FANCY):
            // the FAST quad path and the OFF case both existed in
            // `SkyRenderer::render` and no player could select either.
            .with_cloud_status(self.cloud_status)
            // The connected dimension's own `Skybox`. `SkyMode::None` (the Nether)
            // makes `SkyRenderer::render` clear and return, so `stats.sky_drawn`
            // below stays `true` — the target *was* written, which is exactly what
            // the block pass's `Load`-vs-`Clear` choice depends on. Reporting
            // `false` here instead would double-clear and discard the Nether's red
            // horizon.
            .with_sky_mode(self.sky_mode);
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
            // Tracks the terrain path's own group-0 bind-group object (packed
            // or model camera) across both draw loops below, by pointer
            // identity — see `RenderStats::terrain_camera_bind_group_switches`.
            // Intentionally *not* reset between the packed and model loops:
            // entering the model loop after the packed one is one real switch,
            // which is exactly what the counter should show.
            let mut terrain_cam_group_last: Option<*const wgpu::BindGroup> = None;
            for (key, section) in &self.sections {
                if !terrain_cull.visible(key.coord()) {
                    continue;
                }
                // One bind group for the whole packed table; the section is
                // selected by the dynamic offset of its origin slot.
                bind_terrain_camera(
                    &mut pass,
                    &self.packed_cam_bind_group,
                    section.origin_alloc.offset() as u32,
                    &mut terrain_cam_group_last,
                    &mut stats,
                );
                pass.set_vertex_buffer(0, section.mesh.vertices.slice(..));
                pass.set_index_buffer(section.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..section.mesh.index_count, 0, 0..1);
                stats.sections_drawn += 1;
                stats.draw_calls += 1;
                stats.total_quads += section.quad_count;
            }

            if let Some(model) = &self.model {
                pass.set_pipeline(&model.pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.palette_bind_group, &[]);
                pass.set_bind_group(3, &model.anim_bind_group, &[]);
                // Resolve the visible set first, then emit it grouped by arena
                // block. Two reasons this is a collect-then-emit rather than one
                // loop: the cull is evaluated exactly once per section (a
                // block-outer/section-inner loop would re-run it per block), and
                // consecutive draws from one block share a single pair of buffer
                // binds — which is the whole saving, since every section's
                // geometry is now a span of a shared buffer rather than its own
                // (see `ResidentMesh`).
                let mut draws: Vec<TerrainDraw> = Vec::with_capacity(model.sections.len());
                for (key, section) in &model.sections {
                    let Some(mesh) = section.mesh.as_ref() else {
                        continue;
                    };
                    // The cull, per pass. The counters split by *reason* rather
                    // than one total because the split is the live diagnosis: a
                    // frustum false cull is angle-dependent, a distance one is
                    // position-dependent, and an occlusion one only exists once a
                    // walk is installed. `sections_drawn + the three culled
                    // counters == resident sections with opaque geometry` — and
                    // only that; the water pass closes against its own set below.
                    match terrain_cull.classify(key.coord()) {
                        CullVerdict::Visible => {
                            // Shadow mode: this section is on screen and in range,
                            // the walk says it is unreachable, and we draw it
                            // anyway. Only asked in the `Visible` arm — see
                            // `shadow_would_cull`'s doc for why asking it of an
                            // off-screen section would misattribute the cull.
                            if terrain_cull.shadow_would_cull(key.coord()) {
                                stats.sections_occlusion_shadow += 1;
                            }
                        }
                        CullVerdict::Distance => {
                            stats.sections_culled_distance += 1;
                            continue;
                        }
                        CullVerdict::Frustum => {
                            stats.sections_culled_frustum += 1;
                            continue;
                        }
                        CullVerdict::Occlusion => {
                            stats.sections_culled_occlusion += 1;
                            continue;
                        }
                    }
                    draws.push(TerrainDraw::new(
                        mesh,
                        section.origin_alloc.offset() as u32,
                    ));
                    stats.sections_drawn += 1;
                    stats.total_quads += section.quad_count;
                }
                // Opaque terrain is order-independent (depth sorts it), so this
                // pass orders by arena block — the grouping that removes buffer
                // binds. The water pass below must order by *distance* instead and
                // pays for it; see there.
                draws.sort_unstable_by_key(|d| d.block);
                stats.draw_calls += draws.len();
                stats.terrain_buffer_binds += emit_terrain_draws(
                    &mut pass,
                    model,
                    &draws,
                    &mut terrain_cam_group_last,
                    &mut stats.terrain_camera_bind_group_switches,
                );
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
                // `"boat_water_patch"` is the one model in this loop that
                // must **not** draw through the base pipeline: it is a boat's
                // own extra, colour-write-disabled instance
                // (`gpu/entity_passes.rs`'s `prepare_entities`), and drawing
                // it through the textured pipeline would paint a second,
                // visible mirrored hull plank inside every boat rather than
                // leaving the gap it exists to occlude invisible. See
                // `EntityPipeline::water_mask_pipeline`'s doc. Switched back
                // for the next batch in the same loop rather than split into
                // a second pass, so cam group 0 stays bound throughout.
                let mut water_mask_bound = false;
                for batch in &entity_batches {
                    let Some(model) = self.entities.gpu_models.get(batch.model) else {
                        continue;
                    };
                    let is_water_mask = batch.model == "boat_water_patch";
                    if is_water_mask != water_mask_bound {
                        pass.set_pipeline(if is_water_mask {
                            &self.entities.water_mask_pipeline
                        } else {
                            &self.entities.pipeline.pipeline
                        });
                        water_mask_bound = is_water_mask;
                    }
                    // A fetched player skin wins over the model's own sheet, and a
                    // miss falls through to it. That fallback covers three cases
                    // at once and none of them is an error: no skin declared
                    // (every offline-mode server), a fetch still in flight, and a
                    // fetch that failed. See `EntityRenderer::player_skins`.
                    //
                    // The variant sheet (a wolf's breed, a pig's climate) is tried
                    // next, with the same fallback discipline: a reference the pack
                    // does not ship, or no pack at all, draws the model's default
                    // sheet, which is exactly the behaviour before variants were
                    // resolved. See `EntityDrawBatch::variant_sheet`.
                    let texture = batch
                        .skin
                        .as_ref()
                        .and_then(|url| self.entities.player_skins.get(url))
                        .or_else(|| {
                            batch
                                .variant_sheet
                                .and_then(|s| self.entities.variant_textures.get(s))
                        })
                        .or_else(|| self.entities.textures.get(batch.model));
                    let Some(texture) = texture else {
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
                    // A material sheet or a trim sprite — the same
                    // pipeline, the same mesh and the same parts, differing only in
                    // which bind group lands at group 1. A trim batch is always
                    // ordered after its slot's own layers, which is what lets the
                    // coplanar `LessEqual` compare accept it.
                    let texture = match &batch.texture {
                        crate::gpu::ArmourTextureKey::Sheet(key) => {
                            self.entities.armour_textures.get(key)
                        }
                        crate::gpu::ArmourTextureKey::Trim(id) => {
                            self.entities.trim_textures.get(id)
                        }
                    };
                    let Some(texture) = texture else {
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

            // The sheep wool layer, right after armour and before
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

            // Player capes, right after wool and before the mob-fire
            // billboard. Through the **base** entity pipeline, same reason
            // wool is: a cape has no second layer at the same inflation to
            // correct z-fighting for. Group 1 is rebound per cape URL, off
            // the same `player_skins` cache a body's own skin bind group
            // comes from.
            if !cape_batches.is_empty()
                && let Some(model) = &self.entities.cape_gpu
            {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in &cape_batches {
                    let Some(texture) = self.entities.player_skins.get(&batch.url) else {
                        continue;
                    };
                    let Some(range) = model.parts.first() else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(1, batch.buffer.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            // The mob-fire billboard, right after wool and
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

            // Block entities (chests, that fix) — after the mob layers and
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

            // Banner pattern layers, immediately after the
            // opaque block entities whose depth they sit on.
            //
            // Three things about this loop are load-bearing and none of them fits
            // the batched loop above. It is **ordered**: layer 0 is the base colour
            // and each mask paints over the last, so the list must be drawn in
            // sequence and never sorted or coalesced. It binds a **different mask
            // per draw**, so there is nothing to instance. And it uses the
            // alpha-blended, depth-write-off `banner_layer_pipeline` with
            // `fs_main_no_cutout`, because a mask's soft edges must blend rather
            // than be discarded at alpha 0.5 — the ordinary entity pipeline's
            // cutout is exactly what would turn a pattern into jagged confetti.
            //
            // The geometry is the flag part's, and only the flag's: the pole and
            // bar carry no patterns.
            //
            // **`index_of("flag")`, not `.parts.first()`.** `banner_flag_model`'s
            // root part carries no cube of its own (only a `"flag"` child does), so
            // its own `PartRange` is `index_count == 0` — `.first()` silently
            // selected that empty range, passing every guard here and drawing zero
            // indices every frame. This is the pre-existing island in the world's
            // own banner-pattern pass, found while wiring the GUI-icon and
            // first-person-hand siblings and re-deriving the same guard for them.
            if !banner_layer_batches.is_empty()
                && let Some(flag) = self.block_entities.gpu_models.get("banner_flag")
                && let Some(flag_index) = self
                    .block_entities
                    .models
                    .get("banner_flag")
                    .and_then(|mesh| mesh.index_of("flag"))
                && let Some(range) = flag.parts.get(flag_index)
                && range.index_count > 0
            {
                pass.set_pipeline(&self.block_entities.banner_layer_pipeline);
                pass.set_bind_group(0, &self.block_entities.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, flag.vertices.slice(..));
                pass.set_index_buffer(flag.indices.slice(..), wgpu::IndexFormat::Uint32);
                for layer in &banner_layer_batches {
                    let Some(mask) = self.block_entities.banner_patterns.get(&layer.pattern) else {
                        continue;
                    };
                    pass.set_bind_group(1, mask, &[]);
                    pass.set_vertex_buffer(1, layer.instances.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }

            // Sign text, right after the block entities and
            // before translucent water — a sign's board is real terrain
            // (unlike a chest, it has a genuine block model), so by this
            // point in the pass it is already in the depth buffer for the
            // text's own polygon-offset bias to win against. See
            // `gpu/sign_text.rs`'s module doc for the depth pipeline.
            self.sign_text.draw(&mut pass, sign_text_count);

            // The beacon beam's **solid core** only — opaque, depth-writing
            // (`BEACON_BEAM_OPAQUE`, see `gpu/beacon_beam.rs`'s module doc),
            // so it belongs here with the rest of this pass's opaque/cutout
            // geometry and **before translucent water** for the same reason
            // block entities are: it writes depth, so drawing it after water
            // would paint a beam segment submerged in a pool over the water
            // surface. The outer **glow** is translucent and drawn far below,
            // among the other alpha-blended world geometry.
            self.beacon_beam.draw_solid(&mut pass, beacon_solid_count);

            // Experience-orb billboards. After every opaque and cutout entity
            // layer above, and still **before translucent water** for the reason
            // the mobs and block entities are: an orb writes depth, so drawing it
            // after the water surface would paint a submerged orb over it.
            //
            // Its own pipeline (alpha-blended, `0.1` cutout — vanilla's
            // `ENTITY_TRANSLUCENT`) over the base entity pass's **existing** two
            // bind-group layouts and its camera bind group; an orb needs no camera
            // data the mob pass does not already have. Not a fifth bind group —
            // see `EntityPipeline::orb_pipeline`.
            //
            // One vertex/index binding for all eleven sprite cells and one
            // instanced draw per cell on screen: `batch.icon` is the part index of
            // the shared orb mesh, so the cell selection is a range within the
            // buffer rather than a rebind.
            //
            // The dropped-item and moving-block draws below are opaque and
            // depth-writing, so an orb in front of an item occludes it correctly
            // while blending against whatever was behind the *item* rather than the
            // item itself. That is a bounded ordering artifact of any
            // alpha-blended draw that also writes depth (vanilla's own translucent
            // entity phase has it too), not a reason to move this after them —
            // moving it there would put orbs over the water instead, which is the
            // more visible half.
            if !orb_batches.is_empty()
                && let (Some(texture), Some(model)) =
                    (&self.entities.orb_texture, &self.entities.orb_gpu_model)
            {
                pass.set_pipeline(&self.entities.orb_pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_bind_group(1, texture, &[]);
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in &orb_batches {
                    let Some(range) = model.parts.get(batch.icon as usize) else {
                        continue;
                    };
                    if range.index_count == 0 {
                        continue;
                    }
                    pass.set_vertex_buffer(1, batch.buffer.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            if let Some(model) = &self.model {
                // Dropped items, through the *model* pipeline rather than the
                // entity one: an item entity is an item model, not a cuboid
                // rig. Same atlas / palette / animation bind groups as terrain,
                // so a dropped block is textured from exactly the pixels the
                // placed block is. Opaque and depth-writing, drawn alongside the
                // mobs and before translucent water for the same reason they
                // are (see the entity note above).
                // Moving block models, immediately before the dropped items and
                // through the same pipeline and the same four bind groups. A block
                // model is not an item model, so this is a separate buffer and a
                // separate draw call (see `prepare_moving_blocks`) — but it is the
                // same *pipeline*: opaque, depth-writing, world-space positions,
                // drawn before translucent water for the reason the mobs are.
                //
                // Positions are baked in world space, so this binds the shared
                // arena's reserved zero slot exactly as the item draw below does.
                if let Some(mesh) = &moving_block_mesh {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    bind_terrain_camera(
                        &mut pass,
                        &model.cam_bind_group,
                        model.origin_arena.zero_offset(),
                        &mut terrain_cam_group_last,
                        &mut stats,
                    );
                    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                }

                if let Some(mesh) = &item_mesh {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    // Dropped-item geometry bakes world positions into its own
                    // vertices (spin/bob included), so it has no origin of its
                    // own: bind the shared arena's reserved zero slot.
                    bind_terrain_camera(
                        &mut pass,
                        &model.cam_bind_group,
                        model.origin_arena.zero_offset(),
                        &mut terrain_cam_group_last,
                        &mut stats,
                    );
                    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;

                    // The enchantment glint, in this **same** pass and right
                    // after the base draw whose depth it matches — the glint
                    // pipeline compares depth `EQUAL`, so a later pass would
                    // find the buffer already advanced and match nothing.
                    if let Some(glint_mesh) = &item_glint_mesh
                        && let Some(glint) = self.glint.as_ref()
                    {
                        pass.set_pipeline(&glint.pipeline.pipeline);
                        pass.set_bind_group(0, &glint.world_uniform_bind_group, &[]);
                        pass.set_bind_group(1, &glint.texture_bind_group, &[]);
                        pass.set_vertex_buffer(0, glint_mesh.vertices.slice(..));
                        pass.set_index_buffer(
                            glint_mesh.indices.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        pass.draw_indexed(0..glint_mesh.index_count, 0, 0..1);
                        stats.draw_calls += 1;
                    }
                }

                // Filled maps hanging in item frames. Same pipeline
                // and same three shared bind groups as the dropped items above,
                // with **group 1 swapped** to the map's own 128×128 texture — the
                // model shader is at the 4-group floor, so a map texture has to
                // replace the atlas rather than join it.
                if let Some((mesh, texture)) = &framed_maps {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    bind_terrain_camera(
                        &mut pass,
                        &model.cam_bind_group,
                        model.origin_arena.zero_offset(),
                        &mut terrain_cam_group_last,
                        &mut stats,
                    );
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.filled_maps_drawn += mesh.index_count as usize / 6;
                }

                // Mining-crack overlays, drawn after the opaque terrain they sit
                // on (so the block face is already in the depth buffer) and
                // before translucent water. One draw call per target — the local
                // player's own dig and any number of other players' (issue
                // That fix) — each independently textured with its own destroy-stage
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

                // Opaque-layer particles — block-break debris above all — here,
                // **before translucent water**, for the same reason the mobs and
                // block entities above are. Break a block underwater and the
                // debris used to be painted over the surface however deep it was.
                //
                // This is vanilla's split, not a blanket move: `Layer::Opaque`
                // goes in the `solid` phase and `Layer::Translucent` in
                // `afterTerrain`, on either side of the translucent terrain draw
                // (`SubmitNodeCollection.submitQuadParticleGroup` submits the
                // group into both, and `QuadParticleFeatureRenderer.prepareGroup`
                // keeps only the layers whose `translucent()` matches). The
                // translucent half stays below, where every particle used to be.
                //
                // Unlike the mobs, this half is still alpha-blended — what makes
                // the water read correctly is its **depth write**, which the
                // opaque pipeline has and the translucent one does not: water
                // tests depth and does not write it, so it can only blend over a
                // submerged particle that is already in the depth buffer, and can
                // only be rejected in front of one that is nearer. See
                // `ParticleRenderer::new`.
                self.particles
                    .draw_opaque(&mut pass, &self.particle_atlas_bind_group);

                // Translucent water, drawn after all opaque model terrain so the
                // sea floor already written to depth shows through the surface
                // (depth test on, depth write off, alpha blend — the fluid
                // pipeline). Same camera + atlas bind groups as the opaque pass.
                pass.set_pipeline(&model.water_pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.water_anim_bind_group, &[]);
                // Same collect-then-emit shape as the opaque pass above, but
                // ordered **back to front by section**, not by arena block.
                //
                // This is a correctness fix, not a perf one (U5). `model.sections`
                // is a `HashMap`, so before this the translucent water of every
                // section was submitted in *hash iteration order*: alpha blending
                // is order-dependent, so two water surfaces overlapping along the
                // view axis composited in whatever order the hasher happened to
                // produce, and that order changes when a section is added or
                // removed rather than when the camera moves. Vanilla sorts its
                // translucent sections by distance every frame
                // (`LevelRenderer`'s `TRANSLUCENT` pass walks the visible list in
                // reverse); this does the same on section centres.
                //
                // It costs the block grouping for this pass — a back-to-front
                // order interleaves arena blocks — which is exactly the trade the
                // culling work bought room for: `water_sections_drawn` is a small
                // fraction of the resident set now, so the extra binds are on a
                // set that culling already shrank. Correctness wins the tie.
                //
                // Vanilla's *intra*-section resort (`TranslucentMesh`/
                // `SortViewpoint`, re-uploading a section's index order on octant
                // change) is deliberately **not** here: water quads within one
                // section are near-coplanar top faces in the overwhelming case, so
                // the cross-section order is the half that produces the visible
                // artefact, and re-uploading an index span that now lives inside a
                // shared arena block is a separate unit.
                let mut water_draws: Vec<TerrainDraw> =
                    Vec::with_capacity(model.sections.len() / 4);
                for (key, section) in &model.sections {
                    let Some(water) = section.water.as_ref() else {
                        continue;
                    };
                    if !terrain_cull.visible(key.coord()) {
                        stats.water_sections_culled += 1;
                        continue;
                    }
                    stats.water_sections_drawn += 1;
                    let mut draw =
                        TerrainDraw::new(water, section.origin_alloc.offset() as u32);
                    draw.sort_dist2 =
                        super::terrain::section_center_distance_sq(key.coord(), camera.position);
                    water_draws.push(draw);
                    stats.total_quads += section.water_quad_count;
                }
                super::terrain::sort_back_to_front(&mut water_draws);
                stats.draw_calls += water_draws.len();
                stats.terrain_buffer_binds += emit_terrain_draws(
                    &mut pass,
                    model,
                    &water_draws,
                    &mut terrain_cam_group_last,
                    &mut stats.terrain_camera_bind_group_switches,
                );

                // Translucent **block** geometry — stained glass, ice, the
                // nether portal swirl — drawn right after water through its
                // own pipeline (`RenderLayer::Translucent`'s `MODEL_WGSL`
                // variant, not the fluid shader: it needs the palette bind
                // group a fluid-tinted quad has none of). Owner report: "the
                // nether portal swirly block is missing is opaque when it
                // isnt supposed to be" — before this pass existed, every
                // block regardless of `RenderLayer` was folded into the one
                // opaque/cutout mesh above, which draws with no blending.
                //
                // Same camera/atlas/palette/anim bind groups as the opaque
                // pass (see `ModelRenderer::translucent_pipeline`'s doc for
                // why that reuse is sound), same back-to-front-by-section
                // order as water. Not interleaved with water's own sort: the
                // two are separate pipelines and separate draw batches, so a
                // translucent block and a water surface that overlap along
                // the view axis in the *same* section pair can still
                // composite in the wrong order relative to each other. Known,
                // narrower limitation than "opaque" — see `translucent`
                // field's doc.
                pass.set_pipeline(&model.translucent_pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.palette_bind_group, &[]);
                pass.set_bind_group(3, &model.anim_bind_group, &[]);
                let mut translucent_draws: Vec<TerrainDraw> =
                    Vec::with_capacity(model.sections.len() / 16);
                for (key, section) in &model.sections {
                    let Some(translucent) = section.translucent.as_ref() else {
                        continue;
                    };
                    if !terrain_cull.visible(key.coord()) {
                        stats.translucent_sections_culled += 1;
                        continue;
                    }
                    stats.translucent_sections_drawn += 1;
                    let mut draw =
                        TerrainDraw::new(translucent, section.origin_alloc.offset() as u32);
                    draw.sort_dist2 =
                        super::terrain::section_center_distance_sq(key.coord(), camera.position);
                    translucent_draws.push(draw);
                    stats.total_quads += section.translucent_quad_count;
                }
                super::terrain::sort_back_to_front(&mut translucent_draws);
                stats.draw_calls += translucent_draws.len();
                stats.terrain_buffer_binds += emit_terrain_draws(
                    &mut pass,
                    model,
                    &translucent_draws,
                    &mut terrain_cam_group_last,
                    &mut stats.terrain_camera_bind_group_switches,
                );
            }

            // The beacon beam's outer **glow** — alpha-blended, depth-test
            // only (`BEACON_BEAM_TRANSLUCENT`, see `gpu/beacon_beam.rs`'s
            // module doc). Placed here, after translucent terrain and
            // outside the `if let Some(model)` gate above (a beacon draws
            // with or without the vanilla-atlas terrain renderer), for the
            // same reason the translucent debris below is: it needs a depth
            // buffer that already holds every opaque surface, including
            // translucent water and blocks, or it would show through them.
            self.beacon_beam.draw_glow(&mut pass, beacon_glow_count);

            // The *translucent* half of the debris last among the world geometry
            // (the opaque half is above, before the water): it is alpha-blended
            // with depth write off, so it must read a depth buffer that already
            // holds every opaque surface, or fragments behind a wall would show
            // through. Vanilla's `afterTerrain` phase, i.e. after translucent
            // terrain. The outline is drawn after it, as vanilla does.
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

            // Right beside the debug lines, for the same reason: a plugin
            // billboard (issue #161) is a world-space overlay a plugin author
            // wants clearly visible, not a piece of real terrain competing
            // for draw order.
            self.plugin_billboards.draw(&mut pass, plugin_billboard_count);

            // Nametags last of all, real depth-tested against
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

        // The screen overlays, each from its own closed fix: their own `Load` passes (see
        // `ScreenEffectRenderer::draw_underwater`'s doc — they must not erase
        // the world/hand just drawn), run last, matching vanilla's own order
        // (`GameRenderer.java`: the hand, then
        // `screenEffectRenderer.submit`/`Hud.extractCameraOverlays`, then the
        // HUD/feature renderers — this shell's HUD draws in a later, separate
        // pass in `app.rs`).
        //
        // Two independent gate groups, not one — see
        // `ScreenEffects::any_active`'s doc for why: underwater/fire/pumpkin/
        // spyglass are first-person-only in vanilla, freeze/confusion/portal
        // are not (`Hud.java` are siblings of the `isFirstPerson`
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
                    // positive — `Hud.java`'s own `if`/`else if`.
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

        // Residency, measured — **not** `vram_bytes(stats.total_quads)`, which is
        // what this was. `total_quads` only accumulates over sections that
        // survived the cull, so that form reported a VRAM figure that moved every
        // time the camera turned on the spot, and it priced live-vanilla quads at
        // the packed path's 72 B instead of a `ModelVertex` quad's 152 B. See
        // `RenderState::resident_mesh_bytes`.
        stats.vram_bytes = self.resident_mesh_bytes();
        stats.vram_reserved_bytes = self.reserved_mesh_bytes();
        stats
    }
}

/// Emit one pass's worth of resolved terrain draws, grouped so that consecutive
/// draws out of the same arena block share a single vertex+index bind. Returns the
/// number of buffer-bind *pairs* issued — the counter that says whether the
/// grouping is actually working.
///
/// `draws` arrives **already ordered**, and the two passes order it differently:
/// opaque sorts by block (grouping is free, since depth sorts the pixels), water
/// sorts back to front (the order is the correctness requirement — see the water
/// pass). Sorting here instead would silently undo the water order, which is why
/// it moved out. [`DEDICATED_BLOCK`] is `u32::MAX`, so under the opaque order the
/// rare section that fell back to its own buffers sorts to the end and never
/// splits an arena run; under the water order it pays its own bind pair wherever
/// it lands, which is exactly the pre-arena cost for that one section.
fn emit_terrain_draws(
    pass: &mut wgpu::RenderPass<'_>,
    model: &super::terrain::ModelRenderer,
    draws: &[TerrainDraw<'_>],
    terrain_cam_group_last: &mut Option<*const wgpu::BindGroup>,
    terrain_camera_bind_group_switches: &mut usize,
) -> usize {
    let mut bound: Option<u32> = None;
    let mut bind_pairs = 0usize;
    for draw in draws.iter() {
        match draw.dedicated {
            Some(mesh) => {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                bound = None;
                bind_pairs += 1;
            }
            None if bound != Some(draw.block) => {
                let (Some(vertices), Some(indices)) = (
                    model.mesh_arena.vertex_buffer(draw.block),
                    model.mesh_arena.index_buffer(draw.block),
                ) else {
                    // Unreachable: a live `ArenaMesh` names a block that exists,
                    // and blocks are never released. Skipping the draw rather than
                    // indexing blind keeps a future change to block lifetime from
                    // turning into a panic in the render loop.
                    continue;
                };
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                bound = Some(draw.block);
                bind_pairs += 1;
            }
            None => {}
        }
        // One shared bind group for every section; only the dynamic offset (this
        // section's slot in the origin arena) changes per draw. Tracked by
        // pointer identity below, not counted as a switch — see
        // `bind_terrain_camera`.
        let ptr = std::ptr::from_ref(&model.cam_bind_group);
        if *terrain_cam_group_last != Some(ptr) {
            *terrain_cam_group_last = Some(ptr);
            *terrain_camera_bind_group_switches += 1;
        }
        pass.set_bind_group(0, &model.cam_bind_group, &[draw.origin_offset]);
        pass.draw_indexed(
            draw.first_index..draw.first_index + draw.index_count,
            draw.base_vertex,
            0..1,
        );
    }
    bind_pairs
}

/// Bind a terrain draw's group 0 (shared camera + per-section origin arena),
/// recording a [`RenderStats::terrain_camera_bind_group_switches`] tick only
/// when the bind-group **object** differs from the previous terrain bind —
/// never for an offset-only change, which is the cheap, expected-every-draw
/// case `set_bind_group`'s dynamic-offset argument exists for. See that
/// field's doc for what a non-flat count would mean.
fn bind_terrain_camera(
    pass: &mut wgpu::RenderPass<'_>,
    group: &wgpu::BindGroup,
    offset: u32,
    last: &mut Option<*const wgpu::BindGroup>,
    stats: &mut RenderStats,
) {
    let ptr = std::ptr::from_ref(group);
    if *last != Some(ptr) {
        *last = Some(ptr);
        stats.terrain_camera_bind_group_switches += 1;
    }
    pass.set_bind_group(0, group, &[offset]);
}
