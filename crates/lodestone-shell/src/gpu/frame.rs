//! Ordered world encoding, shared by standalone renders and the shell frame.
//! Opaque geometry precedes water; the first-person pass clears depth;
//! screen effects follow the world. Submission belongs to the caller.
use lodestone_render::{
    Camera, CameraUniform, CullVerdict, TerrainCull, crack_pipeline::GpuCrackMesh,
    spinning_effect_angle_degrees, update_model_shared_camera_buffer,
};

use crate::entities::EntityDraw;

use super::first_person::FirstPersonHand;
use super::terrain::TerrainDraw;
use super::{CrackTarget, RenderState, RenderStats, ScreenEffects};

impl RenderState {
    /// Harvest ready world readbacks without waiting or submitting more GPU
    /// work. Queries measure world passes, even in a shared encoder; HUD work is
    /// unmeasured. This optional poll gives the shell a fresher end-frame view.
    pub fn gpu_timing_end_frame(&self, device: &wgpu::Device, _queue: &wgpu::Queue) {
        if let Some(timer) = self.gpu_timer.borrow_mut().as_mut() {
            timer.poll(device);
        }
    }

    /// Drain the CPU sub-phase timings and counts the most recent
    /// `render_*` call on this thread recorded — `(name, Some(ms))` per
    /// `gpu::gpu_timing::WorldSubphase`, plus the packed/model section counts
    /// visited.
    ///
    /// **This consumes the readings**, exactly as
    /// `app::frame_profile::FrameProfiler::mark` does; the two must not both
    /// be called for one frame or whichever runs second sees `None` for every
    /// slot and counts a bridge miss. In the shell, `FrameProfiler` is the
    /// only caller. This accessor exists for `benches/frame_profile.rs`, which
    /// drives `RenderState` directly and has no `FrameProfiler` at all.
    #[must_use]
    pub fn take_world_subphase_report(
        &self,
    ) -> (Vec<(&'static str, Option<f32>)>, Option<(usize, usize)>) {
        let (timings, counts) = crate::gpu::gpu_timing::take_world_subphases();
        let named = crate::gpu::gpu_timing::WorldSubphase::ALL
            .into_iter()
            .zip(timings)
            .map(|(sp, ms)| (sp.name(), ms))
            .collect();
        (
            named,
            counts.map(|c| (c.packed_sections_visited, c.model_sections_visited)),
        )
    }

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
        let mut encoder = crate::gpu::gpu_timing::primary_encoder(device, "world");
        let stats = self.encode_with_crack_and_effects(
            device, queue, view, camera, outline, entities, cracks, screen_effects, &mut encoder,
        );
        self.submit_encoded_frame(queue, encoder);
        stats
    }

    /// Finish and submit a composed world frame, then start its query readback.
    pub fn submit_encoded_frame(
        &self,
        queue: &wgpu::Queue,
        encoder: wgpu::CommandEncoder,
    ) -> crate::gpu::gpu_timing::PrimarySubmitCheckpoints {
        let mut checkpoints = crate::gpu::gpu_timing::submit_primary_encoder(queue, encoder);
        if let Some(timer) = self.gpu_timer.borrow_mut().as_mut() {
            timer.after_submit();
        }
        checkpoints.submitted_at = crate::platform::Instant::now();
        checkpoints
    }

    /// Encode the world into a caller-owned frame. Submit before encoding another world.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn encode_with_crack_and_effects(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        cracks: &[CrackTarget],
        screen_effects: ScreenEffects,
        encoder: &mut wgpu::CommandEncoder,
    ) -> RenderStats {
        // Frame-profiling sub-phase timing (`gpu::gpu_timing`): CPU wall time
        // from here to `begin_render_pass` below is every `prepare_*`/uniform
        // write this pass needs before the pass opens (a render pass cannot
        // itself create or write a buffer) — see `WorldSubphase::PrepareBuffers`'s
        // own doc. A single `Instant::now()` local; nothing else here changes.
        let world_encode_t0 = crate::platform::Instant::now();
        if let Some(timer) = self.gpu_timer.borrow_mut().as_mut() {
            timer.begin_frame(device);
        }
        self.instance_arena.begin_frame();

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
        let world_eye = self.eye_bob()
            * lodestone_render::nausea_portal_warp(warp_intensity, warp_angle_degrees);
        let map_eye = world_eye * camera.view_matrix();
        let map_view_projection = camera.projection_matrix() * map_eye;
        let view_proj = map_view_projection.to_cols_array_2d();

        // This frame's fog **and** its `sky_darken` lane, hoisted above both
        // terrain paths so they physically cannot disagree. It used
        // to be computed inside the `if let Some(model)` below, which is why the
        // packed path had no way to reach it.
        let fog = self.fog_with_clock(camera);

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

        if let Some(distant) = &self.distant_terrain {
            distant.prepare(queue, view_proj, fog);
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
        // `prepare` returns early on an empty slice. `viewport_px` sizes the
        // on-screen ribbon width the same way `self.outline.prepare` already
        // does just above — see `DebugLineRenderer`'s module doc for why this
        // pass stopped being a `LineList`.
        let debug_line_count = self.debug_lines.prepare(
            device,
            queue,
            &view_proj,
            (self.depth.width, self.depth.height),
            super::debug_lines::MIN_LINE_WIDTH_PX,
            &self.debug_lines_source.sample(camera.position),
        );

        // The effect source supplies ids only; geometry is rebuilt from this
        // frame's entity draws so interpolation, culling and entity removal
        // cannot leave an outline at an old location. This is a separate
        // renderer from F3 lines because the gameplay effect is unconditional.
        let glow_ids = self.entity_glow_source.sample();
        let glow_outline_vertices =
            super::debug_lines::glowing_entity_outline_vertices(entities, &glow_ids);
        let glow_outline_count = self.glow_outline.prepare(
            device,
            queue,
            &view_proj,
            (self.depth.width, self.depth.height),
            super::debug_lines::VANILLA_LINE_WIDTH_PX,
            &glow_outline_vertices,
        );

        // Same constraint for the plugin-billboard pass: sample
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

        // This frame's lightmap inputs, polled once for all three flat-colour
        // world-text passes below (nametags, sign text, `text_display`) — the
        // same two sources the terrain and entity passes already read, and the
        // same `EntityLightSource` alongside them. Before this the three
        // sampled no lightmap at all and every glyph in the world drew
        // full-bright. See `gpu/nametag.rs`'s `WorldTextLight` and
        // `docs/world-text-lighting.md`.
        let world_text_light =
            super::nametag::WorldTextLight::new(self.sky_darken.value(), self.effective_ambient_light());

        // Nametag vertices, same "upload before the pass opens"
        // constraint as outline/debug-lines above. Reads the same
        // (possibly body-extended) `entities` slice; the local third-person
        // body's own draw always carries `name_tag: None`
        // (`ThirdPersonBodyState::into_draw`), so this is a no-op for it.
        let name_tag_counts = self.nametag.prepare(
            queue,
            &view_proj,
            camera,
            entities,
            world_text_light,
            &self.entity_light,
        );

        // Sign text, same "upload before the pass opens"
        // constraint. Not derived from `entities` — a sign is a *block*,
        // gathered from the world's block-entity records exactly like
        // `block_entity_source`/`skull_source` below, just with no cull or
        // batch step of its own (see `gpu/sign_text.rs`'s module doc for why
        // this is not a billboard and needs no camera basis).
        let signs = self.sign_source.signs(camera.position);
        let sign_prepare =
            self.sign_text.prepare(queue, &view_proj, camera.position, &signs, world_text_light);
        let sign_text_count = sign_prepare.vertices;
        stats.sign_text_vertices = sign_text_count;

        // `text_display` glyphs and background panels, same "upload before
        // the pass opens" constraint. Unlike sign text, this *does* need a
        // camera basis — a `text_display`'s billboard mode can track the
        // camera, unlike a sign's fixed orientation — so `camera` (not just
        // `camera.position`) is threaded through. See `gpu/display_text.rs`'s
        // module doc.
        // `(background_count, glyph_count, see_through_count)`: the panels,
        // the ink and everything a `FLAG_SEE_THROUGH` display contributes go
        // through three different pipelines, matching vanilla's own
        // `TEXT_BACKGROUND`/`TEXT_POLYGON_OFFSET`/`TEXT_*_SEE_THROUGH` split.
        let display_text_counts = self.display_text.prepare(
            device,
            queue,
            &view_proj,
            &self.display_draws,
            camera,
            world_text_light,
            &self.entity_light,
        );

        // Beacon beams, same "upload before the pass opens" constraint and
        // the same not-derived-from-`entities` shape as sign text above — a
        // beacon is a *block*, gathered from world state. See
        // `gpu/beacon_beam.rs`'s module doc for why this returns two counts
        // (solid core / outer glow) rather than one.
        let beacons = self.beacon_source.beacons(camera.position);
        let (beacon_solid_count, beacon_glow_count) =
            self.beacon_beam.prepare(queue, &view_proj, &beacons);

        // Lightning bolts, rebuilt from scratch each frame exactly as vanilla's
        // own `submit` does. Before the pass opens, like every sibling here.
        let lightning_bolt_count = self.lightning_bolt.prepare(queue, &view_proj, entities);
        stats.lightning_bolt_vertices = lightning_bolt_count as usize;
        stats.beacon_beam_solid_vertices = beacon_solid_count;
        stats.beacon_beam_glow_vertices = beacon_glow_count;

        // End gateway teleport beams — same pass, a second texture. See
        // `gpu/beacon_beam.rs::BeaconBeamRenderer::prepare_gateway`'s doc
        // for why this does not rewrite `cam_uniform` itself (the call just
        // above already did, unconditionally, this frame).
        let end_gateway_beams = self.end_gateway_beam_source.beams(camera.position);
        let (end_gateway_beam_solid_count, end_gateway_beam_glow_count) =
            self.beacon_beam.prepare_gateway(queue, &end_gateway_beams);

        // End portals / end gateways, same "upload before the pass opens"
        // constraint and the same not-derived-from-`entities` shape as the
        // beacon beam above — both are *blocks*, gathered from world state.
        // `game_time` follows the same ticks-plus-partial-tick convention
        // `beacon_source`'s own `animation_time` uses; see
        // `gpu/end_portal.wgsl`'s doc for why the exact vanilla `GameTime`
        // scale is not re-derived here.
        let end_portals = self.end_portal_source.portals(camera.position);
        let end_gateways = self.end_gateway_source.gateways(camera.position);
        let end_portal_count = self.end_portal.prepare(
            queue,
            &view_proj,
            self.end_portal_game_time,
            &end_portals,
            &end_gateways,
        );
        stats.end_portal_vertices = end_portal_count;

        // Resolve, frustum-cull and upload entity instances *before* the pass —
        // buffers can't be created mid-pass, and the entity camera uniform (no
        // section origin; the world position lives in each instance matrix) must
        // be written first too.
        let mut entity_batches =
            self.prepare_entities(device, queue, camera, entities, &mut stats);

        // The mob spawner's/trial spawner's spinning display mob — not a
        // `BlockEntitySource` consumer, but the ordinary mob pipeline at a
        // nested placement (see `gpu/spawner_mobs.rs`). Appended into the
        // same **visible** batch vec `prepare_entities` returns, so the draw
        // loop below needs no changes: a spawner's mob is, from that loop's
        // point of view, just another `EntityDrawBatch`. It never joins the
        // separate invisible water-mask phase.
        //
        // `write_entity_camera_uniform` runs unconditionally first, for the
        // reason its own doc gives: `prepare_entities` only rewrites group 0
        // when `entities` is non-empty, and a spawner can be the only thing
        // in view.
        self.write_entity_camera_uniform(queue, camera);
        let spawner_spawns = self.spawner_source.spawner_mobs(camera.position);
        entity_batches
            .visible
            .extend(self.prepare_spawner_mobs(device, queue, camera, &spawner_spawns));

        // Humanoid armour layers, over the same instances — resolved from the
        // same `entities` slice and the same resolver, so a helmet cannot be
        // posed off a head the body pass did not draw. Uploaded here for the
        // usual reason: no buffer creation mid-pass.
        let armour_batches = self.prepare_armour(device, queue, camera, entities, &mut stats);

        // The sheep wool layer, over the same instances, for the
        // same reason armour is: no buffer creation mid-pass, and never posed
        // off a pose the body pass did not also draw.
        let wool_batches = self.prepare_wool(device, queue, camera, entities, &mut stats);

        // Player capes, over the same instances, for the same reason
        // armour/wool are: no buffer creation mid-pass, and never posed off a
        // pose the body pass did not also draw. Grouped by cape URL rather
        // than by wearer part — see `RenderState::prepare_cape`'s doc.
        let cape_batches = self.prepare_cape(device, queue, camera, entities, &mut stats);

        // The elytra wings — the layer the cape pass suppresses itself for,
        // over the same instances and for the same reason as everything
        // above: no buffer creation mid-pass. See
        // `RenderState::prepare_elytra`'s doc, including how its target is
        // selected from fall-flying, crouching, and movement state.
        let elytra_batches = self.prepare_elytra(device, queue, camera, entities, &mut stats);

        // Paintings, over the same entity slice and for the same reason as
        // everything above: no buffer creation mid-pass. Not part of the mob
        // batch — a painting has no rig at all, so `prepare_entities` never
        // sees one. See `RenderState::prepare_paintings`.
        let painting_batches = self.prepare_paintings(device, queue, camera, entities, &mut stats);

        // The mob-fire billboard, over the same instances, for
        // the same reason armour/wool are: no buffer creation mid-pass.
        let flame_batches = self.prepare_flame(device, camera, entities, &mut stats);

        // The entity ground-shadow decal (owner report: "entity shadows are
        // missing"), over the same instances, for the same reason as
        // everything above: no buffer creation mid-pass.
        let shadow_batch = self.prepare_shadows(device, camera, entities, &mut stats);

        // Experience-orb billboards, over the same `entities` slice and for the
        // same reason as everything above: no buffer creation mid-pass. An orb has
        // no cuboid rig, so `prepare_entities` above skips it entirely — this is
        // the only thing that puts an orb on screen.
        let orb_batches = self.prepare_orbs(device, queue, camera, entities, &mut stats);

        // The camera-facing entity sprites — a dragon fireball and a fishing
        // bobber. Same reasoning as the orbs immediately above: neither has a
        // cuboid rig, so `prepare_entities` skips both and this is the only
        // thing that puts either on screen.
        let sprite_batches =
            self.prepare_entity_sprites(device, queue, camera, entities, &mut stats);
        // The fishing line back to whoever cast it. Uploaded here, before the
        // pass opens, for the reason everything in this block is: buffers
        // cannot be written mid-pass. Reads the same (possibly body-extended)
        // `entities` slice, which is load-bearing rather than incidental — the
        // synthetic third-person body's presence in it is exactly how
        // `fishing_line_vertices` tells a detached camera from a first-person
        // one.
        let fishing_line_vertices = self.fishing_line_vertices(camera, entities);
        stats.fishing_line_segments = fishing_line_vertices.len() / 2;
        let fishing_line_count = self.fishing_line.prepare(
            device,
            queue,
            &view_proj,
            (self.depth.width, self.depth.height),
            super::debug_lines::VANILLA_LINE_WIDTH_PX,
            &fishing_line_vertices,
        );

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

        // Every world-space `EntityInstanceRaw` producer above appended bytes
        // to one CPU arena. Upload them together now, after the final producer
        // and before any draw can consume one of the recorded ranges.
        let instance_buffer = self.instance_arena.upload(device, queue);

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
        let moving_block_meshes = self.prepare_moving_blocks(device, camera, entities, &mut stats);
        // Maps in item frames. Built here rather than inside the pass
        // for the reason above — it creates a texture and a bind group — and kept
        // separate from `item_mesh` because it draws with a different group 1.
        let framed_maps = self.prepare_framed_maps(
            device,
            queue,
            camera,
            entities,
            map_view_projection,
            map_view_projection,
        );
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
        let first_person_hand = if stats.third_person_body_drawn || screen_effects.spectator {
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
        // and its `SkyRenderer` passes never
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
            // banded in a colour the sky never is. Void fog now comes from the
            // connected level (`RenderState::void_fog`, pushed per frame from
            // `Sim::void_fog`) rather than the `VoidFog::OVERWORLD` constant
            // this line used to name: that constant is only right for a
            // non-flat overworld. See `docs/sky-and-air-bubbles.md`.
            let clock = self.time_of_day.sample();
            let frame = lodestone_render::SkyFrame::new(clock.time_of_day, day_sky_color)
            .with_cloud_time(clock.game_time)
            .with_fog_color(self.fog.color)
            .with_atmosphere(self.atmosphere(camera))
            .with_render_distance(self.render_distance_chunks)
            .with_void_fog(self.void_fog)
            // Vanilla's Clouds option. This builder had **zero** production
            // callers, so the pass always drew `CloudStatus::default()` (FANCY):
            // the FAST quad path and the OFF case both existed in
            // `SkyRenderer::render` and no player could select either.
            .with_cloud_status(self.cloud_status)
            .with_cloud_color(self.cloud_color)
            // The connected dimension's own `Skybox`. `SkyMode::None` (the Nether)
            // makes `SkyRenderer::render` clear and return, so `stats.sky_drawn`
            // below stays `true` — the target *was* written, which is exactly what
            // the block pass's `Load`-vs-`Clear` choice depends on. Reporting
            // `false` here instead would double-clear and discard the Nether's red
            // horizon.
            .with_sky_mode(self.sky_mode);
            let clear = frame.clear_color_wgpu(camera.position.y);
            sky.render(device, queue, encoder, view, camera, &frame, clear);
            true
        } else {
            false
        };

        // See `world_encode_t0` above: everything before this point is
        // buffer prep (uniform writes, `prepare_*` calls, the sky pass);
        // everything from here on is the one "block pass" — see
        // `gpu::gpu_timing::WorldSubphase`'s own doc.
        crate::gpu::gpu_timing::record_world_subphase(
            crate::gpu::gpu_timing::WorldSubphase::PrepareBuffers,
            world_encode_t0.elapsed().as_secs_f32() * 1000.0,
        );
        let world_encode_terrain_t0 = crate::platform::Instant::now();
        // Set from *inside* the pass block below (after opaque terrain, once
        // per call — see that checkpoint) and read again after the block
        // closes (the `pass` drop point) — a `Cell` rather than a plain
        // `let` because the assignment site is nested one scope deeper than
        // this declaration and the read site, and a `let` there would not
        // outlive the block. The placeholder value is always overwritten
        // before it is ever read.
        let world_encode_other_t0 = std::cell::Cell::new(world_encode_terrain_t0);

        // This frame's raw (non-sRGB) view of the same colour texture `view`
        // names, for the three flat-colour world-text passes — see
        // `RenderState::set_world_text_view` for why vanilla's text and its
        // background plate must composite on gamma bytes rather than through
        // the sRGB view every other pipeline here targets.
        //
        // **Taken**, not borrowed: a swapchain image is presented at the end of
        // the frame, so a view of it must not survive into the next one.
        let world_text_view = self.world_text_view.borrow_mut().take();
        let world_text_target: Option<&wgpu::TextureView> = match world_text_view.as_ref() {
            Some(v) => Some(v),
            // No raw view supplied this frame. Falling back to `view` is only
            // legal while the three text renderers are still built for the
            // target's own format — true for every caller that has never
            // called `set_world_text_view`, and for a target that is already
            // non-sRGB, where the two views have the same format anyway. Once
            // they have been re-pointed, attaching `view` would put a pipeline
            // and its attachment at different formats, which `wgpu` rejects at
            // `set_pipeline` and takes the whole frame down for; drop the text
            // for this frame instead and say so.
            None if self.world_text_format == self.color_format => Some(view),
            None => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    tracing::error!(
                        target: "render",
                        "world text skipped: set_world_text_view was not called this frame, \
                         and the text pipelines target {:?} while the frame's view is {:?}",
                        self.world_text_format,
                        self.color_format,
                    );
                });
                None
            }
        };
        // Whether either world-text pass has anything to draw. An empty render
        // pass is not free — it still stores and reloads a colour and a depth
        // attachment — so a frame with no signs, holograms or nametags opens
        // neither; the world pass can then stay open through its tail.
        let draw_world_text =
            world_text_target.is_some() && (sign_text_count > 0 || !display_text_counts.is_empty());
        let draw_nametags = world_text_target.is_some() && name_tag_counts != (0, 0);

        // Tracks the terrain path's own group-0 bind-group object (packed or
        // model camera) across every draw loop below, by pointer identity —
        // see `RenderStats::terrain_camera_bind_group_switches`. Intentionally
        // *not* reset between the packed and model loops: entering the model
        // loop after the packed one is one real switch, which is exactly what
        // the counter should show. It is declared out here rather than inside
        // the opaque body because world text can require a second pass for
        // the translucent loops, and it *is* reset at that boundary — a new
        // render pass inherits no bindings, so the first bind in it is a real
        // switch and the counter would otherwise miss one.
        let mut terrain_cam_group_last: Option<*const wgpu::BindGroup> = None;

        let visible_model_sections = self.model.as_ref().map_or_else(Vec::new, |model| {
            collect_visible_model_sections(model, camera, &terrain_cull, &mut stats)
        });
        let mut terrain_draws = Vec::with_capacity(visible_model_sections.len());
        let mut world_pass_begins = 0;
        let mut world_text_pass_begins = 0;
        let mut nametag_pass_begins = 0;

        let timer = self.gpu_timer.borrow();
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
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            // Only the final world pass owns the end edge. A full timer ring
            // supplies no timestamp descriptors for this frame.
            timestamp_writes: timer.as_ref().and_then(|t| {
                if !draw_world_text && !draw_nametags {
                    t.writes("world")
                } else {
                    t.writes_begin("world")
                }
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        world_pass_begins += 1;
        {
            if let Some(distant) = &self.distant_terrain {
                distant.draw(&mut pass);
            }
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
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
                let pipeline = model.terrain_pipeline.as_ref().unwrap_or(&model.pipeline);
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.palette_bind_group, &[]);
                pass.set_bind_group(3, &model.anim_bind_group, &[]);
                for (section, _) in &visible_model_sections {
                    let Some(mesh) = section.mesh.as_ref() else {
                        continue;
                    };
                    terrain_draws.push(TerrainDraw::new(
                        mesh,
                        section.origin_alloc.offset() as u32,
                    ));
                    stats.sections_drawn += 1;
                    stats.total_quads += section.quad_count;
                }
                terrain_draws.sort_unstable_by_key(|d| d.block);
                stats.draw_calls += terrain_draws.len();
                emit_terrain_draws(
                    &mut pass,
                    model,
                    &terrain_draws,
                    &mut terrain_cam_group_last,
                    &mut stats,
                );
            }

            // Frame-profiling sub-phase timing (`gpu::gpu_timing`): opaque
            // terrain's own cull-and-draw cost, both loops above, separated
            // from everything else this pass still has to draw below — see
            // `WorldSubphase::TerrainCullAndDraw`'s doc. Sections *visited*
            // (not just drawn) alongside it: the packed loop has no cull
            // counters of its own, so `self.sections.len()` is its whole
            // "visited" figure; compare the model side against
            // `RenderStats::sections_drawn` plus its three `sections_culled_*`
            // fields, already on the F3 overlay.
            crate::gpu::gpu_timing::record_world_subphase(
                crate::gpu::gpu_timing::WorldSubphase::TerrainCullAndDraw,
                world_encode_terrain_t0.elapsed().as_secs_f32() * 1000.0,
            );
            world_encode_other_t0.set(crate::platform::Instant::now());

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
            if !entity_batches.visible.is_empty() {
                for batch in &entity_batches.visible {
                    // `PlayerModel` is constructed with
                    // `RenderTypes::entityTranslucent` in 26.2.  Its skin's
                    // partially-alpha outer layer therefore blends with the
                    // `0.1` cutout threshold, while ordinary mob sheets stay
                    // on the opaque/cutout pipeline.  Both pipelines share
                    // these exact bind-group layouts and the same
                    // `LessEqual`/depth-write state.
                    if matches!(batch.model, "player_wide" | "player_slim") {
                        pass.set_pipeline(&self.entities.player_skin_pipeline);
                    } else {
                        pass.set_pipeline(&self.entities.pipeline.pipeline);
                    }
                    pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                    let Some(model) = self.entities.gpu_models.get(batch.model) else {
                        continue;
                    };
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
                    for (range, instances) in model.parts.iter().zip(&batch.parts) {
                        let (Some(instances), true) = (instances.as_ref(), range.index_count > 0)
                        else {
                            continue;
                        };
                        let Some(instance_buffer) = instance_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, instance_buffer.slice(instances.clone()));
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
                    for (range, instances, count) in &batch.parts {
                        let Some(instance_buffer) = instance_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, instance_buffer.slice(instances.clone()));
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
                    for (range, instances, count) in &wool_batches {
                        let Some(instance_buffer) = instance_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, instance_buffer.slice(instances.clone()));
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
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(batch.instances.clone()));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            // The elytra wings, right after the cape — the layer that
            // replaces it for a wearer with an elytra in the chest slot.
            // Through the **base** entity pipeline, same reason wool and the
            // cape are: no second layer at the same inflation to correct
            // z-fighting for. Group 1 is rebound per batch, off the jar sheet
            // for a wearer with no cape and off `player_skins` for one with.
            if !elytra_batches.is_empty()
                && let Some(model) = &self.entities.elytra_gpu
            {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in &elytra_batches {
                    let texture = match &batch.texture {
                        Some(url) => self.entities.player_skins.get(url),
                        None => self.entities.elytra_texture.as_ref(),
                    };
                    let Some(texture) = texture else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(batch.instances.clone()));
                    let end = batch.range.index_start + batch.range.index_count;
                    pass.draw_indexed(batch.range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            // Paintings, right after the elytra. Through the **base** entity
            // pipeline: a painting is ordinary opaque cutout geometry with no
            // second coplanar layer, exactly like wool and the cape. Group 1 is
            // rebound per batch — the variant's own sprite for a front batch,
            // the shared back tile for a frame batch.
            if !painting_batches.is_empty() {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                for batch in &painting_batches {
                    let Some((_, model)) = self.entities.painting_models.get(batch.model) else {
                        continue;
                    };
                    let Some(range) = model.parts.get(batch.part) else {
                        continue;
                    };
                    let texture = match batch.variant {
                        Some(variant) => self.entities.painting_textures.get(variant),
                        None => self.entities.painting_back_texture.as_ref(),
                    };
                    let Some(texture) = texture else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(batch.instances.clone()));
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

            // The entity ground-shadow decal (owner report: "entity shadows
            // are missing") — right after the mob-fire billboard and before
            // block entities, translucent with depth write off: it is a flat
            // decal painted onto ground that already wrote its own depth
            // (terrain draws before any entity pass), so it must not write
            // depth itself or it would fight whatever draws over it next at
            // the same depth. See `EntityPipeline::shadow_pipeline`'s doc for
            // the vanilla `RenderPipelines.ENTITY_SHADOW` state this mirrors.
            if let Some(batch) = &shadow_batch
                && let Some(texture) = &self.entities.shadow_texture
            {
                pass.set_pipeline(&self.entities.shadow_pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_bind_group(1, texture, &[]);
                pass.set_vertex_buffer(0, batch.buffer.slice(..));
                pass.draw(0..batch.count, 0..1);
                stats.draw_calls += 1;
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
                    // Static sheets stay in this pass's texture set; placed
                    // player heads reuse the entity pass's remote-skin cache.
                    // A dynamic cache miss is normal while its fetch or upload
                    // is pending (or failed), and deliberately falls back to
                    // the existing Steve bind group for that one frame.
                    let texture = match &batch.texture {
                        lodestone_render::BlockEntityTexture::Static(stem) => {
                            self.block_entities.textures.get(stem)
                        }
                        lodestone_render::BlockEntityTexture::PlayerSkin(url) => self
                            .entities
                            .player_skins
                            .get(url.as_ref())
                            .or_else(|| self.block_entities.textures.get("entity/player/wide/steve")),
                    };
                    let Some(texture) = texture else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, instances) in model.parts.iter().zip(&batch.parts) {
                        let (Some(instances), true) = (instances.as_ref(), range.index_count > 0)
                        else {
                            continue;
                        };
                        let Some(instance_buffer) = instance_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, instance_buffer.slice(instances.clone()));
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
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(layer.instances.clone()));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }

        }

        // Sign text and `text_display`, in their own pass on the **raw**
        // (non-sRGB) view of this same colour texture, because vanilla
        // composites text and its background panel on gamma bytes — see
        // `RenderState::set_world_text_view`. A `wgpu` render pass fixes one
        // attachment format for every pipeline in it, so a separate pass is
        // the only way to have these two blend differently from the terrain
        // and entities around them.
        //
        // **The pass boundary is here, and not later, on purpose.** Sign text
        // goes right after the block entities and before translucent water — a
        // sign's board is real terrain (unlike a chest, it has a genuine block
        // model), so by this point it is already in the depth buffer for the
        // text's own polygon-offset bias to win against; and `text_display`
        // goes immediately after sign text for the same "already in the depth
        // buffer, opaque/cutout geometry" reasoning. Moving either past the
        // translucent geometry, the particles or the weather below would put a
        // raindrop in front of a sign *behind* it. See `gpu/sign_text.rs`'s and
        // `gpu/display_text.rs`'s module docs for the depth pipelines —
        // `display_text` draws four ranges through four pipelines: unbiased
        // panel, polygon-offset shadow, ink at twice that offset in both terms
        // so a near-grazing plane's own depth gradient cannot let the shadow
        // win, and the see-through pair that neither tests nor writes depth,
        // which is why it is drawn last.
        if draw_world_text
            && let Some(text_view) = world_text_target
        {
            drop(pass);
            let mut text_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world text pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: text_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Always `Load`: the block pass above has just drawn
                        // the terrain and entities this text composites over.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    // The same depth buffer, loaded rather than cleared —
                    // sign text tests *and* writes depth, and the block pass
                    // resuming below depends on both.
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            world_text_pass_begins += 1;
            self.sign_text.draw(&mut text_pass, sign_text_count);
            self.display_text.draw(&mut text_pass, &display_text_counts);
            drop(text_pass);

            // Exactly one real pass records the world end edge. An incomplete
            // edge mask makes the frame invalid, even if old ticks are positive.
            let world_span_end = if draw_nametags {
                None
            } else {
                timer.as_ref().and_then(|t| t.writes_end("world"))
            };
            pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("block pass (translucent and overlays)"),
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
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: world_span_end,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            world_pass_begins += 1;
            // A fresh pass inherits no bind groups, so the next terrain bind is
            // a genuine switch — see the declaration above.
            terrain_cam_group_last = None;
        }

        {
            // The beacon beam's **solid core** only — opaque, depth-writing
            // (`BEACON_BEAM_OPAQUE`, see `gpu/beacon_beam.rs`'s module doc),
            // so it belongs here with the rest of this pass's opaque/cutout
            // geometry and **before translucent water** for the same reason
            // block entities are: it writes depth, so drawing it after water
            // would paint a beam segment submerged in a pool over the water
            // surface. The outer **glow** is translucent and drawn far below,
            // among the other alpha-blended world geometry.
            self.beacon_beam.draw_solid(&mut pass, beacon_solid_count);
            // The end gateway teleport beam's own solid core — same
            // pipeline, own texture and buffer.
            self.beacon_beam
                .draw_gateway_solid(&mut pass, end_gateway_beam_solid_count);

            // The end portal / end gateway star-field surface — fully
            // opaque (`DepthStencilState.DEFAULT`, no blend), so it belongs
            // here too, before translucent water, for the same
            // depth-writing reason as the beacon beam's solid core above.
            self.end_portal.draw(&mut pass, end_portal_count);

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
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(batch.instances.clone()));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            // The camera-facing entity sprites, immediately after the orbs and
            // before translucent water for the reason they are: both write
            // depth, so drawing either after the water surface would paint a
            // submerged bobber over it.
            //
            // The **base** entity pipeline, not the orb's: vanilla's
            // `entityCutout`/`entityCutoutCull` is `DepthStencilState.DEFAULT`
            // plus a `0.5` alpha cutout, which is exactly what `fs_main` already
            // is — see `EntityRenderer::sprite_gpu_model`. One vertex/index
            // binding for both sprites and one instanced draw per sprite on
            // screen, but group 1 is rebound per batch because the two sprites
            // are two separate standalone sheets rather than two cells of one.
            if !sprite_batches.is_empty()
                && let Some(model) = &self.entities.sprite_gpu_model
            {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in &sprite_batches {
                    let (Some(range), Some(Some(texture))) = (
                        model.parts.get(batch.sprite),
                        self.entities.sprite_textures.get(batch.sprite),
                    ) else {
                        continue;
                    };
                    if range.index_count == 0 {
                        continue;
                    }
                    pass.set_bind_group(1, texture, &[]);
                    let Some(instance_buffer) = instance_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, instance_buffer.slice(batch.instances.clone()));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            // The fishing line, immediately after the bobber it hangs off, so
            // the two reach the frame together and a missing one is
            // unambiguous. Depth-tested against terrain (a line behind a wall
            // does not bleed through) but not depth-writing, which is this
            // pass's own state rather than vanilla's own generic-line render type —
            // at 2.5 logical pixels the difference is whether two overlapping
            // lines z-fight, and they do not.
            self.fishing_line.draw(&mut pass, fishing_line_count);

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

                // Generic moving blocks retain the ordinary model camera and
                // depth state. Item-frame bodies are emitted separately below:
                // vanilla gives only that draw `entitySolidZOffsetForward`.
                if let Some(mesh) = moving_block_meshes
                    .as_ref()
                    .and_then(|meshes| meshes.ordinary.as_ref())
                {
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

                if let Some(mesh) = moving_block_meshes
                    .as_ref()
                    .and_then(|meshes| meshes.item_frames.as_ref())
                {
                    pass.set_pipeline(&model.surface_pipeline.pipeline);
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

                let framed_map_instances_submitted = framed_maps
                    .iter()
                    .map(|(mesh, _)| mesh.index_count as usize / 6)
                    .sum();
                let framed_map_instances_before = stats.filled_maps_drawn;
                let map_diagnostic_switches = super::maps::map_diagnostic_switches();
                let map_pipeline = match (
                    map_diagnostic_switches.disable_backface_cull,
                    map_diagnostic_switches.disable_depth(),
                ) {
                    (false, false) => &model.map_surface_pipeline,
                    (true, false) => &model.map_surface_no_cull_pipeline,
                    (false, true) => &model.map_surface_no_depth_pipeline,
                    (true, true) => &model.map_surface_no_cull_no_depth_pipeline,
                };
                for (mesh, texture) in &framed_maps {
                    // Keep vanilla's physical `1.01 / 128` separation and add
                    // the equivalent finite-precision ordering for Lodestone's
                    // forward-depth wgpu projection. Without this dedicated
                    // second raster step a visible frame flickers against its
                    // front plate, while an invisible frame can lose the whole
                    // parallel quad against its attachment wall.
                    pass.set_pipeline(&map_pipeline.pipeline);
                    bind_terrain_camera(
                        &mut pass,
                        &model.cam_bind_group,
                        model.origin_arena.zero_offset(),
                        &mut terrain_cam_group_last,
                        &mut stats,
                    );
                    pass.set_bind_group(1, &**texture, &[]);
                    pass.set_bind_group(2, &model.palette_bind_group, &[]);
                    pass.set_bind_group(3, &model.anim_bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.filled_maps_drawn += mesh.index_count as usize / 6;
                }
                super::maps::note_framed_map_draw(
                    camera,
                    framed_map_instances_submitted,
                    framed_maps.len(),
                    stats.filled_maps_drawn - framed_map_instances_before,
                );

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

            }

            // Opaque-layer particles — block-break debris above all — here,
            // **before translucent water**, for the same reason the mobs and
            // block entities above are. Break a block underwater and the
            // debris used to be painted over the surface however deep it was.
            //
            // **Outside the `if let Some(model)` gate**, and that placement is
            // the whole of a bug this pass shipped for two weeks: the gate is
            // present only because the water and translucent-terrain draws
            // below need the model renderer, and folding this call into it made
            // every `Layer::Opaque` particle — all block-break debris, plus
            // flame, crit, lava, dust and the drips — conditional on a renderer
            // they do not use. On the packed (demo / no-vanilla-jar) path
            // `self.model` is `None`, so the debris was uploaded, counted and
            // never submitted: `particles_drawn` reported 64 over a frame with
            // nothing in it. The gate is therefore split rather than widened —
            // the ordering against the items, maps and cracks above and the
            // water below is unchanged.
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
            let opaque_submitted = self
                .particles
                .draw_opaque(&mut pass, &self.particle_atlas_bind_group);

            // Boat-interior water masks, after every visible opaque/cutout
            // draw and immediately before translucent water. They write depth
            // but no colour. This phase boundary is what prevents the mask
            // from erasing a rider's legs/arms where they intersect the hull:
            // every visible fragment is already in colour and depth before a
            // mask can run, while the water surface below still fails against
            // the mask as intended.
            if !entity_batches.water_masks.is_empty()
                && let Some(texture) = self.entities.textures.get("boat_water_patch")
            {
                pass.set_pipeline(&self.entities.water_mask_pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                pass.set_bind_group(1, texture, &[]);
                for batch in &entity_batches.water_masks {
                    let Some(model) = self.entities.gpu_models.get(batch.model) else {
                        continue;
                    };
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (range, instances) in model.parts.iter().zip(&batch.parts) {
                        let (Some(instances), true) = (instances.as_ref(), range.index_count > 0)
                        else {
                            continue;
                        };
                        let Some(instance_buffer) = instance_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, instance_buffer.slice(instances.clone()));
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                        stats.draw_calls += 1;
                    }
                }
            }

            if let Some(model) = &self.model {
                // Translucent water, drawn after all opaque model terrain so the
                // sea floor already written to depth shows through the surface
                // (depth test on, depth write off, alpha blend — the fluid
                // pipeline). Same camera + atlas bind groups as the opaque pass.
                pass.set_pipeline(&model.water_pipeline.pipeline);
                pass.set_bind_group(1, &model.atlas_bind_group, &[]);
                pass.set_bind_group(2, &model.water_anim_bind_group, &[]);
                terrain_draws.clear();
                for (section, distance) in &visible_model_sections {
                    let Some(water) = section.water.as_ref() else {
                        continue;
                    };
                    stats.water_sections_drawn += 1;
                    let mut draw =
                        TerrainDraw::new(water, section.origin_alloc.offset() as u32);
                    draw.sort_dist2 = *distance;
                    terrain_draws.push(draw);
                    stats.total_quads += section.water_quad_count;
                }
                super::terrain::sort_back_to_front(&mut terrain_draws);
                stats.draw_calls += terrain_draws.len();
                emit_terrain_draws(
                    &mut pass,
                    model,
                    &terrain_draws,
                    &mut terrain_cam_group_last,
                    &mut stats,
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
                terrain_draws.clear();
                for (section, distance) in &visible_model_sections {
                    let Some(translucent) = section.translucent.as_ref() else {
                        continue;
                    };
                    stats.translucent_sections_drawn += 1;
                    let mut draw =
                        TerrainDraw::new(translucent, section.origin_alloc.offset() as u32);
                    draw.sort_dist2 = *distance;
                    terrain_draws.push(draw);
                    stats.total_quads += section.translucent_quad_count;
                }
                super::terrain::sort_back_to_front(&mut terrain_draws);
                stats.draw_calls += terrain_draws.len();
                emit_terrain_draws(
                    &mut pass,
                    model,
                    &terrain_draws,
                    &mut terrain_cam_group_last,
                    &mut stats,
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
            // Lightning bolts, with the translucent geometry and for a
            // sharper version of the same reason: the pass is **additive**, so
            // whatever it should brighten has to already be in the framebuffer,
            // and anything drawn opaquely over it afterward would erase it. See
            // `gpu/lightning_bolt.rs`'s module doc.
            self.lightning_bolt.draw(&mut pass, lightning_bolt_count);
            // The end gateway teleport beam's own glow — same placement
            // reasoning as the beacon's, own texture and buffer.
            self.beacon_beam
                .draw_gateway_glow(&mut pass, end_gateway_beam_glow_count);

            // The *translucent* half of the debris last among the world geometry
            // (the opaque half is above, before the water): it is alpha-blended
            // with depth write off, so it must read a depth buffer that already
            // holds every opaque surface, or fragments behind a wall would show
            // through. Vanilla's `afterTerrain` phase, i.e. after translucent
            // terrain. The outline is drawn after it, as vanilla does.
            // Summed from what the two draws **submitted**, not from
            // `ParticleRenderer::count`, so a half that never reaches its pass
            // reads as zero here instead of as a full, healthy frame — see
            // `ParticleRenderer::draw_opaque`.
            stats.particles_drawn = opaque_submitted
                + self
                    .particles
                    .draw(&mut pass, &self.particle_atlas_bind_group);
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

            // Entity-effect outlines share the world depth buffer, so an
            // entity behind a wall remains occluded while the outline still
            // reads over the entity and weather already drawn in this pass.
            self.glow_outline.draw(&mut pass, glow_outline_count);

            if outline.is_some() {
                self.outline.draw(&mut pass);
            }

            // After the outline, for the same reason it is after debris: it
            // is a diagnostic overlay, so it should read clearly over
            // everything real that was drawn this frame.
            self.debug_lines.draw(&mut pass, debug_line_count);

            // Right beside the debug lines, for the same reason: a plugin
            // billboard is a world-space overlay a plugin author
            // wants clearly visible, not a piece of real terrain competing
            // for draw order.
            self.plugin_billboards.draw(&mut pass, plugin_billboard_count);

        }
        drop(pass);
        drop(timer);

        // Nametags last of all, real depth-tested against this same
        // terrain+entity depth buffer — see `gpu/nametag.rs`'s module doc for
        // the normal/see-through split and their exact depth settings.
        //
        // Their own pass, on the raw (non-sRGB) view, for the same reason the
        // sign-text pass above has one: the plate is black at vanilla's 25%
        // background opacity and vanilla blends it on gamma bytes. Both this
        // pass's pipelines are still drawn in one pass, and in the same order
        // as before, so the plate still paints over the opaque normal-pass
        // glyphs exactly as `SubmitNodeCollection`'s phase list does.
        if draw_nametags
            && let Some(text_view) = world_text_target
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world nametag pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: text_view,
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
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                // The `"world"` span's end edge — see the pass above.
                timestamp_writes: self
                    .gpu_timer
                    .borrow()
                    .as_ref()
                    .and_then(|t| t.writes_end("world")),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            nametag_pass_begins += 1;
            self.nametag.draw(&mut pass, name_tag_counts);
        }

        // The first-person arm/held-item pass: its own pass, with the depth
        // buffer cleared. See [`Self::draw_first_person_hand`] for why the
        // clear is there and why it is not optional.
        if let Some(hand) = &first_person_hand {
            self.draw_first_person_hand(encoder, view, hand, &mut stats);
        }

        // The screen overlays, each from its own closed fix: their own `Load` passes (see
        // `ScreenEffectRenderer::draw_underwater`'s doc — they must not erase
        // the world/hand just drawn), run last, matching vanilla's own order
        // (vanilla's own game-renderer rendering: the hand, then
        // its own screen-effect submission/camera-overlay extraction, then the
        // HUD/feature renderers — this shell's HUD draws in a later, separate
        // pass in `app.rs`).
        //
        // Two independent gate groups, not one — see
        // `ScreenEffects::any_active`'s doc for why: underwater/fire/pumpkin/
        // spyglass are first-person-only in vanilla, freeze/confusion/portal
        // are not (vanilla's own hud rendering has them as siblings of the `isFirstPerson`
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
                        fx.draw_underwater(queue, encoder, view, camera.yaw, camera.pitch, light);
                        stats.underwater_overlay_drawn = true;
                    }
                    if screen_effects.on_fire {
                        fx.draw_fire(queue, encoder, view, screen_effects.tick);
                        stats.fire_overlay_drawn = true;
                    }
                    if screen_effects.wearing_pumpkin {
                        fx.draw_pumpkin(encoder, view);
                        stats.pumpkin_overlay_drawn = true;
                    }
                    if screen_effects.scoping {
                        fx.draw_spyglass(queue, encoder, view, camera.aspect);
                        stats.spyglass_overlay_drawn = true;
                    }
                }
                if screen_effects.camera_agnostic_group_active() {
                    if screen_effects.freeze_percent > 0.0 {
                        fx.draw_freeze(queue, encoder, view, screen_effects.freeze_percent);
                        stats.freeze_overlay_drawn = true;
                    }
                    // Portal takes priority over confusion when both are
                    // positive — vanilla's own hud rendering's own `if`/`else if`.
                    if screen_effects.portal_intensity > 0.0 {
                        let frame = (screen_effects.tick % u64::from(fx.portal_frame_count())) as u32;
                        fx.draw_portal(queue, encoder, view, frame, screen_effects.portal_intensity);
                        stats.portal_overlay_drawn = true;
                    } else if screen_effects.nausea_intensity > 0.0 {
                        stats.confusion_overlay_drawn = fx.draw_confusion(
                            queue, encoder, view, screen_effects.nausea_intensity,
                        );
                    }
                    if screen_effects.vision_obscuration > 0.0 {
                        fx.draw_vision_obscuration(
                            queue,
                            encoder,
                            view,
                            screen_effects.vision_obscuration,
                        );
                        stats.vision_obscuration_drawn = true;
                    }
                }
                if screen_effects.border_warning_active() {
                    fx.draw_border_warning(
                        queue,
                        encoder,
                        view,
                        screen_effects.border_warning_strength,
                    );
                    stats.border_warning_overlay_drawn = true;
                }
            }
        }

        crate::gpu::gpu_timing::record_world_subphase(
            crate::gpu::gpu_timing::WorldSubphase::OtherDraws,
            world_encode_other_t0.get().elapsed().as_secs_f32() * 1000.0,
        );
        if let Some(timer) = self.gpu_timer.borrow_mut().as_mut() {
            timer.resolve(encoder);
        }

        // Residency, measured — **not** `vram_bytes(stats.total_quads)`, which is
        // what this was. `total_quads` only accumulates over sections that
        // survived the cull, so that form reported a VRAM figure that moved every
        // time the camera turned on the spot, and it priced live-vanilla quads at
        // the packed path's 72 B instead of a `ModelVertex` quad's 152 B. See
        // `RenderState::resident_mesh_bytes`.
        (stats.vram_bytes, stats.vram_reserved_bytes) = self.mesh_storage_bytes();
        // Record this only after every production pass has updated `stats`.
        // The CSV witnesses are therefore actual submissions, not a terrain-only
        // snapshot that would report zero for later entity, water, sign, and
        // particle work regardless of what reached the renderer.
        crate::gpu::gpu_timing::record_world_subphase_counts(
            crate::gpu::gpu_timing::WorldSubphaseCounts {
                packed_sections_visited: self.sections.len(),
                model_sections_visited: self.model.as_ref().map_or(0, |m| m.sections.len()),
                opaque_sections_drawn: stats.sections_drawn,
                water_sections_drawn: stats.water_sections_drawn,
                translucent_sections_drawn: stats.translucent_sections_drawn,
                entities_drawn: stats.entities_drawn,
                block_entities_drawn: stats.block_entities_drawn,
                sign_text_vertices: stats.sign_text_vertices,
                particles_drawn: stats.particles_drawn,
                world_pass_begins,
                world_text_pass_begins,
                nametag_pass_begins,
                terrain_camera_bind_calls: stats.terrain_camera_bind_calls,
                terrain_origin_vertex_binds: stats.terrain_origin_vertex_binds,
                terrain_indexed_draw_calls: stats.terrain_indexed_draw_calls,
                terrain_buffer_bind_pairs: stats.terrain_buffer_binds,
            },
        );
        self.terrain_cull_diagnostics
            .borrow_mut()
            .report(camera, &terrain_cull, stats, sign_prepare);
        stats
    }
}

fn collect_visible_model_sections<'a>(
    model: &'a super::terrain::ModelRenderer,
    camera: &Camera,
    cull: &TerrainCull,
    stats: &mut RenderStats,
) -> Vec<(&'a super::terrain::ModelSectionGpu, f32)> {
    let mut visible = Vec::with_capacity(model.sections.len());
    for (key, section) in &model.sections {
        if section.mesh.is_none() && section.water.is_none() && section.translucent.is_none() {
            continue;
        }
        let coord = key.coord();
        let verdict = cull.classify(coord);
        if section.mesh.is_some() {
            match verdict {
                CullVerdict::Visible => {
                    stats.sections_occlusion_shadow += usize::from(cull.shadow_would_cull(coord));
                }
                CullVerdict::Distance => stats.sections_culled_distance += 1,
                CullVerdict::Frustum => stats.sections_culled_frustum += 1,
                CullVerdict::Occlusion => stats.sections_culled_occlusion += 1,
            }
        }
        if verdict != CullVerdict::Visible {
            stats.water_sections_culled += usize::from(section.water.is_some());
            stats.translucent_sections_culled += usize::from(section.translucent.is_some());
            continue;
        }
        let distance = if section.water.is_some() || section.translucent.is_some() {
            super::terrain::section_center_distance_sq(coord, camera.position)
        } else {
            0.0
        };
        visible.push((section, distance));
    }
    visible
}

/// Preserves pass order while sharing consecutive arena buffer bindings.
fn emit_terrain_draws(
    pass: &mut wgpu::RenderPass<'_>,
    model: &super::terrain::ModelRenderer,
    draws: &[TerrainDraw<'_>],
    terrain_cam_group_last: &mut Option<*const wgpu::BindGroup>,
    stats: &mut RenderStats,
) {
    if draws.is_empty() {
        return;
    }
    let instance_origins = model.terrain_pipeline.is_some();
    let origin_shift = model.origin_arena.stride().trailing_zeros();
    if instance_origins {
        bind_terrain_camera(
            pass, &model.cam_bind_group, model.origin_arena.zero_offset(),
            terrain_cam_group_last, stats,
        );
        pass.set_vertex_buffer(1, model.origin_arena.buffer().slice(..));
        stats.terrain_origin_vertex_binds += 1;
    }
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
        let instance = if instance_origins {
            draw.origin_offset >> origin_shift
        } else {
            bind_terrain_camera(
                pass, &model.cam_bind_group, draw.origin_offset,
                terrain_cam_group_last, stats,
            );
            0
        };
        pass.draw_indexed(
            draw.first_index..draw.first_index + draw.index_count,
            draw.base_vertex,
            instance..instance + 1,
        );
        stats.terrain_indexed_draw_calls += 1;
    }
    stats.terrain_buffer_binds += bind_pairs;
}

/// Count group identity changes separately from actual binding calls.
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
    stats.terrain_camera_bind_calls += 1;
}
